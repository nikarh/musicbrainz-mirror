//! The database being served, and its replacement by a newer dump without a restart.
//!
//! Data directory layout:
//! - `musicbrainz-<dump>.sqlite`: a finished database, named by the dump it was built from. The newest is served;
//!   its name is what keeps the same dump from being downloaded again.
//! - `import/<dump>/`: the archives of a dump being imported (kept until it is imported, so a failed or interrupted
//!   import doesn't download them again), `import/<dump>.sqlite.partial`: the database being built, `failed` in
//!   the dump's directory: its import failed (it isn't retried until the next start or a newer dump).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwapOption;

use crate::config::Config;
use crate::db::{Db, DbError};
use crate::dump::{self, DumpError};
use crate::import::{self, ImportError};

/// Most of a database's pages are read through mmap (the database is never written after it is built).
const MMAP_SIZE: u64 = 1 << 40;

#[derive(Debug, thiserror::Error)]
pub enum MirrorError {
    #[error(transparent)]
    Dump(#[from] DumpError),
    #[error(transparent)]
    Import(#[from] ImportError),
    #[error(transparent)]
    Db(#[from] DbError),
    #[error("{0}: {1}")]
    Io(PathBuf, std::io::Error),
    #[error("dump {0} is out, but only {1} GB are free (at least {2} GB needed)")]
    NoSpace(String, u64, u64),
    #[error("no database in {0} and updates are off")]
    NoDatabase(PathBuf),
    #[error("the import task failed: {0}")]
    Task(String),
}

pub struct Mirror {
    config: Config,
    client: reqwest::Client,
    current: Arc<ArcSwapOption<Db>>,
    /// Serializes database changes.
    lock: tokio::sync::Mutex<()>,
}

const PREFIX: &str = "musicbrainz-";
const SUFFIX: &str = ".sqlite";

/// The dump a database file was built from, by its name.
fn file_dump(name: &str) -> Option<&str> {
    name.strip_prefix(PREFIX)
        .and_then(|n| n.strip_suffix(SUFFIX))
        .filter(|d| dump::is_dump_id(d) || *d == "local")
}

impl Mirror {
    pub fn new(config: Config) -> Result<Self, reqwest::Error> {
        let client = reqwest::Client::builder()
            .user_agent(concat!(
                "musicbrainz-mirror/",
                env!("CARGO_PKG_VERSION"),
                " ( https://github.com/nikarh/musicbrainz-mirror )"
            ))
            .connect_timeout(Duration::from_secs(30))
            .build()?;
        Ok(Self {
            config,
            client,
            current: Arc::new(ArcSwapOption::empty()),
            lock: tokio::sync::Mutex::new(()),
        })
    }

    /// The database served (shared with the web service).
    pub fn current(&self) -> Arc<ArcSwapOption<Db>> {
        Arc::clone(&self.current)
    }

    fn import_dir(&self) -> PathBuf {
        self.config.data_dir.join("import")
    }

    /// Opens the newest finished database in the data directory, if there is one, and removes leftovers
    /// (databases being built, older databases).
    pub async fn start(&self) -> Result<(), MirrorError> {
        let _guard = self.lock.lock().await;
        let dir = self.config.data_dir.clone();
        let import_dir = self.import_dir();
        tokio::fs::create_dir_all(&import_dir)
            .await
            .map_err(|e| MirrorError::Io(import_dir.clone(), e))?;
        let mut entries = tokio::fs::read_dir(&import_dir)
            .await
            .map_err(|e| MirrorError::Io(import_dir.clone(), e))?;
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| MirrorError::Io(import_dir.clone(), e))?
        {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.ends_with(".partial") || name.contains(".partial-") {
                tracing::info!(file = name, "removing an interrupted import");
                remove(&entry.path()).await;
            } else if entry.path().is_dir() {
                // A failed import is retried after a restart.
                remove(&entry.path().join("failed")).await;
            }
        }

        let mut databases = Vec::new();
        let mut entries = tokio::fs::read_dir(&dir)
            .await
            .map_err(|e| MirrorError::Io(dir.clone(), e))?;
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| MirrorError::Io(dir.clone(), e))?
        {
            let name = entry.file_name().to_string_lossy().into_owned();
            if file_dump(&name).is_some() {
                databases.push(name);
            }
        }
        databases.sort();
        while let Some(newest) = databases.pop() {
            let path = dir.join(&newest);
            let opened = tokio::task::spawn_blocking(move || Db::open(&path, MMAP_SIZE))
                .await
                .map_err(|e| MirrorError::Task(e.to_string()))?;
            match opened {
                Ok(db) => {
                    tracing::info!(dump = db.dump, "serving");
                    self.current.store(Some(Arc::new(db)));
                    for old in databases {
                        tracing::info!(file = old, "removing an older database");
                        remove_db(&dir.join(old)).await;
                    }
                    return Ok(());
                }
                Err(e) => {
                    // Built by another version (or broken): rebuilt from the next dump.
                    tracing::warn!(file = newest, error = %e, "can't serve this database; removing it");
                    remove_db(&dir.join(&newest)).await;
                }
            }
        }
        if self.config.update_interval.is_none() {
            return Err(MirrorError::NoDatabase(dir));
        }
        tracing::info!(
            "no database yet; the web service answers 503 until the first dump is imported"
        );
        Ok(())
    }

    /// Checks for a newer dump every `update_interval`, forever.
    pub async fn run_updates(self: Arc<Self>) {
        let Some(interval) = self.config.update_interval else {
            return;
        };
        loop {
            if let Err(e) = self.update().await {
                tracing::error!(error = %e, "update failed");
            }
            tokio::time::sleep(interval).await;
        }
    }

    /// Imports the newest dump when it isn't the one served, and switches to it.
    pub async fn update(&self) -> Result<(), MirrorError> {
        let id = dump::latest(&self.client, &self.config.dump_url).await?;
        let served = self.current.load_full().map(|db| db.dump.clone());
        if served
            .as_deref()
            .is_some_and(|s| dump::is_dump_id(s) && s >= id.as_str())
        {
            tracing::debug!(dump = id, "the newest dump is being served");
            return Ok(());
        }
        let work = self.import_dir().join(&id);
        if tokio::fs::try_exists(work.join("failed"))
            .await
            .unwrap_or(false)
        {
            tracing::warn!(
                dump = id,
                "skipping a dump whose import failed (retried after a restart)"
            );
            return Ok(());
        }
        self.remove_other_imports(&id).await;

        let free = free_bytes(&self.config.data_dir)
            .map_err(|e| MirrorError::Io(self.config.data_dir.clone(), e))?;
        if free < self.config.min_free_bytes {
            return Err(MirrorError::NoSpace(
                id,
                free >> 30,
                self.config.min_free_bytes >> 30,
            ));
        }
        tracing::info!(dump = id, "a new dump is out; downloading");
        let archives = dump::download(&self.client, &self.config.dump_url, &id, &work).await?;

        let partial = self.import_dir().join(format!("{id}.sqlite.partial"));
        let built = {
            let (archives, partial, id, cache) = (
                archives.clone(),
                partial.clone(),
                id.clone(),
                self.config.cache_mb,
            );
            tokio::task::spawn_blocking(move || import::build(&archives, &id, &partial, cache))
                .await
        };
        let result = match built {
            Ok(Ok(_)) => self.switch(&id, &partial).await,
            Ok(Err(e)) => Err(e.into()),
            Err(e) => Err(MirrorError::Task(e.to_string())),
        };
        match result {
            Ok(()) => {
                remove_dir(&work).await;
                Ok(())
            }
            Err(e) => {
                remove_db(&partial).await;
                let _ = tokio::fs::write(work.join("failed"), e.to_string()).await;
                Err(e)
            }
        }
    }

    /// Moves a built database into place, opens it and serves it; the replaced one is removed once the requests
    /// using it are done.
    async fn switch(&self, id: &str, partial: &Path) -> Result<(), MirrorError> {
        let path = self.config.data_dir.join(format!("{PREFIX}{id}{SUFFIX}"));
        tokio::fs::rename(partial, &path)
            .await
            .map_err(|e| MirrorError::Io(path.clone(), e))?;
        let opened = {
            let path = path.clone();
            tokio::task::spawn_blocking(move || Db::open(&path, MMAP_SIZE))
                .await
                .map_err(|e| MirrorError::Task(e.to_string()))?
        };
        let db = match opened {
            Ok(db) => db,
            Err(e) => {
                remove_db(&path).await;
                return Err(e.into());
            }
        };
        let _guard = self.lock.lock().await;
        let previous = self.current.swap(Some(Arc::new(db)));
        tracing::info!(dump = id, "serving the new dump");
        if let Some(previous) = previous {
            tokio::spawn(retire(previous));
        }
        Ok(())
    }

    /// Removes the downloads of other dumps (an older one superseded before its import finished).
    async fn remove_other_imports(&self, keep: &str) {
        let dir = self.import_dir();
        let Ok(mut entries) = tokio::fs::read_dir(&dir).await else {
            return;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name != keep && entry.path().is_dir() {
                tracing::info!(dump = name, "removing the downloads of a superseded dump");
                remove_dir(&entry.path()).await;
            }
        }
    }
}

/// Removes a replaced database once nothing uses it (or after 10 minutes: its file is unlinked, the open handles
/// keep working).
async fn retire(db: Arc<Db>) {
    for _ in 0..600 {
        if Arc::strong_count(&db) == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    let path = db.path.clone();
    drop(db);
    tracing::info!(file = %path.display(), "removing the replaced database");
    remove_db(&path).await;
}

async fn remove(path: &Path) {
    if let Err(e) = tokio::fs::remove_file(path).await
        && e.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(file = %path.display(), error = %e, "can't remove");
    }
}

async fn remove_db(path: &Path) {
    for suffix in ["", "-journal", "-wal", "-shm"] {
        remove(&import::side_file(path, suffix)).await;
    }
}

async fn remove_dir(path: &Path) {
    if let Err(e) = tokio::fs::remove_dir_all(path).await
        && e.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(dir = %path.display(), error = %e, "can't remove");
    }
}

fn free_bytes(dir: &Path) -> std::io::Result<u64> {
    let stat = rustix::fs::statvfs(dir)?;
    Ok(stat.f_bavail.saturating_mul(stat.f_frsize))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn database_file_names() {
        assert_eq!(
            file_dump("musicbrainz-20261104-002222.sqlite"),
            Some("20261104-002222")
        );
        assert_eq!(file_dump("musicbrainz-local.sqlite"), Some("local"));
        assert_eq!(file_dump("musicbrainz-x.sqlite"), None);
        assert_eq!(file_dump("other.sqlite"), None);
    }
}
