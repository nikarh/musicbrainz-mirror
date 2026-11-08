//! Interruptions: the process stopped (killed) at any point of an update, and started again. What was downloaded
//! isn't downloaded again.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::{Path as UrlPath, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use musicbrainz_mirror::dump;
use sha2::{Digest as _, Sha256};

mod common;
use common::{archive, fixture};

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
