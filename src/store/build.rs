//! Builds a store from MusicBrainz dump archives (`mbdump.tar.bz2`, `mbdump-derived.tar.bz2`,
//! `mbdump-cover-art-archive.tar.bz2`) or from directories they were unpacked into.
//!
//! Each archive is read by a thread of its own, which hands the rows of its tables to a writer thread of its own:
//! the archives are decompressed and their tables written in parallel, and nothing waits on a single writer. A
//! table's rows are written as they come (in id order, mostly); only the keys of its indexes are kept in memory, to
//! be sorted at the end of the table.

use std::collections::HashMap;
use std::io::{BufRead as _, BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Instant;

use super::copy;
use super::file::{
    CsrWriter, HashIndexWriter, NONE, Pool, PoolWriter, Rows as RowsFile, RowsWriter, u32_at,
    with_ext, write_gid_index,
};
use super::schema::{self, Rows, Table, Ty};

/// Rows are handed to the writer in chunks of about this size.
const CHUNK: usize = 4 << 20;

#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    #[error("{0}: {1}")]
    Io(PathBuf, std::io::Error),
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
    #[error("{0} exists: a store is built into a new directory")]
    Exists(PathBuf),
    #[error("the import was interrupted")]
    Interrupted,
    #[error("{0}")]
    Other(String),
}

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

/// What a finished build holds: rows per table, and the dump's `TIMESTAMP`, `SCHEMA_SEQUENCE`,
/// `REPLICATION_SEQUENCE`.
#[derive(Debug, Default)]
pub struct Summary {
    pub rows: HashMap<String, u64>,
    pub info: HashMap<String, String>,
}

enum Msg {
    Begin(String),
    Rows(Vec<u8>),
    End,
    Info(String, String),
    Failed(BuildError),
}

