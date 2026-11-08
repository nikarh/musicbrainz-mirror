# musicbrainz-mirror

A self-hosted, read-only [MusicBrainz](https://musicbrainz.org) web service: the `/ws/2` API (lookups, browsing,
search; JSON and XML) and the [Cover Art Archive](https://coverartarchive.org)'s listing API, answered from a
local copy of MusicBrainz's [database dumps](https://musicbrainz.org/doc/MusicBrainz_Database/Download), which it
keeps up to date by itself. One binary and a directory of memory-mapped files instead of
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

- **Search** is [tantivy](https://github.com/quickwit-oss/tantivy) (BM25), not MusicBrainz's Solr. Every search
  field of MusicBrainz's search server exists and the query syntax is Lucene's (`+`/`AND` required, `-`/`NOT`
  excluded, the other clauses rank, and one of them must match when none is required), so results are shaped the
  same and the obvious matches come first; the ranking, the scores (100 for the best match) and the counts are not
  the same.
- Nothing that needs an account: no collections (browsing by collection answers 404), and `user-tags`,
  `user-genres`, `user-ratings` are empty. Anything that writes answers 503, as a read-only MusicBrainz does.
- `/ws/2/discid` finds releases by disc ID only, not by a TOC's track lengths (`toc=` without a matching disc ID
  answers an empty list). There are no CD stubs, and searches of annotations, CD stubs and URLs find nothing.
- Data is as of the dump: MusicBrainz publishes one twice a week.

## Disk space and time

The three archives it needs (core data, derived data such as tags and ratings, the Cover Art Archive's) are about
8 GB to download (2026). The store built from a full dump (November 2026) is about 19 GB: 15 GB of tables and
indexes, 4 GB of search indexes. Building it takes about 10 minutes of one core to decompress the 7 GB core archive
(the other archives decompress at the same time), 3 minutes of writing tables, and 6 minutes of search indexing with
4 threads (`--build-threads`). With `--compress-rows` the store is about 15 GB (11 GB of tables), for 12 more
seconds of building; lookups take about a third longer (a release with everything beets asks for: 2 ms instead of
1.6 ms, at the median), searches the same. During an update the old and the new store both exist, and the archives stay until
the build is done: have room for twice the store plus the archives.

Until the first build is done, the web service answers 503. Later builds happen in the background; the old store is
served until the new one is ready.

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

- `read_only: true`: nothing is written outside `/data` (temporary files of a build included), no tmpfs needed.
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

Two different "latest"s: the image tag `:latest` (and GitHub's
[latest release](https://github.com/nikarh/musicbrainz-mirror/releases/latest)) is the newest release, while the
GitHub pre-release named `latest` is rebuilt from every push to `main`, together with the image tags `:main` and
`:sha-<commit>`. Builds of `main` say so in `--version`: `0.1.1-dev+0333d98` is commit `0333d98`, after release 0.1.1
or on the way to the next one. To verify a build of `main`, download from `releases/download/latest/` instead of
`releases/latest/download/`.

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
| `--data-dir` | `MUSICBRAINZ_MIRROR_DATA_DIR` | `/data` | Stores (`musicbrainz-<dump>/`), downloads and builds (`import/`) |
| `--listen` | `MUSICBRAINZ_MIRROR_LISTEN` | `0.0.0.0:5000` | HTTP address |
| `--update-interval` | `MUSICBRAINZ_MIRROR_UPDATE_INTERVAL` | `6h` | How often to look for a newer dump (`30m`, `1d`); `0`: never |
| `--min-free-gb` | `MUSICBRAINZ_MIRROR_MIN_FREE_GB` | `40` | Free space an update needs before it starts (what an interrupted one downloaded and built counts as free) |
| `--dump-url` | `MUSICBRAINZ_MIRROR_DUMP_URL` | MetaBrainz's | The full exports (a directory with `LATEST` and a directory per dump) |
| `--cover-art-url` | `MUSICBRAINZ_MIRROR_COVER_ART_URL` | `https://coverartarchive.org` | Where cover art image URLs point |
| `--build-threads` | `MUSICBRAINZ_MIRROR_BUILD_THREADS` | `2` | Threads that build the search indexes |
| `--build-memory-mb` | `MUSICBRAINZ_MIRROR_BUILD_MEMORY_MB` | `1024` | Memory of the search index builders (the rest of a build needs little) |
| `--compress-rows` | `MUSICBRAINZ_MIRROR_COMPRESS_ROWS` | off | Build stores with their rows compressed: smaller, a little slower to read (see below) |

`RUST_LOG` sets the log filter (default `info`).

What the log says at `info`: the settings it started with; the store served, its size and the free space; every
check for a new dump (and when the next one is); downloads (progress, checksum verified), each archive read, each
table written, each search index built, how long each step took; the switch to a new store and the removal of the old
one and of the downloads. Every request is two lines, paired by `id`: `received` (method, URL, user agent) and
`answered` (status, total time, time spent answering without the wait for a free thread, size, and what it held:
what a search found or a browse listed and how many of them it returned, the entity a lookup found, the error).
`RUST_LOG=info,access=warn` leaves the requests out.

A check for a new dump is one small request (`LATEST`); the served store is named after its dump, so the same
dump is never downloaded twice. An interrupted download resumes; every archive is checked against the dump's
`SHA256SUMS`. A dump whose build fails is skipped until the next start (or the next dump).

The process can be stopped (or killed, or lose power) at any point: a store is only served once all of it is on
disk, and nothing is lost but the work in progress. After a restart, archives already downloaded are checked rather
than downloaded again, an interrupted download continues where it stopped, and an interrupted build continues after
its last finished step (the tables, the derived values, the search indexes); once past the tables it doesn't need
the archives.

With updates off it serves the newest store in the data directory. `musicbrainz-mirror import` builds one (into a
new directory) from archives (or directories they were unpacked into) without serving it; run again with the same
`--out` and `--dump`, it continues an interrupted import:

```sh
musicbrainz-mirror import --out /srv/musicbrainz/musicbrainz-local \
  mbdump.tar.bz2 mbdump-derived.tar.bz2 mbdump-cover-art-archive.tar.bz2
```

## How it works

A build streams the archives (one decompressing thread per archive) into a directory of immutable files, each
memory-mapped when served ([src/store](src/store)):

- per table, the columns the web service needs as fixed-width rows found by id, with their strings in a pool
  (identical short strings stored once);
- the indexes browsing and lookups go through: lists of rows by a key (compressed), MBIDs (row ids sorted by MBID),
  strings (hashes);
- what musicbrainz-server keeps in materialized tables: first release dates, the country of an area, the areas in
  an area, the genre of a tag, the latest annotation of an entity;
- a tantivy index per searchable entity type.

Memory stays bounded while building: rows that arrive out of order go through temporary files, sorted in buckets.
Nothing is loaded when serving: answers read the mapped files, so the operating system's page cache holds what is
used. A store's files never change once it is built: a new dump becomes a new store beside the served one,
and the old one is deleted once no request uses it. Don't write into a store that is being served (copying files
over it, restoring a backup into it): put a store in place as a new directory, as updates do.

The JSON follows musicbrainz-server's serializer, including which fields appear where (an artist at the top level
has a life span and its areas, the same artist in an artist credit doesn't), the order of relationships, and the
order browse pages are cut in. A test checks that every search field of every entity type finds what it should.

## Development

[mise](https://mise.jdx.dev) installs the tools; `mise run check` runs everything CI does, `mise run build` builds
the binary, `mise run docker` the image. `mise run import <archives>` builds `dev-data/musicbrainz-local`,
`mise run dev` serves it on port 5000. MusicBrainz's
[sample dump](https://data.metabrainz.org/pub/musicbrainz/data/sample/) (350 MB, unpack it first) builds in under a
minute.

The compatibility test's fixture and expected answers are made by [examples/fixture.rs](examples/fixture.rs) (see
its header) from a store built from a full dump.

## License

This project's code is MIT or Apache-2.0, at your option. MusicBrainz's data is MetaBrainz's: the core data is in the
public domain (CC0), the rest under CC BY-NC-SA 3.0; see [MusicBrainz's license](https://musicbrainz.org/doc/About/Data_License).
The test fixture in `tests/fixtures` and the answers in `tests/compat/expected` are MusicBrainz data.
