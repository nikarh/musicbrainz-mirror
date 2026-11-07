//! Every fixed SQL statement the web service runs. They are all listed in [`ALL`], so the tests can check that each
//! one is answered from an index (`tests::every_query_uses_indexes`); queries built at runtime (browse ordering,
//! search) have their own tests next to their builders.

pub const ARTIST: &str = "SELECT gid, name, sort_name, begin_date_year, begin_date_month, begin_date_day, \
    end_date_year, end_date_month, end_date_day, ended, type, area, gender, comment, begin_area, end_area \
    FROM artist WHERE id = ?1";
pub const AREA: &str = "SELECT gid, name, begin_date_year, begin_date_month, begin_date_day, \
    end_date_year, end_date_month, end_date_day, ended, type, comment FROM area WHERE id = ?1";
pub const AREA_ISO1: &str = "SELECT code FROM iso_3166_1 WHERE area = ?1 ORDER BY code";
pub const AREA_ISO2: &str = "SELECT code FROM iso_3166_2 WHERE area = ?1 ORDER BY code";
pub const AREA_ISO3: &str = "SELECT code FROM iso_3166_3 WHERE area = ?1 ORDER BY code";
pub const AREA_COUNTRY: &str = "SELECT code FROM area_country WHERE area = ?1";
pub const EVENT: &str = "SELECT gid, name, begin_date_year, begin_date_month, begin_date_day, \
    end_date_year, end_date_month, end_date_day, ended, type, time, cancelled, setlist, comment \
    FROM event WHERE id = ?1";
pub const GENRE: &str = "SELECT gid, name, comment FROM genre WHERE id = ?1";
pub const INSTRUMENT: &str =
    "SELECT gid, name, type, comment, description FROM instrument WHERE id = ?1";
pub const LABEL: &str = "SELECT gid, name, begin_date_year, begin_date_month, begin_date_day, \
    end_date_year, end_date_month, end_date_day, ended, type, label_code, area, comment FROM label WHERE id = ?1";
pub const PLACE: &str = "SELECT gid, name, begin_date_year, begin_date_month, begin_date_day, \
    end_date_year, end_date_month, end_date_day, ended, type, address, area, coordinates, comment \
    FROM place WHERE id = ?1";
pub const RECORDING: &str =
    "SELECT gid, name, artist_credit, length, comment, video FROM recording WHERE id = ?1";
pub const RECORDING_FIRST_RELEASE_DATE: &str =
    "SELECT date FROM recording_first_release_date WHERE recording = ?1";
pub const RECORDING_ISRCS: &str = "SELECT isrc FROM isrc WHERE recording = ?1 ORDER BY isrc";
pub const RELEASE: &str = "SELECT r.gid, r.name, r.artist_credit, r.release_group, r.status, r.packaging, \
    r.language, r.script, r.barcode, r.comment, r.quality, m.amazon_asin, m.cover_art_presence \
    FROM release r LEFT JOIN release_meta m ON m.id = r.id WHERE r.id = ?1";
pub const RELEASE_EVENTS: &str = "SELECT e.country, e.date_year, e.date_month, e.date_day FROM ( \
    SELECT country, date_year, date_month, date_day FROM release_country WHERE release = ?1 \
    UNION ALL SELECT NULL, date_year, date_month, date_day FROM release_unknown_country WHERE release = ?1 \
    ) e LEFT JOIN area a ON a.id = e.country \
    ORDER BY e.date_year NULLS LAST, e.date_month NULLS LAST, e.date_day NULLS LAST, a.name NULLS LAST";
pub const RELEASE_LABELS: &str = "SELECT rl.label, rl.catalog_number FROM release_label rl \
    LEFT JOIN label l ON l.id = rl.label WHERE rl.release = ?1 \
    ORDER BY rl.catalog_number NULLS LAST, l.name COLLATE NOCASE NULLS LAST, rl.id";
pub const RELEASE_MEDIUMS: &str = "SELECT id, gid, position, format, name, track_count FROM medium WHERE release = ?1 ORDER BY position, id";
pub const MEDIUM: &str =
    "SELECT id, gid, position, format, name, track_count FROM medium WHERE id = ?1";
pub const MEDIUM_TRACKS: &str = "SELECT id, gid, recording, position, number, name, artist_credit, length, \
    is_data_track FROM track WHERE medium = ?1 ORDER BY position, id";
