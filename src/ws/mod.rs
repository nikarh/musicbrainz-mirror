//! The web service: musicbrainz-server's `/ws/2` (read-only), answered from the store.

pub mod browse;
pub mod caa;
pub mod entity;
pub mod inc;
pub mod search;
pub mod xml;

use std::sync::Arc;

use arc_swap::ArcSwapOption;
use axum::Router;
use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde_json::{Map, Value, json};

use self::browse::{Linked, Page};
use self::entity::{Ctx, Filter, Missing, Opts};
use self::inc::{Inc, Mode};
use crate::kind::Kind;
use crate::store::Store;

/// Releases of a browse with `inc=recordings` stop once they have this many tracks (musicbrainz-server's
/// `WS_TRACK_LIMIT`), except the first.
const TRACK_LIMIT: u32 = 500;

/// The store being served; empty until the first import is done.
pub type Current = Arc<ArcSwapOption<Store>>;

#[derive(Clone)]
pub struct AppState {
    pub store: Current,
    /// Where cover art images are (their URLs in the Cover Art Archive API point there).
    pub cover_art_url: Arc<str>,
    /// Requests running at once (each takes a blocking thread).
    pub permits: Arc<tokio::sync::Semaphore>,
}

pub fn router(store: Current, cover_art_url: &str) -> Router {
    let permits = std::thread::available_parallelism().map_or(4, |n| n.get() * 2);
    let state = AppState {
        store,
        cover_art_url: cover_art_url.trim_end_matches('/').into(),
        permits: Arc::new(tokio::sync::Semaphore::new(permits)),
    };
    Router::new()
        .route("/healthz", get(healthz))
        .route("/ws/2/{resource}", get(ws).fallback(read_only))
        .route("/ws/2/{resource}/", get(ws).fallback(read_only))
        .route("/ws/2/{resource}/{id}", get(ws_id).fallback(read_only))
        .merge(caa::router())
        .fallback(not_found_route)
        .with_state(state)
}

async fn healthz(State(state): State<AppState>) -> Response {
    match state.store.load_full() {
        Some(store) => (StatusCode::OK, format!("ok {}\n", store.dump())).into_response(),
        // Up, but nothing to serve yet: healthy as a process.
        None => (StatusCode::OK, "ok, no database yet\n").into_response(),
    }
}

async fn not_found_route() -> Response {
    (StatusCode::NOT_FOUND, "Not Found\n").into_response()
}

/// The output format of a request: `fmt=`, else the Accept header, else XML (musicbrainz-server's default).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Json,
    Xml,
    /// `fmt=txt`, only for `/ws/2/genre/all`.
    Txt,
}

/// A request's query parameters, in order; and whether the answer is XML.
#[derive(Debug, Default)]
pub struct Params(Vec<(String, String)>, bool);

