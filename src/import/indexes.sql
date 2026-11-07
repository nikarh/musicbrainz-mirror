-- Created once the dump's rows are in (faster than keeping them up to date while inserting). Every query the web
-- service runs is checked to use them (src/db/queries.rs tests).

CREATE UNIQUE INDEX area_gid ON area (gid);
CREATE INDEX area_type_idx ON area (type);
CREATE INDEX iso_3166_1_area ON iso_3166_1 (area);
CREATE INDEX iso_3166_1_code ON iso_3166_1 (code);
CREATE INDEX iso_3166_2_area ON iso_3166_2 (area);
CREATE INDEX iso_3166_3_area ON iso_3166_3 (area);

CREATE UNIQUE INDEX artist_gid ON artist (gid);
CREATE INDEX artist_area ON artist (area);
CREATE INDEX artist_begin_area ON artist (begin_area);
CREATE INDEX artist_end_area ON artist (end_area);
CREATE INDEX artist_ipi_artist ON artist_ipi (artist);
CREATE INDEX artist_isni_artist ON artist_isni (artist);
CREATE INDEX artist_credit_name_artist_credit ON artist_credit_name (artist_credit, position);
CREATE INDEX artist_credit_name_artist ON artist_credit_name (artist);

CREATE UNIQUE INDEX event_gid ON event (gid);
CREATE UNIQUE INDEX genre_gid ON genre (gid);
CREATE INDEX genre_name ON genre (name);
CREATE UNIQUE INDEX instrument_gid ON instrument (gid);

CREATE UNIQUE INDEX label_gid ON label (gid);
CREATE INDEX label_area ON label (area);
CREATE INDEX label_ipi_label ON label_ipi (label);
CREATE INDEX label_isni_label ON label_isni (label);

CREATE UNIQUE INDEX place_gid ON place (gid);
CREATE INDEX place_area ON place (area);

CREATE UNIQUE INDEX recording_gid ON recording (gid);
CREATE INDEX recording_artist_credit ON recording (artist_credit);
CREATE INDEX isrc_recording ON isrc (recording);
CREATE INDEX isrc_isrc ON isrc (isrc);

CREATE UNIQUE INDEX release_gid ON release (gid);
CREATE INDEX release_artist_credit ON release (artist_credit);
CREATE INDEX release_release_group ON release (release_group);
CREATE INDEX release_barcode ON release (barcode);
CREATE INDEX release_country_release ON release_country (release);
CREATE INDEX release_country_country ON release_country (country);
CREATE INDEX release_unknown_country_release ON release_unknown_country (release);
CREATE INDEX release_label_release ON release_label (release);
CREATE INDEX release_label_label ON release_label (label);

CREATE UNIQUE INDEX release_group_gid ON release_group (gid);
CREATE INDEX release_group_artist_credit ON release_group (artist_credit);
CREATE INDEX release_group_secondary_type_join_release_group
    ON release_group_secondary_type_join (release_group);

CREATE UNIQUE INDEX medium_gid ON medium (gid);
CREATE INDEX medium_release ON medium (release, position);
CREATE UNIQUE INDEX track_gid ON track (gid);
CREATE INDEX track_medium ON track (medium, position);
CREATE INDEX track_recording ON track (recording);
CREATE INDEX track_artist_credit ON track (artist_credit);
CREATE INDEX cdtoc_discid ON cdtoc (discid);
CREATE INDEX medium_cdtoc_medium ON medium_cdtoc (medium);
CREATE INDEX medium_cdtoc_cdtoc ON medium_cdtoc (cdtoc);

CREATE UNIQUE INDEX series_gid ON series (gid);
CREATE UNIQUE INDEX url_gid ON url (gid);
CREATE INDEX url_url ON url (url);

CREATE UNIQUE INDEX work_gid ON work (gid);
CREATE INDEX work_language_work ON work_language (work);
CREATE INDEX iswc_work ON iswc (work);
CREATE INDEX iswc_iswc ON iswc (iswc);
CREATE INDEX work_attribute_work ON work_attribute (work);

CREATE INDEX link_attribute_link ON link_attribute (link);
CREATE INDEX link_attribute_credit_link ON link_attribute_credit (link);
CREATE INDEX link_attribute_text_value_link ON link_attribute_text_value (link);
CREATE INDEX rel_entity0 ON rel (kind0, entity0, kind1);
CREATE INDEX rel_entity1 ON rel (kind1, entity1, kind0);

CREATE INDEX alias_entity ON alias (kind, entity);
CREATE INDEX tag_name ON tag (name);
CREATE INDEX entity_tag_entity ON entity_tag (kind, entity);
CREATE INDEX entity_tag_tag ON entity_tag (tag, kind);
CREATE UNIQUE INDEX rating_entity ON rating (kind, entity);
CREATE INDEX gid_redirect_gid ON gid_redirect (gid);
CREATE INDEX annotation_link_entity ON annotation_link (kind, entity);

CREATE INDEX cover_art_release ON cover_art (release, ordering);
CREATE INDEX cover_art_type_id ON cover_art_type (id);
