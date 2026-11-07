//! Builds the database from MusicBrainz dump archives (`mbdump.tar.bz2`, `mbdump-derived.tar.bz2`,
//! `mbdump-cover-art-archive.tar.bz2`) or from directories they were unpacked into.
//!
//! Each archive is decompressed by a thread of its own, which hands the rows of the tables in `spec` to one writer;
//! the writer inserts them into a new SQLite file in one transaction, then creates the indexes (`indexes.sql`) and
//! the derived tables and search indexes (`derive.sql`).

pub mod copy;
pub mod spec;

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Instant;

use rusqlite::fallible_iterator::FallibleIterator as _;
use rusqlite::types::Value;

use self::spec::Spec;

pub const SCHEMA: &str = include_str!("schema.sql");
pub const INDEXES: &str = include_str!("indexes.sql");
pub const DERIVE: &str = include_str!("derive.sql");

/// The version of this database layout, in `meta`. A database of another version is not opened (it is rebuilt from
/// the next dump).
pub const DB_VERSION: &str = "1";

/// Rows are handed to the writer in chunks of about this size.
const CHUNK: usize = 4 << 20;

/// Tables without which a dump isn't one.
const REQUIRED: &[&str] = &[
    "artist",
    "artist_credit",
    "recording",
    "release",
    "release_group",
    "medium",
    "track",
];

#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("{0}: {1}")]
    Io(PathBuf, std::io::Error),
    #[error("database: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error(
        "{table}: a row has {got} columns, {expected} expected (the MusicBrainz schema changed?)"
    )]
    Columns {
        table: String,
        got: usize,
        expected: usize,
    },
    #[error("no {0} table in the dump")]
    Missing(&'static str),
    #[error("the import was interrupted")]
    Interrupted,
}

/// What a finished import holds.
#[derive(Debug, Default)]
pub struct Summary {
    /// Rows per imported dump file.
    pub rows: HashMap<String, u64>,
    /// The dump's `TIMESTAMP`, `SCHEMA_SEQUENCE` and `REPLICATION_SEQUENCE`.
    pub info: HashMap<String, String>,
}

enum Msg {
    Rows { table: String, data: Vec<u8> },
    End { table: String },
    Info { name: String, value: String },
    Failed(ImportError),
}

/// Builds the database at `out` (replaced if it exists) from `sources`: dump archives or directories with an
/// `mbdump/` directory in them. `dump` names the dump (its directory name, e.g. `20261104-002222`). `cache_mib` is
/// SQLite's page cache while building.
pub fn build(
    sources: &[PathBuf],
    dump: &str,
    out: &Path,
    cache_mib: u32,
) -> Result<Summary, ImportError> {
    let started = Instant::now();
    for suffix in ["", "-journal", "-wal", "-shm"] {
        let _ = std::fs::remove_file(side_file(out, suffix));
    }
    let mut db = rusqlite::Connection::open(out)?;
    let temp_dir = out
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    db.execute_batch(&format!(
        "PRAGMA journal_mode = OFF; PRAGMA synchronous = OFF; PRAGMA locking_mode = EXCLUSIVE;
         PRAGMA cache_size = -{}; PRAGMA temp_store = FILE; PRAGMA temp_store_directory = '{}';",
        u64::from(cache_mib) * 1024,
        temp_dir.display().to_string().replace('\'', "''"),
    ))?;
    db.set_prepared_statement_cache_capacity(64);
    db.execute_batch(SCHEMA)?;

    let specs = spec::all();
    let (tx, rx) = mpsc::sync_channel::<Msg>(16);
    let readers: Vec<_> = sources
        .iter()
        .map(|source| {
            let (tx, source, specs) = (tx.clone(), source.clone(), specs.clone());
            std::thread::spawn(move || {
                if let Err(e) = read_source(&source, &specs, &tx) {
                    let _ = tx.send(Msg::Failed(e));
                }
            })
        })
        .collect();
    drop(tx);

    let mut summary = Summary::default();
    let written = write_rows(&mut db, &specs, &rx, &mut summary);
    // On failure the readers notice the closed channel and stop.
    drop(rx);
    for reader in readers {
        let _ = reader.join();
    }
    written?;
    for table in REQUIRED {
        if !summary.rows.contains_key(*table) {
            return Err(ImportError::Missing(table));
        }
    }
    tracing::info!(
        elapsed_s = started.elapsed().as_secs(),
        "rows imported; creating indexes"
    );

    run_script(&db, "indexes", INDEXES)?;
    tracing::info!(
        elapsed_s = started.elapsed().as_secs(),
        "indexes created; building search indexes"
    );
    run_script(&db, "derived tables", DERIVE)?;

    let mut meta = db.prepare("INSERT INTO meta (key, value) VALUES (?1, ?2)")?;
    meta.execute(("dump", dump))?;
    meta.execute(("db_version", DB_VERSION))?;
    for (name, value) in &summary.info {
        meta.execute((name.to_lowercase(), value))?;
    }
    drop(meta);
    db.execute_batch("PRAGMA analysis_limit = 1000; ANALYZE;")?;
    db.close().map_err(|(_, e)| e)?;
    tracing::info!(elapsed_s = started.elapsed().as_secs(), "database built");
    Ok(summary)
}

