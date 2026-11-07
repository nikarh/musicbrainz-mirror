//! Search: a tantivy index per entity type, made from the store, queried with the Lucene syntax of MusicBrainz's
//! search (parsed by `ws::search::query`).
//!
//! Each entity type has the fields of MusicBrainz's search server: text (analyzed: split on anything that isn't a
//! letter or digit, lowercased, accents folded), exact values (MBIDs, codes, types, statuses, tags: lowercased, as
//! a whole) and numbers (dates packed as `yyyymmdd`, durations, counts). Queries combine clauses as Lucene does:
//! `+` / `AND` must match, `-` / `NOT` must not, the others add to the score (and at least one must match when
//! nothing is required); results are ranked by BM25.

use std::collections::HashMap;
use std::ops::Bound;
use std::path::Path;

use tantivy::collector::{Count, TopDocs};
use tantivy::query::{
    BooleanQuery, Occur as TOccur, PhraseQuery, Query, RangeQuery, RegexQuery, TermQuery,
};
use tantivy::schema::{
    FAST, Field as TField, INDEXED, IndexRecordOption, STRING, Schema, TextFieldIndexing,
    TextOptions, Value as _,
};
use tantivy::tokenizer::{
    AsciiFoldingFilter, LowerCaser, RemoveLongFilter, SimpleTokenizer, TextAnalyzer,
};
use tantivy::{Index, IndexReader, TantivyDocument, Term};

use super::derive::pack;
use super::{Row, Store};
use crate::kind::Kind;
use crate::ws::search::query::{Node, Occur};

const ANALYZER: &str = "mb";

/// How a field is indexed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    /// Analyzed text; with positions (phrases) or without.
    Text(bool),
    /// An exact value, lowercased.
    Exact,
    /// A number (dates packed as yyyymmdd).
    Number,
    /// A partial date given as text (`2001`, `2001-05`): a range of packed dates.
    Date,
}

