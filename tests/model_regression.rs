use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::path::PathBuf;

use minivectordb::db::Database;
use minivectordb::value::Value;

mod support;
use support::unique_temp_path;

type ModelRow = (String, String);

fn unique_dir() -> PathBuf {
    unique_temp_path("minivectordb-model")
}

fn next(state: &mut u64) -> u64 {
    *state = state
        .wrapping_mul(2_862_933_555_777_941_757)
        .wrapping_add(3_037_000_493);
    *state
}

fn check(database: &mut Database, model: &BTreeMap<i64, ModelRow>) {
    let result = database.execute("SELECT * FROM model ORDER BY id").unwrap();
    let actual = result
        .rows
        .into_iter()
        .map(|row| {
            (
                row[0].as_bigint().unwrap(),
                (
                    row[1].as_varchar().unwrap().to_owned(),
                    row[2].as_varchar().unwrap().to_owned(),
                ),
            )
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(&actual, model);
}

#[test]
fn random_transaction_sequences_match_in_memory_oracle_across_restarts() {
    let directory = unique_dir();
    let mut database = Database::open(&directory).unwrap();
    database
        .execute(
            "CREATE TABLE model (\
               id BIGINT PRIMARY KEY,\
               category VARCHAR(16),\
               body VARCHAR(32)\
             )",
        )
        .unwrap();
    let mut model = BTreeMap::new();
    let mut random = 0x9e37_79b9_7f4a_7c15_u64;

    for step in 0..240 {
        let id = (next(&mut random) % 40) as i64;
        match next(&mut random) % 6 {
            0 | 1 => {
                if let Entry::Vacant(entry) = model.entry(id) {
                    let category = format!("c{}", next(&mut random) % 4);
                    let body = format!("v{step}");
                    database
                        .execute(&format!(
                            "INSERT INTO model VALUES ({id},'{category}','{body}')"
                        ))
                        .unwrap();
                    entry.insert((category, body));
                }
            }
            2 => {
                if model.contains_key(&id) {
                    let body = format!("u{step}");
                    database
                        .execute(&format!("UPDATE model SET body='{body}' WHERE id={id}"))
                        .unwrap();
                    model.get_mut(&id).unwrap().1 = body;
                }
            }
            3 => {
                database
                    .execute(&format!("DELETE FROM model WHERE id={id}"))
                    .unwrap();
                model.remove(&id);
            }
            4 | 5 => {
                let category = format!("c{}", next(&mut random) % 4);
                let before = model.clone();
                database.execute("BEGIN").unwrap();
                database
                    .execute(&format!(
                        "UPDATE model SET category='{category}' WHERE category='c0'"
                    ))
                    .unwrap();
                for row in model.values_mut() {
                    if row.0 == "c0" {
                        row.0 = category.clone();
                    }
                }
                if id % 2 == 0 {
                    database
                        .execute(&format!("DELETE FROM model WHERE id={id}"))
                        .unwrap();
                    model.remove(&id);
                    database.execute("COMMIT").unwrap();
                } else {
                    database.execute("ROLLBACK").unwrap();
                    model = before;
                }
            }
            _ => unreachable!(),
        }
        check(&mut database, &model);
        if step % 40 == 39 {
            database.close().unwrap();
            drop(database);
            database = Database::open(&directory).unwrap();
            check(&mut database, &model);
        }
    }

    database.close().unwrap();
    for row in database.execute("SELECT * FROM model").unwrap().rows {
        assert!(matches!(row[0], Value::BigInt(_)));
    }
    drop(database);
    std::fs::remove_dir_all(directory).unwrap();
}
