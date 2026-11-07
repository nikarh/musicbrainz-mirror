//! Builds the compatibility test's fixture: a small MusicBrainz dump with exactly what the requests of
//! `tests/compat/requests.txt` need, cut from a store built from a full dump, and records musicbrainz.org's
//! answers to the same requests.
//!
//! ```sh
//! cargo run --release --features record --example fixture -- dump --store DIR   # writes tests/fixtures/dump
//! cargo run --release --features record --example fixture -- record             # writes tests/compat/expected
//! ```
//!
//! The dump: every request is answered from the full store, which records each row it reads (the `record`
//! feature). Added to those are the rows the derived values are computed from (every release of a recording shown,
//! for its first release date; the areas containing an area, for its country), the small tables without indexes
//! (lookup tables) in full, and the annotation links and cover art types of the rows read. They are written back in
//! the dump's format: the columns the store keeps, the others empty.

// A development tool: it panics on anything unexpected.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stdout,
    clippy::indexing_slicing,
    clippy::too_many_lines,
    clippy::match_same_arms
)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use musicbrainz_mirror::kind::Kind;
use musicbrainz_mirror::store::build::SAME_AS_RECORDING;
use musicbrainz_mirror::store::schema::{self, Rows, Ty};
use musicbrainz_mirror::store::{Store, record, search};
use musicbrainz_mirror::ws;
use serde_json::Value;

const REQUESTS: &str = "tests/compat/requests.txt";
const OUT: &str = "tests/fixtures/dump";
const EXPECTED: &str = "tests/compat/expected";

/// The requests: `name path` per line.
fn requests() -> Vec<(String, String)> {
    std::fs::read_to_string(REQUESTS)
        .unwrap()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| {
            let (name, path) = l.split_once(char::is_whitespace).unwrap();
            (name.to_owned(), path.trim().to_owned())
        })
        .collect()
}

fn main() {
    let mut args = pico_args::Arguments::from_env();
    match args.subcommand().unwrap().as_deref() {
        Some("dump") => {
            let store: PathBuf = args.value_from_str("--store").unwrap();
            dump(&store);
        }
        Some("record") => record(),
        _ => println!("usage: fixture dump --store DIR | fixture record"),
    }
}

/// Saves musicbrainz.org's answers (at most one request a second, as its rules ask).
fn record() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let client = reqwest::Client::builder()
        .user_agent("musicbrainz-mirror-tests/0.1 ( https://github.com/nikarh/musicbrainz-mirror )")
        .build()
        .unwrap();
    std::fs::create_dir_all(EXPECTED).unwrap();
    for (name, path) in requests() {
        let url = format!("https://musicbrainz.org{path}");
        let (status, body) = runtime.block_on(async {
            for _ in 0..5 {
                let response = client.get(&url).send().await.unwrap();
                let status = response.status().as_u16();
                let body = response.text().await.unwrap();
                tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
                if status != 503 {
                    return (status, body);
                }
            }
            panic!("{url}: 503");
        });
        let ext = if path.contains("fmt=json") {
            "json"
        } else {
            "xml"
        };
        let body = if ext == "json" {
            serde_json::to_string_pretty(&serde_json::from_str::<Value>(&body).unwrap()).unwrap()
        } else {
            body
        };
        std::fs::write(
            format!("{EXPECTED}/{name}.{ext}"),
            format!("{status}\n{body}\n"),
        )
        .unwrap();
        println!("{status} {name}");
    }
}

/// The part-of link type between areas.
const PART_OF: &str = "de7cc874-8b1b-3a05-8272-f3834c968fb7";
/// Lookup tables (without indexes) with fewer rows than this are written in full.
const SMALL: usize = 30_000;

fn dump(dir: &Path) {
    let store = Arc::new(Store::open(dir).unwrap());
    let current: ws::Current = Arc::new(arc_swap::ArcSwapOption::new(Some(Arc::clone(&store))));
    let router = ws::router(current, "https://coverartarchive.org");
    let runtime = tokio::runtime::Runtime::new().unwrap();
    for (name, path) in requests() {
        let status = runtime.block_on(async {
            use tower::ServiceExt as _;
            let request = axum::http::Request::get(&path)
                .body(axum::body::Body::empty())
                .unwrap();
            let response = router.clone().oneshot(request).await.unwrap();
            let status = response.status().as_u16();
            let _ = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            status
        });
        println!("{status} {name}");
    }
    search_coverage(&store);
    dependencies(&store);
    let rows = record::take();
    write(&store, &rows);
}