fn write_rows(
    db: &mut rusqlite::Connection,
    specs: &HashMap<String, Spec>,
    rx: &mpsc::Receiver<Msg>,
    summary: &mut Summary,
) -> Result<(), ImportError> {
    let tx = db.transaction()?;
    for msg in rx {
        match msg {
            Msg::Rows { table, data } => {
                let Some(spec) = specs.get(&table) else {
                    continue;
                };
                let mut stmt = tx.prepare_cached(&spec.insert_sql())?;
                let mut rows = 0u64;
                let mut fields = Vec::new();
                for line in data.split(|&b| b == b'\n').filter(|l| !l.is_empty()) {
                    copy::split_row(line, &mut fields);
                    if fields.len() != spec.column_count {
                        return Err(ImportError::Columns {
                            table,
                            got: fields.len(),
                            expected: spec.column_count,
                        });
                    }
                    if let Some(required) = spec.require
                        && fields
                            .get(required)
                            .is_none_or(|f| f.as_deref().is_none_or(|v| v == "0"))
                    {
                        continue;
                    }
                    for (param, (index, _, boolean)) in spec.picks.iter().enumerate() {
                        let value = match fields.get(*index).and_then(Option::as_deref) {
                            None => Value::Null,
                            Some(v) if *boolean => copy::parse_bool(v)
                                .map_or(Value::Null, |b| Value::Integer(i64::from(b))),
                            Some(v) => Value::Text(v.to_owned()),
                        };
                        stmt.raw_bind_parameter(param + 1, value)?;
                    }
                    stmt.raw_execute()?;
                    rows += 1;
                }
                *summary.rows.entry(table).or_default() += rows;
            }
            Msg::End { table } => {
                let rows = summary.rows.entry(table.clone()).or_default();
                tracing::info!(table, rows = *rows, "imported");
            }
            Msg::Info { name, value } => {
                summary.info.insert(name, value);
            }
            Msg::Failed(e) => return Err(e),
        }
    }
    tx.commit()?;
    Ok(())
}

/// Runs each statement of a script, logging the slow ones.
fn run_script(db: &rusqlite::Connection, name: &str, script: &str) -> Result<(), ImportError> {
    let mut batch = rusqlite::Batch::new(db, script);
    while let Some(mut stmt) = batch.next()? {
        let started = Instant::now();
        stmt.raw_execute()?;
        let elapsed = started.elapsed();
        if elapsed.as_secs() >= 10 {
            let sql = stmt.expanded_sql().unwrap_or_default();
            let what = sql
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty() && !l.starts_with("--"))
                .unwrap_or("");
            tracing::info!(
                script = name,
                elapsed_s = elapsed.as_secs(),
                statement = what,
                "done"
            );
        }
    }
    Ok(())
}

