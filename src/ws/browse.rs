//! Browsing: the entities linked to another one (`/ws/2/release?artist=<mbid>`), a page at a time, in the order of
//! musicbrainz-server's `find_by_*` queries (where it matters: which entities are on which page). The linked
//! entities are found through the store's indexes, then ordered and cut.

use std::collections::BTreeSet;

use super::entity::{Ctx, Filter, collation_key};
use crate::kind::Kind;
use crate::store::Store;
use crate::store::derive::pack;
use crate::store::file::NONE;

/// What entities are browsed by. The id is the linked entity's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Linked {
    Area(u32),
    Artist(u32),
    /// Releases with the artist in a track's credit but not in the release's (`track_artist=`).
    TrackArtist(u32),
    Event(u32),
    Label(u32),
    Place(u32),
    Recording(u32),
    Release(u32),
    ReleaseGroup(u32),
    Series(u32),
    Track(u32),
    Work(u32),
}

#[derive(Debug, Clone, Copy)]
pub struct Page {
    pub limit: usize,
    pub offset: usize,
}

/// What a browse parameter stands for: see [`Linked::parse`].
pub type LinkedParam = (&'static str, Option<Kind>, fn(u32) -> Linked);

impl Linked {
    /// A browse parameter: the table of the linked entity (to find it by MBID), its kind (for MBIDs it replaced;
    /// tracks have none), and the variant.
    #[must_use]
    pub fn parse(param: &str) -> Option<LinkedParam> {
        Some(match param {
            "area" => ("area", Some(Kind::Area), Self::Area),
            "artist" => ("artist", Some(Kind::Artist), Self::Artist),
            "track_artist" => ("artist", Some(Kind::Artist), Self::TrackArtist),
            "event" => ("event", Some(Kind::Event), Self::Event),
            "label" => ("label", Some(Kind::Label), Self::Label),
            "place" => ("place", Some(Kind::Place), Self::Place),
            "recording" => ("recording", Some(Kind::Recording), Self::Recording),
            "release" => ("release", Some(Kind::Release), Self::Release),
            "release-group" => (
                "release_group",
                Some(Kind::ReleaseGroup),
                Self::ReleaseGroup,
            ),
            "series" => ("series", Some(Kind::Series), Self::Series),
            "track" => ("track", None, Self::Track),
            "work" => ("work", Some(Kind::Work), Self::Work),
            _ => return None,
        })
    }
}

/// The browse parameters each entity type accepts, in the order they are looked for (`collection` is accepted and
/// answers 404: there are no collections here).
#[must_use]
pub fn params(kind: Kind) -> &'static [&'static str] {
    match kind {
        Kind::Artist => &[
            "area",
            "recording",
            "release",
            "release-group",
            "work",
            "collection",
        ],
        Kind::Event => &["area", "artist", "collection", "event", "place", "series"],
        Kind::Label => &["area", "release", "collection"],
        Kind::Place => &["area", "collection"],
        Kind::Recording => &["artist", "release", "collection", "work"],
        Kind::Release => &[
            "area",
            "track_artist",
            "artist",
            "label",
            "recording",
            "release-group",
            "track",
            "collection",
        ],
        Kind::ReleaseGroup => &["artist", "release", "collection"],
        Kind::Work => &["artist", "collection"],
        Kind::Area | Kind::Genre | Kind::Instrument | Kind::Series => &["collection"],
        Kind::Url => &[],
    }
}

/// The other side of relationships: entities of `other` related to `id` (of `kind`) through `l_<a>_<b>`.
fn related(store: &Store, kind: Kind, id: u32, other: Kind) -> Vec<u32> {
    let (a, b) = if kind.table() <= other.table() {
        (kind, other)
    } else {
        (other, kind)
    };
    let table = store.t(&format!("l_{}_{}", a.table(), b.table()));
    let mut out = Vec::new();
    if kind.table() <= other.table() {
        out.extend(table.by("entity0", id).filter_map(|r| r.u32("entity1")));
    }
    if kind.table() >= other.table() {
        out.extend(table.by("entity1", id).filter_map(|r| r.u32("entity0")));
    }
    out
}