pub const MEDIUM_CDTOC_TRACK_COUNT: &str =
    "SELECT count(*) FROM track WHERE medium = ?1 AND position > 0 AND NOT is_data_track";
pub const MEDIUM_HAS_PREGAP: &str =
    "SELECT EXISTS (SELECT 1 FROM track WHERE medium = ?1 AND position = 0)";
pub const MEDIUM_CDTOCS: &str = "SELECT c.discid, c.track_count, c.leadout_offset, c.track_offset \
    FROM medium_cdtoc mc JOIN cdtoc c ON c.id = mc.cdtoc WHERE mc.medium = ?1 ORDER BY c.discid";
pub const RECORDING_TRACK_ON_RELEASE: &str = "SELECT t.medium, t.id FROM track t JOIN medium m ON m.id = t.medium \
    WHERE t.recording = ?1 AND m.release = ?2 ORDER BY m.position, t.position LIMIT 1";
pub const TRACK: &str = "SELECT id, gid, recording, position, number, name, artist_credit, length, \
    is_data_track FROM track WHERE id = ?1";
pub const RELEASE_GROUP: &str = "SELECT rg.gid, rg.name, rg.artist_credit, rg.type, rg.comment, \
    m.first_release_date_year, m.first_release_date_month, m.first_release_date_day \
    FROM release_group rg LEFT JOIN release_group_meta m ON m.id = rg.id WHERE rg.id = ?1";
pub const RELEASE_GROUP_SECONDARY_TYPES: &str = "SELECT j.secondary_type FROM release_group_secondary_type_join j \
    JOIN release_group_secondary_type t ON t.id = j.secondary_type WHERE j.release_group = ?1 ORDER BY t.name";
pub const SERIES: &str = "SELECT gid, name, comment, type, ordering_type FROM series WHERE id = ?1";
pub const URL: &str = "SELECT gid, url FROM url WHERE id = ?1";
pub const WORK: &str = "SELECT gid, name, type, comment FROM work WHERE id = ?1";
pub const WORK_ISWCS: &str = "SELECT iswc FROM iswc WHERE work = ?1 ORDER BY iswc";
pub const WORK_LANGUAGES: &str = "SELECT language FROM work_language WHERE work = ?1";
pub const WORK_ATTRIBUTES: &str = "SELECT work_attribute_type, work_attribute_type_allowed_value, \
    work_attribute_text FROM work_attribute WHERE work = ?1 ORDER BY id";

pub const ARTIST_CREDIT: &str = "SELECT artist, name, join_phrase FROM artist_credit_name \
    WHERE artist_credit = ?1 ORDER BY position";
pub const ARTIST_IPIS: &str = "SELECT ipi FROM artist_ipi WHERE artist = ?1 ORDER BY ipi";
pub const ARTIST_ISNIS: &str = "SELECT isni FROM artist_isni WHERE artist = ?1 ORDER BY isni";
pub const LABEL_IPIS: &str = "SELECT ipi FROM label_ipi WHERE label = ?1 ORDER BY ipi";
pub const LABEL_ISNIS: &str = "SELECT isni FROM label_isni WHERE label = ?1 ORDER BY isni";

pub const ALIASES: &str = "SELECT name, sort_name, locale, type, primary_for_locale, begin_date_year, \
    begin_date_month, begin_date_day, end_date_year, end_date_month, end_date_day, ended \
    FROM alias WHERE kind = ?1 AND entity = ?2";
pub const TAGS: &str = "SELECT t.name, et.count FROM entity_tag et JOIN tag t ON t.id = et.tag \
    WHERE et.kind = ?1 AND et.entity = ?2 AND et.count > 0";
pub const GENRES: &str = "SELECT g.gid, g.name, g.comment, et.count FROM entity_tag et \
    JOIN tag t ON t.id = et.tag JOIN genre g ON g.name = t.name \
    WHERE et.kind = ?1 AND et.entity = ?2 AND et.count > 0";
pub const RATING: &str = "SELECT rating, rating_count FROM rating WHERE kind = ?1 AND entity = ?2";
pub const ANNOTATION: &str = "SELECT text FROM annotation WHERE kind = ?1 AND entity = ?2";

/// Relationships where the entity is entity0 (`?1` its kind, `?2` its id, `?3` the target kind).
pub const RELS_FORWARD: &str = "SELECT rel.rowid, rel.entity1, rel.link, rel.link_order, rel.entity0_credit, \
    rel.entity1_credit, l.link_type, l.begin_date_year, l.begin_date_month, l.begin_date_day, l.end_date_year, \
    l.end_date_month, l.end_date_day, l.ended FROM rel JOIN link l ON l.id = rel.link \
    WHERE rel.kind0 = ?1 AND rel.entity0 = ?2 AND rel.kind1 = ?3";