/// A stable 32-bit hash (FNV-1a) of a string, for the string indexes (they outlive the binary that wrote them, so
/// the hash must not change between versions).
#[must_use]
pub fn string_hash(s: &str) -> u32 {
    let mut h = 0x811c_9dc5u32;
    for &b in s.as_bytes() {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

/// Marks a track's title as its recording's.
pub const SAME_AS_RECORDING: u32 = u32::MAX;

/// Writes the tables of `sources` into `out` (a new, empty directory).
pub fn tables(sources: &[PathBuf], out: &Path) -> Result<Summary, BuildError> {
    let started = Instant::now();
    let specs = schema::all();
    let mut handles = Vec::new();
    let (results_tx, results_rx) = mpsc::channel();
    for source in sources {
        let (tx, rx) = mpsc::sync_channel::<Msg>(8);
        let (source, specs_r) = (source.clone(), specs.clone());
        handles.push(std::thread::spawn(move || {
            let began = Instant::now();
            let name = source
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            tracing::info!(source = name, "reading (decompressing as it goes)");
            match read_source(&source, &specs_r, &tx) {
                Ok(()) => {
                    tracing::info!(source = name, elapsed_s = began.elapsed().as_secs(), "read");
                }
                Err(e) => {
                    let _ = tx.send(Msg::Failed(e));
                }
            }
        }));
        let (out, specs_w, results_tx) = (out.to_owned(), specs.clone(), results_tx.clone());
        handles.push(std::thread::spawn(move || {
            let _ = results_tx.send(write_tables(&rx, &specs_w, &out));
        }));
    }
    drop(results_tx);
    let mut summary = Summary::default();
    let mut failure = None;
    for result in results_rx {
        match result {
            Ok(s) => {
                summary.rows.extend(s.rows);
                summary.info.extend(s.info);
            }
            Err(e) => failure = Some(failure.unwrap_or(e)),
        }
    }
    for h in handles {
        let _ = h.join();
    }
    if let Some(e) = failure {
        return Err(e);
    }
    for t in REQUIRED {
        if !summary.rows.contains_key(*t) {
            return Err(BuildError::Missing(t));
        }
    }
    tracing::info!(elapsed_s = started.elapsed().as_secs(), "tables written");
    Ok(summary)
}

fn write_tables(
    rx: &mpsc::Receiver<Msg>,
    specs: &HashMap<String, Table>,
    out: &Path,
) -> Result<Summary, BuildError> {
    let mut summary = Summary::default();
    let mut current: Option<TableWriter> = None;
    let mut began = Instant::now();
    for msg in rx {
        match msg {
            Msg::Begin(name) => {
                let spec = specs
                    .get(&name)
                    .ok_or_else(|| BuildError::Other(format!("unknown table {name}")))?;
                current = Some(TableWriter::create(spec, out)?);
                began = Instant::now();
            }
            Msg::Rows(data) => {
                if let Some(w) = &mut current {
                    w.rows(&data)?;
                }
            }
            Msg::End => {
                if let Some(w) = current.take() {
                    let (name, rows) = (w.spec.name.clone(), w.count);
                    let started = Instant::now();
                    w.finish(out)?;
                    tracing::info!(
                        table = name,
                        rows,
                        total_ms = began.elapsed().as_millis(),
                        finish_ms = started.elapsed().as_millis(),
                        "written"
                    );
                    *summary.rows.entry(name).or_default() += rows;
                }
            }
            Msg::Info(name, value) => {
                summary.info.insert(name, value);
            }
            Msg::Failed(e) => return Err(e),
        }
    }
    Ok(summary)
}

struct TableWriter {
    spec: Table,
    rows: RowsWriter,
    pool: Option<PoolWriter>,
    csr: Vec<CsrWriter>,
    sorted: Vec<HashIndexWriter>,
    count: u64,
    row: Vec<u8>,
    /// For tracks: the recordings (written before them, from the same archive), to recognize their titles.
    recordings: Option<(RowsFile, Pool, usize, Option<usize>)>,
}

impl TableWriter {
    fn create(spec: &Table, out: &Path) -> Result<Self, BuildError> {
        let io = |e| BuildError::Io(out.join(&spec.name), e);
        let base = out.join(&spec.name);
        let has_strings = spec
            .fields
            .iter()
            .any(|f| matches!(f.ty, Ty::Str | Ty::InternedStr));
        let interned = spec.fields.iter().any(|f| f.ty == Ty::InternedStr);
        let recordings = if spec.name == "track" {
            let all = schema::all();
            let recording = all.get("recording");
            let width = recording.map_or(0, |t| t.width);
            let name = recording
                .and_then(|t| t.field("name"))
                .map_or(0, |f| f.offset);
            let rows = RowsFile::open(&out.join("recording.rows"), width, None).map_err(io)?;
            let pool = Pool::open(&out.join("recording.strings")).map_err(io)?;
            Some((rows, pool, name, spec.field("recording").map(|f| f.column)))
        } else {
            None
        };
        Ok(Self {
            spec: spec.clone(),
            rows: RowsWriter::create(
                &with_ext(&base, "rows"),
                spec.width,
                spec.rows == Rows::Sequential,
            )
            .map_err(io)?,
            pool: if has_strings {
                Some(PoolWriter::create(&with_ext(&base, "strings"), interned).map_err(io)?)
            } else {
                None
            },
            csr: spec
                .csr
                .iter()
                .map(|(key, _)| CsrWriter::create(&with_ext(&base, key)))
                .collect::<Result<_, _>>()
                .map_err(io)?,
            sorted: spec
                .sorted
                .iter()
                .map(|field| HashIndexWriter::create(&with_ext(&base, &format!("{field}.hash"))))
                .collect(),
            count: 0,
            row: vec![0; spec.width],
            recordings,
        })
    }

    fn rows(&mut self, data: &[u8]) -> Result<(), BuildError> {
        let mut fields = Vec::with_capacity(self.spec.column_count);
        for line in data.split(|&b| b == b'\n').filter(|l| !l.is_empty()) {
            copy::split_row(line, &mut fields);
            if fields.len() != self.spec.column_count {
                return Err(BuildError::Columns {
                    table: self.spec.name.clone(),
                    got: fields.len(),
                    expected: self.spec.column_count,
                });
            }
            self.row(&fields)?;
            fields.clear();
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)] // a field of each type
    fn row(&mut self, fields: &[Option<std::borrow::Cow<'_, str>>]) -> Result<(), BuildError> {
        let col = |i: usize| fields.get(i).and_then(|f| f.as_deref());
        if let Some(required) = &self.spec.require
            && let Some(f) = self.spec.field(required)
            && col(f.column).is_none_or(|v| v == "0")
        {
            return Ok(());
        }
        let io = |e| BuildError::Io(PathBuf::from(&self.spec.name), e);
        let position = match self.spec.rows {
            Rows::ById(column) => col(column).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0),
            Rows::Sequential => self.rows.len(),
        };
        let row_id = u32::try_from(position)
            .map_err(|_| BuildError::Other(format!("{}: id too big", self.spec.name)))?;
        self.row.fill(0);
        for (i, f) in self.spec.fields.iter().enumerate() {
            let value = col(f.column);
            let bytes = self
                .row
                .get_mut(f.offset..f.offset + f.ty.width())
                .unwrap_or_default();
            match f.ty {
                Ty::Gid => bytes.copy_from_slice(&value.map_or(0, parse_uuid).to_le_bytes()),
                Ty::U32 => {
                    bytes.copy_from_slice(
                        &value
                            .and_then(|v| v.parse::<u32>().ok())
                            .unwrap_or(NONE)
                            .to_le_bytes(),
                    );
                }
                Ty::I32 => {
                    let v = value
                        .and_then(|v| v.parse::<i32>().ok())
                        .unwrap_or(i32::MIN);
                    bytes.copy_from_slice(&v.to_le_bytes());
                }
                Ty::U64 => {
                    bytes.copy_from_slice(
                        &value
                            .and_then(|v| v.parse::<u64>().ok())
                            .unwrap_or(u64::MAX)
                            .to_le_bytes(),
                    );
                }
                Ty::Bool => {
                    if let Some(b) = bytes.first_mut() {
                        *b = u8::from(value.and_then(copy::parse_bool).unwrap_or(false));
                    }
                }
                Ty::Str | Ty::InternedStr => {
                    let same_as_recording = f.name == "name"
                        && self.recordings.as_ref().is_some_and(
                            |(rows, pool, name_offset, column)| {
                                let recording =
                                    column.and_then(col).and_then(|r| r.parse::<u32>().ok());
                                let recording = recording.and_then(|r| rows.get(r));
                                recording.and_then(|r| pool.get(u32_at(&r, *name_offset))) == value
                            },
                        );
                    let offset = if same_as_recording {
                        SAME_AS_RECORDING
                    } else {
                        let pool = self
                            .pool
                            .as_mut()
                            .ok_or_else(|| BuildError::Other("no pool".into()))?;
                        pool.add(value, f.ty == Ty::InternedStr).map_err(io)?
                    };
                    bytes.copy_from_slice(&offset.to_le_bytes());
                }
                Ty::Date => {
                    let year = value
                        .and_then(|v| v.parse::<i16>().ok())
                        .unwrap_or(i16::MIN);
                    let month = col(f.column + 1)
                        .and_then(|v| v.parse::<u8>().ok())
                        .unwrap_or(0);
                    let day = col(f.column + 2)
                        .and_then(|v| v.parse::<u8>().ok())
                        .unwrap_or(0);
                    let [y0, y1] = year.to_le_bytes();
                    bytes.copy_from_slice(&[y0, y1, month, day]);
                }
            }
            let _ = i;
        }
        for (writer, (key, order)) in self.csr.iter_mut().zip(&self.spec.csr) {
            let key = self
                .spec
                .field(key)
                .map_or(NONE, |f| super::file::u32_at(&self.row, f.offset));
            let order = order
                .as_ref()
                .and_then(|o| self.spec.field(o))
                .map_or(0, |f| super::file::u32_at(&self.row, f.offset));
            writer.add(key, order, row_id).map_err(io)?;
        }
        for (index, field) in self.sorted.iter_mut().zip(&self.spec.sorted) {
            if let Some(f) = self.spec.field(field)
                && let Some(value) = col(f.column).filter(|v| !v.is_empty())
            {
                index.add(string_hash(value), row_id).map_err(io)?;
            }
        }
        self.rows.put(position, &self.row).map_err(io)?;
        self.count += 1;
        Ok(())
    }

    fn finish(self, out: &Path) -> Result<(), BuildError> {
        let base = out.join(&self.spec.name);
        let io = |e| BuildError::Io(base.clone(), e);
        self.rows.finish().map_err(io)?;
        if let Some(pool) = self.pool {
            pool.finish().map_err(io)?;
        }
        for (writer, (key, _)) in self.csr.into_iter().zip(&self.spec.csr) {
            writer.write(&with_ext(&base, key)).map_err(io)?;
        }
        for index in self.sorted {
            index.finish().map_err(io)?;
        }
        if self.spec.has_gid() {
            let gid = self
                .spec
                .fields
                .iter()
                .find(|f| f.ty == Ty::Gid)
                .map_or(0, |f| f.offset);
            let rows =
                RowsFile::open(&with_ext(&base, "rows"), self.spec.width, None).map_err(io)?;
            write_gid_index(&rows, gid, &with_ext(&base, "gids")).map_err(io)?;
        }
        Ok(())
    }
}

/// An MBID as a number (0 for anything that isn't one).
#[must_use]
pub fn parse_uuid(s: &str) -> u128 {
    let mut n = 0u128;
    let mut digits = 0;
    for c in s.chars() {
        if c == '-' {
            continue;
        }
        let Some(d) = c.to_digit(16) else { return 0 };
        n = (n << 4) | u128::from(d);
        digits += 1;
    }
    if digits == 32 { n } else { 0 }
}

/// An MBID as text.
#[must_use]
pub fn format_uuid(n: u128) -> String {
    let h = format!("{n:032x}");
    format!(
        "{}-{}-{}-{}-{}",
        h.get(0..8).unwrap_or(""),
        h.get(8..12).unwrap_or(""),
        h.get(12..16).unwrap_or(""),
        h.get(16..20).unwrap_or(""),
        h.get(20..32).unwrap_or("")
    )
}

fn read_source(
    source: &Path,
    specs: &HashMap<String, Table>,
    tx: &mpsc::SyncSender<Msg>,
) -> Result<(), BuildError> {
    let io = |e| BuildError::Io(source.to_owned(), e);
    if source.is_dir() {
        for name in ["TIMESTAMP", "SCHEMA_SEQUENCE", "REPLICATION_SEQUENCE"] {
            if let Ok(value) = std::fs::read_to_string(source.join(name)) {
                send(tx, Msg::Info(name.to_owned(), value.trim().to_owned()))?;
            }
        }
        let dir = source.join("mbdump");
        let mut entries: Vec<_> = std::fs::read_dir(&dir)
            .map_err(|e| BuildError::Io(dir.clone(), e))?
            .filter_map(Result::ok)
            .collect();
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let path = format!("mbdump/{}", entry.file_name().to_string_lossy());
            if let Some(table) = schema::file_table(&path).filter(|t| specs.contains_key(*t)) {
                let file = std::fs::File::open(entry.path())
                    .map_err(|e| BuildError::Io(entry.path(), e))?;
                send_table(table, file, tx)?;
            }
        }
        return Ok(());
    }
    let file = std::fs::File::open(source).map_err(io)?;
    let reader: Box<dyn Read> = if source.extension().is_some_and(|e| e == "tar") {
        Box::new(BufReader::with_capacity(1 << 20, file))
    } else {
        Box::new(bzip2::read::MultiBzDecoder::new(BufReader::with_capacity(
            1 << 20,
            file,
        )))
    };
    let mut archive = tar::Archive::new(reader);
    for entry in archive.entries().map_err(io)? {
        let mut entry = entry.map_err(io)?;
        let path = entry.path().map_err(io)?.to_string_lossy().into_owned();
        if let Some(table) = schema::file_table(&path).filter(|t| specs.contains_key(*t)) {
            let table = table.to_owned();
            send_table(&table, &mut entry, tx)?;
        } else if matches!(
            path.as_str(),
            "TIMESTAMP" | "SCHEMA_SEQUENCE" | "REPLICATION_SEQUENCE"
        ) {
            let mut value = String::new();
            entry.read_to_string(&mut value).map_err(io)?;
            send(tx, Msg::Info(path, value.trim().to_owned()))?;
        }
    }
    Ok(())
}

fn send_table(
    table: &str,
    reader: impl Read,
    tx: &mpsc::SyncSender<Msg>,
) -> Result<(), BuildError> {
    send(tx, Msg::Begin(table.to_owned()))?;
    let mut reader = BufReader::with_capacity(1 << 20, reader);
    loop {
        let mut data = Vec::with_capacity(CHUNK + 4096);
        while data.len() < CHUNK {
            let n = reader
                .read_until(b'\n', &mut data)
                .map_err(|e| BuildError::Io(PathBuf::from(table), e))?;
            if n == 0 {
                break;
            }
        }
        if data.is_empty() {
            break;
        }
        send(tx, Msg::Rows(data))?;
    }
    send(tx, Msg::End)
}

fn send(tx: &mpsc::SyncSender<Msg>, msg: Msg) -> Result<(), BuildError> {
    tx.send(msg).map_err(|_| BuildError::Interrupted)
}

/// How a store is built.
#[derive(Debug, Clone)]
pub struct BuildOptions {
    /// Search indexing threads (and the threads that compress rows).
    pub threads: usize,
    /// The search indexers' memory.
    pub memory_mb: usize,
    /// Rows compressed in blocks (see `file::Rows`).
    pub compress_rows: bool,
}

/// Tables with fewer bytes of rows than this stay plain when rows are compressed.
const COMPRESS_MIN: u64 = 64 << 10;
/// zstd's level for rows.
const COMPRESS_LEVEL: i32 = 3;

/// The steps of a build, in order. Each is recorded in `build.json` once its files are on disk, and an interrupted
/// build continues after the last one recorded. Only the first reads the dump.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Step {
    /// The directory is the build's (nothing in it to keep yet).
    Started,
    /// The tables, from the dump.
    Tables,
    /// The derived values (`derive.rs`).
    Derived,
    /// The search indexes.
    Search,
}

