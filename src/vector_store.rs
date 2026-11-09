use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::error::{DbError, ErrorCategory};
use crate::vector::l2_distance;

const PAGE_SIZE: u64 = 64 * 1024;

pub struct VectorStore {
    backend: Backend,
}

enum Backend {
    Memory(Vec<Vec<f32>>),
    Paged(Box<PagedStore>),
}

impl VectorStore {
    pub fn memory() -> Self {
        Self {
            backend: Backend::Memory(Vec::new()),
        }
    }

    pub fn open(path: impl AsRef<Path>, cache_limit: usize) -> Result<Self, DbError> {
        Ok(Self {
            backend: Backend::Paged(Box::new(PagedStore::open(path.as_ref(), cache_limit)?)),
        })
    }

    pub fn append(&mut self, values: &[f32]) -> Result<u64, DbError> {
        match &mut self.backend {
            Backend::Memory(vectors) => {
                let handle = vectors.len() as u64;
                vectors.push(values.to_vec());
                Ok(handle)
            }
            Backend::Paged(store) => store.append(values),
        }
    }

    pub fn read(&mut self, handle: u64) -> Result<Vec<f32>, DbError> {
        match &mut self.backend {
            Backend::Memory(vectors) => vectors
                .get(handle as usize)
                .cloned()
                .ok_or_else(|| invalid_vector_handle(handle)),
            Backend::Paged(store) => store.read(handle),
        }
    }

    pub fn read_into(&mut self, handle: u64, output: &mut Vec<f32>) -> Result<(), DbError> {
        match &mut self.backend {
            Backend::Memory(vectors) => {
                let values = vectors
                    .get(handle as usize)
                    .ok_or_else(|| invalid_vector_handle(handle))?;
                output.clear();
                output.extend_from_slice(values);
                Ok(())
            }
            Backend::Paged(store) => store.read_into(handle, output),
        }
    }

    pub fn l2_distance(&mut self, handle: u64, query: &[f32]) -> Result<f64, DbError> {
        match &mut self.backend {
            Backend::Memory(vectors) => {
                let values = vectors
                    .get(handle as usize)
                    .ok_or_else(|| invalid_vector_handle(handle))?;
                l2_distance(values, query)
            }
            Backend::Paged(store) => store.l2_distance(handle, query),
        }
    }

    pub fn sync_data(&mut self) -> Result<(), DbError> {
        if let Backend::Paged(store) = &mut self.backend {
            store.file.flush()?;
            store.file.sync_data()?;
        }
        Ok(())
    }

    pub fn cache_bytes(&self) -> usize {
        match &self.backend {
            Backend::Memory(_) => 0,
            Backend::Paged(store) => store.cache.bytes + store.vector_cache.bytes,
        }
    }

    pub fn path(&self) -> Option<&Path> {
        match &self.backend {
            Backend::Memory(_) => None,
            Backend::Paged(store) => Some(&store.path),
        }
    }
}

struct PagedStore {
    file: File,
    path: PathBuf,
    end: u64,
    cache: PageCache,
    vector_cache: VectorValueCache,
    byte_scratch: Vec<u8>,
    vector_scratch: Vec<f32>,
}

