//! Search (`/ws/2/<type>?query=...`): the query, in the Lucene syntax of MusicBrainz's search (query.rs), goes to
//! the store's search index (`store::search`); results are shaped as MusicBrainz's search server shapes them
//! (format.rs), with a 0-100 `score` (relative to the best match, as the search server's).

mod format;
pub mod query;

use serde_json::{Map, Value, json};

use super::Answer;
use super::browse::Page;
use crate::kind::Kind;
use crate::store::Store;

/// Scales a score to 0-100 of the best one.
fn scaled(score: f32, top: f32) -> u32 {
    if top <= 0.0 {
        return 100;
    }
    // A ratio of positive scores: 0..=100.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let scaled = (score / top * 100.0).round().clamp(0.0, 100.0) as u32;
    scaled
}

/// `/ws/2/<type>?query=`
#[must_use]
pub fn search(store: &Store, kind: Kind, query: &str, page: Page) -> Answer {
    let parsed = query::parse(query);
    let result = store
        .search
        .search(kind, &parsed, page.offset, page.limit)
        .and_then(|(rows, count)| {
            let top = if page.offset == 0 {
                rows.first().map_or(1.0, |r| r.1)
            } else {
                store.search.top_score(kind, &parsed)?
            };
            Ok((rows, count, top))
        });
    let (rows, count, top) = match result {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, query, "search failed");
            return Answer::Error(
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "Internal server error".into(),
            );
        }
    };
    let fmt = format::Formatter { store };
    let items = rows
        .into_iter()
        .map(|(id, score)| {
            let mut item = fmt.entity(kind, id);
            if let Value::Object(obj) = &mut item {
                obj.insert("score".into(), json!(scaled(score, top)));
            }
            item
        })
        .collect();
    envelope(
        kind.plural(),
        super::list_root(kind),
        items,
        count,
        page.offset,
    )
}

/// The search envelope: `{"created": ..., "count": n, "offset": n, "<plural>": [...]}`.
fn envelope(
    plural: &str,
    root: &'static str,
    items: Vec<Value>,
    count: usize,
    offset: usize,
) -> Answer {
    let mut out = Map::new();
    out.insert("created".into(), Value::String(now_iso()));
    out.insert("count".into(), json!(count));
    out.insert("offset".into(), json!(offset));
    out.insert(plural.into(), Value::Array(items));
    Answer::Ok(root, Value::Object(out))
}

/// Searches of what this mirror has no index of (annotations, CD stubs, URLs) find nothing; tags are searched by
/// name.
#[must_use]
pub fn search_other(store: &Store, resource: &str, query: &str, page: Page) -> Answer {
    if resource == "tag" {
        let parsed = query::parse(query);
        return match store.search.tags(&parsed, page.offset, page.limit) {
            Ok((rows, count)) => {
                let top = rows.first().map_or(1.0, |r| r.1);
                let items = rows
                    .into_iter()
                    .map(|(name, score)| json!({"name": name, "score": scaled(score, top)}))
                    .collect();
                envelope("tags", "tag-list", items, count, page.offset)
            }
            Err(e) => {
                tracing::error!(error = %e, query, "tag search failed");
                Answer::Error(
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    "Internal server error".into(),
                )
            }
        };
    }
    let (plural, root) = match resource {
        "annotation" => ("annotations", "annotation-list"),
        "cdstub" => ("cdstubs", "cdstub-list"),
        _ => ("urls", "url-list"),
    };
    envelope(plural, root, Vec::new(), 0, page.offset)
}

/// The current time as the search server writes it: `2026-11-07T18:53:20.305Z`.
fn now_iso() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = i64::try_from(now.as_secs()).unwrap_or(0);
    let (days, rem) = (secs.div_euclid(86400), secs.rem_euclid(86400));
    // Howard Hinnant's civil_from_days
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3600,
        rem / 60 % 60,
        rem % 60,
        now.subsec_millis()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scores_and_time() {
        assert_eq!(scaled(5.0, 10.0), 50);
        assert_eq!(scaled(10.0, 10.0), 100);
        assert_eq!(scaled(1.0, 0.0), 100);
        assert!(now_iso().ends_with('Z'));
    }
}
