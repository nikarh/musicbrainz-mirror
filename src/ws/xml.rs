//! The XML format (MusicBrainz's "MMD", `http://musicbrainz.org/ns/mmd-2.0#`), made from the JSON of a response by
//! the rules musicbrainz-server's XML serializer follows: lists in `<x-list count="n">`, MBIDs and types as
//! attributes (`<status id="...">Official</status>`), relationships grouped by target type, empty values left out.

use std::fmt::Write as _;

use serde_json::{Map, Value};

const NS: &str = "http://musicbrainz.org/ns/mmd-2.0#";
const EXT: &str = "http://musicbrainz.org/ns/ext#-2.0";

/// A response document: `root` names the response (`artist`, `release-list`, `disc`, ...).
pub fn document(root: &str, value: &Value) -> String {
    let mut out = String::from(r#"<?xml version="1.0" encoding="UTF-8"?>"#);
    let obj = value.as_object();
    let created = obj.and_then(|o| o.get("created")).and_then(Value::as_str);
    let _ = write!(out, r#"<metadata xmlns="{NS}" xmlns:ns2="{EXT}""#);
    if let Some(created) = created {
        let _ = write!(out, r#" created="{}""#, escape(created));
    }
    out.push('>');
    match (root.strip_suffix("-list"), obj) {
        (Some(item), Some(obj)) => {
            // A browse or search list: the items are the one array in it.
            let items = obj
                .values()
                .find_map(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let count = obj
                .get(&format!("{item}-count"))
                .or_else(|| obj.get("count"))
                .and_then(Value::as_i64)
                .unwrap_or(i64::try_from(items.len()).unwrap_or(i64::MAX));
            let offset = obj
                .get(&format!("{item}-offset"))
                .or_else(|| obj.get("offset"))
                .and_then(Value::as_i64);
            let _ = write!(out, r#"<{root} count="{count}""#);
            if let Some(offset) = offset {
                let _ = write!(out, r#" offset="{offset}""#);
            }
            out.push('>');
            for v in &items {
                element(&mut out, item, v);
            }
            let _ = write!(out, "</{root}>");
        }
        _ => element(&mut out, root, value),
    }
    out.push_str("</metadata>");
    out
}

#[must_use]
pub fn error(message: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><error><text>{}</text><text>For usage, please see: https://musicbrainz.org/development/mmd</text></error>"#,
        escape(message)
    )
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c if (c as u32) < 0x20 && !matches!(c, '\n' | '\r' | '\t') => {}
            c => out.push(c),
        }
    }
    out
}

fn scalar(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn attr(out: &mut String, name: &str, value: Option<String>) {
    if let Some(v) = value {
        let _ = write!(out, r#" {name}="{}""#, escape(&v));
    }
}

fn text_element(out: &mut String, name: &str, attrs: &[(&str, Option<String>)], text: &str) {
    let _ = write!(out, "<{name}");
    for (k, v) in attrs {
        attr(out, k, v.clone());
    }
    let _ = write!(out, ">{}</{name}>", escape(text));
}

/// The element name of a list's items, by the JSON key of the list.
fn item_name(key: &str) -> Option<&'static str> {
    Some(match key {
        "aliases" => "alias",
        "tags" | "user-tags" => "tag",
        "genres" | "user-genres" => "genre",
        "releases" => "release",
        "recordings" => "recording",
        "release-groups" => "release-group",
        "works" => "work",
        "artists" => "artist",
        "labels" => "label",
        "media" => "medium",
        "tracks" | "data-tracks" => "track",
        "release-events" => "release-event",
        "label-info" => "label-info",
        "isrcs" => "isrc",
        "iswcs" => "iswc",
        "ipis" => "ipi",
        "isnis" => "isni",
        "iso-3166-1-codes" => "iso-3166-1-code",
        "iso-3166-2-codes" => "iso-3166-2-code",
        "iso-3166-3-codes" => "iso-3166-3-code",
        "languages" => "language",
        "secondary-types" => "secondary-type",
        "attributes" => "attribute",
        "discs" => "disc",
        "collections" => "collection",
        _ => return None,
    })
}

/// Where a child goes among its siblings (MMD's order, roughly); unlisted keys follow, alphabetically.
fn order(key: &str) -> usize {
    const ORDER: &[&str] = &[
        "title",
        "name",
        "sort-name",
        "label-code",
        "disambiguation",
        "description",
        "address",
        "coordinates",
        "time",
        "setlist",
        "cancelled",
        "gender",
        "country",
        "area",
        "begin-area",
        "end-area",
        "status",
        "quality",
        "packaging",
        "text-representation",
        "artist-credit",
        "release-group",
        "date",
        "release-events",
        "barcode",
        "asin",
        "cover-art-archive",
        "label-info",
        "position",
        "number",
        "length",
        "format",
        "discs",
        "pregap",
        "tracks",
        "data-tracks",
        "primary-type",
        "secondary-types",
        "first-release-date",
        "video",
        "recording",
        "media",
        "language",
        "languages",
        "iswcs",
        "isrcs",
        "ipis",
        "isnis",
        "iso-3166-1-codes",
        "iso-3166-2-codes",
        "iso-3166-3-codes",
        "life-span",
        "annotation",
        "attributes",
        "relations",
        "releases",
        "recordings",
        "release-groups",
        "works",
        "aliases",
        "tags",
        "user-tags",
        "genres",
        "user-genres",
        "rating",
        "user-rating",
    ];
    ORDER.iter().position(|k| *k == key).unwrap_or(ORDER.len())
}

/// Keys that are written as attributes of their entity or with a sibling.
fn consumed(key: &str) -> bool {
    matches!(
        key,
        "id" | "type"
            | "type-id"
            | "score"
            | "target-type"
            | "artist-credit-id"
            | "track-count"
            | "track-offset"
            | "direction"
            | "attribute-values"
            | "attribute-ids"
            | "attribute-credits"
            | "source-credit"
            | "target-credit"
            | "ordering-key"
            | "begin"
            | "end"
            | "ended"
            | "secondary-type-ids"
            | "offset-count"
            | "offsets"
            | "sectors"
            | "count"
            | "primary-type-id"
    ) || key.ends_with("-id")
}

/// An entity (or other object) as an element named `name`.
fn element(out: &mut String, name: &str, value: &Value) {
    let Some(obj) = value.as_object() else {
        if let Some(text) = scalar(value) {
            text_element(out, name, &[], &text);
        }
        return;
    };
    match name {
        "relation" => return relation(out, obj),
        "alias" => return alias(out, obj),
        "tag" => {
            let _ = write!(
                out,
                r#"<tag count="{}">"#,
                obj.get("count").and_then(Value::as_i64).unwrap_or(0)
            );
            text_element(
                out,
                "name",
                &[],
                obj.get("name").and_then(Value::as_str).unwrap_or(""),
            );
            out.push_str("</tag>");
            return;
        }
        "disc" => return disc(out, obj),
        "life-span" => {
            out.push_str("<life-span>");
            for k in ["begin", "end"] {
                if let Some(v) = obj.get(k).and_then(scalar) {
                    text_element(out, k, &[], &v);
                }
            }
            if obj.get("ended") == Some(&Value::Bool(true)) {
                out.push_str("<ended>true</ended>");
            }
            out.push_str("</life-span>");
            return;
        }
        "rating" | "user-rating" => {
            // Only ratings someone gave.
            if obj.get("value").is_none_or(Value::is_null) {
                return;
            }
            let _ = write!(out, "<{name}");
            if let Some(votes) = obj.get("votes-count").and_then(Value::as_i64) {
                let _ = write!(out, r#" votes-count="{votes}""#);
            }
            match obj.get("value").and_then(scalar) {
                Some(v) => {
                    let _ = write!(out, ">{v}</{name}>");
                }
                None => out.push_str("/>"),
            }
            return;
        }
        _ => {}
    }

    let _ = write!(out, "<{name}");
    attr(out, "id", obj.get("id").and_then(scalar));
    if name == "release-group" {
        attr(out, "type", obj.get("primary-type").and_then(scalar));
        attr(out, "type-id", obj.get("primary-type-id").and_then(scalar));
    } else if name == "attribute" {
        // a work's attribute: <attribute type="Key" type-id="..." value-id="...">value</attribute>
        attr(out, "type", obj.get("type").and_then(scalar));
        attr(out, "type-id", obj.get("type-id").and_then(scalar));
        attr(out, "value-id", obj.get("value-id").and_then(scalar));
        let _ = write!(
            out,
            ">{}</attribute>",
            escape(obj.get("value").and_then(Value::as_str).unwrap_or(""))
        );
        return;
    } else {
        attr(out, "type", obj.get("type").and_then(scalar));
        attr(out, "type-id", obj.get("type-id").and_then(scalar));
    }
    if name == "genre" {
        attr(out, "count", obj.get("count").and_then(scalar));
    }
    attr(out, "ns2:score", obj.get("score").and_then(scalar));
    out.push('>');

    let mut keys: Vec<&String> = obj.keys().filter(|k| !consumed(k)).collect();
    keys.sort_by_key(|k| (order(k), k.as_str()));
    for key in keys {
        let v = &obj[key.as_str()];
        // A track's title and artist credit only when they differ from its recording's.
        if name == "track"
            && (key == "title" || key == "artist-credit")
            && obj.get("recording").and_then(|r| r.get(key.as_str())) == Some(v)
        {
            continue;
        }
        // A work's (first) ISWC is on its own, too.
        if name == "work"
            && key == "iswcs"
            && let Some(iswc) = v.as_array().and_then(|l| l.first()).and_then(scalar)
        {
            text_element(out, "iswc", &[], &iswc);
        }
        child(out, name, obj, key, v);
    }
    let _ = write!(out, "</{name}>");
}

#[allow(clippy::too_many_lines)]
fn child(out: &mut String, parent: &str, obj: &Map<String, Value>, key: &str, v: &Value) {
    match (key, v) {
        (_, Value::Null) => {}
        ("annotation", Value::String(text)) => {
            let _ = write!(
                out,
                "<annotation><text>{}</text></annotation>",
                escape(text)
            );
        }
        ("artist-credit", Value::Array(credits)) => {
            out.push_str("<artist-credit");
            attr(out, "id", obj.get("artist-credit-id").and_then(scalar));
            out.push('>');
            for credit in credits {
                out.push_str("<name-credit");
                attr(out, "joinphrase", credit.get("joinphrase").and_then(scalar));
                out.push('>');
                let name = credit.get("name").and_then(Value::as_str).unwrap_or("");
                let artist = credit.get("artist").cloned().unwrap_or(Value::Null);
                if artist.get("name").and_then(Value::as_str) != Some(name) {
                    text_element(out, "name", &[], name);
                }
                element(out, "artist", &artist);
                out.push_str("</name-credit>");
            }
            out.push_str("</artist-credit>");
        }
        ("relations", Value::Array(rels)) => {
            // grouped by target type, in their order
            let mut groups: Vec<(String, Vec<&Value>)> = Vec::new();
            for rel in rels {
                let target = rel
                    .get("target-type")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .replace('_', "-");
                match groups.iter_mut().find(|(t, _)| *t == target) {
                    Some((_, list)) => list.push(rel),
                    None => groups.push((target, vec![rel])),
                }
            }
            for (target, list) in groups {
                let _ = write!(out, r#"<relation-list target-type="{target}">"#);
                for rel in list {
                    element(out, "relation", rel);
                }
                out.push_str("</relation-list>");
            }
        }
        ("tracks" | "data-tracks", Value::Array(items)) => {
            let list = if key == "tracks" {
                "track-list"
            } else {
                "data-track-list"
            };
            let count = if key == "tracks" {
                obj.get("track-count")
                    .and_then(Value::as_i64)
                    .unwrap_or(i64::try_from(items.len()).unwrap_or(i64::MAX))
            } else {
                i64::try_from(items.len()).unwrap_or(i64::MAX)
            };
            let _ = write!(out, r#"<{list} count="{count}""#);
            if key == "tracks"
                && let Some(offset) = obj.get("track-offset").and_then(Value::as_i64)
            {
                let _ = write!(out, r#" offset="{offset}""#);
            }
            out.push('>');
            for item in items {
                element(out, "track", item);
            }
            let _ = write!(out, "</{list}>");
        }
        ("secondary-types", Value::Array(items)) if !items.is_empty() => {
            let ids = obj.get("secondary-type-ids").and_then(Value::as_array);
            out.push_str("<secondary-type-list>");
            for (i, item) in items.iter().enumerate() {
                let id = ids.and_then(|ids| ids.get(i)).and_then(scalar);
                text_element(
                    out,
                    "secondary-type",
                    &[("id", id)],
                    item.as_str().unwrap_or(""),
                );
            }
            out.push_str("</secondary-type-list>");
        }
        ("isrcs", Value::Array(items)) if !items.is_empty() => {
            let _ = write!(out, r#"<isrc-list count="{}">"#, items.len());
            for item in items {
                let _ = write!(
                    out,
                    r#"<isrc id="{}"/>"#,
                    escape(item.as_str().unwrap_or(""))
                );
            }
            out.push_str("</isrc-list>");
        }
        (_, Value::Array(items)) => {
            let Some(item) = item_name(key) else { return };
            if items.is_empty() && !matches!(key, "label-info" | "media") {
                return;
            }
            // Only lists of entities (and aliases) have a count.
            let counted = !matches!(
                key,
                "tags"
                    | "user-tags"
                    | "genres"
                    | "user-genres"
                    | "ipis"
                    | "isnis"
                    | "iswcs"
                    | "languages"
                    | "iso-3166-1-codes"
                    | "iso-3166-2-codes"
                    | "iso-3166-3-codes"
                    | "attributes"
            );
            if counted {
                let _ = write!(out, r#"<{item}-list count="{}">"#, items.len());
            } else {
                let _ = write!(out, "<{item}-list>");
            }
            for v in items {
                element(out, item, v);
            }
            let _ = write!(out, "</{item}-list>");
        }
        ("cover-art-archive", Value::Object(caa)) => {
            out.push_str("<cover-art-archive>");
            for k in ["artwork", "darkened", "count", "front", "back"] {
                if let Some(v) = caa.get(k).and_then(scalar) {
                    if k == "darkened" && v == "false" {
                        continue;
                    }
                    text_element(out, k, &[], &v);
                }
            }
            out.push_str("</cover-art-archive>");
        }
        (_, Value::Object(_)) => element(out, key, v),
        (_, Value::Bool(false)) if key != "cancelled" => {}
        (_, v) => {
            let Some(text) = scalar(v) else { return };
            let id = obj.get(&format!("{key}-id")).and_then(scalar);
            text_element(out, key, &[("id", id)], &text);
            let _ = parent;
        }
    }
    // A medium's track count without its tracks
    if parent == "medium" && key == "format" && obj.get("tracks").is_none() {
        let count = obj.get("track-count").and_then(Value::as_i64).unwrap_or(0);
        let _ = write!(out, r#"<track-list count="{count}"/>"#);
    }
}

fn alias(out: &mut String, obj: &Map<String, Value>) {
    out.push_str("<alias");
    for key in ["locale", "sort-name", "type", "type-id"] {
        attr(out, key, obj.get(key).and_then(scalar));
    }
    if obj.get("primary") == Some(&Value::Bool(true)) {
        out.push_str(r#" primary="primary""#);
    }
    for (key, xml) in [
        ("begin", "begin-date"),
        ("end", "end-date"),
        ("begin-date", "begin-date"),
        ("end-date", "end-date"),
    ] {
        attr(out, xml, obj.get(key).and_then(scalar));
    }
    let _ = write!(
        out,
        ">{}</alias>",
        escape(obj.get("name").and_then(Value::as_str).unwrap_or(""))
    );
}

fn disc(out: &mut String, obj: &Map<String, Value>) {
    out.push_str("<disc");
    attr(out, "id", obj.get("id").and_then(scalar));
    out.push('>');
    if let Some(sectors) = obj.get("sectors").and_then(scalar) {
        text_element(out, "sectors", &[], &sectors);
    }
    if let Some(offsets) = obj.get("offsets").and_then(Value::as_array) {
        let _ = write!(out, r#"<offset-list count="{}">"#, offsets.len());
        for (i, offset) in offsets.iter().enumerate() {
            let _ = write!(out, r#"<offset position="{}">{offset}</offset>"#, i + 1);
        }
        out.push_str("</offset-list>");
    }
    if let Some(releases) = obj.get("releases").and_then(Value::as_array) {
        let _ = write!(out, r#"<release-list count="{}">"#, releases.len());
        for r in releases {
            element(out, "release", r);
        }
        out.push_str("</release-list>");
    }
    out.push_str("</disc>");
}

fn relation(out: &mut String, obj: &Map<String, Value>) {
    out.push_str("<relation");
    attr(out, "type", obj.get("type").and_then(scalar));
    attr(out, "type-id", obj.get("type-id").and_then(scalar));
    out.push('>');
    let target_type = obj.get("target-type").and_then(Value::as_str).unwrap_or("");
    let target = obj.get(target_type).cloned().unwrap_or(Value::Null);
    if target_type == "url" {
        text_element(
            out,
            "target",
            &[("id", target.get("id").and_then(scalar))],
            target.get("resource").and_then(Value::as_str).unwrap_or(""),
        );
    } else {
        text_element(
            out,
            "target",
            &[],
            target.get("id").and_then(Value::as_str).unwrap_or(""),
        );
    }
    if let Some(key) = obj.get("ordering-key").and_then(scalar) {
        text_element(out, "ordering-key", &[], &key);
    }
    if let Some(direction) = obj.get("direction").and_then(scalar) {
        text_element(out, "direction", &[], &direction);
    }
    let names = obj
        .get("attributes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if !names.is_empty() {
        out.push_str("<attribute-list>");
        for name in names {
            let name = name.as_str().unwrap_or("");
            let get = |key: &str| obj.get(key).and_then(|m| m.get(name)).and_then(scalar);
            text_element(
                out,
                "attribute",
                &[
                    ("type-id", get("attribute-ids")),
                    ("value", get("attribute-values")),
                    ("credited-as", get("attribute-credits")),
                ],
                name,
            );
        }
        out.push_str("</attribute-list>");
    }
    for key in ["begin", "end"] {
        if let Some(v) = obj.get(key).and_then(scalar) {
            text_element(out, key, &[], &v);
        }
    }
    if obj.get("ended") == Some(&Value::Bool(true)) {
        out.push_str("<ended>true</ended>");
    }
    for key in ["source-credit", "target-credit"] {
        if let Some(v) = obj.get(key).and_then(scalar) {
            text_element(out, key, &[], &v);
        }
    }
    if target_type != "url" {
        element(out, &target_type.replace('_', "-"), &target);
    }
    out.push_str("</relation>");
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn entities_and_lists() {
        let artist = json!({
            "id": "a", "name": "N & M", "sort-name": "N", "type": "Group", "type-id": "t", "disambiguation": "",
            "life-span": {"begin": "1988", "end": null, "ended": false},
            "aliases": [{"name": "NIN", "sort-name": "NIN", "locale": null, "primary": null, "type": null,
                "type-id": null, "begin": null, "end": null, "ended": false}],
            "gender": null, "gender-id": null,
        });
        assert_eq!(
            document("artist", &artist),
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?><metadata xmlns="{NS}" xmlns:ns2="{EXT}"><artist id="a" type="Group" type-id="t"><name>N &amp; M</name><sort-name>N</sort-name><life-span><begin>1988</begin></life-span><alias-list count="1"><alias sort-name="NIN">NIN</alias></alias-list></artist></metadata>"#
            )
        );
        let list = json!({"releases": [{"id": "r", "title": "T", "status": "Official", "status-id": "s"}],
            "release-count": 7, "release-offset": 0});
        assert!(document("release-list", &list).contains(
            r#"<release-list count="7" offset="0"><release id="r"><title>T</title><status id="s">Official</status></release></release-list>"#
        ));
        assert!(error("Not Found").contains("<error><text>Not Found</text>"));
    }

    #[test]
    fn relations_group_by_target_type() {
        let rec = json!({"id": "x", "title": "t", "relations": [
            {"type": "producer", "type-id": "p", "direction": "backward", "target-type": "artist",
             "artist": {"id": "a", "name": "A", "sort-name": "A"}, "attributes": ["co"],
             "attribute-ids": {"co": "c"}, "attribute-values": {}, "begin": null, "end": null, "ended": false},
            {"type": "free streaming", "type-id": "f", "direction": "forward", "target-type": "url",
             "url": {"id": "u", "resource": "https://x"}, "attributes": [], "ended": false},
        ]});
        let xml = document("recording", &rec);
        assert!(xml.contains(r#"<relation-list target-type="artist"><relation type="producer" type-id="p"><target>a</target><direction>backward</direction><attribute-list><attribute type-id="c">co</attribute></attribute-list><artist id="a"><name>A</name><sort-name>A</sort-name></artist></relation></relation-list>"#), "{xml}");
        assert!(xml.contains(r#"<relation-list target-type="url"><relation type="free streaming" type-id="f"><target id="u">https://x</target><direction>forward</direction></relation></relation-list>"#), "{xml}");
    }
}
