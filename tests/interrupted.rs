//! Interruptions: the process stopped (killed) at any point of an update, and started again. Nothing it left is
//! served unless it is whole, what was downloaded isn't downloaded again, and an interrupted build continues to the
//! same store as one that wasn't interrupted.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::fmt::Write as _;
use std::io::BufRead as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Path as UrlPath, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use musicbrainz_mirror::config::{BuildOptions, Config};
use musicbrainz_mirror::dump;
use musicbrainz_mirror::mirror::Mirror;
use sha2::{Digest as _, Sha256};

mod common;
use common::{archive, assert_same_store, fixture};

const DUMP: &str = "20261101-000000";

/// A test's own directory under cargo's temporary directory for tests (the same each run: a run replaces it).
fn temp_dir(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("interrupted-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn sha256(data: &[u8]) -> String {
    Sha256::digest(data).iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// MusicBrainz's full exports, as a web server with ranges does them.
struct Exports {
    latest: Mutex<String>,
    /// file name → contents, the same for every dump
    files: Vec<(String, Vec<u8>)>,
    /// Requests of archives, and the bytes of archives sent.
    requests: AtomicUsize,
    sent: AtomicU64,
    /// Answers requests of a range with the whole file (as servers without ranges do).
    ignore_ranges: AtomicBool,
}

impl Exports {
    fn new(files: Vec<(String, Vec<u8>)>) -> Arc<Self> {
        Arc::new(Self {
            latest: Mutex::new(DUMP.into()),
            files,
            requests: AtomicUsize::new(0),
            sent: AtomicU64::new(0),
            ignore_ranges: AtomicBool::new(false),
        })
    }

    /// The fixture as the three archives of a dump (all of its tables in the core one).
    fn fixture() -> Arc<Self> {
        let fixture = fixture();
        Self::new(vec![
            ("mbdump.tar.bz2".into(), archive(&fixture, &[""])),
            (
                "mbdump-derived.tar.bz2".into(),
                archive(&fixture, &["nothing"]),
            ),
            (
                "mbdump-cover-art-archive.tar.bz2".into(),
                archive(&fixture, &["nothing"]),
            ),
        ])
    }

    fn file(&self, name: &str) -> &[u8] {
        &self.files.iter().find(|(n, _)| n == name).unwrap().1
    }

    fn counts(&self) -> (usize, u64) {
        (
            self.requests.load(Ordering::Relaxed),
            self.sent.load(Ordering::Relaxed),
        )
    }

    /// Serves them on a local port.
    async fn serve(self: &Arc<Self>) -> std::net::SocketAddr {
        let app = axum::Router::new()
            .route("/exports/{*path}", axum::routing::get(serve))
            .with_state(Arc::clone(self));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        addr
    }
}

async fn serve(
    State(exports): State<Arc<Exports>>,
    UrlPath(path): UrlPath<String>,
    headers: HeaderMap,
) -> axum::response::Response {
    if path == "LATEST" {
        return exports.latest.lock().unwrap().clone().into_response();
    }
    let Some((_dump, file)) = path.split_once('/') else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if file == "SHA256SUMS" {
        let mut sums = String::new();
        for (name, data) in &exports.files {
            let _ = writeln!(sums, "{} *{name}", sha256(data));
        }
        return sums.into_response();
    }
    let Some((_, data)) = exports.files.iter().find(|(name, _)| name == file) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    exports.requests.fetch_add(1, Ordering::Relaxed);
    let from = headers
        .get(header::RANGE)
        .and_then(|r| r.to_str().ok())
        .and_then(|r| r.strip_prefix("bytes="))
        .and_then(|r| r.strip_suffix('-'))
        .and_then(|r| r.parse::<usize>().ok())
        .filter(|_| !exports.ignore_ranges.load(Ordering::Relaxed));
    match from {
        None => {
            exports.sent.fetch_add(data.len() as u64, Ordering::Relaxed);
            data.clone().into_response()
        }
        Some(from) if from >= data.len() => (
            StatusCode::RANGE_NOT_SATISFIABLE,
            [(header::CONTENT_RANGE, format!("bytes */{}", data.len()))],
        )
            .into_response(),
        Some(from) => {
            exports
                .sent
                .fetch_add((data.len() - from) as u64, Ordering::Relaxed);
            (
                StatusCode::PARTIAL_CONTENT,
                [(
                    header::CONTENT_RANGE,
                    format!("bytes {from}-{}/{}", data.len() - 1, data.len()),
                )],
                data[from..].to_vec(),
            )
                .into_response()
        }
    }
}

/// Downloads `mbdump.tar.bz2` into `dir`, as the updater does.
async fn fetch(exports: &Exports, addr: std::net::SocketAddr, dir: &Path) -> Result<(), String> {
    let file = "mbdump.tar.bz2";
    // (as the updater does: reqwest is built without one)
    let _ = rustls::crypto::ring::default_provider().install_default();
    dump::fetch(
        &reqwest::Client::new(),
        &format!("http://{addr}/exports/{DUMP}/{file}"),
        &dir.join(file),
        &sha256(exports.file(file)),
    )
    .await
    .map_err(|e| e.to_string())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_interrupted_download_continues_where_it_stopped() {
    let exports = Exports::fixture();
    let addr = exports.serve().await;
    let data = exports.file("mbdump.tar.bz2").to_vec();
    let dir = temp_dir("download");
    let (dest, partial) = (
        dir.join("mbdump.tar.bz2"),
        dir.join("mbdump.tar.bz2.partial"),
    );
    let half = data.len() / 2;

    // Stopped halfway: the rest is downloaded.
    std::fs::write(&partial, &data[..half]).unwrap();
    fetch(&exports, addr, &dir).await.unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), data);
    assert!(!partial.exists());
    assert_eq!(exports.counts(), (1, (data.len() - half) as u64));

    // Downloaded: checked, not downloaded again.
    fetch(&exports, addr, &dir).await.unwrap();
    assert_eq!(exports.counts(), (1, (data.len() - half) as u64));

    // Stopped after the last byte, before the download was checked and moved into place: nothing more to download.
    std::fs::rename(&dest, &partial).unwrap();
    fetch(&exports, addr, &dir).await.unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), data);
    assert_eq!(exports.counts(), (2, (data.len() - half) as u64));

    // A corrupt download in place: downloaded again.
    std::fs::write(&dest, &data[..half]).unwrap();
    fetch(&exports, addr, &dir).await.unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), data);
    assert_eq!(exports.counts(), (3, (data.len() * 2 - half) as u64));

    // What was there before the interruption was corrupt: found out once the download is whole, and started over.
    std::fs::remove_file(&dest).unwrap();
    let mut corrupt = data[..half].to_vec();
    corrupt[0] ^= 1;
    std::fs::write(&partial, &corrupt).unwrap();
    let error = fetch(&exports, addr, &dir).await.unwrap_err();
    assert!(error.contains("checksum"), "{error}");
    assert!(!partial.exists() && !dest.exists());
    fetch(&exports, addr, &dir).await.unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), data);

    // A server that doesn't do ranges: the whole file, over what was there.
    std::fs::remove_file(&dest).unwrap();
    std::fs::write(&partial, &data[..half]).unwrap();
    exports.ignore_ranges.store(true, Ordering::Relaxed);
    fetch(&exports, addr, &dir).await.unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), data);

    std::fs::remove_dir_all(&dir).unwrap();
}