/// The search fields of an entity type: (index field, kind, the query field names that mean it).
#[allow(clippy::too_many_lines)] // a table
#[must_use]
pub fn fields(kind: Kind) -> Vec<(&'static str, FieldKind, &'static [&'static str])> {
    use FieldKind::{Date, Exact, Number, Text};
    let mut common = vec![
        ("alias", Text(true), &["alias"][..]),
        ("comment", Text(true), &["comment"][..]),
        ("tag", Exact, &["tag"][..]),
    ];
    let own: Vec<(&'static str, FieldKind, &'static [&'static str])> = match kind {
        Kind::Artist => vec![
            ("name", Text(true), &["artist", "artistaccent", "name"]),
            ("sortname", Text(true), &["sortname"]),
            ("arid", Exact, &["arid"]),
            ("type", Exact, &["type"]),
            ("gender", Exact, &["gender"]),
            ("country", Exact, &["country"]),
            ("area", Text(true), &["area"]),
            ("beginarea", Text(true), &["beginarea"]),
            ("endarea", Text(true), &["endarea"]),
            ("begin", Date, &["begin"]),
            ("end", Date, &["end"]),
            ("ended", Exact, &["ended"]),
            ("ipi", Exact, &["ipi"]),
            ("isni", Exact, &["isni"]),
        ],
        Kind::Release => vec![
            ("name", Text(true), &["release", "releaseaccent"]),
            (
                "artist",
                Text(true),
                &["artist", "artistname", "creditname"],
            ),
            ("label", Text(true), &["label"]),
            ("catno", Text(true), &["catno"]),
            ("reid", Exact, &["reid"]),
            ("rgid", Exact, &["rgid"]),
            ("arid", Exact, &["arid"]),
            ("laid", Exact, &["laid"]),
            ("barcode", Exact, &["barcode"]),
            ("asin", Exact, &["asin"]),
            ("country", Exact, &["country"]),
            ("date", Date, &["date"]),
            ("format", Exact, &["format"]),
            ("mediums", Number, &["mediums"]),
            ("tracks", Number, &["tracks"]),
            ("tracksmedium", Number, &["tracksmedium"]),
            ("status", Exact, &["status"]),
            ("primarytype", Exact, &["primarytype", "type"]),
            ("secondarytype", Exact, &["secondarytype"]),
            ("packaging", Exact, &["packaging"]),
            ("lang", Exact, &["lang"]),
            ("script", Exact, &["script"]),
        ],
        Kind::ReleaseGroup => vec![
            ("name", Text(true), &["releasegroup", "releasegroupaccent"]),
            (
                "artist",
                Text(true),
                &["artist", "artistname", "creditname"],
            ),
            ("release", Text(false), &["release"]),
            ("rgid", Exact, &["rgid"]),
            ("reid", Exact, &["reid"]),
            ("arid", Exact, &["arid"]),
            ("primarytype", Exact, &["primarytype", "type"]),
            ("secondarytype", Exact, &["secondarytype"]),
            ("status", Exact, &["status"]),
            ("firstreleasedate", Date, &["firstreleasedate"]),
            ("releases", Number, &["releases"]),
        ],
        Kind::Recording => vec![
            ("name", Text(true), &["recording", "recordingaccent"]),
            (
                "artist",
                Text(true),
                &["artist", "artistname", "creditname"],
            ),
            ("release", Text(false), &["release"]),
            ("rid", Exact, &["rid"]),
            ("arid", Exact, &["arid"]),
            ("reid", Exact, &["reid"]),
            ("rgid", Exact, &["rgid"]),
            ("tid", Exact, &["tid"]),
            ("isrc", Exact, &["isrc"]),
            ("dur", Number, &["dur"]),
            ("qdur", Number, &["qdur"]),
            ("video", Exact, &["video"]),
            ("firstreleasedate", Date, &["firstreleasedate"]),
            ("date", Date, &["date"]),
            ("tnum", Number, &["tnum", "position"]),
            ("number", Exact, &["number"]),
            ("tracks", Number, &["tracks", "tracksrelease"]),
            ("status", Exact, &["status"]),
            ("format", Exact, &["format"]),
            ("country", Exact, &["country"]),
            ("primarytype", Exact, &["primarytype", "type"]),
        ],
        Kind::Work => vec![
            ("name", Text(true), &["work", "workaccent"]),
            ("artist", Text(true), &["artist"]),
            ("wid", Exact, &["wid"]),
            ("iswc", Exact, &["iswc"]),
            ("type", Exact, &["type"]),
            ("lang", Exact, &["lang"]),
            ("arid", Exact, &["arid"]),
        ],
        Kind::Label => vec![
            ("name", Text(true), &["label", "labelaccent"]),
            ("laid", Exact, &["laid"]),
            ("type", Exact, &["type"]),
            ("code", Number, &["code", "labelcode"]),
            ("country", Exact, &["country"]),
            ("area", Text(true), &["area"]),
            ("begin", Date, &["begin"]),
            ("ended", Exact, &["ended"]),
            ("ipi", Exact, &["ipi"]),
            ("isni", Exact, &["isni"]),
        ],
        Kind::Area => vec![
            ("name", Text(true), &["area", "areaaccent"]),
            ("aid", Exact, &["aid"]),
            ("type", Exact, &["type"]),
            ("iso1", Exact, &["iso", "iso1"]),
            ("iso2", Exact, &["iso2"]),
            ("iso3", Exact, &["iso3"]),
            ("ended", Exact, &["ended"]),
        ],
        Kind::Place => vec![
            ("name", Text(true), &["place", "placeaccent"]),
            ("address", Text(true), &["address"]),
            ("pid", Exact, &["pid"]),
            ("type", Exact, &["type"]),
            ("area", Text(true), &["area"]),
            ("ended", Exact, &["ended"]),
        ],
        Kind::Event => vec![
            ("name", Text(true), &["event", "eventaccent"]),
            ("eid", Exact, &["eid"]),
            ("type", Exact, &["type"]),
            ("begin", Date, &["begin"]),
            ("ended", Exact, &["ended"]),
            ("arid", Exact, &["arid"]),
        ],
        Kind::Instrument => vec![
            ("name", Text(true), &["instrument", "instrumentaccent"]),
            ("description", Text(true), &["description"]),
            ("iid", Exact, &["iid"]),
            ("type", Exact, &["type"]),
        ],
        Kind::Series => vec![
            ("name", Text(true), &["series", "seriesaccent"]),
            ("sid", Exact, &["sid"]),
            ("type", Exact, &["type"]),
        ],
        Kind::Genre | Kind::Url => return Vec::new(),
    };
    common.extend(own);
    common
}

/// The fields a term without a field is searched in.
fn default_fields(kind: Kind) -> &'static [&'static str] {
    match kind {
        Kind::Artist => &["name", "sortname", "alias"],
        _ => &["name", "alias"],
    }
}

/// The entity types with a search index (and tags, in their own).
pub const SEARCHABLE: [Kind; 11] = [
    Kind::Area,
    Kind::Artist,
    Kind::Event,
    Kind::Instrument,
    Kind::Label,
    Kind::Place,
    Kind::Recording,
    Kind::Release,
    Kind::ReleaseGroup,
    Kind::Series,
    Kind::Work,
];

fn analyzer() -> TextAnalyzer {
    TextAnalyzer::builder(SimpleTokenizer::default())
        .filter(RemoveLongFilter::limit(100))
        .filter(LowerCaser)
        .filter(AsciiFoldingFilter)
        .build()
}

/// The words of a text, as the text fields have them.
#[must_use]
pub fn words(text: &str) -> Vec<String> {
    let mut analyzer = analyzer();
    let mut stream = analyzer.token_stream(text);
    let mut out = Vec::new();
    while let Some(token) = stream.next() {
        out.push(token.text.clone());
    }
    out
}

fn schema(kind: Kind) -> (Schema, HashMap<&'static str, (TField, FieldKind)>, TField) {
    let mut builder = Schema::builder();
    let id = builder.add_u64_field("id", FAST);
    let mut map = HashMap::new();
    for (name, field_kind, _) in fields(kind) {
        let field = match field_kind {
            FieldKind::Text(positions) => {
                let record = if positions {
                    IndexRecordOption::WithFreqsAndPositions
                } else {
                    IndexRecordOption::WithFreqs
                };
                let indexing = TextFieldIndexing::default()
                    .set_tokenizer(ANALYZER)
                    .set_index_option(record);
                builder.add_text_field(name, TextOptions::default().set_indexing_options(indexing))
            }
            FieldKind::Exact => builder.add_text_field(name, STRING),
            FieldKind::Number | FieldKind::Date => builder.add_u64_field(name, INDEXED),
        };
        map.insert(name, (field, field_kind));
    }
    (builder.build(), map, id)
}

struct KindIndex {
    reader: IndexReader,
    fields: HashMap<&'static str, (TField, FieldKind)>,
}

/// The search indexes of a store.
#[derive(Default)]
pub struct SearchIndex {
    kinds: HashMap<Kind, KindIndex>,
    tags: Option<(IndexReader, TField)>,
}

fn open_index(dir: &Path) -> tantivy::Result<Index> {
    let index = Index::open_in_dir(dir)?;
    index.tokenizers().register(ANALYZER, analyzer());
    Ok(index)
}

impl SearchIndex {
    pub fn open(dir: &Path) -> tantivy::Result<Self> {
        let mut kinds = HashMap::new();
        for kind in SEARCHABLE {
            let index = open_index(&dir.join(kind.table()))?;
            let (_, fields, _) = schema(kind);
            kinds.insert(
                kind,
                KindIndex {
                    reader: index.reader()?,
                    fields,
                },
            );
        }
        let tags = open_index(&dir.join("tag"))?;
        let name = tags.schema().get_field("name")?;
        Ok(Self {
            kinds,
            tags: Some((tags.reader()?, name)),
        })
    }

    /// Searches an entity type: a page of (id, score), and how many matched.
    pub fn search(
        &self,
        kind: Kind,
        query: &[(Occur, Node)],
        offset: usize,
        limit: usize,
    ) -> tantivy::Result<(Vec<(u32, f32)>, usize)> {
        let Some(index) = self.kinds.get(&kind) else {
            return Ok((Vec::new(), 0));
        };
        let Some(query) = build_query(kind, &index.fields, query) else {
            return Ok((Vec::new(), 0));
        };
        let searcher = index.reader.searcher();
        let (top, count) = searcher.search(
            &query,
            &(
                TopDocs::with_limit(limit.max(1))
                    .and_offset(offset)
                    .order_by_score(),
                Count,
            ),
        )?;
        let mut out = Vec::with_capacity(top.len());
        for (score, address) in top {
            let segment = searcher.segment_reader(address.segment_ord);
            let ids = segment.fast_fields().u64("id")?;
            if let Some(id) = ids.first(address.doc_id) {
                out.push((u32::try_from(id).unwrap_or(u32::MAX), score));
            }
        }
        Ok((out, count))
    }

    /// The best score of a query (the first result's), to scale scores to 0-100.
    pub fn top_score(&self, kind: Kind, query: &[(Occur, Node)]) -> tantivy::Result<f32> {
        Ok(self
            .search(kind, query, 0, 1)?
            .0
            .first()
            .map_or(1.0, |r| r.1))
    }

    /// Searches tag names: (name, score) and how many matched.
    pub fn tags(
        &self,
        query: &[(Occur, Node)],
        offset: usize,
        limit: usize,
    ) -> tantivy::Result<(Vec<(String, f32)>, usize)> {
        let Some((reader, name)) = &self.tags else {
            return Ok((Vec::new(), 0));
        };
        let fields = HashMap::from([("name", (*name, FieldKind::Text(true)))]);
        let clauses: Vec<(TOccur, Box<dyn Query>)> = query
            .iter()
            .filter(|(_, n)| matches!(n.field(), None | Some("tag")))
            .filter_map(|(o, n)| node_query(&fields, &["name"], n, true).map(|q| (occur(*o), q)))
            .collect();
        if clauses.is_empty() {
            return Ok((Vec::new(), 0));
        }
        let query = BooleanQuery::new(clauses);
        let searcher = reader.searcher();
        let (top, count) = searcher.search(
            &query,
            &(
                TopDocs::with_limit(limit.max(1))
                    .and_offset(offset)
                    .order_by_score(),
                Count,
            ),
        )?;
        let mut out = Vec::new();
        for (score, address) in top {
            let doc: TantivyDocument = searcher.doc(address)?;
            if let Some(n) = doc.get_first(*name).and_then(|v| v.as_str()) {
                out.push((n.to_owned(), score));
            }
        }
        Ok((out, count))
    }
}

fn occur(o: Occur) -> TOccur {
    match o {
        Occur::Should => TOccur::Should,
        Occur::Must => TOccur::Must,
        Occur::MustNot => TOccur::MustNot,
    }
}

/// The query of a parsed query; `None` when nothing can match.
fn build_query(
    kind: Kind,
    fields: &HashMap<&'static str, (TField, FieldKind)>,
    query: &[(Occur, Node)],
) -> Option<Box<dyn Query>> {
    let mut clauses = Vec::new();
    for (o, node) in query {
        match field_query(kind, fields, node) {
            Some(q) => clauses.push((occur(*o), q)),
            // An unknown field (or nothing searchable) matches nothing.
            None if *o == Occur::Must => return None,
            None => {}
        }
    }
    if clauses.iter().all(|(o, _)| *o == TOccur::MustNot) {
        return None;
    }
    Some(Box::new(BooleanQuery::new(clauses)))
}

fn field_query(
    kind: Kind,
    fields: &HashMap<&'static str, (TField, FieldKind)>,
    node: &Node,
) -> Option<Box<dyn Query>> {
    let names: Vec<&'static str> = match node.field() {
        None => default_fields(kind).to_vec(),
        Some(name) => {
            let (index_field, _, _) = self::fields(kind)
                .into_iter()
                .find(|(_, _, names)| names.contains(&name))?;
            vec![index_field]
        }
    };
    node_query(fields, &names, node, false)
}

