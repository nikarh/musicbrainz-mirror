-- Built from the imported tables once their indexes exist: what MusicBrainz keeps in materialized tables, the latest
-- annotations, and the search indexes.

-- The latest version of each entity's annotation (MusicBrainz shows only that one).
CREATE TABLE annotation (kind INTEGER NOT NULL, entity INTEGER NOT NULL, text TEXT, PRIMARY KEY (kind, entity))
    WITHOUT ROWID;
INSERT INTO annotation (kind, entity, text)
SELECT kind, entity, text FROM (
    SELECT l.kind, l.entity, a.text,
        row_number() OVER (PARTITION BY l.kind, l.entity ORDER BY a.created DESC, a.id DESC) AS n
    FROM annotation_link l JOIN annotation_text a ON a.id = l.annotation
) WHERE n = 1;
DROP TABLE annotation_link;
DROP TABLE annotation_text;

-- Release groups' ratings, kept with their other derived values in release_group_meta.
INSERT INTO rating (kind, entity, rating, rating_count)
SELECT 10, id, rating, rating_count FROM release_group_meta WHERE rating_count > 0;

-- First release dates, as musicbrainz-server's release_first_release_date and recording_first_release_date: the
-- earliest release event (missing parts sort last), not counting cancelled releases. A date is stored packed as
-- year * 10000 + month * 100 + day, with 9999, 99 and 99 for missing parts (src/db/date.rs).
CREATE TABLE release_first_release_date (release INTEGER PRIMARY KEY, date INTEGER NOT NULL);
INSERT INTO release_first_release_date (release, date)
SELECT d.release, min(coalesce(d.date_year, 9999) * 10000 + coalesce(d.date_month, 99) * 100 + coalesce(d.date_day, 99))
FROM (
    SELECT release, date_year, date_month, date_day FROM release_country
    WHERE date_year IS NOT NULL OR date_month IS NOT NULL OR date_day IS NOT NULL
    UNION ALL
    SELECT release, date_year, date_month, date_day FROM release_unknown_country
) d
JOIN release r ON r.id = d.release
WHERE r.status IS NOT 6
GROUP BY d.release;

CREATE TABLE recording_first_release_date (recording INTEGER PRIMARY KEY, date INTEGER NOT NULL);
INSERT INTO recording_first_release_date (recording, date)
SELECT t.recording, min(f.date)
FROM track t
JOIN medium m ON m.id = t.medium
JOIN release_first_release_date f ON f.release = m.release
GROUP BY t.recording;

-- Which areas are in which ("part of" relationships, transitively), as musicbrainz-server's area_containment:
-- browsing by an area includes what is in the areas it contains.
CREATE TABLE area_containment (descendant INTEGER NOT NULL, parent INTEGER NOT NULL, depth INTEGER NOT NULL);
WITH RECURSIVE
    part_of (parent, child) AS (
        SELECT rel.entity0, rel.entity1 FROM rel
        JOIN link l ON l.id = rel.link JOIN link_type lt ON lt.id = l.link_type
        WHERE rel.kind0 = 1 AND rel.kind1 = 1 AND lt.gid = 'de7cc874-8b1b-3a05-8272-f3834c968fb7'
    ),
    up (descendant, parent, depth) AS (
        SELECT child, parent, 1 FROM part_of
        UNION
        SELECT up.descendant, p.parent, up.depth + 1 FROM up JOIN part_of p ON p.child = up.parent
        WHERE up.depth < 10
    )
INSERT INTO area_containment (descendant, parent, depth)
SELECT descendant, parent, min(depth) FROM up GROUP BY descendant, parent;
CREATE INDEX area_containment_parent ON area_containment (parent);
CREATE INDEX area_containment_descendant ON area_containment (descendant, depth);

-- Artists' country: the ISO 3166-1 code of their area, or of the nearest country containing it, as
-- musicbrainz-server's Artist::load_country_code.
CREATE TABLE area_country (area INTEGER PRIMARY KEY, code TEXT NOT NULL);
WITH candidates (area, code, depth) AS (
    SELECT area, code, 0 FROM iso_3166_1
    UNION ALL
    SELECT ac.descendant, iso.code, ac.depth FROM area_containment ac
    JOIN area a ON a.id = ac.parent AND a.type = 1
    JOIN iso_3166_1 iso ON iso.area = a.id
)
INSERT INTO area_country (area, code)
SELECT area, code FROM (
    SELECT area, code, row_number() OVER (PARTITION BY area ORDER BY depth, code) AS n FROM candidates
) WHERE n = 1;

-- Search. Contentless FTS5 tables: a row's rowid is the entity's id; the text is only indexed. Accents are folded and
-- case ignored, as MusicBrainz's search does.
CREATE VIRTUAL TABLE artist_fts USING fts5(
    name, sortname, alias, comment, content='', tokenize='unicode61 remove_diacritics 2'
);
INSERT INTO artist_fts (rowid, name, sortname, alias, comment)
SELECT a.id, a.name, a.sort_name,
    (SELECT group_concat(al.name || ' ' || al.sort_name, ' ') FROM alias al WHERE al.kind = 2 AND al.entity = a.id),
    a.comment
FROM artist a;

CREATE VIRTUAL TABLE release_group_fts USING fts5(
    name, alias, artist, comment, content='', tokenize='unicode61 remove_diacritics 2'
);
INSERT INTO release_group_fts (rowid, name, alias, artist, comment)
SELECT rg.id, rg.name,
    (SELECT group_concat(al.name, ' ') FROM alias al WHERE al.kind = 10 AND al.entity = rg.id),
    (SELECT group_concat(n, ' ') FROM (
        SELECT acn.name AS n FROM artist_credit_name acn WHERE acn.artist_credit = rg.artist_credit
        UNION SELECT a.name FROM artist_credit_name acn JOIN artist a ON a.id = acn.artist
            WHERE acn.artist_credit = rg.artist_credit)),
    rg.comment
