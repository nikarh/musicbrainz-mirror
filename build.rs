//! Sets `MUSICBRAINZ_MIRROR_VERSION`: the package version plus `MUSICBRAINZ_MIRROR_VERSION_SUFFIX` (e.g.
//! `-dev+abc1234` for builds of main), so a build says what it is in `--version`.

fn main() {
    println!("cargo::rerun-if-env-changed=MUSICBRAINZ_MIRROR_VERSION_SUFFIX");
    let suffix = std::env::var("MUSICBRAINZ_MIRROR_VERSION_SUFFIX").unwrap_or_default();
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    println!("cargo::rustc-env=MUSICBRAINZ_MIRROR_VERSION={version}{suffix}");
}
