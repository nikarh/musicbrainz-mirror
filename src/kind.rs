//! The MusicBrainz entity types that have MBIDs.

/// An entity type. The database stores it as [`Kind::code`] wherever a table holds rows of several types (aliases,
/// tags, ratings, annotations, relationships, MBID redirects).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    Area,
    Artist,
    Event,
    Genre,
    Instrument,
    Label,
    Place,
    Recording,
    Release,
    ReleaseGroup,
    Series,
    Url,
    Work,
}

impl Kind {
    /// In the order MusicBrainz names its relationship tables: `l_<a>_<b>` with `a <= b`.
    pub const ALL: [Self; 13] = [
        Self::Area,
        Self::Artist,
        Self::Event,
        Self::Genre,
        Self::Instrument,
        Self::Label,
        Self::Place,
        Self::Recording,
        Self::Release,
        Self::ReleaseGroup,
        Self::Series,
        Self::Url,
        Self::Work,
    ];

    #[must_use]
    pub fn code(self) -> i64 {
        match self {
            Self::Area => 1,
            Self::Artist => 2,
            Self::Event => 3,
            Self::Genre => 4,
            Self::Instrument => 5,
            Self::Label => 6,
            Self::Place => 7,
            Self::Recording => 8,
            Self::Release => 9,
            Self::ReleaseGroup => 10,
            Self::Series => 11,
            Self::Url => 12,
            Self::Work => 13,
        }
    }

    #[must_use]
    pub fn from_code(code: i64) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.code() == code)
    }

    /// The name of its table (and the prefix of its tables) in the MusicBrainz schema.
    #[must_use]
    pub fn table(self) -> &'static str {
        match self {
            Self::Area => "area",
            Self::Artist => "artist",
            Self::Event => "event",
            Self::Genre => "genre",
            Self::Instrument => "instrument",
            Self::Label => "label",
            Self::Place => "place",
            Self::Recording => "recording",
            Self::Release => "release",
            Self::ReleaseGroup => "release_group",
            Self::Series => "series",
            Self::Url => "url",
            Self::Work => "work",
        }
    }

    /// The name in the web service: its path, `target-type`, `<name>-rels`.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::ReleaseGroup => "release-group",
            other => other.table(),
        }
    }

    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.name() == name)
    }

    /// The name of a list of these in the web service (`artists`, `release-groups`, `series`).
    #[must_use]
    pub fn plural(self) -> &'static str {
        match self {
            Self::Area => "areas",
            Self::Artist => "artists",
            Self::Event => "events",
            Self::Genre => "genres",
            Self::Instrument => "instruments",
            Self::Label => "labels",
            Self::Place => "places",
            Self::Recording => "recordings",
            Self::Release => "releases",
            Self::ReleaseGroup => "release-groups",
            Self::Series => "series",
            Self::Url => "urls",
            Self::Work => "works",
        }
    }

    /// Whether it has aliases (`<table>_alias`).
    #[must_use]
    pub fn has_aliases(self) -> bool {
        !matches!(self, Self::Url)
    }

    /// Whether it has tags (`<table>_tag`).
    #[must_use]
    pub fn has_tags(self) -> bool {
        !matches!(self, Self::Url | Self::Genre)
    }

    /// Whether it has ratings (`<table>_meta`).
    #[must_use]
    pub fn has_ratings(self) -> bool {
        matches!(
            self,
            Self::Artist
                | Self::Event
                | Self::Label
                | Self::Place
                | Self::Recording
                | Self::ReleaseGroup
                | Self::Work
        )
    }

    /// Whether it has annotations (`<table>_annotation`).
    #[must_use]
    pub fn has_annotations(self) -> bool {
        !matches!(self, Self::Url)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_and_names_round_trip() {
        for kind in Kind::ALL {
            assert_eq!(Kind::from_code(kind.code()), Some(kind));
            assert_eq!(Kind::from_name(kind.name()), Some(kind));
        }
        assert!(Kind::ALL.windows(2).all(|w| w[0].table() < w[1].table()));
    }
}
