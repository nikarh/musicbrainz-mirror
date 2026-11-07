//! Compatibility with musicbrainz.org: the requests of `tests/compat/requests.txt`, answered from a database
//! imported from the fixture dump (`tests/fixtures/dump`, the data the requests need, cut from a full dump by
//! `examples/fixture.rs`), must give what musicbrainz.org gave (`tests/compat/expected`, recorded by the same tool
//! when the fixture was made). JSON must be equal; XML must have the same elements, attributes and text (in any
//! order).
//!
//! Differences that are known and accepted are listed in `tests/compat/known-differences.txt` (request name and
//! JSON path, with the reason).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::print_stderr,
    clippy::many_single_char_names,
    clippy::items_after_statements
)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use musicbrainz_mirror::store::{self, Store, search};
use musicbrainz_mirror::ws;
use musicbrainz_mirror::ws::search::query;
use serde_json::Value;
use tower::ServiceExt as _;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The fixture imported (with its rows compressed or not), once per test binary.
fn database(compressed: bool) -> &'static Path {
    static PLAIN: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    static COMPRESSED: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    let cell = if compressed { &COMPRESSED } else { &PLAIN };
    cell.get_or_init(|| {
        // under cargo's temporary directory for tests, the same each run (a run replaces it)
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(if compressed {
            "compat-compressed"
        } else {
            "compat-plain"
        });
        let _ = std::fs::remove_dir_all(&dir);
        store::build::build(
            &[root().join("tests/fixtures/dump")],
            "fixture",
            &dir,
            &store::build::BuildOptions {
                threads: 1,
                memory_mb: 50,
                compress_rows: compressed,
            },
        )
        .unwrap();
        dir
    })
}

async fn get(router: &axum::Router, path: &str) -> (u16, String) {
    let request = axum::http::Request::get(path)
        .body(axum::body::Body::empty())
        .unwrap();
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status().as_u16();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8(body.to_vec()).unwrap())
}

fn diff(path: &str, ours: &Value, theirs: &Value, out: &mut Vec<String>) {
    match (ours, theirs) {
        (Value::Object(a), Value::Object(b)) => {
            for key in a
                .keys()
                .chain(b.keys())
                .collect::<std::collections::BTreeSet<_>>()
            {
                let p = format!("{path}.{key}");
                match (a.get(key), b.get(key)) {
                    (Some(x), Some(y)) => diff(&p, x, y, out),
                    (Some(x), None) => out.push(format!("{p}: only here: {x}")),
                    (None, Some(y)) => out.push(format!("{p}: only at musicbrainz.org: {y}")),
                    (None, None) => {}
                }
            }
        }
        (Value::Array(a), Value::Array(b)) => {
            if a.len() != b.len() {
                out.push(format!(
                    "{path}: {} items here, {} at musicbrainz.org",
                    a.len(),
                    b.len()
                ));
            }
            for (i, (x, y)) in a.iter().zip(b).enumerate() {
                diff(&format!("{path}[{i}]"), x, y, out);
            }
        }
        // 4.0 and 4 are the same rating
        (Value::Number(a), Value::Number(b)) if a.as_f64() == b.as_f64() => {}
        _ if ours != theirs => {
            out.push(format!("{path}: {ours} here, {theirs} at musicbrainz.org"));
        }
        _ => {}
    }
}

/// An XML document as the multiset of its elements' paths, attributes and texts.
fn xml_facts(xml: &str) -> BTreeMap<String, usize> {
    let doc = roxmltree::Document::parse(xml).unwrap();
    let mut facts = BTreeMap::new();
    fn walk(node: roxmltree::Node<'_, '_>, path: &str, facts: &mut BTreeMap<String, usize>) {
        for child in node.children().filter(roxmltree::Node::is_element) {
            let name = child.tag_name().name();
            let p = format!("{path}/{name}");
            let mut attrs: Vec<String> = child
                .attributes()
                // the search server's timestamp
                .filter(|a| a.name() != "created")
                .map(|a| format!("{}={}", a.name(), a.value()))
                .collect();
            attrs.sort();
            let text = child
                .children()
                .filter(roxmltree::Node::is_text)
                .map(|t| t.text().unwrap_or(""))
                .collect::<String>();
            *facts
                .entry(format!("{p}[{}] {}", attrs.join(" "), text.trim()))
                .or_default() += 1;
            walk(child, &p, facts);
        }
    }
    walk(doc.root(), "", &mut facts);
    facts
}

#[tokio::test]
async fn answers_as_musicbrainz_org() {
    for compressed in [false, true] {
        answers_from(compressed).await;
    }
}

