//! The `inc` parameter: which related data a request asks for, validated as musicbrainz-server validates it
//! (`MusicBrainz::Server::WebService::Validator`), with its error messages.

use std::collections::BTreeSet;

use crate::kind::Kind;

/// What a request asks for, besides the entities themselves.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct Inc {
    pub aliases: bool,
    pub annotation: bool,
    pub artist_credits: bool,
    pub discids: bool,
    pub isrcs: bool,
    pub media: bool,
    pub labels: bool,
    pub recordings: bool,
    pub releases: bool,
    pub release_groups: bool,
    pub works: bool,
    pub tags: bool,
    pub genres: bool,
    pub ratings: bool,
    pub user_tags: bool,
    pub user_genres: bool,
    pub user_ratings: bool,
    pub collections: bool,
    pub user_collections: bool,
    pub recording_level_rels: bool,
    pub release_group_level_rels: bool,
    pub work_level_rels: bool,
    pub various_artists: bool,
    /// Target types of the relationships asked for (`<type>-rels`), in MusicBrainz's order.
    pub rels: BTreeSet<Kind>,
}

/// The kind of request, which decides the allowed `inc` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Lookup,
    Browse,
}

impl Inc {
    #[must_use]
    pub fn has_rels(&self) -> bool {
        !self.rels.is_empty()
    }

    /// Whether tags or genres (or their user versions) are asked for.
    #[must_use]
    pub fn tags_or_genres(&self) -> bool {
        self.tags || self.genres || self.user_tags || self.user_genres
    }

    fn set(&mut self, name: &str) -> bool {
        let flag = match name {
            "aliases" => &mut self.aliases,
            "annotation" => &mut self.annotation,
            "artist-credits" | "artists" => &mut self.artist_credits,
            "discids" => &mut self.discids,
            "isrcs" => &mut self.isrcs,
            "media" => &mut self.media,
            "labels" => &mut self.labels,
            "recordings" => &mut self.recordings,
            "releases" => &mut self.releases,
            "release-groups" => &mut self.release_groups,
            "works" => &mut self.works,
            "tags" => &mut self.tags,
            "genres" => &mut self.genres,
            "ratings" => &mut self.ratings,
            "user-tags" => &mut self.user_tags,
            "user-genres" => &mut self.user_genres,
            "user-ratings" => &mut self.user_ratings,
            "collections" => &mut self.collections,
            "user-collections" => &mut self.user_collections,
            "recording-level-rels" => &mut self.recording_level_rels,
            "release-group-level-rels" => &mut self.release_group_level_rels,
            "work-level-rels" => &mut self.work_level_rels,
            "various-artists" => &mut self.various_artists,
            // Accepted by musicbrainz-server and ignored (PUIDs are long gone; statuses and types are parameters).
            "puids" | "status" | "type" | "release-group-status" => return true,
            _ => return false,
        };
        *flag = true;
        true
    }
}

/// The `inc` values allowed for a resource and mode (besides `<type>-rels` where `_relations` is listed). `None`:
/// the resource has no such mode.
#[allow(clippy::match_same_arms, clippy::too_many_lines)] // one list per resource, as musicbrainz-server has them
fn allowed(resource: &str, mode: Mode) -> Option<&'static [&'static str]> {
    Some(match (resource, mode) {
        ("area" | "place" | "work" | "event", _) => &[
            "aliases",
            "annotation",
            "_relations",
            "tags",
            "user-tags",
            "genres",
            "user-genres",
            "ratings",
            "user-ratings",
        ],
        ("artist", Mode::Lookup) => &[
            "recordings",
            "releases",
            "release-groups",
            "works",
            "aliases",
            "annotation",
            "_relations",
            "tags",
            "user-tags",
            "genres",
            "user-genres",
            "ratings",
            "user-ratings",
        ],
        ("artist", Mode::Browse) => &[
            "aliases",
            "annotation",
            "_relations",
            "tags",
            "user-tags",
            "genres",
            "user-genres",
            "ratings",
            "user-ratings",
        ],
        ("instrument" | "series", _) => &[
            "aliases",
            "annotation",
            "_relations",
            "tags",
            "user-tags",
            "genres",
            "user-genres",
        ],
        ("label", Mode::Lookup) => &[
            "releases",
            "aliases",
            "annotation",
            "_relations",
            "tags",
            "user-tags",
            "genres",
            "user-genres",
            "ratings",
            "user-ratings",
        ],
        ("label", Mode::Browse) => &[
            "aliases",
            "annotation",
            "_relations",
            "tags",
            "user-tags",
            "genres",
            "user-genres",
            "ratings",
            "user-ratings",
        ],
        ("recording", Mode::Lookup) => &[
            "artists",
            "releases",
            "artist-credits",
            "puids",
            "isrcs",
            "aliases",
            "_relations",
            "tags",
            "user-tags",
            "genres",
            "user-genres",
            "ratings",
            "user-ratings",
            "release-groups",
            "work-level-rels",
            "annotation",
        ],
        ("recording", Mode::Browse) => &[
            "aliases",
            "artist-credits",
            "puids",
            "isrcs",
            "annotation",
            "_relations",
            "tags",
            "user-tags",
            "genres",
            "user-genres",
            "ratings",
            "user-ratings",
            "work-level-rels",
        ],
        ("release", Mode::Lookup) => &[
            "artists",
            "labels",
            "recordings",
            "release-groups",
            "aliases",
            "tags",
            "user-tags",
            "genres",
            "user-genres",
            "ratings",
            "user-ratings",
            "collections",
            "user-collections",
            "artist-credits",
            "discids",
            "media",
            "recording-level-rels",
            "release-group-level-rels",
            "work-level-rels",
            "_relations",
            "annotation",
        ],
        ("release", Mode::Browse) => &[
            "aliases",
            "artist-credits",
            "labels",
            "recordings",
            "discids",
            "tags",
            "user-tags",
            "genres",
            "user-genres",
            "ratings",
            "user-ratings",
            "release-groups",
            "media",
            "recording-level-rels",
            "release-group-level-rels",
            "work-level-rels",
            "_relations",
            "annotation",
        ],
        ("release-group", Mode::Lookup) => &[
            "artists",
            "releases",
            "artist-credits",
            "aliases",
            "annotation",
            "_relations",
            "tags",
            "user-tags",
            "genres",
            "user-genres",
            "ratings",
            "user-ratings",
        ],
        ("release-group", Mode::Browse) => &[
            "aliases",
            "artist-credits",
            "annotation",
            "_relations",
            "release-group-status",
            "tags",
            "user-tags",
            "genres",
            "user-genres",
            "ratings",
            "user-ratings",
        ],
        ("url", _) => &["_relations"],
        ("genre", _) => &[],
        ("discid", Mode::Lookup) => &[
            "artists",
            "labels",
            "recordings",
            "release-groups",
            "artist-credits",
            "tags",
            "user-tags",
            "genres",
            "user-genres",
            "aliases",
            "puids",
            "isrcs",
            "_relations",
            "cdstubs",
        ],
        ("isrc", Mode::Lookup) => &[
            "artists",
            "releases",
            "puids",
            "isrcs",
            "artist-credits",
            "aliases",
            "_relations",
            "tags",
            "user-tags",
            "ratings",
            "user-ratings",
        ],
        ("iswc", Mode::Lookup) => &[
            "artists",
            "aliases",
            "artist-credits",
            "_relations",
            "tags",
            "user-tags",
            "ratings",
            "user-ratings",
        ],
        _ => return None,
    })
}

