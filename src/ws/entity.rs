//! Entities as the web service's JSON. This follows musicbrainz-server's JSON serializer
//! (`WebService::Serializer::JSON::2::*`) and what its controllers load before serializing: which fields appear
//! depends on the entity type, `inc`, and where in the response an entity is (top level, in a list, in an artist
//! credit, as a relationship target).

// Rows are read into tuples, as wide as the columns they read.
#![allow(clippy::type_complexity)]

use std::collections::HashSet;

use rusqlite::{Connection, OptionalExtension as _, Row};
use serde_json::{Map, Value, json};

use super::inc::Inc;
use crate::db::{Lookups, date, queries as q};
use crate::kind::Kind;

pub type Obj = Map<String, Value>;
type R<T> = rusqlite::Result<T>;

/// Most lists of related entities in a lookup are cut at this many (musicbrainz-server's `$MAX_ITEMS`).
pub const MAX_ITEMS: i64 = 25;

/// Release filters of browse requests and of lists in lookups (`status=`, `type=`).
#[derive(Debug, Default, Clone)]
pub struct Filter {
    pub status: Vec<i64>,
    pub primary_types: Vec<i64>,
    pub secondary_types: Vec<i64>,
}

impl Filter {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.status.is_empty() && self.primary_types.is_empty() && self.secondary_types.is_empty()
    }
}

/// Which relationships of an entity are loaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rels {
    None,
    All,
    /// Of works reached through `work-level-rels`: only those where the work's side has cardinality 0 (so a work
    /// doesn't list every recording of it).
    Cardinal,
}

/// Where an entity is in the response; musicbrainz-server's `$toplevel` and package variables.
#[derive(Debug, Clone, Copy)]
#[allow(clippy::struct_excessive_bools)]
pub struct Opts {
    pub toplevel: bool,
    /// Serialized with the request's `inc`. musicbrainz-server serializes some nested entities (an artist's area,
    /// a release event's country) without it: no aliases, tags, ratings or relationships for those.
    pub with_inc: bool,
    pub aliases: bool,
    pub tags: bool,
    pub sublevel_ratings: bool,
    /// The artist credit of the release being serialized, and of the track (to leave out what they repeat).
    pub release_ac: Option<i64>,
    pub track_ac: Option<i64>,
    pub rels: Rels,
    /// Whether a recording's first release date was loaded.
    pub first_release_date: bool,
    /// Whether an area's type was loaded.
    pub area_type: bool,
    /// Whether a work's ISWCs and languages were loaded (not for works only reached through another work's
    /// relationships).
    pub work_details: bool,
}

impl Opts {
    #[must_use]
    pub fn toplevel() -> Self {
        Self {
            toplevel: true,
            with_inc: true,
            aliases: true,
            tags: true,
            sublevel_ratings: false,
            release_ac: None,
            track_ac: None,
            rels: Rels::All,
            first_release_date: true,
            area_type: true,
            work_details: true,
        }
    }

    #[must_use]
    pub fn nested() -> Self {
        Self {
            toplevel: false,
            rels: Rels::None,
            first_release_date: false,
            area_type: false,
            ..Self::toplevel()
        }
    }

    fn bare() -> Self {
        Self {
            with_inc: false,
            ..Self::nested()
        }
    }

    fn not_toplevel(self) -> Self {
        Self {
            toplevel: false,
            rels: Rels::None,
            first_release_date: false,
            ..self
        }
    }
}

/// What entities are serialized with.
pub struct Ctx<'a> {
    pub conn: &'a Connection,
    pub lk: &'a Lookups,
    pub inc: &'a Inc,
    pub filter: &'a Filter,
    /// The answer is XML: musicbrainz-server's XML serializer shows what was loaded, without the JSON
    /// serializer's rules for leaving out repeated aliases and tags, and ratings wherever they were loaded.
    pub xml: bool,
}

struct Period {
    begin: String,
    end: String,
    ended: bool,
}

impl Period {
    /// From 7 columns starting at `i`: begin year, month, day, end year, month, day, ended.
    fn from_row(row: &Row<'_>, i: usize) -> R<Self> {
        Ok(Self {
            begin: date::format(row.get(i)?, row.get(i + 1)?, row.get(i + 2)?),
            end: date::format(row.get(i + 3)?, row.get(i + 4)?, row.get(i + 5)?),
            ended: row.get::<_, Option<bool>>(i + 6)?.unwrap_or(false),
        })
    }

    fn write(&self, into: &mut Obj) {
        into.insert("begin".into(), opt_str(&self.begin));
        into.insert("end".into(), opt_str(&self.end));
        into.insert("ended".into(), Value::Bool(self.ended));
    }
}

/// An empty string as null (musicbrainz-server's `format || JSON::null`).
fn opt_str(s: &str) -> Value {
    if s.is_empty() {
        Value::Null
    } else {
        Value::String(s.to_owned())
    }
}

fn text(v: Option<String>) -> Value {
    v.map_or(Value::Null, Value::String)
}

fn int(v: Option<i64>) -> Value {
    v.map_or(Value::Null, Value::from)
}

/// Common to every entity: what musicbrainz-server's `serialize_entity` adds to the type's own serializer.
struct Common<'a> {
    kind: Kind,
    id: i64,
    gid: String,
    period: Option<Period>,
    /// (lookup table, id) of the entity's type, for entity types with a "simple" type.
    type_: Option<(&'a str, Option<i64>)>,
}