/// Rows scanned per entity type for entities with search fields the fixture has none of yet.
const SCAN: usize = 3_000_000;

/// Reads an entity with each search field (and what its search document is made of), so the tests can check that
/// every field is indexed.
fn search_coverage(store: &Store) {
    let saved = record::take();
    let mut chosen = Vec::new();
    for kind in search::SEARCHABLE {
        let mut missing: BTreeSet<&str> = search::fields(kind)
            .iter()
            .filter_map(|f| f.2.first().copied())
            .collect();
        for row in store.t(kind.table()).iter().take(SCAN) {
            let values = search::field_values(store, kind, &row);
            if values.iter().any(|(name, _)| missing.contains(name)) {
                for (name, _) in &values {
                    missing.remove(name);
                }
                chosen.push((kind, row.id));
            }
            if missing.is_empty() {
                break;
            }
        }
        if !missing.is_empty() {
            println!("{}: no entity with {missing:?}", kind.name());
        }
    }
    // What the scan read isn't wanted: only what the chosen entities' documents are made of.
    let _ = record::take();
    for (kind, id) in chosen {
        let row = store.t(kind.table()).get(id).unwrap();
        let _ = search::field_values(store, kind, &row);
    }
    for (table, ids) in saved {
        for id in ids {
            let _ = store.t(&table).get(id);
        }
    }
}

/// Reads the rows the derived values of the rows read are computed from (recorded as they are read).
fn dependencies(store: &Store) {
    let rows = record::take();
    let ids = |table: &str| rows.get(table).cloned().unwrap_or_default();
    // Recordings: every release they are on, for their first release date.
    for recording in ids("recording") {
        for track in store.t("track").by("recording", recording) {
            let medium = track.u32("medium").and_then(|m| store.t("medium").get(m));
            if let Some(release) = medium.and_then(|m| m.u32("release")) {
                let _ = store.t("release").get(release);
            }
        }
    }
    // Releases: their events, for their first release date.
    let mut releases = ids("release");
    releases.extend(record::of("release"));
    for release in releases {
        let _ = store.t("release_country").by("release", release).count();
        let _ = store
            .t("release_unknown_country")
            .by("release", release)
            .count();
    }
    // Areas: the areas containing them, with their codes, for their country.
    let part_of = store
        .t("link_type")
        .iter()
        .find(|t| t.gid() == PART_OF)
        .map(|t| t.id);
    let mut areas: BTreeSet<u32> = ids("area");
    let mut seen = BTreeSet::new();
    while let Some(area) = areas.pop_first() {
        if !seen.insert(area) {
            continue;
        }
        let _ = store.t("iso_3166_1").by("area", area).count();
        for rel in store.t("l_area_area").by("entity1", area) {
            let link = rel.u32("link").and_then(|l| store.t("link").get(l));
            if link.and_then(|l| l.u32("link_type")) == part_of
                && let Some(parent) = rel.u32("entity0").and_then(|p| store.t("area").get(p))
            {
                areas.insert(parent.id);
            }
        }
    }
    // The artist credits of the credit names read (search results show their MBIDs).
    for name in ids("artist_credit_name")
        .into_iter()
        .chain(record::of("artist_credit_name"))
    {
        let row = store.t("artist_credit_name").get(name).unwrap();
        if let Some(credit) = row.u32("artist_credit") {
            let _ = store.t("artist_credit").get(credit);
        }
    }
    // Lookup tables, in full.
    for (name, spec) in schema::all() {
        let table = store.t(&name);
        // (not data tables: an entity type's own, its MBID redirects and metadata)
        let data = Kind::ALL
            .iter()
            .any(|k| k.table() == name && *k != Kind::Genre)
            || name.ends_with("_gid_redirect")
            || name.ends_with("_meta")
            || name == "release_group_cover_art";
        if spec.csr.is_empty() && table.len() < SMALL && !data {
            let _ = table.iter().count();
        }
    }
    // Put back what was taken.
    for (table, ids) in rows {
        for id in ids {
            let _ = store.t(&table).get(id);
        }
    }
}

