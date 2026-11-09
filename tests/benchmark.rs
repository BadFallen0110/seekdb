use std::time::Instant;
use std::time::{SystemTime, UNIX_EPOCH};

use minivectordb::db::Database;
use minivectordb::value::Value;

const ROWS: usize = 10_000;
const DIMENSIONS: usize = 128;
const QUERIES: usize = 1_000;
const K: usize = 10;

struct RowVector {
    id: i64,
    category: &'static str,
    vector: Vec<f32>,
}

fn next_random(state: &mut u64) -> f32 {
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1);
    ((*state >> 32) as u32) as f32 / u32::MAX as f32
}

fn vector_literal(vector: &[f32]) -> String {
    format!(
        "[{}]",
        vector
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",")
    )
}

fn brute_force(rows: &[RowVector], query: &[f32], category: Option<&str>) -> Vec<i64> {
    let mut distances = rows
        .iter()
        .filter(|row| category.is_none_or(|category| row.category == category))
        .map(|row| {
            let distance = row
                .vector
                .iter()
                .zip(query)
                .map(|(left, right)| {
                    let difference = f64::from(*left) - f64::from(*right);
                    difference * difference
                })
                .sum::<f64>();
            (distance, row.id)
        })
        .collect::<Vec<_>>();
    distances.sort_by(|left, right| {
        left.0
            .partial_cmp(&right.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(left.1.cmp(&right.1))
    });
    distances.into_iter().take(K).map(|(_, id)| id).collect()
}

#[test]
#[ignore = "offline 10k x 128 regression benchmark"]
fn ten_thousand_vectors_return_exact_recall() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = std::env::temp_dir().join(format!(
        "minivectordb-benchmark-{}-{nonce}",
        std::process::id()
    ));
    let mut random = 0x1234_5678_9abc_def0_u64;
    let rows = (0..ROWS)
        .map(|id| RowVector {
            id: id as i64 + 1,
            category: if id % 2 == 0 { "even" } else { "odd" },
            vector: (0..DIMENSIONS).map(|_| next_random(&mut random)).collect(),
        })
        .collect::<Vec<_>>();

    let mut database = Database::open_with_cache_limit(&directory, 64 * 1024 * 1024).unwrap();
    database
        .execute(
            "CREATE TABLE bench (\
               id BIGINT PRIMARY KEY,\
               category VARCHAR(8),\
               embedding VECTOR(128)\
             )",
        )
        .unwrap();

    let write_started = Instant::now();
    for batch in rows.chunks(100) {
        let values = batch
            .iter()
            .map(|row| {
                format!(
                    "({},'{}','{}')",
                    row.id,
                    row.category,
                    vector_literal(&row.vector)
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        database
            .execute(&format!("INSERT INTO bench VALUES {values}"))
            .unwrap();
    }
    let write_elapsed = write_started.elapsed();

    let queries = (0..QUERIES)
        .map(|_| {
            (0..DIMENSIONS)
                .map(|_| next_random(&mut random))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let query_started = Instant::now();
    let mut matched = 0_usize;
    for (index, query) in queries.iter().enumerate() {
        let category = (index % 2 == 1).then_some("even");
        let predicate = category
            .map(|category| format!(" WHERE category='{category}'"))
            .unwrap_or_default();
        let result = database
            .execute(&format!(
                "SELECT id, L2_DISTANCE(embedding,'{}') AS d \
                 FROM bench{predicate} ORDER BY d LIMIT {K}",
                vector_literal(query)
            ))
            .unwrap();
        let actual = result
            .rows
            .iter()
            .map(|row| match row[0] {
                Value::BigInt(id) => id,
                _ => panic!("benchmark id was not BIGINT"),
            })
            .collect::<Vec<_>>();
        assert_eq!(actual, brute_force(&rows, query, category));
        matched += actual.len();
    }
    let query_elapsed = query_started.elapsed();

    let recall = matched as f64 / (QUERIES * K) as f64;
    let cache_bytes = database.vector_cache_bytes();
    eprintln!(
        "rows={ROWS} dimensions={DIMENSIONS} queries={QUERIES} recall={recall:.6} write={write_elapsed:?} query={query_elapsed:?} cache_bytes={cache_bytes}"
    );
    assert_eq!(recall, 1.0);
    database.close().unwrap();
    let mut reopened = Database::open_with_cache_limit(&directory, 64 * 1024 * 1024).unwrap();
    assert_eq!(
        reopened
            .execute("SELECT id FROM bench ORDER BY id DESC LIMIT 1")
            .unwrap()
            .rows[0][0],
        Value::BigInt(ROWS as i64)
    );
    std::fs::remove_dir_all(directory).unwrap();
}
