//! musicbrainz-mirror: MusicBrainz's web service (`/ws/2`, read-only) on a local copy of MusicBrainz's database
//! dumps.
//!
//! - [`import`] builds an SQLite database from the dump archives.

pub mod import;
pub mod kind;
