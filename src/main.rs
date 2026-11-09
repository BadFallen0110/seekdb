use std::env;
use std::io::{BufReader, BufWriter, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use minivectordb::db::Database;
use minivectordb::protocol::{ProtocolConfig, run};
use minivectordb::sql::QueryMode;

fn main() -> ExitCode {
    match run_main() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("fatal: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run_main() -> Result<(), Box<dyn std::error::Error>> {
    let mut data_directory =
        PathBuf::from(env::var("MINIVECTORDDB_DATA_DIR").unwrap_or_else(|_| "/data".to_owned()));
    let mut ready_marker = env::var("MINIVECTORDDB_READY").unwrap_or_else(|_| "READY".to_owned());
    let mut mode = env::var("MINIVECTORDDB_QUERY_MODE").ok();

    let mut arguments = env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--data-dir" => {
                data_directory = PathBuf::from(
                    arguments
                        .next()
                        .ok_or("--data-dir requires a path argument")?,
                );
            }
            "--ready" => {
                ready_marker = arguments
                    .next()
                    .ok_or("--ready requires a marker argument")?;
            }
            "--mode" => {
                mode = Some(arguments.next().ok_or("--mode requires exact or ann")?);
            }
            "--output" => {
                let _ = arguments
                    .next()
                    .ok_or("--output requires a format argument")?;
                // The default framed adapter is used until the official pack is
                // available; accepting the option keeps deployment compatible.
            }
            "--help" | "-h" => {
                let mut stdout = BufWriter::new(std::io::stdout().lock());
                writeln!(
                    stdout,
                    "minivectordb [--data-dir PATH] [--ready TEXT] [--mode exact|ann]"
                )?;
                stdout.flush()?;
                return Ok(());
            }
            unknown => return Err(format!("unknown argument `{unknown}`").into()),
        }
    }

    let mut database = Database::open(data_directory)?;
    if let Some(mode) = mode {
        database.set_query_mode(match mode.to_ascii_lowercase().as_str() {
            "exact" => QueryMode::Exact,
            "ann" => QueryMode::Ann,
            _ => return Err("query mode must be exact or ann".into()),
        });
    }

    let reader = BufReader::new(std::io::stdin());
    let writer = BufWriter::new(std::io::stdout().lock());
    run(
        &mut database,
        reader,
        writer,
        &ProtocolConfig { ready_marker },
    )?;
    Ok(())
}