/// The artist credits an artist is in.
fn credits_of(store: &Store, artist: u32) -> Vec<u32> {
    store
        .t("artist_credit_name")
        .by("artist", artist)
        .filter_map(|r| r.u32("artist_credit"))
        .collect()
}

fn credit_artists(store: &Store, ac: Option<u32>) -> Vec<u32> {
    store
        .t("artist_credit_name")
        .by("artist_credit", ac.unwrap_or(NONE))
        .filter_map(|r| r.u32("artist"))
        .collect()
}

/// An area and the areas in it.
fn area_and_descendants(store: &Store, area: u32) -> Vec<u32> {
    let mut out = vec![area];
    out.extend(store.derived.area_descendants(area));
    out
}

fn medium_release(store: &Store, medium: Option<u32>) -> Option<u32> {
    store.t("medium").get(medium?)?.u32("release")
}

/// The ids of the browsed entities (in no particular order); `None`: not a browse musicbrainz-server has.
#[allow(clippy::too_many_lines)]
#[must_use]
pub fn candidates(store: &Store, kind: Kind, linked: Linked) -> Option<BTreeSet<u32>> {
    use Linked as L;
    let mut set = BTreeSet::new();
    match (kind, linked) {
        (Kind::Artist, L::Area(area)) => {
            for a in area_and_descendants(store, area) {
                for column in ["area", "begin_area", "end_area"] {
                    set.extend(store.t("artist").by(column, a).map(|r| r.id));
                }
            }
        }
        (Kind::Artist, L::Recording(r)) => {
            set.extend(credit_artists(
                store,
                store.t("recording").get(r)?.u32("artist_credit"),
            ));
        }
        (Kind::Artist, L::Release(r)) => {
            set.extend(credit_artists(
                store,
                store.t("release").get(r)?.u32("artist_credit"),
            ));
            for m in store.t("medium").by("release", r) {
                for t in store.t("track").by("medium", m.id) {
                    set.extend(credit_artists(store, t.u32("artist_credit")));
                }
            }
        }
        (Kind::Artist, L::ReleaseGroup(rg)) => {
            set.extend(credit_artists(
                store,
                store.t("release_group").get(rg)?.u32("artist_credit"),
            ));
            for r in store.t("release").by("release_group", rg) {
                set.extend(credit_artists(store, r.u32("artist_credit")));
            }
        }
        (Kind::Artist, L::Work(w)) => {
            set.extend(related(store, Kind::Work, w, Kind::Artist));
            for r in related(store, Kind::Work, w, Kind::Recording) {
                set.extend(credit_artists(
                    store,
                    store
                        .t("recording")
                        .get(r)
                        .and_then(|r| r.u32("artist_credit")),
                ));
            }
        }
        (Kind::Event, L::Area(id)) => set.extend(related(store, Kind::Area, id, Kind::Event)),
        (Kind::Event, L::Artist(id)) => set.extend(related(store, Kind::Artist, id, Kind::Event)),
        (Kind::Event, L::Place(id)) => set.extend(related(store, Kind::Place, id, Kind::Event)),
        (Kind::Event, L::Event(id)) => set.extend(related(store, Kind::Event, id, Kind::Event)),
        (Kind::Event, L::Series(id)) => set.extend(related(store, Kind::Series, id, Kind::Event)),
        (Kind::Label, L::Area(area)) => {
            for a in area_and_descendants(store, area) {
                set.extend(store.t("label").by("area", a).map(|r| r.id));
            }
        }
        (Kind::Label, L::Release(r)) => {
            set.extend(
                store
                    .t("release_label")
                    .by("release", r)
                    .filter_map(|rl| rl.u32("label")),
            );
        }
        (Kind::Place, L::Area(area)) => {
            for a in area_and_descendants(store, area) {
                set.extend(store.t("place").by("area", a).map(|r| r.id));
            }
        }
        (Kind::Recording, L::Artist(a)) => {
            for ac in credits_of(store, a) {
                set.extend(store.t("recording").by("artist_credit", ac).map(|r| r.id));
            }
        }
        (Kind::Recording, L::Release(r)) => {
            for m in store.t("medium").by("release", r) {
                set.extend(
                    store
                        .t("track")
                        .by("medium", m.id)
                        .filter_map(|t| t.u32("recording")),
                );
            }
        }
        (Kind::Recording, L::Work(w)) => set.extend(related(store, Kind::Work, w, Kind::Recording)),
        (Kind::Release, L::Area(area)) => {
            set.extend(
                store
                    .t("release_country")
                    .by("country", area)
                    .filter_map(|e| e.u32("release")),
            );
        }
        (Kind::Release, L::Artist(a)) => {
            for ac in credits_of(store, a) {
                set.extend(store.t("release").by("artist_credit", ac).map(|r| r.id));
            }
        }
        (Kind::Release, L::TrackArtist(a)) => {
            let credits = credits_of(store, a);
            for ac in &credits {
                for t in store.t("track").by("artist_credit", *ac) {
                    set.extend(medium_release(store, t.u32("medium")));
                }
            }
            for ac in &credits {
                for r in store.t("release").by("artist_credit", *ac) {
                    set.remove(&r.id);
                }
            }
        }
        (Kind::Release, L::Label(l)) => {
            set.extend(
                store
                    .t("release_label")
                    .by("label", l)
                    .filter_map(|rl| rl.u32("release")),
            );
        }
        (Kind::Release, L::Recording(r)) => {
            for t in store.t("track").by("recording", r) {
                set.extend(medium_release(store, t.u32("medium")));
            }
        }
        (Kind::Release, L::ReleaseGroup(rg)) => {
            set.extend(store.t("release").by("release_group", rg).map(|r| r.id));
        }
        (Kind::Release, L::Track(t)) => {
            set.extend(medium_release(
                store,
                store.t("track").get(t)?.u32("medium"),
            ));
        }
        (Kind::ReleaseGroup, L::Artist(a)) => {
            for ac in credits_of(store, a) {
                set.extend(
                    store
                        .t("release_group")
                        .by("artist_credit", ac)
                        .map(|r| r.id),
                );
            }
        }
        (Kind::ReleaseGroup, L::Release(r)) => {
            set.extend(store.t("release").get(r)?.u32("release_group"));
        }
        (Kind::Work, L::Artist(a)) => {
            set.extend(related(store, Kind::Artist, a, Kind::Work));
            for ac in credits_of(store, a) {
                for r in store.t("recording").by("artist_credit", ac) {
                    set.extend(related(store, Kind::Recording, r.id, Kind::Work));
                }
            }
        }
        _ => return None,
    }
    Some(set)
}