impl Params {
    fn parse(raw: Option<&str>) -> Self {
        Self(
            raw.map(|q| serde_urlencoded::from_str(q).unwrap_or_default())
                .unwrap_or_default(),
            false,
        )
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// A parameter with a non-empty value (musicbrainz-server ignores empty ones).
    #[must_use]
    pub fn value(&self, name: &str) -> Option<&str> {
        self.get(name).filter(|v| !v.is_empty())
    }

    fn all(&self, name: &str) -> Vec<&str> {
        self.0
            .iter()
            .filter(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
            .collect()
    }

    fn format(&self, headers: &HeaderMap) -> Format {
        match self.get("fmt") {
            Some("json") => Format::Json,
            Some("xml") => Format::Xml,
            Some("txt") => Format::Txt,
            _ => {
                let accept = headers
                    .get(header::ACCEPT)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("");
                if accept.contains("application/json") {
                    Format::Json
                } else {
                    Format::Xml
                }
            }
        }
    }
}

/// An answer before it is formatted.
pub enum Answer {
    /// An entity or list, with the name of its root element (`artist`, `release-list`, `disc`, ...).
    Ok(&'static str, Value),
    Text(String),
    Error(StatusCode, String),
}

impl Answer {
    fn bad(message: impl Into<String>) -> Self {
        Self::Error(StatusCode::BAD_REQUEST, message.into())
    }

    fn not_found() -> Self {
        Self::Error(StatusCode::NOT_FOUND, "Not Found".into())
    }

    fn respond(self, format: Format) -> Response {
        let (status, body, content_type) = match self {
            Self::Text(text) => (StatusCode::OK, text, "text/plain; charset=utf-8"),
            Self::Ok(root, value) if format == Format::Xml => {
                (StatusCode::OK, xml::document(root, &value), "application/xml; charset=utf-8")
            }
            Self::Ok(_, value) => (StatusCode::OK, value.to_string(), "application/json; charset=utf-8"),
            Self::Error(status, message) if format == Format::Xml => {
                (status, xml::error(&message), "application/xml; charset=utf-8")
            }
            Self::Error(status, message) => (
                status,
                json!({"error": message, "help": "For usage, please see: https://musicbrainz.org/development/mmd"})
                    .to_string(),
                "application/json; charset=utf-8",
            ),
        };
        let mut response = (status, body).into_response();
        response
            .headers_mut()
            .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
        response.headers_mut().insert(
            header::ACCESS_CONTROL_ALLOW_ORIGIN,
            HeaderValue::from_static("*"),
        );
        response
    }
}

/// Edits are not possible here: as musicbrainz-server answers them in read-only mode.
async fn read_only(method: Method, RawQuery(raw): RawQuery, headers: HeaderMap) -> Response {
    let format = Params::parse(raw.as_deref()).format(&headers);
    if method == Method::OPTIONS {
        return StatusCode::NO_CONTENT.into_response();
    }
    Answer::Error(
        StatusCode::SERVICE_UNAVAILABLE,
        "The database is currently in readonly mode and cannot handle your request".into(),
    )
    .respond(format)
}

async fn ws(
    State(state): State<AppState>,
    Path(resource): Path<String>,
    RawQuery(raw): RawQuery,
    headers: HeaderMap,
) -> Response {
    let mut params = Params::parse(raw.as_deref());
    let format = params.format(&headers);
    params.1 = format == Format::Xml;
    run(state, format, move |store| {
        collection(store, &resource, &params)
    })
    .await
}

async fn ws_id(
    State(state): State<AppState>,
    Path((resource, id)): Path<(String, String)>,
    RawQuery(raw): RawQuery,
    headers: HeaderMap,
) -> Response {
    let mut params = Params::parse(raw.as_deref());
    let format = params.format(&headers);
    params.1 = format == Format::Xml;
    run(state, format, move |store| {
        lookup(store, &resource, &id, &params, format)
    })
    .await
}

/// Answers a request from the store, on a blocking thread (page faults of the mapped files block).
async fn run<F>(state: AppState, format: Format, answer: F) -> Response
where
    F: FnOnce(&Store) -> Result<Answer, Missing> + Send + 'static,
{
    let Some(store) = state.store.load_full() else {
        return Answer::Error(
            StatusCode::SERVICE_UNAVAILABLE,
            "The database is not ready yet: the first MusicBrainz dump is being imported".into(),
        )
        .respond(format);
    };
    let Ok(_permit) = state.permits.acquire().await else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match tokio::task::spawn_blocking(move || answer(&store)).await {
        Ok(Ok(answer)) => answer.respond(format),
        Ok(Err(e)) => {
            tracing::error!(error = %e, "request failed");
            Answer::Error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Internal server error".into(),
            )
            .respond(format)
        }
        Err(e) => {
            tracing::error!(error = %e, "request panicked");
            Answer::Error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Internal server error".into(),
            )
            .respond(format)
        }
    }
}

/// Whether `s` is an MBID (a UUID, any case, but not the nil UUID, as musicbrainz-server's `is_guid`).
#[must_use]
pub fn is_mbid(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 36
        && b.iter().any(|c| *c != b'0' && *c != b'-')
        && b.iter().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => *c == b'-',
            _ => c.is_ascii_hexdigit(),
        })
}

