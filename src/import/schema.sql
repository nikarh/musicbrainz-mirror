-- The database built from a MusicBrainz dump. Tables and columns keep MusicBrainz's names (admin/sql/CreateTables.sql
-- of musicbrainz-server), with only the columns the web service needs. Tables MusicBrainz has once per entity type
-- (aliases, tags, ratings, annotations, MBID redirects, relationships) are one table here, with the entity type in
-- `kind` (src/kind.rs). Indexes are created after the data is in (indexes.sql), derived tables last (derive.sql).

CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL) WITHOUT ROWID;

-- Lookup tables, small; read into memory when the database is opened.
CREATE TABLE area_type (id INTEGER PRIMARY KEY, name TEXT NOT NULL, gid TEXT);
CREATE TABLE artist_type (id INTEGER PRIMARY KEY, name TEXT NOT NULL, gid TEXT);
CREATE TABLE event_type (id INTEGER PRIMARY KEY, name TEXT NOT NULL, gid TEXT);
CREATE TABLE instrument_type (id INTEGER PRIMARY KEY, name TEXT NOT NULL, gid TEXT);
CREATE TABLE label_type (id INTEGER PRIMARY KEY, name TEXT NOT NULL, gid TEXT);
CREATE TABLE place_type (id INTEGER PRIMARY KEY, name TEXT NOT NULL, gid TEXT);
CREATE TABLE series_type (id INTEGER PRIMARY KEY, name TEXT NOT NULL, gid TEXT);
CREATE TABLE work_type (id INTEGER PRIMARY KEY, name TEXT NOT NULL, gid TEXT);
CREATE TABLE gender (id INTEGER PRIMARY KEY, name TEXT NOT NULL, gid TEXT);
CREATE TABLE release_status (id INTEGER PRIMARY KEY, name TEXT NOT NULL, gid TEXT);
CREATE TABLE release_packaging (id INTEGER PRIMARY KEY, name TEXT NOT NULL, gid TEXT);
CREATE TABLE release_group_primary_type (id INTEGER PRIMARY KEY, name TEXT NOT NULL, gid TEXT);
CREATE TABLE release_group_secondary_type (id INTEGER PRIMARY KEY, name TEXT NOT NULL, gid TEXT);
CREATE TABLE medium_format (id INTEGER PRIMARY KEY, name TEXT NOT NULL, gid TEXT);
CREATE TABLE series_ordering_type (id INTEGER PRIMARY KEY, name TEXT NOT NULL, gid TEXT);
CREATE TABLE work_attribute_type (id INTEGER PRIMARY KEY, name TEXT NOT NULL, gid TEXT, free_text INTEGER);
CREATE TABLE work_attribute_type_allowed_value (
    id INTEGER PRIMARY KEY, work_attribute_type INTEGER, value TEXT, gid TEXT
);
CREATE TABLE alias_type (kind INTEGER NOT NULL, id INTEGER NOT NULL, name TEXT NOT NULL, gid TEXT,
    PRIMARY KEY (kind, id)) WITHOUT ROWID;
CREATE TABLE language (id INTEGER PRIMARY KEY, iso_code_3 TEXT, name TEXT);
CREATE TABLE script (id INTEGER PRIMARY KEY, iso_code TEXT, name TEXT);
CREATE TABLE link_type (
    id INTEGER PRIMARY KEY, gid TEXT, entity_type0 TEXT, entity_type1 TEXT, name TEXT NOT NULL,
    entity0_cardinality INTEGER, entity1_cardinality INTEGER
);
CREATE TABLE link_attribute_type (id INTEGER PRIMARY KEY, parent INTEGER, root INTEGER, gid TEXT, name TEXT NOT NULL);
CREATE TABLE link_creditable_attribute_type (attribute_type INTEGER PRIMARY KEY);
CREATE TABLE art_type (id INTEGER PRIMARY KEY, name TEXT NOT NULL, gid TEXT);
CREATE TABLE image_type (mime_type TEXT PRIMARY KEY, suffix TEXT NOT NULL) WITHOUT ROWID;

-- Entities
CREATE TABLE area (
    id INTEGER PRIMARY KEY, gid TEXT NOT NULL, name TEXT NOT NULL, type INTEGER,
    begin_date_year INTEGER, begin_date_month INTEGER, begin_date_day INTEGER,
    end_date_year INTEGER, end_date_month INTEGER, end_date_day INTEGER, ended INTEGER, comment TEXT
);
CREATE TABLE iso_3166_1 (area INTEGER NOT NULL, code TEXT NOT NULL);
CREATE TABLE iso_3166_2 (area INTEGER NOT NULL, code TEXT NOT NULL);
CREATE TABLE iso_3166_3 (area INTEGER NOT NULL, code TEXT NOT NULL);
CREATE TABLE country_area (area INTEGER PRIMARY KEY);

