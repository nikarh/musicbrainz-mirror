//! Search (`/ws/2/<type>?query=...`), on the FTS5 tables of derive.sql.
//!
//! MusicBrainz's search server is Solr; this is an approximation of it that ranks the results clients look for
//! first. A query's clauses (query.rs) are either text, matched in a search table, or field values (MBIDs, types,
//! dates, counts), checked in SQL:
//!
//! - Text: first all of it must match (every clause and word); if that finds too few, any clause may (`OR`), as in
//!   Lucene, ranked after the strict matches. Ranking within each is FTS5's BM25.
//! - Field values marked `+` (or joined with `AND`) must match; others only move results that match them up, so a
//!   wrong hint (`tracks:12` for an album with 13) doesn't hide the right result. A query of field values only
//!   matches entities with at least one of them.
//!
//! Results are shaped as the search server shapes them (format.rs), with a 0-100 `score`.

mod format;
pub mod query;

use std::collections::HashSet;

use rusqlite::{Connection, ToSql};
use serde_json::{Map, Value, json};

use self::query::{Node, Occur};
use super::Answer;
use super::browse::Page;
use crate::db::Lookups;
use crate::kind::Kind;

type R<T> = rusqlite::Result<T>;

/// Candidates looked at for ranking (at least; more when a later page is asked for).
const RANK_WINDOW: i64 = 100;

/// Where a query field is matched.
enum Field {
    /// In these columns of the entity type's search table.
    Text(&'static [&'static str]),
    /// A condition on the entity (`e`) in SQL.
    Sql(Cond),
}