/// `type=` and `status=` (musicbrainz-server's `validate_type` / `validate_status`).
fn filter(store: &Store, resource: &str, inc: &Inc, params: &Params) -> Result<Filter, String> {
    let by_name = |table: &str, name: &str| {
        store
            .t(table)
            .iter()
            .find(|r| r.str("name").is_some_and(|n| n.to_lowercase() == name))
            .map(|r| r.id)
    };
    let mut filter = Filter::default();
    if let Some(types) = params.value("type") {
        if !(inc.releases
            || inc.release_groups
            || resource == "release"
            || resource == "release-group")
        {
            return Err(
                "type is not a valid parameter unless releases or release-groups are requested."
                    .into(),
            );
        }
        for t in types.split('|').filter(|t| !t.is_empty()) {
            let name = t
                .strip_prefix("sa-")
                .or_else(|| t.strip_prefix("va-"))
                .unwrap_or(t);
            match (
                by_name("release_group_primary_type", name),
                by_name("release_group_secondary_type", name),
            ) {
                (Some(id), _) => filter.primary_types.push(id),
                (None, Some(id)) => filter.secondary_types.push(id),
                _ => return Err(format!("{t} is not a recognized release-group type.")),
            }
        }
    }
    if let Some(statuses) = params.value("status") {
        if !(inc.releases || resource == "release") {
            return Err("status is not a valid parameter unless releases are requested.".into());
        }
        for s in statuses.split('|').filter(|s| !s.is_empty()) {
            let name = s
                .strip_prefix("sa-")
                .or_else(|| s.strip_prefix("va-"))
                .unwrap_or(s);
            match by_name("release_status", name) {
                Some(id) => filter.status.push(id),
                None => return Err(format!("{s} is not a recognized release status.")),
            }
        }
    }
    if let Some(rg_status) = params.value("release-group-status") {
        if !(resource == "release-group" && params.get("artist").is_some()) {
            return Err(
                "release-group-status is not a valid parameter unless release groups are browsed by artist.".into()
            );
        }
        if rg_status != "website-default" && rg_status != "all" {
            return Err(format!(
                "{rg_status} is not a valid value for the release-group-status parameter."
            ));
        }
    }
    Ok(filter)
}

/// `limit` (at most 100, 25 by default) and `offset`.
fn page(params: &Params) -> Result<Page, String> {
    let parse = |name: &str, default: usize| -> Result<usize, String> {
        match params.value(name) {
            None => Ok(default),
            Some(v) => match v.parse::<usize>() {
                Ok(n) if v.bytes().all(|b| b.is_ascii_digit()) => Ok(n),
                _ => Err("The 'limit' and 'offset' parameters must be positive integers".into()),
            },
        }
    };
    let limit = match parse("limit", 25)? {
        0 => 25,
        n => n.min(100),
    };
    Ok(Page {
        limit,
        offset: parse("offset", 0)?,
    })
}

fn inc_param(resource: &str, mode: Mode, params: &Params) -> Result<Inc, String> {
    let incs = params.all("inc");
    if incs.len() > 1 {
        return Err(
            "Inc arguments must be combined with a space (+ or %20), but you provided multiple parameters".into()
        );
    }
    inc::parse(resource, mode, incs.first().copied())
}

/// Returns early with a 400 answer.
macro_rules! bad {
    ($result:expr) => {
        match $result {
            Ok(v) => v,
            Err(e) => return Ok(Answer::bad(e)),
        }
    };
}

/// `/ws/2/<resource>/<id>`
fn lookup(
    store: &Store,
    resource: &str,
    id: &str,
    params: &Params,
    format: Format,
) -> Result<Answer, Missing> {
    match resource {
        "discid" => return discid(store, id, params),
        "isrc" => return isrc(store, id, params),
        "iswc" => return iswc(store, id, params),
        "genre" if id == "all" => return genre_all(store, params, format),
        _ => {}
    }
    let Some(kind) = Kind::from_name(resource) else {
        return Ok(Answer::not_found());
    };
    let inc = bad!(inc_param(resource, Mode::Lookup, params));
    let filter = bad!(filter(store, resource, &inc, params));
    if !is_mbid(id) {
        return Ok(Answer::bad("Invalid mbid."));
    }
    let Some(entity) = store.resolve(kind.table(), Some(kind), id) else {
        return Ok(Answer::not_found());
    };
    let ctx = Ctx {
        store,
        inc: &inc,
        filter: &filter,
        xml: params.1,
    };
    Ok(Answer::Ok(
        kind.name(),
        ctx.entity(kind, entity, Opts::toplevel())?,
    ))
}

