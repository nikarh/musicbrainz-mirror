//! What the store keeps of each dump table: its fields (typed, fixed width), how its rows are indexed, and the
//! indexes over it. The importer writes by these definitions and the reader reads by them.
//!
//! A dump file (`mbdump/<table>`) has the columns of the MusicBrainz table in the order of musicbrainz-server's
//! `admin/sql/CreateTables.sql` (for [`SCHEMA_SEQUENCE`]). Each row is checked to have exactly that many columns: a
//! schema change that adds or removes columns fails the import instead of filling the wrong fields.

use std::collections::HashMap;

use crate::kind::Kind;

/// The MusicBrainz schema version (the dump's `SCHEMA_SEQUENCE`) the column lists are for.
pub const SCHEMA_SEQUENCE: u32 = 31;

/// The type of a field, and its width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ty {
    /// An MBID (16 bytes); 0 where there is none.
    Gid,
    /// An unsigned integer (4 bytes); `NONE` for NULL.
    U32,
    /// A big integer (8 bytes); `u64::MAX` for NULL.
    U64,
    /// 1 byte, 0 or 1 (NULL is 0).
    Bool,
    /// A string's offset in the table's pool (4 bytes).
    Str,
    /// Like `Str`, but identical strings share one copy.
    InternedStr,
    /// A partial date (4 bytes): year (i16, `i16::MIN` for none), month, day (0 for none).
    Date,
}

