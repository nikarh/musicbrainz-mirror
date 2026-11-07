//! Configuration from command line flags, with environment variables as the fallback.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

pub use crate::store::build::BuildOptions;

pub const DEFAULT_DUMP_URL: &str = "https://data.metabrainz.org/pub/musicbrainz/data/fullexport";
pub const DEFAULT_COVER_ART_URL: &str = "https://coverartarchive.org";

pub const HELP: &str = "\
musicbrainz-mirror: MusicBrainz's web service (ws/2, read-only) on a local copy of MusicBrainz's database dumps

Usage: musicbrainz-mirror [serve] [OPTIONS]
       musicbrainz-mirror import --out DIR [--dump ID] [--build-threads N] [--compress-rows] ARCHIVE_OR_DIR...
       musicbrainz-mirror healthcheck [--listen ADDR]

Options (each has an environment variable fallback):
  --data-dir DIR           MUSICBRAINZ_MIRROR_DATA_DIR        stores, downloads and builds [/data]
  --listen ADDR            MUSICBRAINZ_MIRROR_LISTEN          HTTP address [0.0.0.0:5000]
  --update-interval TIME   MUSICBRAINZ_MIRROR_UPDATE_INTERVAL how often to check for a new dump (30m, 6h, 7d);
                                                              0: never [6h]
  --min-free-gb GB         MUSICBRAINZ_MIRROR_MIN_FREE_GB     free space an update needs before it starts [40]
  --dump-url URL           MUSICBRAINZ_MIRROR_DUMP_URL        where the full exports are [MetaBrainz's]
  --cover-art-url URL      MUSICBRAINZ_MIRROR_COVER_ART_URL   where cover art images are [https://coverartarchive.org]
  --build-threads N        MUSICBRAINZ_MIRROR_BUILD_THREADS   threads that build the search indexes [2]
  --build-memory-mb MB     MUSICBRAINZ_MIRROR_BUILD_MEMORY_MB memory of the search index builders [1024]
  --compress-rows          MUSICBRAINZ_MIRROR_COMPRESS_ROWS   build stores with their rows compressed (smaller,
                                                              a little slower to read; stores of either kind are
                                                              served) [off]
  -h, --help               this help
  -V, --version            the version

import builds a store from dump archives (mbdump.tar.bz2, mbdump-derived.tar.bz2, mbdump-cover-art-archive.tar.bz2)
or directories they were unpacked into, without serving it. --out is a new directory (it must not exist).

Logging: RUST_LOG (default info).";

#[derive(Debug, Clone)]
pub struct Config {
    pub data_dir: PathBuf,
    pub listen: SocketAddr,
    /// `None`: never check for a new dump (the newest database in the data directory is served).
    pub update_interval: Option<Duration>,
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
        let interval: Interval = value(
            args,
            "--update-interval",
            "MUSICBRAINZ_MIRROR_UPDATE_INTERVAL",
            Interval(Some(Duration::from_hours(6))),
        )?;
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
            update_interval: interval.0,
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

/// A duration like `90s`, `30m`, `6h` or `7d`; `0` is none.
struct Interval(Option<Duration>);

impl FromStr for Interval {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, ()> {
        if s == "0" {
            return Ok(Self(None));
        }
        let split = s.find(|c: char| !c.is_ascii_digit()).ok_or(())?;
        let (number, unit) = s.split_at(split);
        let number: u64 = number.parse().map_err(|_| ())?;
        let seconds = match unit {
            "s" => 1,
            "m" => 60,
            "h" => 3600,
            "d" => 86400,
            _ => return Err(()),
        };
        match number.checked_mul(seconds) {
            Some(0) => Ok(Self(None)),
            Some(secs) => Ok(Self(Some(Duration::from_secs(secs)))),
            None => Err(()),
        }
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
            "--update-interval",
            "0",
            "--dump-url",
            "http://h/",
        ])
        .unwrap();
        assert_eq!(c.data_dir, PathBuf::from("/x"));
        assert_eq!(c.update_interval, None);
        assert_eq!(c.dump_url, "http://h");
        assert_eq!(c.listen.port(), 5000);
        assert_eq!(c.cover_art_url, DEFAULT_COVER_ART_URL);
    }

    #[test]
    fn rejects_unknown_and_invalid() {
        assert!(matches!(parse(&["--nope"]), Err(ConfigError::Unknown(_))));
        assert!(matches!(
            parse(&["--update-interval", "x"]),
            Err(ConfigError::Invalid { .. })
        ));
        assert!(matches!(
            parse(&["--update-interval", "24"]),
            Err(ConfigError::Invalid { .. })
        ));
    }

    #[test]
    fn intervals() {
        let interval = |s: &str| parse(&["--update-interval", s]).unwrap().update_interval;
        assert_eq!(interval("90s"), Some(Duration::from_secs(90)));
        assert_eq!(interval("30m"), Some(Duration::from_mins(30)));
        assert_eq!(interval("7d"), Some(Duration::from_hours(7 * 24)));
        assert_eq!(interval("0"), None);
        assert_eq!(
            parse(&[]).unwrap().update_interval,
            Some(Duration::from_hours(6))
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
