use std::env;
use std::io::{BufReader, BufWriter, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use minivectordb::db::Database;
use minivectordb::protocol::{OutputFormat, ProtocolConfig, run};
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
        PathBuf::from(env::var("MINIVECTORDB_DATA_DIR").unwrap_or_else(|_| "/data".to_owned()));
    let mut ready_marker = env::var("MINIVECTORDB_READY").unwrap_or_else(|_| "READY".to_owned());
    let mut mode = env::var("MINIVECTORDB_QUERY_MODE").ok();
    let mut output_format = env::var("MINIVECTORDB_OUTPUT").unwrap_or_else(|_| "framed".to_owned());
    let mut cache_limit = None;

    let mut arguments = env::args().skip(1);
    while let Some(argument) = arguments.next() {
        if let Some(value) = argument.strip_prefix("--data-dir=") {
            data_directory = PathBuf::from(value);
            continue;
        }
        if let Some(value) = argument.strip_prefix("--ready=") {
            ready_marker = value.to_owned();
            continue;
        }
        if let Some(value) = argument.strip_prefix("--mode=") {
            mode = Some(value.to_owned());
            continue;
        }
        if let Some(value) = argument.strip_prefix("--output=") {
            output_format = value.to_owned();
            continue;
        }
        if let Some(value) = argument.strip_prefix("--cache-bytes=") {
            cache_limit = Some(value.parse::<usize>()?);
            continue;
        }
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
                output_format = arguments
                    .next()
                    .ok_or("--output requires a format argument")?;
            }
            "--cache-bytes" => {
                cache_limit = Some(
                    arguments
                        .next()
                        .ok_or("--cache-bytes requires an integer")?
                        .parse::<usize>()?,
                );
            }
            "--help" | "-h" => {
                let mut stdout = BufWriter::new(std::io::stdout().lock());
                writeln!(
                    stdout,
                    "minivectordb [--data-dir PATH] [--ready TEXT] \
                     [--mode exact|ann] [--output framed|tsv|mysql] \
                     [--cache-bytes N]"
                )?;
                stdout.flush()?;
                return Ok(());
            }
            unknown => return Err(format!("unknown argument `{unknown}`").into()),
        }
    }

    let mut database = match cache_limit {
        Some(cache_limit) => Database::open_with_cache_limit(data_directory, cache_limit)?,
        None => Database::open(data_directory)?,
    };
    if let Some(mode) = mode {
        database.set_query_mode(match mode.to_ascii_lowercase().as_str() {
            "exact" => QueryMode::Exact,
            "ann" => QueryMode::Ann,
            _ => return Err("query mode must be exact or ann".into()),
        });
    }
    let output_format = match output_format.to_ascii_lowercase().as_str() {
        "framed" => OutputFormat::Framed,
        "tsv" | "mysql-batch" => OutputFormat::Tsv,
        "mysql" => OutputFormat::Mysql,
        _ => return Err("output format must be framed, tsv, or mysql".into()),
    };

    let reader = BufReader::new(std::io::stdin());
    let writer = BufWriter::new(std::io::stdout().lock());
    run(
        &mut database,
        reader,
        writer,
        &ProtocolConfig {
            ready_marker,
            output_format,
        },
    )?;
    Ok(())
}
