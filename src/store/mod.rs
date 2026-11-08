//! The store: a MusicBrainz dump as immutable, memory-mapped files (see `file.rs`), built by `build.rs` and
//! `derive.rs` by the definitions of `schema.rs`, searched through `search.rs`.

pub mod build;
pub mod copy;
pub mod derive;
pub mod file;
pub mod schema;
pub mod search;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use self::build::{SAME_AS_RECORDING, format_uuid, parse_uuid, string_hash};
use self::file::{Csr, GidIndex, HashIndex, NONE, Pool, RowBuf, Rows, u32_at, u128_at, with_ext};
use self::schema::{Rows as RowsKind, Ty};
use crate::kind::Kind;

/// The version of the store's layout, in `meta.json`. A store of another version is not opened (it is rebuilt from
/// the next dump).
pub const FORMAT_VERSION: u32 = 3;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("{0}: {1}")]
    Io(PathBuf, std::io::Error),
    #[error("{0}: store format {1}, this version reads {FORMAT_VERSION}")]
    Version(PathBuf, u32),
    #[error("{0}: {1}")]
    Search(PathBuf, String),
    #[error("{0}: damaged: {1}")]
    Damaged(PathBuf, String),
}

impl StoreError {
    /// Whether the store is shown to be unusable for good: unfinished, a file of it missing, of another size than
    /// it was built with, or found corrupt. The other errors say nothing about the store (they can come from the
    /// system: memory maps, file descriptors, permissions, a failing disk), and another version's store is whole.
    #[must_use]
    pub fn is_damage(&self) -> bool {
        matches!(self, Self::Damaged(..))
    }
}

/// The manifest of a store: every file of it (but `meta.json` and the manifest) and its size, written before
/// `meta.json`. Stores built before manifests existed have none, and are opened without that check.
pub const MANIFEST: &str = "files.json";

/// The files of a store and their sizes, by path from its directory, for its manifest. tantivy's lock files are left
/// out: readers create them.
pub fn manifest(dir: &Path) -> std::io::Result<std::collections::BTreeMap<String, u64>> {
    fn walk(
        dir: &Path,
        prefix: &str,
        files: &mut std::collections::BTreeMap<String, u64>,
    ) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let name = format!("{prefix}{}", entry.file_name().to_string_lossy());
            let meta = entry.metadata()?;
            if meta.is_dir() {
                walk(&entry.path(), &format!("{name}/"), files)?;
            } else if !(name == "meta.json"
                || name == MANIFEST
                || entry.file_name().to_string_lossy().starts_with(".tantivy-"))
            {
                files.insert(name, meta.len());
            }
        }
        Ok(())
    }
    let mut files = std::collections::BTreeMap::new();
    walk(dir, "", &mut files)?;
    Ok(files)
}

/// Checks that every file of a store's manifest is there with its size.
fn check_manifest(dir: &Path) -> Result<(), StoreError> {
    let path = dir.join(MANIFEST);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(StoreError::Io(path, e)),
    };
    let files: std::collections::BTreeMap<String, u64> = serde_json::from_slice(&bytes)
        .map_err(|e| StoreError::Damaged(path.clone(), e.to_string()))?;
    for (name, size) in files {
        let file = dir.join(&name);
        match std::fs::metadata(&file) {
            Ok(m) if m.is_file() && m.len() == size => {}
            Ok(m) => {
                return Err(StoreError::Damaged(
                    file,
                    format!("{} bytes, {size} when it was built", m.len()),
                ));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(StoreError::Damaged(file, "missing".into()));
            }
            Err(e) => return Err(StoreError::Io(file, e)),
        }
    }
    Ok(())
}

/// A table of the store.
pub struct Table {
    pub spec: schema::Table,
    rows: Rows,
    pool: Pool,
    csr: HashMap<String, Csr>,
    sorted: HashMap<String, HashIndex>,
    gids: Option<(GidIndex, usize)>,
    /// The field holding a row's id, to tell rows that exist from holes (tables without MBIDs).
    id_field: Option<usize>,
}