/// A condition: SQL with `?` for the value, which is bound as given (after `normalize`).
#[derive(Clone, Copy)]
enum Cond {
    /// `e.id IN (subquery)`: selective (MBIDs, codes), can drive a query.
    In(&'static str),
    /// Any expression over `e` with one `?`.
    Expr(&'static str),
    /// A type or status by name: the lookup table and the column of `e` (or an expression with `?` for the id).
    Named(&'static str, &'static str),
    /// A partial date or range of them: SQL with `{cond}` for the condition over `date_year`, `date_month`,
    /// `date_day` (or the packed `date` when `packed`).
    Date(&'static str, bool),
    /// A number or range of them: SQL with `{cond}` over `n`.
    Number(&'static str),
    /// `true` / `false`
    Bool(&'static str),
}

/// The default columns of each search table, and its table name.
fn table(kind: Kind) -> Option<(&'static str, &'static [&'static str])> {
    Some(match kind {
        Kind::Artist => ("artist_fts", &["name", "sortname", "alias"]),
        Kind::Release => ("release_fts", &["name", "alias"]),
        Kind::ReleaseGroup => ("release_group_fts", &["name", "alias"]),
        Kind::Recording => ("recording_fts", &["name", "alias"]),
        Kind::Work => ("work_fts", &["name", "alias"]),
        Kind::Label => ("label_fts", &["name", "alias"]),
        Kind::Area => ("area_fts", &["name", "alias"]),
        Kind::Place => ("place_fts", &["name", "alias"]),
        Kind::Event => ("event_fts", &["name", "alias"]),
        Kind::Instrument => ("instrument_fts", &["name", "alias"]),
        Kind::Series => ("series_fts", &["name", "alias"]),
        Kind::Genre | Kind::Url => return None,
    })
}

const TAG: &str = "SELECT et.entity FROM tag t JOIN entity_tag et ON et.tag = t.id AND et.kind = {kind} WHERE t.name = ?";
const RELEASE_DATE: &str = "EXISTS (SELECT 1 FROM release_country WHERE release = e.id AND {cond} \
    UNION ALL SELECT 1 FROM release_unknown_country WHERE release = e.id AND {cond})";

/// The fields of each entity type's search (the search server's names).
#[allow(clippy::too_many_lines, clippy::match_same_arms)] // a table
fn field(kind: Kind, name: &str) -> Option<Field> {
    use Cond::{Bool, Date, Expr, In, Named, Number};
    use Field::{Sql, Text};
    let common = match name {
        "alias" => Some(Text(&["alias"])),
        "comment" => Some(Text(&["comment"])),
        "tag" => Some(Sql(In(TAG))),
        _ => None,
    };
    if common.is_some() {
        return common;
    }
    Some(match (kind, name) {
        (Kind::Artist, "artist" | "artistaccent" | "name") => Text(&["name", "alias"]),
        (Kind::Artist, "sortname") => Text(&["sortname"]),
        (Kind::Artist, "arid") => Sql(In("SELECT id FROM artist WHERE gid = ?")),
        (Kind::Artist, "type") => Sql(Named("artist_type", "e.type")),
        (Kind::Artist, "gender") => Sql(Named("gender", "e.gender")),
        (Kind::Artist, "country") => Sql(Expr(
            "(SELECT code FROM area_country WHERE area = e.area) = upper(?)",
        )),
        (Kind::Artist, "area") => Sql(Expr("(SELECT lower(name) FROM area WHERE id = e.area) = ?")),
        (Kind::Artist, "beginarea") => Sql(Expr(
            "(SELECT lower(name) FROM area WHERE id = e.begin_area) = ?",
        )),
        (Kind::Artist, "endarea") => Sql(Expr(
            "(SELECT lower(name) FROM area WHERE id = e.end_area) = ?",
        )),
        (Kind::Artist, "begin") => Sql(Date("({cond})", false)),
        (Kind::Artist, "ended") => Sql(Bool("e.ended")),
        (Kind::Artist, "ipi") => Sql(In("SELECT artist FROM artist_ipi WHERE ipi = ?")),
        (Kind::Artist, "isni") => Sql(In("SELECT artist FROM artist_isni WHERE isni = ?")),

        (Kind::Release, "release" | "releaseaccent") => Text(&["name", "alias"]),
        (Kind::Release, "artist" | "artistname" | "creditname") => Text(&["artist"]),
        (Kind::Release, "label") => Text(&["label"]),
        (Kind::Release, "catno") => Text(&["catno"]),
        (Kind::Release, "reid") => Sql(In("SELECT id FROM release WHERE gid = ?")),
        (Kind::Release, "rgid") => Sql(In(
            "SELECT r.id FROM release_group rg JOIN release r ON r.release_group = rg.id WHERE rg.gid = ?",
        )),
        (Kind::Release, "arid") => Sql(In(
            "SELECT r.id FROM artist a JOIN artist_credit_name acn ON acn.artist = a.id \
            JOIN release r ON r.artist_credit = acn.artist_credit WHERE a.gid = ?",
        )),
        (Kind::Release, "laid") => Sql(In(
            "SELECT rl.release FROM label l JOIN release_label rl ON rl.label = l.id WHERE l.gid = ?",
        )),
        (Kind::Release, "barcode") => Sql(In("SELECT id FROM release WHERE barcode = ?")),
        (Kind::Release, "asin") => Sql(Expr(
            "(SELECT lower(amazon_asin) FROM release_meta WHERE id = e.id) = ?",
        )),
        (Kind::Release, "country") => Sql(Expr(
            "EXISTS (SELECT 1 FROM release_country rc \
            JOIN iso_3166_1 iso ON iso.area = rc.country WHERE rc.release = e.id AND iso.code = upper(?))",
        )),
        (Kind::Release, "date") => Sql(Date(RELEASE_DATE, false)),
        (Kind::Release, "format") => Sql(Named(
            "medium_format",
            "EXISTS (SELECT 1 FROM medium WHERE release = e.id AND format = ?)",
        )),
        (Kind::Release, "mediums") => Sql(Number(
            "(SELECT count(*) FROM medium WHERE release = e.id) {cond}",
        )),
        (Kind::Release, "tracks") => Sql(Number(
            "(SELECT coalesce(sum(track_count), 0) FROM medium WHERE release = e.id) {cond}",
        )),
        (Kind::Release, "tracksmedium") => Sql(Number(
            "EXISTS (SELECT 1 FROM medium WHERE release = e.id AND track_count {cond})",
        )),
        (Kind::Release, "status") => Sql(Named("release_status", "e.status")),
        (Kind::Release, "primarytype" | "type") => Sql(Named(
            "release_group_primary_type",
            "(SELECT type FROM release_group WHERE id = e.release_group) = ?",
        )),
        (Kind::Release, "secondarytype") => Sql(Named(
            "release_group_secondary_type",
            "EXISTS (SELECT 1 FROM release_group_secondary_type_join WHERE release_group = e.release_group \
             AND secondary_type = ?)",
        )),
        (Kind::Release, "packaging") => Sql(Named("release_packaging", "e.packaging")),
        (Kind::Release, "lang") => Sql(Expr(
            "(SELECT iso_code_3 FROM language WHERE id = e.language) = ?",
        )),
        (Kind::Release, "script") => Sql(Expr(
            "(SELECT lower(iso_code) FROM script WHERE id = e.script) = ?",
        )),

        (Kind::ReleaseGroup, "releasegroup" | "releasegroupaccent") => Text(&["name", "alias"]),
        (Kind::ReleaseGroup, "artist" | "artistname" | "creditname") => Text(&["artist"]),
        (Kind::ReleaseGroup, "rgid") => Sql(In("SELECT id FROM release_group WHERE gid = ?")),
        (Kind::ReleaseGroup, "reid") => Sql(In("SELECT release_group FROM release WHERE gid = ?")),
        (Kind::ReleaseGroup, "arid") => Sql(In(
            "SELECT rg.id FROM artist a JOIN artist_credit_name acn \
            ON acn.artist = a.id JOIN release_group rg ON rg.artist_credit = acn.artist_credit WHERE a.gid = ?",
        )),
        (Kind::ReleaseGroup, "release") => Sql(Expr(
            "EXISTS (SELECT 1 FROM release WHERE release_group = e.id AND lower(name) = ?)",
        )),
        (Kind::ReleaseGroup, "primarytype" | "type") => {
            Sql(Named("release_group_primary_type", "e.type"))
        }
        (Kind::ReleaseGroup, "secondarytype") => Sql(Named(
            "release_group_secondary_type",
            "EXISTS (SELECT 1 FROM release_group_secondary_type_join WHERE release_group = e.id AND secondary_type = ?)",
        )),
        (Kind::ReleaseGroup, "status") => Sql(Named(
            "release_status",
            "EXISTS (SELECT 1 FROM release WHERE release_group = e.id AND status = ?)",
        )),
        (Kind::ReleaseGroup, "firstreleasedate") => Sql(Date(
            "EXISTS (SELECT 1 FROM release_group_meta m WHERE m.id = e.id AND {cond})",
            false,
        )),
        (Kind::ReleaseGroup, "releases") => Sql(Number(
            "(SELECT count(*) FROM release WHERE release_group = e.id) {cond}",
        )),

        (Kind::Recording, "recording" | "recordingaccent") => Text(&["name", "alias"]),
        (Kind::Recording, "artist" | "artistname" | "creditname") => Text(&["artist"]),
        (Kind::Recording, "release") => Text(&["release"]),
        (Kind::Recording, "rid") => Sql(In("SELECT id FROM recording WHERE gid = ?")),
        (Kind::Recording, "arid") => {
            Sql(In("SELECT r.id FROM artist a JOIN artist_credit_name acn \
            ON acn.artist = a.id JOIN recording r ON r.artist_credit = acn.artist_credit WHERE a.gid = ?"))
        }
        (Kind::Recording, "reid") => Sql(In(
            "SELECT t.recording FROM release r JOIN medium m ON m.release = r.id \
            JOIN track t ON t.medium = m.id WHERE r.gid = ?",
        )),
        (Kind::Recording, "rgid") => Sql(In("SELECT t.recording FROM release_group rg \
            JOIN release r ON r.release_group = rg.id JOIN medium m ON m.release = r.id \
            JOIN track t ON t.medium = m.id WHERE rg.gid = ?")),
        (Kind::Recording, "tid") => Sql(In("SELECT recording FROM track WHERE gid = ?")),
        (Kind::Recording, "isrc") => Sql(In("SELECT recording FROM isrc WHERE isrc = upper(?)")),
        (Kind::Recording, "dur") => Sql(Number("e.length {cond}")),
        (Kind::Recording, "qdur") => Sql(Number("(e.length / 2000) {cond}")),
        (Kind::Recording, "video") => Sql(Bool("e.video")),
        (Kind::Recording, "firstreleasedate") => Sql(Date(
            "EXISTS (SELECT 1 FROM recording_first_release_date f WHERE f.recording = e.id AND {cond})",
            true,
        )),
        (Kind::Recording, "tnum" | "position") => Sql(Number(
            "EXISTS (SELECT 1 FROM track WHERE recording = e.id AND position {cond})",
        )),
        (Kind::Recording, "number") => Sql(Expr(
            "EXISTS (SELECT 1 FROM track WHERE recording = e.id AND lower(number) = ?)",
        )),
        (Kind::Recording, "tracks" | "tracksrelease") => Sql(Number(
            "EXISTS (SELECT 1 FROM track t JOIN medium m ON m.id = t.medium \
             WHERE t.recording = e.id AND m.track_count {cond})",
        )),
        (Kind::Recording, "status") => Sql(Named(
            "release_status",
            "EXISTS (SELECT 1 FROM track t JOIN medium m ON m.id = t.medium JOIN release r ON r.id = m.release \
             WHERE t.recording = e.id AND r.status = ?)",
        )),
        (Kind::Recording, "format") => Sql(Named(
            "medium_format",
            "EXISTS (SELECT 1 FROM track t JOIN medium m ON m.id = t.medium WHERE t.recording = e.id AND m.format = ?)",
        )),
        (Kind::Recording, "country") => Sql(Expr(
            "EXISTS (SELECT 1 FROM track t JOIN medium m ON m.id = t.medium \
            JOIN release_country rc ON rc.release = m.release JOIN iso_3166_1 iso ON iso.area = rc.country \
            WHERE t.recording = e.id AND iso.code = upper(?))",
        )),
        (Kind::Recording, "primarytype" | "type") => Sql(Named(
            "release_group_primary_type",
            "EXISTS (SELECT 1 FROM track t JOIN medium m ON m.id = t.medium JOIN release r ON r.id = m.release \
             JOIN release_group rg ON rg.id = r.release_group WHERE t.recording = e.id AND rg.type = ?)",
        )),

        (Kind::Work, "work" | "workaccent") => Text(&["name", "alias"]),
        (Kind::Work, "artist") => Text(&["artist"]),
        (Kind::Work, "wid") => Sql(In("SELECT id FROM work WHERE gid = ?")),
        (Kind::Work, "iswc") => Sql(In("SELECT work FROM iswc WHERE iswc = upper(?)")),
        (Kind::Work, "type") => Sql(Named("work_type", "e.type")),
        (Kind::Work, "lang") => Sql(Expr(
            "EXISTS (SELECT 1 FROM work_language wl JOIN language l \
            ON l.id = wl.language WHERE wl.work = e.id AND l.iso_code_3 = ?)",
        )),
        (Kind::Work, "arid") => Sql(In(
            "SELECT rel.entity1 FROM artist a JOIN rel ON rel.kind0 = 2 \
            AND rel.entity0 = a.id AND rel.kind1 = 13 WHERE a.gid = ?",
        )),

        (Kind::Label, "label" | "labelaccent") => Text(&["name", "alias"]),
        (Kind::Label, "laid") => Sql(In("SELECT id FROM label WHERE gid = ?")),
        (Kind::Label, "type") => Sql(Named("label_type", "e.type")),
        (Kind::Label, "code" | "labelcode") => Sql(Number("e.label_code {cond}")),
        (Kind::Label, "country") => Sql(Expr(
            "(SELECT code FROM area_country WHERE area = e.area) = upper(?)",
        )),
        (Kind::Label, "area") => Sql(Expr("(SELECT lower(name) FROM area WHERE id = e.area) = ?")),
        (Kind::Label, "begin") => Sql(Date("({cond})", false)),
        (Kind::Label, "ended") => Sql(Bool("e.ended")),
        (Kind::Label, "ipi") => Sql(In("SELECT label FROM label_ipi WHERE ipi = ?")),
        (Kind::Label, "isni") => Sql(In("SELECT label FROM label_isni WHERE isni = ?")),

        (Kind::Area, "area" | "areaaccent") => Text(&["name", "alias"]),
        (Kind::Area, "aid") => Sql(In("SELECT id FROM area WHERE gid = ?")),
        (Kind::Area, "type") => Sql(Named("area_type", "e.type")),
        (Kind::Area, "iso" | "iso1") => {
            Sql(In("SELECT area FROM iso_3166_1 WHERE code = upper(?)"))
        }
        (Kind::Area, "iso2") => Sql(In(
            "SELECT area FROM iso_3166_2 WHERE upper(code) = upper(?)",
        )),
        (Kind::Area, "iso3") => Sql(In(
            "SELECT area FROM iso_3166_3 WHERE upper(code) = upper(?)",
        )),
        (Kind::Area, "ended") => Sql(Bool("e.ended")),

        (Kind::Place, "place" | "placeaccent") => Text(&["name", "alias"]),
        (Kind::Place, "address") => Text(&["address"]),
        (Kind::Place, "pid") => Sql(In("SELECT id FROM place WHERE gid = ?")),
        (Kind::Place, "type") => Sql(Named("place_type", "e.type")),
        (Kind::Place, "area") => Sql(Expr("(SELECT lower(name) FROM area WHERE id = e.area) = ?")),
        (Kind::Place, "ended") => Sql(Bool("e.ended")),

        (Kind::Event, "event" | "eventaccent") => Text(&["name", "alias"]),
        (Kind::Event, "eid") => Sql(In("SELECT id FROM event WHERE gid = ?")),
        (Kind::Event, "type") => Sql(Named("event_type", "e.type")),
        (Kind::Event, "begin") => Sql(Date("({cond})", false)),
        (Kind::Event, "ended") => Sql(Bool("e.ended")),
        (Kind::Event, "arid") => Sql(In(
            "SELECT rel.entity1 FROM artist a JOIN rel ON rel.kind0 = 2 \
            AND rel.entity0 = a.id AND rel.kind1 = 3 WHERE a.gid = ?",
        )),

        (Kind::Instrument, "instrument" | "instrumentaccent") => Text(&["name", "alias"]),
        (Kind::Instrument, "description") => Text(&["description"]),
        (Kind::Instrument, "iid") => Sql(In("SELECT id FROM instrument WHERE gid = ?")),
        (Kind::Instrument, "type") => Sql(Named("instrument_type", "e.type")),

        (Kind::Series, "series" | "seriesaccent") => Text(&["name", "alias"]),
        (Kind::Series, "sid") => Sql(In("SELECT id FROM series WHERE gid = ?")),
        (Kind::Series, "type") => Sql(Named("series_type", "e.type")),
        _ => return None,
    })
}

/// A condition made concrete: SQL over `e` and its parameters.
#[derive(Debug, Clone)]
struct Sql {
    sql: String,
    params: Vec<String>,
}

impl Sql {
    fn never() -> Self {
        Self {
            sql: "0".into(),
            params: Vec::new(),
        }
    }
}

/// A partial date (`2001`, `2001-05`, `2001-05-03`) as a condition on year/month/day columns, or a range of them.
fn date_cond(node: &Node, packed: bool) -> Option<(String, Vec<String>)> {
    let pack = |d: &str, upper: bool| -> Option<i64> {
        let mut parts = d.split('-').map(|p| p.trim().parse::<i64>());
        let y = parts.next()?.ok()?;
        let m = parts.next().and_then(Result::ok);
        let day = parts.next().and_then(Result::ok);
        let fill = if upper { 99 } else { 0 };
        Some(y * 10000 + m.unwrap_or(fill) * 100 + day.unwrap_or(fill))
    };
    let value = if packed {
        "date".to_owned()
    } else {
        "(coalesce(date_year, 9999) * 10000 + coalesce(date_month, 99) * 100 + coalesce(date_day, 99))".to_owned()
    };
    if let Node::Range { from, to, .. } = node {
        let from = from.as_deref().and_then(|f| pack(f, false)).unwrap_or(0);
        let to = to
            .as_deref()
            .and_then(|t| pack(t, true))
            .unwrap_or(99_999_999);
        Some((format!("{value} BETWEEN {from} AND {to}"), Vec::new()))
    } else {
        let text = node.text();
        let from = pack(&text, false)?;
        let to = pack(&text, true)?;
        Some((format!("{value} BETWEEN {from} AND {to}"), Vec::new()))
    }
}

fn number_cond(node: &Node) -> Option<String> {
    let num = |s: &str| s.trim().parse::<i64>().ok();
    match node {
        Node::Range { from, to, .. } => Some(format!(
            "BETWEEN {} AND {}",
            from.as_deref().and_then(num).unwrap_or(i64::MIN),
            to.as_deref().and_then(num).unwrap_or(i64::MAX)
        )),
        _ => Some(format!("= {}", num(&node.text())?)),
    }
}

fn sql_cond(kind: Kind, lk: &Lookups, cond: Cond, node: &Node) -> Sql {
    let value = node.text().trim().to_lowercase();
    let replace_kind = |s: &str| s.replace("{kind}", &kind.code().to_string());
    match cond {
        Cond::In(sql) => Sql {
            sql: format!("e.id IN ({})", replace_kind(sql)),
            params: vec![value],
        },
        Cond::Expr(sql) => Sql {
            sql: sql.to_owned(),
            params: vec![value],
        },
        Cond::Named(table, expr) => {
            let id = lk
                .all(table)
                .find(|(_, n)| n.name.to_lowercase() == value)
                .map(|(id, _)| id);
            match id {
                Some(id) if expr.contains('?') => Sql {
                    sql: expr.to_owned(),
                    params: vec![id.to_string()],
                },
                Some(id) => Sql {
                    sql: format!("{expr} = {id}"),
                    params: Vec::new(),
                },
                None => Sql::never(),
            }
        }
        Cond::Date(template, packed) => match date_cond(node, packed) {
            Some((cond, params)) => {
                // For the entity's own dates, the columns are begin_date_*.
                let cond = if template == "({cond})" {
                    cond.replace("date_", "e.begin_date_")
                } else {
                    cond
                };
                Sql {
                    sql: template.replace("{cond}", &cond),
                    params,
                }
            }
            None => Sql::never(),
        },
        Cond::Number(template) => match number_cond(node) {
            Some(cond) => Sql {
                sql: template.replace("{cond}", &cond),
                params: Vec::new(),
            },
            None => Sql::never(),
        },
        Cond::Bool(column) => match value.as_str() {
            "true" | "1" | "yes" => Sql {
                sql: format!("{column} = 1"),
                params: Vec::new(),
            },
            "false" | "0" | "no" => Sql {
                sql: format!("coalesce({column}, 0) = 0"),
                params: Vec::new(),
            },
            _ => Sql::never(),
        },
    }
}

/// The words of a text, as the search tables' tokenizer splits them (letters and digits; everything else
/// separates).
fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// An FTS5 expression for a node, in `columns`. `strict`: the words of groups must all match.
fn fts_expr(node: &Node, strict: bool) -> Option<String> {
    match node {
        Node::Term { text, prefix, .. } => {
            let words = words(text);
            match words.as_slice() {
                [] => None,
                [one] => Some(format!("\"{one}\"{}", if *prefix { " *" } else { "" })),
                many => Some(format!("\"{}\"", many.join(" "))),
            }
        }
        Node::Phrase { text, .. } => {
            let words = words(text);
            (!words.is_empty()).then(|| format!("\"{}\"", words.join(" ")))
        }
        Node::Range { .. } => None,
        Node::Group { children, .. } => {
            let mut positive = Vec::new();
            let mut must = Vec::new();
            let mut negative = Vec::new();
            for (occur, child) in children {
                let Some(expr) = fts_expr(child, strict) else {
                    continue;
                };
                match occur {
                    Occur::Must => must.push(expr),
                    Occur::Should => positive.push(expr),
                    Occur::MustNot => negative.push(expr),
                }
            }
            let should = if positive.is_empty() {
                None
            } else if strict {
                Some(positive.join(" AND "))
            } else {
                Some(format!("({})", positive.join(" OR ")))
            };
            let mut all: Vec<String> = must;
            all.extend(should);
            if all.is_empty() {
                return None;
            }
            let mut expr = format!("({})", all.join(" AND "));
            for n in negative {
                expr = format!("({expr} NOT {n})");
            }
            Some(expr)
        }
    }
}

fn in_columns(columns: &[&str], expr: &str) -> String {
    format!("{{{}}} : {expr}", columns.join(" "))
}

/// A query, planned.
#[derive(Debug, Default)]
struct Plan {
    /// FTS expressions: all of the text (`strict`), and the text where any clause may match (`relaxed`, when it is
    /// different).
    strict: Option<String>,
    relaxed: Option<String>,
    /// Conditions that must (not) hold, and those that only rank.
    must: Vec<Sql>,
    should: Vec<Sql>,
    /// A clause that can never match is required.
    impossible: bool,
}

fn plan(kind: Kind, lk: &Lookups, query: &str) -> Plan {
    let Some((_, default_columns)) = table(kind) else {
        return Plan {
            impossible: true,
            ..Plan::default()
        };
    };
    let mut plan = Plan::default();
    let mut strict_parts = Vec::new();
    let mut must_text = Vec::new();
    let mut should_text = Vec::new();
    let mut not_text = Vec::new();

    for (occur, node) in query::parse(query) {
        let field = match node.field() {
            None => Some(Field::Text(default_columns)),
            Some(name) => field(kind, name),
        };
        match field {
            None => {
                // An unknown field matches nothing.
                if occur == Occur::Must {
                    plan.impossible = true;
                }
            }
            Some(Field::Text(columns)) => {
                let (Some(strict), Some(relaxed)) = (fts_expr(&node, true), fts_expr(&node, false))
                else {
                    continue;
                };
                let strict = in_columns(columns, &strict);
                let relaxed = in_columns(columns, &relaxed);
                match occur {
                    Occur::Must => {
                        strict_parts.push(strict.clone());
                        must_text.push(strict);
                    }
                    Occur::Should => {
                        strict_parts.push(strict);
                        should_text.push(relaxed);
                    }
                    Occur::MustNot => not_text.push(strict),
                }
            }
            Some(Field::Sql(cond)) => {
                let sql = sql_cond(kind, lk, cond, &node);
                match occur {
                    Occur::Must => plan.must.push(sql),
                    Occur::Should => plan.should.push(sql),
                    Occur::MustNot => plan.must.push(Sql {
                        sql: format!("NOT ({})", sql.sql),
                        ..sql
                    }),
                }
            }
        }
    }

    let negate = |mut expr: String| {
        for n in &not_text {
            expr = format!("({expr}) NOT ({n})");
        }
        expr
    };
    if !strict_parts.is_empty() {
        plan.strict = Some(negate(strict_parts.join(" AND ")));
        if should_text.len() > 1 || !must_text.is_empty() && !should_text.is_empty() {
            let mut relaxed = must_text.clone();
            relaxed.push(format!("({})", should_text.join(" OR ")));
            plan.relaxed = Some(negate(relaxed.join(" AND ")));
        } else if should_text.len() == 1 {
            // One clause: relaxing means any of its words.
            plan.relaxed = Some(negate(should_text.join("")));
        }
        if plan.relaxed == plan.strict {
            plan.relaxed = None;
        }
    }
    plan
}

/// The SQL for one pass of a text search: (page, count). `?1` is the FTS expression, then the parameters of
/// `must`.
fn text_sql(kind: Kind, plan: &Plan) -> (String, String) {
    let (fts, _) = table(kind).unwrap_or(("", &[]));
    let must: String = plan.must.iter().fold(String::new(), |mut s, c| {
        s.push_str(" AND ");
        s.push_str(&c.sql);
        s
    });
    let join = if must.is_empty() {
        String::new()
    } else {
        format!(" JOIN {} e ON e.id = {fts}.rowid", kind.table())
    };
    (
        format!(
            "SELECT {fts}.rowid, bm25({fts}) FROM {fts}{join} WHERE {fts} MATCH ?{must} \
             ORDER BY bm25({fts}), {fts}.rowid LIMIT ?"
        ),
        format!("SELECT count(*) FROM {fts}{join} WHERE {fts} MATCH ?{must}"),
    )
}

/// The SQL of a search without text: (page, count), with the parameters of `must`, then `should`.
fn fields_sql(kind: Kind, plan: &Plan) -> (String, String) {
    let mut conditions: Vec<String> = plan.must.iter().map(|c| c.sql.clone()).collect();
    if !plan.should.is_empty() {
        conditions.push(format!(
            "({})",
            plan.should
                .iter()
                .map(|c| c.sql.as_str())
                .collect::<Vec<_>>()
                .join(" OR ")
        ));
    }
    let table = kind.table();
    let condition = if conditions.is_empty() {
        "1".into()
    } else {
        conditions.join(" AND ")
    };
    (
        format!("SELECT e.id FROM {table} e WHERE {condition} ORDER BY e.id LIMIT ? OFFSET ?"),
        format!("SELECT count(*) FROM {table} e WHERE {condition}"),
    )
}

fn params<'a>(first: Option<&'a String>, conds: &'a [&'a Sql]) -> Vec<&'a dyn ToSql> {
    let mut out: Vec<&dyn ToSql> = Vec::new();
    if let Some(first) = first {
        out.push(first as &dyn ToSql);
    }
    for c in conds {
        for p in &c.params {
            out.push(p as &dyn ToSql);
        }
    }
    out
}

/// Runs a search: the page of entity ids with their scores, and how many matched.
// Scores are small ratios of small counts.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
fn run(conn: &Connection, kind: Kind, plan: &Plan, page: Page) -> R<(Vec<(i64, i64)>, i64)> {
    if plan.impossible {
        return Ok((Vec::new(), 0));
    }
    let must: Vec<&Sql> = plan.must.iter().collect();
    let Some(strict) = &plan.strict else {
        if plan.should.is_empty() && plan.must.is_empty() {
            return Ok((Vec::new(), 0));
        }
        let (page_sql, count_sql) = fields_sql(kind, plan);
        let mut conds = must.clone();
        conds.extend(plan.should.iter());
        let mut p = params(None, &conds);
        let (limit, offset) = (page.limit, page.offset);
        let count: i64 = conn
            .prepare_cached(&count_sql)?
            .query_row(p.as_slice(), |r| r.get(0))?;
        p.push(&limit);
        p.push(&offset);
        let ids = conn
            .prepare_cached(&page_sql)?
            .query_map(p.as_slice(), |r| Ok((r.get(0)?, 100)))?
            .collect::<R<Vec<_>>>()?;
        return Ok((ids, count));
    };

    let window = (page.offset + page.limit).max(RANK_WINDOW);
    let (page_sql, count_sql) = text_sql(kind, plan);
    let fetch = |expr: &String| -> R<(Vec<(i64, f64)>, i64)> {
        let mut p = params(Some(expr), &must);
        let count: i64 = conn
            .prepare_cached(&count_sql)?
            .query_row(p.as_slice(), |r| r.get(0))?;
        p.push(&window);
        let mut stmt = conn.prepare_cached(&page_sql)?;
        let rows = stmt
            .query_map(p.as_slice(), |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<R<_>>()?;
        Ok((rows, count))
    };
    // (id, strict, bm25)
    let (strict_rows, strict_count) = fetch(strict)?;
    let mut candidates: Vec<(i64, bool, f64)> = strict_rows
        .iter()
        .map(|(id, rank)| (*id, true, *rank))
        .collect();
    let mut count = strict_count;
    if strict_count < page.offset + page.limit
        && let Some(relaxed) = &plan.relaxed
    {
        let seen: HashSet<i64> = candidates.iter().map(|c| c.0).collect();
        let (relaxed_rows, relaxed_count) = fetch(relaxed)?;
        candidates.extend(
            relaxed_rows
                .into_iter()
                .filter(|(id, _)| !seen.contains(id))
                .map(|(id, r)| (id, false, r)),
        );
        count = relaxed_count.max(strict_count);
    }

    // How many of the ranking conditions each candidate meets.
    let mut ranked: Vec<(i64, f64)> = Vec::with_capacity(candidates.len());
    let best = candidates.iter().map(|c| c.2).fold(0.0_f64, f64::min);
    for (id, is_strict, rank) in candidates {
        let mut met = 0usize;
        for cond in &plan.should {
            let sql = format!(
                "SELECT EXISTS (SELECT 1 FROM {} e WHERE e.id = ? AND {})",
                kind.table(),
                cond.sql
            );
            let mut p: Vec<&dyn ToSql> = vec![&id];
            p.extend(cond.params.iter().map(|x| x as &dyn ToSql));
            if conn
                .prepare_cached(&sql)?
                .query_row(p.as_slice(), |r| r.get::<_, bool>(0))?
            {
                met += 1;
            }
        }
        let text = if best < 0.0 { rank / best } else { 1.0 };
        let fields = if plan.should.is_empty() {
            0.0
        } else {
            met as f64 / plan.should.len() as f64
        };
        let score = if is_strict { 2.0 } else { 1.0 } + text + fields;
        ranked.push((id, score));
    }
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    let top = ranked.first().map_or(1.0, |r| r.1);
    let page_rows = ranked
        .into_iter()
        .skip(usize::try_from(page.offset).unwrap_or(0))
        .take(usize::try_from(page.limit).unwrap_or(25))
        .map(|(id, score)| (id, (score / top * 100.0).round() as i64))
        .collect();
    Ok((page_rows, count))
}

/// `/ws/2/<type>?query=`
pub fn search(conn: &Connection, lk: &Lookups, kind: Kind, query: &str, page: Page) -> R<Answer> {
    let plan = plan(kind, lk, query);
    let (rows, count) = run(conn, kind, &plan, page)?;
    let fmt = format::Formatter { conn, lk };
    let mut items = Vec::with_capacity(rows.len());
    for (id, score) in rows {
        let mut item = fmt.entity(kind, id)?;
        if let Value::Object(obj) = &mut item {
            obj.insert("score".into(), json!(score));
        }
        items.push(item);
    }
    Ok(envelope(
        kind.plural(),
        super::list_root(kind),
        items,
        count,
        page.offset,
    ))
}

/// The search envelope: `{"created": ..., "count": n, "offset": n, "<plural>": [...]}`.
fn envelope(
    plural: &str,
    root: &'static str,
    items: Vec<Value>,
    count: i64,
    offset: i64,
) -> Answer {
    let mut out = Map::new();
    out.insert("created".into(), Value::String(now_iso()));
    out.insert("count".into(), json!(count));
    out.insert("offset".into(), json!(offset));
    out.insert(plural.into(), Value::Array(items));
    Answer::Ok(root, Value::Object(out))
}

/// Searches of what this mirror has no index of (annotations, CD stubs, URLs) find nothing; tags are searched by
/// name.
#[allow(clippy::cast_possible_truncation)] // a score of 0-100
pub fn search_other(conn: &Connection, resource: &str, query: &str, page: Page) -> R<Answer> {
    if resource == "tag" {
        let text: Vec<String> = query::parse(query)
            .iter()
            .filter(|(o, n)| *o != Occur::MustNot && matches!(n.field(), None | Some("tag")))
            .filter_map(|(_, n)| fts_expr(n, false))
            .collect();
        if text.is_empty() {
            return Ok(envelope("tags", "tag-list", Vec::new(), 0, page.offset));
        }
        let expr = text.join(" OR ");
        let count: i64 = conn
            .prepare_cached("SELECT count(*) FROM tag_fts WHERE tag_fts MATCH ?1")?
            .query_row([&expr], |r| r.get(0))?;
        let rows: Vec<(String, f64)> = conn
            .prepare_cached(
                "SELECT t.name, bm25(tag_fts) FROM tag_fts JOIN tag t ON t.id = tag_fts.rowid \
                 WHERE tag_fts MATCH ?1 ORDER BY bm25(tag_fts), t.id LIMIT ?2 OFFSET ?3",
            )?
            .query_map((&expr, page.limit, page.offset), |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?
            .collect::<R<_>>()?;
        let best = rows.first().map_or(-1.0, |r| r.1).min(-f64::EPSILON);
        let items = rows
            .into_iter()
            .map(
                |(name, rank)| json!({"name": name, "score": (rank / best * 100.0).round() as i64}),
            )
            .collect();
        return Ok(envelope("tags", "tag-list", items, count, page.offset));
    }
    let (plural, root) = match resource {
        "annotation" => ("annotations", "annotation-list"),
        "cdstub" => ("cdstubs", "cdstub-list"),
        _ => ("urls", "url-list"),
    };
    Ok(envelope(plural, root, Vec::new(), 0, page.offset))
}

/// The current time as the search server writes it: `2026-11-07T18:53:20.305Z`.
fn now_iso() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = i64::try_from(now.as_secs()).unwrap_or(0);
    let (days, rem) = (secs.div_euclid(86400), secs.rem_euclid(86400));
    // Howard Hinnant's civil_from_days
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3600,
        rem / 60 % 60,
        rem % 60,
        now.subsec_millis()
    )
}

/// Every field of every entity type's search with a sample value, for the query plan tests.
#[cfg(test)]
#[must_use]
pub fn sample_queries() -> Vec<(Kind, String)> {
    const FIELDS: &[&str] = &[
        "alias",
        "comment",
        "tag",
        "artist",
        "artistaccent",
        "name",
        "sortname",
        "arid",
        "type",
        "gender",
        "country",
        "area",
        "beginarea",
        "endarea",
        "begin",
        "ended",
        "ipi",
        "isni",
        "release",
        "releaseaccent",
        "artistname",
        "creditname",
        "label",
        "catno",
        "reid",
        "rgid",
        "laid",
        "barcode",
        "asin",
        "date",
        "format",
        "mediums",
        "tracks",
        "tracksmedium",
        "status",
        "primarytype",
        "secondarytype",
        "packaging",
        "lang",
        "script",
        "releasegroup",
        "releases",
        "firstreleasedate",
        "recording",
        "rid",
        "tid",
        "isrc",
        "dur",
        "qdur",
        "video",
        "tnum",
        "position",
        "number",
        "tracksrelease",
        "work",
        "wid",
        "iswc",
        "labelaccent",
        "code",
        "aid",
        "iso",
        "iso1",
        "iso2",
        "iso3",
        "place",
        "address",
        "pid",
        "event",
        "eid",
        "instrument",
        "description",
        "iid",
        "series",
        "sid",
    ];
    let mut out = Vec::new();
    for kind in Kind::ALL {
        if table(kind).is_none() {
            continue;
        }
        out.push((kind, "love".to_owned()));
        for name in FIELDS {
            if field(kind, name).is_some() {
                // with text (the search table finds candidates) and alone (the condition does)
                out.push((kind, format!("love {name}:1")));
                out.push((kind, format!("+love +{name}:1")));
                out.push((kind, format!("{name}:1")));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plans_text_and_fields() {
        let lk = Lookups::default();
        let p = plan(
            Kind::Release,
            &lk,
            "release:(the downward spiral) artist:(nine inch nails) tracks:(14)",
        );
        assert_eq!(
            p.strict.as_deref(),
            Some(
                r#"{name alias} : ("the" AND "downward" AND "spiral") AND {artist} : ("nine" AND "inch" AND "nails")"#
            )
        );
        assert_eq!(
            p.relaxed.as_deref(),
            Some(
                r#"({name alias} : (("the" OR "downward" OR "spiral")) OR {artist} : (("nine" OR "inch" OR "nails")))"#
            )
        );
        assert_eq!(p.should.len(), 1);
        assert!(p.must.is_empty());

        let p = plan(
            Kind::Recording,
            &lk,
            "+arid:B7FFD2AF-418F-4BE2-BDD1-22F8B48613DA",
        );
        assert!(p.strict.is_none());
        assert_eq!(p.must.len(), 1);
        assert_eq!(
            p.must[0].params,
            vec!["b7ffd2af-418f-4be2-bdd1-22f8b48613da".to_owned()]
        );

        assert!(plan(Kind::Artist, &lk, "+nosuchfield:x").impossible);
    }

    #[test]
    fn words_and_expressions() {
        assert_eq!(words("AC/DC: Live!"), vec!["ac", "dc", "live"]);
        let q = query::parse("ac/dc love*");
        assert_eq!(fts_expr(&q[0].1, true).as_deref(), Some("\"ac dc\""));
        assert_eq!(fts_expr(&q[1].1, true).as_deref(), Some("\"love\" *"));
        assert_eq!(fts_expr(&query::parse("\"\"")[0].1, true), None);
    }

    #[test]
    fn dates() {
        let node = query::parse("date:2001-05").remove(0).1;
        assert_eq!(
            date_cond(&node, true).map(|c| c.0).as_deref(),
            Some("date BETWEEN 20010500 AND 20010599")
        );
        assert!(now_iso().ends_with('Z'));
    }
}
