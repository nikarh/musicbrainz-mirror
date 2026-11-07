//! Test helpers: an empty database with every table and index, and a check that a statement's query plan doesn't
//! scan a table it should find its rows in by an index.

// Test helpers: they panic on failure.
#![allow(clippy::missing_panics_doc, clippy::must_use_candidate)]

use rusqlite::Connection;

use crate::import::{DERIVE, INDEXES, SCHEMA};

/// Tables small enough to scan: lookup tables (hundreds of rows at most).
const SMALL: &[&str] = &[
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
    "work_attribute_type_allowed_value",
    "alias_type",
    "language",
    "script",
    "link_type",
    "link_attribute_type",
    "link_creditable_attribute_type",
    "art_type",
    "image_type",
    "genre",
    "meta",
];

/// An empty database as the importer leaves it.
pub fn empty_db() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(SCHEMA).unwrap();
    db.execute_batch(INDEXES).unwrap();
    db.execute_batch(DERIVE).unwrap();
    db
}

/// The problems of a statement's plan: full scans of tables that aren't small (the search tables' own scans,
/// `VIRTUAL TABLE INDEX`, are their index).
pub fn scans(db: &Connection, sql: &str) -> Vec<String> {
    let mut stmt = db
        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .unwrap_or_else(|e| panic!("{e}: {sql}"));
    let nulls = vec![rusqlite::types::Value::Null; stmt.parameter_count()];
    let details: Vec<String> = stmt
        .query_map(rusqlite::params_from_iter(nulls), |r| r.get::<_, String>(3))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    // Subqueries and CTEs the plan goes through as a whole.
    let derived: Vec<String> = details
        .iter()
        .filter_map(|d| {
            d.strip_prefix("CO-ROUTINE ")
                .or_else(|| d.strip_prefix("MATERIALIZE "))
        })
        .map(|n| n.split_whitespace().next().unwrap_or("").to_owned())
        .collect();
    details
        .into_iter()
        .filter(|d| {
            let Some(rest) = d.strip_prefix("SCAN ") else {
                return false;
            };
            let table = rest.split_whitespace().next().unwrap_or("");
            !rest.contains("VIRTUAL TABLE")
                && !rest.contains("USING INDEX")
                && !rest.contains("USING COVERING INDEX")
                && !SMALL.contains(&table)
                && !derived.iter().any(|n| n == table)
                // a subquery's or CTE's own result
                && !table.starts_with('(')
                && !rest.starts_with("CONSTANT ROW")
        })
        .collect()
}

/// Asserts that none of the statements scans a table, listing every one that does.
pub fn assert_indexed(db: &Connection, statements: &[(String, String)]) {
    let mut failures = Vec::new();
    for (name, sql) in statements {
        let scans = scans(db, sql);
        if !scans.is_empty() {
            failures.push(format!("{name}: {}\n    {sql}", scans.join(", ")));
        }
    }
    assert!(
        failures.is_empty(),
        "statements scanning tables:\n{}",
        failures.join("\n")
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::queries;
    use crate::kind::Kind;

    #[test]
    fn every_query_uses_indexes() {
        let db = empty_db();
        let mut statements: Vec<(String, String)> = queries::ALL
            .iter()
            .map(|(name, sql)| ((*name).to_owned(), (*sql).to_owned()))
            .collect();
        for kind in Kind::ALL {
            statements.push((
                format!("by_gid({})", kind.table()),
                queries::by_gid(kind.table()),
            ));
            statements.push((format!("sort_key({kind:?})"), queries::sort_key(kind)));
        }
        for table in ["medium", "track"] {
            statements.push((format!("by_gid({table})"), queries::by_gid(table)));
        }
        assert_indexed(&db, &statements);
    }

    #[test]
    fn every_query_is_listed() {
        let source = include_str!("queries.rs");
        for line in source.lines() {
            if let Some(name) = line
                .strip_prefix("pub const ")
                .and_then(|l| l.split(':').next())
                && name != "ALL"
            {
                assert!(
                    queries::ALL.iter().any(|(n, _)| *n == name),
                    "{name} is not in queries::ALL"
                );
            }
        }
    }

    #[test]
    fn every_browse_uses_indexes() {
        let db = empty_db();
        let filter = crate::ws::entity::Filter {
            status: vec![1],
            primary_types: vec![1],
            secondary_types: vec![1],
        };
        let mut statements = Vec::new();
        for (kind, linked) in crate::ws::browse::all() {
            for filter in [&crate::ws::entity::Filter::default(), &filter] {
                let (page, count) = crate::ws::browse::sql(kind, linked, filter).unwrap();
                statements.push((format!("{kind:?} by {linked:?} (page)"), page));
                statements.push((format!("{kind:?} by {linked:?} (count)"), count));
            }
        }
        assert_indexed(&db, &statements);
    }

    #[test]
    fn every_search_uses_indexes() {
        let db = empty_db();
        let statements = crate::ws::search::sample_sql();
        assert!(statements.len() > 300);
        assert_indexed(&db, &statements);
    }

    #[test]
    fn the_check_finds_scans() {
        let db = empty_db();
        assert_eq!(
            scans(&db, "SELECT id FROM recording WHERE name = 'x'"),
            vec!["SCAN recording".to_owned()]
        );
        assert_eq!(
            scans(&db, "SELECT id FROM recording WHERE gid = 'x'"),
            Vec::<String>::new()
        );
        assert_eq!(
            scans(&db, "SELECT id FROM genre ORDER BY name"),
            Vec::<String>::new()
        );
    }
}