/// A node in one or more fields (more: any of them).
fn node_query(
    fields: &HashMap<&'static str, (TField, FieldKind)>,
    names: &[&str],
    node: &Node,
    _tags: bool,
) -> Option<Box<dyn Query>> {
    let per_field: Vec<Box<dyn Query>> = names
        .iter()
        .filter_map(|n| fields.get(n))
        .filter_map(|(f, k)| one_field(*f, *k, node))
        .collect();
    match per_field.len() {
        0 => None,
        1 => per_field.into_iter().next(),
        _ => Some(Box::new(BooleanQuery::new(
            per_field.into_iter().map(|q| (TOccur::Should, q)).collect(),
        ))),
    }
}

/// A node in one field.
fn one_field(field: TField, kind: FieldKind, node: &Node) -> Option<Box<dyn Query>> {
    match (node, kind) {
        (Node::Group { children, .. }, _) => {
            let clauses: Vec<(TOccur, Box<dyn Query>)> = children
                .iter()
                .filter_map(|(o, n)| one_field(field, kind, n).map(|q| (occur(*o), q)))
                .collect();
            if clauses.is_empty() {
                return None;
            }
            Some(Box::new(BooleanQuery::new(clauses)))
        }
        (Node::Term { text, prefix, .. }, FieldKind::Text(positions)) => {
            let words = words(text);
            match words.as_slice() {
                [] => None,
                [one] if *prefix => {
                    // Words are letters and digits only: nothing to escape.
                    RegexQuery::from_pattern(&format!("{one}.*"), field)
                        .ok()
                        .map(|q| Box::new(q) as Box<dyn Query>)
                }
                [one] => Some(term(field, one)),
                many => Some(phrase(field, many, positions)),
            }
        }
        (Node::Phrase { text, .. }, FieldKind::Text(positions)) => {
            let words = words(text);
            match words.as_slice() {
                [] => None,
                [one] => Some(term(field, one)),
                many => Some(phrase(field, many, positions)),
            }
        }
        (Node::Term { text, .. } | Node::Phrase { text, .. }, FieldKind::Exact) => {
            let value = text.trim().to_lowercase();
            (!value.is_empty()).then(|| {
                Box::new(TermQuery::new(
                    Term::from_field_text(field, &value),
                    IndexRecordOption::Basic,
                )) as Box<dyn Query>
            })
        }
        (Node::Term { text, .. } | Node::Phrase { text, .. }, FieldKind::Number) => {
            let n: u64 = text.trim().parse().ok()?;
            Some(Box::new(TermQuery::new(
                Term::from_field_u64(field, n),
                IndexRecordOption::Basic,
            )))
        }
        (Node::Term { text, .. } | Node::Phrase { text, .. }, FieldKind::Date) => {
            let (from, to) = date_range(text)?;
            Some(range(field, Some(from), Some(to)))
        }
        (Node::Range { from, to, .. }, FieldKind::Number) => {
            let from = from.as_deref().and_then(|f| f.trim().parse().ok());
            let to = to.as_deref().and_then(|t| t.trim().parse().ok());
            Some(range(field, from, to))
        }
        (Node::Range { from, to, .. }, FieldKind::Date) => {
            let from = from.as_deref().and_then(date_range).map(|r| r.0);
            let to = to.as_deref().and_then(date_range).map(|r| r.1);
            Some(range(field, from, to))
        }
        (Node::Range { .. }, _) => None,
    }
}