/// The record of a build in progress (`build.json`, removed when the store is finished).
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct Progress {
    dump: String,
    /// The program that built the steps so far: another one continues nothing.
    version: String,
    format_version: u32,
    done: Step,
    /// From the tables: the dump's `TIMESTAMP`, `SCHEMA_SEQUENCE`, `REPLICATION_SEQUENCE`; rows per table.
    info: HashMap<String, String>,
    rows: HashMap<String, u64>,
}

const PROGRESS: &str = "build.json";
const VERSION: &str = env!("MUSICBRAINZ_MIRROR_VERSION");

impl Progress {
    fn read(out: &Path, dump: &str) -> Option<Self> {
        let progress: Self =
            serde_json::from_slice(&std::fs::read(out.join(PROGRESS)).ok()?).ok()?;
        (progress.dump == dump
            && progress.version == VERSION
            && progress.format_version == super::FORMAT_VERSION)
            .then_some(progress)
    }

    /// Records a finished step, once everything it wrote is on disk.
    fn done(&mut self, out: &Path, step: Step) -> Result<(), BuildError> {
        self.done = step;
        let io = |e| BuildError::Io(out.join(PROGRESS), e);
        super::file::sync_filesystem(out).map_err(io)?;
        let json = serde_json::to_vec_pretty(self).map_err(|e| BuildError::Other(e.to_string()))?;
        super::file::write_durably(&out.join(PROGRESS), &json).map_err(io)
    }

