//! Search results, shaped as MusicBrainz's search server returns them. They differ from lookups: empty values are
//! left out, there is an `artist-credit-id`, aliases have `begin-date` / `end-date`, releases have `count` (of
//! media) and `track-count`, and lists of related entities are shorter.

// Rows are read into tuples, as wide as the columns they read.
#![allow(clippy::type_complexity)]

use rusqlite::{Connection, OptionalExtension as _};
use serde_json::{Map, Value, json};

use crate::db::{Lookups, date, queries as q};
use crate::kind::Kind;

type R<T> = rusqlite::Result<T>;
type Obj = Map<String, Value>;

/// Releases listed in a recording result, at most.
const RECORDING_RELEASES: usize = 25;

pub struct Formatter<'a> {
    pub conn: &'a Connection,
    pub lk: &'a Lookups,
}

/// Inserts a string unless it is empty.
fn put(obj: &mut Obj, key: &str, value: Option<String>) {
    if let Some(v) = value.filter(|v| !v.is_empty()) {
        obj.insert(key.into(), Value::String(v));
    }
}

fn put_list(obj: &mut Obj, key: &str, list: Vec<Value>) {
    if !list.is_empty() {
        obj.insert(key.into(), Value::Array(list));
    }
}

impl Formatter<'_> {
    fn strings(&self, sql: &str, id: i64) -> R<Vec<String>> {
        self.conn
            .prepare_cached(sql)?
            .query_map([id], |r| r.get(0))?
            .collect()
    }

    fn named(&self, obj: &mut Obj, key: &str, table: &str, id: Option<i64>) {
        if let Some(n) = self.lk.named(table, id) {
            obj.insert(key.into(), Value::String(n.name.clone()));
            if let Some(gid) = &n.gid {
                obj.insert(format!("{key}-id"), Value::String(gid.clone()));
            }
        }
    }

    pub fn entity(&self, kind: Kind, id: i64) -> R<Value> {
        Ok(Value::Object(match kind {
            Kind::Artist => self.artist(id)?,
            Kind::Release => self.release(id, true)?,
            Kind::ReleaseGroup => self.release_group(id)?,
            Kind::Recording => self.recording(id)?,
            Kind::Work => self.work(id)?,
            Kind::Label => self.label(id)?,
            Kind::Area => self.area(id, true)?,
            Kind::Place => self.place(id)?,
            Kind::Event => self.event(id)?,
            Kind::Instrument => {
                self.simple(id, Kind::Instrument, q::INSTRUMENT, "instrument_type")?
            }
            Kind::Series => self.simple(id, Kind::Series, q::SERIES, "series_type")?,
            Kind::Genre | Kind::Url => Obj::new(),
        }))
    }

    /// Life span: begin and end when known, `ended` true or null.
    fn life_span(begin: String, end: String, ended: bool) -> Value {
        let mut span = Obj::new();
        put(&mut span, "begin", Some(begin));
        put(&mut span, "end", Some(end));
        span.insert(
            "ended".into(),
            if ended {
                Value::Bool(true)
            } else {
                Value::Null
            },
        );
        Value::Object(span)
    }

    fn aliases(&self, kind: Kind, id: i64) -> R<Vec<Value>> {
        let mut stmt = self.conn.prepare_cached(q::ALIASES)?;
        let rows = stmt.query_map((kind.code(), id), |r| {
            let locale: Option<String> = r.get(2)?;
            let mut a = Obj::new();
            a.insert("name".into(), Value::String(r.get(0)?));
            a.insert("sort-name".into(), Value::String(r.get(1)?));
            let type_ = self.lk.alias_type(kind, r.get(3)?);
            a.insert(
                "type".into(),
                type_.map_or(Value::Null, |t| Value::String(t.name.clone())),
            );
            if let Some(gid) = type_.and_then(|t| t.gid.clone()) {
                a.insert("type-id".into(), Value::String(gid));
            }
            a.insert(
                "primary".into(),
                if r.get::<_, Option<bool>>(4)?.unwrap_or(false) {
                    Value::Bool(true)
                } else {
                    Value::Null
                },
            );
            a.insert("locale".into(), locale.map_or(Value::Null, Value::String));
            let begin = date::format(r.get(5)?, r.get(6)?, r.get(7)?);
            let end = date::format(r.get(8)?, r.get(9)?, r.get(10)?);
            a.insert(
                "begin-date".into(),
                if begin.is_empty() {
                    Value::Null
                } else {
                    Value::String(begin)
                },
            );
            a.insert(
                "end-date".into(),
                if end.is_empty() {
                    Value::Null
                } else {
                    Value::String(end)
                },
            );
            Ok(Value::Object(a))
        })?;
        rows.collect()
    }

    fn tags(&self, kind: Kind, id: i64) -> R<Vec<Value>> {
        let mut tags: Vec<(String, i64)> = self
            .conn
            .prepare_cached(q::TAGS)?
            .query_map((kind.code(), id), |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<R<_>>()?;
        tags.sort();
        Ok(tags
            .into_iter()
            .map(|(name, count)| json!({"count": count, "name": name}))
            .collect())
    }

    /// Aliases and tags, when there are any.
    fn extras(&self, obj: &mut Obj, kind: Kind, id: i64) -> R<()> {
        put_list(obj, "aliases", self.aliases(kind, id)?);
        put_list(obj, "tags", self.tags(kind, id)?);
        Ok(())
    }

    fn area(&self, id: i64, full: bool) -> R<Obj> {
        type Row = (
            String,
            String,
            String,
            String,
            bool,
            Option<i64>,
            Option<String>,
        );
        let (gid, name, begin, end, ended, type_id, comment): Row =
            self.conn.prepare_cached(q::AREA)?.query_row([id], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    date::format(r.get(2)?, r.get(3)?, r.get(4)?),
                    date::format(r.get(5)?, r.get(6)?, r.get(7)?),
                    r.get::<_, Option<bool>>(8)?.unwrap_or(false),
                    r.get(9)?,
                    r.get(10)?,
                ))
            })?;
        let mut out = Obj::new();
        out.insert("id".into(), Value::String(gid));
        self.named(&mut out, "type", "area_type", type_id);
        out.insert("name".into(), Value::String(name.clone()));
        out.insert("sort-name".into(), Value::String(name));
        if full {
            put(&mut out, "disambiguation", comment);
            for (key, sql) in [
                ("iso-3166-1-codes", q::AREA_ISO1),
                ("iso-3166-2-codes", q::AREA_ISO2),
                ("iso-3166-3-codes", q::AREA_ISO3),
            ] {
                put_list(
                    &mut out,
                    key,
                    self.strings(sql, id)?
                        .into_iter()
                        .map(Value::String)
                        .collect(),
                );
            }
            self.extras(&mut out, Kind::Area, id)?;
        }
        out.insert("life-span".into(), Self::life_span(begin, end, ended));
        Ok(out)
    }

    fn artist(&self, id: i64) -> R<Obj> {
        let row = self.conn.prepare_cached(q::ARTIST)?.query_row([id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                date::format(r.get(3)?, r.get(4)?, r.get(5)?),
                date::format(r.get(6)?, r.get(7)?, r.get(8)?),
                r.get::<_, Option<bool>>(9)?.unwrap_or(false),
                r.get::<_, Option<i64>>(10)?,
                r.get::<_, Option<i64>>(11)?,
                r.get::<_, Option<i64>>(12)?,
                r.get::<_, Option<String>>(13)?,
                r.get::<_, Option<i64>>(14)?,
                r.get::<_, Option<i64>>(15)?,
            ))
        })?;
        let (
            gid,
            name,
            sort_name,
            begin,
            end,
            ended,
            type_id,
            area,
            gender,
            comment,
            begin_area,
            end_area,
        ) = row;
        let mut out = Obj::new();
        out.insert("id".into(), Value::String(gid));
        self.named(&mut out, "type", "artist_type", type_id);
        out.insert("name".into(), Value::String(name));
        out.insert("sort-name".into(), Value::String(sort_name));
        self.named(&mut out, "gender", "gender", gender);
        if let Some(area) = area {
            let country: Option<String> = self
                .conn
                .prepare_cached(q::AREA_COUNTRY)?
                .query_row([area], |r| r.get(0))
                .optional()?;
            put(&mut out, "country", country);
        }
        for (key, area) in [
            ("area", area),
            ("begin-area", begin_area),
            ("end-area", end_area),
        ] {
            if let Some(area) = area {
                out.insert(key.into(), Value::Object(self.area(area, false)?));
            }
        }
        let ipis = self.strings(q::ARTIST_IPIS, id)?;
        put_list(
            &mut out,
            "ipis",
            ipis.into_iter().map(Value::String).collect(),
        );
        let isnis = self.strings(q::ARTIST_ISNIS, id)?;
        put_list(
            &mut out,
            "isnis",
            isnis.into_iter().map(Value::String).collect(),
        );
        out.insert("life-span".into(), Self::life_span(begin, end, ended));
        put(&mut out, "disambiguation", comment);
        self.extras(&mut out, Kind::Artist, id)?;
        Ok(out)
    }

    /// An artist credit and its MBID: (`artist-credit`, `artist-credit-id`). Artists have their aliases with
    /// `aliases`.
    fn artist_credit(&self, ac: i64, aliases: bool) -> R<(Value, Option<String>)> {
        let names: Vec<(i64, String, Option<String>)> = self
            .conn
            .prepare_cached(q::ARTIST_CREDIT)?
            .query_map([ac], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<R<_>>()?;
        let mut out = Vec::with_capacity(names.len());
        for (artist, name, join) in names {
            let (gid, artist_name, sort_name, comment): (String, String, String, Option<String>) =
                self.conn
                    .prepare_cached(q::ARTIST)?
                    .query_row([artist], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(13)?))
                    })?;
            let mut a = Obj::new();
            a.insert("id".into(), Value::String(gid));
            a.insert("name".into(), Value::String(artist_name));
            a.insert("sort-name".into(), Value::String(sort_name));
            put(&mut a, "disambiguation", comment);
            if aliases {
                put_list(&mut a, "aliases", self.aliases(Kind::Artist, artist)?);
            }
            let mut credit = Obj::new();
            credit.insert("name".into(), Value::String(name));
            put(&mut credit, "joinphrase", join);
            credit.insert("artist".into(), Value::Object(a));
            out.push(Value::Object(credit));
        }
        let gid: Option<String> = self
            .conn
            .prepare_cached("SELECT gid FROM artist_credit WHERE id = ?1")?
            .query_row([ac], |r| r.get(0))
            .optional()?
            .flatten();
        Ok((Value::Array(out), gid))
    }

    fn release_group_short(&self, rg: i64, with_comment: bool) -> R<Obj> {
        let (gid, name, type_id, comment): (String, String, Option<i64>, Option<String>) = self
            .conn
            .prepare_cached(q::RELEASE_GROUP)?
            .query_row([rg], |r| Ok((r.get(0)?, r.get(1)?, r.get(3)?, r.get(4)?)))?;
        let mut out = Obj::new();
        out.insert("id".into(), Value::String(gid));
        if let Some(t) = self.lk.named("release_group_primary_type", type_id) {
            if let Some(g) = &t.gid {
                out.insert("type-id".into(), Value::String(g.clone()));
                out.insert("primary-type-id".into(), Value::String(g.clone()));
            }
            out.insert("primary-type".into(), Value::String(t.name.clone()));
        }
        out.insert("title".into(), Value::String(name));
        if with_comment {
            put(&mut out, "disambiguation", comment);
        }
        self.secondary_types(&mut out, rg)?;
        Ok(out)
    }

    fn secondary_types(&self, out: &mut Obj, rg: i64) -> R<()> {
        let types: Vec<i64> = self
            .conn
            .prepare_cached(q::RELEASE_GROUP_SECONDARY_TYPES)?
            .query_map([rg], |r| r.get(0))?
            .collect::<R<_>>()?;
        let named: Vec<_> = types
            .iter()
            .filter_map(|t| self.lk.named("release_group_secondary_type", Some(*t)))
            .collect();
        put_list(
            out,
            "secondary-types",
            named
                .iter()
                .map(|n| Value::String(n.name.clone()))
                .collect(),
        );
        put_list(
            out,
            "secondary-type-ids",
            named
                .iter()
                .filter_map(|n| n.gid.clone())
                .map(Value::String)
                .collect(),
        );
        Ok(())
    }

    /// Release events, the first one's date and country.
    fn release_events(&self, out: &mut Obj, release: i64) -> R<()> {
        let events: Vec<(Option<i64>, String)> = self
            .conn
            .prepare_cached(q::RELEASE_EVENTS)?
            .query_map([release], |r| {
                Ok((r.get(0)?, date::format(r.get(1)?, r.get(2)?, r.get(3)?)))
            })?
            .collect::<R<_>>()?;
        if let Some((country, first)) = events.first() {
            put(out, "date", Some(first.clone()));
            if let Some(country) = country {
                put(
                    out,
                    "country",
                    self.strings(q::AREA_ISO1, *country)?.into_iter().next(),
                );
            }
        }
        let mut list = Vec::new();
        for (country, date) in events {
            let mut event = Obj::new();
            put(&mut event, "date", Some(date));
            if let Some(country) = country {
                let (gid, name): (String, String) = self
                    .conn
                    .prepare_cached(q::AREA)?
                    .query_row([country], |r| Ok((r.get(0)?, r.get(1)?)))?;
                let mut area = Obj::new();
                area.insert("id".into(), Value::String(gid));
                area.insert("name".into(), Value::String(name.clone()));
                area.insert("sort-name".into(), Value::String(name));
                put_list(
                    &mut area,
                    "iso-3166-1-codes",
                    self.strings(q::AREA_ISO1, country)?
                        .into_iter()
                        .map(Value::String)
                        .collect(),
                );
                event.insert("area".into(), Value::Object(area));
            }
            list.push(Value::Object(event));
        }
        put_list(out, "release-events", list);
        Ok(())
    }

    /// A release; `full` for a release result (labels, media formats, tags), else as listed in a recording.
    #[allow(clippy::too_many_lines)]
    fn release(&self, id: i64, full: bool) -> R<Obj> {
        type Row = (
            String,
            String,
            i64,
            i64,
            Option<i64>,
            Option<i64>,
            Option<i64>,
            Option<i64>,
        );
        type Row2 = (Option<String>, Option<String>, Option<String>);
        let (gid, name, ac, rg, status, packaging, language, script): Row =
            self.conn.prepare_cached(q::RELEASE)?.query_row([id], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                ))
            })?;
        let (barcode, comment, asin): Row2 = self
            .conn
            .prepare_cached(q::RELEASE)?
            .query_row([id], |r| Ok((r.get(8)?, r.get(9)?, r.get(11)?)))?;
        let mediums: Vec<(i64, Option<String>, i64, Option<i64>, i64)> = self
            .conn
            .prepare_cached(q::RELEASE_MEDIUMS)?
            .query_map([id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(5)?))
            })?
            .collect::<R<_>>()?;
        let mut out = Obj::new();
        out.insert("id".into(), Value::String(gid));
        out.insert("title".into(), Value::String(name));
        self.named(&mut out, "status", "release_status", status);
        let (credit, credit_id) = self.artist_credit(ac, false)?;
        put(&mut out, "artist-credit-id", credit_id);
        out.insert("artist-credit".into(), credit);
        out.insert("count".into(), json!(mediums.len()));
        out.insert(
            "release-group".into(),
            Value::Object(self.release_group_short(rg, !full)?),
        );
        self.release_events(&mut out, id)?;
        out.insert(
            "track-count".into(),
            json!(mediums.iter().map(|m| m.4).sum::<i64>()),
        );
        if !full {
            return Ok(out);
        }
        put(&mut out, "disambiguation", comment);
        self.named(&mut out, "packaging", "release_packaging", packaging);
        let mut text = Obj::new();
        put(
            &mut text,
            "language",
            language.and_then(|l| self.lk.languages.get(&l).cloned()),
        );
        put(
            &mut text,
            "script",
            script.and_then(|s| self.lk.scripts.get(&s).cloned()),
        );
        out.insert("text-representation".into(), Value::Object(text));
        put(&mut out, "barcode", barcode);
        put(&mut out, "asin", asin);
        let labels: Vec<(Option<i64>, Option<String>)> = self
            .conn
            .prepare_cached(q::RELEASE_LABELS)?
            .query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<R<_>>()?;
        let mut label_info = Vec::new();
        for (label, catno) in labels {
            let mut info = Obj::new();
            put(&mut info, "catalog-number", catno);
            if let Some(label) = label {
                let (lgid, lname): (String, String) = self
                    .conn
                    .prepare_cached(q::LABEL)?
                    .query_row([label], |r| Ok((r.get(0)?, r.get(1)?)))?;
                info.insert("label".into(), json!({"id": lgid, "name": lname}));
            }
            label_info.push(Value::Object(info));
        }
        put_list(&mut out, "label-info", label_info);
        let mut media = Vec::new();
        for (medium, mgid, _, format, track_count) in mediums {
            let mut m = Obj::new();
            put(&mut m, "id", mgid);
            if let Some(f) = self.lk.named("medium_format", format) {
                m.insert("format".into(), Value::String(f.name.clone()));
            }
            let discs: i64 = self
                .conn
                .prepare_cached("SELECT count(*) FROM medium_cdtoc WHERE medium = ?1")?
                .query_row([medium], |r| r.get(0))?;
            m.insert("disc-count".into(), json!(discs));
            m.insert("track-count".into(), json!(track_count));
            media.push(Value::Object(m));
        }
        out.insert("media".into(), Value::Array(media));
        put_list(&mut out, "tags", self.tags(Kind::Release, id)?);
        Ok(out)
    }

    fn release_group(&self, id: i64) -> R<Obj> {
        type Row = (String, String, i64, Option<i64>, Option<String>, String);
        let (gid, name, ac, type_id, comment, first): Row = self
            .conn
            .prepare_cached(q::RELEASE_GROUP)?
            .query_row([id], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    date::format(r.get(5)?, r.get(6)?, r.get(7)?),
                ))
            })?;
        let mut out = Obj::new();
        out.insert("id".into(), Value::String(gid));
        if let Some(t) = self.lk.named("release_group_primary_type", type_id) {
            if let Some(g) = &t.gid {
                out.insert("type-id".into(), Value::String(g.clone()));
                out.insert("primary-type-id".into(), Value::String(g.clone()));
            }
            out.insert("primary-type".into(), Value::String(t.name.clone()));
        }
        self.secondary_types(&mut out, id)?;
        out.insert("title".into(), Value::String(name));
        put(&mut out, "first-release-date", Some(first));
        put(&mut out, "disambiguation", comment);
        let (credit, credit_id) = self.artist_credit(ac, false)?;
        put(&mut out, "artist-credit-id", credit_id);
        out.insert("artist-credit".into(), credit);
        let releases: Vec<(String, String, Option<i64>)> = self
            .conn
            .prepare_cached(
                "SELECT gid, name, status FROM release WHERE release_group = ?1 ORDER BY id",
            )?
            .query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<R<_>>()?;
        out.insert("count".into(), json!(releases.len()));
        let releases = releases
            .into_iter()
            .map(|(gid, title, status)| {
                let mut r = Obj::new();
                r.insert("id".into(), Value::String(gid));
                r.insert("title".into(), Value::String(title));
                self.named(&mut r, "status", "release_status", status);
                Value::Object(r)
            })
            .collect();
        out.insert("releases".into(), Value::Array(releases));
        put_list(&mut out, "tags", self.tags(Kind::ReleaseGroup, id)?);
        Ok(out)
    }

    fn recording(&self, id: i64) -> R<Obj> {
        type Row = (
            String,
            String,
            i64,
            Option<i64>,
            Option<String>,
            Option<bool>,
        );
        let (gid, name, ac, length, comment, video): Row = self
            .conn
            .prepare_cached(q::RECORDING)?
            .query_row([id], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            })?;
        let mut out = Obj::new();
        out.insert("id".into(), Value::String(gid));
        out.insert("title".into(), Value::String(name));
        if let Some(length) = length {
            out.insert("length".into(), json!(length));
        }
        out.insert(
            "video".into(),
            if video.unwrap_or(false) {
                Value::Bool(true)
            } else {
                Value::Null
            },
        );
        put(&mut out, "disambiguation", comment);
        let (credit, credit_id) = self.artist_credit(ac, true)?;
        put(&mut out, "artist-credit-id", credit_id);
        out.insert("artist-credit".into(), credit);
        let first: Option<i64> = self
            .conn
            .prepare_cached(q::RECORDING_FIRST_RELEASE_DATE)?
            .query_row([id], |r| r.get(0))
            .optional()?;
        put(
            &mut out,
            "first-release-date",
            first.map(date::format_packed),
        );
        let isrcs = self.strings(q::RECORDING_ISRCS, id)?;
        put_list(
            &mut out,
            "isrcs",
            isrcs.into_iter().map(Value::String).collect(),
        );

        // The releases it is on, each with the medium and track.
        let tracks: Vec<(i64, String, i64, Option<String>, String, Option<i64>, i64)> = self
            .conn
            .prepare_cached(
                "SELECT m.release, t.gid, t.position, t.number, t.name, t.length, t.medium FROM track t \
                 JOIN medium m ON m.id = t.medium WHERE t.recording = ?1 ORDER BY m.release, m.position, t.position",
            )?
            .query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)))?
            .collect::<R<_>>()?;
        let mut releases = Vec::new();
        let mut last = None;
        for (release, tgid, position, number, title, tlength, medium) in tracks {
            if last == Some(release) || releases.len() >= RECORDING_RELEASES {
                continue;
            }
            last = Some(release);
            let mut r = self.release(release, false)?;
            let (mgid, mposition, format, track_count): (Option<String>, i64, Option<i64>, i64) =
                self.conn
                    .prepare_cached(q::MEDIUM)?
                    .query_row([medium], |row| {
                        Ok((row.get(1)?, row.get(2)?, row.get(3)?, row.get(5)?))
                    })?;
            let mut m = Obj::new();
            put(&mut m, "id", mgid);
            m.insert("position".into(), json!(mposition));
            if let Some(f) = self.lk.named("medium_format", format) {
                m.insert("format".into(), Value::String(f.name.clone()));
            }
            let mut track = Obj::new();
            track.insert("id".into(), Value::String(tgid));
            put(&mut track, "number", number);
            track.insert("title".into(), Value::String(title));
            if let Some(l) = tlength {
                track.insert("length".into(), json!(l));
            }
            m.insert("track".into(), json!([track]));
            m.insert("track-count".into(), json!(track_count));
            m.insert("track-offset".into(), json!(position - 1));
            r.insert("media".into(), json!([m]));
            releases.push(Value::Object(r));
        }
        put_list(&mut out, "releases", releases);
        put_list(&mut out, "tags", self.tags(Kind::Recording, id)?);
        Ok(out)
    }

    fn work(&self, id: i64) -> R<Obj> {
        let (gid, name, type_id, comment): (String, String, Option<i64>, Option<String>) = self
            .conn
            .prepare_cached(q::WORK)?
            .query_row([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
        let mut out = Obj::new();
        out.insert("id".into(), Value::String(gid));
        if let Some(t) = self.lk.named("work_type", type_id) {
            out.insert("type".into(), Value::String(t.name.clone()));
        }
        out.insert("title".into(), Value::String(name));
        let languages: Vec<String> = self
            .conn
            .prepare_cached(q::WORK_LANGUAGES)?
            .query_map([id], |r| r.get::<_, i64>(0))?
            .collect::<R<Vec<_>>>()?
            .into_iter()
            .filter_map(|l| self.lk.languages.get(&l).cloned())
            .collect();
        match languages.as_slice() {
            [] => {}
            [one] => {
                out.insert("language".into(), Value::String(one.clone()));
            }
            _ => {
                out.insert("language".into(), Value::String("mul".into()));
            }
        }
        put_list(
            &mut out,
            "languages",
            languages.into_iter().map(Value::String).collect(),
        );
        let iswcs = self.strings(q::WORK_ISWCS, id)?;
        put_list(
            &mut out,
            "iswcs",
            iswcs.into_iter().map(Value::String).collect(),
        );
        put(&mut out, "disambiguation", comment);
        // Its artists (composer, lyricist, ...)
        let rels: Vec<(i64, i64)> = self
            .conn
            .prepare_cached(q::RELS_BACKWARD)?
            .query_map((Kind::Work.code(), id, Kind::Artist.code()), |r| {
                Ok((r.get(1)?, r.get(6)?))
            })?
            .collect::<R<_>>()?;
        let mut relations = Vec::new();
        for (artist, link_type) in rels {
            let (agid, aname, asort): (String, String, String) = self
                .conn
                .prepare_cached(q::ARTIST)?
                .query_row([artist], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            let lt = self.lk.link_types.get(&link_type);
            relations.push(json!({
                "type": lt.map(|t| t.name.clone()),
                "type-id": lt.and_then(|t| t.gid.clone()),
                "direction": "backward",
                "artist": {"id": agid, "name": aname, "sort-name": asort},
            }));
        }
        put_list(&mut out, "relations", relations);
        self.extras(&mut out, Kind::Work, id)?;
        Ok(out)
    }

    fn label(&self, id: i64) -> R<Obj> {
        let row = self.conn.prepare_cached(q::LABEL)?.query_row([id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                date::format(r.get(2)?, r.get(3)?, r.get(4)?),
                date::format(r.get(5)?, r.get(6)?, r.get(7)?),
                r.get::<_, Option<bool>>(8)?.unwrap_or(false),
                r.get::<_, Option<i64>>(9)?,
                r.get::<_, Option<i64>>(10)?,
                r.get::<_, Option<i64>>(11)?,
                r.get::<_, Option<String>>(12)?,
            ))
        })?;
        let (gid, name, begin, end, ended, type_id, code, area, comment) = row;
        let mut out = Obj::new();
        out.insert("id".into(), Value::String(gid));
        self.named(&mut out, "type", "label_type", type_id);
        out.insert("name".into(), Value::String(name.clone()));
        out.insert("sort-name".into(), Value::String(name));
        if let Some(code) = code {
            out.insert("label-code".into(), json!(code));
        }
        if let Some(area) = area {
            put(
                &mut out,
                "country",
                self.strings(q::AREA_ISO1, area)?.into_iter().next(),
            );
            out.insert("area".into(), Value::Object(self.area(area, false)?));
        }
        let ipis = self.strings(q::LABEL_IPIS, id)?;
        put_list(
            &mut out,
            "ipis",
            ipis.into_iter().map(Value::String).collect(),
        );
        let isnis = self.strings(q::LABEL_ISNIS, id)?;
        put_list(
            &mut out,
            "isnis",
            isnis.into_iter().map(Value::String).collect(),
        );
        out.insert("life-span".into(), Self::life_span(begin, end, ended));
        put(&mut out, "disambiguation", comment);
        self.extras(&mut out, Kind::Label, id)?;
        Ok(out)
    }

    fn place(&self, id: i64) -> R<Obj> {
        let row = self.conn.prepare_cached(q::PLACE)?.query_row([id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                date::format(r.get(2)?, r.get(3)?, r.get(4)?),
                date::format(r.get(5)?, r.get(6)?, r.get(7)?),
                r.get::<_, Option<bool>>(8)?.unwrap_or(false),
                r.get::<_, Option<i64>>(9)?,
                r.get::<_, Option<String>>(10)?,
                r.get::<_, Option<i64>>(11)?,
                r.get::<_, Option<String>>(12)?,
                r.get::<_, Option<String>>(13)?,
            ))
        })?;
        let (gid, name, begin, end, ended, type_id, address, area, coordinates, comment) = row;
        let mut out = Obj::new();
        out.insert("id".into(), Value::String(gid));
        self.named(&mut out, "type", "place_type", type_id);
        out.insert("name".into(), Value::String(name));
        put(&mut out, "address", address);
        // The search server has coordinates as strings.
        if let Some((lat, long)) = coordinates
            .as_deref()
            .and_then(|c| c.trim_matches(|c| c == '(' || c == ')').split_once(','))
        {
            out.insert(
                "coordinates".into(),
                json!({"latitude": lat.trim(), "longitude": long.trim()}),
            );
        }
        if let Some(area) = area {
            out.insert("area".into(), Value::Object(self.area(area, false)?));
        }
        out.insert("life-span".into(), Self::life_span(begin, end, ended));
        put(&mut out, "disambiguation", comment);
        self.extras(&mut out, Kind::Place, id)?;
        Ok(out)
    }

    fn event(&self, id: i64) -> R<Obj> {
        let row = self.conn.prepare_cached(q::EVENT)?.query_row([id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                date::format(r.get(2)?, r.get(3)?, r.get(4)?),
                date::format(r.get(5)?, r.get(6)?, r.get(7)?),
                r.get::<_, Option<bool>>(8)?.unwrap_or(false),
                r.get::<_, Option<i64>>(9)?,
                r.get::<_, Option<String>>(10)?,
                r.get::<_, Option<String>>(13)?,
            ))
        })?;
        let (gid, name, begin, end, ended, type_id, time, comment) = row;
        let mut out = Obj::new();
        out.insert("id".into(), Value::String(gid));
        if let Some(t) = self.lk.named("event_type", type_id) {
            out.insert("type".into(), Value::String(t.name.clone()));
        }
        out.insert("name".into(), Value::String(name));
        put(
            &mut out,
            "time",
            time.map(|t| t.get(..5).unwrap_or(&t).to_owned()),
        );
        let mut span = Obj::new();
        put(&mut span, "begin", Some(begin));
        put(&mut span, "end", Some(end));
        if ended {
            span.insert("ended".into(), Value::Bool(true));
        }
        out.insert("life-span".into(), Value::Object(span));
        put(&mut out, "disambiguation", comment);
        // Its artists
        let rels: Vec<(i64, i64)> = self
            .conn
            .prepare_cached(q::RELS_BACKWARD)?
            .query_map((Kind::Event.code(), id, Kind::Artist.code()), |r| {
                Ok((r.get(1)?, r.get(6)?))
            })?
            .collect::<R<_>>()?;
        let mut relations = Vec::new();
        for (artist, link_type) in rels {
            let (agid, aname, asort): (String, String, String) = self
                .conn
                .prepare_cached(q::ARTIST)?
                .query_row([artist], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            let lt = self.lk.link_types.get(&link_type);
            relations.push(json!({
                "type": lt.map(|t| t.name.clone()),
                "type-id": lt.and_then(|t| t.gid.clone()),
                "direction": "backward",
                "artist": {"id": agid, "name": aname, "sort-name": asort},
            }));
        }
        put_list(&mut out, "relations", relations);
        self.extras(&mut out, Kind::Event, id)?;
        Ok(out)
    }

    /// Instruments and series: name, type, disambiguation (and an instrument's description).
    fn simple(&self, id: i64, kind: Kind, sql: &str, type_table: &str) -> R<Obj> {
        let (gid, name): (String, String) = self
            .conn
            .prepare_cached(sql)?
            .query_row([id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        let (type_id, comment, description): (Option<i64>, Option<String>, Option<String>) =
            if kind == Kind::Series {
                self.conn
                    .prepare_cached(sql)?
                    .query_row([id], |r| Ok((r.get(3)?, r.get(2)?, None)))?
            } else {
                self.conn
                    .prepare_cached(sql)?
                    .query_row([id], |r| Ok((r.get(2)?, r.get(3)?, r.get(4)?)))?
            };
        let mut out = Obj::new();
        out.insert("id".into(), Value::String(gid));
        self.named(&mut out, "type", type_table, type_id);
        out.insert("name".into(), Value::String(name));
        put(&mut out, "description", description);
        put(&mut out, "disambiguation", comment);
        self.extras(&mut out, kind, id)?;
        Ok(out)
    }
}
