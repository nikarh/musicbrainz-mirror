//! Search results, shaped as MusicBrainz's search server returns them. They differ from lookups: empty values are
//! left out, there is an `artist-credit-id`, aliases have `begin-date` / `end-date`, releases have `count` (of
//! media) and `track-count`, and lists of related entities are shorter.

use serde_json::{Map, Value, json};

use crate::date;
use crate::kind::Kind;
use crate::store::{Row, Store};
use crate::ws::entity::{Ctx, Filter};
use crate::ws::inc::Inc;

type Obj = Map<String, Value>;

/// Releases listed in a recording result, at most.
const RECORDING_RELEASES: usize = 25;

pub struct Formatter<'a> {
    pub store: &'a Store,
}

/// Inserts a string unless it is empty.
fn put(obj: &mut Obj, key: &str, value: Option<&str>) {
    if let Some(v) = value.filter(|v| !v.is_empty()) {
        obj.insert(key.into(), Value::String(v.to_owned()));
    }
}

fn put_list(obj: &mut Obj, key: &str, list: Vec<Value>) {
    if !list.is_empty() {
        obj.insert(key.into(), Value::Array(list));
    }
}

fn strings<'a>(rows: impl Iterator<Item = Row<'a>>, field: &str) -> Vec<Value> {
    let mut v: Vec<&str> = rows.filter_map(|r| r.str(field)).collect();
    v.sort_unstable();
    v.into_iter().map(|s| Value::String(s.to_owned())).collect()
}

fn life_span(row: &Row<'_>, with_ended_null: bool) -> Value {
    let (by, bm, bd) = row.date("begin");
    let (ey, em, ed) = row.date("end");
    let mut span = Obj::new();
    put(&mut span, "begin", Some(&date::format(by, bm, bd)));
    put(&mut span, "end", Some(&date::format(ey, em, ed)));
    if row.bool("ended") {
        span.insert("ended".into(), Value::Bool(true));
    } else if with_ended_null {
        span.insert("ended".into(), Value::Null);
    }
    Value::Object(span)
}