impl Table {
    fn open(dir: &Path, spec: schema::Table) -> std::io::Result<Self> {
        let base = dir.join(&spec.name);
        let csr = spec
            .csr
            .iter()
            .map(|(key, _)| Ok((key.clone(), Csr::open(&with_ext(&base, key))?)))
            .collect::<std::io::Result<_>>()?;
        let sorted = spec
            .sorted
            .iter()
            .map(|f| {
                Ok((
                    f.clone(),
                    HashIndex::open(&with_ext(&base, &format!("{f}.hash")))?,
                ))
            })
            .collect::<std::io::Result<_>>()?;
        let gids = if spec.has_gid() {
            let offset = spec
                .fields
                .iter()
                .find(|f| f.ty == Ty::Gid)
                .map_or(0, |f| f.offset);
            Some((GidIndex::open(&with_ext(&base, "gids"))?, offset))
        } else {
            None
        };
        let id_field = match spec.rows {
            RowsKind::ById(column) if !spec.has_gid() => spec
                .fields
                .iter()
                .find(|f| f.column == column)
                .map(|f| f.offset),
            _ => None,
        };
        Ok(Self {
            rows: Rows::open(
                &with_ext(&base, "rows"),
                spec.width,
                gids.as_ref().map(|g| g.1),
            )?,
            pool: Pool::open(&with_ext(&base, "strings"))?,
            csr,
            sorted,
            gids,
            id_field,
            spec,
        })
    }

    /// A row by id (by position for tables in dump order).
    #[must_use]
    pub fn get(&self, id: u32) -> Option<Row<'_>> {
        if id == NONE {
            return None;
        }
        // An MBID is read without the rest of the row: a hole is found without decompressing anything.
        if let Some((_, offset)) = &self.gids
            && self.rows.gid(id, *offset) == 0
        {
            return None;
        }
        let bytes = self.rows.get(id)?;
        // Without MBIDs, a row of a table by id exists when its id field is its id; every position of a table in
        // dump order is a row.
        let exists = match (&self.gids, self.id_field) {
            (None, Some(offset)) => u32_at(&bytes, offset) == id,
            _ => true,
        };
        #[cfg(feature = "record")]
        if exists {
            record::touch(&self.spec.name, id);
        }
        exists.then_some(Row {
            table: self,
            id,
            bytes,
        })
    }

    /// The rows whose `key` field is `value` (by a CSR index).
    pub fn by(&self, key: &str, value: u32) -> impl Iterator<Item = Row<'_>> + '_ {
        self.csr
            .get(key)
            .into_iter()
            .flat_map(move |csr| csr.get(value))
            .filter_map(|id| self.get(id))
    }

    /// The rows whose string `field` is `value` (by a string index).
    #[must_use]
    pub fn find(&self, field: &str, value: &str) -> Vec<Row<'_>> {
        let Some(index) = self.sorted.get(field) else {
            return Vec::new();
        };
        index
            .rows(string_hash(value))
            .filter_map(|id| self.get(id))
            .filter(|r| r.str(field) == Some(value))
            .collect()
    }

    /// The row with this MBID.
    #[must_use]
    pub fn by_gid(&self, gid: &str) -> Option<Row<'_>> {
        let (index, offset) = self.gids.as_ref()?;
        let gid = parse_uuid(gid);
        if gid == 0 {
            return None;
        }
        index
            .find(&self.rows, *offset, gid)
            .and_then(|id| self.get(id))
    }

    /// The number of row slots (rows, and holes in tables by id).
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.len() == 0
    }

    /// Every row (ids in order).
    pub fn iter(&self) -> impl Iterator<Item = Row<'_>> + '_ {
        (0..u32::try_from(self.rows.len()).unwrap_or(u32::MAX))
            .filter_map(|p| self.get(self.rows.id_at(p)))
    }
}

/// A row of a table.
#[derive(Clone, Copy)]
pub struct Row<'a> {
    pub table: &'a Table,
    pub id: u32,
    bytes: RowBuf,
}