/// `/ws/2/<resource>?...`: a search (`query=`), a browse, or a URL by its address.
fn collection(store: &Store, resource: &str, params: &Params) -> Result<Answer, Missing> {
    let Some(kind) = Kind::from_name(resource) else {
        return Ok(match resource {
            "annotation" | "tag" | "cdstub" if params.value("query").is_some() => {
                let page = bad!(page(params));
                search::search_other(store, resource, params.value("query").unwrap_or(""), page)
            }
            _ => Answer::not_found(),
        });
    };
    if let Some(query) = params.value("query") {
        let page = bad!(page(params));
        if kind == Kind::Genre {
            return Ok(Answer::Error(
                StatusCode::NOT_IMPLEMENTED,
                "This hasn't been implemented yet.".into(),
            ));
        }
        return Ok(search::search(store, kind, query, page));
    }
    if kind == Kind::Url && params.get("resource").is_some() {
        return url_by_resource(store, params);
    }
    let Some((param, value)) = browse::params(kind)
        .iter()
        .find_map(|p| params.value(p).map(|v| (*p, v)))
    else {
        return Ok(Answer::bad(format!(
            "The given parameters do not match any available query type for the {resource} resource."
        )));
    };
    let mut inc = bad!(inc_param(resource, Mode::Browse, params));
    let filter = bad!(filter(store, resource, &inc, params));
    let page = bad!(page(params));
    if !is_mbid(value) {
        return Ok(Answer::bad("Invalid mbid."));
    }
    // There are no collections (they are editors' data, not in the dumps).
    let Some((table, linked_kind, make)) = Linked::parse(param) else {
        return Ok(Answer::not_found());
    };
    let Some(linked_id) = store.resolve(table, linked_kind, value) else {
        return Ok(Answer::not_found());
    };
    let linked = make(linked_id);
    if matches!(linked, Linked::Track(_)) {
        inc.recordings = true;
    }
    let ctx = Ctx {
        store,
        inc: &inc,
        filter: &filter,
        xml: params.1,
    };
    let (mut ids, total) = browse::ids(&ctx, kind, linked, page);
    if kind == Kind::Release && inc.recordings {
        limit_by_tracks(store, &mut ids);
    }
    let items = ids
        .iter()
        .map(|id| ctx.entity(kind, *id, Opts::toplevel()))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(list(kind, items, total, page.offset))
}

/// A browse's or search's list: `{"<plural>": [...], "<type>-count": n, "<type>-offset": n}`, sorted by MBID as
/// musicbrainz-server sorts it (genres by name).
#[must_use]
pub fn list(kind: Kind, mut items: Vec<Value>, total: usize, offset: usize) -> Answer {
    let key = if kind == Kind::Genre { "name" } else { "id" };
    items.sort_by(|a, b| {
        a.get(key)
            .and_then(Value::as_str)
            .cmp(&b.get(key).and_then(Value::as_str))
    });
    let mut out = Map::new();
    out.insert(kind.plural().into(), Value::Array(items));
    out.insert(format!("{}-count", kind.name()), json!(total));
    out.insert(format!("{}-offset", kind.name()), json!(offset));
    Answer::Ok(list_root(kind), Value::Object(out))
}

#[must_use]
pub fn list_root(kind: Kind) -> &'static str {
    match kind {
        Kind::Area => "area-list",
        Kind::Artist => "artist-list",
        Kind::Event => "event-list",
        Kind::Genre => "genre-list",
        Kind::Instrument => "instrument-list",
        Kind::Label => "label-list",
        Kind::Place => "place-list",
        Kind::Recording => "recording-list",
        Kind::Release => "release-list",
        Kind::ReleaseGroup => "release-group-list",
        Kind::Series => "series-list",
        Kind::Url => "url-list",
        Kind::Work => "work-list",
    }
}