FROM release_group rg;

CREATE VIRTUAL TABLE release_fts USING fts5(
    name, alias, artist, label, catno, comment, content='', tokenize='unicode61 remove_diacritics 2'
);
INSERT INTO release_fts (rowid, name, alias, artist, label, catno, comment)
SELECT r.id, r.name,
    (SELECT group_concat(al.name, ' ') FROM alias al WHERE al.kind = 9 AND al.entity = r.id),
    (SELECT group_concat(n, ' ') FROM (
        SELECT acn.name AS n FROM artist_credit_name acn WHERE acn.artist_credit = r.artist_credit
        UNION SELECT a.name FROM artist_credit_name acn JOIN artist a ON a.id = acn.artist
            WHERE acn.artist_credit = r.artist_credit)),
    (SELECT group_concat(l.name, ' ') FROM release_label rl JOIN label l ON l.id = rl.label WHERE rl.release = r.id),
    (SELECT group_concat(rl.catalog_number, ' ') FROM release_label rl WHERE rl.release = r.id),
    r.comment
FROM release r;

CREATE VIRTUAL TABLE recording_fts USING fts5(
    name, alias, artist, release, comment, content='', tokenize='unicode61 remove_diacritics 2'
);
INSERT INTO recording_fts (rowid, name, alias, artist, release, comment)
SELECT r.id, r.name,
    (SELECT group_concat(al.name, ' ') FROM alias al WHERE al.kind = 8 AND al.entity = r.id),
    (SELECT group_concat(n, ' ') FROM (
        SELECT acn.name AS n FROM artist_credit_name acn WHERE acn.artist_credit = r.artist_credit
        UNION SELECT a.name FROM artist_credit_name acn JOIN artist a ON a.id = acn.artist
            WHERE acn.artist_credit = r.artist_credit)),
    (SELECT group_concat(n, ' ') FROM (
        SELECT DISTINCT rl.name AS n FROM track t
        JOIN medium m ON m.id = t.medium JOIN release rl ON rl.id = m.release
        WHERE t.recording = r.id)),
    r.comment
FROM recording r;

CREATE VIRTUAL TABLE work_fts USING fts5(
    name, alias, artist, comment, content='', tokenize='unicode61 remove_diacritics 2'
);
INSERT INTO work_fts (rowid, name, alias, artist, comment)
SELECT w.id, w.name,
    (SELECT group_concat(al.name, ' ') FROM alias al WHERE al.kind = 13 AND al.entity = w.id),
    (SELECT group_concat(n, ' ') FROM (
        SELECT DISTINCT a.name AS n FROM rel JOIN artist a ON a.id = rel.entity0
        WHERE rel.kind1 = 13 AND rel.entity1 = w.id AND rel.kind0 = 2)),
    w.comment
FROM work w;

CREATE VIRTUAL TABLE label_fts USING fts5(name, alias, comment, content='', tokenize='unicode61 remove_diacritics 2');
INSERT INTO label_fts (rowid, name, alias, comment)
SELECT l.id, l.name,
    (SELECT group_concat(al.name || ' ' || al.sort_name, ' ') FROM alias al WHERE al.kind = 6 AND al.entity = l.id),
    l.comment
FROM label l;

CREATE VIRTUAL TABLE area_fts USING fts5(name, alias, comment, content='', tokenize='unicode61 remove_diacritics 2');
INSERT INTO area_fts (rowid, name, alias, comment)
SELECT x.id, x.name,
    (SELECT group_concat(al.name || ' ' || al.sort_name, ' ') FROM alias al WHERE al.kind = 1 AND al.entity = x.id),
    x.comment
FROM area x;

CREATE VIRTUAL TABLE place_fts USING fts5(
    name, alias, address, comment, content='', tokenize='unicode61 remove_diacritics 2'
);
INSERT INTO place_fts (rowid, name, alias, address, comment)
SELECT x.id, x.name,
    (SELECT group_concat(al.name, ' ') FROM alias al WHERE al.kind = 7 AND al.entity = x.id),
    x.address, x.comment
FROM place x;

CREATE VIRTUAL TABLE event_fts USING fts5(name, alias, comment, content='', tokenize='unicode61 remove_diacritics 2');
INSERT INTO event_fts (rowid, name, alias, comment)
SELECT x.id, x.name,
    (SELECT group_concat(al.name, ' ') FROM alias al WHERE al.kind = 3 AND al.entity = x.id),
    x.comment
FROM event x;

CREATE VIRTUAL TABLE instrument_fts USING fts5(
    name, alias, description, comment, content='', tokenize='unicode61 remove_diacritics 2'
);
INSERT INTO instrument_fts (rowid, name, alias, description, comment)
SELECT x.id, x.name,
    (SELECT group_concat(al.name, ' ') FROM alias al WHERE al.kind = 5 AND al.entity = x.id),
    x.description, x.comment
FROM instrument x;

CREATE VIRTUAL TABLE series_fts USING fts5(name, alias, comment, content='', tokenize='unicode61 remove_diacritics 2');
INSERT INTO series_fts (rowid, name, alias, comment)
SELECT x.id, x.name,
    (SELECT group_concat(al.name, ' ') FROM alias al WHERE al.kind = 11 AND al.entity = x.id),
    x.comment
FROM series x;

CREATE VIRTUAL TABLE tag_fts USING fts5(name, content='', tokenize='unicode61 remove_diacritics 2');
INSERT INTO tag_fts (rowid, name) SELECT id, name FROM tag;