    fn summary(&self) -> Summary {
        Summary {
            rows: self.rows.clone(),
            info: self.info.clone(),
        }
    }
}

/// Whether the build into `out` finished: it has a `meta.json` (written last).
#[must_use]
pub fn finished(out: &Path) -> bool {
    out.join("meta.json").exists()
}

/// Whether continuing a build of `dump` into `out` reads the dump: not when it is finished or past its tables.
#[must_use]
pub fn needs_sources(out: &Path, dump: &str) -> bool {
    !finished(out) && Progress::read(out, dump).is_none_or(|p| p.done < Step::Tables)
}

/// Builds a complete store into `out` (created: it must not exist): the tables, the derived files, the search
/// indexes, the rows compressed (when asked), and `meta.json` last (a store without it is unfinished), once the rest
/// is on disk.
pub fn build(
    sources: &[PathBuf],
    dump: &str,
    out: &Path,
    options: &BuildOptions,
) -> Result<Summary, BuildError> {
    run(sources, dump, out, options, false, None)
}

/// Continues a build of `dump` into `out` that was interrupted, after its last finished step; a build of something
/// else (another dump, another version of this program) or one interrupted before its first step is started over.
/// Without `out`, the same as [`build`]. A finished store (or a directory that isn't a build's) is left alone:
/// [`BuildError::Exists`].
pub fn resume(
    sources: &[PathBuf],
    dump: &str,
    out: &Path,
    options: &BuildOptions,
) -> Result<Summary, BuildError> {
    run(sources, dump, out, options, true, None)
}

