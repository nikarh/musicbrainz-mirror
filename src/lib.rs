//! musicbrainz-mirror: MusicBrainz's web service (`/ws/2`, read-only) and the Cover Art Archive's API on a local
//! copy of MusicBrainz's database dumps, which it keeps up to date.
//!
//! - [`store`] holds a dump as immutable, memory-mapped files and a search index, built from the dump archives,
//! - [`mirror`] downloads new dumps, builds stores from them and switches to them without a restart,
//! - [`ws`] answers the web service from the store.

pub mod config;
pub mod date;
pub mod dump;
pub mod kind;
pub mod mirror;
pub mod store;
pub mod ws;
