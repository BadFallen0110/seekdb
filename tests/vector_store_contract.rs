use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use minivectordb::vector_store::VectorStore;

fn unique_file() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "minivectordb-vectors-{}-{nonce}.bin",
        std::process::id()
    ))
}

#[test]
fn paged_store_round_trips_vectors_and_bounds_cache() {
    let path = unique_file();
    let mut store = VectorStore::open(&path, 64 * 1024).unwrap();
    let mut handles = Vec::new();
    for value in 0..1_000 {
        handles.push(store.append(&vec![value as f32; 128]).unwrap());
    }
    for (value, handle) in handles.iter().enumerate() {
        let vector = store.read(*handle).unwrap();
        assert_eq!(vector.len(), 128);
        assert_eq!(vector[0], value as f32);
    }
    assert!(store.cache_bytes() <= 64 * 1024);
    assert!(store.cache_bytes() > 0);
    drop(store);
    std::fs::remove_file(path).unwrap();
}