fn term(field: TField, word: &str) -> Box<dyn Query> {
    Box::new(TermQuery::new(
        Term::from_field_text(field, word),
        IndexRecordOption::WithFreqs,
    ))
}

/// Words that must appear together: a phrase where positions are indexed, all of them elsewhere.
fn phrase(field: TField, words: &[String], positions: bool) -> Box<dyn Query> {
    if positions {
        return Box::new(PhraseQuery::new(
            words
                .iter()
                .map(|w| Term::from_field_text(field, w))
                .collect(),
        ));
    }
    Box::new(BooleanQuery::new(
        words
            .iter()
            .map(|w| (TOccur::Must, term(field, w)))
            .collect(),
    ))
}

fn range(field: TField, from: Option<u64>, to: Option<u64>) -> Box<dyn Query> {
    let bound = |v: Option<u64>| {
        v.map_or(Bound::Unbounded, |v| {
            Bound::Included(Term::from_field_u64(field, v))
        })
    };
    Box::new(RangeQuery::new(bound(from), bound(to)))
}

/// The packed dates a partial date covers: `2001` is 2001-01-01 to 2001-12-31 (and 2001 without month or day).
fn date_range(text: &str) -> Option<(u64, u64)> {
    let mut parts = text.trim().split('-').map(|p| p.trim().parse::<u64>());
    let y = parts.next()?.ok()?;
    let m = parts.next().and_then(Result::ok);
    let d = parts.next().and_then(Result::ok);
    let lo = y * 10000 + m.unwrap_or(0) * 100 + d.unwrap_or(0);
    let hi = y * 10000 + m.unwrap_or(99) * 100 + d.unwrap_or(99);
    Some((lo, hi))
}