/// Cuts a page of releases so that it holds at most [`TRACK_LIMIT`] tracks (but at least one release).
fn limit_by_tracks(store: &Store, ids: &mut Vec<u32>) {
    let mut tracks = 0;
    let mut keep = 0;
    for id in ids.iter() {
        tracks += store
            .t("medium")
            .by("release", *id)
            .filter_map(|m| m.u32("track_count"))
            .sum::<u32>();
        if tracks > TRACK_LIMIT && keep > 0 {
            break;
        }
        keep += 1;
    }
    ids.truncate(keep);
}

fn url_by_resource(store: &Store, params: &Params) -> Result<Answer, Missing> {
    let inc = bad!(inc_param("url", Mode::Lookup, params));
    let filter = Filter::default();
    let ctx = Ctx {
        store,
        inc: &inc,
        filter: &filter,
        xml: params.1,
    };
    let resources = params.all("resource");
    let find = |url: &str| store.t("url").find("url", url).first().map(|r| r.id);
    if let [one] = resources.as_slice() {
        let Some(id) = find(one) else {
            return Ok(Answer::not_found());
        };
        return Ok(Answer::Ok("url", ctx.url(id, Opts::toplevel())?));
    }
    let mut items = Vec::new();
    for url in resources.iter().take(100) {
        if let Some(id) = find(url) {
            items.push(ctx.url(id, Opts::toplevel())?);
        }
    }
    let total = items.len();
    Ok(list(Kind::Url, items, total, 0))
}

fn isrc(store: &Store, isrc: &str, params: &Params) -> Result<Answer, Missing> {
    let inc = bad!(inc_param("isrc", Mode::Lookup, params));
    let valid = isrc.len() == 12
        && isrc.bytes().take(2).all(|b| b.is_ascii_uppercase())
        && isrc
            .bytes()
            .skip(2)
            .take(3)
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        && isrc.bytes().skip(5).all(|b| b.is_ascii_digit());
    if !valid {
        return Ok(Answer::bad("Invalid isrc."));
    }
    let mut ids: Vec<(String, u32)> = store
        .t("isrc")
        .find("isrc", isrc)
        .iter()
        .filter_map(|r| r.u32("recording"))
        .filter_map(|id| store.t("recording").get(id).map(|r| (r.text("name"), id)))
        .collect();
    if ids.is_empty() {
        return Ok(Answer::not_found());
    }
    ids.sort();
    let filter = Filter::default();
    let ctx = Ctx {
        store,
        inc: &inc,
        filter: &filter,
        xml: params.1,
    };
    // Loaded as the requested recordings, serialized as nested ones.
    let o = Opts {
        toplevel: false,
        ..Opts::toplevel()
    };
    let recordings = ids
        .iter()
        .map(|(_, id)| ctx.recording(*id, o))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Answer::Ok(
        "isrc",
        json!({"isrc": isrc, "recordings": recordings}),
    ))
}

fn iswc(store: &Store, iswc: &str, params: &Params) -> Result<Answer, Missing> {
    let inc = bad!(inc_param("iswc", Mode::Lookup, params));
    // T-DDD.DDD.DDD-C, with or without the separators
    let digits: String = iswc.chars().filter(char::is_ascii_digit).collect();
    if !(iswc.starts_with('T') && digits.len() == 10) {
        return Ok(Answer::bad("Invalid iswc."));
    }
    let normalized = format!(
        "T-{}.{}.{}-{}",
        digits.get(0..3).unwrap_or(""),
        digits.get(3..6).unwrap_or(""),
        digits.get(6..9).unwrap_or(""),
        digits.get(9..10).unwrap_or("")
    );
    let mut ids: Vec<(String, u32)> = store
        .t("iswc")
        .find("iswc", &normalized)
        .iter()
        .filter_map(|r| r.u32("work"))
        .filter_map(|id| store.t("work").get(id).map(|w| (w.text("name"), id)))
        .collect();
    if ids.is_empty() {
        return Ok(Answer::not_found());
    }
    ids.sort();
    let filter = Filter::default();
    let ctx = Ctx {
        store,
        inc: &inc,
        filter: &filter,
        xml: params.1,
    };
    let works = ids
        .iter()
        .map(|(_, id)| ctx.work(*id, Opts::toplevel()))
        .collect::<Result<Vec<_>, _>>()?;
    let total = works.len();
    Ok(list(Kind::Work, works, total, 0))
}