impl<'a> Row<'a> {
    fn field(&self, name: &str) -> Option<&'a schema::Field> {
        let field = self.table.spec.field(name);
        debug_assert!(field.is_some(), "{}: no field {name}", self.table.spec.name);
        field
    }

    /// An integer field; `None` for NULL.
    #[must_use]
    pub fn u32(&self, name: &str) -> Option<u32> {
        let f = self.field(name)?;
        Some(u32_at(&self.bytes, f.offset)).filter(|v| *v != NONE)
    }

    /// A signed integer field; `None` for NULL.
    #[must_use]
    pub fn i32(&self, name: &str) -> Option<i32> {
        let f = self.field(name)?;
        Some(u32_at(&self.bytes, f.offset).cast_signed()).filter(|v| *v != i32::MIN)
    }

    pub fn i64(&self, name: &str) -> Option<i64> {
        self.u32(name).map(i64::from)
    }

    pub fn u64(&self, name: &str) -> Option<u64> {
        let f = self.field(name)?;
        self.bytes
            .get(f.offset..f.offset + 8)
            .and_then(|b| b.try_into().ok())
            .map(u64::from_le_bytes)
            .filter(|v| *v != u64::MAX)
    }

    #[must_use]
    pub fn bool(&self, name: &str) -> bool {
        self.field(name)
            .and_then(|f| self.bytes.get(f.offset))
            .is_some_and(|b| *b != 0)
    }

    /// A string field; `None` for NULL.
    #[must_use]
    pub fn str(&self, name: &str) -> Option<&'a str> {
        let f = self.field(name)?;
        let offset = u32_at(&self.bytes, f.offset);
        if offset == SAME_AS_RECORDING {
            return None;
        }
        self.table.pool.get(offset)
    }

    /// A string field's raw offset (to tell a track's own title from its recording's).
    #[must_use]
    pub fn str_offset(&self, name: &str) -> u32 {
        self.field(name)
            .map_or(NONE, |f| u32_at(&self.bytes, f.offset))
    }

    /// A string field, empty for NULL.
    #[must_use]
    pub fn text(&self, name: &str) -> String {
        self.str(name).unwrap_or_default().to_owned()
    }

    /// A date field: year, month, day.
    #[must_use]
    pub fn date(&self, name: &str) -> (Option<i64>, Option<i64>, Option<i64>) {
        let Some(f) = self.field(name) else {
            return (None, None, None);
        };
        let Some(&[y0, y1, month, day]) = self.bytes.get(f.offset..f.offset + 4) else {
            return (None, None, None);
        };
        let year = i16::from_le_bytes([y0, y1]);
        (
            (year != i16::MIN).then_some(i64::from(year)),
            (month != 0).then_some(i64::from(month)),
            (day != 0).then_some(i64::from(day)),
        )
    }

    /// The row's MBID, as text.
    #[must_use]
    pub fn gid(&self) -> String {
        self.table
            .gids
            .as_ref()
            .map(|(_, offset)| format_uuid(u128_at(&self.bytes, *offset)))
            .unwrap_or_default()
    }
}

/// Which rows answering requests read (for the compatibility test's fixture, `examples/fixture.rs`).
#[cfg(feature = "record")]
pub mod record {
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Mutex;

    static ROWS: Mutex<BTreeMap<String, BTreeSet<u32>>> = Mutex::new(BTreeMap::new());

    pub(super) fn touch(table: &str, id: u32) {
        if let Ok(mut rows) = ROWS.lock() {
            if let Some(ids) = rows.get_mut(table) {
                ids.insert(id);
            } else {
                rows.insert(table.to_owned(), BTreeSet::from([id]));
            }
        }
    }

    /// The rows of a table read so far.
    pub fn of(table: &str) -> BTreeSet<u32> {
        ROWS.lock()
            .ok()
            .and_then(|rows| rows.get(table).cloned())
            .unwrap_or_default()
    }

    /// The rows read so far, by table (and forgets them).
    pub fn take() -> BTreeMap<String, BTreeSet<u32>> {
        ROWS.lock()
            .map(|mut rows| std::mem::take(&mut *rows))
            .unwrap_or_default()
    }
}

/// An open store.
pub struct Store {
    pub dir: PathBuf,
    pub meta: HashMap<String, String>,
    tables: HashMap<String, Table>,
    pub derived: derive::Derived,
    pub search: search::SearchIndex,
}

