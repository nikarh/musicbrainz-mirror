//! Compares the dump tables the store reads (`src/store/schema.rs`) with a checkout of musicbrainz-server, whose
//! schema the dumps follow. CI runs it on a schedule against the branches that become the dumps' schema
//! (`.github/workflows/upstream-schema.yml`) and opens an issue when something differs.
//!
//! ```sh
//! git clone --depth 1 --filter=blob:none --sparse https://github.com/metabrainz/musicbrainz-server mbs
//! git -C mbs sparse-checkout set --no-cone /admin/sql/ /lib/DBDefs.pm.sample
//! cargo run --example upstream_schema -- mbs [URL of the checkout's tree, for links]
//! ```
//!
//! Prints a Markdown report of what differs: the schema sequence, schema change scripts newer than the store's
//! sequence (with their contents), and a diff of the columns of every table the store reads whose columns changed (a
//! changed column list fails the import, as each row's column count is checked). Exits 0 when nothing differs, 3 when
//! something does, 1 when the checkout can't be read.

#![allow(clippy::print_stdout, clippy::print_stderr, clippy::indexing_slicing)]

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;
use std::process::ExitCode;

use musicbrainz_mirror::store::schema::{self, SCHEMA_SEQUENCE};

const DIFFERS: u8 = 3;
/// Tables of the store that no dump has: genres have no MBID redirects, and the store keeps the table (empty) so that
/// MBIDs of every entity type resolve the same way. Reported when the schema gets one.
const NOT_DUMPED: &[&str] = &["genre_gid_redirect"];
/// A schema change script longer than this is cut (an issue's body is limited to 64 KiB).
const SCRIPT_MAX: usize = 12_000;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(dir) = args.next() else {
        eprintln!("usage: upstream_schema <musicbrainz-server checkout> [URL of its tree]");
        return ExitCode::FAILURE;
    };
    let url = args.next();
    match report(Path::new(&dir), url.as_deref()) {
        Ok(None) => ExitCode::SUCCESS,
        Ok(Some(text)) => {
            print!("{text}");
            ExitCode::from(DIFFERS)
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

fn report(dir: &Path, url: Option<&str>) -> Result<Option<String>, String> {
    let read =
        |path: &str| std::fs::read_to_string(dir.join(path)).map_err(|e| format!("{path}: {e}"));
    let link = |path: &str| match url {
        Some(url) => format!("[`{path}`]({}/{path})", url.trim_end_matches('/')),
        None => format!("`{path}`"),
    };
    // Inside HTML (a `<summary>`), where GitHub renders no Markdown.
    let html_link = |path: &str| match url {
        Some(url) => format!(
            "<a href=\"{}/{path}\"><code>{path}</code></a>",
            url.trim_end_matches('/')
        ),
        None => format!("<code>{path}</code>"),
    };

    let mut upstream = create_tables(&read("admin/sql/CreateTables.sql")?)?;
    // The dumps' `cover_art_archive.<table>` files, which the store reads as `<table>`.
    for (name, columns) in create_tables(&read("admin/sql/caa/CreateTables.sql")?)? {
        if upstream.insert(name.clone(), columns).is_some() {
            return Err(format!(
                "{name} is in both CreateTables.sql and caa/CreateTables.sql"
            ));
        }
    }
    let sequence = schema_sequence(&read("lib/DBDefs.pm.sample")?)
        .ok_or("lib/DBDefs.pm.sample: no DB_SCHEMA_SEQUENCE")?;
    let scripts = newer_scripts(&dir.join("admin/sql/updates/schema-change"))?;

    let mut tables: Vec<_> = schema::all().into_values().collect();
    tables.sort_by(|a, b| a.name.cmp(&b.name));
    let mut columns = String::new();
    for table in &tables {
        let not_dumped = NOT_DUMPED.contains(&table.name.as_str());
        match upstream.get(&table.name) {
            None if not_dumped => {}
            None => _ = writeln!(columns, "- `{}`: no longer in the schema", table.name),
            Some(_) if not_dumped => _ = writeln!(columns, "- `{}`: now in the schema", table.name),
            Some(theirs) if *theirs != table.columns => {
                _ = writeln!(columns, "- `{}`:\n\n  ```diff", table.name);
                for line in diff(&table.columns, theirs) {
                    _ = writeln!(columns, "  {line}");
                }
                _ = writeln!(columns, "  ```");
            }
            Some(_) => {}
        }
    }

    if sequence == SCHEMA_SEQUENCE && scripts.is_empty() && columns.is_empty() {
        return Ok(None);
    }
    let mut out = String::new();
    if sequence != SCHEMA_SEQUENCE {
        _ = writeln!(
            out,
            "Schema sequence **{sequence}** (`DB_SCHEMA_SEQUENCE`); the store's `SCHEMA_SEQUENCE` is {SCHEMA_SEQUENCE}.\n"
        );
    }
    if columns.is_empty() {
        _ = writeln!(
            out,
            "The columns of the tables the store reads are unchanged.\n"
        );
    } else {
        _ = writeln!(
            out,
            "Tables the store reads whose columns changed (`-` the store's, `+` this branch's {}):\n\n{columns}",
            link("admin/sql/CreateTables.sql")
        );
    }
    if !scripts.is_empty() {
        _ = writeln!(out, "Schema change scripts newer than {SCHEMA_SEQUENCE}:\n");
        for (name, text) in &scripts {
            let path = format!("admin/sql/updates/schema-change/{name}");
            let cut = text.len() > SCRIPT_MAX;
            let text = if cut {
                &text[..text.floor_char_boundary(SCRIPT_MAX)]
            } else {
                text.as_str()
            };
            _ = writeln!(
                out,
                "<details><summary>{}</summary>\n\n```sql\n{}\n{}```\n\n</details>\n",
                html_link(&path),
                text.trim_end(),
                if cut { "-- (cut)\n" } else { "" }
            );
        }
    }
    Ok(Some(out))
}

/// The columns of every `CREATE TABLE` (by table name, without its schema), in order.
fn create_tables(sql: &str) -> Result<BTreeMap<String, Vec<String>>, String> {
    let sql = strip_comments(sql);
    let upper = sql.to_ascii_uppercase();
    let mut tables = BTreeMap::new();
    let mut from = 0;
    while let Some(at) = upper[from..].find("CREATE TABLE ") {
        let start = from + at + "CREATE TABLE ".len();
        let open = sql[start..]
            .find('(')
            .map(|i| start + i)
            .ok_or("CREATE TABLE without columns")?;
        let name = sql[start..open].trim();
        let name = name
            .rsplit('.')
            .next()
            .unwrap_or(name)
            .trim_matches('"')
            .to_owned();
        let close =
            closing(&sql, open).ok_or_else(|| format!("{name}: no end of the column list"))?;
        let columns = split_top_level(&sql[open + 1..close])
            .into_iter()
            .filter_map(|element| {
                let first = element.split_whitespace().next()?;
                let constraint = [
                    "CONSTRAINT",
                    "PRIMARY",
                    "UNIQUE",
                    "CHECK",
                    "FOREIGN",
                    "EXCLUDE",
                    "LIKE",
                ]
                .iter()
                .any(|k| first.eq_ignore_ascii_case(k));
                (!constraint).then(|| first.trim_matches('"').to_owned())
            })
            .collect();
        if tables.insert(name.clone(), columns).is_some() {
            return Err(format!("{name} is created twice"));
        }
        from = close;
    }
    Ok(tables)
}

/// Without `--` comments (outside of string literals).
fn strip_comments(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    for line in sql.lines() {
        let mut quoted = false;
        let mut end = line.len();
        let bytes = line.as_bytes();
        for (i, &b) in bytes.iter().enumerate() {
            match b {
                b'\'' => quoted = !quoted,
                b'-' if !quoted && bytes.get(i + 1) == Some(&b'-') => {
                    end = i;
                    break;
                }
                _ => {}
            }
        }
        out.push_str(&line[..end]);
        out.push('\n');
    }
    out
}

/// The position of the parenthesis closing the one at `open`.
fn closing(sql: &str, open: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut quoted = false;
    for (i, b) in sql.bytes().enumerate().skip(open) {
        match b {
            b'\'' => quoted = !quoted,
            b'(' if !quoted => depth += 1,
            b')' if !quoted => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Splits at the commas outside of parentheses and string literals.
fn split_top_level(body: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut depth, mut quoted, mut start) = (0usize, false, 0);
    for (i, b) in body.bytes().enumerate() {
        match b {
            b'\'' => quoted = !quoted,
            b'(' if !quoted => depth += 1,
            b')' if !quoted => depth = depth.saturating_sub(1),
            b',' if !quoted && depth == 0 => {
                parts.push(body[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(body[start..].trim());
    parts.retain(|p| !p.is_empty());
    parts
}

/// `sub DB_SCHEMA_SEQUENCE { 31 }`
fn schema_sequence(perl: &str) -> Option<u32> {
    let line = perl
        .lines()
        .find(|l| l.trim_start().starts_with("sub DB_SCHEMA_SEQUENCE"))?;
    let inner = line.split_once('{')?.1.split_once('}')?.0;
    inner.trim().parse().ok()
}

/// The schema change scripts (`<sequence>.<part>.sql`) for sequences after the store's: (file name, contents).
fn newer_scripts(dir: &Path) -> Result<Vec<(String, String)>, String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut scripts = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("{}: {e}", dir.display()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let sequence = name
            .split_once('.')
            .and_then(|(n, _)| n.parse::<u32>().ok());
        if sequence.is_some_and(|n| n > SCHEMA_SEQUENCE)
            && entry.path().extension().is_some_and(|e| e == "sql")
        {
            let text = std::fs::read_to_string(entry.path()).map_err(|e| format!("{name}: {e}"))?;
            scripts.push((sequence, name, text));
        }
    }
    scripts.sort();
    Ok(scripts
        .into_iter()
        .map(|(_, name, text)| (name, text))
        .collect())
}

/// A line diff of two column lists: ` ` kept, `-` only in `old`, `+` only in `new`.
fn diff(old: &[String], new: &[String]) -> Vec<String> {
    let (n, m) = (old.len(), new.len());
    // Lengths of the longest common subsequence of old[i..] and new[j..].
    let mut lcs = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if old[i] == new[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let (mut i, mut j, mut out) = (0, 0, Vec::new());
    while i < n || j < m {
        if i < n && j < m && old[i] == new[j] {
            out.push(format!(" {}", old[i]));
            (i, j) = (i + 1, j + 1);
        } else if j < m && (i == n || lcs[i][j + 1] >= lcs[i + 1][j]) {
            out.push(format!("+{}", new[j]));
            j += 1;
        } else {
            out.push(format!("-{}", old[i]));
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SQL: &str = "
CREATE TABLE artist ( -- replicate (verbose)
    id                  SERIAL,
    comment             VARCHAR(255) NOT NULL DEFAULT '', -- a comment, with -- and (
    ended               BOOLEAN NOT NULL DEFAULT FALSE
      CONSTRAINT artist_ended_check CHECK (
        (end_date_year IS NOT NULL OR
         end_date_month IS NOT NULL) AND ended = TRUE
      ),
    begin_area          INTEGER -- references area.id
);

create table cover_art_archive.cover_art_type (
    id BIGINT NOT NULL, -- PK, references cover_art_archive.cover_art.id CASCADE,
    type_id INTEGER NOT NULL, -- PK, references cover_art_archive.art_type.id,
    CONSTRAINT one_type UNIQUE (id, type_id)
);
";

    fn strings(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn columns_of_create_table() {
        let tables = create_tables(SQL).unwrap_or_default();
        assert_eq!(
            tables.get("artist"),
            Some(&strings("id comment ended begin_area"))
        );
        assert_eq!(tables.get("cover_art_type"), Some(&strings("id type_id")));
        assert_eq!(tables.len(), 2);
    }

    #[test]
    fn sequence() {
        assert_eq!(
            schema_sequence("sub X { 1 }\nsub DB_SCHEMA_SEQUENCE { 31 }\n"),
            Some(31)
        );
        assert_eq!(schema_sequence("sub ACTIVE_SCHEMA_SEQUENCE { 31 }\n"), None);
    }

    #[test]
    fn column_diff() {
        assert_eq!(
            diff(
                &strings("id name type comment"),
                &strings("id name comment kind")
            ),
            strings("_id _name -type _comment +kind")
                .into_iter()
                .map(|s| s.replacen('_', " ", 1))
                .collect::<Vec<_>>()
        );
    }
}