fn run(
    sources: &[PathBuf],
    dump: &str,
    out: &Path,
    options: &BuildOptions,
    resume: bool,
    stop_after: Option<Step>,
) -> Result<Summary, BuildError> {
    let started = Instant::now();
    let io = |e| BuildError::Io(out.to_owned(), e);
    let mut progress = begin(dump, out, resume)?;
    let stop = |progress: &Progress| stop_after.is_some_and(|s| progress.done >= s);
    if stop(&progress) {
        return Ok(progress.summary());
    }
    if progress.done < Step::Tables {
        // What an interrupted run of this step left.
        clear(out).map_err(io)?;
        let summary = tables(sources, out)?;
        progress.info = summary.info;
        progress.rows = summary.rows;
        progress.done(out, Step::Tables)?;
    }
    if stop(&progress) {
        return Ok(progress.summary());
    }
    if progress.done < Step::Derived {
        // Without the derived files an interrupted run of this step left: they are rewritten, not read.
        let store = super::Store::tables_only(out).map_err(io)?;
        super::derive::build(&store).map_err(io)?;
        drop(store);
        tracing::info!(
            elapsed_s = started.elapsed().as_secs(),
            "derived values written"
        );
        progress.done(out, Step::Derived)?;
    }
    if stop(&progress) {
        return Ok(progress.summary());
    }
    if progress.done < Step::Search {
        let search = out.join("search");
        if let Err(e) = std::fs::remove_dir_all(&search)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            return Err(BuildError::Io(search, e));
        }
        // With the derived values: the search documents use them.
        let store = super::Store::partial(out).map_err(io)?;
        super::search::build(&store, &search, options.threads, options.memory_mb)
            .map_err(|e| BuildError::Other(format!("search index: {e}")))?;
        drop(store);
        tracing::info!(
            elapsed_s = started.elapsed().as_secs(),
            "search indexes built"
        );
        progress.done(out, Step::Search)?;
    }
    if stop(&progress) {
        return Ok(progress.summary());
    }
    // Table by table, each finished before its plain rows are removed: an interrupted run leaves tables to
    // compress (again), never a table without rows. What it left of a table it didn't finish is removed (the
    // compressed rows are read when they are there), whether or not this run compresses that table.
    for spec in schema::all().into_values() {
        let rows = with_ext(&out.join(&spec.name), "rows");
        if rows.exists() {
            for ext in ["z", "zi", "gid"] {
                let path = with_ext(&rows, ext);
                if let Err(e) = std::fs::remove_file(&path)
                    && e.kind() != std::io::ErrorKind::NotFound
                {
                    return Err(BuildError::Io(path, e));
                }
            }
        }
    }
    if options.compress_rows {
        compress(out, options.threads)?;
        tracing::info!(elapsed_s = started.elapsed().as_secs(), "rows compressed");
    }
    let mut files = super::manifest(out).map_err(io)?;
    // (removed once the store is finished)
    files.remove(PROGRESS);
    let json = serde_json::to_vec_pretty(&files).map_err(|e| BuildError::Other(e.to_string()))?;
    super::file::write_durably(&out.join(super::MANIFEST), &json)
        .map_err(|e| BuildError::Io(out.join(super::MANIFEST), e))?;
    let mut meta: std::collections::BTreeMap<String, String> = progress
        .info
        .iter()
        .map(|(k, v)| (k.to_lowercase(), v.clone()))
        .collect();
    meta.insert("dump".into(), dump.to_owned());
    meta.insert("format_version".into(), super::FORMAT_VERSION.to_string());
    let json = serde_json::to_vec_pretty(&meta).map_err(|e| BuildError::Other(e.to_string()))?;
    // Every file of the store on disk before the mark of a finished store: after a power loss, a store with a
    // `meta.json` has all of its contents.
    super::file::sync_filesystem(out).map_err(io)?;
    super::file::write_durably(&out.join("meta.json"), &json)
        .map_err(|e| BuildError::Io(out.join("meta.json"), e))?;
    std::fs::remove_file(out.join(PROGRESS)).map_err(io)?;
    tracing::info!(elapsed_s = started.elapsed().as_secs(), "store built");
    Ok(progress.summary())
}

