//! musicbrainz-mirror: MusicBrainz's web service (`/ws/2`, read-only) and the Cover Art Archive's API on a local
//! copy of MusicBrainz's database dumps, which it keeps up to date.
//!
//! - [`import`] builds an SQLite database from the dump archives,
//! - [`mirror`] downloads new dumps, imports them and switches to them without a restart,
//! - [`ws`] answers the web service from the database.

pub mod config;
pub mod db;
pub mod dump;
pub mod import;
pub mod kind;
pub mod mirror;
pub mod ws;
