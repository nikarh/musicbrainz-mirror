//! What the integration tests share: dump archives made from the fixture, and a store's contents to compare.

#![allow(dead_code, clippy::unwrap_used)] // each test uses some

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

pub fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dump")
}

/// A dump archive: a bzip2'd tar of the fixture's files whose names start with one of `prefixes`.
pub fn archive(fixture: &Path, prefixes: &[&str]) -> Vec<u8> {
    let encoder = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::fast());
    let mut tar = tar::Builder::new(encoder);
    for name in ["TIMESTAMP", "SCHEMA_SEQUENCE", "REPLICATION_SEQUENCE"] {
        tar.append_path_with_name(fixture.join(name), name).unwrap();
    }
    let mut files: Vec<_> = std::fs::read_dir(fixture.join("mbdump"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    files.sort();
    for file in files {
        let name = file.file_name().unwrap().to_string_lossy().into_owned();
        if prefixes.iter().any(|p| p.is_empty() || name.starts_with(p)) {
            tar.append_path_with_name(&file, format!("mbdump/{name}"))
                .unwrap();
        }
    }
    let mut encoder = tar.into_inner().unwrap();
    encoder.flush().unwrap();
    encoder.finish().unwrap()
}

/// The files of a store and their contents, but the search indexes' (tantivy's files aren't the same from one build
/// to the next): the number of documents of each.
pub fn contents(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut files = BTreeMap::new();
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
        } else {
            files.insert(name, std::fs::read(entry.path()).unwrap());
        }
    }
    files
}

/// Asserts that two stores have the same files with the same contents.
pub fn assert_same_store(got: &Path, expected: &Path) {
    let (got, expected) = (contents(got), contents(expected));
    assert_eq!(
        got.keys().collect::<Vec<_>>(),
        expected.keys().collect::<Vec<_>>()
    );
    for (name, bytes) in &expected {
        assert!(got.get(name) == Some(bytes), "{name} differs");
    }
}