async fn answers_from(compressed: bool) {
    let store = Store::open(database(compressed)).unwrap();
    let router = ws::router(
        Arc::new(arc_swap::ArcSwapOption::from_pointee(store)),
        "https://coverartarchive.org",
    );
    let known: Vec<(String, String)> =
        std::fs::read_to_string(root().join("tests/compat/known-differences.txt"))
            .unwrap_or_default()
            .lines()
            .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
            .map(|l| {
                let mut parts = l.split_whitespace();
                (
                    parts.next().unwrap().to_owned(),
                    parts.next().unwrap_or("").to_owned(),
                )
            })
            .collect();

    let requests = std::fs::read_to_string(root().join("tests/compat/requests.txt")).unwrap();
    let mut failures = Vec::new();
    let mut checked = 0;
    for line in requests
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
    {
        let (name, path) = line.split_once(char::is_whitespace).unwrap();
        let path = path.trim();
        let json = path.contains("fmt=json");
        let expected_file = root().join(format!(
            "tests/compat/expected/{name}.{}",
            if json { "json" } else { "xml" }
        ));
        let expected = std::fs::read_to_string(&expected_file)
            .unwrap_or_else(|e| panic!("{}: {e}", expected_file.display()));
        let (status, body) = expected.split_once('\n').unwrap();
        let (our_status, ours) = get(&router, path).await;
        let mut diffs = Vec::new();
        if our_status.to_string() != status {
            diffs.push(format!(
                "status {our_status} here, {status} at musicbrainz.org"
            ));
        }
        if json {
            let theirs: Value = serde_json::from_str(body).unwrap();
            let ours: Value = serde_json::from_str(&ours).unwrap_or(Value::String(ours));
            diff("", &ours, &theirs, &mut diffs);
        } else {
            let (a, b) = (xml_facts(&ours), xml_facts(body));
            for (fact, n) in &a {
                if b.get(fact) != Some(n) {
                    diffs.push(format!(
                        "{fact}: {n}× here, {}× at musicbrainz.org",
                        b.get(fact).unwrap_or(&0)
                    ));
                }
            }
            for (fact, n) in &b {
                if !a.contains_key(fact) {
                    diffs.push(format!("{fact}: only at musicbrainz.org ({n}×)"));
                }
            }
        }
        diffs.retain(|d| {
            !known
                .iter()
                .any(|(n, p)| n == name && d.starts_with(p.as_str()))
        });
        checked += 1;
        if !diffs.is_empty() {
            failures.push(format!("{name} ({path}):\n  {}", diffs.join("\n  ")));
        }
    }
    assert!(checked > 0);
    assert!(
        failures.is_empty(),
        "{} of {checked} requests differ (compressed rows: {compressed}):\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// Every search field of every entity type is indexed: the fixture has an entity with each (the fixture tool
/// picks them), and `field:"value"` finds the entity for each of its values.
#[test]
fn every_search_field_is_indexed() {
    let store = Store::open(database(false)).unwrap();
    let mut failures = Vec::new();
    for kind in search::SEARCHABLE {
        let spec = search::fields(kind);
        let names: std::collections::BTreeSet<&str> =
            spec.iter().flat_map(|f| f.2.iter().copied()).collect();
        let mut found = std::collections::BTreeSet::new();
        let mut checked = std::collections::BTreeMap::new();
        for row in store.t(kind.table()).iter() {
            for (name, value) in search::field_values(&store, kind, &row) {
                if !names.contains(name) {
                    failures.push(format!("{}: {name} is not a search field", kind.name()));
                    continue;
                }
                // (enough values of each field to show it works)
                let checks = checked.entry(name).or_insert(0);
                *checks += 1;
                if *checks > 50 {
                    continue;
                }
                let text = value.replace('\\', "\\\\").replace('"', "\\\"");
                let q = format!("{name}:\"{text}\"");
                let (hits, _) = store
                    .search
                    .search(&store, kind, &query::parse(&q), 0, 10_000)
                    .unwrap();
                if hits.iter().any(|h| h.0 == row.id) {
                    found.insert(name);
                } else if value.chars().any(char::is_alphanumeric) {
                    // (a text of punctuation only has no words to find it by)
                    failures.push(format!(
                        "{} {}: {q} doesn't find it",
                        kind.name(),
                        row.gid()
                    ));
                }
            }
        }
        for name in spec.iter().filter_map(|f| f.2.first()) {
            if !found.contains(name) {
                failures.push(format!(
                    "{}: no entity of the fixture has {name}",
                    kind.name()
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