CREATE TABLE artist (
    id INTEGER PRIMARY KEY, gid TEXT NOT NULL, name TEXT NOT NULL, sort_name TEXT NOT NULL,
    begin_date_year INTEGER, begin_date_month INTEGER, begin_date_day INTEGER,
    end_date_year INTEGER, end_date_month INTEGER, end_date_day INTEGER,
    type INTEGER, area INTEGER, gender INTEGER, comment TEXT, ended INTEGER, begin_area INTEGER, end_area INTEGER
);
CREATE TABLE artist_ipi (artist INTEGER NOT NULL, ipi TEXT NOT NULL);
CREATE TABLE artist_isni (artist INTEGER NOT NULL, isni TEXT NOT NULL);
CREATE TABLE artist_credit (id INTEGER PRIMARY KEY, name TEXT NOT NULL, gid TEXT);
CREATE TABLE artist_credit_name (
    artist_credit INTEGER NOT NULL, position INTEGER NOT NULL, artist INTEGER NOT NULL, name TEXT NOT NULL,
    join_phrase TEXT
);

CREATE TABLE event (
    id INTEGER PRIMARY KEY, gid TEXT NOT NULL, name TEXT NOT NULL,
    begin_date_year INTEGER, begin_date_month INTEGER, begin_date_day INTEGER,
    end_date_year INTEGER, end_date_month INTEGER, end_date_day INTEGER,
    time TEXT, type INTEGER, cancelled INTEGER, setlist TEXT, comment TEXT, ended INTEGER
);

CREATE TABLE genre (id INTEGER PRIMARY KEY, gid TEXT NOT NULL, name TEXT NOT NULL, comment TEXT);

CREATE TABLE instrument (
    id INTEGER PRIMARY KEY, gid TEXT NOT NULL, name TEXT NOT NULL, type INTEGER, comment TEXT, description TEXT
);

CREATE TABLE label (
    id INTEGER PRIMARY KEY, gid TEXT NOT NULL, name TEXT NOT NULL,
    begin_date_year INTEGER, begin_date_month INTEGER, begin_date_day INTEGER,
    end_date_year INTEGER, end_date_month INTEGER, end_date_day INTEGER,
    label_code INTEGER, type INTEGER, area INTEGER, comment TEXT, ended INTEGER
);
CREATE TABLE label_ipi (label INTEGER NOT NULL, ipi TEXT NOT NULL);
CREATE TABLE label_isni (label INTEGER NOT NULL, isni TEXT NOT NULL);

CREATE TABLE place (
    id INTEGER PRIMARY KEY, gid TEXT NOT NULL, name TEXT NOT NULL, type INTEGER, address TEXT, area INTEGER,
    coordinates TEXT, comment TEXT,
    begin_date_year INTEGER, begin_date_month INTEGER, begin_date_day INTEGER,
    end_date_year INTEGER, end_date_month INTEGER, end_date_day INTEGER, ended INTEGER
);

CREATE TABLE recording (
    id INTEGER PRIMARY KEY, gid TEXT NOT NULL, name TEXT NOT NULL, artist_credit INTEGER NOT NULL, length INTEGER,
    comment TEXT, video INTEGER
);
CREATE TABLE isrc (recording INTEGER NOT NULL, isrc TEXT NOT NULL);

CREATE TABLE release (
    id INTEGER PRIMARY KEY, gid TEXT NOT NULL, name TEXT NOT NULL, artist_credit INTEGER NOT NULL,
    release_group INTEGER NOT NULL, status INTEGER, packaging INTEGER, language INTEGER, script INTEGER,
    barcode TEXT, comment TEXT, quality INTEGER
);
CREATE TABLE release_country (
    release INTEGER NOT NULL, country INTEGER NOT NULL, date_year INTEGER, date_month INTEGER, date_day INTEGER
);
CREATE TABLE release_unknown_country (
    release INTEGER NOT NULL, date_year INTEGER, date_month INTEGER, date_day INTEGER
);
CREATE TABLE release_label (id INTEGER PRIMARY KEY, release INTEGER NOT NULL, label INTEGER, catalog_number TEXT);
CREATE TABLE release_meta (id INTEGER PRIMARY KEY, amazon_asin TEXT, cover_art_presence TEXT);

CREATE TABLE release_group (
    id INTEGER PRIMARY KEY, gid TEXT NOT NULL, name TEXT NOT NULL, artist_credit INTEGER NOT NULL, type INTEGER,
    comment TEXT
);
CREATE TABLE release_group_meta (
    id INTEGER PRIMARY KEY, release_count INTEGER,
    first_release_date_year INTEGER, first_release_date_month INTEGER, first_release_date_day INTEGER,
    rating INTEGER, rating_count INTEGER
);
CREATE TABLE release_group_secondary_type_join (release_group INTEGER NOT NULL, secondary_type INTEGER NOT NULL);

