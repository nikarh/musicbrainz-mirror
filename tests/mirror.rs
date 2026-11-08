//! The updater end to end: a local server stands in for MusicBrainz's full exports (`LATEST`, `SHA256SUMS`, the
//! archives, made from the fixture dump); the mirror downloads a dump, imports it, serves it, doesn't download it
//! again while it is the newest, and switches to a newer one.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Path as UrlPath, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use musicbrainz_mirror::config::{BuildOptions, Config};
use musicbrainz_mirror::mirror::Mirror;
use sha2::{Digest as _, Sha256};

mod common;
use common::archive;

#[derive(Default)]
struct Exports {
    latest: Mutex<String>,
    /// file name → contents, the same for every dump
    files: Vec<(String, Vec<u8>)>,
    archive_downloads: AtomicUsize,
}

async fn serve(
    State(exports): State<Arc<Exports>>,
    UrlPath(path): UrlPath<String>,
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
            let hash = Sha256::digest(data);
            let hex: String = hash.iter().fold(String::new(), |mut s, b| {
                let _ = write!(s, "{b:02x}");
                s
            });
            let _ = writeln!(sums, "{hex} *{name}");
        }
        return sums.into_response();
    }
    match exports.files.iter().find(|(name, _)| name == file) {
        Some((_, data)) => {
            exports.archive_downloads.fetch_add(1, Ordering::Relaxed);
            data.clone().into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// A test's own directory under cargo's temporary directory for tests (the same each run: a run replaces it).
fn temp_dir(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("mirror-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[tokio::test(flavor = "multi_thread")]
async fn downloads_imports_and_switches_dumps() {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dump");
    let core = archive(&fixture, &[""]);
    let exports = Arc::new(Exports {
        latest: Mutex::new("20261101-000000\n".into()),
        files: vec![
            ("mbdump.tar.bz2".into(), core),
            // The fixture's archives overlap: what is in two of them is imported twice. Derived and cover art
            // tables only appear in the core archive here, so these two are just headers.
            (
                "mbdump-derived.tar.bz2".into(),
                archive(&fixture, &["nothing"]),
            ),
            (
                "mbdump-cover-art-archive.tar.bz2".into(),
                archive(&fixture, &["nothing"]),
            ),
            ("mbdump-edit.tar.bz2".into(), b"not downloaded".to_vec()),
        ],
        archive_downloads: AtomicUsize::new(0),
    });
    let app = axum::Router::new()
        .route("/exports/{*path}", axum::routing::get(serve))
        .with_state(Arc::clone(&exports));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let data_dir = temp_dir("mirror");
    let config = Config {
        data_dir: data_dir.clone(),
        listen: "127.0.0.1:0".parse().unwrap(),
        update_interval: Some(Duration::from_hours(1)),
        min_free_bytes: 0,
        dump_url: format!("http://{addr}/exports"),
        cover_art_url: "https://coverartarchive.org".into(),
        build: BuildOptions {
            threads: 2,
            memory_mb: 64,
            compress_rows: true,
        },
    };
    let mirror = Mirror::new(config).unwrap();
    mirror.start().await.unwrap();
    assert!(mirror.current().load_full().is_none());

    // The first dump: downloaded, imported, served, its downloads removed.
    mirror.update().await.unwrap();
    assert_eq!(
        mirror.current().load_full().unwrap().dump(),
        "20261101-000000"
    );
    assert_eq!(exports.archive_downloads.load(Ordering::Relaxed), 3);
    assert!(
        data_dir
            .join("musicbrainz-20261101-000000/meta.json")
            .exists()
    );
    assert!(!data_dir.join("import/20261101-000000").exists());

    // Still the newest: nothing downloaded.
    mirror.update().await.unwrap();
    assert_eq!(exports.archive_downloads.load(Ordering::Relaxed), 3);

    // A newer one: switched to; the old database is removed once nothing uses it.
    *exports.latest.lock().unwrap() = "20261104-000000".into();
    mirror.update().await.unwrap();
    assert_eq!(
        mirror.current().load_full().unwrap().dump(),
        "20261104-000000"
    );
    assert_eq!(exports.archive_downloads.load(Ordering::Relaxed), 6);
    for _ in 0..50 {
        if !data_dir.join("musicbrainz-20261101-000000").exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(!data_dir.join("musicbrainz-20261101-000000").exists());

    // A restart serves what is there.
    let restarted = Mirror::new(Config {
        update_interval: None,
        ..mirror_config(&data_dir, addr)
    })
    .unwrap();
    restarted.start().await.unwrap();
    assert_eq!(
        restarted.current().load_full().unwrap().dump(),
        "20261104-000000"
    );
    std::fs::remove_dir_all(&data_dir).unwrap();
}

fn mirror_config(data_dir: &Path, addr: std::net::SocketAddr) -> Config {
    Config {
        data_dir: data_dir.to_owned(),
        listen: "127.0.0.1:0".parse().unwrap(),
        update_interval: Some(Duration::from_hours(1)),
        min_free_bytes: 0,
        dump_url: format!("http://{addr}/exports"),
        cover_art_url: "https://coverartarchive.org".into(),
        build: BuildOptions {
            threads: 2,
            memory_mb: 64,
            compress_rows: true,
        },
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_corrupt_download_is_not_imported() {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dump");
    let exports = Arc::new(Exports {
        latest: Mutex::new("20261101-000000".into()),
        files: vec![
            ("mbdump.tar.bz2".into(), archive(&fixture, &[""])),
            (
                "mbdump-derived.tar.bz2".into(),
                archive(&fixture, &["nothing"]),
            ),
            (
                "mbdump-cover-art-archive.tar.bz2".into(),
                archive(&fixture, &["nothing"]),
            ),
        ],
        archive_downloads: AtomicUsize::new(0),
    });
    // The checksums are of these files, the server sends others.
    let sums_of = Arc::clone(&exports);
    let bad = Arc::new(Exports {
        latest: Mutex::new("20261101-000000".into()),
        files: sums_of
            .files
            .iter()
            .map(|(n, d)| (n.clone(), d.iter().rev().copied().collect()))
            .collect(),
        archive_downloads: AtomicUsize::new(0),
    });
    let app = axum::Router::new()
        .route(
            "/exports/{*path}",
            axum::routing::get(
                move |State(bad): State<Arc<Exports>>, UrlPath(path): UrlPath<String>| {
                    let good = Arc::clone(&sums_of);
                    async move {
                        if path.ends_with("SHA256SUMS") || path == "LATEST" {
                            serve(State(good), UrlPath(path)).await
                        } else {
                            serve(State(bad), UrlPath(path)).await
                        }
                    }
                },
            ),
        )
        .with_state(Arc::clone(&bad));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let data_dir = temp_dir("corrupt");
    let mirror = Mirror::new(mirror_config(&data_dir, addr)).unwrap();
    mirror.start().await.unwrap();
    let error = mirror.update().await.unwrap_err().to_string();
    assert!(error.contains("checksum"), "{error}");
    assert!(mirror.current().load_full().is_none());
    std::fs::remove_dir_all(&data_dir).unwrap();
}
