use std::cmp::Ordering;
use std::collections::BinaryHeap;

use crate::error::{DbError, ErrorCategory};
use crate::value::Value;

pub fn parse_vector(source: &str, dimensions: usize) -> Result<Vec<f32>, DbError> {
    let trimmed = source.trim();
    if !trimmed.starts_with('[') || !trimmed.ends_with(']') {
        return Err(DbError::new(
            ErrorCategory::Syntax,
            "vector literal must be enclosed in brackets",
        ));
    }
    let body = &trimmed[1..trimmed.len() - 1];
    let values = if body.trim().is_empty() {
        Vec::new()
    } else {
        body.split(',')
            .map(|part| {
                let part = part.trim();
                let value = part.parse::<f32>().map_err(|_| {
                    DbError::new(
                        ErrorCategory::TypeMismatch,
                        format!("invalid VECTOR number `{part}`"),
                    )
                })?;
                if !value.is_finite() {
                    return Err(DbError::new(
                        ErrorCategory::Range,
                        "VECTOR values must be finite float32 numbers",
                    ));
                }
                Ok(value)
            })
            .collect::<Result<Vec<_>, _>>()?
    };

    match Value::vector(values, dimensions)? {
        Value::Vector(values) => Ok(values),
        _ => unreachable!(),
    }
}

pub fn l2_squared(left: &[f32], right: &[f32]) -> Result<f64, DbError> {
    if left.len() != right.len() {
        return Err(DbError::new(
            ErrorCategory::TypeMismatch,
            format!(
                "VECTOR dimension mismatch: {} and {}",
                left.len(),
                right.len()
            ),
        ));
    }

    #[cfg(target_arch = "x86_64")]
    {
        if std::is_x86_feature_detected!("avx2") {
            // The caller validated equal lengths and finite components.
            return Ok(unsafe { l2_squared_avx2(left, right) });
        }
    }
    Ok(l2_squared_scalar(left, right))
}

pub fn l2_distance(left: &[f32], right: &[f32]) -> Result<f64, DbError> {
    Ok(l2_squared(left, right)?.sqrt())
}

fn l2_squared_scalar(left: &[f32], right: &[f32]) -> f64 {
    left.iter()
        .zip(right)
        .map(|(left, right)| {
            let difference = f64::from(*left) - f64::from(*right);
            difference * difference
        })
        .sum()
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn l2_squared_avx2(left: &[f32], right: &[f32]) -> f64 {
    use std::arch::x86_64::{
        _mm256_add_ps, _mm256_loadu_ps, _mm256_mul_ps, _mm256_setzero_ps, _mm256_storeu_ps,
        _mm256_sub_ps,
    };

    let mut accumulator = _mm256_setzero_ps();
    let mut position = 0;
    while position + 8 <= left.len() {
        // SAFETY: position has at least eight readable f32 elements in both
        // slices, and loadu accepts unaligned pointers.
        let left_chunk = unsafe { _mm256_loadu_ps(left.as_ptr().add(position)) };
        let right_chunk = unsafe { _mm256_loadu_ps(right.as_ptr().add(position)) };
        let difference = _mm256_sub_ps(left_chunk, right_chunk);
        accumulator = _mm256_add_ps(accumulator, _mm256_mul_ps(difference, difference));
        position += 8;
    }

    let mut lanes = [0.0_f32; 8];
    // SAFETY: lanes has room for exactly one eight-float AVX register.
    unsafe { _mm256_storeu_ps(lanes.as_mut_ptr(), accumulator) };
    let mut sum = lanes.iter().map(|value| f64::from(*value)).sum::<f64>();
    while position < left.len() {
        let difference = f64::from(left[position]) - f64::from(right[position]);
        sum += difference * difference;
        position += 1;
    }
    sum
}

#[derive(Debug)]
struct HeapEntry {
    distance: f64,
    primary_key: i64,
}

impl PartialEq for HeapEntry {
    fn eq(&self, other: &Self) -> bool {
        self.distance == other.distance && self.primary_key == other.primary_key
    }
}

impl Eq for HeapEntry {}

impl PartialOrd for HeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for HeapEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        self.distance
            .partial_cmp(&other.distance)
            .unwrap_or(Ordering::Equal)
            .then_with(|| self.primary_key.cmp(&other.primary_key))
    }
}

pub fn select_topk(mut candidates: Vec<(f64, i64)>, limit: usize) -> Vec<(f64, i64)> {
    let mut topk = TopKAccumulator::new(limit);
    for (distance, primary_key) in candidates.drain(..) {
        topk.push(distance, primary_key);
    }
    topk.finish()
}

pub struct TopKAccumulator {
    limit: usize,
    heap: BinaryHeap<HeapEntry>,
}

impl TopKAccumulator {
    pub fn new(limit: usize) -> Self {
        Self {
            limit,
            heap: BinaryHeap::with_capacity(limit.saturating_add(1)),
        }
    }

    pub fn push(&mut self, distance: f64, primary_key: i64) {
        if self.limit == 0 {
            return;
        }
        let entry = HeapEntry {
            distance,
            primary_key,
        };
        if self.heap.len() < self.limit {
            self.heap.push(entry);
        } else if self.heap.peek().is_some_and(|largest| entry < *largest) {
            self.heap.pop();
            self.heap.push(entry);
        }
    }

    pub fn finish(self) -> Vec<(f64, i64)> {
        let mut selected = self
            .heap
            .into_iter()
            .map(|entry| (entry.distance, entry.primary_key))
            .collect::<Vec<_>>();
        selected.sort_by(|left, right| {
            left.0
                .partial_cmp(&right.0)
                .unwrap_or(Ordering::Equal)
                .then_with(|| left.1.cmp(&right.1))
        });
        selected
    }
}