impl Ctx<'_> {
    fn stmt(&self, sql: &str) -> R<rusqlite::CachedStatement<'_>> {
        self.conn.prepare_cached(sql)
    }

    fn strings(&self, sql: &str, id: i64) -> R<Vec<String>> {
        self.stmt(sql)?.query_map([id], |r| r.get(0))?.collect()
    }

    fn named(&self, into: &mut Obj, key: &str, table: &str, id: Option<i64>) {
        let named = self.lk.named(table, id);
        into.insert(
            key.into(),
            named.map_or(Value::Null, |n| Value::String(n.name.clone())),
        );
        into.insert(
            format!("{key}-id"),
            named
                .and_then(|n| n.gid.clone())
                .map_or(Value::Null, Value::String),
        );
    }

    /// An entity of any type.
    pub fn entity(&self, kind: Kind, id: i64, o: Opts) -> R<Value> {
        match kind {
            Kind::Area => self.area(id, o),
            Kind::Artist => self.artist(id, o),
            Kind::Event => self.event(id, o),
            Kind::Genre => self.genre(id, o),
            Kind::Instrument => self.instrument(id, o),
            Kind::Label => self.label(id, o),
            Kind::Place => self.place(id, o),
            Kind::Recording => self.recording(id, o),
            Kind::Release => self.release(id, o),
            Kind::ReleaseGroup => self.release_group(id, o),
            Kind::Series => self.series(id, o),
            Kind::Url => self.url(id, o),
            Kind::Work => self.work(id, o),
        }
    }

    fn finish(&self, out: &mut Obj, c: Common<'_>, o: Opts) -> R<()> {
        let inc = self.inc;
        let artist_in_credits = || -> R<bool> {
            if c.kind != Kind::Artist {
                return Ok(false);
            }
            for ac in [o.release_ac, o.track_ac].into_iter().flatten() {
                if self.credit_artists(ac)?.contains(&c.id) {
                    return Ok(true);
                }
            }
            Ok(false)
        };

        if c.kind.has_aliases() && o.with_inc && o.aliases && inc.aliases {
            // Recording artists repeated from the release's or track's credit don't repeat their aliases.
            let repeated =
                !self.xml && o.release_ac.is_some() && o.track_ac.is_some() && artist_in_credits()?;
            if !repeated {
                out.insert("aliases".into(), self.aliases(c.kind, c.id)?);
            }
        }
        if c.kind.has_annotations() && o.toplevel && o.with_inc && inc.annotation {
            let text: Option<Option<String>> = self
                .stmt(q::ANNOTATION)?
                .query_row((c.kind.code(), c.id), |r| r.get(0))
                .optional()?;
            out.insert(
                "annotation".into(),
                text.flatten().map_or(Value::Null, Value::String),
            );
        }
        out.insert("id".into(), Value::String(c.gid));
        if o.toplevel {
            let codes = match c.kind {
                Kind::Artist => Some((q::ARTIST_IPIS, q::ARTIST_ISNIS)),
                Kind::Label => Some((q::LABEL_IPIS, q::LABEL_ISNIS)),
                _ => None,
            };
            if let Some((ipis, isnis)) = codes {
                out.insert("ipis".into(), json!(self.strings(ipis, c.id)?));
                out.insert("isnis".into(), json!(self.strings(isnis, c.id)?));
            }
        }
        if let Some(period) = &c.period
            && (o.toplevel || c.kind == Kind::Event)
        {
            let mut span = Obj::new();
            period.write(&mut span);
            out.insert("life-span".into(), Value::Object(span));
        }
        if c.kind.has_ratings()
            && (o.toplevel || o.sublevel_ratings || self.xml && o.tags)
            && o.with_inc
            && (inc.ratings || inc.user_ratings)
        {
            self.rating(out, c.kind, c.id)?;
        }
        if o.with_inc && inc.has_rels() && o.rels != Rels::None {
            let rels = self.relations(c.kind, c.id, o.rels == Rels::Cardinal)?;
            out.insert("relations".into(), Value::Array(rels));
        }
        if c.kind.has_tags()
            && o.with_inc
            && o.tags
            && inc.tags_or_genres()
            && (self.xml || !artist_in_credits()?)
        {
            self.tags(out, c.kind, c.id)?;
        }
        if let Some((table, type_id)) = c.type_ {
            self.named(out, "type", table, type_id);
        }
        Ok(())
    }

    fn aliases(&self, kind: Kind, id: i64) -> R<Value> {
        let mut stmt = self.stmt(q::ALIASES)?;
        let mut aliases: Vec<((String, Option<String>, String), Obj)> = stmt
            .query_map((kind.code(), id), |r| {
                let name: String = r.get(0)?;
                let locale: Option<String> = r.get(2)?;
                let mut a = Obj::new();
                a.insert("name".into(), Value::String(name.clone()));
                a.insert("sort-name".into(), Value::String(r.get(1)?));
                a.insert(
                    "primary".into(),
                    if locale.is_some() {
                        Value::Bool(r.get::<_, Option<bool>>(4)?.unwrap_or(false))
                    } else {
                        Value::Null
                    },
                );
                a.insert("locale".into(), text(locale));
                let type_ = self.lk.alias_type(kind, r.get(3)?);
                a.insert(
                    "type".into(),
                    type_.map_or(Value::Null, |t| Value::String(t.name.clone())),
                );
                a.insert(
                    "type-id".into(),
                    type_
                        .and_then(|t| t.gid.clone())
                        .map_or(Value::Null, Value::String),
                );
                Period::from_row(r, 5)?.write(&mut a);
                let sort_name: String = r.get(1)?;
                let locale: Option<String> = r.get(2)?;
                Ok(((name, locale, sort_name), a))
            })?
            .collect::<R<_>>()?;
        aliases.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(Value::Array(
            aliases.into_iter().map(|(_, a)| Value::Object(a)).collect(),
        ))
    }

    fn tags(&self, out: &mut Obj, kind: Kind, id: i64) -> R<()> {
        let inc = self.inc;
        if inc.tags {
            let mut tags: Vec<(String, i64)> = self
                .stmt(q::TAGS)?
                .query_map((kind.code(), id), |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<R<_>>()?;
            tags.sort();
            out.insert(
                "tags".into(),
                Value::Array(
                    tags.into_iter()
                        .map(|(name, count)| json!({"name": name, "count": count}))
                        .collect(),
                ),
            );
        }
        if inc.genres {
            let mut genres: Vec<(String, String, String, i64)> = self
                .stmt(q::GENRES)?
                .query_map((kind.code(), id), |r| {
                    Ok((
                        r.get(1)?,
                        r.get(0)?,
                        r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                        r.get(3)?,
                    ))
                })?
                .collect::<R<_>>()?;
            genres.sort();
            out.insert(
                "genres".into(),
                Value::Array(
                    genres
                        .into_iter()
                        .map(|(name, gid, comment, count)| {
                            json!({"name": name, "id": gid, "disambiguation": comment, "count": count})
                        })
                        .collect(),
                ),
            );
        }
        // There are no users here: nobody has tagged anything.
        if inc.user_tags {
            out.insert("user-tags".into(), json!([]));
        }
        if inc.user_genres {
            out.insert("user-genres".into(), json!([]));
        }
        Ok(())
    }

    fn rating(&self, out: &mut Obj, kind: Kind, id: i64) -> R<()> {
        if self.inc.ratings {
            let row: Option<(Option<i64>, Option<i64>)> = self
                .stmt(q::RATING)?
                .query_row((kind.code(), id), |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?;
            let rating = match row {
                Some((Some(rating), Some(count))) if count > 0 => {
                    // Stored out of 100, shown out of 5: 80 is 4, 76 is 3.8.
                    let value = if rating * 5 % 100 == 0 {
                        json!(rating * 5 / 100)
                    } else {
                        json!(f64::from(i32::try_from(rating).unwrap_or(0)) * 5.0 / 100.0)
                    };
                    json!({"value": value, "votes-count": count})
                }
                _ => json!({"value": null, "votes-count": 0}),
            };
            out.insert("rating".into(), rating);
        }
        if self.inc.user_ratings {
            out.insert("user-rating".into(), json!({"value": null}));
        }
        Ok(())
    }

    /// The artists of an artist credit.
    fn credit_artists(&self, ac: i64) -> R<HashSet<i64>> {
        self.stmt(q::ARTIST_CREDIT)?
            .query_map([ac], |r| r.get(0))?
            .collect()
    }

    /// An artist credit, as `artist-credit`.
    pub fn artist_credit(&self, ac: i64, o: Opts) -> R<Value> {
        let names: Vec<(i64, String, Option<String>)> = self
            .stmt(q::ARTIST_CREDIT)?
            .query_map([ac], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<R<_>>()?;
        let o = o.not_toplevel();
        let mut out = Vec::with_capacity(names.len());
        for (artist, name, join) in names {
            out.push(json!({
                "name": name,
                "joinphrase": join.unwrap_or_default(),
                "artist": self.artist(artist, o)?,
            }));
        }
        Ok(Value::Array(out))
    }

    // Entity types

    pub fn area(&self, id: i64, o: Opts) -> R<Value> {
        let (gid, name, period, type_id, comment): (
            String,
            String,
            Period,
            Option<i64>,
            Option<String>,
        ) = self.stmt(q::AREA)?.query_row([id], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                Period::from_row(r, 2)?,
                r.get(9)?,
                r.get(10)?,
            ))
        })?;
        let mut out = Obj::new();
        out.insert("name".into(), Value::String(name.clone()));
        out.insert("sort-name".into(), Value::String(name));
        out.insert(
            "disambiguation".into(),
            Value::String(comment.unwrap_or_default()),
        );
        for (key, sql) in [
            ("iso-3166-1-codes", q::AREA_ISO1),
            ("iso-3166-2-codes", q::AREA_ISO2),
            ("iso-3166-3-codes", q::AREA_ISO3),
        ] {
            let codes = self.strings(sql, id)?;
            if !codes.is_empty() {
                out.insert(key.into(), json!(codes));
            }
        }
        let type_id = if o.area_type || o.toplevel {
            type_id
        } else {
            None
        };
        self.finish(
            &mut out,
            Common {
                kind: Kind::Area,
                id,
                gid,
                period: Some(period),
                type_: Some(("area_type", type_id)),
            },
            o,
        )?;
        Ok(Value::Object(out))
    }

    fn area_or_null(&self, id: Option<i64>) -> R<Value> {
        match id {
            Some(id) => self.area(id, Opts::bare()),
            None => Ok(Value::Null),
        }
    }

    /// An area's own ISO 3166-1 code.
    fn country_code(&self, area: Option<i64>) -> R<Option<String>> {
        let Some(area) = area else { return Ok(None) };
        Ok(self.strings(q::AREA_ISO1, area)?.into_iter().next())
    }

    pub fn artist(&self, id: i64, o: Opts) -> R<Value> {
        type Row = (
            String,
            String,
            String,
            Period,
            Option<i64>,
            Option<i64>,
            Option<i64>,
            Option<String>,
        );
        let (gid, name, sort_name, period, type_id, area, gender, comment): Row =
            self.stmt(q::ARTIST)?.query_row([id], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    Period::from_row(r, 3)?,
                    r.get(10)?,
                    r.get(11)?,
                    r.get(12)?,
                    r.get(13)?,
                ))
            })?;
        let mut out = Obj::new();
        out.insert("name".into(), Value::String(name));
        out.insert("sort-name".into(), Value::String(sort_name));
        out.insert(
            "disambiguation".into(),
            Value::String(comment.unwrap_or_default()),
        );
        let country: Option<String> = match area {
            Some(area) => self
                .stmt(q::AREA_COUNTRY)?
                .query_row([area], |r| r.get(0))
                .optional()?,
            None => None,
        };
        out.insert("country".into(), text(country));
        if o.toplevel {
            let (begin_area, end_area): (Option<i64>, Option<i64>) = self
                .stmt(q::ARTIST)?
                .query_row([id], |r| Ok((r.get(14)?, r.get(15)?)))?;
            self.named(&mut out, "gender", "gender", gender);
            out.insert("area".into(), self.area_or_null(area)?);
            out.insert("begin-area".into(), self.area_or_null(begin_area)?);
            out.insert("end-area".into(), self.area_or_null(end_area)?);
            self.artist_lists(&mut out, id)?;
        }
        self.finish(
            &mut out,
            Common {
                kind: Kind::Artist,
                id,
                gid,
                period: Some(period),
                type_: Some(("artist_type", type_id)),
            },
            o,
        )?;
        Ok(Value::Object(out))
    }

    /// The related lists of an artist lookup (`inc=recordings`, `releases`, `release-groups`, `works`).
    fn artist_lists(&self, out: &mut Obj, id: i64) -> R<()> {
        use super::browse::{self, Linked};
        let inc = self.inc;
        let list = Opts::nested();
        let page = browse::Page {
            limit: MAX_ITEMS,
            offset: 0,
        };
        if inc.recordings {
            let (ids, _) = browse::ids(self, Kind::Recording, Linked::Artist(id), page)?;
            out.insert("recordings".into(), self.list(Kind::Recording, &ids, list)?);
        }
        if inc.releases {
            let linked = if inc.various_artists {
                Linked::TrackArtist(id)
            } else {
                Linked::Artist(id)
            };
            let (ids, _) = browse::ids(self, Kind::Release, linked, page)?;
            out.insert("releases".into(), self.list(Kind::Release, &ids, list)?);
        }
        if inc.release_groups {
            let (ids, _) = browse::ids(self, Kind::ReleaseGroup, Linked::Artist(id), page)?;
            out.insert(
                "release-groups".into(),
                self.list(Kind::ReleaseGroup, &ids, list)?,
            );
        }
        if inc.works {
            let (ids, _) = browse::ids(self, Kind::Work, Linked::Artist(id), page)?;
            out.insert("works".into(), self.list(Kind::Work, &ids, list)?);
        }
        Ok(())
    }

    /// Entities in the order given.
    pub fn list(&self, kind: Kind, ids: &[i64], o: Opts) -> R<Value> {
        ids.iter()
            .map(|id| self.entity(kind, *id, o))
            .collect::<R<Vec<_>>>()
            .map(Value::Array)
    }

    pub fn event(&self, id: i64, o: Opts) -> R<Value> {
        type Row = (
            String,
            String,
            Period,
            Option<i64>,
            Option<String>,
            Option<bool>,
            Option<String>,
            Option<String>,
        );
        let (gid, name, period, type_id, time, cancelled, setlist, comment): Row =
            self.stmt(q::EVENT)?.query_row([id], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    Period::from_row(r, 2)?,
                    r.get(9)?,
                    r.get(10)?,
                    r.get(11)?,
                    r.get(12)?,
                    r.get(13)?,
                ))
            })?;
        let mut out = Obj::new();
        out.insert("name".into(), Value::String(name));
        out.insert(
            "disambiguation".into(),
            Value::String(comment.unwrap_or_default()),
        );
        // HH:MM, without the seconds
        let time = time
            .map(|t| t.get(..5).unwrap_or(&t).to_owned())
            .unwrap_or_default();
        out.insert("time".into(), Value::String(time));
        out.insert("setlist".into(), Value::String(setlist.unwrap_or_default()));
        out.insert("cancelled".into(), Value::Bool(cancelled.unwrap_or(false)));
        self.finish(
            &mut out,
            Common {
                kind: Kind::Event,
                id,
                gid,
                period: Some(period),
                type_: Some(("event_type", type_id)),
            },
            o,
        )?;
        Ok(Value::Object(out))
    }

    pub fn genre(&self, id: i64, o: Opts) -> R<Value> {
        let (gid, name, comment): (String, String, Option<String>) = self
            .stmt(q::GENRE)?
            .query_row([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        let mut out = Obj::new();
        out.insert("name".into(), Value::String(name));
        out.insert(
            "disambiguation".into(),
            Value::String(comment.unwrap_or_default()),
        );
        self.finish(
            &mut out,
            Common {
                kind: Kind::Genre,
                id,
                gid,
                period: None,
                type_: None,
            },
            o,
        )?;
        Ok(Value::Object(out))
    }

    pub fn instrument(&self, id: i64, o: Opts) -> R<Value> {
        let (gid, name, type_id, comment, description): (
            String,
            String,
            Option<i64>,
            Option<String>,
            Option<String>,
        ) = self.stmt(q::INSTRUMENT)?.query_row([id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?;
        let mut out = Obj::new();
        out.insert("name".into(), Value::String(name));
        out.insert(
            "disambiguation".into(),
            Value::String(comment.unwrap_or_default()),
        );
        out.insert(
            "description".into(),
            Value::String(description.unwrap_or_default()),
        );
        self.finish(
            &mut out,
            Common {
                kind: Kind::Instrument,
                id,
                gid,
                period: None,
                type_: Some(("instrument_type", type_id)),
            },
            o,
        )?;
        Ok(Value::Object(out))
    }

    pub fn label(&self, id: i64, o: Opts) -> R<Value> {
        type Row = (
            String,
            String,
            Period,
            Option<i64>,
            Option<i64>,
            Option<i64>,
            Option<String>,
        );
        let (gid, name, period, type_id, label_code, area, comment): Row =
            self.stmt(q::LABEL)?.query_row([id], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    Period::from_row(r, 2)?,
                    r.get(9)?,
                    r.get(10)?,
                    r.get(11)?,
                    r.get(12)?,
                ))
            })?;
        let mut out = Obj::new();
        out.insert("name".into(), Value::String(name.clone()));
        out.insert("sort-name".into(), Value::String(name));
        out.insert("label-code".into(), int(label_code));
        out.insert(
            "disambiguation".into(),
            Value::String(comment.unwrap_or_default()),
        );
        if o.toplevel {
            out.insert("country".into(), text(self.country_code(area)?));
            out.insert("area".into(), self.area_or_null(area)?);
            if self.inc.releases {
                use super::browse::{self, Linked};
                let page = browse::Page {
                    limit: MAX_ITEMS,
                    offset: 0,
                };
                let (ids, _) = browse::ids(self, Kind::Release, Linked::Label(id), page)?;
                out.insert(
                    "releases".into(),
                    self.list(Kind::Release, &ids, Opts::nested())?,
                );
            }
        }
        self.finish(
            &mut out,
            Common {
                kind: Kind::Label,
                id,
                gid,
                period: Some(period),
                type_: Some(("label_type", type_id)),
            },
            o,
        )?;
        Ok(Value::Object(out))
    }

    pub fn place(&self, id: i64, o: Opts) -> R<Value> {
        type Row = (
            String,
            String,
            Period,
            Option<i64>,
            Option<String>,
            Option<i64>,
            Option<String>,
            Option<String>,
        );
        let (gid, name, period, type_id, address, area, coordinates, comment): Row =
            self.stmt(q::PLACE)?.query_row([id], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    Period::from_row(r, 2)?,
                    r.get(9)?,
                    r.get(10)?,
                    r.get(11)?,
                    r.get(12)?,
                    r.get(13)?,
                ))
            })?;
        let mut out = Obj::new();
        out.insert("name".into(), Value::String(name));
        out.insert(
            "disambiguation".into(),
            Value::String(comment.unwrap_or_default()),
        );
        out.insert("address".into(), Value::String(address.unwrap_or_default()));
        // A place that is a relationship target has its area's type loaded, one at the top level not.
        let area = match area {
            Some(area) => self.area(
                area,
                Opts {
                    area_type: !o.toplevel,
                    ..Opts::bare()
                },
            )?,
            None => Value::Null,
        };
        out.insert("area".into(), area);
        out.insert(
            "coordinates".into(),
            coordinates
                .as_deref()
                .and_then(parse_point)
                .unwrap_or(Value::Null),
        );
        self.finish(
            &mut out,
            Common {
                kind: Kind::Place,
                id,
                gid,
                period: Some(period),
                type_: Some(("place_type", type_id)),
            },
            o,
        )?;
        Ok(Value::Object(out))
    }

    pub fn recording(&self, id: i64, o: Opts) -> R<Value> {
        self.recording_with(id, o, None)
    }

    /// A recording; `releases` replaces the list of releases a lookup has (`inc=releases`).
    fn recording_with(&self, id: i64, o: Opts, releases: Option<Value>) -> R<Value> {
        type Row = (
            String,
            String,
            i64,
            Option<i64>,
            Option<String>,
            Option<bool>,
        );
        let (gid, name, ac, length, comment, video): Row =
            self.stmt(q::RECORDING)?.query_row([id], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            })?;
        let inc = self.inc;
        let mut out = Obj::new();
        out.insert("title".into(), Value::String(name));
        out.insert(
            "disambiguation".into(),
            Value::String(comment.unwrap_or_default()),
        );
        out.insert("length".into(), int(length));
        out.insert("video".into(), Value::Bool(video.unwrap_or(false)));
        if o.with_inc && inc.artist_credits {
            out.insert("artist-credit".into(), self.artist_credit(ac, o)?);
        }
        if o.toplevel && inc.releases {
            let releases = match releases {
                Some(r) => r,
                None => self.recording_releases(id)?,
            };
            out.insert("releases".into(), releases);
        }
        if o.with_inc && inc.isrcs {
            out.insert("isrcs".into(), json!(self.strings(q::RECORDING_ISRCS, id)?));
        }
        if o.first_release_date {
            let first: Option<i64> = self
                .stmt(q::RECORDING_FIRST_RELEASE_DATE)?
                .query_row([id], |r| r.get(0))
                .optional()?;
            if let Some(first) = first {
                out.insert(
                    "first-release-date".into(),
                    Value::String(date::format_packed(first)),
                );
            }
        }
        self.finish(
            &mut out,
            Common {
                kind: Kind::Recording,
                id,
                gid,
                period: None,
                type_: None,
            },
            o,
        )?;
        Ok(Value::Object(out))
    }

    /// The releases of a recording lookup: with `inc=media`, each with only the medium and track of the recording.
    fn recording_releases(&self, id: i64) -> R<Value> {
        use super::browse::{self, Linked};
        let page = browse::Page {
            limit: MAX_ITEMS,
            offset: 0,
        };
        let (ids, _) = browse::ids(self, Kind::Release, Linked::Recording(id), page)?;
        let o = Opts::nested();
        let mut releases = Vec::with_capacity(ids.len());
        for release in ids {
            let mut value = self.release(release, Opts { ..o })?;
            if self.inc.media
                && let Value::Object(obj) = &mut value
            {
                let track: Option<(i64, i64)> = self
                    .stmt(q::RECORDING_TRACK_ON_RELEASE)?
                    .query_row((id, release), |r| Ok((r.get(0)?, r.get(1)?)))
                    .optional()?;
                let media = match track {
                    Some((medium, track)) => vec![self.medium(medium, o, Some(&[track]))?],
                    None => Vec::new(),
                };
                obj.insert("media".into(), Value::Array(media));
            }
            releases.push(value);
        }
        Ok(Value::Array(releases))
    }

    #[allow(clippy::too_many_lines)]
    pub fn release(&self, id: i64, o: Opts) -> R<Value> {
        type Row = (
            String,
            String,
            i64,
            i64,
            Option<i64>,
            Option<i64>,
            Option<i64>,
            Option<i64>,
            Option<String>,
            Option<String>,
            Option<i64>,
            Option<String>,
            Option<String>,
        );
        let row: Row = self.stmt(q::RELEASE)?.query_row([id], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
                r.get(8)?,
                r.get(9)?,
                r.get(10)?,
                r.get(11)?,
                r.get(12)?,
            ))
        })?;
        let (
            gid,
            name,
            ac,
            rg,
            status,
            packaging,
            language,
            script,
            barcode,
            comment,
            quality,
            asin,
            presence,
        ) = row;
        let inc = self.inc;
        let mut out = Obj::new();
        out.insert("title".into(), Value::String(name));

        let events: Vec<(Option<i64>, String)> = self
            .stmt(q::RELEASE_EVENTS)?
            .query_map([id], |r| {
                Ok((r.get(0)?, date::format(r.get(1)?, r.get(2)?, r.get(3)?)))
            })?
            .collect::<R<_>>()?;
        if let Some((country, first_date)) = events.first() {
            out.insert("date".into(), Value::String(first_date.clone()));
            out.insert("country".into(), text(self.country_code(*country)?));
            let mut list = Vec::with_capacity(events.len());
            for (country, date) in &events {
                list.push(json!({"date": date, "area": self.area_or_null(*country)?}));
            }
            out.insert("release-events".into(), Value::Array(list));
        }
        if o.toplevel {
            out.insert("asin".into(), text(asin));
        }
        out.insert("barcode".into(), text(barcode));
        out.insert(
            "disambiguation".into(),
            Value::String(comment.unwrap_or_default()),
        );
        self.named(&mut out, "status", "release_status", status);
        let quality = match quality.unwrap_or(-1) {
            0 => "low",
            2 => "high",
            _ => "normal",
        };
        out.insert("quality".into(), Value::String(quality.into()));
        self.named(&mut out, "packaging", "release_packaging", packaging);
        if o.toplevel {
            let (count, front, back) = if presence.as_deref() == Some("present") {
                self.stmt(q::COVER_ART_STATS)?
                    .query_row([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            } else {
                (0i64, false, false)
            };
            out.insert(
                "cover-art-archive".into(),
                json!({
                    "artwork": presence.as_deref() == Some("present"),
                    "darkened": presence.as_deref() == Some("darkened"),
                    "count": count,
                    "front": front,
                    "back": back,
                }),
            );
        }
        out.insert(
            "text-representation".into(),
            json!({
                "script": script.and_then(|s| self.lk.scripts.get(&s)),
                "language": language.and_then(|l| self.lk.languages.get(&l)),
            }),
        );
        if o.with_inc && (inc.collections || inc.user_collections) {
            out.insert("collections".into(), json!([]));
        }
        let in_release = Opts {
            sublevel_ratings: true,
            release_ac: Some(ac),
            ..o.not_toplevel()
        };
        if o.with_inc && inc.release_groups {
            let rels = if o.toplevel && inc.release_group_level_rels {
                Rels::All
            } else {
                Rels::None
            };
            out.insert(
                "release-group".into(),
                self.release_group(rg, Opts { rels, ..in_release })?,
            );
        }
        if o.with_inc && inc.artist_credits {
            out.insert("artist-credit".into(), self.artist_credit(ac, o)?);
        }
        if o.toplevel && inc.labels {
            let labels: Vec<(Option<i64>, Option<String>)> = self
                .stmt(q::RELEASE_LABELS)?
                .query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<R<_>>()?;
            let mut list = Vec::with_capacity(labels.len());
            for (label, catno) in labels {
                let label = match label {
                    Some(l) => self.label(l, o.not_toplevel())?,
                    None => Value::Null,
                };
                list.push(json!({"catalog-number": catno, "label": label}));
            }
            out.insert("label-info".into(), Value::Array(list));
        }
        if o.with_inc && (inc.media || inc.discids || inc.recordings) {
            let mediums: Vec<i64> = self
                .stmt(q::RELEASE_MEDIUMS)?
                .query_map([id], |r| r.get(0))?
                .collect::<R<_>>()?;
            let mut list = Vec::with_capacity(mediums.len());
            for medium in mediums {
                list.push(self.medium(medium, in_release, None)?);
            }
            out.insert("media".into(), Value::Array(list));
        }
        self.finish(
            &mut out,
            Common {
                kind: Kind::Release,
                id,
                gid,
                period: None,
                type_: None,
            },
            o,
        )?;
        Ok(Value::Object(out))
    }

    /// A medium; `only` limits its tracks to these (a recording's releases with `inc=media`).
    #[allow(clippy::too_many_lines)]
    fn medium(&self, id: i64, o: Opts, only: Option<&[i64]>) -> R<Value> {
        type Row = (i64, Option<String>, i64, Option<i64>, Option<String>, i64);
        let (_, gid, position, format, name, _track_count): Row =
            self.stmt(q::MEDIUM)?.query_row([id], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            })?;
        let inc = self.inc;
        let mut out = Obj::new();
        out.insert("title".into(), Value::String(name.unwrap_or_default()));
        self.named(&mut out, "format", "medium_format", format);
        out.insert("position".into(), json!(position));
        if inc.discids {
            let mut stmt = self.stmt(q::MEDIUM_CDTOCS)?;
            let discs = stmt
                .query_map([id], |r| {
                    Ok(cdtoc_json(
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get::<_, Option<String>>(3)?.as_deref(),
                    ))
                })?
                .collect::<R<Vec<_>>>()?;
            out.insert("discs".into(), Value::Array(discs));
        }
        let cdtoc_count: i64 = self
            .stmt(q::MEDIUM_CDTOC_TRACK_COUNT)?
            .query_row([id], |r| r.get(0))?;
        out.insert("track-count".into(), json!(cdtoc_count));
        if let Some(gid) = gid {
            out.insert("id".into(), Value::String(gid));
        }

        let with_tracks = inc.recordings || only.is_some();
        if with_tracks {
            type Track = (
                i64,
                String,
                i64,
                i64,
                Option<String>,
                String,
                i64,
                Option<i64>,
                Option<bool>,
            );
            let mut tracks: Vec<Track> = self
                .stmt(q::MEDIUM_TRACKS)?
                .query_map([id], |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                        r.get(8)?,
                    ))
                })?
                .collect::<R<_>>()?;
            if let Some(only) = only {
                tracks.retain(|t| only.contains(&t.0));
            }
            let has_pregap: bool = self
                .stmt(q::MEDIUM_HAS_PREGAP)?
                .query_row([id], |r| r.get(0))?;
            let serialize = |t: &Track| -> R<Value> {
                let (_, gid, recording, position, number, name, ac, length, _) = t;
                let mut track = Obj::new();
                track.insert("id".into(), Value::String(gid.clone()));
                track.insert("length".into(), int(*length));
                track.insert("number".into(), text(number.clone()));
                track.insert("position".into(), json!(position));
                track.insert("title".into(), Value::String(name.clone()));
                if inc.recordings {
                    let rels = if inc.recording_level_rels {
                        Rels::All
                    } else {
                        Rels::None
                    };
                    let recording_opts = Opts {
                        track_ac: Some(*ac),
                        rels,
                        first_release_date: true,
                        ..o
                    };
                    track.insert(
                        "recording".into(),
                        self.recording(*recording, recording_opts)?,
                    );
                }
                if inc.artist_credits {
                    track.insert("artist-credit".into(), self.artist_credit(*ac, o)?);
                }
                Ok(Value::Object(track))
            };
            let min = tracks.first().map_or(0, |t| t.3);
            if has_pregap && let Some(first) = tracks.first().filter(|t| t.3 == 0) {
                out.insert("pregap".into(), serialize(first)?);
            }
            let audio: Vec<&Track> = tracks
                .iter()
                .filter(|t| t.3 > 0 && !t.8.unwrap_or(false))
                .collect();
            if !audio.is_empty() {
                out.insert(
                    "tracks".into(),
                    audio
                        .into_iter()
                        .map(serialize)
                        .collect::<R<Vec<_>>>()?
                        .into(),
                );
                out.insert(
                    "track-offset".into(),
                    json!(if has_pregap { 0 } else { min - 1 }),
                );
            }
            let data: Vec<&Track> = tracks
                .iter()
                .filter(|t| t.3 > 0 && t.8.unwrap_or(false))
                .collect();
            if !data.is_empty() {
                out.insert(
                    "data-tracks".into(),
                    data.into_iter()
                        .map(serialize)
                        .collect::<R<Vec<_>>>()?
                        .into(),
                );
            }
        }
        Ok(Value::Object(out))
    }

    pub fn release_group(&self, id: i64, o: Opts) -> R<Value> {
        type Row = (String, String, i64, Option<i64>, Option<String>, String);
        let (gid, name, ac, type_id, comment, first): Row =
            self.stmt(q::RELEASE_GROUP)?.query_row([id], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    date::format(r.get(5)?, r.get(6)?, r.get(7)?),
                ))
            })?;
        let inc = self.inc;
        let mut out = Obj::new();
        out.insert("title".into(), Value::String(name));
        self.named(
            &mut out,
            "primary-type",
            "release_group_primary_type",
            type_id,
        );
        let secondary: Vec<i64> = self
            .stmt(q::RELEASE_GROUP_SECONDARY_TYPES)?
            .query_map([id], |r| r.get(0))?
            .collect::<R<_>>()?;
        let names: Vec<_> = secondary
            .iter()
            .filter_map(|t| self.lk.named("release_group_secondary_type", Some(*t)))
            .collect();
        out.insert(
            "secondary-types".into(),
            json!(names.iter().map(|n| &n.name).collect::<Vec<_>>()),
        );
        out.insert(
            "secondary-type-ids".into(),
            json!(names.iter().map(|n| &n.gid).collect::<Vec<_>>()),
        );
        out.insert("first-release-date".into(), Value::String(first));
        out.insert(
            "disambiguation".into(),
            Value::String(comment.unwrap_or_default()),
        );
        if o.with_inc && inc.artist_credits {
            out.insert("artist-credit".into(), self.artist_credit(ac, o)?);
        }
        // Only a release group of the request has its releases loaded; a nested one shows an empty list.
        if o.with_inc && inc.releases && !o.toplevel {
            out.insert("releases".into(), json!([]));
        } else if o.with_inc && inc.releases {
            use super::browse::{self, Linked};
            let page = browse::Page {
                limit: MAX_ITEMS,
                offset: 0,
            };
            let (ids, _) = browse::ids(self, Kind::Release, Linked::ReleaseGroup(id), page)?;
            out.insert(
                "releases".into(),
                self.list(Kind::Release, &ids, Opts::nested())?,
            );
        }
        self.finish(
            &mut out,
            Common {
                kind: Kind::ReleaseGroup,
                id,
                gid,
                period: None,
                type_: None,
            },
            o,
        )?;
        Ok(Value::Object(out))
    }

    pub fn series(&self, id: i64, o: Opts) -> R<Value> {
        let (gid, name, comment, type_id): (String, String, Option<String>, Option<i64>) = self
            .stmt(q::SERIES)?
            .query_row([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
        let mut out = Obj::new();
        out.insert("name".into(), Value::String(name));
        out.insert(
            "disambiguation".into(),
            Value::String(comment.unwrap_or_default()),
        );
        self.finish(
            &mut out,
            Common {
                kind: Kind::Series,
                id,
                gid,
                period: None,
                type_: Some(("series_type", type_id)),
            },
            o,
        )?;
        Ok(Value::Object(out))
    }

    pub fn url(&self, id: i64, o: Opts) -> R<Value> {
        let (gid, url): (String, String) = self
            .stmt(q::URL)?
            .query_row([id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        let mut out = Obj::new();
        out.insert("resource".into(), Value::String(url));
        self.finish(
            &mut out,
            Common {
                kind: Kind::Url,
                id,
                gid,
                period: None,
                type_: None,
            },
            o,
        )?;
        Ok(Value::Object(out))
    }

    pub fn work(&self, id: i64, o: Opts) -> R<Value> {
        let (gid, name, type_id, comment): (String, String, Option<i64>, Option<String>) = self
            .stmt(q::WORK)?
            .query_row([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
        let mut out = Obj::new();
        out.insert("title".into(), Value::String(name));
        out.insert(
            "disambiguation".into(),
            Value::String(comment.unwrap_or_default()),
        );
        let iswcs = if o.work_details {
            self.strings(q::WORK_ISWCS, id)?
        } else {
            Vec::new()
        };
        out.insert("iswcs".into(), json!(iswcs));
        // Only the works of the request have their attributes loaded.
        let mut attributes = Vec::new();
        if o.toplevel {
            let rows: Vec<(i64, Option<i64>, Option<String>)> = self
                .stmt(q::WORK_ATTRIBUTES)?
                .query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<R<_>>()?;
            for (type_id, allowed, free_text) in rows {
                let mut attribute = Obj::new();
                let allowed = allowed.and_then(|a| self.lk.work_attribute_values.get(&a));
                let value = allowed
                    .map(|a| a.0.clone())
                    .or(free_text)
                    .unwrap_or_default();
                attribute.insert("value".into(), Value::String(value));
                if let Some(gid) = allowed.and_then(|a| a.1.clone()) {
                    attribute.insert("value-id".into(), Value::String(gid));
                }
                self.named(&mut attribute, "type", "work_attribute_type", Some(type_id));
                attributes.push(Value::Object(attribute));
            }
        }
        out.insert("attributes".into(), Value::Array(attributes));
        let languages: Vec<String> = if o.work_details {
            self.stmt(q::WORK_LANGUAGES)?
                .query_map([id], |r| r.get::<_, i64>(0))?
                .collect::<R<Vec<_>>>()?
                .into_iter()
                .filter_map(|l| self.lk.languages.get(&l).cloned())
                .collect()
        } else {
            Vec::new()
        };
        let language = match languages.as_slice() {
            [] => Value::Null,
            [one] => Value::String(one.clone()),
            _ => Value::String("mul".into()),
        };
        out.insert("languages".into(), json!(languages));
        out.insert("language".into(), language);
        self.finish(
            &mut out,
            Common {
                kind: Kind::Work,
                id,
                gid,
                period: None,
                type_: Some(("work_type", type_id)),
            },
            o,
        )?;
        Ok(Value::Object(out))
    }

    // Relationships

    #[allow(clippy::too_many_lines)]
    fn relations(&self, kind: Kind, id: i64, cardinal: bool) -> R<Vec<Value>> {
        let mut out = Vec::new();
        for target in self.inc.rels.iter().copied() {
            let mut rels = Vec::new();
            for (sql, backward, applies) in [
                (q::RELS_FORWARD, false, kind.table() <= target.table()),
                (q::RELS_BACKWARD, true, kind.table() >= target.table()),
            ] {
                if !applies {
                    continue;
                }
                let mut stmt = self.stmt(sql)?;
                let rows = stmt.query_map((kind.code(), id, target.code()), |r| {
                    Ok(Rel {
                        rowid: r.get(0)?,
                        target: r.get(1)?,
                        link: r.get(2)?,
                        link_order: r.get(3)?,
                        source_credit: r.get(4)?,
                        target_credit: r.get(5)?,
                        link_type: r.get(6)?,
                        begin: (r.get(7)?, r.get(8)?, r.get(9)?),
                        end: (r.get(10)?, r.get(11)?, r.get(12)?),
                        ended: r.get::<_, Option<bool>>(13)?.unwrap_or(false),
                        backward,
                    })
                })?;
                for rel in rows {
                    let rel = rel?;
                    let link_type = self.lk.link_types.get(&rel.link_type);
                    // From a work reached through work-level-rels, only relationships where the work's side is
                    // "one" (so not all its recordings).
                    if cardinal {
                        let cardinality = link_type.map_or(0, |t| {
                            if rel.backward {
                                t.cardinality1
                            } else {
                                t.cardinality0
                            }
                        });
                        if cardinality != 0 {
                            continue;
                        }
                    }
                    rels.push(rel);
                }
            }
            if rels.is_empty() {
                continue;
            }
            // Serialized targets, with the name they sort by.
            let target_opts = Opts {
                aliases: false,
                tags: false,
                rels: if target == Kind::Work && self.inc.work_level_rels && !cardinal {
                    Rels::Cardinal
                } else {
                    Rels::None
                },
                area_type: true,
                work_details: !cardinal,
                ..Opts::nested()
            };
            let mut items = Vec::with_capacity(rels.len());
            for rel in rels {
                let value = self.entity(target, rel.target, target_opts)?;
                let sort_name = value
                    .get("sort-name")
                    .or_else(|| value.get("resource"))
                    .or_else(|| value.get("name"))
                    .or_else(|| value.get("title"))
                    .and_then(Value::as_str)
                    .map(collation_key)
                    .unwrap_or_default();
                items.push((rel, value, sort_name));
            }
            let nulls_last = |d: (Option<i64>, Option<i64>, Option<i64>)| {
                (
                    d.0.unwrap_or(i64::MAX),
                    d.1.unwrap_or(i64::MAX),
                    d.2.unwrap_or(i64::MAX),
                )
            };
            // As musicbrainz-server's query orders them: link type name (its database collation ignores case),
            // dates, then the name and id of the entity it joins as the target. For relationships between two
            // entities of the same type that is entity0 (the query's join ends up on it), so forward ones keep
            // their row order.
            let source_key = if kind == target {
                Some(self.sort_key(kind, id)?)
            } else {
                None
            };
            items.sort_by(|(a, _, an), (b, _, bn)| {
                let name = |r: &Rel| {
                    self.lk
                        .link_types
                        .get(&r.link_type)
                        .map(|t| t.name.to_lowercase())
                        .unwrap_or_default()
                };
                let joined = |r: &Rel, n: &'_ String| -> (String, i64) {
                    match &source_key {
                        Some(source) if !r.backward => (source.clone(), id),
                        _ => (n.clone(), r.target),
                    }
                };
                name(a)
                    .cmp(&name(b))
                    .then(nulls_last(a.begin).cmp(&nulls_last(b.begin)))
                    .then(nulls_last(a.end).cmp(&nulls_last(b.end)))
                    .then(a.ended.cmp(&b.ended))
                    .then(joined(a, an).cmp(&joined(b, bn)))
                    .then(a.rowid.cmp(&b.rowid))
            });
            for (rel, value, _) in items {
                out.push(self.relation(target, &rel, value)?);
            }
        }
        Ok(out)
    }

    /// What relationships to an entity sort by: its sort name, URL or name, lowercased.
    fn sort_key(&self, kind: Kind, id: i64) -> R<String> {
        let name: Option<String> = self
            .stmt(&q::sort_key(kind))?
            .query_row([id], |r| r.get(0))
            .optional()?;
        Ok(collation_key(&name.unwrap_or_default()))
    }

    fn relation(&self, target: Kind, rel: &Rel, value: Value) -> R<Value> {
        let mut out = Obj::new();
        let link_type = self.lk.link_types.get(&rel.link_type);
        out.insert(
            "type".into(),
            link_type.map_or(Value::Null, |t| Value::String(t.name.clone())),
        );
        out.insert(
            "type-id".into(),
            link_type
                .and_then(|t| t.gid.clone())
                .map_or(Value::Null, Value::String),
        );
        out.insert(
            "direction".into(),
            Value::String(if rel.backward { "backward" } else { "forward" }.into()),
        );
        if let Some(order) = rel.link_order.filter(|o| *o != 0) {
            out.insert("ordering-key".into(), json!(order));
        }
        out.insert(
            "begin".into(),
            opt_str(&date::format(rel.begin.0, rel.begin.1, rel.begin.2)),
        );
        out.insert(
            "end".into(),
            opt_str(&date::format(rel.end.0, rel.end.1, rel.end.2)),
        );
        out.insert("ended".into(), Value::Bool(rel.ended));

        let mut attributes: Vec<(String, Option<String>, Option<String>, Option<String>, bool)> =
            self.stmt(q::LINK_ATTRIBUTES)?
                .query_map([rel.link], |r| {
                    let type_id: i64 = r.get(0)?;
                    let t = self.lk.attribute_types.get(&type_id);
                    Ok((
                        t.map(|t| t.name.clone()).unwrap_or_default(),
                        t.and_then(|t| t.gid.clone()),
                        r.get(1)?,
                        r.get(2)?,
                        t.is_some_and(|t| t.creditable),
                    ))
                })?
                .collect::<R<_>>()?;
        attributes.sort_by(|a, b| a.0.cmp(&b.0));
        out.insert(
            "attributes".into(),
            json!(attributes.iter().map(|a| &a.0).collect::<Vec<_>>()),
        );
        let values: Obj = attributes
            .iter()
            .filter_map(|a| {
                a.2.as_ref()
                    .filter(|v| !v.is_empty())
                    .map(|v| (a.0.clone(), Value::String(v.clone())))
            })
            .collect();
        out.insert("attribute-values".into(), Value::Object(values));
        let ids: Obj = attributes
            .iter()
            .map(|a| (a.0.clone(), a.1.clone().map_or(Value::Null, Value::String)))
            .collect();
        out.insert("attribute-ids".into(), Value::Object(ids));
        if attributes.iter().any(|a| a.4) {
            let credits: Obj = attributes
                .iter()
                .filter_map(|a| {
                    a.3.as_ref()
                        .filter(|v| !v.is_empty())
                        .map(|v| (a.0.clone(), Value::String(v.clone())))
                })
                .collect();
            out.insert("attribute-credits".into(), Value::Object(credits));
        }
        out.insert("target-type".into(), Value::String(target.table().into()));
        out.insert(target.table().into(), value);
        out.insert(
            "source-credit".into(),
            Value::String(rel.source_credit.clone().unwrap_or_default()),
        );
        out.insert(
            "target-credit".into(),
            Value::String(rel.target_credit.clone().unwrap_or_default()),
        );
        Ok(Value::Object(out))
    }
}

/// A relationship of the entity being serialized, as read.
struct Rel {
    rowid: i64,
    target: i64,
    link: i64,
    link_order: Option<i64>,
    source_credit: Option<String>,
    target_credit: Option<String>,
    link_type: i64,
    begin: (Option<i64>, Option<i64>, Option<i64>),
    end: (Option<i64>, Option<i64>, Option<i64>),
    ended: bool,
    backward: bool,
}

/// A sort key close to musicbrainz-server's ICU collation: case only decides between otherwise equal strings, and
/// then lowercase comes first.
fn collation_key(s: &str) -> String {
    let swapped: String = s
        .chars()
        .map(|c| {
            if c.is_uppercase() {
                c.to_lowercase().next().unwrap_or(c)
            } else {
                c.to_uppercase().next().unwrap_or(c)
            }
        })
        .collect();
    format!("{}\u{0}{swapped}", s.to_lowercase())
}

/// A place's coordinates, `(latitude,longitude)` in the dump.
fn parse_point(point: &str) -> Option<Value> {
    let (lat, long) = point
        .trim_matches(|c| c == '(' || c == ')')
        .split_once(',')?;
    Some(
        json!({"latitude": lat.trim().parse::<f64>().ok()?, "longitude": long.trim().parse::<f64>().ok()?}),
    )
}

/// A disc ID's table of contents.
pub fn cdtoc_json(
    discid: String,
    track_count: Option<i64>,
    leadout: Option<i64>,
    offsets: Option<&str>,
) -> Value {
    let offsets = offsets
        .map(crate::import::copy::parse_int_array)
        .unwrap_or_default();
    let mut out = Obj::new();
    out.insert("id".into(), Value::String(discid));
    out.insert("offset-count".into(), int(track_count));
    out.insert("sectors".into(), int(leadout));
    if !offsets.is_empty() {
        out.insert("offsets".into(), json!(offsets));
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn points() {
        assert_eq!(
            parse_point("(48.88389,2.30889)"),
            Some(json!({"latitude": 48.88389, "longitude": 2.30889}))
        );
        assert_eq!(parse_point("x"), None);
    }
}