fn read_source(
    source: &Path,
    specs: &HashMap<String, Spec>,
    tx: &mpsc::SyncSender<Msg>,
) -> Result<(), ImportError> {
    let io = |e| ImportError::Io(source.to_owned(), e);
    if source.is_dir() {
        for name in ["TIMESTAMP", "SCHEMA_SEQUENCE", "REPLICATION_SEQUENCE"] {
            if let Ok(value) = std::fs::read_to_string(source.join(name)) {
                send(
                    tx,
                    Msg::Info {
                        name: name.to_owned(),
                        value: value.trim().to_owned(),
                    },
                )?;
            }
        }
        let dir = source.join("mbdump");
        let mut entries: Vec<_> = std::fs::read_dir(&dir)
            .map_err(|e| ImportError::Io(dir.clone(), e))?
            .filter_map(Result::ok)
            .collect();
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let path = format!("mbdump/{}", entry.file_name().to_string_lossy());
            if let Some(table) = spec::file_table(&path).filter(|t| specs.contains_key(*t)) {
                let file = std::fs::File::open(entry.path())
                    .map_err(|e| ImportError::Io(entry.path(), e))?;
                send_table(table, file, tx).map_err(|e| e.with_path(&entry.path()))?;
            }
        }
        return Ok(());
    }

    let file = std::fs::File::open(source).map_err(io)?;
    let decoder = bzip2::read::MultiBzDecoder::new(BufReader::with_capacity(1 << 20, file));
    let mut archive = tar::Archive::new(decoder);
    for entry in archive.entries().map_err(io)? {
        let mut entry = entry.map_err(io)?;
        let path = entry.path().map_err(io)?.to_string_lossy().into_owned();
        if let Some(table) = spec::file_table(&path).filter(|t| specs.contains_key(*t)) {
            let table = table.to_owned();
            tracing::debug!(table, archive = %source.display(), "reading");
            send_table(&table, &mut entry, tx).map_err(|e| e.with_path(source))?;
        } else if matches!(
            path.as_str(),
            "TIMESTAMP" | "SCHEMA_SEQUENCE" | "REPLICATION_SEQUENCE"
        ) {
            let mut value = String::new();
            entry.read_to_string(&mut value).map_err(io)?;
            send(
                tx,
                Msg::Info {
                    name: path,
                    value: value.trim().to_owned(),
                },
            )?;
        }
    }
    Ok(())
}

/// Sends a dump file's rows in chunks that end at line ends.
fn send_table(
    table: &str,
    reader: impl Read,
    tx: &mpsc::SyncSender<Msg>,
) -> Result<(), ImportError> {
    let mut reader = BufReader::with_capacity(1 << 20, reader);
    loop {
        let mut data = Vec::with_capacity(CHUNK + 4096);
        while data.len() < CHUNK {
            let n = reader
                .read_until(b'\n', &mut data)
                .map_err(|e| ImportError::Io(PathBuf::new(), e))?;
            if n == 0 {
                break;
            }
        }
        if data.is_empty() {
            break;
        }
        send(
            tx,
            Msg::Rows {
                table: table.to_owned(),
                data,
            },
        )?;
    }
    send(
        tx,
        Msg::End {
            table: table.to_owned(),
        },
    )
}

fn send(tx: &mpsc::SyncSender<Msg>, msg: Msg) -> Result<(), ImportError> {
    tx.send(msg).map_err(|_| ImportError::Interrupted)
}

impl ImportError {
    fn with_path(self, path: &Path) -> Self {
        match self {
            Self::Io(p, e) if p.as_os_str().is_empty() => Self::Io(path.to_owned(), e),
            other => other,
        }
    }
}

#[must_use]
pub fn side_file(db: &Path, suffix: &str) -> PathBuf {
    let mut name = db.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_run_on_an_empty_database() {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch(SCHEMA).unwrap();
        run_script(&db, "indexes", INDEXES).unwrap();
        run_script(&db, "derive", DERIVE).unwrap();
    }
}
