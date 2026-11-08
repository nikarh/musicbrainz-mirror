//! The store being served, and its replacement by a newer dump without a restart.
//!
//! Data directory layout:
//! - `musicbrainz-<dump>/`: a finished store (it has a `meta.json`), named by the dump it was built from. The
//!   newest is served; its name is what keeps the same dump from being downloaded again.
//! - `import/<dump>/`: the archives of a dump being imported (kept until it is imported, so a failed or interrupted
//!   import doesn't download them again), with `failed` in it when its import failed (it isn't retried until the
//!   next start or a newer dump); `import/<dump>.partial/`: the store being built (an interrupted build continues
//!   after its last finished step, see `store::build`; a failed one is removed).
//! - `import/<name>.removing/`: a store or build being removed. Nothing is removed in place, so a crash never leaves
//!   part of a store under a name that would be served or continued.
//!
//! The process can stop at any point: a download continues where it stopped (and is checked whole before it is
//! used), a build after its last finished step, and a finished store is only ever served whole. A store is removed
//! only when it is shown to be damaged (see `StoreError::is_damage`). The data directory is locked by the process
//! using it.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwapOption;

use crate::config::Config;
use crate::dump::{self, DumpError};
use crate::store::build::{self, BuildError};
use crate::store::file::DirLock;
use crate::store::{Store, StoreError};

#[derive(Debug, thiserror::Error)]
pub enum MirrorError {
    #[error(transparent)]
    Dump(#[from] DumpError),
    #[error(transparent)]
    Build(#[from] BuildError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("{0}: {1}")]
    Io(PathBuf, std::io::Error),
    #[error("dump {0} is out, but only {1} GB are free (at least {2} GB needed)")]
    NoSpace(String, u64, u64),
    #[error("no store in {0} and updates are off")]
    NoStore(PathBuf),
    #[error("the import task failed: {0}")]
    Task(String),
    #[error("{0} is in use by another process")]
    Locked(PathBuf),
}

pub struct Mirror {
    config: Config,
    client: reqwest::Client,
    current: Arc<ArcSwapOption<Store>>,
    /// Serializes store changes.
    lock: tokio::sync::Mutex<()>,
    /// The data directory, for this process only (from `start`).
    dir_lock: std::sync::Mutex<Option<DirLock>>,
}

const PREFIX: &str = "musicbrainz-";

/// The dump a store directory was built from, by its name.
fn dir_dump(name: &str) -> Option<&str> {
    name.strip_prefix(PREFIX)
        .filter(|d| dump::is_dump_id(d) || *d == "local")
}

impl Mirror {
    pub fn new(config: Config) -> Result<Self, reqwest::Error> {
        // TLS with ring (installed once; later calls keep the first)
        let _ = rustls::crypto::ring::default_provider().install_default();
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
            dir_lock: std::sync::Mutex::new(None),
        })
    }

    /// The store served (shared with the web service).
    #[must_use]
    pub fn current(&self) -> Arc<ArcSwapOption<Store>> {
        Arc::clone(&self.current)
    }

    fn import_dir(&self) -> PathBuf {
        self.config.data_dir.join("import")
    }