// Building

/// Values of a document's fields.
#[derive(Default)]
struct Doc {
    text: Vec<(&'static str, String)>,
    exact: Vec<(&'static str, String)>,
    numbers: Vec<(&'static str, u64)>,
}

impl Doc {
    fn text(&mut self, field: &'static str, value: Option<&str>) {
        if let Some(v) = value.filter(|v| !v.is_empty()) {
            self.text.push((field, v.to_owned()));
        }
    }

    fn exact(&mut self, field: &'static str, value: Option<&str>) {
        if let Some(v) = value.filter(|v| !v.is_empty()) {
            self.exact.push((field, v.to_lowercase()));
        }
    }

    fn number(&mut self, field: &'static str, value: Option<u32>) {
        if let Some(v) = value {
            self.numbers.push((field, u64::from(v)));
        }
    }

    fn date(&mut self, field: &'static str, date: (Option<i64>, Option<i64>, Option<i64>)) {
        if date.0.is_some() {
            self.numbers.push((field, u64::from(pack(date))));
        }
    }
}

/// The names of an artist credit's artists, credited and real.
fn credit_names(
    store: &Store,
    ac: Option<u32>,
    doc: &mut Doc,
    field: &'static str,
    arid: Option<&'static str>,
) {
    for name in store
        .t("artist_credit_name")
        .by("artist_credit", ac.unwrap_or(u32::MAX))
    {
        doc.text(field, name.str("name"));
        if let Some(artist) = name.u32("artist").and_then(|a| store.t("artist").get(a)) {
            if artist.str("name") != name.str("name") {
                doc.text(field, artist.str("name"));
            }
            if let Some(arid) = arid {
                doc.exact(arid, Some(&artist.gid()));
            }
        }
    }
}

fn common(store: &Store, kind: Kind, row: &Row<'_>, doc: &mut Doc) {
    let t = kind.table();
    for alias in store.t(&format!("{t}_alias")).by("entity", row.id) {
        doc.text("alias", alias.str("name"));
        if alias.str("sort_name") != alias.str("name") {
            doc.text("alias", alias.str("sort_name"));
        }
    }
    doc.text("comment", row.str("comment"));
    if kind.has_tags() {
        for tag in store.t(&format!("{t}_tag")).by("entity", row.id) {
            if tag.u32("count").is_some_and(|c| c > 0 && c < u32::MAX) {
                doc.exact(
                    "tag",
                    tag.u32("tag")
                        .and_then(|t| store.t("tag").get(t))
                        .and_then(|t| t.str("name")),
                );
            }
        }
    }
}

fn type_name<'a>(store: &'a Store, table: &str, id: Option<u32>) -> Option<&'a str> {
    store.named(table, id).map(|n| n.0)
}

fn area_name(store: &Store, area: Option<u32>) -> Option<&str> {
    store.t("area").get(area?)?.str("name")
}

fn country_codes(store: &Store, area: Option<u32>) -> Vec<String> {
    let Some(area) = area else { return Vec::new() };
    store
        .t("iso_3166_1")
        .by("area", area)
        .filter_map(|r| r.str("code").map(str::to_owned))
        .collect()
}

/// A document of an entity.
#[allow(clippy::too_many_lines)]
fn document(store: &Store, kind: Kind, row: &Row<'_>) -> Doc {
    let mut doc = Doc::default();
    common(store, kind, row, &mut doc);
    let gid = row.gid();
    match kind {
        Kind::Artist => {
            doc.text("name", row.str("name"));
            doc.text("sortname", row.str("sort_name"));
            doc.exact("arid", Some(&gid));
            doc.exact("type", type_name(store, "artist_type", row.u32("type")));
            doc.exact("gender", type_name(store, "gender", row.u32("gender")));
            let country = row.u32("area").and_then(|a| store.derived.area_country(a));
            for code in country_codes(store, country) {
                doc.exact("country", Some(&code));
            }
            doc.text("area", area_name(store, row.u32("area")));
            doc.text("beginarea", area_name(store, row.u32("begin_area")));
            doc.text("endarea", area_name(store, row.u32("end_area")));
            doc.date("begin", row.date("begin"));
            doc.date("end", row.date("end"));
            doc.exact(
                "ended",
                Some(if row.bool("ended") { "true" } else { "false" }),
            );
            for ipi in store.t("artist_ipi").by("artist", row.id) {
                doc.exact("ipi", ipi.str("ipi"));
            }
            for isni in store.t("artist_isni").by("artist", row.id) {
                doc.exact("isni", isni.str("isni"));
            }
        }
        Kind::Release => {
            doc.text("name", row.str("name"));
            credit_names(
                store,
                row.u32("artist_credit"),
                &mut doc,
                "artist",
                Some("arid"),
            );
            for rl in store.t("release_label").by("release", row.id) {
                if let Some(label) = rl.u32("label").and_then(|l| store.t("label").get(l)) {
                    doc.text("label", label.str("name"));
                    doc.exact("laid", Some(&label.gid()));
                }
                doc.text("catno", rl.str("catalog_number"));
            }
            doc.exact("reid", Some(&gid));
            if let Some(rg) = row
                .u32("release_group")
                .and_then(|g| store.t("release_group").get(g))
            {
                doc.exact("rgid", Some(&rg.gid()));
                doc.exact(
                    "primarytype",
                    type_name(store, "release_group_primary_type", rg.u32("type")),
                );
                for st in store
                    .t("release_group_secondary_type_join")
                    .by("release_group", rg.id)
                {
                    doc.exact(
                        "secondarytype",
                        type_name(
                            store,
                            "release_group_secondary_type",
                            st.u32("secondary_type"),
                        ),
                    );
                }
            }
            doc.exact("barcode", row.str("barcode"));
            doc.exact(
                "asin",
                store
                    .t("release_meta")
                    .get(row.id)
                    .and_then(|m| m.str("amazon_asin")),
            );
            for event in store.t("release_country").by("release", row.id) {
                for code in country_codes(store, event.u32("country")) {
                    doc.exact("country", Some(&code));
                }
                doc.date("date", event.date("date"));
            }
            for event in store.t("release_unknown_country").by("release", row.id) {
                doc.date("date", event.date("date"));
            }
            let mut mediums = 0;
            let mut tracks = 0;
            for m in store.t("medium").by("release", row.id) {
                mediums += 1;
                let count = m.u32("track_count").unwrap_or(0);
                tracks += count;
                doc.number("tracksmedium", Some(count));
                doc.exact("format", type_name(store, "medium_format", m.u32("format")));
            }
            doc.number("mediums", Some(mediums));
            doc.number("tracks", Some(tracks));
            doc.exact(
                "status",
                type_name(store, "release_status", row.u32("status")),
            );
            doc.exact(
                "packaging",
                type_name(store, "release_packaging", row.u32("packaging")),
            );
            doc.exact("lang", store.language(row.u32("language")));
            doc.exact("script", store.script(row.u32("script")));
        }
        Kind::ReleaseGroup => {
            doc.text("name", row.str("name"));
            credit_names(
                store,
                row.u32("artist_credit"),
                &mut doc,
                "artist",
                Some("arid"),
            );
            doc.exact("rgid", Some(&gid));
            doc.exact(
                "primarytype",
                type_name(store, "release_group_primary_type", row.u32("type")),
            );
            for st in store
                .t("release_group_secondary_type_join")
                .by("release_group", row.id)
            {
                doc.exact(
                    "secondarytype",
                    type_name(
                        store,
                        "release_group_secondary_type",
                        st.u32("secondary_type"),
                    ),
                );
            }
            let mut releases = 0;
            for r in store.t("release").by("release_group", row.id) {
                releases += 1;
                doc.text("release", r.str("name"));
                doc.exact("reid", Some(&r.gid()));
                doc.exact(
                    "status",
                    type_name(store, "release_status", r.u32("status")),
                );
            }
            doc.number("releases", Some(releases));
            if let Some(meta) = store.t("release_group_meta").get(row.id) {
                doc.date("firstreleasedate", meta.date("first_release_date"));
            }
        }
        Kind::Recording => {
            doc.text("name", row.str("name"));
            credit_names(
                store,
                row.u32("artist_credit"),
                &mut doc,
                "artist",
                Some("arid"),
            );
            doc.exact("rid", Some(&gid));
            doc.number("dur", row.u32("length"));
            doc.number("qdur", row.u32("length").map(|l| l / 2000));
            doc.exact(
                "video",
                Some(if row.bool("video") { "true" } else { "false" }),
            );
            if let Some(first) = store.derived.recording_first_date(row.id)
                && first / 10000 != 9999
            {
                doc.numbers.push(("firstreleasedate", u64::from(first)));
            }
            for isrc in store.t("isrc").by("recording", row.id) {
                doc.exact("isrc", isrc.str("isrc"));
            }
            let mut seen_releases = Vec::new();
            for track in store.t("track").by("recording", row.id) {
                doc.exact("tid", Some(&track.gid()));
                doc.number("tnum", track.u32("position"));
                doc.exact("number", track.str("number"));
                let Some(medium) = track.u32("medium").and_then(|m| store.t("medium").get(m))
                else {
                    continue;
                };
                doc.number("tracks", medium.u32("track_count"));
                doc.exact(
                    "format",
                    type_name(store, "medium_format", medium.u32("format")),
                );
                let Some(release) = medium
                    .u32("release")
                    .and_then(|r| store.t("release").get(r))
                else {
                    continue;
                };
                if seen_releases.contains(&release.id) {
                    continue;
                }
                seen_releases.push(release.id);
                doc.text("release", release.str("name"));
                doc.exact("reid", Some(&release.gid()));
                doc.exact(
                    "status",
                    type_name(store, "release_status", release.u32("status")),
                );
                if let Some(rg) = release
                    .u32("release_group")
                    .and_then(|g| store.t("release_group").get(g))
                {
                    doc.exact("rgid", Some(&rg.gid()));
                    doc.exact(
                        "primarytype",
                        type_name(store, "release_group_primary_type", rg.u32("type")),
                    );
                }
                for event in store.t("release_country").by("release", release.id) {
                    for code in country_codes(store, event.u32("country")) {
                        doc.exact("country", Some(&code));
                    }
                    doc.date("date", event.date("date"));
                }
            }
        }
        Kind::Work => {
            doc.text("name", row.str("name"));
            doc.exact("wid", Some(&gid));
            doc.exact("type", type_name(store, "work_type", row.u32("type")));
            for iswc in store.t("iswc").by("work", row.id) {
                doc.exact("iswc", iswc.str("iswc"));
            }
            for lang in store.t("work_language").by("work", row.id) {
                doc.exact("lang", store.language(lang.u32("language")));
            }
            for rel in store.t("l_artist_work").by("entity1", row.id) {
                if let Some(artist) = rel.u32("entity0").and_then(|a| store.t("artist").get(a)) {
                    doc.text("artist", artist.str("name"));
                    doc.exact("arid", Some(&artist.gid()));
                }
            }
        }
        Kind::Label => {
            doc.text("name", row.str("name"));
            doc.exact("laid", Some(&gid));
            doc.exact("type", type_name(store, "label_type", row.u32("type")));
            doc.number("code", row.u32("label_code"));
            for code in country_codes(store, row.u32("area")) {
                doc.exact("country", Some(&code));
            }
            doc.text("area", area_name(store, row.u32("area")));
            doc.date("begin", row.date("begin"));
            doc.exact(
                "ended",
                Some(if row.bool("ended") { "true" } else { "false" }),
            );
            for ipi in store.t("label_ipi").by("label", row.id) {
                doc.exact("ipi", ipi.str("ipi"));
            }
            for isni in store.t("label_isni").by("label", row.id) {
                doc.exact("isni", isni.str("isni"));
            }
        }
        Kind::Area => {
            doc.text("name", row.str("name"));
            doc.exact("aid", Some(&gid));
            doc.exact("type", type_name(store, "area_type", row.u32("type")));
            for (field, table) in [
                ("iso1", "iso_3166_1"),
                ("iso2", "iso_3166_2"),
                ("iso3", "iso_3166_3"),
            ] {
                for code in store.t(table).by("area", row.id) {
                    doc.exact(field, code.str("code"));
                }
            }
            doc.exact(
                "ended",
                Some(if row.bool("ended") { "true" } else { "false" }),
            );
        }
        Kind::Place => {
            doc.text("name", row.str("name"));
            doc.text("address", row.str("address"));
            doc.exact("pid", Some(&gid));
            doc.exact("type", type_name(store, "place_type", row.u32("type")));
            doc.text("area", area_name(store, row.u32("area")));
            doc.exact(
                "ended",
                Some(if row.bool("ended") { "true" } else { "false" }),
            );
        }
        Kind::Event => {
            doc.text("name", row.str("name"));
            doc.exact("eid", Some(&gid));
            doc.exact("type", type_name(store, "event_type", row.u32("type")));
            doc.date("begin", row.date("begin"));
            doc.exact(
                "ended",
                Some(if row.bool("ended") { "true" } else { "false" }),
            );
            for rel in store.t("l_artist_event").by("entity1", row.id) {
                if let Some(artist) = rel.u32("entity0").and_then(|a| store.t("artist").get(a)) {
                    doc.exact("arid", Some(&artist.gid()));
                }
            }
        }
        Kind::Instrument => {
            doc.text("name", row.str("name"));
            doc.text("description", row.str("description"));
            doc.exact("iid", Some(&gid));
            doc.exact("type", type_name(store, "instrument_type", row.u32("type")));
        }
        Kind::Series => {
            doc.text("name", row.str("name"));
            doc.exact("sid", Some(&gid));
            doc.exact("type", type_name(store, "series_type", row.u32("type")));
        }
        Kind::Genre | Kind::Url => {}
    }
    doc
}

/// An entity's search fields and their values, as a query would give them (`field:value`, the query field being
/// the first name of the index field; dates as `yyyy[-mm[-dd]]`). For the fixture tool and the tests that check
/// that every field is indexed.
#[must_use]
pub fn field_values(store: &Store, kind: Kind, row: &Row<'_>) -> Vec<(&'static str, String)> {
    let doc = document(store, kind, row);
    let spec = fields(kind);
    let query_name = |name: &str| {
        spec.iter()
            .find(|f| f.0 == name)
            .map(|f| (f.2.first().copied(), f.1))
    };
    let mut out = Vec::new();
    for (name, value) in doc.text.iter().chain(&doc.exact) {
        out.push((
            query_name(name).and_then(|q| q.0).unwrap_or(name),
            value.clone(),
        ));
    }
    for (name, value) in &doc.numbers {
        let Some((q, field_kind)) = query_name(name) else {
            out.push((*name, value.to_string()));
            continue;
        };
        let text = if field_kind == FieldKind::Date {
            let (y, m, d) = (value / 10000, value / 100 % 100, value % 100);
            match (m, d) {
                (99, _) => format!("{y:04}"),
                (_, 99) => format!("{y:04}-{m:02}"),
                _ => format!("{y:04}-{m:02}-{d:02}"),
            }
        } else {
            value.to_string()
        };
        out.push((q.unwrap_or(name), text));
    }
    out
}

/// Builds the search indexes into `dir`, with `threads` indexing threads and `heap_mb` of memory for them.
pub fn build(store: &Store, dir: &Path, threads: usize, heap_mb: usize) -> tantivy::Result<()> {
    for kind in SEARCHABLE {
        let started = std::time::Instant::now();
        let path = dir.join(kind.table());
        std::fs::create_dir_all(&path)?;
        let (schema, fields, id) = schema(kind);
        let index = Index::create_in_dir(&path, schema)?;
        index.tokenizers().register(ANALYZER, analyzer());
        let threads = threads.max(1);
        let mut writer = index.writer_with_num_threads::<TantivyDocument>(
            threads,
            (heap_mb << 20).max(threads * (15 << 20)),
        )?;
        let mut count = 0u64;
        for row in store.t(kind.table()).iter() {
            let values = document(store, kind, &row);
            let mut doc = TantivyDocument::default();
            doc.add_u64(id, u64::from(row.id));
            for (name, value) in &values.text {
                if let Some((f, _)) = fields.get(name) {
                    doc.add_text(*f, value);
                }
            }
            for (name, value) in &values.exact {
                if let Some((f, _)) = fields.get(name) {
                    doc.add_text(*f, value);
                }
            }
            for (name, value) in &values.numbers {
                if let Some((f, _)) = fields.get(name) {
                    doc.add_u64(*f, *value);
                }
            }
            writer.add_document(doc)?;
            count += 1;
        }
        writer.commit()?;
        writer.wait_merging_threads()?;
        tracing::info!(
            kind = kind.name(),
            documents = count,
            elapsed_s = started.elapsed().as_secs(),
            "search index built"
        );
    }
    // tags
    let path = dir.join("tag");
    std::fs::create_dir_all(&path)?;
    let mut builder = Schema::builder();
    let indexing = TextFieldIndexing::default()
        .set_tokenizer(ANALYZER)
        .set_index_option(IndexRecordOption::WithFreqsAndPositions);
    let name = builder.add_text_field(
        "name",
        TextOptions::default()
            .set_indexing_options(indexing)
            .set_stored(),
    );
    let index = Index::create_in_dir(&path, builder.build())?;
    index.tokenizers().register(ANALYZER, analyzer());
    let mut writer = index.writer_with_num_threads::<TantivyDocument>(1, 50 << 20)?;
    for tag in store.t("tag").iter() {
        if let Some(n) = tag.str("name") {
            let mut doc = TantivyDocument::default();
            doc.add_text(name, n);
            writer.add_document(doc)?;
        }
    }
    writer.commit()?;
    writer.wait_merging_threads()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn analysis_and_dates() {
        assert_eq!(words("AC/DC: Live! Café"), vec!["ac", "dc", "live", "cafe"]);
        assert_eq!(date_range("2001-05"), Some((2001_0500, 2001_0599)));
        assert_eq!(date_range("2001"), Some((2001_0000, 2001_9999)));
        assert_eq!(date_range("x"), None);
    }
}