impl Formatter<'_> {
    fn ctx(&self) -> Ctx<'_> {
        static INC: std::sync::OnceLock<Inc> = std::sync::OnceLock::new();
        static FILTER: std::sync::OnceLock<Filter> = std::sync::OnceLock::new();
        Ctx {
            store: self.store,
            inc: INC.get_or_init(Inc::default),
            filter: FILTER.get_or_init(Filter::default),
            xml: false,
        }
    }

    fn named(&self, obj: &mut Obj, key: &str, table: &str, id: Option<u32>) {
        if let Some((name, gid)) = self.store.named(table, id) {
            obj.insert(key.into(), Value::String(name.to_owned()));
            if let Some(gid) = gid {
                obj.insert(format!("{key}-id"), Value::String(gid));
            }
        }
    }

    #[must_use]
    pub fn entity(&self, kind: Kind, id: u32) -> Value {
        let Some(row) = self.store.entity(kind, id) else {
            return Value::Null;
        };
        Value::Object(match kind {
            Kind::Artist => self.artist(&row),
            Kind::Release => self.release(&row, true),
            Kind::ReleaseGroup => self.release_group(&row),
            Kind::Recording => self.recording(&row),
            Kind::Work => self.work(&row),
            Kind::Label => self.label(&row),
            Kind::Area => self.area(&row, true),
            Kind::Place => self.place(&row),
            Kind::Event => self.event(&row),
            Kind::Instrument => self.simple(&row, Kind::Instrument, "instrument_type"),
            Kind::Series => self.simple(&row, Kind::Series, "series_type"),
            Kind::Genre | Kind::Url => Obj::new(),
        })
    }

    fn aliases(&self, kind: Kind, id: u32) -> Vec<Value> {
        self.store
            .t(&format!("{}_alias", kind.table()))
            .by("entity", id)
            .map(|r| {
                let mut a = Obj::new();
                a.insert("name".into(), Value::String(r.text("name")));
                a.insert("sort-name".into(), Value::String(r.text("sort_name")));
                let type_ = self.store.alias_type(kind, r.u32("type"));
                a.insert(
                    "type".into(),
                    type_
                        .as_ref()
                        .map_or(Value::Null, |t| Value::String(t.0.to_owned())),
                );
                if let Some(gid) = type_.and_then(|t| t.1) {
                    a.insert("type-id".into(), Value::String(gid));
                }
                a.insert(
                    "primary".into(),
                    if r.bool("primary_for_locale") {
                        Value::Bool(true)
                    } else {
                        Value::Null
                    },
                );
                a.insert(
                    "locale".into(),
                    r.str("locale")
                        .map_or(Value::Null, |l| Value::String(l.to_owned())),
                );
                let (by, bm, bd) = r.date("begin");
                let (ey, em, ed) = r.date("end");
                let (begin, end) = (date::format(by, bm, bd), date::format(ey, em, ed));
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
                Value::Object(a)
            })
            .collect()
    }

    fn tags(&self, kind: Kind, id: u32) -> Vec<Value> {
        let mut tags: Vec<(String, i32)> = self
            .store
            .t(&format!("{}_tag", kind.table()))
            .by("entity", id)
            .filter_map(|r| {
                let count = r.i32("count").filter(|c| *c > 0)?;
                Some((self.store.t("tag").get(r.u32("tag")?)?.text("name"), count))
            })
            .collect();
        tags.sort();
        tags.into_iter()
            .map(|(name, count)| json!({"count": count, "name": name}))
            .collect()
    }

    /// Aliases and tags, when there are any.
    fn extras(&self, obj: &mut Obj, kind: Kind, id: u32) {
        put_list(obj, "aliases", self.aliases(kind, id));
        if kind.has_tags() {
            put_list(obj, "tags", self.tags(kind, id));
        }
    }

    fn area(&self, row: &Row<'_>, full: bool) -> Obj {
        let mut out = Obj::new();
        out.insert("id".into(), Value::String(row.gid()));
        self.named(&mut out, "type", "area_type", row.u32("type"));
        out.insert("name".into(), Value::String(row.text("name")));
        out.insert("sort-name".into(), Value::String(row.text("name")));
        if full {
            put(&mut out, "disambiguation", row.str("comment"));
            for (key, table) in [
                ("iso-3166-1-codes", "iso_3166_1"),
                ("iso-3166-2-codes", "iso_3166_2"),
                ("iso-3166-3-codes", "iso_3166_3"),
            ] {
                put_list(
                    &mut out,
                    key,
                    strings(self.store.t(table).by("area", row.id), "code"),
                );
            }
            self.extras(&mut out, Kind::Area, row.id);
        }
        out.insert("life-span".into(), life_span(row, true));
        out
    }

    fn area_by_id(&self, id: Option<u32>) -> Option<Obj> {
        self.store.t("area").get(id?).map(|a| self.area(&a, false))
    }

    fn code(&self, area: Option<u32>) -> Option<String> {
        let code = crate::store::derive::code(self.store, area?);
        (!code.is_empty()).then_some(code)
    }

    fn artist(&self, row: &Row<'_>) -> Obj {
        let mut out = Obj::new();
        out.insert("id".into(), Value::String(row.gid()));
        self.named(&mut out, "type", "artist_type", row.u32("type"));
        out.insert("name".into(), Value::String(row.text("name")));
        out.insert("sort-name".into(), Value::String(row.text("sort_name")));
        self.named(&mut out, "gender", "gender", row.u32("gender"));
        let country = row
            .u32("area")
            .and_then(|a| self.store.derived.area_country(a));
        put(&mut out, "country", self.code(country).as_deref());
        for (key, field) in [
            ("area", "area"),
            ("begin-area", "begin_area"),
            ("end-area", "end_area"),
        ] {
            if let Some(area) = self.area_by_id(row.u32(field)) {
                out.insert(key.into(), Value::Object(area));
            }
        }
        put_list(
            &mut out,
            "ipis",
            strings(self.store.t("artist_ipi").by("artist", row.id), "ipi"),
        );
        put_list(
            &mut out,
            "isnis",
            strings(self.store.t("artist_isni").by("artist", row.id), "isni"),
        );
        out.insert("life-span".into(), life_span(row, true));
        put(&mut out, "disambiguation", row.str("comment"));
        self.extras(&mut out, Kind::Artist, row.id);
        out
    }

    /// An artist credit and its MBID: (`artist-credit`, `artist-credit-id`). Artists have their aliases with
    /// `aliases`.
    fn artist_credit(&self, ac: Option<u32>, aliases: bool) -> (Value, Option<String>) {
        let Some(ac) = ac else {
            return (json!([]), None);
        };
        let mut out = Vec::new();
        for name in self.store.t("artist_credit_name").by("artist_credit", ac) {
            let Some(artist) = name
                .u32("artist")
                .and_then(|a| self.store.t("artist").get(a))
            else {
                continue;
            };
            let mut a = Obj::new();
            a.insert("id".into(), Value::String(artist.gid()));
            a.insert("name".into(), Value::String(artist.text("name")));
            a.insert("sort-name".into(), Value::String(artist.text("sort_name")));
            put(&mut a, "disambiguation", artist.str("comment"));
            if aliases {
                put_list(&mut a, "aliases", self.aliases(Kind::Artist, artist.id));
            }
            let mut credit = Obj::new();
            credit.insert("name".into(), Value::String(name.text("name")));
            put(&mut credit, "joinphrase", name.str("join_phrase"));
            credit.insert("artist".into(), Value::Object(a));
            out.push(Value::Object(credit));
        }
        let gid = self.store.t("artist_credit").get(ac).map(|c| c.gid());
        (Value::Array(out), gid)
    }

    fn primary_type(&self, out: &mut Obj, type_id: Option<u32>) {
        if let Some((name, gid)) = self.store.named("release_group_primary_type", type_id) {
            if let Some(g) = gid {
                out.insert("type-id".into(), Value::String(g.clone()));
                out.insert("primary-type-id".into(), Value::String(g));
            }
            out.insert("primary-type".into(), Value::String(name.to_owned()));
        }
    }

    fn release_group_short(&self, rg: &Row<'_>, with_comment: bool) -> Obj {
        let mut out = Obj::new();
        out.insert("id".into(), Value::String(rg.gid()));
        self.primary_type(&mut out, rg.u32("type"));
        out.insert("title".into(), Value::String(rg.text("name")));
        if with_comment {
            put(&mut out, "disambiguation", rg.str("comment"));
        }
        self.secondary_types(&mut out, rg.id);
        out
    }

    fn secondary_types(&self, out: &mut Obj, rg: u32) {
        let mut named: Vec<(&str, Option<String>)> = self
            .store
            .t("release_group_secondary_type_join")
            .by("release_group", rg)
            .filter_map(|j| {
                self.store
                    .named("release_group_secondary_type", j.u32("secondary_type"))
            })
            .collect();
        named.sort_by(|a, b| a.0.cmp(b.0));
        put_list(
            out,
            "secondary-types",
            named
                .iter()
                .map(|n| Value::String(n.0.to_owned()))
                .collect(),
        );
        put_list(
            out,
            "secondary-type-ids",
            named
                .iter()
                .filter_map(|n| n.1.clone())
                .map(Value::String)
                .collect(),
        );
    }

    /// Release events, the first one's date and country.
    fn release_events(&self, out: &mut Obj, release: u32) {
        let events = self.ctx().release_events(release);
        if let Some((country, first)) = events.first() {
            put(out, "date", Some(first));
            put(out, "country", self.code(*country).as_deref());
        }
        let mut list = Vec::new();
        for (country, date) in events {
            let mut event = Obj::new();
            put(&mut event, "date", Some(&date));
            if let Some(area) = country.and_then(|c| self.store.t("area").get(c)) {
                let mut a = Obj::new();
                a.insert("id".into(), Value::String(area.gid()));
                a.insert("name".into(), Value::String(area.text("name")));
                a.insert("sort-name".into(), Value::String(area.text("name")));
                put_list(
                    &mut a,
                    "iso-3166-1-codes",
                    strings(self.store.t("iso_3166_1").by("area", area.id), "code"),
                );
                event.insert("area".into(), Value::Object(a));
            }
            list.push(Value::Object(event));
        }
        put_list(out, "release-events", list);
    }

    /// A release; `full` for a release result (labels, media formats, tags), else as listed in a recording.
    fn release(&self, row: &Row<'_>, full: bool) -> Obj {
        let mediums: Vec<Row<'_>> = self.store.t("medium").by("release", row.id).collect();
        let mut out = Obj::new();
        out.insert("id".into(), Value::String(row.gid()));
        out.insert("title".into(), Value::String(row.text("name")));
        self.named(&mut out, "status", "release_status", row.u32("status"));
        let (credit, credit_id) = self.artist_credit(row.u32("artist_credit"), false);
        put(&mut out, "artist-credit-id", credit_id.as_deref());
        out.insert("artist-credit".into(), credit);
        out.insert("count".into(), json!(mediums.len()));
        if let Some(rg) = row
            .u32("release_group")
            .and_then(|g| self.store.t("release_group").get(g))
        {
            out.insert(
                "release-group".into(),
                Value::Object(self.release_group_short(&rg, !full)),
            );
        }
        self.release_events(&mut out, row.id);
        out.insert(
            "track-count".into(),
            json!(
                mediums
                    .iter()
                    .filter_map(|m| m.u32("track_count"))
                    .sum::<u32>()
            ),
        );
        if !full {
            return out;
        }
        put(&mut out, "disambiguation", row.str("comment"));
        self.named(
            &mut out,
            "packaging",
            "release_packaging",
            row.u32("packaging"),
        );
        let mut text = Obj::new();
        put(
            &mut text,
            "language",
            self.store.language(row.u32("language")),
        );
        put(&mut text, "script", self.store.script(row.u32("script")));
        out.insert("text-representation".into(), Value::Object(text));
        put(&mut out, "barcode", row.str("barcode"));
        put(
            &mut out,
            "asin",
            self.store
                .t("release_meta")
                .get(row.id)
                .and_then(|m| m.str("amazon_asin")),
        );
        let mut label_info = Vec::new();
        for rl in self.store.t("release_label").by("release", row.id) {
            let mut info = Obj::new();
            put(&mut info, "catalog-number", rl.str("catalog_number"));
            if let Some(label) = rl.u32("label").and_then(|l| self.store.t("label").get(l)) {
                info.insert(
                    "label".into(),
                    json!({"id": label.gid(), "name": label.text("name")}),
                );
            }
            label_info.push(Value::Object(info));
        }
        put_list(&mut out, "label-info", label_info);
        let mut media = Vec::new();
        for m in &mediums {
            let mut medium = Obj::new();
            medium.insert("id".into(), Value::String(m.gid()));
            if let Some((format, _)) = self.store.named("medium_format", m.u32("format")) {
                medium.insert("format".into(), Value::String(format.to_owned()));
            }
            medium.insert(
                "disc-count".into(),
                json!(self.store.t("medium_cdtoc").by("medium", m.id).count()),
            );
            medium.insert(
                "track-count".into(),
                json!(m.u32("track_count").unwrap_or(0)),
            );
            media.push(Value::Object(medium));
        }
        out.insert("media".into(), Value::Array(media));
        put_list(&mut out, "tags", self.tags(Kind::Release, row.id));
        out
    }

    fn release_group(&self, row: &Row<'_>) -> Obj {
        let mut out = Obj::new();
        out.insert("id".into(), Value::String(row.gid()));
        self.primary_type(&mut out, row.u32("type"));
        self.secondary_types(&mut out, row.id);
        out.insert("title".into(), Value::String(row.text("name")));
        if let Some(meta) = self.store.t("release_group_meta").get(row.id) {
            let (y, m, d) = meta.date("first_release_date");
            put(&mut out, "first-release-date", Some(&date::format(y, m, d)));
        }
        put(&mut out, "disambiguation", row.str("comment"));
        let (credit, credit_id) = self.artist_credit(row.u32("artist_credit"), false);
        put(&mut out, "artist-credit-id", credit_id.as_deref());
        out.insert("artist-credit".into(), credit);
        let releases: Vec<Value> = self
            .store
            .t("release")
            .by("release_group", row.id)
            .map(|r| {
                let mut o = Obj::new();
                o.insert("id".into(), Value::String(r.gid()));
                o.insert("title".into(), Value::String(r.text("name")));
                self.named(&mut o, "status", "release_status", r.u32("status"));
                Value::Object(o)
            })
            .collect();
        out.insert("count".into(), json!(releases.len()));
        out.insert("releases".into(), Value::Array(releases));
        put_list(&mut out, "tags", self.tags(Kind::ReleaseGroup, row.id));
        out
    }

    fn recording(&self, row: &Row<'_>) -> Obj {
        let mut out = Obj::new();
        out.insert("id".into(), Value::String(row.gid()));
        out.insert("title".into(), Value::String(row.text("name")));
        if let Some(length) = row.u32("length") {
            out.insert("length".into(), json!(length));
        }
        out.insert(
            "video".into(),
            if row.bool("video") {
                Value::Bool(true)
            } else {
                Value::Null
            },
        );
        put(&mut out, "disambiguation", row.str("comment"));
        let (credit, credit_id) = self.artist_credit(row.u32("artist_credit"), true);
        put(&mut out, "artist-credit-id", credit_id.as_deref());
        out.insert("artist-credit".into(), credit);
        if let Some(first) = self.store.derived.recording_first_date(row.id) {
            put(
                &mut out,
                "first-release-date",
                Some(&date::format_packed(i64::from(first))),
            );
        }
        put_list(
            &mut out,
            "isrcs",
            strings(self.store.t("isrc").by("recording", row.id), "isrc"),
        );

        // The releases it is on, each with the medium and track.
        let ctx = self.ctx();
        let mut tracks: Vec<(u32, Row<'_>, Row<'_>)> = self
            .store
            .t("track")
            .by("recording", row.id)
            .filter_map(|t| {
                let medium = self.store.t("medium").get(t.u32("medium")?)?;
                Some((medium.u32("release")?, medium, t))
            })
            .collect();
        tracks.sort_by_key(|(release, medium, track)| {
            (*release, medium.u32("position"), track.u32("position"))
        });
        let mut releases = Vec::new();
        let mut last = None;
        for (release, medium, track) in tracks {
            if last == Some(release) || releases.len() >= RECORDING_RELEASES {
                continue;
            }
            last = Some(release);
            let Some(release_row) = self.store.t("release").get(release) else {
                continue;
            };
            let mut r = self.release(&release_row, false);
            let mut m = Obj::new();
            m.insert("id".into(), Value::String(medium.gid()));
            m.insert("position".into(), json!(medium.u32("position")));
            if let Some((format, _)) = self.store.named("medium_format", medium.u32("format")) {
                m.insert("format".into(), Value::String(format.to_owned()));
            }
            let mut t = Obj::new();
            t.insert("id".into(), Value::String(track.gid()));
            put(&mut t, "number", track.str("number"));
            t.insert("title".into(), Value::String(ctx.track_title(&track)));
            if let Some(l) = track.u32("length") {
                t.insert("length".into(), json!(l));
            }
            m.insert("track".into(), json!([t]));
            m.insert(
                "track-count".into(),
                json!(medium.u32("track_count").unwrap_or(0)),
            );
            m.insert(
                "track-offset".into(),
                json!(i64::from(track.u32("position").unwrap_or(1)) - 1),
            );
            r.insert("media".into(), json!([m]));
            releases.push(Value::Object(r));
        }
        put_list(&mut out, "releases", releases);
        put_list(&mut out, "tags", self.tags(Kind::Recording, row.id));
        out
    }

    /// Relationships of a work or event to artists, as the search server lists them.
    fn artist_relations(&self, table: &str, id: u32) -> Vec<Value> {
        self.store
            .t(table)
            .by("entity1", id)
            .filter_map(|rel| {
                let artist = self.store.t("artist").get(rel.u32("entity0")?)?;
                let link = self.store.t("link").get(rel.u32("link")?)?;
                let lt = self.store.t("link_type").get(link.u32("link_type")?);
                Some(json!({
                    "type": lt.map(|t| t.text("name")),
                    "type-id": lt.map(|t| t.gid()),
                    "direction": "backward",
                    "artist": {"id": artist.gid(), "name": artist.text("name"), "sort-name": artist.text("sort_name")},
                }))
            })
            .collect()
    }

    fn work(&self, row: &Row<'_>) -> Obj {
        let mut out = Obj::new();
        out.insert("id".into(), Value::String(row.gid()));
        if let Some((t, _)) = self.store.named("work_type", row.u32("type")) {
            out.insert("type".into(), Value::String(t.to_owned()));
        }
        out.insert("title".into(), Value::String(row.text("name")));
        let languages: Vec<String> = self
            .store
            .t("work_language")
            .by("work", row.id)
            .filter_map(|l| self.store.language(l.u32("language")).map(str::to_owned))
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
        put_list(
            &mut out,
            "iswcs",
            strings(self.store.t("iswc").by("work", row.id), "iswc"),
        );
        put(&mut out, "disambiguation", row.str("comment"));
        put_list(
            &mut out,
            "relations",
            self.artist_relations("l_artist_work", row.id),
        );
        self.extras(&mut out, Kind::Work, row.id);
        out
    }

    fn label(&self, row: &Row<'_>) -> Obj {
        let mut out = Obj::new();
        out.insert("id".into(), Value::String(row.gid()));
        self.named(&mut out, "type", "label_type", row.u32("type"));
        out.insert("name".into(), Value::String(row.text("name")));
        out.insert("sort-name".into(), Value::String(row.text("name")));
        if let Some(code) = row.u32("label_code") {
            out.insert("label-code".into(), json!(code));
        }
        put(&mut out, "country", self.code(row.u32("area")).as_deref());
        if let Some(area) = self.area_by_id(row.u32("area")) {
            out.insert("area".into(), Value::Object(area));
        }
        put_list(
            &mut out,
            "ipis",
            strings(self.store.t("label_ipi").by("label", row.id), "ipi"),
        );
        put_list(
            &mut out,
            "isnis",
            strings(self.store.t("label_isni").by("label", row.id), "isni"),
        );
        out.insert("life-span".into(), life_span(row, true));
        put(&mut out, "disambiguation", row.str("comment"));
        self.extras(&mut out, Kind::Label, row.id);
        out
    }

    fn place(&self, row: &Row<'_>) -> Obj {
        let mut out = Obj::new();
        out.insert("id".into(), Value::String(row.gid()));
        self.named(&mut out, "type", "place_type", row.u32("type"));
        out.insert("name".into(), Value::String(row.text("name")));
        put(&mut out, "address", row.str("address"));
        // The search server has coordinates as strings.
        if let Some((lat, long)) = row
            .str("coordinates")
            .and_then(|c| c.trim_matches(|c| c == '(' || c == ')').split_once(','))
        {
            out.insert(
                "coordinates".into(),
                json!({"latitude": lat.trim(), "longitude": long.trim()}),
            );
        }
        if let Some(area) = self.area_by_id(row.u32("area")) {
            out.insert("area".into(), Value::Object(area));
        }
        out.insert("life-span".into(), life_span(row, true));
        put(&mut out, "disambiguation", row.str("comment"));
        self.extras(&mut out, Kind::Place, row.id);
        out
    }

    fn event(&self, row: &Row<'_>) -> Obj {
        let mut out = Obj::new();
        out.insert("id".into(), Value::String(row.gid()));
        if let Some((t, _)) = self.store.named("event_type", row.u32("type")) {
            out.insert("type".into(), Value::String(t.to_owned()));
        }
        out.insert("name".into(), Value::String(row.text("name")));
        put(
            &mut out,
            "time",
            row.str("time").map(|t| t.get(..5).unwrap_or(t)),
        );
        out.insert("life-span".into(), life_span(row, false));
        put(&mut out, "disambiguation", row.str("comment"));
        put_list(
            &mut out,
            "relations",
            self.artist_relations("l_artist_event", row.id),
        );
        self.extras(&mut out, Kind::Event, row.id);
        out
    }

    /// Instruments and series: name, type, disambiguation (and an instrument's description).
    fn simple(&self, row: &Row<'_>, kind: Kind, type_table: &str) -> Obj {
        let mut out = Obj::new();
        out.insert("id".into(), Value::String(row.gid()));
        self.named(&mut out, "type", type_table, row.u32("type"));
        out.insert("name".into(), Value::String(row.text("name")));
        if kind == Kind::Instrument {
            put(&mut out, "description", row.str("description"));
        }
        put(&mut out, "disambiguation", row.str("comment"));
        self.extras(&mut out, kind, row.id);
        out
    }
}