/// `inc` values only allowed with another one: (the other one, resource or "" for any, values).
const EXTRA: &[(&str, &str, &[&str])] = &[
    ("recordings", "", &["artist-credits", "puids", "isrcs"]),
    ("recording-rels", "", &["artist-credits"]),
    ("releases", "artist", &["various-artists"]),
    (
        "releases",
        "",
        &["artist-credits", "discids", "media", "status", "type"],
    ),
    ("release-rels", "", &["artist-credits"]),
    ("release-groups", "", &["artist-credits", "type"]),
    ("release-group-rels", "", &["artist-credits"]),
    ("works", "", &["artist-credits"]),
];

/// Parses and validates `inc` for a request; the error is the message musicbrainz-server answers with.
pub fn parse(resource: &str, mode: Mode, inc: Option<&str>) -> Result<Inc, String> {
    let allowed = allowed(resource, mode).unwrap_or(&[]);
    let relations = allowed.contains(&"_relations");
    let values: Vec<String> = inc
        .unwrap_or("")
        .split(['+', ' '])
        .filter(|v| !v.is_empty())
        .map(|v| v.replace("mediums", "media"))
        .collect();

    let extra: BTreeSet<&str> = values
        .iter()
        .flat_map(|v| {
            EXTRA
                .iter()
                .filter(move |(with, r, _)| with == v && (r.is_empty() || *r == resource))
                .flat_map(|(_, _, extra)| extra.iter().copied())
        })
        .collect();

    let mut out = Inc::default();
    for value in &values {
        if relations && let Some(kind) = value.strip_suffix("-rels").and_then(Kind::from_name) {
            out.rels.insert(kind);
            continue;
        }
        if !allowed.contains(&value.as_str()) && !extra.contains(value.as_str()) {
            let possible: BTreeSet<&str> = EXTRA
                .iter()
                .filter(|(_, r, extra)| {
                    (r.is_empty() || *r == resource) && extra.contains(&value.as_str())
                })
                .map(|(with, _, _)| *with)
                .collect();
            return Err(if possible.is_empty() {
                format!("{value} is not a valid inc parameter for the {resource} resource.")
            } else {
                format!(
                    "{value} is not a valid option for the inc parameter for the {resource} resource unless you \
                     specify one of the following other inc parameters: {}",
                    possible.into_iter().collect::<Vec<_>>().join(", ")
                )
            });
        }
        if !out.set(value) {
            return Err(format!(
                "{value} is not a valid inc parameter for the {resource} resource."
            ));
        }
    }
    if out.discids {
        out.media = true;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_validates() {
        let inc = parse(
            "release",
            Mode::Lookup,
            Some("artists+recordings artist-rels+mediums"),
        )
        .unwrap();
        assert!(inc.artist_credits && inc.recordings && inc.media);
        assert_eq!(inc.rels, BTreeSet::from([Kind::Artist]));

        let inc = parse("release", Mode::Lookup, Some("discids")).unwrap();
        assert!(inc.media);

        assert_eq!(
            parse("artist", Mode::Lookup, Some("recordings+isrcs")).map(|i| i.isrcs),
            Ok(true)
        );
        assert_eq!(
            parse("artist", Mode::Lookup, Some("isrcs")),
            Err("isrcs is not a valid option for the inc parameter for the artist resource unless you specify one \
                 of the following other inc parameters: recordings"
                .to_owned())
        );
        assert_eq!(
            parse("artist", Mode::Browse, Some("recordings")),
            Err("recordings is not a valid inc parameter for the artist resource.".to_owned())
        );
        assert!(parse("artist", Mode::Lookup, Some("releases+various-artists")).is_ok());
        assert!(parse("label", Mode::Lookup, Some("releases+various-artists")).is_err());
        assert!(parse("url", Mode::Lookup, Some("artist-rels")).is_ok());
        assert!(parse("url", Mode::Lookup, Some("aliases")).is_err());
        assert!(parse("genre", Mode::Lookup, None).is_ok());
    }
}