impl PagedStore {
    fn open(path: &Path, cache_limit: usize) -> Result<Self, DbError> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)?;
        let end = file.metadata()?.len();
        let page_limit = cache_limit.min(1024 * 1024);
        let vector_limit = cache_limit.saturating_sub(page_limit);
        Ok(Self {
            file,
            path: path.to_path_buf(),
            end,
            cache: PageCache::new(page_limit.max(PAGE_SIZE as usize)),
            vector_cache: VectorValueCache::new(vector_limit),
            byte_scratch: Vec::new(),
            vector_scratch: Vec::new(),
        })
    }

    fn append(&mut self, values: &[f32]) -> Result<u64, DbError> {
        let length = u32::try_from(values.len()).map_err(|_| {
            DbError::new(
                ErrorCategory::Range,
                "VECTOR dimension does not fit in the storage format",
            )
        })?;
        let handle = self.end;
        self.file.seek(SeekFrom::Start(handle))?;
        self.file.write_all(&length.to_le_bytes())?;
        for value in values {
            self.file.write_all(&value.to_le_bytes())?;
        }
        self.end = self
            .end
            .checked_add(4 + u64::from(length) * 4)
            .ok_or_else(|| DbError::new(ErrorCategory::Range, "VECTOR storage offset overflow"))?;
        self.cache.invalidate_from(handle);
        self.vector_cache.invalidate_from(handle);
        Ok(handle)
    }

    fn read(&mut self, handle: u64) -> Result<Vec<f32>, DbError> {
        let mut output = Vec::new();
        self.read_into(handle, &mut output)?;
        Ok(output)
    }

    fn read_into(&mut self, handle: u64, output: &mut Vec<f32>) -> Result<(), DbError> {
        if handle >= self.end {
            return Err(invalid_vector_handle(handle));
        }
        if self.vector_cache.get_into(handle, output) {
            return Ok(());
        }
        let mut length_bytes = [0_u8; 4];
        self.read_bytes_into(handle, &mut length_bytes)?;
        let length = u32::from_le_bytes(length_bytes) as usize;
        let byte_length = length
            .checked_mul(4)
            .ok_or_else(|| DbError::new(ErrorCategory::Corruption, "VECTOR length overflow"))?;
        let mut scratch = std::mem::take(&mut self.byte_scratch);
        scratch.resize(byte_length, 0);
        let read_result = self.read_bytes_into(handle + 4, &mut scratch);
        self.byte_scratch = scratch;
        read_result?;
        output.clear();
        output.reserve(length);
        for chunk in self.byte_scratch.as_chunks::<4>().0 {
            output.push(f32::from_le_bytes(*chunk));
        }
        self.vector_cache.insert(handle, output.clone());
        Ok(())
    }

    fn l2_distance(&mut self, handle: u64, query: &[f32]) -> Result<f64, DbError> {
        if handle >= self.end {
            return Err(invalid_vector_handle(handle));
        }
        if let Some(values) = self.vector_cache.get(handle) {
            return l2_distance(values, query);
        }
        let mut scratch = std::mem::take(&mut self.vector_scratch);
        let result = self.read_into(handle, &mut scratch);
        let distance = match result {
            Ok(()) => l2_distance(&scratch, query),
            Err(error) => Err(error),
        };
        self.vector_scratch = scratch;
        distance
    }

    fn read_bytes_into(&mut self, offset: u64, output: &mut [u8]) -> Result<(), DbError> {
        let length = output.len();
        if length == 0 {
            return Ok(());
        }
        let end = offset
            .checked_add(length as u64)
            .ok_or_else(|| DbError::new(ErrorCategory::Corruption, "VECTOR read overflow"))?;
        if end > self.end {
            return Err(DbError::new(
                ErrorCategory::Corruption,
                "VECTOR record extends beyond vector file",
            ));
        }
        let mut written = 0;
        let mut position = offset;
        while position < end {
            let page_number = position / PAGE_SIZE;
            let page_start = page_number * PAGE_SIZE;
            let in_page = (position - page_start) as usize;
            self.cache.ensure_page(&mut self.file, page_number)?;
            let page = &self.cache.pages[&page_number].bytes;
            let available = page.len().saturating_sub(in_page);
            if available == 0 {
                return Err(DbError::new(
                    ErrorCategory::Corruption,
                    "VECTOR page ended before requested record",
                ));
            }
            let take = available.min((end - position) as usize);
            output[written..written + take].copy_from_slice(&page[in_page..in_page + take]);
            written += take;
            position += take as u64;
        }
        Ok(())
    }
}

struct PageCache {
    limit: usize,
    bytes: usize,
    tick: u64,
    pages: HashMap<u64, CachedPage>,
}

struct CachedPage {
    tick: u64,
    bytes: Vec<u8>,
}

struct VectorValueCache {
    limit: usize,
    bytes: usize,
    tick: u64,
    values: HashMap<u64, CachedVector>,
}

