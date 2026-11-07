//! The Cover Art Archive's API (coverartarchive.org): which images a release (or release group) has, from the
//! Cover Art Archive dump. The images themselves are not in the dump: their URLs point at the Cover Art Archive
//! (`--cover-art-url`), and `/front`, `/back` redirect there.

use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde_json::{Value, json};

use super::{AppState, is_mbid};
use crate::kind::Kind;
use crate::store::Store;

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
    id: u64,
    comment: String,
    edit: Option<u32>,
    suffix: String,
    types: Vec<String>,
}

/// A release's images (none unless its cover art is "present"), with the release's MBID.
fn images(store: &Store, release: u32) -> Option<(String, Vec<Image>)> {
    let row = store.t("release").get(release)?;
    let presence = store
        .t("release_meta")
        .get(release)
        .and_then(|m| m.str("cover_art_presence"));
    if presence != Some("present") {
        return Some((row.gid(), Vec::new()));
    }
    let art_types: Vec<(u32, String)> = store
        .t("art_type")
        .iter()
        .map(|t| (t.id, t.text("name")))
        .collect();
    let mut images: Vec<(u32, u64, Image)> = store
        .t("cover_art")
        .by("release", release)
        .filter_map(|c| {
            let bits = store.derived.cover_art_types(c.id);
            let mut types: Vec<String> = art_types
                .iter()
                .filter(|(id, _)| *id < 32 && bits & (1 << id) != 0)
                .map(|t| t.1.clone())
                .collect();
            types.sort();
            let suffix = store
                .t("image_type")
                .iter()
                .find(|i| i.str("mime_type") == c.str("mime_type"))
                .map_or_else(|| "jpg".to_owned(), |i| i.text("suffix"));
            let id = c.u64("id")?;
            Some((
                c.u32("ordering").unwrap_or(0),
                id,
                Image {
                    id,
                    comment: c.text("comment"),
                    edit: c.u32("edit"),
                    suffix,
                    types,
                },
            ))
        })
        .collect();
    images.sort_by_key(|(ordering, id, _)| (*ordering, *id));
    Some((row.gid(), images.into_iter().map(|i| i.2).collect()))
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
/// The release whose cover art stands for a release group: the one chosen for it, else the earliest with a front
/// image.
fn release_group_release(store: &Store, rg: u32) -> Option<u32> {
    if let Some(release) = store
        .t("release_group_cover_art")
        .get(rg)
        .and_then(|r| r.u32("release"))
    {
        return Some(release);
    }
    let front = store.id_by_name("art_type", "Front")?;
    store
        .t("release")
        .by("release_group", rg)
        .filter(|r| {
            store
                .t("release_meta")
                .get(r.id)
                .and_then(|m| m.str("cover_art_presence"))
                == Some("present")
        })
        .filter(|r| {
            store
                .t("cover_art")
                .by("release", r.id)
                .any(|c| front < 32 && store.derived.cover_art_types(c.id) & (1 << front) != 0)
        })
        .min_by_key(|r| {
            (
                store.derived.release_first_date(r.id).unwrap_or(u32::MAX),
                r.id,
            )
        })
        .map(|r| r.id)
}

enum Found {
    Listing(Value),
    Redirect(String),
    Missing,
    BadRequest,
}

async fn answer<F>(state: AppState, f: F) -> Response
where
    F: FnOnce(&Store, &str) -> Found + Send + 'static,
{
    let Some(store) = state.store.load_full() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "The database is not ready yet\n",
        )
            .into_response();
    };
    let base = state.cover_art_url.clone();
    match tokio::task::spawn_blocking(move || f(&store, &base)).await {
        Ok(Found::Listing(v)) => {
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
        Ok(Found::Redirect(url)) => match HeaderValue::from_str(&url) {
            Ok(location) => {
                let mut r = StatusCode::TEMPORARY_REDIRECT.into_response();
                r.headers_mut().insert(header::LOCATION, location);
                r
            }
            Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        },
        Ok(Found::Missing) => (StatusCode::NOT_FOUND, "No cover art found\n").into_response(),
        Ok(Found::BadRequest) => (StatusCode::BAD_REQUEST, "Invalid MBID\n").into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// The images of the release (or release group) by MBID.
fn find(store: &Store, kind: Kind, mbid: &str) -> Option<(String, Vec<Image>)> {
    let id = store.resolve(kind.table(), Some(kind), mbid)?;
    let release = if kind == Kind::ReleaseGroup {
        release_group_release(store, id)?
    } else {
        id
    };
    images(store, release).filter(|(_, images)| !images.is_empty())
}

fn list(store: &Store, base: &str, kind: Kind, mbid: &str) -> Found {
    let mbid = mbid.trim_end_matches(".json");
    if !is_mbid(mbid) {
        return Found::BadRequest;
    }
    match find(store, kind, mbid) {
        Some((gid, images)) => Found::Listing(listing(base, &gid, &images)),
        None => Found::Missing,
    }
}

/// `front`, `back`, `front-250`, ..., or `<image id>[-size].<ext>`: a redirect to the image.
fn image(store: &Store, base: &str, kind: Kind, mbid: &str, image: &str) -> Found {
    if !is_mbid(mbid) {
        return Found::BadRequest;
    }
    let Some((gid, images)) = find(store, kind, mbid) else {
        return Found::Missing;
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
    match chosen {
        Some(i) => {
            let suffix = if size.is_empty() {
                i.suffix.as_str()
            } else {
                "jpg"
            };
            Found::Redirect(format!("{base}/release/{gid}/{}{size}.{suffix}", i.id))
        }
        None => Found::Missing,
    }
}

async fn release(State(state): State<AppState>, Path(mbid): Path<String>) -> Response {
    answer(state, move |store, base| {
        list(store, base, Kind::Release, &mbid)
    })
    .await
}

async fn release_group(State(state): State<AppState>, Path(mbid): Path<String>) -> Response {
    answer(state, move |store, base| {
        list(store, base, Kind::ReleaseGroup, &mbid)
    })
    .await
}

async fn release_image(
    State(state): State<AppState>,
    Path((mbid, name)): Path<(String, String)>,
) -> Response {
    answer(state, move |store, base| {
        image(store, base, Kind::Release, &mbid, &name)
    })
    .await
}

async fn release_group_image(
    State(state): State<AppState>,
    Path((mbid, name)): Path<(String, String)>,
) -> Response {
    answer(state, move |store, base| {
        image(store, base, Kind::ReleaseGroup, &mbid, &name)
    })
    .await
}
