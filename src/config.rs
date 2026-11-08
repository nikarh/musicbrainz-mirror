//! Configuration from command line flags, with environment variables as the fallback.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use jiff::tz::TimeZone;
use jiff::{Timestamp, Zoned};

pub use crate::store::build::{BuildOptions, SchemaMismatch};

pub const DEFAULT_DUMP_URL: &str = "https://data.metabrainz.org/pub/musicbrainz/data/fullexport";
pub const DEFAULT_COVER_ART_URL: &str = "https://coverartarchive.org";

pub const HELP: &str = "\
musicbrainz-mirror: MusicBrainz's web service (ws/2, read-only) on a local copy of MusicBrainz's database dumps

Usage: musicbrainz-mirror [serve] [OPTIONS]
       musicbrainz-mirror import --out DIR [--dump ID] [--build-threads N] [--compress-rows]
                                 [--schema-mismatch MODE] ARCHIVE_OR_DIR...
       musicbrainz-mirror healthcheck [--listen ADDR]

Options (each has an environment variable fallback):
  --data-dir DIR           MUSICBRAINZ_MIRROR_DATA_DIR        stores, downloads and builds [/data]
  --listen ADDR            MUSICBRAINZ_MIRROR_LISTEN          HTTP address [0.0.0.0:5000]
  --update-schedule CRON   MUSICBRAINZ_MIRROR_UPDATE_SCHEDULE when to check for a new dump, besides at start: a cron
                                                              expression (minute hour day month weekday) in the
                                                              time zone of TZ (else the system's); off: never
                                                              [34 2 * * *, every day at 02:34]
  --min-free-gb GB         MUSICBRAINZ_MIRROR_MIN_FREE_GB     free space an update needs before it starts (what an
                                                              interrupted one downloaded and built counts) [40]
  --dump-url URL           MUSICBRAINZ_MIRROR_DUMP_URL        where the full exports are [MetaBrainz's]
  --cover-art-url URL      MUSICBRAINZ_MIRROR_COVER_ART_URL   where cover art images are [https://coverartarchive.org]
  --build-threads N        MUSICBRAINZ_MIRROR_BUILD_THREADS   threads that build the search indexes [2]
  --build-memory-mb MB     MUSICBRAINZ_MIRROR_BUILD_MEMORY_MB memory of the search index builders [1024]
  --compress-rows          MUSICBRAINZ_MIRROR_COMPRESS_ROWS   build stores with their rows compressed (smaller,
                                                              a little slower to read; stores of either kind are
                                                              served) [off]
  --schema-mismatch MODE   MUSICBRAINZ_MIRROR_SCHEMA_MISMATCH a dump of another MusicBrainz schema than this version
                                                              reads: refuse (the import fails; the store being
                                                              served stays) or warn (imported anyway, and fails
                                                              only if a table's column count changed) [refuse]
  -h, --help               this help
  -V, --version            the version

import builds a store from dump archives (mbdump.tar.bz2, mbdump-derived.tar.bz2, mbdump-cover-art-archive.tar.bz2)
or directories they were unpacked into, without serving it. --out is a new directory, or one an interrupted import
(of the same --dump) left: that import continues.

Logging: RUST_LOG (default info).";

#[derive(Debug, Clone)]
pub struct Config {
    pub data_dir: PathBuf,
    pub listen: SocketAddr,
    /// `None`: never check for a new dump (the newest database in the data directory is served).
    pub update_schedule: Option<Schedule>,
    pub min_free_bytes: u64,
    pub dump_url: String,
    pub cover_art_url: String,
    pub build: BuildOptions,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{name}: invalid value {value:?}")]
    Invalid { name: String, value: String },
    #[error(transparent)]
    Args(#[from] pico_args::Error),
    #[error("unknown arguments: {0}")]
    Unknown(String),
    #[error("{old} was replaced by {new}")]
    Replaced {
        old: &'static str,
        new: &'static str,
    },
}

impl Config {
    pub fn parse(args: &mut pico_args::Arguments) -> Result<Self, ConfigError> {
        let data_dir: String = value(
            args,
            "--data-dir",
            "MUSICBRAINZ_MIRROR_DATA_DIR",
            "/data".to_owned(),
        )?;
        let listen = value(
            args,
            "--listen",
            "MUSICBRAINZ_MIRROR_LISTEN",
            SocketAddr::from(([0, 0, 0, 0], 5000)),
        )?;
        if args.contains("--update-interval") {
            return Err(ConfigError::Replaced {
                old: "--update-interval",
                new: "--update-schedule",
            });
        }
        if std::env::var_os("MUSICBRAINZ_MIRROR_UPDATE_INTERVAL").is_some() {
            return Err(ConfigError::Replaced {
                old: "MUSICBRAINZ_MIRROR_UPDATE_INTERVAL",
                new: "MUSICBRAINZ_MIRROR_UPDATE_SCHEDULE",
            });
        }
        let cron: Cron = value(
            args,
            "--update-schedule",
            "MUSICBRAINZ_MIRROR_UPDATE_SCHEDULE",
            Cron(Some(default_schedule())),
        )?;
        let update_schedule = match cron.0 {
            Some(cron) => Some(Schedule {
                cron,
                tz: time_zone()?,
            }),
            None => None,
        };
        let min_free_gb: u64 = value(args, "--min-free-gb", "MUSICBRAINZ_MIRROR_MIN_FREE_GB", 40)?;
        let dump_url = value(
            args,
            "--dump-url",
            "MUSICBRAINZ_MIRROR_DUMP_URL",
            DEFAULT_DUMP_URL.to_owned(),
        )?;
        let cover_art_url = value(
            args,
            "--cover-art-url",
            "MUSICBRAINZ_MIRROR_COVER_ART_URL",
            DEFAULT_COVER_ART_URL.to_owned(),
        )?;
        let build = build_options(args)?;
        finish(args)?;
        Ok(Self {
            data_dir: PathBuf::from(data_dir),
            listen,
            update_schedule,
            min_free_bytes: min_free_gb.saturating_mul(1 << 30),
            dump_url: dump_url.trim_end_matches('/').to_owned(),
            cover_art_url: cover_art_url.trim_end_matches('/').to_owned(),
            build,
        })
    }
}

/// `import` subcommand arguments.
#[derive(Debug)]
pub struct ImportArgs {
    pub out: PathBuf,
    pub dump: String,
    pub build: BuildOptions,
    pub sources: Vec<PathBuf>,
}

impl ImportArgs {
    pub fn parse(mut args: pico_args::Arguments) -> Result<Self, ConfigError> {
        let out: PathBuf = args.value_from_str("--out")?;
        let dump = args
            .opt_value_from_str("--dump")?
            .unwrap_or_else(|| "local".to_owned());
        let build = build_options(&mut args)?;
        let sources: Vec<PathBuf> = args.finish().into_iter().map(PathBuf::from).collect();
        if sources.is_empty() {
            return Err(ConfigError::Unknown(
                "no dump archives or directories given".into(),
            ));
        }
        Ok(Self {
            out,
            dump,
            build,
            sources,
        })
    }
}

fn build_options(args: &mut pico_args::Arguments) -> Result<BuildOptions, ConfigError> {
    Ok(BuildOptions {
        threads: value(
            args,
            "--build-threads",
            "MUSICBRAINZ_MIRROR_BUILD_THREADS",
            2,
        )?,
        memory_mb: value(
            args,
            "--build-memory-mb",
            "MUSICBRAINZ_MIRROR_BUILD_MEMORY_MB",
            1024,
        )?,
        compress_rows: switch(args, "--compress-rows", "MUSICBRAINZ_MIRROR_COMPRESS_ROWS")?,
        schema_mismatch: value(
            args,
            "--schema-mismatch",
            "MUSICBRAINZ_MIRROR_SCHEMA_MISMATCH",
            SchemaMismatch::Refuse,
        )?,
    })
}

/// An on/off option: the flag, else the environment variable (`true`/`false`, `1`/`0`).
fn switch(
    args: &mut pico_args::Arguments,
    flag: &'static str,
    env: &str,
) -> Result<bool, ConfigError> {
    if args.contains(flag) {
        return Ok(true);
    }
    match std::env::var(env).as_deref() {
        Err(_) | Ok("" | "false" | "0") => Ok(false),
        Ok("true" | "1") => Ok(true),
        Ok(other) => Err(ConfigError::Invalid {
            name: env.to_owned(),
            value: other.to_owned(),
        }),
    }
}

fn finish(args: &mut pico_args::Arguments) -> Result<(), ConfigError> {
    let rest = std::mem::replace(args, pico_args::Arguments::from_vec(Vec::new())).finish();
    if rest.is_empty() {
        return Ok(());
    }
    let rest: Vec<_> = rest
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    Err(ConfigError::Unknown(rest.join(" ")))
}

/// The default schedule: every day at 02:34.
pub const DEFAULT_UPDATE_SCHEDULE: &str = "34 2 * * *";

/// When to check for a new dump: a cron expression in a time zone.
#[derive(Debug, Clone)]
pub struct Schedule {
    cron: croner::Cron,
    tz: TimeZone,
}

impl Schedule {
    /// A cron expression (`minute hour day month weekday`, or a nickname like `@daily`) in the time zone `tz`.
    pub fn parse(expr: &str, tz: TimeZone) -> Result<Self, croner::errors::CronError> {
        Ok(Self {
            cron: expr.parse()?,
            tz,
        })
    }

    /// The first time after `now` the schedule fires (`None` for one that never does, like `0 0 30 2 *`).
    #[must_use]
    pub fn next_after(&self, now: Timestamp) -> Option<Zoned> {
        self.cron
            .find_next_occurrence(&now.to_zoned(self.tz.clone()), false)
            .ok()
    }

    /// The next time the schedule fires, and how long until then.
    #[must_use]
    pub fn next(&self) -> Option<(Zoned, Duration)> {
        let now = Timestamp::now();
        let next = self.next_after(now)?;
        let wait = Duration::try_from(now.duration_until(next.timestamp())).unwrap_or_default();
        Some((next, wait))
    }
}

impl Default for Schedule {
    /// Every day at 02:34 UTC.
    fn default() -> Self {
        Self {
            cron: default_schedule(),
            tz: TimeZone::UTC,
        }
    }
}

impl std::fmt::Display for Schedule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let tz = self.tz.iana_name().unwrap_or("local time");
        write!(f, "{} ({tz})", self.cron.pattern.as_str())
    }
}

/// A cron expression; `off` is none.
struct Cron(Option<croner::Cron>);

fn default_schedule() -> croner::Cron {
    // A test checks that it parses.
    DEFAULT_UPDATE_SCHEDULE
        .parse()
        .unwrap_or_else(|_| unreachable!("the default schedule parses"))
}

impl FromStr for Cron {
    type Err = croner::errors::CronError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.trim().eq_ignore_ascii_case("off") {
            return Ok(Self(None));
        }
        s.parse().map(|c| Self(Some(c)))
    }
}

/// The time zone of `TZ`, else the system's, else UTC; a `TZ` that names no time zone is an error.
fn time_zone() -> Result<TimeZone, ConfigError> {
    match TimeZone::try_system() {
        Ok(tz) => Ok(tz),
        Err(_) => match std::env::var("TZ") {
            Ok(tz) if !tz.is_empty() => Err(ConfigError::Invalid {
                name: "TZ".to_owned(),
                value: tz,
            }),
            _ => Ok(TimeZone::UTC),
        },
    }
}

/// The flag's value, else the environment variable's, else the default.
fn value<T: FromStr>(
    args: &mut pico_args::Arguments,
    flag: &'static str,
    env: &str,
    default: T,
) -> Result<T, ConfigError> {
    let raw = match args.opt_value_from_str::<_, String>(flag)? {
        Some(v) => (flag, v),
        None => match std::env::var(env) {
            Ok(v) if !v.is_empty() => (env, v),
            _ => return Ok(default),
        },
    };
    raw.1.parse().map_err(|_| ConfigError::Invalid {
        name: raw.0.to_owned(),
        value: raw.1,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Config, ConfigError> {
        let mut args = pico_args::Arguments::from_vec(args.iter().map(Into::into).collect());
        Config::parse(&mut args)
    }

    #[test]
    fn flags_override_defaults() {
        let c = parse(&[
            "--data-dir",
            "/x",
            "--update-schedule",
            "off",
            "--dump-url",
            "http://h/",
        ])
        .unwrap();
        assert_eq!(c.data_dir, PathBuf::from("/x"));
        assert!(c.update_schedule.is_none());
        assert_eq!(c.dump_url, "http://h");
        assert_eq!(c.listen.port(), 5000);
        assert_eq!(c.cover_art_url, DEFAULT_COVER_ART_URL);
        assert_eq!(c.build.schema_mismatch, SchemaMismatch::Refuse);
        let c = parse(&["--schema-mismatch", "warn"]).unwrap();
        assert_eq!(c.build.schema_mismatch, SchemaMismatch::Warn);
        assert!(matches!(
            parse(&["--schema-mismatch", "ignore"]),
            Err(ConfigError::Invalid { .. })
        ));
    }

    #[test]
    fn rejects_unknown_and_invalid() {
        assert!(matches!(parse(&["--nope"]), Err(ConfigError::Unknown(_))));
        assert!(matches!(
            parse(&["--update-schedule", "x"]),
            Err(ConfigError::Invalid { .. })
        ));
        assert!(matches!(
            parse(&["--update-schedule", "61 * * * *"]),
            Err(ConfigError::Invalid { .. })
        ));
        assert!(matches!(
            parse(&["--update-interval", "24h"]),
            Err(ConfigError::Replaced { .. })
        ));
    }

    #[test]
    fn schedules() {
        let at = |cron: &str, tz: &str, now: &str| {
            Schedule::parse(cron, jiff::tz::db().get(tz).unwrap())
                .unwrap()
                .next_after(now.parse().unwrap())
                .unwrap()
                .timestamp()
                .to_string()
        };
        // the default: every day at 02:34
        let daily = DEFAULT_UPDATE_SCHEDULE;
        assert_eq!(
            at(daily, "UTC", "2026-11-08T01:00:00Z"),
            "2026-11-08T02:34:00Z"
        );
        assert_eq!(
            at(daily, "UTC", "2026-11-08T02:34:00Z"),
            "2026-11-09T02:34:00Z"
        );
        // in the time zone given, across a daylight saving change
        assert_eq!(
            at(daily, "Europe/Riga", "2026-10-24T12:00:00Z"),
            "2026-10-24T23:34:00Z"
        );
        assert_eq!(
            at(daily, "Europe/Riga", "2026-10-25T12:00:00Z"),
            "2026-10-26T00:34:00Z"
        );
        assert_eq!(
            at("0 */6 * * *", "UTC", "2026-11-08T07:00:00Z"),
            "2026-11-08T12:00:00Z"
        );
        assert_eq!(
            at("@weekly", "UTC", "2026-11-08T07:00:00Z"),
            "2026-11-15T00:00:00Z"
        );
        assert!("Off".parse::<Cron>().unwrap().0.is_none());
        assert_eq!(
            parse(&[])
                .unwrap()
                .update_schedule
                .unwrap()
                .cron
                .pattern
                .as_str(),
            DEFAULT_UPDATE_SCHEDULE
        );
    }

    #[test]
    fn import_args() {
        let args = |a: &[&str]| {
            ImportArgs::parse(pico_args::Arguments::from_vec(
                a.iter().map(Into::into).collect(),
            ))
        };
        let a = args(&[
            "--out",
            "x.sqlite",
            "--dump",
            "20261104-002222",
            "a.tar.bz2",
            "dir",
        ])
        .unwrap();
        assert_eq!(a.out, PathBuf::from("x.sqlite"));
        assert_eq!(a.dump, "20261104-002222");
        assert_eq!(a.sources.len(), 2);
        assert!(args(&["--out", "x"]).is_err());
    }
}