/// Runs `musicbrainz-mirror import`, killed (SIGKILL) once its log has `kill_at.1` lines with `kill_at.0`. Whether it
/// finished.
fn import(out: &Path, kill_at: Option<(&str, usize)>) -> bool {
    let mut child = Command::new(env!("CARGO_BIN_EXE_musicbrainz-mirror"))
        .arg("import")
        .arg("--out")
        .arg(out)
        .args(["--dump", DUMP, "--compress-rows", "--build-threads", "1"])
        .arg(fixture())
        .env("RUST_LOG", "info")
        .env("NO_COLOR", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some((pattern, nth)) = kill_at {
        let mut seen = 0;
        for line in std::io::BufReader::new(child.stdout.take().unwrap()).lines() {
            if line.unwrap().contains(pattern) {
                seen += 1;
                if seen == nth {
                    child.kill().unwrap();
                    break;
                }
            }
        }
    }
    let output = child.wait_with_output().unwrap();
    let finished = output.status.success();
    assert!(
        finished || kill_at.is_some(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    finished
}

#[test]
fn an_import_killed_at_any_step_continues_to_the_same_store() {
    let dir = temp_dir("import");
    let reference = dir.join("reference");
    assert!(import(&reference, None));

    // Killed while writing the tables, right after, after the derived values, while building the search indexes,
    // after (while compressing rows, if it hasn't finished yet); then let finish.
    let out = dir.join("killed");
    for kill_at in [
        ("written", 20),
        ("tables written", 1),
        ("derived values written", 1),
        ("search index built", 3),
        ("search indexes built", 1),
    ] {
        assert!(
            !import(&out, Some(kill_at)) || kill_at.0 == "search indexes built",
            "finished before {kill_at:?}"
        );
        assert!(
            musicbrainz_mirror::store::Store::open(&out).is_err() || out.join("meta.json").exists()
        );
    }
    if !out.join("meta.json").exists() {
        assert!(import(&out, None));
    }
    assert_same_store(&out, &reference);

    // A finished store isn't touched.
    let output = Command::new(env!("CARGO_BIN_EXE_musicbrainz-mirror"))
        .arg("import")
        .arg("--out")
        .arg(&out)
        .args(["--dump", DUMP])
        .arg(fixture())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_same_store(&out, &reference);
    std::fs::remove_dir_all(&dir).unwrap();
}

fn config(data_dir: &Path, addr: std::net::SocketAddr) -> Config {
    Config {
        data_dir: data_dir.to_owned(),
        listen: "127.0.0.1:0".parse().unwrap(),
        update_interval: Some(Duration::from_hours(1)),
        min_free_bytes: 0,
        dump_url: format!("http://{addr}/exports"),
        cover_art_url: "https://coverartarchive.org".into(),
        build: BuildOptions {
            threads: 1,
            memory_mb: 50,
            compress_rows: true,
            schema_mismatch: musicbrainz_mirror::config::SchemaMismatch::Refuse,
        },
    }
}

/// Starts the updater on `data_dir` (as after a restart) and lets it update.
async fn restart(data_dir: &Path, addr: std::net::SocketAddr) -> Mirror {
    let mirror = Mirror::new(config(data_dir, addr)).unwrap();
    mirror.start().await.unwrap();
    mirror.update().await.unwrap();
    assert_eq!(mirror.current().load_full().unwrap().dump(), DUMP);
    mirror
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restarted_update_uses_what_it_had_downloaded_and_built() {
    let exports = Exports::fixture();
    let addr = exports.serve().await;
    let data_dir = temp_dir("restart");
    let import = data_dir.join("import");
    let (work, partial) = (import.join(DUMP), import.join(format!("{DUMP}.partial")));
    let store = data_dir.join(format!("musicbrainz-{DUMP}"));

    // Stopped after the downloads: they are checked, not downloaded again.
    std::fs::create_dir_all(&work).unwrap();
    for (name, data) in &exports.files {
        std::fs::write(work.join(name), data).unwrap();
    }
    let mirror = restart(&data_dir, addr).await;
    assert_eq!(exports.counts(), (0, 0));
    assert!(!work.exists() && !partial.exists());
    drop(mirror);
    std::fs::remove_dir_all(&store).unwrap();

    // Stopped while building, past the tables, the downloads already removed: the build continues without them.
    std::fs::create_dir_all(&import).unwrap();
    assert!(!import_killed(&partial, "derived values written"));
    let mirror = restart(&data_dir, addr).await;
    assert_eq!(exports.counts(), (0, 0));
    drop(mirror);
    std::fs::remove_dir_all(&store).unwrap();

    // Stopped once the store was built, before it was moved into place: served as it is.
    assert!(import_killed(&partial, "no such line"));
    let mirror = restart(&data_dir, addr).await;
    assert_eq!(exports.counts(), (0, 0));
    assert!(!partial.exists());
    drop(mirror);

    // Stopped after the switch, before the downloads were removed (and with a store being removed): removed at
    // start.
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(work.join("mbdump.tar.bz2"), b"left").unwrap();
    std::fs::create_dir_all(import.join("musicbrainz-20261001-000000.removing")).unwrap();
    // An older dump's leftovers too, and a newer one's (kept: its import continues).
    let newer = "20261201-000000";
    for name in ["20261001-000000", "20261001-000000.partial", newer] {
        std::fs::create_dir_all(import.join(name)).unwrap();
    }
    let mirror = Mirror::new(config(&data_dir, addr)).unwrap();
    mirror.start().await.unwrap();
    let mut left: Vec<_> = std::fs::read_dir(&import)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    left.sort();
    assert_eq!(left, [newer]);

    std::fs::remove_dir_all(&data_dir).unwrap();
}

/// Imports the fixture into `out` as the updater would, killed at the first log line with `pattern`. Whether it
/// finished.
fn import_killed(out: &Path, pattern: &str) -> bool {
    import(out, Some((pattern, 1)))
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &to.join(entry.file_name()));
        } else {
            std::fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
        }
    }
}

/// Starts the updater on a data directory with two stores, the newer one changed by `change`: what is served, and
/// whether the newer one is still there.
async fn start_with(
    name: &str,
    template: &Path,
    change: impl FnOnce(&Path),
) -> (Result<Option<String>, String>, bool) {
    let data_dir = temp_dir(name);
    let (older, newer) = ("20261001-000000", DUMP);
    copy_dir(template, &data_dir.join(format!("musicbrainz-{older}")));
    let newer_dir = data_dir.join(format!("musicbrainz-{newer}"));
    copy_dir(template, &newer_dir);
    change(&newer_dir);
    let mirror = Mirror::new(config(&data_dir, "127.0.0.1:9".parse().unwrap())).unwrap();
    let started = mirror
        .start()
        .await
        .map(|()| {
            mirror
                .current()
                .load_full()
                .map(|s| s.dir.file_name().unwrap().to_string_lossy().into_owned())
        })
        .map_err(|e| e.to_string());
    let kept = newer_dir.exists();
    drop(mirror);
    std::fs::remove_dir_all(&data_dir).unwrap();
    (started, kept)
}

/// A store that can't be opened is removed only when it is shown to be damaged.
#[tokio::test(flavor = "multi_thread")]
async fn a_store_is_removed_only_when_shown_damaged() {
    let dir = temp_dir("damaged");
    let template = dir.join("store");
    musicbrainz_mirror::store::build::build(
        &[fixture()],
        DUMP,
        &template,
        &BuildOptions {
            threads: 1,
            memory_mb: 50,
            compress_rows: false,
            schema_mismatch: musicbrainz_mirror::config::SchemaMismatch::Refuse,
        },
    )
    .unwrap();
    let older = Ok(Some("musicbrainz-20261001-000000".to_owned()));
    let newer = Ok(Some(format!("musicbrainz-{DUMP}")));

    // Whole: served.
    assert_eq!(start_with("whole", &template, |_| {}).await, (newer, true));
    // Built before stores had a manifest: served, unverified.
    let unverified = start_with("no-manifest", &template, |d| {
        std::fs::remove_file(d.join("files.json")).unwrap();
    })
    .await;
    assert_eq!(unverified, (Ok(Some(format!("musicbrainz-{DUMP}"))), true));
    // A file missing, or of another size: removed, the older store served.
    let missing = start_with("missing", &template, |d| {
        std::fs::remove_file(d.join("artist.rows")).unwrap();
    })
    .await;
    assert_eq!(missing, (older.clone(), false));
    let truncated = start_with("truncated", &template, |d| {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(d.join("search/artist/meta.json"))
            .unwrap();
        file.set_len(10).unwrap();
    })
    .await;
    assert_eq!(truncated, (older.clone(), false));
    // Unfinished (no meta.json): removed.
    let unfinished = start_with("unfinished", &template, |d| {
        std::fs::remove_file(d.join("meta.json")).unwrap();
    })
    .await;
    assert_eq!(unfinished, (older.clone(), false));
    // Another version's: not served, kept until replaced.
    let other_version = start_with("version", &template, |d| {
        let meta = std::fs::read_to_string(d.join("meta.json")).unwrap();
        let meta = meta.replace(
            &format!(
                "\"format_version\": \"{}\"",
                musicbrainz_mirror::store::FORMAT_VERSION
            ),
            "\"format_version\": \"1\"",
        );
        std::fs::write(d.join("meta.json"), meta).unwrap();
    })
    .await;
    assert_eq!(other_version, (older, true));
    // Can't be read, for a reason that says nothing about the store: the start fails, nothing is removed (nor an
    // older store served instead).
    let unreadable = start_with("unreadable", &template, |d| {
        std::fs::remove_file(d.join("meta.json")).unwrap();
        std::fs::create_dir(d.join("meta.json")).unwrap();
    })
    .await;
    assert!(unreadable.0.is_err() && unreadable.1, "{unreadable:?}");

    std::fs::remove_dir_all(&dir).unwrap();
}

/// One process at a time on a data directory.
#[tokio::test(flavor = "multi_thread")]
async fn a_data_directory_is_used_by_one_process() {
    let data_dir = temp_dir("locked");
    let addr = "127.0.0.1:9".parse().unwrap();
    let first = Mirror::new(config(&data_dir, addr)).unwrap();
    first.start().await.unwrap();
    let second = Mirror::new(config(&data_dir, addr)).unwrap();
    let error = second.start().await.unwrap_err().to_string();
    assert!(error.contains("in use by another process"), "{error}");
    drop(first);
    second.start().await.unwrap();
    drop(second);
    std::fs::remove_dir_all(&data_dir).unwrap();
}
