//! The Cover Art Archive's API: not there yet.

use axum::Router;

use super::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
}
