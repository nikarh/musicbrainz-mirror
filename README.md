# musicbrainz-mirror

A self-hosted, read-only [MusicBrainz](https://musicbrainz.org) web service: the `/ws/2` API (lookups, browsing,
search; JSON and XML) and the [Cover Art Archive](https://coverartarchive.org)'s listing API, answered from a
local copy of MusicBrainz's [database dumps](https://musicbrainz.org/doc/MusicBrainz_Database/Download), which it
keeps up to date by itself. One binary and an SQLite file instead of
[musicbrainz-docker](https://github.com/metabrainz/musicbrainz-docker)'s PostgreSQL, Solr, Redis and website. Useful
when a music library needs MusicBrainz for many albums: no rate limit, no network round trips.

Point a client at it instead of `musicbrainz.org`: beets (`musicbrainz.host`), Picard (Options → Network), or
anything built on musicbrainzngs (`musicbrainzngs.set_hostname`).

## What it answers

- **Lookups** `/ws/2/<entity>/<mbid>?inc=...` of areas, artists, events, genres, instruments, labels, places,
  recordings, releases, release groups, series, URLs and works, also by MBIDs merged into another; `/ws/2/discid`,
  `/ws/2/isrc`, `/ws/2/iswc`, `/ws/2/url?resource=`, `/ws/2/genre/all`.
- **Browsing** `/ws/2/<entity>?<linked entity>=<mbid>`, with `limit`, `offset`, `type` and `status`.
- **Search** `/ws/2/<entity>?query=...` in MusicBrainz's Lucene syntax, for every entity type above (but genres and
  URLs) and tags.
- JSON (`fmt=json` or `Accept: application/json`) and XML (otherwise, as musicbrainz.org).
- The Cover Art Archive's `/release/<mbid>` and `/release-group/<mbid>` (and `/front`, `/back`, `/front-250`, ...).
  The dumps list a release's images, not the images themselves: their URLs point at the Cover Art Archive.

Lookups and browsing answer what musicbrainz.org answers, field for field; a test checks this against answers
recorded from musicbrainz.org ([tests/compat.rs](tests/compat.rs)). Where it differs:

- **Search** is SQLite's full-text search, not MusicBrainz's Solr. The results are shaped the same and the obvious
  matches come first, but the ranking, the scores and the counts are not the same. A query is matched as a whole
  first; when that finds too few, any part of it may match. Field values (`tracks:12`, `date:2001`, `country:GB`)
  only rank results unless required with `+`, so a wrong hint doesn't hide the right result.
- Nothing that needs an account: no collections (browsing by collection answers 404), and `user-tags`,
  `user-genres`, `user-ratings` are empty. Anything that writes answers 503, as a read-only MusicBrainz does.
- `/ws/2/discid` finds releases by disc ID only, not by a TOC's track lengths (`toc=` without a matching disc ID
  answers an empty list). There are no CD stubs, and searches of annotations, CD stubs and URLs find nothing.
- Data is as of the dump: MusicBrainz publishes one twice a week.

## Disk space and time