fn discid(store: &Store, discid: &str, params: &Params) -> Result<Answer, Missing> {
    let mut inc = bad!(inc_param("discid", Mode::Lookup, params));
    let valid = discid.len() == 28
        && discid
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    let toc = params.get("toc");
    if !valid && toc.is_none() {
        return Ok(Answer::bad("Invalid discid."));
    }
    if let Some(cdstubs) = params.value("cdstubs")
        && cdstubs != "yes"
        && cdstubs != "no"
    {
        return Ok(Answer::bad(
            "Invalid argument \"cdstubs\": must be \"yes\" or \"no\"",
        ));
    }
    let mut page = bad!(page(params));
    page.limit = page.limit.min(25);
    inc.media = true;
    inc.discids = true;
    let filter = Filter::default();
    let ctx = Ctx {
        store,
        inc: &inc,
        filter: &filter,
        xml: params.1,
    };
    if valid && let Some(cdtoc) = store.t("cdtoc").find("discid", discid).first().copied() {
        let mut releases: Vec<u32> = store
            .t("medium_cdtoc")
            .by("cdtoc", cdtoc.id)
            .filter_map(|mc| store.t("medium").get(mc.u32("medium")?)?.u32("release"))
            .collect();
        releases.sort_unstable();
        releases.dedup();
        let releases = releases
            .into_iter()
            .skip(page.offset)
            .take(page.limit)
            .map(|r| ctx.release(r, Opts::toplevel()))
            .collect::<Result<Vec<_>, _>>()?;
        let mut disc = entity::cdtoc_json(&cdtoc);
        if let Value::Object(obj) = &mut disc {
            obj.insert("releases".into(), Value::Array(releases));
        }
        return Ok(Answer::Ok("disc", disc));
    }
    if toc.is_some() {
        // Looking releases up by the track lengths of a TOC needs musicbrainz-server's medium_index, which the
        // dumps don't have in a usable form: no fuzzy matches.
        return Ok(list(Kind::Release, Vec::new(), 0, page.offset));
    }
    Ok(Answer::not_found())
}

fn genre_all(store: &Store, params: &Params, format: Format) -> Result<Answer, Missing> {
    let mut genres: Vec<(String, u32)> = store
        .t("genre")
        .iter()
        .map(|g| (g.text("name"), g.id))
        .collect();
    genres.sort();
    if format == Format::Txt {
        return Ok(Answer::Text(
            genres.into_iter().map(|(n, _)| n + "\n").collect(),
        ));
    }
    let page = bad!(page(params));
    let inc = Inc::default();
    let filter = Filter::default();
    let ctx = Ctx {
        store,
        inc: &inc,
        filter: &filter,
        xml: params.1,
    };
    let total = genres.len();
    let items = genres
        .iter()
        .skip(page.offset)
        .take(page.limit)
        .map(|(_, id)| ctx.genre(*id, Opts::toplevel()))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(list(Kind::Genre, items, total, page.offset))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mbids() {
        assert!(is_mbid("b84ee12a-09ef-421b-82de-0441a926375b"));
        assert!(is_mbid("B84EE12A-09EF-421B-82DE-0441A926375B"));
        assert!(!is_mbid("b84ee12a09ef421b82de0441a926375b"));
        assert!(!is_mbid("x84ee12a-09ef-421b-82de-0441a926375b"));
        assert!(!is_mbid("00000000-0000-0000-0000-000000000000"));
    }

    #[test]
    fn pages() {
        let page = |q: &str| super::page(&Params::parse(Some(q))).map(|p| (p.limit, p.offset));
        assert_eq!(page("limit=0"), Ok((25, 0)));
        assert_eq!(page("limit=500"), Ok((100, 0)));
        assert_eq!(page("offset=7"), Ok((25, 7)));
        assert!(page("limit=-1").is_err());
        assert!(page("offset=x").is_err());
    }
}
