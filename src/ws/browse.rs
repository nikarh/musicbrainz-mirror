//! Browsing: the entities linked to another one (`/ws/2/release?artist=<mbid>`), a page at a time, in the order of
//! musicbrainz-server's `find_by_*` queries (where it matters: which entities are on which page).

use super::entity::{Ctx, Filter};
use crate::kind::Kind;

type R<T> = rusqlite::Result<T>;

/// What entities are browsed by. The id is the database id of the linked entity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Linked {
    Area(i64),
    Artist(i64),
    /// Releases with the artist in a track's credit but not in the release's (`track_artist=`).
    TrackArtist(i64),
    Event(i64),
    Label(i64),
    Place(i64),
    Recording(i64),
    Release(i64),
    ReleaseGroup(i64),
    Series(i64),
    Track(i64),
    Work(i64),
}

#[derive(Debug, Clone, Copy)]
pub struct Page {
    pub limit: i64,
    pub offset: i64,
}

/// What a browse parameter stands for: see [`Linked::parse`].
pub type LinkedParam = (&'static str, Option<Kind>, fn(i64) -> Linked);

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

/// Areas: the area and the areas in it.
const IN_AREA: &str = "(SELECT ?1 UNION SELECT descendant FROM area_containment WHERE parent = ?1)";

/// Releases ordered as musicbrainz-server orders an artist's: first release date, catalog number, barcode, name.
const RELEASE_ORDER: &str = "(SELECT date FROM release_first_release_date WHERE release = e.id) NULLS LAST, \
    (SELECT min(catalog_number) FROM release_label WHERE release = e.id) NULLS LAST, \
    e.barcode NULLS LAST, e.name, e.id";
/// Releases by their earliest event, then name (musicbrainz-server's `find_by_recording`).
const RELEASE_DATE_ORDER: &str = "(SELECT min(coalesce(date_year, 9999) * 10000 + coalesce(date_month, 99) * 100 \
    + coalesce(date_day, 99)) FROM (SELECT date_year, date_month, date_day FROM release_country WHERE release = e.id \
    UNION ALL SELECT date_year, date_month, date_day FROM release_unknown_country WHERE release = e.id)) NULLS LAST, \
    e.name, e.id";
const EVENT_ORDER: &str = "e.begin_date_year DESC NULLS LAST, e.begin_date_month DESC NULLS LAST, \
    e.begin_date_day DESC NULLS LAST, e.time DESC NULLS LAST, e.name, e.id";

/// The query selecting the browsed entities' ids (`?1`: the linked entity's id), and their order (over `e`, the
/// entity's table). `None`: not a browse musicbrainz-server has.
#[allow(clippy::too_many_lines)] // a table
#[must_use]
pub fn query(kind: Kind, linked: Linked) -> Option<(String, &'static str)> {
    use Linked as L;
    let rel = |k0: Kind, k1: Kind, linked_side: u8| {
        let (mine, theirs) = if linked_side == 0 {
            ("entity1", "entity0")
        } else {
            ("entity0", "entity1")
        };
        format!(
            "SELECT {mine} FROM rel WHERE kind{s} = {} AND {theirs} = ?1 AND kind{o} = {}",
            if linked_side == 0 {
                k0.code()
            } else {
                k1.code()
            },
            if linked_side == 0 {
                k1.code()
            } else {
                k0.code()
            },
            s = linked_side,
            o = 1 - linked_side,
        )
    };
    let credited = |table: &str| {
        format!(
            "SELECT x.id FROM artist_credit_name acn JOIN {table} x ON x.artist_credit = acn.artist_credit \
             WHERE acn.artist = ?1"
        )
    };
    Some(match (kind, linked) {
        (Kind::Artist, L::Area(_)) => (
            format!(
                "SELECT id FROM artist WHERE area IN {IN_AREA} UNION SELECT id FROM artist WHERE begin_area IN \
                 {IN_AREA} UNION SELECT id FROM artist WHERE end_area IN {IN_AREA}"
            ),
            "e.name, e.id",
        ),
        (Kind::Artist, L::Recording(_)) => (
            "SELECT acn.artist FROM recording r JOIN artist_credit_name acn ON acn.artist_credit = r.artist_credit \
             WHERE r.id = ?1"
                .into(),
            "e.name, e.id",
        ),
        (Kind::Artist, L::Release(_)) => (
            "SELECT acn.artist FROM release r JOIN artist_credit_name acn ON acn.artist_credit = r.artist_credit \
             WHERE r.id = ?1 UNION SELECT acn.artist FROM medium m JOIN track t ON t.medium = m.id \
             JOIN artist_credit_name acn ON acn.artist_credit = t.artist_credit WHERE m.release = ?1"
                .into(),
            "e.name, e.id",
        ),
        (Kind::Artist, L::ReleaseGroup(_)) => (
            "SELECT acn.artist FROM release_group rg JOIN artist_credit_name acn \
             ON acn.artist_credit = rg.artist_credit WHERE rg.id = ?1 UNION SELECT acn.artist FROM release r \
             JOIN artist_credit_name acn ON acn.artist_credit = r.artist_credit WHERE r.release_group = ?1"
                .into(),
            "e.name, e.id",
        ),
        (Kind::Artist, L::Work(_)) => (
            format!(
                "{} UNION SELECT acn.artist FROM rel JOIN recording r ON r.id = rel.entity0 \
                 JOIN artist_credit_name acn ON acn.artist_credit = r.artist_credit \
                 WHERE rel.kind1 = 13 AND rel.entity1 = ?1 AND rel.kind0 = 8",
                rel(Kind::Artist, Kind::Work, 1)
            ),
            "e.name, e.id",
        ),
        (Kind::Event, L::Area(_)) => (rel(Kind::Area, Kind::Event, 0), EVENT_ORDER),
        (Kind::Event, L::Artist(_)) => (rel(Kind::Artist, Kind::Event, 0), EVENT_ORDER),
        (Kind::Event, L::Place(_)) => (rel(Kind::Event, Kind::Place, 1), EVENT_ORDER),
        (Kind::Event, L::Event(_)) => (
            format!("{} UNION {}", rel(Kind::Event, Kind::Event, 0), rel(Kind::Event, Kind::Event, 1)),
            EVENT_ORDER,
        ),
        (Kind::Event, L::Series(_)) => (rel(Kind::Event, Kind::Series, 1), EVENT_ORDER),
        (Kind::Label, L::Area(_)) => (format!("SELECT id FROM label WHERE area IN {IN_AREA}"), "e.name, e.id"),
        (Kind::Label, L::Release(_)) => (
            "SELECT label FROM release_label WHERE release = ?1 AND label IS NOT NULL".into(),
            "e.name, e.id",
        ),
        (Kind::Place, L::Area(_)) => (format!("SELECT id FROM place WHERE area IN {IN_AREA}"), "e.name, e.id"),
        (Kind::Recording, L::Artist(_)) => (credited("recording"), "e.name, e.comment, e.id"),
        (Kind::Recording, L::Release(_)) => (
            "SELECT t.recording FROM medium m JOIN track t ON t.medium = m.id WHERE m.release = ?1".into(),
            "e.name, e.id",
        ),
        (Kind::Recording, L::Work(_)) => (rel(Kind::Recording, Kind::Work, 1), "e.name, e.id"),
        (Kind::Release, L::Area(_)) => {
            ("SELECT release FROM release_country WHERE country = ?1".into(), "e.name, e.id")
        }
        (Kind::Release, L::Artist(_)) => (credited("release"), RELEASE_ORDER),
        (Kind::Release, L::TrackArtist(_)) => (
            "SELECT m.release FROM artist_credit_name acn JOIN track t ON t.artist_credit = acn.artist_credit \
             JOIN medium m ON m.id = t.medium WHERE acn.artist = ?1 \
             EXCEPT SELECT x.id FROM artist_credit_name acn JOIN release x ON x.artist_credit = acn.artist_credit \
             WHERE acn.artist = ?1"
                .into(),
            RELEASE_ORDER,
        ),
        (Kind::Release, L::Label(_)) => ("SELECT release FROM release_label WHERE label = ?1".into(), RELEASE_ORDER),
        (Kind::Release, L::Recording(_)) => (
            "SELECT m.release FROM track t JOIN medium m ON m.id = t.medium WHERE t.recording = ?1".into(),
            RELEASE_DATE_ORDER,
        ),
        (Kind::Release, L::ReleaseGroup(_)) => {
            ("SELECT id FROM release WHERE release_group = ?1".into(), RELEASE_ORDER)
        }
        (Kind::Release, L::Track(_)) => (
            "SELECT m.release FROM track t JOIN medium m ON m.id = t.medium WHERE t.id = ?1".into(),
            "e.id",
        ),
        (Kind::ReleaseGroup, L::Artist(_)) => (
            credited("release_group"),
            "e.type NULLS LAST, (SELECT first_release_date_year FROM release_group_meta WHERE id = e.id) NULLS LAST, \
             (SELECT first_release_date_month FROM release_group_meta WHERE id = e.id) NULLS LAST, \
             (SELECT first_release_date_day FROM release_group_meta WHERE id = e.id) NULLS LAST, e.name, e.id",
        ),
        (Kind::ReleaseGroup, L::Release(_)) => {
            ("SELECT release_group FROM release WHERE id = ?1".into(), "e.name, e.id")
        }
        (Kind::Work, L::Artist(_)) => (
            format!(
                "{} UNION SELECT rel.entity1 FROM artist_credit_name acn \
                 JOIN recording r ON r.artist_credit = acn.artist_credit \
                 JOIN rel ON rel.kind0 = 8 AND rel.entity0 = r.id AND rel.kind1 = 13 WHERE acn.artist = ?1",
                rel(Kind::Artist, Kind::Work, 0)
            ),
            "e.name, e.id",
        ),
        _ => return None,
    })
}

/// The filter conditions (over `e`) for `status=` and `type=`.
fn filter_sql(kind: Kind, filter: &Filter) -> String {
    let list = |ids: &[i64]| {
        ids.iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut conditions = Vec::new();
    match kind {
        Kind::Release => {
            if !filter.status.is_empty() {
                conditions.push(format!("e.status IN ({})", list(&filter.status)));
            }
            if !filter.primary_types.is_empty() {
                conditions.push(format!(
                    "EXISTS (SELECT 1 FROM release_group rg WHERE rg.id = e.release_group AND rg.type IN ({}))",
                    list(&filter.primary_types)
                ));
            }
            if !filter.secondary_types.is_empty() {
                conditions.push(format!(
                    "EXISTS (SELECT 1 FROM release_group_secondary_type_join st \
                     WHERE st.release_group = e.release_group AND st.secondary_type IN ({}))",
                    list(&filter.secondary_types)
                ));
            }
        }
        Kind::ReleaseGroup => {
            if !filter.primary_types.is_empty() {
                conditions.push(format!("e.type IN ({})", list(&filter.primary_types)));
            }
            if !filter.secondary_types.is_empty() {
                conditions.push(format!(
                    "EXISTS (SELECT 1 FROM release_group_secondary_type_join st \
                     WHERE st.release_group = e.id AND st.secondary_type IN ({}))",
                    list(&filter.secondary_types)
                ));
            }
        }
        _ => {}
    }
    if conditions.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", conditions.join(" AND "))
    }
}

/// The SQL of a page of a browse and of its count: (page with `?2` limit and `?3` offset, count).
#[must_use]
pub fn sql(kind: Kind, linked: Linked, filter: &Filter) -> Option<(String, String)> {
    let (inner, order) = query(kind, linked)?;
    let filter = filter_sql(kind, filter);
    let table = kind.table();
    Some((
        format!(
            "SELECT e.id FROM {table} e WHERE e.id IN ({inner}) {} ORDER BY {order} LIMIT ?2 OFFSET ?3",
            filter.replacen("WHERE", "AND", 1)
        ),
        format!(
            "SELECT count(*) FROM {table} e WHERE e.id IN ({inner}) {}",
            filter.replacen("WHERE", "AND", 1)
        ),
    ))
}

/// A page of the entities linked to another, and how many there are in all.
pub fn ids(ctx: &Ctx<'_>, kind: Kind, linked: Linked, page: Page) -> R<(Vec<i64>, i64)> {
    let Some((page_sql, count_sql)) = sql(kind, linked, ctx.filter) else {
        return Ok((Vec::new(), 0));
    };
    let id = match linked {
        Linked::Area(id)
        | Linked::Artist(id)
        | Linked::TrackArtist(id)
        | Linked::Event(id)
        | Linked::Label(id)
        | Linked::Place(id)
        | Linked::Recording(id)
        | Linked::Release(id)
        | Linked::ReleaseGroup(id)
        | Linked::Series(id)
        | Linked::Track(id)
        | Linked::Work(id) => id,
    };
    let ids = ctx
        .conn
        .prepare_cached(&page_sql)?
        .query_map((id, page.limit, page.offset), |r| r.get(0))?
        .collect::<R<Vec<i64>>>()?;
    let total = ctx
        .conn
        .prepare_cached(&count_sql)?
        .query_row([id], |r| r.get(0))?;
    Ok((ids, total))
}

/// Every browse there is, for the query plan tests.
#[cfg(test)]
#[must_use]
pub fn all() -> Vec<(Kind, Linked)> {
    let mut out = Vec::new();
    for kind in Kind::ALL {
        for param in params(kind) {
            if let Some((_, _, make)) = Linked::parse(param) {
                let linked = make(1);
                if query(kind, linked).is_some() {
                    out.push((kind, linked));
                }
            }
        }
    }
    out.push((Kind::Release, Linked::TrackArtist(1)));
    out
}
