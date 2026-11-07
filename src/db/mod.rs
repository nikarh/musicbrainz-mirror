//! An imported database, opened read-only: a pool of connections and the lookup tables, read into memory once.

pub mod date;
pub mod queries;

use std::collections::HashMap;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::{Connection, OpenFlags, OptionalExtension as _};

use crate::import::DB_VERSION;
use crate::kind::Kind;

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("{0}: {1}")]
    Open(PathBuf, rusqlite::Error),
    #[error("{0}: database layout {1:?}, this version reads {DB_VERSION:?}")]
    Version(PathBuf, String),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

/// A row of a lookup table: a type, status, format, ...
#[derive(Debug, Clone)]
pub struct Named {
    pub name: String,
    pub gid: Option<String>,
}

#[derive(Debug, Clone)]
pub struct LinkType {
    pub name: String,
    pub gid: Option<String>,
    pub cardinality0: i64,
    pub cardinality1: i64,
}

#[derive(Debug, Clone)]
pub struct AttributeType {
    pub name: String,
    pub gid: Option<String>,
    pub creditable: bool,
}

/// The small tables, in memory.
#[derive(Debug, Default)]
pub struct Lookups {
    /// By table name and id: `artist_type`, `gender`, `release_status`, `medium_format`, ...
    named: HashMap<&'static str, HashMap<i64, Named>>,
    alias_types: HashMap<(i64, i64), Named>,
    pub languages: HashMap<i64, String>,
    pub scripts: HashMap<i64, String>,
    pub link_types: HashMap<i64, LinkType>,
    pub attribute_types: HashMap<i64, AttributeType>,
    /// Allowed values of work attributes: (value, gid)
    pub work_attribute_values: HashMap<i64, (String, Option<String>)>,
}

/// The lookup tables of `(id, name, gid)`.
pub const NAMED_TABLES: &[&str] = &[
    "area_type",
    "artist_type",
    "event_type",
    "instrument_type",
    "label_type",
    "place_type",
    "series_type",
    "work_type",
    "gender",
    "release_status",
    "release_packaging",
    "release_group_primary_type",
    "release_group_secondary_type",
    "medium_format",
    "series_ordering_type",
    "work_attribute_type",
    "art_type",
];

impl Lookups {
    fn load(db: &Connection) -> rusqlite::Result<Self> {
        let mut lookups = Self::default();
        for table in NAMED_TABLES {
            let mut stmt = db.prepare(&format!("SELECT id, name, gid FROM {table}"))?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get(0)?,
                    Named {
                        name: r.get(1)?,
                        gid: r.get(2)?,
                    },
                ))
            })?;
            lookups.named.insert(table, rows.collect::<Result<_, _>>()?);
        }
        let mut stmt = db.prepare("SELECT kind, id, name, gid FROM alias_type")?;
        lookups.alias_types = stmt
            .query_map([], |r| {
                Ok((
                    (r.get(0)?, r.get(1)?),
                    Named {
                        name: r.get(2)?,
                        gid: r.get(3)?,
                    },
                ))
            })?
            .collect::<Result<_, _>>()?;
        let mut stmt =
            db.prepare("SELECT id, iso_code_3 FROM language WHERE iso_code_3 IS NOT NULL")?;
        lookups.languages = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        let mut stmt = db.prepare("SELECT id, iso_code FROM script")?;
        lookups.scripts = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        let mut stmt = db.prepare(
            "SELECT id, name, gid, entity0_cardinality, entity1_cardinality FROM link_type",
        )?;
        lookups.link_types = stmt
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    LinkType {
                        name: r.get(1)?,
                        gid: r.get(2)?,
                        cardinality0: r.get::<_, Option<i64>>(3)?.unwrap_or(0),
                        cardinality1: r.get::<_, Option<i64>>(4)?.unwrap_or(0),
                    },
                ))
            })?
            .collect::<Result<_, _>>()?;
        let mut stmt = db.prepare(
            "SELECT t.id, t.name, t.gid, c.attribute_type IS NOT NULL FROM link_attribute_type t
             LEFT JOIN link_creditable_attribute_type c ON c.attribute_type = t.id",
        )?;
        lookups.attribute_types = stmt
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    AttributeType {
                        name: r.get(1)?,
                        gid: r.get(2)?,
                        creditable: r.get(3)?,
                    },
                ))
            })?
            .collect::<Result<_, _>>()?;
        let mut stmt =
            db.prepare("SELECT id, value, gid FROM work_attribute_type_allowed_value")?;
        lookups.work_attribute_values = stmt
            .query_map([], |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?))))?
            .collect::<Result<_, _>>()?;
        Ok(lookups)
    }

    /// A row of one of [`NAMED_TABLES`].
    #[must_use]
    pub fn named(&self, table: &str, id: Option<i64>) -> Option<&Named> {
        self.named.get(table)?.get(&id?)
    }

    #[must_use]
    pub fn alias_type(&self, kind: Kind, id: Option<i64>) -> Option<&Named> {
        self.alias_types.get(&(kind.code(), id?))
    }

    /// The id of a row of a lookup table by its name, case-insensitively.
    #[must_use]
    pub fn id_by_name(&self, table: &str, name: &str) -> Option<i64> {
        self.named
            .get(table)?
            .iter()
            .find(|(_, n)| n.name.eq_ignore_ascii_case(name))
            .map(|(id, _)| *id)
    }

    /// All rows of a lookup table.
    pub fn all(&self, table: &str) -> impl Iterator<Item = (i64, &Named)> {
        self.named
            .get(table)
            .into_iter()
            .flatten()
            .map(|(id, n)| (*id, n))
    }
}