/// A PostgreSQL COPY text field.
fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out
}

/// Writes the dump files: the rows read of each table, with the columns the store keeps.
fn write(store: &Store, rows: &BTreeMap<String, BTreeSet<u32>>) {
    let out = Path::new(OUT);
    let _ = std::fs::remove_dir_all(out);
    std::fs::create_dir_all(out.join("mbdump")).unwrap();
    for (key, file) in [
        ("timestamp", "TIMESTAMP"),
        ("schema_sequence", "SCHEMA_SEQUENCE"),
        ("replication_sequence", "REPLICATION_SEQUENCE"),
    ] {
        std::fs::write(
            out.join(file),
            format!("{}\n", store.meta.get(key).cloned().unwrap_or_default()),
        )
        .unwrap();
    }
    let none = BTreeSet::new();
    let annotations = rows.get("annotation").unwrap_or(&none);
    let cover_art: BTreeSet<u64> = rows
        .get("cover_art")
        .unwrap_or(&none)
        .iter()
        .filter_map(|&p| store.t("cover_art").get(p)?.u64("id"))
        .collect();

    let mut specs: Vec<_> = schema::all().into_values().collect();
    specs.sort_by(|a, b| a.name.cmp(&b.name));
    let mut total = 0;
    for spec in specs {
        let table = store.t(&spec.name);
        // Tables without an index to find the rows by: what the rows read refer to.
        let ids: Vec<u32> = if spec.name.ends_with("_annotation") {
            table
                .iter()
                .filter(|r| {
                    r.u32("annotation")
                        .is_some_and(|a| annotations.contains(&a))
                })
                .map(|r| r.id)
                .collect()
        } else if spec.name == "cover_art_type" {
            table
                .iter()
                .filter(|r| r.u64("id").is_some_and(|id| cover_art.contains(&id)))
                .map(|r| r.id)
                .collect()
        } else {
            rows.get(&spec.name)
                .map(|s| s.iter().copied().collect())
                .unwrap_or_default()
        };
        if ids.is_empty() {
            continue;
        }
        let mut text = String::new();
        for id in &ids {
            let row = table.get(*id).unwrap();
            let mut fields = vec!["\\N".to_owned(); spec.column_count];
            if let Rows::ById(column) = spec.rows {
                fields[column] = id.to_string();
            }
            for f in &spec.fields {
                match f.ty {
                    Ty::Gid => fields[f.column] = row.gid(),
                    Ty::U32 => {
                        if let Some(v) = row.u32(&f.name) {
                            fields[f.column] = v.to_string();
                        }
                    }
                    Ty::U64 => {
                        if let Some(v) = row.u64(&f.name) {
                            fields[f.column] = v.to_string();
                        }
                    }
                    Ty::Bool => fields[f.column] = if row.bool(&f.name) { "t" } else { "f" }.into(),
                    Ty::Str | Ty::InternedStr => {
                        let value = if row.str_offset(&f.name) == SAME_AS_RECORDING {
                            let recording = row
                                .u32("recording")
                                .and_then(|r| store.t("recording").get(r))
                                .unwrap();
                            recording.str("name")
                        } else {
                            row.str(&f.name)
                        };
                        if let Some(v) = value {
                            fields[f.column] = escape(v);
                        }
                    }
                    Ty::Date => {
                        let (y, m, d) = row.date(&f.name);
                        for (i, part) in [y, m, d].into_iter().enumerate() {
                            if let Some(v) = part {
                                fields[f.column + i] = v.to_string();
                            }
                        }
                    }
                }
            }
            text.push_str(&fields.join("\t"));
            text.push('\n');
        }
        total += ids.len();
        std::fs::write(out.join("mbdump").join(file_name(&spec.name)), text).unwrap();
    }
    println!("wrote {total} rows to {OUT}");
}

/// The file name a full export gives a table (the Cover Art Archive's are in a schema of their own).
fn file_name(table: &str) -> String {
    match table {
        "cover_art" | "cover_art_type" | "art_type" | "image_type" | "release_group_cover_art" => {
            format!("cover_art_archive.{table}")
        }
        _ => table.to_owned(),
    }
}