CREATE TABLE medium (
    id INTEGER PRIMARY KEY, release INTEGER NOT NULL, position INTEGER NOT NULL, format INTEGER, name TEXT,
    track_count INTEGER NOT NULL, gid TEXT
);
CREATE TABLE track (
    id INTEGER PRIMARY KEY, gid TEXT NOT NULL, recording INTEGER NOT NULL, medium INTEGER NOT NULL,
    position INTEGER NOT NULL, number TEXT, name TEXT NOT NULL, artist_credit INTEGER NOT NULL, length INTEGER,
    is_data_track INTEGER
);
CREATE TABLE cdtoc (
    id INTEGER PRIMARY KEY, discid TEXT NOT NULL, freedb_id TEXT, track_count INTEGER, leadout_offset INTEGER,
    track_offset TEXT
);
CREATE TABLE medium_cdtoc (id INTEGER PRIMARY KEY, medium INTEGER NOT NULL, cdtoc INTEGER NOT NULL);

CREATE TABLE series (
    id INTEGER PRIMARY KEY, gid TEXT NOT NULL, name TEXT NOT NULL, comment TEXT, type INTEGER, ordering_type INTEGER
);

CREATE TABLE url (id INTEGER PRIMARY KEY, gid TEXT NOT NULL, url TEXT NOT NULL);

CREATE TABLE work (id INTEGER PRIMARY KEY, gid TEXT NOT NULL, name TEXT NOT NULL, type INTEGER, comment TEXT);
CREATE TABLE work_language (work INTEGER NOT NULL, language INTEGER NOT NULL);
CREATE TABLE iswc (work INTEGER NOT NULL, iswc TEXT NOT NULL);
CREATE TABLE work_attribute (
    id INTEGER PRIMARY KEY, work INTEGER NOT NULL, work_attribute_type INTEGER NOT NULL,
    work_attribute_type_allowed_value INTEGER, work_attribute_text TEXT
);

-- Relationships: every l_<a>_<b> table in one.
CREATE TABLE link (
    id INTEGER PRIMARY KEY, link_type INTEGER NOT NULL,
    begin_date_year INTEGER, begin_date_month INTEGER, begin_date_day INTEGER,
    end_date_year INTEGER, end_date_month INTEGER, end_date_day INTEGER, ended INTEGER
);
CREATE TABLE link_attribute (link INTEGER NOT NULL, attribute_type INTEGER NOT NULL);
CREATE TABLE link_attribute_credit (link INTEGER NOT NULL, attribute_type INTEGER NOT NULL, credited_as TEXT);
CREATE TABLE link_attribute_text_value (link INTEGER NOT NULL, attribute_type INTEGER NOT NULL, text_value TEXT);
CREATE TABLE rel (
    kind0 INTEGER NOT NULL, entity0 INTEGER NOT NULL, kind1 INTEGER NOT NULL, entity1 INTEGER NOT NULL,
    link INTEGER NOT NULL, link_order INTEGER, entity0_credit TEXT, entity1_credit TEXT
);

-- Per entity type tables, merged.
CREATE TABLE alias (
    kind INTEGER NOT NULL, entity INTEGER NOT NULL, name TEXT NOT NULL, sort_name TEXT NOT NULL, locale TEXT,
    type INTEGER, primary_for_locale INTEGER,
    begin_date_year INTEGER, begin_date_month INTEGER, begin_date_day INTEGER,
    end_date_year INTEGER, end_date_month INTEGER, end_date_day INTEGER, ended INTEGER
);
CREATE TABLE tag (id INTEGER PRIMARY KEY, name TEXT NOT NULL);
CREATE TABLE entity_tag (kind INTEGER NOT NULL, entity INTEGER NOT NULL, tag INTEGER NOT NULL, count INTEGER NOT NULL);
CREATE TABLE rating (kind INTEGER NOT NULL, entity INTEGER NOT NULL, rating INTEGER, rating_count INTEGER);
CREATE TABLE gid_redirect (kind INTEGER NOT NULL, gid TEXT NOT NULL, new_id INTEGER NOT NULL);
-- Every version of every annotation; derive.sql keeps the latest of each entity in `annotation`.
CREATE TABLE annotation_text (id INTEGER PRIMARY KEY, text TEXT, created TEXT);
CREATE TABLE annotation_link (kind INTEGER NOT NULL, entity INTEGER NOT NULL, annotation INTEGER NOT NULL);

-- Cover Art Archive (the images themselves stay at the Internet Archive)
CREATE TABLE cover_art (
    id INTEGER PRIMARY KEY, release INTEGER NOT NULL, comment TEXT, edit INTEGER, ordering INTEGER, mime_type TEXT
);
CREATE TABLE cover_art_type (id INTEGER NOT NULL, type_id INTEGER NOT NULL);
CREATE TABLE release_group_cover_art (release_group INTEGER PRIMARY KEY, release INTEGER NOT NULL);