/// The build's directory and its progress: a new directory, or (`resume`) the one of an interrupted build.
fn begin(dump: &str, out: &Path, resume: bool) -> Result<Progress, BuildError> {
    let io = |e| BuildError::Io(out.to_owned(), e);
    if resume && out.exists() {
        if finished(out) {
            return Err(BuildError::Exists(out.to_owned()));
        }
        if let Some(progress) = Progress::read(out, dump) {
            tracing::info!(after = ?progress.done, "continuing an interrupted build");
            return Ok(progress);
        }
        // A build's directory has a `build.json` from its start (or nothing yet, when it was interrupted before
        // writing it).
        let ours = out.join(PROGRESS).exists()
            || std::fs::read_dir(out)
                .map_err(io)?
                .filter_map(Result::ok)
                .all(|e| e.file_name() == *format!("{PROGRESS}.tmp"));
        if !ours {
            return Err(BuildError::Exists(out.to_owned()));
        }
        // Recorded as started before anything is removed (by the first step), so that it stays a build's.
        tracing::info!("starting an interrupted build of something else over");
        return start(dump, out);
    }
    // A new directory, never an existing one: its files are created by truncating whatever has their names, which
    // under a store being served (memory-mapped) would change the bytes it reads.
    if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| BuildError::Io(parent.to_owned(), e))?;
    }
    std::fs::create_dir(out).map_err(|e| match e.kind() {
        std::io::ErrorKind::AlreadyExists => BuildError::Exists(out.to_owned()),
        _ => io(e),
    })?;
    start(dump, out)
}