struct CachedVector {
    offset: u64,
    tick: u64,
    values: Vec<f32>,
}

impl VectorValueCache {
    fn new(limit: usize) -> Self {
        Self {
            limit,
            bytes: 0,
            tick: 0,
            values: HashMap::new(),
        }
    }

    fn get_into(&mut self, handle: u64, output: &mut Vec<f32>) -> bool {
        self.tick += 1;
        let tick = self.tick;
        let Some(cached) = self.values.get_mut(&handle) else {
            return false;
        };
        cached.tick = tick;
        output.clear();
        output.extend_from_slice(&cached.values);
        true
    }

    fn get(&mut self, handle: u64) -> Option<&[f32]> {
        self.tick += 1;
        let tick = self.tick;
        let cached = self.values.get_mut(&handle)?;
        cached.tick = tick;
        Some(&cached.values)
    }

    fn insert(&mut self, handle: u64, values: Vec<f32>) {
        if self.limit == 0 {
            return;
        }
        let byte_count = values.len() * std::mem::size_of::<f32>();
        if byte_count > self.limit {
            return;
        }
        while self.bytes.saturating_add(byte_count) > self.limit {
            let Some(oldest) = self
                .values
                .iter()
                .min_by_key(|(_, cached)| cached.tick)
                .map(|(handle, _)| *handle)
            else {
                return;
            };
            if let Some(removed) = self.values.remove(&oldest) {
                self.bytes -= removed.values.len() * std::mem::size_of::<f32>();
            }
        }
        self.tick += 1;
        self.values.insert(
            handle,
            CachedVector {
                offset: handle,
                tick: self.tick,
                values,
            },
        );
        self.bytes += byte_count;
    }

    fn invalidate_from(&mut self, offset: u64) {
        let affected = self
            .values
            .iter()
            .filter(|(_, cached)| cached.offset >= offset)
            .map(|(handle, _)| *handle)
            .collect::<Vec<_>>();
        for handle in affected {
            if let Some(removed) = self.values.remove(&handle) {
                self.bytes -= removed.values.len() * std::mem::size_of::<f32>();
            }
        }
    }
}

impl PageCache {
    fn new(limit: usize) -> Self {
        Self {
            limit,
            bytes: 0,
            tick: 0,
            pages: HashMap::new(),
        }
    }

    fn ensure_page(&mut self, file: &mut File, page_number: u64) -> Result<(), DbError> {
        self.tick += 1;
        let tick = self.tick;
        if let Some(page) = self.pages.get_mut(&page_number) {
            page.tick = tick;
            return Ok(());
        }

        file.seek(SeekFrom::Start(page_number * PAGE_SIZE))?;
        let mut bytes = vec![0_u8; PAGE_SIZE as usize];
        let read = file.read(&mut bytes)?;
        bytes.truncate(read);
        if bytes.is_empty() {
            return Err(DbError::new(
                ErrorCategory::Corruption,
                "VECTOR page is missing",
            ));
        }
        while self.bytes.saturating_add(bytes.len()) > self.limit && !self.pages.is_empty() {
            let Some(oldest) = self
                .pages
                .iter()
                .min_by_key(|(_, page)| page.tick)
                .map(|(number, _)| *number)
            else {
                break;
            };
            if let Some(removed) = self.pages.remove(&oldest) {
                self.bytes -= removed.bytes.len();
            }
        }
        self.bytes += bytes.len();
        self.pages.insert(page_number, CachedPage { tick, bytes });
        Ok(())
    }

    fn invalidate_from(&mut self, offset: u64) {
        let first_page = offset / PAGE_SIZE;
        let affected = self
            .pages
            .keys()
            .copied()
            .filter(|page| *page >= first_page)
            .collect::<Vec<_>>();
        for page in affected {
            if let Some(removed) = self.pages.remove(&page) {
                self.bytes -= removed.bytes.len();
            }
        }
    }
}

fn invalid_vector_handle(handle: u64) -> DbError {
    DbError::new(
        ErrorCategory::Corruption,
        format!("invalid VECTOR handle {handle}"),
    )
}