impl Ty {
    #[must_use]
    pub fn width(self) -> usize {
        match self {
            Self::Gid => 16,
            Self::U64 => 8,
            Self::Bool => 1,
            Self::U32 | Self::Str | Self::InternedStr | Self::Date => 4,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Field {
    pub name: String,
    pub ty: Ty,
    /// The dump column (for dates: the first of the three, year).
    pub column: usize,
    pub offset: usize,
}

/// How a table's rows are numbered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rows {
    /// Row `n` is the row whose this dump column is `n` (entities, lookup tables).
    ById(usize),
    /// In dump order.
    Sequential,
}

#[derive(Debug, Clone)]
pub struct Table {
    /// The dump file, and the store's file names.
    pub name: String,
    pub column_count: usize,
    pub rows: Rows,
    pub fields: Vec<Field>,
    pub width: usize,
    /// CSR indexes: (key field, order field: rows of a key in the order of this field, else dump order).
    pub csr: Vec<(String, Option<String>)>,
    /// String fields with a sorted index (to find rows by the value).
    pub sorted: Vec<String>,
    /// Rows where this field is NULL or 0 are skipped (ratings without votes).
    pub require: Option<String>,
}

impl Table {
    #[must_use]
    pub fn field(&self, name: &str) -> Option<&Field> {
        self.fields.iter().find(|f| f.name == name)
    }

    /// Whether the rows have an MBID (and an MBID index).
    #[must_use]
    pub fn has_gid(&self) -> bool {
        self.fields.iter().any(|f| f.ty == Ty::Gid)
    }
}

/// Builds a definition. `columns`: all of the dump file's columns, in order. `fields`: the kept ones, each
/// `name[=column][:type]` (type `u32` by default; `gid`, `u64`, `bool`, `str`, `istr` (interned), `date` (from
/// `<name>_year`, `_month`, `_day`, or `<name>_date_year`, ...)).
fn table(name: &str, columns: &str, rows: Option<&str>, fields: &str) -> Table {
    let columns: Vec<&str> = columns.split_whitespace().collect();
    let mut offset = 0;
    let fields = fields
        .split_whitespace()
        .map(|spec| {
            let (spec, ty) = spec.split_once(':').unwrap_or((spec, "u32"));
            let (name, column) = spec.split_once('=').unwrap_or((spec, spec));
            let ty = match ty {
                "gid" => Ty::Gid,
                "u64" => Ty::U64,
                "bool" => Ty::Bool,
                "str" => Ty::Str,
                "istr" => Ty::InternedStr,
                "date" => Ty::Date,
                _ => Ty::U32,
            };
            let column_name = if ty == Ty::Date {
                if columns.contains(&format!("{column}_year").as_str()) {
                    format!("{column}_year")
                } else {
                    format!("{column}_date_year")
                }
            } else {
                column.to_owned()
            };
            // A typo here is caught by the tests (every definition is built there).
            let column = columns
                .iter()
                .position(|c| *c == column_name)
                .unwrap_or(usize::MAX);
            let field = Field {
                name: name.to_owned(),
                ty,
                column,
                offset,
            };
            offset += ty.width();
            field
        })
        .collect();
    Table {
        name: name.to_owned(),
        column_count: columns.len(),
        rows: rows.map_or(Rows::Sequential, |r| {
            Rows::ById(columns.iter().position(|c| *c == r).unwrap_or(usize::MAX))
        }),
        fields,
        width: offset,
        csr: Vec::new(),
        sorted: Vec::new(),
        require: None,
    }
}

impl Table {
    fn csr(mut self, key: &str, order: Option<&str>) -> Self {
        self.csr.push((key.to_owned(), order.map(str::to_owned)));
        self
    }

    fn sorted(mut self, field: &str) -> Self {
        self.sorted.push(field.to_owned());
        self
    }
}

const TYPE_COLUMNS: &str = "id name parent child_order description gid";
const DATES: &str =
    "begin_date_year begin_date_month begin_date_day end_date_year end_date_month end_date_day";

/// Every table of the store, by dump file name.
#[allow(clippy::too_many_lines)] // a table
#[must_use]
pub fn all() -> HashMap<String, Table> {
    let id = Some("id");
    let lookup = |name: &str| table(name, TYPE_COLUMNS, id, "id name:istr gid:gid");
    let mut tables = vec![
        lookup("area_type"),
        lookup("artist_type"),
        lookup("event_type"),
        lookup("instrument_type"),
        lookup("label_type"),
        lookup("place_type"),
        table("series_type", "id name entity_type parent child_order description gid", id, "id name:istr gid:gid"),
        lookup("work_type"),
        lookup("gender"),
        lookup("release_status"),
        lookup("release_packaging"),
        lookup("release_group_primary_type"),
        lookup("release_group_secondary_type"),
        table(
            "medium_format",
            "id name parent child_order year has_discids description gid",
            id,
            "id name:istr gid:gid",
        ),
        lookup("series_ordering_type"),
        table(
            "work_attribute_type",
            "id name comment free_text parent child_order description gid",
            id,
            "id name:istr gid:gid free_text:bool",
        ),
        table(
            "work_attribute_type_allowed_value",
            "id work_attribute_type value parent child_order description gid",
            id,
            "id work_attribute_type value:istr gid:gid",
        ),
        table(
            "language",
            "id iso_code_2t iso_code_2b iso_code_1 name frequency iso_code_3",
            id,
            "id iso_code_3:istr name:istr",
        ),
        table("script", "id iso_code iso_number name frequency", id, "id iso_code:istr name:istr"),
        table(
            "link_type",
            "id parent child_order gid entity_type0 entity_type1 name description link_phrase reverse_link_phrase \
             long_link_phrase last_updated is_deprecated has_dates entity0_cardinality entity1_cardinality",
            id,
            "id gid:gid name:istr entity0_cardinality entity1_cardinality",
        ),
        table(
            "link_attribute_type",
            "id parent root child_order gid name description last_updated",
            id,
            "id gid:gid name:istr",
        ),
        table("link_creditable_attribute_type", "attribute_type", Some("attribute_type"), "attribute_type"),
        table("art_type", TYPE_COLUMNS, id, "id name:istr gid:gid"),
        table("image_type", "mime_type suffix", None, "mime_type:istr suffix:istr"),
        // entities
        table(
            "area",
            &format!("id gid name type edits_pending last_updated {DATES} ended comment"),
            id,
            "gid:gid name:istr type begin:date end:date ended:bool comment:istr",
        ),
        table("iso_3166_1", "area code", None, "area code:istr").csr("area", None).sorted("code"),
        table("iso_3166_2", "area code", None, "area code:istr").csr("area", None).sorted("code"),
        table("iso_3166_3", "area code", None, "area code:istr").csr("area", None).sorted("code"),
        table(
            "artist",
            &format!(
                "id gid name sort_name {DATES} type area gender comment edits_pending last_updated ended begin_area \
                 end_area"
            ),
            id,
            "gid:gid name:istr sort_name:istr begin:date end:date type area gender comment:istr ended:bool \
             begin_area end_area",
        )
        .csr("area", None)
        .csr("begin_area", None)
        .csr("end_area", None),
        table("artist_ipi", "artist ipi edits_pending created", None, "artist ipi:str")
            .csr("artist", None)
            .sorted("ipi"),
        table("artist_isni", "artist isni edits_pending created", None, "artist isni:str")
            .csr("artist", None)
            .sorted("isni"),
        table(
            "artist_credit",
            "id name artist_count ref_count created edits_pending gid",
            id,
            "gid:gid name:istr",
        ),
        table(
            "artist_credit_name",
            "artist_credit position artist name join_phrase",
            None,
            "artist_credit position artist name:istr join_phrase:istr",
        )
        .csr("artist_credit", Some("position"))
        .csr("artist", None),
        table(
            "event",
            &format!("id gid name {DATES} time type cancelled setlist comment edits_pending last_updated ended"),
            id,
            "gid:gid name:istr begin:date end:date time:istr type cancelled:bool setlist:str comment:istr ended:bool",
        ),
        table("genre", "id gid name comment edits_pending last_updated", id, "gid:gid name:istr comment:istr")
            .sorted("name"),
        table(
            "instrument",
            "id gid name type edits_pending last_updated comment description",
            id,
            "gid:gid name:istr type comment:istr description:istr",
        ),
        table(
            "label",
            &format!("id gid name {DATES} label_code type area comment edits_pending last_updated ended"),
            id,
            "gid:gid name:istr begin:date end:date label_code type area comment:istr ended:bool",
        )
        .csr("area", None),
        table("label_ipi", "label ipi edits_pending created", None, "label ipi:str").csr("label", None).sorted("ipi"),
        table("label_isni", "label isni edits_pending created", None, "label isni:str")
            .csr("label", None)
            .sorted("isni"),
        table(
            "place",
            &format!("id gid name type address area coordinates comment edits_pending last_updated {DATES} ended"),
            id,
            "gid:gid name:istr type address:istr area coordinates:istr comment:istr begin:date end:date ended:bool",
        )
        .csr("area", None),
        table(
            "recording",
            "id gid name artist_credit length comment edits_pending last_updated video",
            id,
            "gid:gid name:istr artist_credit length comment:istr video:bool",
        )
        .csr("artist_credit", None),
        table("isrc", "id recording isrc edits_pending created", None, "recording isrc:str")
            .csr("recording", None)
            .sorted("isrc"),
        table(
            "release",
            "id gid name artist_credit release_group status packaging language script barcode comment \
             edits_pending quality last_updated",
            id,
            "gid:gid name:istr artist_credit release_group status packaging language script barcode:str \
             comment:istr quality",
        )
        .csr("artist_credit", None)
        .csr("release_group", None)
        .sorted("barcode"),
        table(
            "release_country",
            "release country date_year date_month date_day",
            None,
            "release country date:date",
        )
        .csr("release", None)
        .csr("country", None),
        table("release_unknown_country", "release date_year date_month date_day", None, "release date:date")
            .csr("release", None),
        table(
            "release_label",
            "id release label catalog_number last_updated",
            None,
            "id release label catalog_number:istr",
        )
        .csr("release", Some("id"))
        .csr("label", None),
        table(
            "release_meta",
            "id date_added info_url amazon_asin cover_art_presence",
            id,
            "id amazon_asin:str cover_art_presence:istr",
        ),
        table(
            "release_group",
            "id gid name artist_credit type comment edits_pending last_updated",
            id,
            "gid:gid name:istr artist_credit type comment:istr",
        )
        .csr("artist_credit", None),
        table(
            "release_group_meta",
            "id release_count first_release_date_year first_release_date_month first_release_date_day rating \
             rating_count",
            id,
            "id release_count first_release_date:date rating rating_count",
        ),
        table(
            "release_group_secondary_type_join",
            "release_group secondary_type created",
            None,
            "release_group secondary_type",
        )
        .csr("release_group", None),
        table(
            "medium",
            "id release position format name edits_pending last_updated track_count gid",
            id,
            "gid:gid release position format name:istr track_count",
        )
        .csr("release", Some("position")),
        table(
            "track",
            "id gid recording medium position number name artist_credit length edits_pending last_updated \
             is_data_track",
            id,
            "gid:gid recording medium position number:istr name:istr artist_credit length is_data_track:bool",
        )
        .csr("medium", Some("position"))
        .csr("recording", None)
        .csr("artist_credit", None),
        table(
            "cdtoc",
            "id discid freedb_id track_count leadout_offset track_offset created",
            id,
            "id discid:str track_count leadout_offset track_offset:str",
        )
        .sorted("discid"),
        table("medium_cdtoc", "id medium cdtoc edits_pending last_updated", None, "medium cdtoc")
            .csr("medium", None)
            .csr("cdtoc", None),
        table(
            "series",
            "id gid name comment type ordering_type edits_pending last_updated",
            id,
            "gid:gid name:istr comment:istr type ordering_type",
        ),
        table("url", "id gid url edits_pending last_updated", id, "gid:gid url:str").sorted("url"),
        table(
            "work",
            "id gid name type comment edits_pending last_updated",
            id,
            "gid:gid name:istr type comment:istr",
        ),
        table("work_language", "work language edits_pending created", None, "work language").csr("work", None),
        table("iswc", "id work iswc edits_pending created", None, "work iswc:str").csr("work", None).sorted("iswc"),
        table(
            "work_attribute",
            "id work work_attribute_type work_attribute_type_allowed_value work_attribute_text",
            None,
            "id work work_attribute_type work_attribute_type_allowed_value work_attribute_text:istr",
        )
        .csr("work", Some("id")),
        // relationships
        table(
            "link",
            &format!("id link_type {DATES} attribute_count created ended"),
            id,
            "id link_type begin:date end:date ended:bool",
        ),
        table("link_attribute", "link attribute_type created", None, "link attribute_type").csr("link", None),
        table(
            "link_attribute_credit",
            "link attribute_type credited_as",
            None,
            "link attribute_type credited_as:istr",
        )
        .csr("link", None),
        table(
            "link_attribute_text_value",
            "link attribute_type text_value",
            None,
            "link attribute_type text_value:istr",
        )
        .csr("link", None),
        // derived
        table("tag", "id name ref_count", id, "id name:istr"),
        table("annotation", "id editor text changelog created", id, "id text:str created:istr"),
        // Cover Art Archive (the full export names these `cover_art_archive.<table>`)
        table(
            "cover_art",
            "id release comment edit ordering date_uploaded edits_pending mime_type filesize thumb_250_filesize \
             thumb_500_filesize thumb_1200_filesize",
            None,
            "id:u64 release comment:istr edit ordering mime_type:istr",
        )
        .csr("release", Some("ordering")),
        table("cover_art_type", "id type_id", None, "id:u64 type_id"),
        table("release_group_cover_art", "release_group release", Some("release_group"), "release_group release"),
    ];

    for kind in Kind::ALL {
        let t = kind.table();
        if kind.has_aliases() {
            tables.push(
                table(
                    &format!("{t}_alias"),
                    &format!("id {t} name locale edits_pending last_updated type sort_name {DATES} primary_for_locale ended"),
                    None,
                    &format!(
                        "entity={t} name:istr sort_name:istr locale:istr type begin:date end:date primary_for_locale:bool \
                         ended:bool"
                    ),
                )
                .csr("entity", None),
            );
            tables.push(table(
                &format!("{t}_alias_type"),
                TYPE_COLUMNS,
                id,
                "id name:istr gid:gid",
            ));
        }
        if kind.has_tags() {
            tables.push(
                table(
                    &format!("{t}_tag"),
                    &format!("{t} tag count last_updated"),
                    None,
                    &format!("entity={t} tag count"),
                )
                .csr("entity", None),
            );
        }
        if kind.has_annotations() {
            tables.push(
                table(
                    &format!("{t}_annotation"),
                    &format!("{t} annotation"),
                    None,
                    &format!("entity={t} annotation"),
                )
                .csr("entity", None),
            );
        }
        tables.push(table(
            &format!("{t}_gid_redirect"),
            "gid new_id created",
            None,
            "gid:gid new_id",
        ));
        if kind.has_ratings() && kind != Kind::ReleaseGroup {
            let columns = if kind == Kind::Event {
                "id rating rating_count event_art_presence"
            } else {
                "id rating rating_count"
            };
            let mut meta = table(&format!("{t}_meta"), columns, id, "id rating rating_count");
            meta.require = Some("rating_count".to_owned());
            tables.push(meta);
        }
    }

    // l_<a>_<b>, a <= b
    for (i, a) in Kind::ALL.iter().enumerate() {
        for b in Kind::ALL.iter().skip(i) {
            tables.push(
                table(
                    &format!("l_{}_{}", a.table(), b.table()),
                    "id link entity0 entity1 edits_pending last_updated link_order entity0_credit entity1_credit",
                    None,
                    "link entity0 entity1 link_order entity0_credit:istr entity1_credit:istr",
                )
                .csr("entity0", None)
                .csr("entity1", None),
            );
        }
    }

    tables.into_iter().map(|t| (t.name.clone(), t)).collect()
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
    fn every_field_names_a_dump_column() {
        for t in all().values() {
            for f in &t.fields {
                assert!(
                    f.column < t.column_count,
                    "{}: no column for {}",
                    t.name,
                    f.name
                );
            }
            for (key, order) in &t.csr {
                assert!(t.field(key).is_some(), "{}: no field {key}", t.name);
                if let Some(order) = order {
                    assert!(t.field(order).is_some(), "{}: no field {order}", t.name);
                }
            }
            for s in &t.sorted {
                assert!(
                    t.field(s)
                        .is_some_and(|f| matches!(f.ty, Ty::Str | Ty::InternedStr)),
                    "{}: {s}",
                    t.name
                );
            }
            if let Rows::ById(column) = &t.rows {
                assert!(*column < t.column_count, "{}: no id column", t.name);
            }
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
    }
}