/// Records a build of `dump` as started in `out`.
fn start(dump: &str, out: &Path) -> Result<Progress, BuildError> {
    let mut progress = Progress {
        dump: dump.to_owned(),
        version: VERSION.to_owned(),
        format_version: super::FORMAT_VERSION,
        done: Step::Started,
        info: HashMap::new(),
        rows: HashMap::new(),
    };
    progress.done(out, Step::Started)?;
    Ok(progress)
}

/// Removes everything from a build's directory but its `build.json`.
fn clear(out: &Path) -> std::io::Result<()> {
    for entry in std::fs::read_dir(out)? {
        let entry = entry?;
        if entry.file_name() == PROGRESS {
            continue;
        }
        if entry.file_type()?.is_dir() {
            std::fs::remove_dir_all(entry.path())?;
        } else {
            std::fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

/// Compresses the rows of every table but the smallest, the largest first, on `threads` threads.
fn compress(out: &Path, threads: usize) -> Result<(), BuildError> {
    let mut work: Vec<(u64, PathBuf, usize, Option<usize>)> = schema::all()
        .into_values()
        .filter_map(|spec| {
            let path = with_ext(&out.join(&spec.name), "rows");
            let size = std::fs::metadata(&path).map_or(0, |m| m.len());
            let gid = spec
                .fields
                .iter()
                .find(|f| f.ty == Ty::Gid)
                .map(|f| f.offset);
            (size >= COMPRESS_MIN).then_some((size, path, spec.width, gid))
        })
        .collect();
    work.sort_by_key(|w| std::cmp::Reverse(w.0));
    let next = std::sync::atomic::AtomicUsize::new(0);
    let failure = std::sync::Mutex::new(None);
    std::thread::scope(|scope| {
        for _ in 0..threads.max(1) {
            scope.spawn(|| {
                while let Some((_, path, width, gid)) =
                    work.get(next.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
                {
                    if let Err(e) = super::file::compress_rows(path, *width, *gid, COMPRESS_LEVEL) {
                        if let Ok(mut f) = failure.lock() {
                            f.get_or_insert(BuildError::Io(path.clone(), e));
                        }
                        return;
                    }
                }
            });
        }
    });
    match failure.into_inner() {
        Ok(Some(e)) => Err(e),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_builds_into_an_existing_directory() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/tmp/build-exists");
        std::fs::create_dir_all(&dir).unwrap();
        let options = BuildOptions {
            threads: 1,
            memory_mb: 50,
            compress_rows: false,
        };
        let result = build(&[dir.join("nothing")], "x", &dir, &options);
        assert!(matches!(result, Err(BuildError::Exists(_))));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn fixture() -> Vec<PathBuf> {
        vec![PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dump")]
    }

    fn temp(name: &str) -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/tmp")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    const COMPRESSED: BuildOptions = BuildOptions {
        threads: 1,
        memory_mb: 50,
        compress_rows: true,
    };

    /// The files of a store and their contents, but the search indexes' (tantivy's files aren't the same from one
    /// build to the next): the number of documents of each.
    fn contents(dir: &Path) -> std::collections::BTreeMap<String, Vec<u8>> {
        let mut files = std::collections::BTreeMap::new();
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == "search" {
                for index in std::fs::read_dir(entry.path()).unwrap() {
                    let index = index.unwrap();
                    let docs = tantivy::Index::open_in_dir(index.path())
                        .unwrap()
                        .reader()
                        .unwrap()
                        .searcher()
                        .num_docs();
                    files.insert(
                        format!("search/{}", index.file_name().to_string_lossy()),
                        docs.to_le_bytes().to_vec(),
                    );
                }
            } else if name == "files.json" {
                // The manifest, without the search indexes' files (named at random).
                let mut manifest: std::collections::BTreeMap<String, u64> =
                    serde_json::from_slice(&std::fs::read(entry.path()).unwrap()).unwrap();
                manifest.retain(|k, _| !k.starts_with("search/"));
                files.insert(name, serde_json::to_vec(&manifest).unwrap());
            } else {
                files.insert(name, std::fs::read(entry.path()).unwrap());
            }
        }
        files
    }

    fn assert_same(dir: &Path, expected: &std::collections::BTreeMap<String, Vec<u8>>) {
        let got = contents(dir);
        let names =
            |m: &std::collections::BTreeMap<String, Vec<u8>>| m.keys().cloned().collect::<Vec<_>>();
        assert_eq!(names(&got), names(expected));
        for (name, bytes) in expected {
            assert!(got.get(name) == Some(bytes), "{name} differs");
        }
    }

    #[test]
    fn resume_leaves_finished_stores_and_other_directories_alone() {
        let dir = temp("resume-alone");
        let finished = dir.join("finished");
        std::fs::create_dir(&finished).unwrap();
        std::fs::write(finished.join("meta.json"), "{}").unwrap();
        let result = resume(&fixture(), "x", &finished, &COMPRESSED);
        assert!(matches!(result, Err(BuildError::Exists(_))));
        let other = dir.join("other");
        std::fs::create_dir(&other).unwrap();
        std::fs::write(other.join("file"), "kept").unwrap();
        let result = resume(&fixture(), "x", &other, &COMPRESSED);
        assert!(matches!(result, Err(BuildError::Exists(_))));
        assert_eq!(std::fs::read(other.join("file")).unwrap(), b"kept");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A build stopped after each step, with what an interrupted run of the next step leaves, continues to the same
    /// store as a build that wasn't interrupted.
    #[test]
    fn an_interrupted_build_continues_to_the_same_store() {
        let dir = temp("resume");
        let reference = dir.join("reference");
        build(&fixture(), "fixture", &reference, &COMPRESSED).unwrap();
        let expected = contents(&reference);
        assert!(!expected.contains_key(PROGRESS));

        for step in [Step::Started, Step::Tables, Step::Derived, Step::Search] {
            let out = dir.join(format!("{step:?}"));
            run(&fixture(), "fixture", &out, &COMPRESSED, false, Some(step)).unwrap();
            assert!(!finished(&out));
            assert_eq!(needs_sources(&out, "fixture"), step < Step::Tables);
            match step {
                // Tables half written.
                Step::Started => {
                    std::fs::write(out.join("artist.rows"), b"partial").unwrap();
                    std::fs::write(out.join("artist.rows.tmp3"), b"partial").unwrap();
                }
                // The derived values written (some tables rewritten), but not recorded.
                Step::Tables => {
                    let store = crate::store::Store::tables_only(&out).unwrap();
                    crate::store::derive::build(&store).unwrap();
                }
                // A search index half built.
                Step::Derived => {
                    std::fs::create_dir_all(out.join("search/artist")).unwrap();
                    std::fs::write(out.join("search/artist/meta.json"), b"{").unwrap();
                }
                // A table half compressed (the fixture's only table big enough to be compressed), and another one
                // (that isn't compressed now).
                Step::Search => {
                    for table in ["language", "artist"] {
                        let base = out.join(table);
                        std::fs::write(with_ext(&base, "rows.zi"), b"partial").unwrap();
                        std::fs::write(with_ext(&base, "rows.z"), b"partial").unwrap();
                    }
                }
            }
            resume(&fixture(), "fixture", &out, &COMPRESSED).unwrap();
            assert_same(&out, &expected);
            crate::store::Store::open(&out).unwrap();
        }

        // A build of another dump, or by another version, is started over (and needs the dump).
        for (key, value) in [("dump", "other"), ("version", "0.0.0")] {
            let out = dir.join(key);
            run(
                &fixture(),
                "fixture",
                &out,
                &COMPRESSED,
                false,
                Some(Step::Started),
            )
            .unwrap();
            std::fs::write(out.join("artist.rows"), b"partial").unwrap();
            let path = out.join(PROGRESS);
            let mut progress: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            progress["done"] = "tables".into();
            std::fs::write(&path, serde_json::to_vec(&progress).unwrap()).unwrap();
            assert!(!needs_sources(&out, "fixture"));
            progress[key] = value.into();
            std::fs::write(&path, serde_json::to_vec(&progress).unwrap()).unwrap();
            assert!(needs_sources(&out, "fixture"));
            run(
                &fixture(),
                "fixture",
                &out,
                &COMPRESSED,
                true,
                Some(Step::Started),
            )
            .unwrap();
            let progress = Progress::read(&out, "fixture").unwrap();
            assert_eq!(progress.done, Step::Started);
        }

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn uuids() {
        let gid = "b84ee12a-09ef-421b-82de-0441a926375b";
        assert_eq!(format_uuid(parse_uuid(gid)), gid);
        assert_eq!(
            parse_uuid("B84EE12A-09EF-421B-82DE-0441A926375B"),
            parse_uuid(gid)
        );
        assert_eq!(parse_uuid("x"), 0);
    }
}
