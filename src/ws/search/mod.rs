//! Search: not there yet.

use rusqlite::Connection;

use super::Answer;
use super::browse::Page;
use crate::db::Lookups;
use crate::kind::Kind;

pub fn search(_: &Connection, _: &Lookups, _: Kind, _: &str, _: Page) -> rusqlite::Result<Answer> {
    Ok(Answer::Error(
        axum::http::StatusCode::NOT_IMPLEMENTED,
        "This hasn't been implemented yet.".into(),
    ))
}

pub fn search_other(
    conn: &Connection,
    _: &str,
    query: &str,
    page: Page,
) -> rusqlite::Result<Answer> {
    search(conn, &Lookups::default(), Kind::Artist, query, page)
}
