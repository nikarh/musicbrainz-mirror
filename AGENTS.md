When adding dependencies don't edit manifests by hand - use the package managers (`cargo add`) so the latest versions
are used.
Commit your changes. Use conventional commits. Split logical changes into individual commits.
Before committing run `mise run check` (formatting, clippy, tests, cargo-deny) - and check its exit status: piping
its output hides failures.

Commit messages, code, tests and docs describe bugs and their fixes in general terms: what goes wrong, under which
conditions, and what changed. Never frame them around a particular deployment, and don't carry over its details.

## Layout

- `src/store/` - the store: `schema.rs` (which dump files, which columns, which indexes), `build.rs` (archives to
  files), `file.rs` (the file formats, readers and bounded-memory writers), `derive.rs` (materialized values),
  `search.rs` (the tantivy indexes and queries), `copy.rs` (PostgreSQL COPY rows), `mod.rs` (an open store).
- `src/ws/` - the web service: `entity.rs` (the JSON of every entity type), `browse.rs`, `search/`, `xml.rs`,
  `caa.rs`, `inc.rs` (inc validation).
- `src/mirror.rs`, `src/dump.rs` - which store is served, downloads, the switch to a new dump.
- `tests/compat.rs` with `tests/compat/` and `tests/fixtures/dump` - answers compared with musicbrainz.org's;
  `examples/fixture.rs` makes the fixture and records the answers.
- `examples/upstream_schema.rs` - compares `schema.rs` with musicbrainz-server's schema (a weekly workflow opens an
  issue when they differ).

## Rules

- Answers follow musicbrainz-server (its JSON and XML serializers and the controllers that load what they show).
  A change to what an answer contains needs a request in `tests/compat/requests.txt` that shows it, with the
  fixture and the expected answers remade.
- Answers go through indexes (`Table::by`, `find`, `by_gid`), never through scans of large tables. Every search field
  must be indexed: `every_search_field_is_indexed` checks each against the fixture.
- The API stays read-only.
- A change to the store's layout bumps `FORMAT_VERSION` in `src/store/mod.rs` (older stores are then rebuilt from
  the next dump).
- A change to the configuration updates `README.md` and the `--help` text in `src/config.rs` in the same commit.
- The container image must keep working read-only, as any uid, with no capabilities: no writes outside the data
  directory, no listening on ports below 1024, nothing needing a shell or files from a distribution.