    /// Opens the newest finished store in the data directory, if there is one, and removes leftovers (older stores,
    /// the downloads and builds of dumps not newer than it, what was being removed).
    pub async fn start(&self) -> Result<(), MirrorError> {
        let _guard = self.lock.lock().await;
        let dir = self.config.data_dir.clone();
        let import_dir = self.import_dir();
        tokio::fs::create_dir_all(&import_dir)
            .await
            .map_err(|e| MirrorError::Io(import_dir.clone(), e))?;
        // One process at a time: two would remove and replace each other's stores and builds.
        let lock = DirLock::try_lock(&dir)
            .map_err(|e| MirrorError::Io(dir.clone(), e))?
            .ok_or_else(|| MirrorError::Locked(dir.clone()))?;
        if let Ok(mut held) = self.dir_lock.lock() {
            *held = Some(lock);
        }

        let mut stores = Vec::new();
        let mut entries = tokio::fs::read_dir(&dir)
            .await
            .map_err(|e| MirrorError::Io(dir.clone(), e))?;
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| MirrorError::Io(dir.clone(), e))?
        {
            let name = entry.file_name().to_string_lossy().into_owned();
            if dir_dump(&name).is_some() && entry.path().is_dir() {
                stores.push(name);
            }
        }
        stores.sort();
        while let Some(newest) = stores.pop() {
            let path = dir.join(&newest);
            let opened = tokio::task::spawn_blocking(move || Store::open(&path))
                .await
                .map_err(|e| MirrorError::Task(e.to_string()))?;
            match opened {
                Ok(store) => {
                    let (size, free) = disk(&dir.join(&newest), &dir).await;
                    tracing::info!(
                        dump = store.dump(),
                        size_gb = %gb(size),
                        free_gb = %gb(free),
                        "serving"
                    );
                    self.current.store(Some(Arc::new(store)));
                    for old in stores.drain(..) {
                        tracing::info!(dir = old, "removing an older store");
                        discard(&dir.join(old), &import_dir).await;
                    }
                }
                Err(e) if e.is_damage() => {
                    tracing::warn!(dir = newest, error = %e, "this store is damaged; removing it");
                    discard(&dir.join(&newest), &import_dir).await;
                }
                Err(e @ StoreError::Version(..)) => {
                    // Whole, but not for this version: replaced by the next store built, kept until then (for the
                    // version that built it).
                    tracing::warn!(dir = newest, error = %e, "can't serve this store (kept until replaced)");
                }
                // Nothing known about the store (out of memory maps or file descriptors, permissions, the disk):
                // not served instead of a newer one, and not removed.
                Err(e) => return Err(e.into()),
            }
        }