/// Relationships where the entity is entity1.
pub const RELS_BACKWARD: &str = "SELECT rel.rowid, rel.entity0, rel.link, rel.link_order, rel.entity1_credit, \
    rel.entity0_credit, l.link_type, l.begin_date_year, l.begin_date_month, l.begin_date_day, l.end_date_year, \
    l.end_date_month, l.end_date_day, l.ended FROM rel JOIN link l ON l.id = rel.link \
    WHERE rel.kind1 = ?1 AND rel.entity1 = ?2 AND rel.kind0 = ?3";
pub const LINK_ATTRIBUTES: &str = "SELECT la.attribute_type, \
    (SELECT text_value FROM link_attribute_text_value v WHERE v.link = la.link AND v.attribute_type = la.attribute_type), \
    (SELECT credited_as FROM link_attribute_credit c WHERE c.link = la.link AND c.attribute_type = la.attribute_type) \
    FROM link_attribute la WHERE la.link = ?1";

pub const COVER_ART_STATS: &str = "SELECT count(DISTINCT ca.id), \
    coalesce(max(ct.type_id = (SELECT id FROM art_type WHERE name = 'Front')), 0), \
    coalesce(max(ct.type_id = (SELECT id FROM art_type WHERE name = 'Back')), 0) \
    FROM cover_art ca LEFT JOIN cover_art_type ct ON ct.id = ca.id WHERE ca.release = ?1";
pub const COVER_ART: &str = "SELECT ca.id, ca.comment, ca.edit, ca.mime_type, \
    (SELECT suffix FROM image_type WHERE mime_type = ca.mime_type) \
    FROM cover_art ca WHERE ca.release = ?1 ORDER BY ca.ordering, ca.id";
pub const COVER_ART_TYPES: &str = "SELECT t.name FROM cover_art_type ct JOIN art_type t ON t.id = ct.type_id \
    WHERE ct.id = ?1 ORDER BY t.name";
pub const RELEASE_GROUP_COVER_RELEASE: &str =
    "SELECT release FROM release_group_cover_art WHERE release_group = ?1";
pub const RELEASE_GROUP_FIRST_COVER_RELEASE: &str = "SELECT r.id FROM release r \
    JOIN release_meta m ON m.id = r.id AND m.cover_art_presence = 'present' \
    LEFT JOIN release_first_release_date d ON d.release = r.id \
    WHERE r.release_group = ?1 \
    AND EXISTS (SELECT 1 FROM cover_art ca JOIN cover_art_type ct ON ct.id = ca.id \
        WHERE ca.release = r.id AND ct.type_id = (SELECT id FROM art_type WHERE name = 'Front')) \
    ORDER BY d.date NULLS LAST, r.id LIMIT 1";

/// MBIDs: an entity's id by its MBID, or by an MBID it replaced.
#[must_use]
pub fn by_gid(table: &str) -> String {
    format!("SELECT id FROM {table} WHERE gid = ?1")
}
/// What relationships to an entity of this kind sort by.
#[must_use]
pub fn sort_key(kind: crate::kind::Kind) -> String {
    use crate::kind::Kind;
    let column = match kind {
        Kind::Artist => "sort_name",
        Kind::Url => "url",
        _ => "name",
    };
    format!("SELECT {column} FROM {} WHERE id = ?1", kind.table())
}
pub const GID_REDIRECT: &str = "SELECT new_id FROM gid_redirect WHERE gid = ?1 AND kind = ?2";

pub const RECORDINGS_BY_ISRC: &str = "SELECT r.id FROM isrc i JOIN recording r ON r.id = i.recording WHERE i.isrc = ?1 ORDER BY r.name, r.id";
pub const WORKS_BY_ISWC: &str =
    "SELECT w.id FROM iswc i JOIN work w ON w.id = i.work WHERE i.iswc = ?1 ORDER BY w.name, w.id";
pub const CDTOC_BY_DISCID: &str =
    "SELECT id, discid, track_count, leadout_offset, track_offset FROM cdtoc WHERE discid = ?1";
pub const RELEASES_BY_CDTOC: &str = "SELECT DISTINCT m.release FROM medium_cdtoc mc \
    JOIN medium m ON m.id = mc.medium WHERE mc.cdtoc = ?1";
