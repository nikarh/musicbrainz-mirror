When adding dependencies don't edit manifests by hand - use the package managers (`cargo add`) so the latest versions
are used.
Commit your changes. Use conventional commits. Split logical changes into individual commits.
Before committing run `mise run check` (formatting, clippy, tests, cargo-deny) - and check its exit status: piping
its output hides failures.

Commit messages, code, tests and docs describe bugs and their fixes in general terms: what goes wrong, under which
conditions, and what changed. Never frame them around a particular deployment, and don't carry over its details.

## Layout

- `src/import/` - the importer: `spec.rs` (which dump files, which columns, where to), `copy.rs` (PostgreSQL COPY
  rows), `schema.sql`, `indexes.sql`, `derive.sql` (materialized tables, search tables).
- `src/db/` - an imported database opened read-only, `queries.rs` (every fixed statement), `plan.rs` (the query
  plan checks).
- `src/ws/` - the web service: `entity.rs` (the JSON of every entity type), `browse.rs`, `search/`, `xml.rs`,
  `caa.rs`, `inc.rs` (inc validation).
- `src/mirror.rs`, `src/dump.rs` - which database is served, downloads, the switch to a new dump.
- `tests/compat.rs` with `tests/compat/` and `tests/fixtures/dump` - answers compared with musicbrainz.org's;
  `examples/fixture.rs` makes the fixture and records the answers.

## Rules

- Answers follow musicbrainz-server (its JSON and XML serializers and the controllers that load what they show).
  A change to what an answer contains needs a request in `tests/compat/requests.txt` that shows it, with the
  fixture and the expected answers remade.
- Every SQL statement is answered from an index: fixed statements go in `src/db/queries.rs` (and `ALL`), statements
  built at runtime get a case in the plan tests.
- The API stays read-only.
- A change to the database layout bumps `DB_VERSION` (older databases are then rebuilt from the next dump).
- A change to the configuration updates `README.md` and the `--help` text in `src/config.rs` in the same commit.
- The container image must keep working read-only, as any uid, with no capabilities: no writes outside the data
  directory, no listening on ports below 1024, nothing needing a shell or files from a distribution.