The three archives it needs (core data, derived data such as tags and ratings, the Cover Art Archive's) are about
8 GB to download (2026). The database built from them is large: over 40 GB, and an import of a full dump takes more
than an hour on a fast desktop (decompressing the 7 GB core archive alone is 10 minutes of one core). A more compact
store that builds much faster is being worked on. During an update the old and the new database both exist, and the
archives stay until the import is done: have room for twice the database plus the archives.

Until the first import is done, the web service answers 503. Later imports happen in the background; the old
database is served until the new one is ready.

## Usage

```yaml
services:
  musicbrainz:
    image: ghcr.io/nikarh/musicbrainz-mirror:latest
    restart: unless-stopped
    read_only: true
    cap_drop: [ALL]
    security_opt: [no-new-privileges:true]
    user: "1000:1000"
    volumes:
      - /srv/musicbrainz:/data
    ports:
      - "5000:5000"
```

- `read_only: true`: nothing is written outside `/data` (SQLite's temporary files of an import included), no
  tmpfs needed.
- `cap_drop: [ALL]`, `no-new-privileges`: it needs no privileges; it listens on 5000.
- `user`: any uid:gid that can write `/data` (the image defaults to nobody, 65534). A new named volume is writable by
  any user; a bind mount must belong to the chosen one.

Then, for beets:

```yaml
musicbrainz:
  host: musicbrainz:5000
  https: no
  ratelimit: 100
```

## Images and downloads

| | Image | Release (binary, signed checksums) |
| --- | --- | --- |
| Releases | `ghcr.io/nikarh/musicbrainz-mirror:X.Y.Z` (and `:X.Y`, `:latest`) | [latest release](https://github.com/nikarh/musicbrainz-mirror/releases/latest) |
| `main` | `ghcr.io/nikarh/musicbrainz-mirror:main` (and `:sha-<commit>`) | [`latest` pre-release](https://github.com/nikarh/musicbrainz-mirror/releases/tag/latest) |

The image is `FROM scratch`: a static x86_64 musl binary and CA certificates. How releases are made:
[docs/releasing.md](docs/releasing.md).

### Verify downloads

```sh
curl -LO https://github.com/nikarh/musicbrainz-mirror/releases/latest/download/musicbrainz-mirror-x86_64-unknown-linux-musl
curl -LO https://github.com/nikarh/musicbrainz-mirror/releases/latest/download/SHA256SUMS
curl -LO https://github.com/nikarh/musicbrainz-mirror/releases/latest/download/SHA256SUMS.asc
curl -L https://raw.githubusercontent.com/nikarh/musicbrainz-mirror/main/docs/signing-key.asc | gpg --import
gpg --verify SHA256SUMS.asc SHA256SUMS
sha256sum --check --ignore-missing SHA256SUMS
```

The key is `CDD60B735BD3CB6BC1A8C6AF9DDF0A471F709EEE` (ed25519, expires 2029-11-06).

## Configuration

Flags, each with an environment variable fallback:

| Flag | Environment | Default | |
| --- | --- | --- | --- |
| `--data-dir` | `MUSICBRAINZ_MIRROR_DATA_DIR` | `/data` | Databases (`musicbrainz-<dump>.sqlite`) and downloads (`import/`) |
| `--listen` | `MUSICBRAINZ_MIRROR_LISTEN` | `0.0.0.0:5000` | HTTP address |
| `--update-interval` | `MUSICBRAINZ_MIRROR_UPDATE_INTERVAL` | `6h` | How often to look for a newer dump (`30m`, `1d`); `0`: never |
| `--min-free-gb` | `MUSICBRAINZ_MIRROR_MIN_FREE_GB` | `150` | Free space an update needs before it starts |
| `--dump-url` | `MUSICBRAINZ_MIRROR_DUMP_URL` | MetaBrainz's | The full exports (a directory with `LATEST` and a directory per dump) |
| `--cover-art-url` | `MUSICBRAINZ_MIRROR_COVER_ART_URL` | `https://coverartarchive.org` | Where cover art image URLs point |
| `--cache-mb` | `MUSICBRAINZ_MIRROR_CACHE_MB` | `1024` | SQLite's page cache while importing |

`RUST_LOG` sets the log filter (default `info`).

A check for a new dump is one small request (`LATEST`); the served database is named after its dump, so the same
dump is never downloaded twice. An interrupted download resumes; every archive is checked against the dump's
`SHA256SUMS`. A dump whose import fails is skipped until the next start (or the next dump).

With updates off it serves the newest database in the data directory. `musicbrainz-mirror import` builds one from
archives (or directories they were unpacked into) without serving it:

```sh
musicbrainz-mirror import --out /srv/musicbrainz/musicbrainz-local.sqlite \
  mbdump.tar.bz2 mbdump-derived.tar.bz2 mbdump-cover-art-archive.tar.bz2
```

## How it works

The importer streams the archives (one decompressing thread per archive, one writer) into a new SQLite file: the
tables and columns the web service needs, in MusicBrainz's layout, with the per entity type tables (aliases, tags,
ratings, relationships, ...) merged. Then indexes, what MusicBrainz keeps in materialized tables (first release
dates, area containment), and FTS5 tables for search. A finished database is never written to again: it is
opened immutable, read through mmap by a pool of connections.

The JSON follows musicbrainz-server's serializer, including which fields appear where (an artist at the top level
has a life span and its areas, the same artist in an artist credit doesn't), the order of relationships, and the
order browse pages are cut in. Every query is checked to be answered from an index
([src/db/plan.rs](src/db/plan.rs)).

## Development

[mise](https://mise.jdx.dev) installs the tools; `mise run check` runs everything CI does, `mise run build` builds
the binary, `mise run docker` the image. `mise run import <archives>` builds `dev-data/musicbrainz-local.sqlite`,
`mise run dev` serves it on port 5000. MusicBrainz's
[sample dump](https://data.metabrainz.org/pub/musicbrainz/data/sample/) (350 MB, unpack it first) imports in two
minutes.

The compatibility test's fixture and expected answers are made by [examples/fixture.rs](examples/fixture.rs) (see
its header) from a database built from a full dump.

## License

This project's code is MIT or Apache-2.0, at your option. MusicBrainz's data is MetaBrainz's: the core data is in the
public domain (CC0), the rest under CC BY-NC-SA 3.0; see [MusicBrainz's license](https://musicbrainz.org/doc/About/Data_License).
The test fixture in `tests/fixtures` and the answers in `tests/compat/expected` are MusicBrainz data.
