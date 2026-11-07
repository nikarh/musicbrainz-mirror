//! Entities as the web service's JSON. This follows musicbrainz-server's JSON serializer
//! (`WebService::Serializer::JSON::2::*`) and what its controllers load before serializing: which fields appear
//! depends on the entity type, `inc`, and where in the response an entity is (top level, in a list, in an artist
//! credit, as a relationship target).

use serde_json::{Map, Value, json};

use super::inc::Inc;
use crate::date;
use crate::kind::Kind;
use crate::store::build::SAME_AS_RECORDING;
use crate::store::{Row, Store};

pub type Obj = Map<String, Value>;

/// A row an entity refers to is missing: the store is inconsistent.
#[derive(Debug, thiserror::Error)]
#[error("{0} {1} is missing")]
pub struct Missing(pub &'static str, pub u32);

pub type R<T> = Result<T, Missing>;
/// How aliases sort: (sort key, locale, name).
type AliasKey = (String, Option<String>, String);
/// A relationship attribute: name, MBID, credited as, text value, whether it is creditable.
type Attribute = (String, Option<String>, Option<String>, Option<String>, bool);

/// Most lists of related entities in a lookup are cut at this many (musicbrainz-server's `$MAX_ITEMS`).
pub const MAX_ITEMS: usize = 25;

/// Release filters of browse requests and of lists in lookups (`status=`, `type=`).
#[derive(Debug, Default, Clone)]
pub struct Filter {
    pub status: Vec<u32>,
    pub primary_types: Vec<u32>,
    pub secondary_types: Vec<u32>,
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
    pub release_ac: Option<u32>,
    pub track_ac: Option<u32>,
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
    pub store: &'a Store,
    pub inc: &'a Inc,
    pub filter: &'a Filter,
    /// The answer is XML: musicbrainz-server's XML serializer shows what was loaded, without the JSON
    /// serializer's rules for leaving out repeated aliases and tags, and ratings wherever they were loaded.
    pub xml: bool,
}

/// An empty string as null (musicbrainz-server's `format || JSON::null`).
fn opt_str(s: &str) -> Value {
    if s.is_empty() {
        Value::Null
    } else {
        Value::String(s.to_owned())
    }
}

fn text(v: Option<&str>) -> Value {
    v.map_or(Value::Null, |s| Value::String(s.to_owned()))
}

fn int(v: Option<u32>) -> Value {
    v.map_or(Value::Null, Value::from)
}

fn period(row: &Row<'_>, into: &mut Obj) {
    let (by, bm, bd) = row.date("begin");
    let (ey, em, ed) = row.date("end");
    into.insert("begin".into(), opt_str(&date::format(by, bm, bd)));
    into.insert("end".into(), opt_str(&date::format(ey, em, ed)));
    into.insert("ended".into(), Value::Bool(row.bool("ended")));
}

/// A sort key close to musicbrainz-server's ICU collation: case only decides between otherwise equal strings, and
/// then lowercase comes first.
#[must_use]
pub fn collation_key(s: &str) -> String {
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

/// Common to every entity: what musicbrainz-server's `serialize_entity` adds to the type's own serializer.
struct Common<'a> {
    kind: Kind,
    id: u32,
    gid: String,
    /// The row has begin, end and ended.
    period: Option<Row<'a>>,
    /// (lookup table, id) of the entity's type, for entity types with a "simple" type.
    type_: Option<(&'a str, Option<u32>)>,
}

/// A relationship of the entity being serialized.
struct Rel<'a> {
    row: Row<'a>,
    target: u32,
    link: Row<'a>,
    backward: bool,
}

impl<'a> Ctx<'a> {
    fn get(&self, table: &'static str, id: u32) -> R<Row<'a>> {
        self.store.t(table).get(id).ok_or(Missing(table, id))
    }

    fn named(&self, into: &mut Obj, key: &str, table: &str, id: Option<u32>) {
        let named = self.store.named(table, id);
        into.insert(
            key.into(),
            named
                .as_ref()
                .map_or(Value::Null, |n| Value::String(n.0.to_owned())),
        );
        into.insert(
            format!("{key}-id"),
            named.and_then(|n| n.1).map_or(Value::Null, Value::String),
        );
    }

    /// An entity of any type.
    pub fn entity(&self, kind: Kind, id: u32, o: Opts) -> R<Value> {
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

    /// Entities in the order given.
    pub fn list(&self, kind: Kind, ids: &[u32], o: Opts) -> R<Value> {
        ids.iter()
            .map(|id| self.entity(kind, *id, o))
            .collect::<R<Vec<_>>>()
            .map(Value::Array)
    }

    fn finish(&self, out: &mut Obj, c: Common<'_>, o: Opts) -> R<()> {
        let inc = self.inc;
        let artist_in_credits = || {
            c.kind == Kind::Artist
                && [o.release_ac, o.track_ac]
                    .into_iter()
                    .flatten()
                    .any(|ac| self.credit_artists(ac).contains(&c.id))
        };
        if c.kind.has_aliases() && o.with_inc && o.aliases && inc.aliases {
            // Recording artists repeated from the release's or track's credit don't repeat their aliases.
            let repeated =
                !self.xml && o.release_ac.is_some() && o.track_ac.is_some() && artist_in_credits();
            if !repeated {
                out.insert("aliases".into(), self.aliases(c.kind, c.id));
            }
        }
        if c.kind.has_annotations() && o.toplevel && o.with_inc && inc.annotation {
            let text = self
                .store
                .derived
                .annotation(c.kind, c.id)
                .and_then(|a| self.store.t("annotation").get(a))
                .and_then(|a| a.str("text").map(str::to_owned));
            out.insert("annotation".into(), text.map_or(Value::Null, Value::String));
        }
        out.insert("id".into(), Value::String(c.gid));
        if o.toplevel && matches!(c.kind, Kind::Artist | Kind::Label) {
            let t = c.kind.table();
            for (key, table, field) in [
                ("ipis", format!("{t}_ipi"), "ipi"),
                ("isnis", format!("{t}_isni"), "isni"),
            ] {
                let mut codes: Vec<&str> = self
                    .store
                    .t(&table)
                    .by(t, c.id)
                    .filter_map(|r| r.str(field))
                    .collect();
                codes.sort_unstable();
                out.insert(key.into(), json!(codes));
            }
        }
        if let Some(row) = &c.period
            && (o.toplevel || c.kind == Kind::Event)
        {
            let mut span = Obj::new();
            period(row, &mut span);
            out.insert("life-span".into(), Value::Object(span));
        }
        if c.kind.has_ratings()
            && (o.toplevel || o.sublevel_ratings || self.xml && o.tags)
            && o.with_inc
            && (inc.ratings || inc.user_ratings)
        {
            self.rating(out, c.kind, c.id);
        }
        if o.with_inc && inc.has_rels() && o.rels != Rels::None {
            let rels = self.relations(c.kind, c.id, o.rels == Rels::Cardinal)?;
            out.insert("relations".into(), Value::Array(rels));
        }
        if c.kind.has_tags()
            && o.with_inc
            && o.tags
            && inc.tags_or_genres()
            && (self.xml || !artist_in_credits())
        {
            self.tags(out, c.kind, c.id);
        }
        if let Some((table, type_id)) = c.type_ {
            self.named(out, "type", table, type_id);
        }
        Ok(())
    }

    fn aliases(&self, kind: Kind, id: u32) -> Value {
        let mut aliases: Vec<(AliasKey, Obj)> = self
            .store
            .t(&format!("{}_alias", kind.table()))
            .by("entity", id)
            .map(|r| {
                let name = r.text("name");
                let locale = r.str("locale");
                let mut a = Obj::new();
                a.insert("name".into(), Value::String(name.clone()));
                a.insert("sort-name".into(), Value::String(r.text("sort_name")));
                a.insert(
                    "primary".into(),
                    if locale.is_some() {
                        Value::Bool(r.bool("primary_for_locale"))
                    } else {
                        Value::Null
                    },
                );
                a.insert("locale".into(), text(locale));
                let type_ = self.store.alias_type(kind, r.u32("type"));
                a.insert(
                    "type".into(),
                    type_
                        .as_ref()
                        .map_or(Value::Null, |t| Value::String(t.0.to_owned())),
                );
                a.insert(
                    "type-id".into(),
                    type_.and_then(|t| t.1).map_or(Value::Null, Value::String),
                );
                period(&r, &mut a);
                ((name, locale.map(str::to_owned), r.text("sort_name")), a)
            })
            .collect();
        aliases.sort_by(|a, b| a.0.cmp(&b.0));
        Value::Array(aliases.into_iter().map(|(_, a)| Value::Object(a)).collect())
    }

    fn tags(&self, out: &mut Obj, kind: Kind, id: u32) {
        let inc = self.inc;
        let rows: Vec<(String, u32, Option<u32>)> = self
            .store
            .t(&format!("{}_tag", kind.table()))
            .by("entity", id)
            .filter_map(|r| {
                let count = r.u32("count").filter(|c| *c > 0 && *c < 0x8000_0000)?;
                let tag = r.u32("tag")?;
                let name = self.store.t("tag").get(tag)?.text("name");
                Some((name, count, self.store.derived.tag_genre(tag)))
            })
            .collect();
        if inc.tags {
            let mut tags: Vec<(&str, u32)> =
                rows.iter().map(|(n, c, _)| (n.as_str(), *c)).collect();
            tags.sort_unstable();
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
            let mut genres: Vec<(String, String, String, u32)> = rows
                .iter()
                .filter_map(|(_, count, genre)| {
                    let g = self.store.t("genre").get((*genre)?)?;
                    Some((g.text("name"), g.gid(), g.text("comment"), *count))
                })
                .collect();
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
    }

    fn rating(&self, out: &mut Obj, kind: Kind, id: u32) {
        if self.inc.ratings {
            let table = if kind == Kind::ReleaseGroup {
                "release_group_meta".to_owned()
            } else {
                format!("{}_meta", kind.table())
            };
            let row = self.store.t(&table).get(id);
            let rating = match row.map(|r| (r.u32("rating"), r.u32("rating_count"))) {
                Some((Some(rating), Some(count))) if count > 0 => {
                    // Stored out of 100, shown out of 5: 80 is 4, 76 is 3.8.
                    let value = if rating * 5 % 100 == 0 {
                        json!(rating * 5 / 100)
                    } else {
                        json!(f64::from(rating) * 5.0 / 100.0)
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
    }

    /// The artists of an artist credit.
    fn credit_artists(&self, ac: u32) -> Vec<u32> {
        self.store
            .t("artist_credit_name")
            .by("artist_credit", ac)
            .filter_map(|r| r.u32("artist"))
            .collect()
    }

    /// An artist credit, as `artist-credit`.
    pub fn artist_credit(&self, ac: u32, o: Opts) -> R<Value> {
        let o = o.not_toplevel();
        let mut out = Vec::new();
        for name in self.store.t("artist_credit_name").by("artist_credit", ac) {
            let artist = name.u32("artist").ok_or(Missing("artist", ac))?;
            out.push(json!({
                "name": name.text("name"),
                "joinphrase": name.text("join_phrase"),
                "artist": self.artist(artist, o)?,
            }));
        }
        Ok(Value::Array(out))
    }

    // Entity types

    pub fn area(&self, id: u32, o: Opts) -> R<Value> {
        let row = self.get("area", id)?;
        let mut out = Obj::new();
        let name = row.text("name");
        out.insert("name".into(), Value::String(name.clone()));
        out.insert("sort-name".into(), Value::String(name));
        out.insert("disambiguation".into(), Value::String(row.text("comment")));
        for (key, table) in [
            ("iso-3166-1-codes", "iso_3166_1"),
            ("iso-3166-2-codes", "iso_3166_2"),
            ("iso-3166-3-codes", "iso_3166_3"),
        ] {
            let mut codes: Vec<&str> = self
                .store
                .t(table)
                .by("area", id)
                .filter_map(|r| r.str("code"))
                .collect();
            codes.sort_unstable();
            if !codes.is_empty() {
                out.insert(key.into(), json!(codes));
            }
        }
        let type_id = if o.area_type || o.toplevel {
            row.u32("type")
        } else {
            None
        };
        self.finish(
            &mut out,
            Common {
                kind: Kind::Area,
                id,
                gid: row.gid(),
                period: Some(row),
                type_: Some(("area_type", type_id)),
            },
            o,
        )?;
        Ok(Value::Object(out))
    }

    fn area_or_null(&self, id: Option<u32>) -> R<Value> {
        match id {
            Some(id) => self.area(id, Opts::bare()),
            None => Ok(Value::Null),
        }
    }

    /// An area's own ISO 3166-1 code.
    fn country_code(&self, area: Option<u32>) -> Option<String> {
        let area = area?;
        let code = crate::store::derive::code(self.store, area);
        (!code.is_empty()).then_some(code)
    }

    pub fn artist(&self, id: u32, o: Opts) -> R<Value> {
        let row = self.get("artist", id)?;
        let mut out = Obj::new();
        out.insert("name".into(), Value::String(row.text("name")));
        out.insert("sort-name".into(), Value::String(row.text("sort_name")));
        out.insert("disambiguation".into(), Value::String(row.text("comment")));
        let country = row
            .u32("area")
            .and_then(|a| self.store.derived.area_country(a));
        out.insert(
            "country".into(),
            self.country_code(country)
                .map_or(Value::Null, Value::String),
        );
        if o.toplevel {
            self.named(&mut out, "gender", "gender", row.u32("gender"));
            out.insert("area".into(), self.area_or_null(row.u32("area"))?);
            out.insert(
                "begin-area".into(),
                self.area_or_null(row.u32("begin_area"))?,
            );
            out.insert("end-area".into(), self.area_or_null(row.u32("end_area"))?);
            self.artist_lists(&mut out, id)?;
        }
        self.finish(
            &mut out,
            Common {
                kind: Kind::Artist,
                id,
                gid: row.gid(),
                period: Some(row),
                type_: Some(("artist_type", row.u32("type"))),
            },
            o,
        )?;
        Ok(Value::Object(out))
    }

    /// The related lists of an artist lookup (`inc=recordings`, `releases`, `release-groups`, `works`).
    fn artist_lists(&self, out: &mut Obj, id: u32) -> R<()> {
        use super::browse::{self, Linked, Page};
        let inc = self.inc;
        let list = Opts::nested();
        let page = Page {
            limit: MAX_ITEMS,
            offset: 0,
        };
        if inc.recordings {
            let (ids, _) = browse::ids(self, Kind::Recording, Linked::Artist(id), page);
            out.insert("recordings".into(), self.list(Kind::Recording, &ids, list)?);
        }
        if inc.releases {
            let linked = if inc.various_artists {
                Linked::TrackArtist(id)
            } else {
                Linked::Artist(id)
            };
            let (ids, _) = browse::ids(self, Kind::Release, linked, page);
            out.insert("releases".into(), self.list(Kind::Release, &ids, list)?);
        }
        if inc.release_groups {
            let (ids, _) = browse::ids(self, Kind::ReleaseGroup, Linked::Artist(id), page);
            out.insert(
                "release-groups".into(),
                self.list(Kind::ReleaseGroup, &ids, list)?,
            );
        }
        if inc.works {
            let (ids, _) = browse::ids(self, Kind::Work, Linked::Artist(id), page);
            out.insert("works".into(), self.list(Kind::Work, &ids, list)?);
        }
        Ok(())
    }

    pub fn event(&self, id: u32, o: Opts) -> R<Value> {
        let row = self.get("event", id)?;
        let mut out = Obj::new();
        out.insert("name".into(), Value::String(row.text("name")));
        out.insert("disambiguation".into(), Value::String(row.text("comment")));
        // HH:MM, without the seconds
        let time = row
            .str("time")
            .map(|t| t.get(..5).unwrap_or(t).to_owned())
            .unwrap_or_default();
        out.insert("time".into(), Value::String(time));
        out.insert("setlist".into(), Value::String(row.text("setlist")));
        out.insert("cancelled".into(), Value::Bool(row.bool("cancelled")));
        self.finish(
            &mut out,
            Common {
                kind: Kind::Event,
                id,
                gid: row.gid(),
                period: Some(row),
                type_: Some(("event_type", row.u32("type"))),
            },
            o,
        )?;
        Ok(Value::Object(out))
    }

    pub fn genre(&self, id: u32, o: Opts) -> R<Value> {
        let row = self.get("genre", id)?;
        let mut out = Obj::new();
        out.insert("name".into(), Value::String(row.text("name")));
        out.insert("disambiguation".into(), Value::String(row.text("comment")));
        self.finish(
            &mut out,
            Common {
                kind: Kind::Genre,
                id,
                gid: row.gid(),
                period: None,
                type_: None,
            },
            o,
        )?;
        Ok(Value::Object(out))
    }

    pub fn instrument(&self, id: u32, o: Opts) -> R<Value> {
        let row = self.get("instrument", id)?;
        let mut out = Obj::new();
        out.insert("name".into(), Value::String(row.text("name")));
        out.insert("disambiguation".into(), Value::String(row.text("comment")));
        out.insert("description".into(), Value::String(row.text("description")));
        let type_ = Some(("instrument_type", row.u32("type")));
        self.finish(
            &mut out,
            Common {
                kind: Kind::Instrument,
                id,
                gid: row.gid(),
                period: None,
                type_,
            },
            o,
        )?;
        Ok(Value::Object(out))
    }

    pub fn label(&self, id: u32, o: Opts) -> R<Value> {
        let row = self.get("label", id)?;
        let mut out = Obj::new();
        let name = row.text("name");
        out.insert("name".into(), Value::String(name.clone()));
        out.insert("sort-name".into(), Value::String(name));
        out.insert("label-code".into(), int(row.u32("label_code")));
        out.insert("disambiguation".into(), Value::String(row.text("comment")));
        if o.toplevel {
            out.insert(
                "country".into(),
                self.country_code(row.u32("area"))
                    .map_or(Value::Null, Value::String),
            );
            out.insert("area".into(), self.area_or_null(row.u32("area"))?);
            if self.inc.releases {
                use super::browse::{self, Linked, Page};
                let (ids, _) = browse::ids(
                    self,
                    Kind::Release,
                    Linked::Label(id),
                    Page {
                        limit: MAX_ITEMS,
                        offset: 0,
                    },
                );
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
                gid: row.gid(),
                period: Some(row),
                type_: Some(("label_type", row.u32("type"))),
            },
            o,
        )?;
        Ok(Value::Object(out))
    }

    pub fn place(&self, id: u32, o: Opts) -> R<Value> {
        let row = self.get("place", id)?;
        let mut out = Obj::new();
        out.insert("name".into(), Value::String(row.text("name")));
        out.insert("disambiguation".into(), Value::String(row.text("comment")));
        out.insert("address".into(), Value::String(row.text("address")));
        // A place that is a relationship target has its area's type loaded, one at the top level not.
        let area = match row.u32("area") {
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
            row.str("coordinates")
                .and_then(parse_point)
                .unwrap_or(Value::Null),
        );
        self.finish(
            &mut out,
            Common {
                kind: Kind::Place,
                id,
                gid: row.gid(),
                period: Some(row),
                type_: Some(("place_type", row.u32("type"))),
            },
            o,
        )?;
        Ok(Value::Object(out))
    }

    pub fn recording(&self, id: u32, o: Opts) -> R<Value> {
        let row = self.get("recording", id)?;
        let inc = self.inc;
        let mut out = Obj::new();
        out.insert("title".into(), Value::String(row.text("name")));
        out.insert("disambiguation".into(), Value::String(row.text("comment")));
        out.insert("length".into(), int(row.u32("length")));
        out.insert("video".into(), Value::Bool(row.bool("video")));
        if o.with_inc
            && inc.artist_credits
            && let Some(ac) = row.u32("artist_credit")
        {
            out.insert("artist-credit".into(), self.artist_credit(ac, o)?);
        }
        if o.toplevel && inc.releases {
            out.insert("releases".into(), self.recording_releases(id)?);
        }
        if o.with_inc && inc.isrcs {
            let mut isrcs: Vec<&str> = self
                .store
                .t("isrc")
                .by("recording", id)
                .filter_map(|r| r.str("isrc"))
                .collect();
            isrcs.sort_unstable();
            out.insert("isrcs".into(), json!(isrcs));
        }
        if o.first_release_date
            && let Some(first) = self.store.derived.recording_first_date(id)
        {
            out.insert(
                "first-release-date".into(),
                Value::String(date::format_packed(i64::from(first))),
            );
        }
        self.finish(
            &mut out,
            Common {
                kind: Kind::Recording,
                id,
                gid: row.gid(),
                period: None,
                type_: None,
            },
            o,
        )?;
        Ok(Value::Object(out))
    }

    /// The releases of a recording lookup: with `inc=media`, each with only the medium and track of the recording.
    fn recording_releases(&self, id: u32) -> R<Value> {
        use super::browse::{self, Linked, Page};
        let (ids, _) = browse::ids(
            self,
            Kind::Release,
            Linked::Recording(id),
            Page {
                limit: MAX_ITEMS,
                offset: 0,
            },
        );
        let o = Opts::nested();
        let mut releases = Vec::with_capacity(ids.len());
        for release in ids {
            let mut value = self.release(release, o)?;
            if self.inc.media
                && let Value::Object(obj) = &mut value
            {
                // The recording's first track on this release.
                let track = self
                    .store
                    .t("track")
                    .by("recording", id)
                    .filter_map(|t| {
                        let medium = self.store.t("medium").get(t.u32("medium")?)?;
                        (medium.u32("release") == Some(release))
                            .then(|| (medium.u32("position"), t.u32("position"), medium.id, t.id))
                    })
                    .min();
                let media = match track {
                    Some((_, _, medium, track)) => vec![self.medium(medium, o, Some(&[track]))?],
                    None => Vec::new(),
                };
                obj.insert("media".into(), Value::Array(media));
            }
            releases.push(value);
        }
        Ok(Value::Array(releases))
    }

    /// A release's events, earliest first (missing parts last, then by country name): (country area, date).
    #[must_use]
    pub fn release_events(&self, id: u32) -> Vec<(Option<u32>, String)> {
        let mut events: Vec<(u32, String, Option<u32>, String)> = Vec::new();
        for e in self.store.t("release_country").by("release", id) {
            let country = e.u32("country");
            let name = country
                .and_then(|c| self.store.t("area").get(c))
                .map(|a| collation_key(&a.text("name")));
            let (y, m, d) = e.date("date");
            events.push((
                crate::store::derive::pack((y, m, d)),
                name.unwrap_or_default(),
                country,
                date::format(y, m, d),
            ));
        }
        for e in self.store.t("release_unknown_country").by("release", id) {
            let (y, m, d) = e.date("date");
            // no country sorts last
            events.push((
                crate::store::derive::pack((y, m, d)),
                "\u{10ffff}".into(),
                None,
                date::format(y, m, d),
            ));
        }
        events.sort();
        events
            .into_iter()
            .map(|(_, _, country, date)| (country, date))
            .collect()
    }

    #[allow(clippy::too_many_lines)]
    pub fn release(&self, id: u32, o: Opts) -> R<Value> {
        let row = self.get("release", id)?;
        let meta = self.store.t("release_meta").get(id);
        let presence = meta.and_then(|m| m.str("cover_art_presence"));
        let ac = row.u32("artist_credit");
        let inc = self.inc;
        let mut out = Obj::new();
        out.insert("title".into(), Value::String(row.text("name")));

        let events = self.release_events(id);
        if let Some((country, first_date)) = events.first() {
            out.insert("date".into(), Value::String(first_date.clone()));
            out.insert(
                "country".into(),
                self.country_code(*country)
                    .map_or(Value::Null, Value::String),
            );
            let mut list = Vec::with_capacity(events.len());
            for (country, date) in &events {
                list.push(json!({"date": date, "area": self.area_or_null(*country)?}));
            }
            out.insert("release-events".into(), Value::Array(list));
        }
        if o.toplevel {
            out.insert("asin".into(), text(meta.and_then(|m| m.str("amazon_asin"))));
        }
        out.insert("barcode".into(), text(row.str("barcode")));
        out.insert("disambiguation".into(), Value::String(row.text("comment")));
        self.named(&mut out, "status", "release_status", row.u32("status"));
        let quality = match row.u32("quality") {
            Some(0) => "low",
            Some(2) => "high",
            _ => "normal",
        };
        out.insert("quality".into(), Value::String(quality.into()));
        self.named(
            &mut out,
            "packaging",
            "release_packaging",
            row.u32("packaging"),
        );
        if o.toplevel {
            let (count, front, back) = if presence == Some("present") {
                self.cover_art_stats(id)
            } else {
                (0, false, false)
            };
            out.insert(
                "cover-art-archive".into(),
                json!({
                    "artwork": presence == Some("present"),
                    "darkened": presence == Some("darkened"),
                    "count": count,
                    "front": front,
                    "back": back,
                }),
            );
        }
        out.insert(
            "text-representation".into(),
            json!({
                "script": self.store.script(row.u32("script")),
                "language": self.store.language(row.u32("language")),
            }),
        );
        if o.with_inc && (inc.collections || inc.user_collections) {
            out.insert("collections".into(), json!([]));
        }
        let in_release = Opts {
            sublevel_ratings: true,
            release_ac: ac,
            ..o.not_toplevel()
        };
        if o.with_inc
            && inc.release_groups
            && let Some(rg) = row.u32("release_group")
        {
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
        if o.with_inc
            && inc.artist_credits
            && let Some(ac) = ac
        {
            out.insert("artist-credit".into(), self.artist_credit(ac, o)?);
        }
        if o.toplevel && inc.labels {
            let mut labels: Vec<(Option<String>, Option<String>, u32, Row<'_>)> = self
                .store
                .t("release_label")
                .by("release", id)
                .map(|rl| {
                    let name = rl
                        .u32("label")
                        .and_then(|l| self.store.t("label").get(l))
                        .map(|l| l.text("name").to_lowercase());
                    (
                        rl.str("catalog_number").map(str::to_owned),
                        name,
                        rl.u32("id").unwrap_or(0),
                        rl,
                    )
                })
                .collect();
            // catalog number, then label name (missing ones last), then id
            labels.sort_by(|a, b| {
                (a.0.is_none(), &a.0, a.1.is_none(), &a.1, a.2).cmp(&(
                    b.0.is_none(),
                    &b.0,
                    b.1.is_none(),
                    &b.1,
                    b.2,
                ))
            });
            let mut list = Vec::with_capacity(labels.len());
            for (catno, _, _, rl) in labels {
                let label = match rl.u32("label") {
                    Some(l) => self.label(l, o.not_toplevel())?,
                    None => Value::Null,
                };
                list.push(json!({"catalog-number": catno, "label": label}));
            }
            out.insert("label-info".into(), Value::Array(list));
        }
        if o.with_inc && (inc.media || inc.discids || inc.recordings) {
            let mediums: Vec<u32> = self
                .store
                .t("medium")
                .by("release", id)
                .map(|m| m.id)
                .collect();
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
                gid: row.gid(),
                period: None,
                type_: None,
            },
            o,
        )?;
        Ok(Value::Object(out))
    }

    /// A release's cover art: how many images, whether one is the front, the back.
    #[must_use]
    pub fn cover_art_stats(&self, release: u32) -> (usize, bool, bool) {
        let front = self.store.id_by_name("art_type", "Front");
        let back = self.store.id_by_name("art_type", "Back");
        let mut count = 0;
        let (mut has_front, mut has_back) = (false, false);
        for image in self.store.t("cover_art").by("release", release) {
            count += 1;
            let types = self.store.derived.cover_art_types(image.id);
            has_front |= front.is_some_and(|f| f < 32 && types & (1 << f) != 0);
            has_back |= back.is_some_and(|b| b < 32 && types & (1 << b) != 0);
        }
        (count, has_front, has_back)
    }

    /// A track's title (its own, or its recording's).
    #[must_use]
    pub fn track_title(&self, track: &Row<'_>) -> String {
        if track.str_offset("name") == SAME_AS_RECORDING {
            return track
                .u32("recording")
                .and_then(|r| self.store.t("recording").get(r))
                .map(|r| r.text("name"))
                .unwrap_or_default();
        }
        track.text("name")
    }

    /// A medium; `only` limits its tracks to these (a recording's releases with `inc=media`).
    #[allow(clippy::too_many_lines)] // the fields of a medium and its tracks
    fn medium(&self, id: u32, o: Opts, only: Option<&[u32]>) -> R<Value> {
        let row = self.get("medium", id)?;
        let inc = self.inc;
        let mut out = Obj::new();
        out.insert("title".into(), Value::String(row.text("name")));
        self.named(&mut out, "format", "medium_format", row.u32("format"));
        out.insert("position".into(), int(row.u32("position")));
        if inc.discids {
            let mut discs: Vec<(String, Value)> = self
                .store
                .t("medium_cdtoc")
                .by("medium", id)
                .filter_map(|mc| self.store.t("cdtoc").get(mc.u32("cdtoc")?))
                .map(|c| (c.text("discid"), cdtoc_json(&c)))
                .collect();
            discs.sort_by(|a, b| a.0.cmp(&b.0));
            out.insert(
                "discs".into(),
                Value::Array(discs.into_iter().map(|d| d.1).collect()),
            );
        }
        let tracks: Vec<Row<'_>> = self.store.t("track").by("medium", id).collect();
        let cdtoc_count = tracks
            .iter()
            .filter(|t| t.u32("position").is_some_and(|p| p > 0) && !t.bool("is_data_track"))
            .count();
        out.insert("track-count".into(), json!(cdtoc_count));
        if row.u32("gid").is_none() || !row.gid().is_empty() {
            out.insert("id".into(), Value::String(row.gid()));
        }
        if inc.recordings || only.is_some() {
            let has_pregap = tracks.iter().any(|t| t.u32("position") == Some(0));
            let shown: Vec<&Row<'_>> = tracks
                .iter()
                .filter(|t| only.is_none_or(|o| o.contains(&t.id)))
                .collect();
            let serialize = |t: &Row<'_>| -> R<Value> {
                let mut track = Obj::new();
                track.insert("id".into(), Value::String(t.gid()));
                track.insert("length".into(), int(t.u32("length")));
                track.insert("number".into(), text(t.str("number")));
                track.insert("position".into(), int(t.u32("position")));
                track.insert("title".into(), Value::String(self.track_title(t)));
                let ac = t.u32("artist_credit");
                if inc.recordings
                    && let Some(recording) = t.u32("recording")
                {
                    let rels = if inc.recording_level_rels {
                        Rels::All
                    } else {
                        Rels::None
                    };
                    let recording_opts = Opts {
                        track_ac: ac,
                        rels,
                        first_release_date: true,
                        ..o
                    };
                    track.insert(
                        "recording".into(),
                        self.recording(recording, recording_opts)?,
                    );
                }
                if inc.artist_credits
                    && let Some(ac) = ac
                {
                    track.insert("artist-credit".into(), self.artist_credit(ac, o)?);
                }
                Ok(Value::Object(track))
            };
            let min = shown.first().and_then(|t| t.u32("position")).unwrap_or(0);
            if has_pregap
                && let Some(first) = shown.first().filter(|t| t.u32("position") == Some(0))
            {
                out.insert("pregap".into(), serialize(first)?);
            }
            let audio: Vec<&&Row<'_>> = shown
                .iter()
                .filter(|t| t.u32("position").is_some_and(|p| p > 0) && !t.bool("is_data_track"))
                .collect();
            if !audio.is_empty() {
                out.insert(
                    "tracks".into(),
                    audio
                        .into_iter()
                        .map(|t| serialize(t))
                        .collect::<R<Vec<_>>>()?
                        .into(),
                );
                out.insert(
                    "track-offset".into(),
                    json!(if has_pregap { 0 } else { i64::from(min) - 1 }),
                );
            }
            let data: Vec<&&Row<'_>> = shown
                .iter()
                .filter(|t| t.u32("position").is_some_and(|p| p > 0) && t.bool("is_data_track"))
                .collect();
            if !data.is_empty() {
                out.insert(
                    "data-tracks".into(),
                    data.into_iter()
                        .map(|t| serialize(t))
                        .collect::<R<Vec<_>>>()?
                        .into(),
                );
            }
        }
        Ok(Value::Object(out))
    }

    pub fn release_group(&self, id: u32, o: Opts) -> R<Value> {
        let row = self.get("release_group", id)?;
        let meta = self.store.t("release_group_meta").get(id);
        let inc = self.inc;
        let mut out = Obj::new();
        out.insert("title".into(), Value::String(row.text("name")));
        self.named(
            &mut out,
            "primary-type",
            "release_group_primary_type",
            row.u32("type"),
        );
        let mut secondary: Vec<(&str, Option<String>)> = self
            .store
            .t("release_group_secondary_type_join")
            .by("release_group", id)
            .filter_map(|j| {
                self.store
                    .named("release_group_secondary_type", j.u32("secondary_type"))
            })
            .collect();
        secondary.sort_by(|a, b| a.0.cmp(b.0));
        out.insert(
            "secondary-types".into(),
            json!(secondary.iter().map(|s| s.0).collect::<Vec<_>>()),
        );
        out.insert(
            "secondary-type-ids".into(),
            json!(secondary.iter().map(|s| &s.1).collect::<Vec<_>>()),
        );
        let first = meta.map_or((None, None, None), |m| m.date("first_release_date"));
        out.insert(
            "first-release-date".into(),
            Value::String(date::format(first.0, first.1, first.2)),
        );
        out.insert("disambiguation".into(), Value::String(row.text("comment")));
        if o.with_inc
            && inc.artist_credits
            && let Some(ac) = row.u32("artist_credit")
        {
            out.insert("artist-credit".into(), self.artist_credit(ac, o)?);
        }
        // Only a release group of the request has its releases loaded; a nested one shows an empty list.
        if o.with_inc && inc.releases && !o.toplevel {
            out.insert("releases".into(), json!([]));
        } else if o.with_inc && inc.releases {
            use super::browse::{self, Linked, Page};
            let (ids, _) = browse::ids(
                self,
                Kind::Release,
                Linked::ReleaseGroup(id),
                Page {
                    limit: MAX_ITEMS,
                    offset: 0,
                },
            );
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
                gid: row.gid(),
                period: None,
                type_: None,
            },
            o,
        )?;
        Ok(Value::Object(out))
    }

    pub fn series(&self, id: u32, o: Opts) -> R<Value> {
        let row = self.get("series", id)?;
        let mut out = Obj::new();
        out.insert("name".into(), Value::String(row.text("name")));
        out.insert("disambiguation".into(), Value::String(row.text("comment")));
        let type_ = Some(("series_type", row.u32("type")));
        self.finish(
            &mut out,
            Common {
                kind: Kind::Series,
                id,
                gid: row.gid(),
                period: None,
                type_,
            },
            o,
        )?;
        Ok(Value::Object(out))
    }

    pub fn url(&self, id: u32, o: Opts) -> R<Value> {
        let row = self.get("url", id)?;
        let mut out = Obj::new();
        out.insert("resource".into(), Value::String(row.text("url")));
        self.finish(
            &mut out,
            Common {
                kind: Kind::Url,
                id,
                gid: row.gid(),
                period: None,
                type_: None,
            },
            o,
        )?;
        Ok(Value::Object(out))
    }

    pub fn work(&self, id: u32, o: Opts) -> R<Value> {
        let row = self.get("work", id)?;
        let mut out = Obj::new();
        out.insert("title".into(), Value::String(row.text("name")));
        out.insert("disambiguation".into(), Value::String(row.text("comment")));
        let mut iswcs: Vec<&str> = if o.work_details {
            self.store
                .t("iswc")
                .by("work", id)
                .filter_map(|r| r.str("iswc"))
                .collect()
        } else {
            Vec::new()
        };
        iswcs.sort_unstable();
        out.insert("iswcs".into(), json!(iswcs));
        // Only the works of the request have their attributes loaded.
        let mut attributes = Vec::new();
        if o.toplevel {
            for a in self.store.t("work_attribute").by("work", id) {
                let mut attribute = Obj::new();
                let allowed = a
                    .u32("work_attribute_type_allowed_value")
                    .and_then(|v| self.store.t("work_attribute_type_allowed_value").get(v));
                let value = allowed
                    .map(|v| v.text("value"))
                    .or_else(|| a.str("work_attribute_text").map(str::to_owned));
                attribute.insert("value".into(), Value::String(value.unwrap_or_default()));
                if let Some(v) = allowed {
                    attribute.insert("value-id".into(), Value::String(v.gid()));
                }
                self.named(
                    &mut attribute,
                    "type",
                    "work_attribute_type",
                    a.u32("work_attribute_type"),
                );
                attributes.push(Value::Object(attribute));
            }
        }
        out.insert("attributes".into(), Value::Array(attributes));
        let languages: Vec<String> = if o.work_details {
            self.store
                .t("work_language")
                .by("work", id)
                .filter_map(|l| self.store.language(l.u32("language")).map(str::to_owned))
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
        let type_ = Some(("work_type", row.u32("type")));
        self.finish(
            &mut out,
            Common {
                kind: Kind::Work,
                id,
                gid: row.gid(),
                period: None,
                type_,
            },
            o,
        )?;
        Ok(Value::Object(out))
    }

    // Relationships

    /// The relationship table between two entity types, and whether `kind` is its entity0.
    fn rel_table(kind: Kind, target: Kind) -> String {
        let (a, b) = if kind.table() <= target.table() {
            (kind, target)
        } else {
            (target, kind)
        };
        format!("l_{}_{}", a.table(), b.table())
    }

    #[allow(clippy::too_many_lines)]
    fn relations(&self, kind: Kind, id: u32, cardinal: bool) -> R<Vec<Value>> {
        let mut out = Vec::new();
        for target in self.inc.rels.iter().copied() {
            let table = self.store.t(&Self::rel_table(kind, target));
            let mut rels = Vec::new();
            for (key, backward, applies) in [
                ("entity0", false, kind.table() <= target.table()),
                ("entity1", true, kind.table() >= target.table()),
            ] {
                if !applies {
                    continue;
                }
                for row in table.by(key, id) {
                    let other = row
                        .u32(if backward { "entity0" } else { "entity1" })
                        .ok_or(Missing("relationship", row.id))?;
                    let link = self.get("link", row.u32("link").unwrap_or(u32::MAX))?;
                    let link_type = self
                        .store
                        .t("link_type")
                        .get(link.u32("link_type").unwrap_or(u32::MAX));
                    // From a work reached through work-level-rels, only relationships where the work's side is
                    // "one" (so not all its recordings).
                    if cardinal {
                        let cardinality = link_type
                            .and_then(|t| {
                                t.u32(if backward {
                                    "entity1_cardinality"
                                } else {
                                    "entity0_cardinality"
                                })
                            })
                            .unwrap_or(0);
                        if cardinality != 0 {
                            continue;
                        }
                    }
                    rels.push(Rel {
                        row,
                        target: other,
                        link,
                        backward,
                    });
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
            // As musicbrainz-server's query orders them: link type name (its database collation ignores case),
            // dates, then the name and id of the entity it joins as the target. For relationships between two
            // entities of the same type that is entity0 (the query's join ends up on it), so forward ones keep
            // their row order.
            let source_key = if kind == target {
                Some(self.sort_key(kind, id))
            } else {
                None
            };
            let type_name = |r: &Rel<'_>| {
                self.store
                    .t("link_type")
                    .get(r.link.u32("link_type").unwrap_or(u32::MAX))
                    .map(|t| t.text("name").to_lowercase())
                    .unwrap_or_default()
            };
            let nulls_last = |d: (Option<i64>, Option<i64>, Option<i64>)| {
                (
                    d.0.unwrap_or(i64::MAX),
                    d.1.unwrap_or(i64::MAX),
                    d.2.unwrap_or(i64::MAX),
                )
            };
            items.sort_by(|(a, _, an), (b, _, bn)| {
                let joined = |r: &Rel<'_>, n: &String| -> (String, u32) {
                    match &source_key {
                        Some(source) if !r.backward => (source.clone(), id),
                        _ => (n.clone(), r.target),
                    }
                };
                type_name(a)
                    .cmp(&type_name(b))
                    .then(nulls_last(a.link.date("begin")).cmp(&nulls_last(b.link.date("begin"))))
                    .then(nulls_last(a.link.date("end")).cmp(&nulls_last(b.link.date("end"))))
                    .then(a.link.bool("ended").cmp(&b.link.bool("ended")))
                    .then(joined(a, an).cmp(&joined(b, bn)))
                    .then(a.row.id.cmp(&b.row.id))
            });
            for (rel, value, _) in items {
                out.push(self.relation(target, &rel, value));
            }
        }
        Ok(out)
    }

    /// What relationships to an entity sort by: its sort name, URL or name.
    fn sort_key(&self, kind: Kind, id: u32) -> String {
        let field = match kind {
            Kind::Artist => "sort_name",
            Kind::Url => "url",
            _ => "name",
        };
        collation_key(
            &self
                .store
                .entity(kind, id)
                .map(|r| r.text(field))
                .unwrap_or_default(),
        )
    }

    fn relation(&self, target: Kind, rel: &Rel<'_>, value: Value) -> Value {
        let mut out = Obj::new();
        let link_type = self
            .store
            .t("link_type")
            .get(rel.link.u32("link_type").unwrap_or(u32::MAX));
        out.insert(
            "type".into(),
            link_type.map_or(Value::Null, |t| Value::String(t.text("name"))),
        );
        out.insert(
            "type-id".into(),
            link_type.map_or(Value::Null, |t| Value::String(t.gid())),
        );
        out.insert(
            "direction".into(),
            Value::String(if rel.backward { "backward" } else { "forward" }.into()),
        );
        if let Some(order) = rel.row.u32("link_order").filter(|o| *o != 0) {
            out.insert("ordering-key".into(), json!(order));
        }
        let (by, bm, bd) = rel.link.date("begin");
        let (ey, em, ed) = rel.link.date("end");
        out.insert("begin".into(), opt_str(&date::format(by, bm, bd)));
        out.insert("end".into(), opt_str(&date::format(ey, em, ed)));
        out.insert("ended".into(), Value::Bool(rel.link.bool("ended")));

        let link = rel.link.id;
        let mut attributes: Vec<Attribute> = self
            .store
            .t("link_attribute")
            .by("link", link)
            .filter_map(|a| {
                let type_id = a.u32("attribute_type")?;
                let t = self.store.t("link_attribute_type").get(type_id)?;
                let value = self
                    .store
                    .t("link_attribute_text_value")
                    .by("link", link)
                    .find(|v| v.u32("attribute_type") == Some(type_id))
                    .and_then(|v| v.str("text_value").map(str::to_owned));
                let credit = self
                    .store
                    .t("link_attribute_credit")
                    .by("link", link)
                    .find(|v| v.u32("attribute_type") == Some(type_id))
                    .and_then(|v| v.str("credited_as").map(str::to_owned));
                let creditable = self
                    .store
                    .t("link_creditable_attribute_type")
                    .get(type_id)
                    .is_some();
                Some((t.text("name"), Some(t.gid()), value, credit, creditable))
            })
            .collect();
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
        let (source_credit, target_credit) = if rel.backward {
            ("entity1_credit", "entity0_credit")
        } else {
            ("entity0_credit", "entity1_credit")
        };
        out.insert(
            "source-credit".into(),
            Value::String(rel.row.text(source_credit)),
        );
        out.insert(
            "target-credit".into(),
            Value::String(rel.row.text(target_credit)),
        );
        Value::Object(out)
    }
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
pub fn cdtoc_json(cdtoc: &Row<'_>) -> Value {
    let offsets = cdtoc
        .str("track_offset")
        .map(crate::store::copy::parse_int_array)
        .unwrap_or_default();
    let mut out = Obj::new();
    out.insert("id".into(), Value::String(cdtoc.text("discid")));
    out.insert("offset-count".into(), int(cdtoc.u32("track_count")));
    out.insert("sectors".into(), int(cdtoc.u32("leadout_offset")));
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