        let served = self.current.load_full().map(|s| s.dump().to_owned());
        let mut entries = tokio::fs::read_dir(&import_dir)
            .await
            .map_err(|e| MirrorError::Io(import_dir.clone(), e))?;
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| MirrorError::Io(import_dir.clone(), e))?
        {
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path();
            if name.ends_with(REMOVING) || name.ends_with(".partial.tmp") {
                tracing::info!(dir = name, "removing what was being removed");
                remove_dir(&path).await;
                remove(&path).await;
            } else if let Some(dump) = import_dump(&name) {
                if served
                    .as_deref()
                    .is_some_and(|s| dump::is_dump_id(s) && s >= dump)
                {
                    tracing::info!(dir = name, "removing the import of a dump already served");
                    discard(&path, &import_dir).await;
                } else if path.is_dir() {
                    // A failed import is retried after a restart; an interrupted one continues.
                    remove(&path.join("failed")).await;
                }
            }
        }

        if served.is_some() {
            return Ok(());
        }
        if self.config.update_interval.is_none() {
            return Err(MirrorError::NoStore(dir));
        }
        tracing::info!(
            "no store yet; the web service answers 503 until the first dump is imported"
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
            tracing::info!("next check for a new dump in {}", human(interval));
            tokio::time::sleep(interval).await;
        }
    }

    /// Imports the newest dump when it isn't the one served, and switches to it.
    pub async fn update(&self) -> Result<(), MirrorError> {
        let id = dump::latest(&self.client, &self.config.dump_url).await?;
        let served = self.current.load_full().map(|s| s.dump().to_owned());
        if served
            .as_deref()
            .is_some_and(|s| dump::is_dump_id(s) && s >= id.as_str())
        {
            tracing::info!(
                dump = id,
                "checked for a new dump: the newest is being served"
            );
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

        // What an interrupted import of this dump downloaded and built is kept: it counts as free space.
        let partial = self.import_dir().join(format!("{id}.partial"));
        let kept = {
            let (work, partial) = (work.clone(), partial.clone());
            tokio::task::spawn_blocking(move || dir_size(&work) + dir_size(&partial))
                .await
                .unwrap_or(0)
        };
        let free = free_bytes(&self.config.data_dir)
            .map_err(|e| MirrorError::Io(self.config.data_dir.clone(), e))?;
        if free.saturating_add(kept) < self.config.min_free_bytes {
            return Err(MirrorError::NoSpace(
                id,
                free.saturating_add(kept) >> 30,
                self.config.min_free_bytes >> 30,
            ));
        }
        let began = std::time::Instant::now();
        let archives = if build::needs_sources(&partial, &id) {
            tracing::info!(
                dump = id,
                free_gb = %gb(free),
                kept_gb = %gb(kept),
                "a new dump is out; downloading"
            );
            let archives = dump::download(&self.client, &self.config.dump_url, &id, &work).await?;
            tracing::info!(
                dump = id,
                elapsed_s = began.elapsed().as_secs(),
                "downloaded"
            );
            archives
        } else {
            // Past the step that reads the archives.
            tracing::info!(
                dump = id,
                free_gb = %gb(free),
                "a new dump is out; its interrupted import continues"
            );
            Vec::new()
        };

        let built = if build::finished(&partial) {
            Ok(Ok(()))
        } else {
            tracing::info!(
                dump = id,
                threads = self.config.build.threads,
                compress_rows = self.config.build.compress_rows,
                "building the store"
            );
            let (partial, id) = (partial.clone(), id.clone());
            let options = self.config.build.clone();
            tokio::task::spawn_blocking(move || {
                build::resume(&archives, &id, &partial, &options).map(|_| ())
            })
            .await
        };
        let result = match built {
            Ok(Ok(())) => self.switch(&id, &partial).await,
            Ok(Err(e)) => Err(e.into()),
            Err(e) => Err(MirrorError::Task(e.to_string())),
        };
        match result {
            Ok(()) => {
                tracing::info!(dump = id, "removing the downloaded archives");
                remove_dir(&work).await;
                Ok(())
            }
            Err(e) => {
                discard(&partial, &self.import_dir()).await;
                let _ = tokio::fs::create_dir_all(&work).await;
                let _ = tokio::fs::write(work.join("failed"), e.to_string()).await;
                Err(e)
            }
        }
    }

    /// Moves a built store into place, opens it and serves it; the replaced one is removed once the requests
    /// using it are done.
    async fn switch(&self, id: &str, partial: &Path) -> Result<(), MirrorError> {
        let path = self.config.data_dir.join(format!("{PREFIX}{id}"));
        discard(&path, &self.import_dir()).await;
        tokio::fs::rename(partial, &path)
            .await
            .map_err(|e| MirrorError::Io(path.clone(), e))?;
        // The move on disk before the archives it was built from are removed.
        sync_dir(&self.config.data_dir).await?;
        let opened = {
            let path = path.clone();
            tokio::task::spawn_blocking(move || Store::open(&path))
                .await
                .map_err(|e| MirrorError::Task(e.to_string()))?
        };
        let store = match opened {
            Ok(s) => s,
            Err(e) => {
                // A damaged store is removed; another error says nothing about the store, which is kept (tried
                // again at the next start).
                if e.is_damage() {
                    discard(&path, &self.import_dir()).await;
                }
                return Err(e.into());
            }
        };
        let (size, free) = disk(&path, &self.config.data_dir).await;
        let _guard = self.lock.lock().await;
        let previous = self.current.swap(Some(Arc::new(store)));
        tracing::info!(
            dump = id,
            size_gb = %gb(size),
            free_gb = %gb(free),
            "serving the new dump"
        );
        if let Some(previous) = previous {
            tokio::spawn(retire(previous, self.import_dir()));
        }
        Ok(())
    }

    /// Removes the downloads and builds of other dumps (an older one superseded before its import finished).
    async fn remove_other_imports(&self, keep: &str) {
        let dir = self.import_dir();
        let Ok(mut entries) = tokio::fs::read_dir(&dir).await else {
            return;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let name = entry.file_name().to_string_lossy().into_owned();
            if import_dump(&name).is_some_and(|d| d != keep) {
                tracing::info!(dir = name, "removing the import of a superseded dump");
                discard(&entry.path(), &dir).await;
            }
        }
    }
}