fn passes(store: &Store, kind: Kind, id: u32, filter: &Filter) -> bool {
    let rg = match kind {
        Kind::Release => {
            let Some(r) = store.t("release").get(id) else {
                return false;
            };
            if !filter.status.is_empty()
                && !r.u32("status").is_some_and(|s| filter.status.contains(&s))
            {
                return false;
            }
            r.u32("release_group")
        }
        Kind::ReleaseGroup => Some(id),
        _ => return true,
    };
    let rg = rg.and_then(|g| store.t("release_group").get(g));
    if !filter.primary_types.is_empty()
        && !rg
            .and_then(|g| g.u32("type"))
            .is_some_and(|t| filter.primary_types.contains(&t))
    {
        return false;
    }
    if !filter.secondary_types.is_empty() {
        let has = rg.is_some_and(|g| {
            store
                .t("release_group_secondary_type_join")
                .by("release_group", g.id)
                .any(|j| {
                    j.u32("secondary_type")
                        .is_some_and(|t| filter.secondary_types.contains(&t))
                })
        });
        if !has {
            return false;
        }
    }
    true
}

/// A sort key: nulls last.
fn last<T: Ord>(v: Option<T>) -> (bool, Option<T>) {
    (v.is_none(), v)
}

type ReleaseKey = (
    (bool, Option<u32>),
    (bool, Option<String>),
    (bool, Option<String>),
    String,
    u32,
);

