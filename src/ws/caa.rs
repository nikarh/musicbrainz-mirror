//! The Cover Art Archive's API (coverartarchive.org): which images a release (or release group) has, from the
//! Cover Art Archive dump. The images themselves are not in the dump: their URLs point at the Cover Art Archive
//! (`--cover-art-url`), and `/front`, `/back` redirect there.

use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use rusqlite::{Connection, OptionalExtension as _};
use serde_json::{Value, json};

use super::{AppState, is_mbid, resolve};

/// A cover art row: id, comment, edit, MIME type, file suffix.
type CoverArtRow = (
    i64,
    Option<String>,
    Option<i64>,
    Option<String>,
    Option<String>,
);
use crate::db::queries as q;
use crate::kind::Kind;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/release/{mbid}", get(release))
        .route("/release/{mbid}/", get(release))
        .route("/release/{mbid}/{image}", get(release_image))
        .route("/release-group/{mbid}", get(release_group))
        .route("/release-group/{mbid}/", get(release_group))
        .route("/release-group/{mbid}/{image}", get(release_group_image))
}

struct Image {
    id: i64,
    comment: String,
    edit: Option<i64>,
    suffix: String,
    types: Vec<String>,
}

/// A release's images (none unless its cover art is "present"), with the release's MBID.
fn images(conn: &Connection, release: i64) -> rusqlite::Result<Option<(String, Vec<Image>)>> {
    let row: Option<(String, Option<String>)> = conn
        .prepare_cached(q::RELEASE)?
        .query_row([release], |r| Ok((r.get(0)?, r.get(12)?)))
        .optional()?;
    let Some((gid, presence)) = row else {
        return Ok(None);
    };
    if presence.as_deref() != Some("present") {
        return Ok(Some((gid, Vec::new())));
    }
    let rows: Vec<CoverArtRow> = conn
        .prepare_cached(q::COVER_ART)?
        .query_map([release], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut images = Vec::with_capacity(rows.len());
    for (id, comment, edit, _mime, suffix) in rows {
        let types: Vec<String> = conn
            .prepare_cached(q::COVER_ART_TYPES)?
            .query_map([id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        images.push(Image {
            id,
            comment: comment.unwrap_or_default(),
            edit,
            suffix: suffix.unwrap_or_else(|| "jpg".into()),
            types,
        });
    }
    Ok(Some((gid, images)))
}

fn listing(base: &str, release_gid: &str, images: &[Image]) -> Value {
    let images: Vec<Value> = images
        .iter()
        .map(|i| {
            let url = |size: &str| format!("{base}/release/{release_gid}/{}{size}.jpg", i.id);
            json!({
                "approved": true,
                "back": i.types.iter().any(|t| t == "Back"),
                "comment": i.comment,
                "edit": i.edit,
                "front": i.types.iter().any(|t| t == "Front"),
                "id": i.id,
                "image": format!("{base}/release/{release_gid}/{}.{}", i.id, i.suffix),
                "thumbnails": {
                    "250": url("-250"), "500": url("-500"), "1200": url("-1200"),
                    "small": url("-250"), "large": url("-500"),
                },
                "types": i.types,
            })
        })
        .collect();
    json!({"images": images, "release": format!("https://musicbrainz.org/release/{release_gid}")})
}

/// The release whose cover art stands for a release group: the one chosen for it, else the earliest with a front
/// image.
fn release_group_release(conn: &Connection, rg: i64) -> rusqlite::Result<Option<i64>> {
    if let Some(release) = conn
        .prepare_cached(q::RELEASE_GROUP_COVER_RELEASE)?
        .query_row([rg], |r| r.get(0))
        .optional()?
    {
        return Ok(Some(release));
    }
    conn.prepare_cached(q::RELEASE_GROUP_FIRST_COVER_RELEASE)?
        .query_row([rg], |r| r.get(0))
        .optional()
}

enum Found {
    Listing(Value),
    Redirect(String),
    Missing,
    BadRequest,
}

async fn answer<F>(state: AppState, f: F) -> Response
where
    F: FnOnce(&Connection, &str) -> rusqlite::Result<Found> + Send + 'static,
{
    let Some(db) = state.db.load_full() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "The database is not ready yet\n",
        )
            .into_response();
    };
    let base = state.cover_art_url.clone();
    let found = tokio::task::spawn_blocking(move || {
        let conn = db.conn().map_err(|e| e.to_string())?;
        f(&conn, &base).map_err(|e| e.to_string())
    })
    .await;
    match found {
        Ok(Ok(Found::Listing(v))) => {
            let mut r = (StatusCode::OK, v.to_string()).into_response();
            r.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            r.headers_mut().insert(
                header::ACCESS_CONTROL_ALLOW_ORIGIN,
                HeaderValue::from_static("*"),
            );
            r
        }
        Ok(Ok(Found::Redirect(url))) => match HeaderValue::from_str(&url) {
            Ok(location) => {
                let mut r = StatusCode::TEMPORARY_REDIRECT.into_response();
                r.headers_mut().insert(header::LOCATION, location);
                r
            }
            Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        },
        Ok(Ok(Found::Missing)) => (StatusCode::NOT_FOUND, "No cover art found\n").into_response(),
        Ok(Ok(Found::BadRequest)) => (StatusCode::BAD_REQUEST, "Invalid MBID\n").into_response(),
        Ok(Err(e)) => {
            tracing::error!(error = %e, "cover art request failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// The images of the release (or release group) by MBID.
fn find(
    conn: &Connection,
    kind: Kind,
    mbid: &str,
) -> rusqlite::Result<Option<(String, Vec<Image>)>> {
    let Some(id) = resolve(conn, kind.table(), Some(kind), mbid)? else {
        return Ok(None);
    };
    let release = if kind == Kind::ReleaseGroup {
        release_group_release(conn, id)?
    } else {
        Some(id)
    };
    match release {
        Some(release) => Ok(images(conn, release)?.filter(|(_, images)| !images.is_empty())),
        None => Ok(None),
    }
}

fn list(conn: &Connection, base: &str, kind: Kind, mbid: &str) -> rusqlite::Result<Found> {
    let mbid = mbid.trim_end_matches(".json");
    if !is_mbid(mbid) {
        return Ok(Found::BadRequest);
    }
    Ok(match find(conn, kind, mbid)? {
        Some((gid, images)) => Found::Listing(listing(base, &gid, &images)),
        None => Found::Missing,
    })
}

/// `front`, `back`, `front-250`, ..., or `<image id>[-size].<ext>`: a redirect to the image.
fn image(
    conn: &Connection,
    base: &str,
    kind: Kind,
    mbid: &str,
    image: &str,
) -> rusqlite::Result<Found> {
    if !is_mbid(mbid) {
        return Ok(Found::BadRequest);
    }
    let Some((gid, images)) = find(conn, kind, mbid)? else {
        return Ok(Found::Missing);
    };
    let name = image.split('.').next().unwrap_or(image);
    let (which, size) = match name.split_once('-') {
        Some((w, s)) if matches!(s, "250" | "500" | "1200") => (w, format!("-{s}")),
        _ => (name, String::new()),
    };
    let chosen = match which {
        "front" => images.iter().find(|i| i.types.iter().any(|t| t == "Front")),
        "back" => images.iter().find(|i| i.types.iter().any(|t| t == "Back")),
        id => images.iter().find(|i| i.id.to_string() == id),
    };
    Ok(match chosen {
        Some(i) => {
            let suffix = if size.is_empty() {
                i.suffix.as_str()
            } else {
                "jpg"
            };
            Found::Redirect(format!("{base}/release/{gid}/{}{size}.{suffix}", i.id))
        }
        None => Found::Missing,
    })
}

async fn release(State(state): State<AppState>, Path(mbid): Path<String>) -> Response {
    answer(state, move |conn, base| {
        list(conn, base, Kind::Release, &mbid)
    })
    .await
}

async fn release_group(State(state): State<AppState>, Path(mbid): Path<String>) -> Response {
    answer(state, move |conn, base| {
        list(conn, base, Kind::ReleaseGroup, &mbid)
    })
    .await
}

async fn release_image(
    State(state): State<AppState>,
    Path((mbid, name)): Path<(String, String)>,
) -> Response {
    answer(state, move |conn, base| {
        image(conn, base, Kind::Release, &mbid, &name)
    })
    .await
}

async fn release_group_image(
    State(state): State<AppState>,
    Path((mbid, name)): Path<(String, String)>,
) -> Response {
    answer(state, move |conn, base| {
        image(conn, base, Kind::ReleaseGroup, &mbid, &name)
    })
    .await
}