/// Removes a replaced store once nothing uses it (or after 10 minutes: its files are unlinked, the open mappings
/// keep working).
async fn retire(store: Arc<Store>, import_dir: PathBuf) {
    for _ in 0..600 {
        if Arc::strong_count(&store) == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    let path = store.dir.clone();
    drop(store);
    tracing::info!(dir = %path.display(), "removing the replaced store");
    discard(&path, &import_dir).await;
    tracing::info!(dir = %path.display(), "removed the replaced store");
}

/// The suffix of what is being removed, in the import directory.
const REMOVING: &str = ".removing";

/// The dump of an import directory's entry: `<dump>` (its downloads) or `<dump>.partial` (its build).
fn import_dump(name: &str) -> Option<&str> {
    Some(name.strip_suffix(".partial").unwrap_or(name)).filter(|d| dump::is_dump_id(d))
}

/// Removes a store or a build without ever leaving part of one under its name (which would be served or continued):
/// it is moved into the import directory first, and what remains of it there after a crash is removed at start.
async fn discard(path: &Path, import_dir: &Path) {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let trash = import_dir.join(format!("{name}{REMOVING}"));
    remove_dir(&trash).await;
    match tokio::fs::rename(path, &trash).await {
        Ok(()) => remove_dir(&trash).await,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            tracing::warn!(dir = %path.display(), error = %e, "can't move away to remove");
            remove_dir(path).await;
        }
    }
}

async fn sync_dir(dir: &Path) -> Result<(), MirrorError> {
    let io = |e| MirrorError::Io(dir.to_owned(), e);
    tokio::fs::File::open(dir)
        .await
        .map_err(io)?
        .sync_all()
        .await
        .map_err(io)
}

async fn remove(path: &Path) {
    if let Err(e) = tokio::fs::remove_file(path).await
        && e.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(file = %path.display(), error = %e, "can't remove");
    }
}

async fn remove_dir(path: &Path) {
    if let Err(e) = tokio::fs::remove_dir_all(path).await
        && e.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(dir = %path.display(), error = %e, "can't remove");
    }
}

/// The size of a store (its files' lengths) and the free space of the data directory, for the logs.
async fn disk(store: &Path, data_dir: &Path) -> (u64, u64) {
    let (store, data_dir) = (store.to_owned(), data_dir.to_owned());
    tokio::task::spawn_blocking(move || (dir_size(&store), free_bytes(&data_dir).unwrap_or(0)))
        .await
        .unwrap_or((0, 0))
}

fn dir_size(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .map(|e| match e.metadata() {
            Ok(m) if m.is_dir() => dir_size(&e.path()),
            Ok(m) => m.len(),
            Err(_) => 0,
        })
        .sum()
}

/// Bytes as GB with one decimal.
fn gb(bytes: u64) -> String {
    let tenths = bytes.saturating_mul(10) >> 30;
    format!("{}.{}", tenths / 10, tenths % 10)
}

/// A duration as the largest whole unit (`6h`, `30m`, `90s`).
#[must_use]
pub fn human(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        _ if s >= 86_400 && s.is_multiple_of(86_400) => format!("{}d", s / 86_400),
        _ if s >= 3600 && s.is_multiple_of(3600) => format!("{}h", s / 3600),
        _ if s >= 60 && s.is_multiple_of(60) => format!("{}m", s / 60),
        _ => format!("{s}s"),
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
    fn store_dir_names() {
        assert_eq!(
            dir_dump("musicbrainz-20261104-002222"),
            Some("20261104-002222")
        );
        assert_eq!(dir_dump("musicbrainz-local"), Some("local"));
        assert_eq!(dir_dump("musicbrainz-x"), None);
        assert_eq!(dir_dump("other"), None);
    }

    #[test]
    fn log_units() {
        assert_eq!(gb(19 << 30), "19.0");
        assert_eq!(gb((3 << 30) / 2), "1.5");
        assert_eq!(human(Duration::from_hours(6)), "6h");
        assert_eq!(human(Duration::from_hours(24)), "1d");
        assert_eq!(human(Duration::from_mins(30)), "30m");
        assert_eq!(human(Duration::from_secs(90)), "90s");
    }
}