/// Releases as musicbrainz-server orders an artist's: first release date, catalog number, barcode, name.
fn release_key(store: &Store, id: u32) -> ReleaseKey {
    let r = store.t("release").get(id);
    let catno = store
        .t("release_label")
        .by("release", id)
        .filter_map(|rl| rl.str("catalog_number").map(str::to_owned))
        .min();
    (
        last(store.derived.release_first_date(id)),
        last(catno),
        last(r.and_then(|r| r.str("barcode").map(str::to_owned))),
        r.map(|r| collation_key(&r.text("name")))
            .unwrap_or_default(),
        id,
    )
}

/// The earliest event of a release, cancelled or not (musicbrainz-server's `find_by_recording`).
fn release_event_date(store: &Store, id: u32) -> Option<u32> {
    let countries = store
        .t("release_country")
        .by("release", id)
        .map(|e| pack(e.date("date")));
    let unknown = store
        .t("release_unknown_country")
        .by("release", id)
        .map(|e| pack(e.date("date")));
    countries.chain(unknown).min()
}

fn name_key(store: &Store, kind: Kind, id: u32) -> (String, u32) {
    (
        store
            .entity(kind, id)
            .map(|r| collation_key(&r.text("name")))
            .unwrap_or_default(),
        id,
    )
}

/// Orders browsed entities.
fn order(store: &Store, kind: Kind, linked: Linked, ids: &mut [u32]) {
    use Linked as L;
    match (kind, linked) {
        (Kind::Release, L::Artist(_) | L::TrackArtist(_) | L::Label(_) | L::ReleaseGroup(_)) => {
            ids.sort_by_cached_key(|id| release_key(store, *id));
        }
        (Kind::Release, L::Recording(_)) => {
            ids.sort_by_cached_key(|id| {
                (
                    last(release_event_date(store, *id)),
                    name_key(store, kind, *id),
                )
            });
        }
        (Kind::ReleaseGroup, L::Artist(_)) => ids.sort_by_cached_key(|id| {
            let rg = store.t("release_group").get(*id);
            let first = store
                .t("release_group_meta")
                .get(*id)
                .map(|m| m.date("first_release_date"));
            let first = first.map(|(y, m, d)| (last(y), last(m), last(d)));
            (
                last(rg.and_then(|g| g.u32("type"))),
                first,
                name_key(store, kind, *id),
            )
        }),
        (Kind::Recording, L::Artist(_)) => ids.sort_by_cached_key(|id| {
            let r = store.t("recording").get(*id);
            let comment = r
                .map(|r| collation_key(&r.text("comment")))
                .unwrap_or_default();
            (name_key(store, kind, *id).0, comment, *id)
        }),
        (Kind::Event, _) => ids.sort_by_cached_key(|id| {
            let e = store.t("event").get(*id);
            let (y, m, d) = e.map_or((None, None, None), |e| e.date("begin"));
            let desc = |v: Option<i64>| std::cmp::Reverse(v.unwrap_or(i64::MIN));
            let time = e.and_then(|e| e.str("time").map(str::to_owned));
            (
                desc(y),
                desc(m),
                desc(d),
                std::cmp::Reverse(time),
                name_key(store, kind, *id),
            )
        }),
        _ => ids.sort_by_cached_key(|id| name_key(store, kind, *id)),
    }
}

/// A page of the entities linked to another, and how many there are in all.
#[must_use]
pub fn ids(ctx: &Ctx<'_>, kind: Kind, linked: Linked, page: Page) -> (Vec<u32>, usize) {
    let store = ctx.store;
    let Some(set) = candidates(store, kind, linked) else {
        return (Vec::new(), 0);
    };
    let mut ids: Vec<u32> = set
        .into_iter()
        .filter(|id| passes(store, kind, *id, ctx.filter))
        .collect();
    let total = ids.len();
    order(store, kind, linked, &mut ids);
    (
        ids.into_iter().skip(page.offset).take(page.limit).collect(),
        total,
    )
}
