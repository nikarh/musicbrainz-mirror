# Security

musicbrainz-mirror serves public data read-only and has no authentication of its own. Security issues are ways to
make it do more than that: write or delete anything beyond its data directory, change the data it serves other than
by importing a dump, make it fetch something other than the configured dump location, run code or SQL through a
crafted request or dump, or break out of its restrictions in the container.

Please report them privately, through
[GitHub's vulnerability reporting](https://github.com/nikarh/musicbrainz-mirror/security/advisories/new), not in a
public issue.
