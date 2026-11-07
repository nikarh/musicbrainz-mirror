//! Which dump files are imported, and how their columns map to the tables of `schema.sql`.
//!
//! A dump file (`mbdump/<table>`) has the columns of the MusicBrainz table in the order of musicbrainz-server's
//! `admin/sql/CreateTables.sql` (for [`SCHEMA_SEQUENCE`]). Each row is checked to have exactly that many columns: a
//! schema change that adds or removes columns fails the import instead of filling the wrong columns.

use std::collections::HashMap;

use crate::kind::Kind;

/// The MusicBrainz schema version (the dump's `SCHEMA_SEQUENCE`) these column lists are for.
pub const SCHEMA_SEQUENCE: u32 = 31;

/// How a dump file is imported.
#[derive(Debug, Clone)]
pub struct Spec {
    /// The file name in `mbdump/`.
    pub dump: String,
    /// How many columns its rows have.
    pub column_count: usize,
    pub target: &'static str,
    /// (index of the dump column, target column, whether it is a boolean)
    pub picks: Vec<(usize, String, bool)>,
    /// Target columns with a constant value (the entity type of merged tables).
    pub fixed: Vec<(&'static str, i64)>,
    /// Rows where this dump column is NULL or 0 are skipped (ratings without votes).
    pub require: Option<usize>,
}

impl Spec {
    /// The INSERT statement, with the picked columns as parameters `?1..` and the fixed values inline.
    #[must_use]
    pub fn insert_sql(&self) -> String {
        let mut columns: Vec<&str> = self.picks.iter().map(|p| p.1.as_str()).collect();
        let mut values: Vec<String> = (1..=self.picks.len()).map(|i| format!("?{i}")).collect();
        for (column, value) in &self.fixed {
            columns.push(column);
            values.push(value.to_string());
        }
        format!(
            "INSERT INTO {} ({}) VALUES ({})",
            self.target,
            columns.join(", "),
            values.join(", ")
        )
    }
}

/// Builds a spec. `columns` are all of the dump file's columns, in order; `keep` names the ones imported, each as
/// `column` (same name in the target), `column=target`, with a `!` suffix for booleans.
fn spec(dump: &str, columns: &str, target: &'static str, keep: &str) -> Spec {
    let columns: Vec<&str> = columns.split_whitespace().collect();
    let picks = keep
        .split_whitespace()
        .map(|k| {
            let (k, boolean) = k.strip_suffix('!').map_or((k, false), |k| (k, true));
            let (source, target) = k.split_once('=').unwrap_or((k, k));
            let index = columns.iter().position(|c| *c == source);
            // A typo here is a programming error caught by the tests (every spec is built there).
            (index.unwrap_or(usize::MAX), target.to_owned(), boolean)
        })
        .collect();
    Spec {
        dump: dump.to_owned(),
        column_count: columns.len(),
        target,
        picks,
        fixed: Vec::new(),
        require: None,
    }
}

const TYPE_COLUMNS: &str = "id name parent child_order description gid";
const DATES: &str =
    "begin_date_year begin_date_month begin_date_day end_date_year end_date_month end_date_day";

/// Every imported dump file, by file name.
#[allow(clippy::too_many_lines)] // a table
#[must_use]
pub fn all() -> HashMap<String, Spec> {
    let mut specs = vec![
        // lookup tables
        spec("area_type", TYPE_COLUMNS, "area_type", "id name gid"),
        spec("artist_type", TYPE_COLUMNS, "artist_type", "id name gid"),
        spec("event_type", TYPE_COLUMNS, "event_type", "id name gid"),
        spec(
            "instrument_type",
            TYPE_COLUMNS,
            "instrument_type",
            "id name gid",
        ),
        spec("label_type", TYPE_COLUMNS, "label_type", "id name gid"),
        spec("place_type", TYPE_COLUMNS, "place_type", "id name gid"),
        spec(
            "series_type",
            "id name entity_type parent child_order description gid",
            "series_type",
            "id name gid",
        ),
        spec("work_type", TYPE_COLUMNS, "work_type", "id name gid"),
        spec("gender", TYPE_COLUMNS, "gender", "id name gid"),
        spec(
            "release_status",
            TYPE_COLUMNS,
            "release_status",
            "id name gid",
        ),
        spec(
            "release_packaging",
            TYPE_COLUMNS,
            "release_packaging",
            "id name gid",
        ),
        spec(
            "release_group_primary_type",
            TYPE_COLUMNS,
            "release_group_primary_type",
            "id name gid",
        ),
        spec(
            "release_group_secondary_type",
            TYPE_COLUMNS,
            "release_group_secondary_type",
            "id name gid",
        ),
        spec(
            "medium_format",
            "id name parent child_order year has_discids description gid",
            "medium_format",
            "id name gid",
        ),
        spec(
            "series_ordering_type",
            TYPE_COLUMNS,
            "series_ordering_type",
            "id name gid",
        ),
        spec(
            "work_attribute_type",
            "id name comment free_text parent child_order description gid",
            "work_attribute_type",
            "id name gid free_text!",
        ),
        spec(
            "work_attribute_type_allowed_value",
            "id work_attribute_type value parent child_order description gid",
            "work_attribute_type_allowed_value",
            "id work_attribute_type value gid",
        ),
        spec(
            "language",
            "id iso_code_2t iso_code_2b iso_code_1 name frequency iso_code_3",
            "language",
            "id iso_code_3 name",
        ),
        spec(
            "script",
            "id iso_code iso_number name frequency",
            "script",
            "id iso_code name",
        ),
        spec(
            "link_type",
            "id parent child_order gid entity_type0 entity_type1 name description link_phrase reverse_link_phrase \
             long_link_phrase last_updated is_deprecated has_dates entity0_cardinality entity1_cardinality",
            "link_type",
            "id gid entity_type0 entity_type1 name entity0_cardinality entity1_cardinality",
        ),
        spec(
            "link_attribute_type",
            "id parent root child_order gid name description last_updated",
            "link_attribute_type",
            "id parent root gid name",
        ),
        spec(
            "link_creditable_attribute_type",
            "attribute_type",
            "link_creditable_attribute_type",
            "attribute_type",
        ),
        // entities
        spec(
            "area",
            &format!("id gid name type edits_pending last_updated {DATES} ended comment"),
            "area",
            &format!("id gid name type {DATES} ended! comment"),
        ),
        spec("iso_3166_1", "area code", "iso_3166_1", "area code"),
        spec("iso_3166_2", "area code", "iso_3166_2", "area code"),
        spec("iso_3166_3", "area code", "iso_3166_3", "area code"),
        spec("country_area", "area", "country_area", "area"),
        spec(
            "artist",
            &format!(
                "id gid name sort_name {DATES} type area gender comment edits_pending last_updated ended begin_area \
                 end_area"
            ),
            "artist",
            &format!(
                "id gid name sort_name {DATES} type area gender comment ended! begin_area end_area"
            ),
        ),
        spec(
            "artist_ipi",
            "artist ipi edits_pending created",
            "artist_ipi",
            "artist ipi",
        ),
        spec(
            "artist_isni",
            "artist isni edits_pending created",
            "artist_isni",
            "artist isni",
        ),
        spec(
            "artist_credit",
            "id name artist_count ref_count created edits_pending gid",
            "artist_credit",
            "id name gid",
        ),
        spec(
            "artist_credit_name",
            "artist_credit position artist name join_phrase",
            "artist_credit_name",
            "artist_credit position artist name join_phrase",
        ),
        spec(
            "event",
            &format!(
                "id gid name {DATES} time type cancelled setlist comment edits_pending last_updated ended"
            ),
            "event",
            &format!("id gid name {DATES} time type cancelled! setlist comment ended!"),
        ),
        spec(
            "genre",
            "id gid name comment edits_pending last_updated",
            "genre",
            "id gid name comment",
        ),
        spec(
            "instrument",
            "id gid name type edits_pending last_updated comment description",
            "instrument",
            "id gid name type comment description",
        ),
        spec(
            "label",
            &format!(
                "id gid name {DATES} label_code type area comment edits_pending last_updated ended"
            ),
            "label",
            &format!("id gid name {DATES} label_code type area comment ended!"),
        ),
        spec(
            "label_ipi",
            "label ipi edits_pending created",
            "label_ipi",
            "label ipi",
        ),
        spec(
            "label_isni",
            "label isni edits_pending created",
            "label_isni",
            "label isni",
        ),
        spec(
            "place",
            &format!(
                "id gid name type address area coordinates comment edits_pending last_updated {DATES} ended"
            ),
            "place",
            &format!("id gid name type address area coordinates comment {DATES} ended!"),
        ),
        spec(
            "recording",
            "id gid name artist_credit length comment edits_pending last_updated video",
            "recording",
            "id gid name artist_credit length comment video!",
        ),
        spec(
            "isrc",
            "id recording isrc edits_pending created",
            "isrc",
            "recording isrc",
        ),
        spec(
            "release",
            "id gid name artist_credit release_group status packaging language script barcode comment \
             edits_pending quality last_updated",
            "release",
            "id gid name artist_credit release_group status packaging language script barcode comment quality",
        ),
        spec(
            "release_country",
            "release country date_year date_month date_day",
            "release_country",
            "release country date_year date_month date_day",
        ),
        spec(
            "release_unknown_country",
            "release date_year date_month date_day",
            "release_unknown_country",
            "release date_year date_month date_day",
        ),
        spec(
            "release_label",
            "id release label catalog_number last_updated",
            "release_label",
            "id release label catalog_number",
        ),
        spec(
            "release_meta",
            "id date_added info_url amazon_asin cover_art_presence",
            "release_meta",
            "id amazon_asin cover_art_presence",
        ),
        spec(
            "release_group",
            "id gid name artist_credit type comment edits_pending last_updated",
            "release_group",
            "id gid name artist_credit type comment",
        ),
        spec(
            "release_group_meta",
            "id release_count first_release_date_year first_release_date_month first_release_date_day rating \
             rating_count",
            "release_group_meta",
            "id release_count first_release_date_year first_release_date_month first_release_date_day rating \
             rating_count",
        ),
        spec(
            "release_group_secondary_type_join",
            "release_group secondary_type created",
            "release_group_secondary_type_join",
            "release_group secondary_type",
        ),
        spec(
            "medium",
            "id release position format name edits_pending last_updated track_count gid",
            "medium",
            "id release position format name track_count gid",
        ),
        spec(
            "track",
            "id gid recording medium position number name artist_credit length edits_pending last_updated \
             is_data_track",
            "track",
            "id gid recording medium position number name artist_credit length is_data_track!",
        ),
        spec(
            "cdtoc",
            "id discid freedb_id track_count leadout_offset track_offset created",
            "cdtoc",
            "id discid freedb_id track_count leadout_offset track_offset",
        ),
        spec(
            "medium_cdtoc",
            "id medium cdtoc edits_pending last_updated",
            "medium_cdtoc",
            "id medium cdtoc",
        ),
        spec(
            "series",
            "id gid name comment type ordering_type edits_pending last_updated",
            "series",
            "id gid name comment type ordering_type",
        ),
        spec(
            "url",
            "id gid url edits_pending last_updated",
            "url",
            "id gid url",
        ),
        spec(
            "work",
            "id gid name type comment edits_pending last_updated",
            "work",
            "id gid name type comment",
        ),
        spec(
            "work_language",
            "work language edits_pending created",
            "work_language",
            "work language",
        ),
        spec(
            "iswc",
            "id work iswc edits_pending created",
            "iswc",
            "work iswc",
        ),
        spec(
            "work_attribute",
            "id work work_attribute_type work_attribute_type_allowed_value work_attribute_text",
            "work_attribute",
            "id work work_attribute_type work_attribute_type_allowed_value work_attribute_text",
        ),
        // relationships
        spec(
            "link",
            &format!("id link_type {DATES} attribute_count created ended"),
            "link",
            &format!("id link_type {DATES} ended!"),
        ),
        spec(
            "link_attribute",
            "link attribute_type created",
            "link_attribute",
            "link attribute_type",
        ),
        spec(
            "link_attribute_credit",
            "link attribute_type credited_as",
            "link_attribute_credit",
            "link attribute_type credited_as",
        ),
        spec(
            "link_attribute_text_value",
            "link attribute_type text_value",
            "link_attribute_text_value",
            "link attribute_type text_value",
        ),
        // derived
        spec("tag", "id name ref_count", "tag", "id name"),
        spec(
            "annotation",
            "id editor text changelog created",
            "annotation_text",
            "id text created",
        ),
        // Cover Art Archive. The full export names these `cover_art_archive.<table>`, the sample dump has the
        // same tables without the schema (see `file_table`).
        spec(
            "cover_art",
            "id release comment edit ordering date_uploaded edits_pending mime_type filesize thumb_250_filesize \
             thumb_500_filesize thumb_1200_filesize",
            "cover_art",
            "id release comment edit ordering mime_type",
        ),
        spec(
            "cover_art_type",
            "id type_id",
            "cover_art_type",
            "id type_id",
        ),
        spec("art_type", TYPE_COLUMNS, "art_type", "id name gid"),
        spec(
            "image_type",
            "mime_type suffix",
            "image_type",
            "mime_type suffix",
        ),
        spec(
            "release_group_cover_art",
            "release_group release",
            "release_group_cover_art",
            "release_group release",
        ),
    ];

    for kind in Kind::ALL {
        let table = kind.table();
        let code = kind.code();
        let mut add = |mut s: Spec| {
            s.fixed.push(("kind", code));
            specs.push(s);
        };
        if kind.has_aliases() {
            add(spec(
                &format!("{table}_alias"),
                &format!(
                    "id {table} name locale edits_pending last_updated type sort_name {DATES} primary_for_locale ended"
                ),
                "alias",
                &format!(
                    "{table}=entity name sort_name locale type {DATES} primary_for_locale! ended!"
                ),
            ));
            add(spec(
                &format!("{table}_alias_type"),
                TYPE_COLUMNS,
                "alias_type",
                "id name gid",
            ));
        }
        if kind.has_tags() {
            add(spec(
                &format!("{table}_tag"),
                &format!("{table} tag count last_updated"),
                "entity_tag",
                &format!("{table}=entity tag count"),
            ));
        }
        if kind.has_annotations() {
            add(spec(
                &format!("{table}_annotation"),
                &format!("{table} annotation"),
                "annotation_link",
                &format!("{table}=entity annotation"),
            ));
        }
        add(spec(
            &format!("{table}_gid_redirect"),
            "gid new_id created",
            "gid_redirect",
            "gid new_id",
        ));
        // Ratings: release groups' are in release_group_meta (derive.sql copies them).
        if kind.has_ratings() && kind != Kind::ReleaseGroup {
            let columns = if kind == Kind::Event {
                "id rating rating_count event_art_presence"
            } else {
                "id rating rating_count"
            };
            let mut meta = spec(
                &format!("{table}_meta"),
                columns,
                "rating",
                "id=entity rating rating_count",
            );
            meta.require = Some(2);
            add(meta);
        }
    }

    // l_<a>_<b>, a <= b
    for (i, a) in Kind::ALL.iter().enumerate() {
        for b in Kind::ALL.iter().skip(i) {
            let mut s = spec(
                &format!("l_{}_{}", a.table(), b.table()),
                "id link entity0 entity1 edits_pending last_updated link_order entity0_credit entity1_credit",
                "rel",
                "link entity0 entity1 link_order entity0_credit entity1_credit",
            );
            s.fixed.push(("kind0", a.code()));
            s.fixed.push(("kind1", b.code()));
            specs.push(s);
        }
    }

    specs.into_iter().map(|s| (s.dump.clone(), s)).collect()
}

/// The table a file in a dump archive holds (`mbdump/artist` → `artist`), if it is one. The Cover Art Archive's
/// tables are in a schema of their own (`mbdump/cover_art_archive.cover_art`); the Event Art Archive's are skipped
/// (their names would collide).
#[must_use]
pub fn file_table(path: &str) -> Option<&str> {
    let name = path.strip_prefix("mbdump/")?;
    if name.contains('/') {
        return None;
    }
    match name.split_once('.') {
        None => Some(name),
        Some(("cover_art_archive", table)) => Some(table),
        Some(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_pick_names_a_dump_column() {
        for spec in all().values() {
            for (index, target, _) in &spec.picks {
                assert!(
                    *index < spec.column_count,
                    "{}: no column for {target}",
                    spec.dump
                );
            }
        }
    }

    #[test]
    fn every_target_column_exists() {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch(include_str!("schema.sql")).unwrap();
        for spec in all().values() {
            db.prepare(&spec.insert_sql())
                .unwrap_or_else(|e| panic!("{}: {e}", spec.dump));
        }
    }

    #[test]
    fn file_names() {
        assert_eq!(file_table("mbdump/artist"), Some("artist"));
        assert_eq!(
            file_table("mbdump/cover_art_archive.cover_art"),
            Some("cover_art")
        );
        assert_eq!(file_table("mbdump/event_art_archive.art_type"), None);
        assert_eq!(file_table("TIMESTAMP"), None);
        assert_eq!(file_table("mbdump/a/b"), None);
    }

    #[test]
    fn relationship_tables() {
        let specs = all();
        let s = &specs["l_release_release_group"];
        assert_eq!(
            s.fixed,
            vec![
                ("kind0", Kind::Release.code()),
                ("kind1", Kind::ReleaseGroup.code())
            ]
        );
        assert!(specs.contains_key("l_artist_recording"));
        assert!(!specs.contains_key("l_recording_artist"));
    }
}