pub const URL_BY_RESOURCE: &str = "SELECT id FROM url WHERE url = ?1";
pub const GENRES_ALL: &str = "SELECT id FROM genre ORDER BY name, id LIMIT ?1 OFFSET ?2";
pub const GENRES_COUNT: &str = "SELECT count(*) FROM genre";
pub const GENRE_NAMES: &str = "SELECT name FROM genre ORDER BY name";

/// Every statement above, with its name.
pub const ALL: &[(&str, &str)] = &[
    ("ARTIST", ARTIST),
    ("AREA", AREA),
    ("AREA_ISO1", AREA_ISO1),
    ("AREA_ISO2", AREA_ISO2),
    ("AREA_ISO3", AREA_ISO3),
    ("AREA_COUNTRY", AREA_COUNTRY),
    ("EVENT", EVENT),
    ("GENRE", GENRE),
    ("INSTRUMENT", INSTRUMENT),
    ("LABEL", LABEL),
    ("PLACE", PLACE),
    ("RECORDING", RECORDING),
    ("RECORDING_FIRST_RELEASE_DATE", RECORDING_FIRST_RELEASE_DATE),
    ("RECORDING_ISRCS", RECORDING_ISRCS),
    ("RELEASE", RELEASE),
    ("RELEASE_EVENTS", RELEASE_EVENTS),
    ("RELEASE_LABELS", RELEASE_LABELS),
    ("RELEASE_MEDIUMS", RELEASE_MEDIUMS),
    ("MEDIUM", MEDIUM),
    ("MEDIUM_TRACKS", MEDIUM_TRACKS),
    ("MEDIUM_CDTOC_TRACK_COUNT", MEDIUM_CDTOC_TRACK_COUNT),
    ("MEDIUM_HAS_PREGAP", MEDIUM_HAS_PREGAP),
    ("MEDIUM_CDTOCS", MEDIUM_CDTOCS),
    ("RECORDING_TRACK_ON_RELEASE", RECORDING_TRACK_ON_RELEASE),
    ("TRACK", TRACK),
    ("RELEASE_GROUP", RELEASE_GROUP),
    (
        "RELEASE_GROUP_SECONDARY_TYPES",
        RELEASE_GROUP_SECONDARY_TYPES,
    ),
    ("SERIES", SERIES),
    ("URL", URL),
    ("WORK", WORK),
    ("WORK_ISWCS", WORK_ISWCS),
    ("WORK_LANGUAGES", WORK_LANGUAGES),
    ("WORK_ATTRIBUTES", WORK_ATTRIBUTES),
    ("ARTIST_CREDIT", ARTIST_CREDIT),
    ("ARTIST_IPIS", ARTIST_IPIS),
    ("ARTIST_ISNIS", ARTIST_ISNIS),
    ("LABEL_IPIS", LABEL_IPIS),
    ("LABEL_ISNIS", LABEL_ISNIS),
    ("ALIASES", ALIASES),
    ("TAGS", TAGS),
    ("GENRES", GENRES),
    ("RATING", RATING),
    ("ANNOTATION", ANNOTATION),
    ("RELS_FORWARD", RELS_FORWARD),
    ("RELS_BACKWARD", RELS_BACKWARD),
    ("LINK_ATTRIBUTES", LINK_ATTRIBUTES),
    ("COVER_ART_STATS", COVER_ART_STATS),
    ("COVER_ART", COVER_ART),
    ("COVER_ART_TYPES", COVER_ART_TYPES),
    ("RELEASE_GROUP_COVER_RELEASE", RELEASE_GROUP_COVER_RELEASE),
    (
        "RELEASE_GROUP_FIRST_COVER_RELEASE",
        RELEASE_GROUP_FIRST_COVER_RELEASE,
    ),
    ("GID_REDIRECT", GID_REDIRECT),
    ("RECORDINGS_BY_ISRC", RECORDINGS_BY_ISRC),
    ("WORKS_BY_ISWC", WORKS_BY_ISWC),
    ("CDTOC_BY_DISCID", CDTOC_BY_DISCID),
    ("RELEASES_BY_CDTOC", RELEASES_BY_CDTOC),
    ("URL_BY_RESOURCE", URL_BY_RESOURCE),
    ("GENRES_ALL", GENRES_ALL),
    ("GENRES_COUNT", GENRES_COUNT),
    ("GENRE_NAMES", GENRE_NAMES),
];