impl Store {
    /// Opens a finished store.
    pub fn open(dir: &Path) -> Result<Self, StoreError> {
        let io = |e| StoreError::Io(dir.to_owned(), e);
        let meta_path = dir.join("meta.json");
        // A store is moved under its name once finished, `meta.json` written last.
        let bytes = std::fs::read(&meta_path).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => {
                StoreError::Damaged(meta_path.clone(), "missing (an unfinished store)".into())
            }
            _ => StoreError::Io(meta_path.clone(), e),
        })?;
        let meta: HashMap<String, String> = serde_json::from_slice(&bytes)
            .map_err(|e| StoreError::Damaged(meta_path.clone(), e.to_string()))?;
        let version = meta
            .get("format_version")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        if version != FORMAT_VERSION {
            return Err(StoreError::Version(dir.to_owned(), version));
        }
        check_manifest(dir)?;
        let tables = Self::open_tables(dir).map_err(io)?;
        let derived = derive::Derived::open(dir).map_err(io)?;
        let search = search::SearchIndex::open(&dir.join("search")).map_err(|e| match e {
            tantivy::TantivyError::DataCorruption(_) => {
                StoreError::Damaged(dir.join("search"), e.to_string())
            }
            _ => StoreError::Search(dir.join("search"), e.to_string()),
        })?;
        Ok(Self {
            dir: dir.to_owned(),
            meta,
            tables,
            derived,
            search,
        })
    }

    /// The tables only (while building the derived files and the search index).
    pub fn open_tables(dir: &Path) -> std::io::Result<HashMap<String, Table>> {
        schema::all()
            .into_iter()
            .map(|(name, spec)| Ok((name, Table::open(dir, spec)?)))
            .collect()
    }

    /// A store of tables only, for the derived values.
    pub fn tables_only(dir: &Path) -> std::io::Result<Self> {
        Ok(Self {
            dir: dir.to_owned(),
            meta: HashMap::new(),
            tables: Self::open_tables(dir)?,
            derived: derive::Derived::default(),
            search: search::SearchIndex::default(),
        })
    }

    /// A store of tables and derived values, for the search indexes.
    pub fn partial(dir: &Path) -> std::io::Result<Self> {
        Ok(Self {
            dir: dir.to_owned(),
            meta: HashMap::new(),
            tables: Self::open_tables(dir)?,
            derived: derive::Derived::open(dir).unwrap_or_default(),
            search: search::SearchIndex::default(),
        })
    }

    /// The dump the store was built from.
    pub fn dump(&self) -> &str {
        self.meta.get("dump").map_or("", String::as_str)
    }

    /// A table by name. Every table of the schema exists (empty if the dump had none of it).
    #[allow(clippy::missing_panics_doc)]
    #[must_use]
    pub fn t(&self, name: &str) -> &Table {
        // Names are the schema's (a typo is a programming error caught by any test that goes through it).
        #[allow(clippy::expect_used)]
        self.tables.get(name).expect("a table of the schema")
    }

    /// An entity by id.
    #[must_use]
    pub fn entity(&self, kind: Kind, id: u32) -> Option<Row<'_>> {
        self.t(kind.table()).get(id)
    }

    /// An entity's id by its MBID, or by an MBID merged into it.
    #[must_use]
    pub fn resolve(&self, table: &str, kind: Option<Kind>, mbid: &str) -> Option<u32> {
        if let Some(row) = self.t(table).by_gid(mbid) {
            return Some(row.id);
        }
        let kind = kind?;
        self.t(&format!("{}_gid_redirect", kind.table()))
            .by_gid(mbid)
            .and_then(|r| r.u32("new_id"))
    }

    /// A lookup table's name and MBID by id (`artist_type`, `gender`, ...).
    #[must_use]
    pub fn named(&self, table: &str, id: Option<u32>) -> Option<(&str, Option<String>)> {
        let row = self.t(table).get(id?)?;
        let gid = row.table.gids.is_some().then(|| row.gid());
        Some((row.str("name").unwrap_or_default(), gid))
    }

    /// A lookup table's id by name, case-insensitively.
    #[must_use]
    pub fn id_by_name(&self, table: &str, name: &str) -> Option<u32> {
        self.t(table)
            .iter()
            .find(|r| r.str("name").is_some_and(|n| n.eq_ignore_ascii_case(name)))
            .map(|r| r.id)
    }

    /// An alias type of an entity type.
    #[must_use]
    pub fn alias_type(&self, kind: Kind, id: Option<u32>) -> Option<(&str, Option<String>)> {
        self.named(&format!("{}_alias_type", kind.table()), id)
    }

    /// A language's ISO 639-3 code.
    #[must_use]
    pub fn language(&self, id: Option<u32>) -> Option<&str> {
        self.t("language").get(id?)?.str("iso_code_3")
    }

    /// A script's ISO 15924 code.
    #[must_use]
    pub fn script(&self, id: Option<u32>) -> Option<&str> {
        self.t("script").get(id?)?.str("iso_code")
    }
}