/// An open database.
pub struct Db {
    pub path: PathBuf,
    /// The dump it was built from (e.g. `20261104-002222`).
    pub dump: String,
    pub meta: HashMap<String, String>,
    pub lookups: Lookups,
    idle: Mutex<Vec<Connection>>,
    mmap_size: u64,
}

impl Db {
    /// Opens a database the importer has finished. It is never written to again, so SQLite opens it as immutable (no
    /// locking, no journal).
    pub fn open(path: &Path, mmap_size: u64) -> Result<Self, DbError> {
        let conn = connect(path, mmap_size).map_err(|e| DbError::Open(path.to_owned(), e))?;
        let meta: HashMap<String, String> = conn
            .prepare("SELECT key, value FROM meta")
            .and_then(|mut s| s.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect())
            .map_err(|e| DbError::Open(path.to_owned(), e))?;
        let version = meta.get("db_version").cloned().unwrap_or_default();
        if version != DB_VERSION {
            return Err(DbError::Version(path.to_owned(), version));
        }
        let lookups = Lookups::load(&conn)?;
        Ok(Self {
            path: path.to_owned(),
            dump: meta.get("dump").cloned().unwrap_or_default(),
            meta,
            lookups,
            idle: Mutex::new(vec![conn]),
            mmap_size,
        })
    }

    /// A connection from the pool (a new one when all are busy). It goes back to the pool when dropped.
    pub fn conn(&self) -> Result<Pooled<'_>, DbError> {
        let idle = self.idle.lock().ok().and_then(|mut i| i.pop());
        let conn = match idle {
            Some(c) => c,
            None => connect(&self.path, self.mmap_size)
                .map_err(|e| DbError::Open(self.path.clone(), e))?,
        };
        Ok(Pooled {
            db: self,
            conn: Some(conn),
        })
    }
}

fn connect(path: &Path, mmap_size: u64) -> rusqlite::Result<Connection> {
    let uri = format!(
        "file:{}?immutable=1",
        path.display()
            .to_string()
            .replace('?', "%3f")
            .replace('#', "%23")
    );
    let conn = Connection::open_with_flags(
        uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.execute_batch(&format!(
        "PRAGMA mmap_size = {mmap_size}; PRAGMA cache_size = -16384; PRAGMA query_only = 1;"
    ))?;
    conn.set_prepared_statement_cache_capacity(256);
    Ok(conn)
}

pub struct Pooled<'a> {
    db: &'a Db,
    conn: Option<Connection>,
}

impl Deref for Pooled<'_> {
    type Target = Connection;

    fn deref(&self) -> &Connection {
        // Only taken out in drop.
        #[allow(clippy::unwrap_used)]
        self.conn.as_ref().unwrap()
    }
}

impl Drop for Pooled<'_> {
    fn drop(&mut self) {
        if let (Some(conn), Ok(mut idle)) = (self.conn.take(), self.db.idle.lock()) {
            idle.push(conn);
        }
    }
}

/// Reads the `meta` table of a database file, if it has one (for files left by an earlier run).
#[must_use]
pub fn read_dump_id(path: &Path) -> Option<String> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    conn.query_row("SELECT value FROM meta WHERE key = 'dump'", [], |r| {
        r.get(0)
    })
    .optional()
    .ok()?
}
