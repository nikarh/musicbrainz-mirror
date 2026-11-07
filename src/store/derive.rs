//! Values computed from the tables once they are written (what musicbrainz-server keeps in materialized tables):
//! passes over the rows, with plain arrays in memory.
//!
//! - `release.first_date`, `recording.first_date`: the earliest release event (missing parts sort last), not
//!   counting cancelled releases, packed as `year * 10000 + month * 100 + day` with 9999 / 99 / 99 for missing parts.
//! - `area.country`: the area whose ISO 3166-1 code is an area's country (itself, else the nearest country
//!   containing it); `area.descendants`: the areas in an area ("part of", transitively).
//! - `tag.genre`: the genre a tag is (by name).
//! - `<entity>.annotation`: the latest annotation of each entity.
//! - `cover_art.types`: the art types of each image, as bits of art type ids.

use std::collections::HashMap;
use std::io::{self, BufWriter, Write as _};
use std::path::Path;

use super::Store;
use super::file::{Csr, CsrWriter, Mapped, NONE, u32_at};
use crate::kind::Kind;

/// The link type of "part of" between areas.
const PART_OF: &str = "de7cc874-8b1b-3a05-8272-f3834c968fb7";
/// Areas are contained at most this deep.
const MAX_DEPTH: usize = 10;

/// The derived files, opened.
#[derive(Default)]
pub struct Derived {
    release_first: Option<Mapped>,
    recording_first: Option<Mapped>,
    area_country: Option<Mapped>,
    area_descendants: Option<Csr>,
    tag_genre: Option<Mapped>,
    cover_art_types: Option<Mapped>,
    annotations: HashMap<Kind, Csr>,
}

fn at(file: Option<&Mapped>, index: u32) -> Option<u32> {
    let v = u32_at(file?.bytes(), index as usize * 4);
    (v != NONE).then_some(v)
}

impl Derived {
    pub fn open(dir: &Path) -> io::Result<Self> {
        let m = |name: &str| Mapped::open(&dir.join(name)).map(Some);
        let mut annotations = HashMap::new();
        for kind in Kind::ALL.into_iter().filter(|k| k.has_annotations()) {
            annotations.insert(
                kind,
                Csr::open(&dir.join(format!("{}.annotation", kind.table())))?,
            );
        }
        Ok(Self {
            release_first: m("release.first_date")?,
            recording_first: m("recording.first_date")?,
            area_country: m("area.country")?,
            area_descendants: Some(Csr::open(&dir.join("area.descendants"))?),
            tag_genre: m("tag.genre")?,
            cover_art_types: m("cover_art.types")?,
            annotations,
        })
    }

    /// A release's first release date (packed).
    #[must_use]
    pub fn release_first_date(&self, release: u32) -> Option<u32> {
        at(self.release_first.as_ref(), release)
    }

    /// A recording's first release date (packed).
    #[must_use]
    pub fn recording_first_date(&self, recording: u32) -> Option<u32> {
        at(self.recording_first.as_ref(), recording)
    }

    /// The area whose ISO 3166-1 code is an area's country.
    #[must_use]
    pub fn area_country(&self, area: u32) -> Option<u32> {
        at(self.area_country.as_ref(), area)
    }

    /// The areas in an area (not including it).
    pub fn area_descendants(&self, area: u32) -> impl Iterator<Item = u32> + '_ {
        self.area_descendants.iter().flat_map(move |c| c.get(area))
    }

    #[must_use]
    pub fn tag_genre(&self, tag: u32) -> Option<u32> {
        at(self.tag_genre.as_ref(), tag)
    }

    /// The art type ids of the image at a position of `cover_art`, as bits.
    #[must_use]
    pub fn cover_art_types(&self, position: u32) -> u32 {
        self.cover_art_types
            .as_ref()
            .map_or(0, |m| u32_at(m.bytes(), position as usize * 4))
    }

    /// The latest annotation of an entity (an `annotation` row id).
    #[must_use]
    pub fn annotation(&self, kind: Kind, entity: u32) -> Option<u32> {
        self.annotations.get(&kind)?.get(entity).next()
    }
}

/// Packs a date: missing parts as 9999 / 99 / 99, so the earliest sorts first with missing parts last.
#[must_use]
pub fn pack(date: (Option<i64>, Option<i64>, Option<i64>)) -> u32 {
    let y = date.0.filter(|y| (0..9999).contains(y)).unwrap_or(9999);
    let m = date.1.unwrap_or(99);
    let d = date.2.unwrap_or(99);
    u32::try_from(y * 10000 + m * 100 + d).unwrap_or(NONE)
}

fn write_array(path: &Path, values: &[u32]) -> io::Result<()> {
    let mut out = BufWriter::with_capacity(1 << 20, std::fs::File::create(path)?);
    for v in values {
        out.write_all(&v.to_le_bytes())?;
    }
    out.into_inner().map_err(io::IntoInnerError::into_error)?;
    Ok(())
}

fn size(store: &Store, table: &str) -> usize {
    store
        .t(table)
        .iter()
        .last()
        .map_or(0, |r| r.id as usize + 1)
}

/// Writes every derived file into the store's directory.
pub fn build(store: &Store) -> io::Result<()> {
    let dir = &store.dir;
    first_release_dates(store, dir)?;
    areas(store, dir)?;

    // Tags that are genres (by name).
    let mut genres = vec![NONE; size(store, "tag")];
    for tag in store.t("tag").iter() {
        if let Some(name) = tag.str("name")
            && let Some(genre) = store.t("genre").find("name", name).first()
            && let Some(slot) = genres.get_mut(tag.id as usize)
        {
            *slot = genre.id;
        }
    }
    write_array(&dir.join("tag.genre"), &genres)?;

    // The latest annotation of each entity: by creation time, then id.
    for kind in Kind::ALL.into_iter().filter(|k| k.has_annotations()) {
        let table = format!("{}_annotation", kind.table());
        let mut latest: HashMap<u32, (String, u32)> = HashMap::new();
        for link in store.t(&table).iter() {
            let (Some(entity), Some(annotation)) = (link.u32("entity"), link.u32("annotation"))
            else {
                continue;
            };
            let created = store
                .t("annotation")
                .get(annotation)
                .map(|a| a.text("created"))
                .unwrap_or_default();
            let key = (created, annotation);
            if latest.get(&entity).is_none_or(|old| *old < key) {
                latest.insert(entity, key);
            }
        }
        let base = dir.join(format!("{}.annotation", kind.table()));
        let mut csr = CsrWriter::create(&base)?;
        for (entity, (_, annotation)) in latest {
            csr.add(entity, 0, annotation)?;
        }
        csr.write(&base)?;
    }

    // Cover art types, as bits of type ids (there are about 20 types).
    let cover_art = store.t("cover_art");
    let mut position_of: HashMap<u64, u32> = HashMap::new();
    for row in cover_art.iter() {
        if let Some(id) = row.u64("id") {
            position_of.insert(id, row.id);
        }
    }
    let mut types = vec![0u32; position_of.len()];
    for row in store.t("cover_art_type").iter() {
        if let (Some(id), Some(type_id)) = (row.u64("id"), row.u32("type_id"))
            && let Some(slot) = position_of
                .get(&id)
                .and_then(|p| types.get_mut(*p as usize))
            && type_id < 32
        {
            *slot |= 1 << type_id;
        }
    }
    write_array(&dir.join("cover_art.types"), &types)
}

fn first_release_dates(store: &Store, dir: &Path) -> io::Result<()> {
    let cancelled = store.id_by_name("release_status", "Cancelled");
    let mut release_first = vec![NONE; size(store, "release")];
    let mut earlier = |release: u32, packed: u32| {
        if let Some(slot) = release_first.get_mut(release as usize)
            && (*slot == NONE || packed < *slot)
        {
            *slot = packed;
        }
    };
    for event in store.t("release_country").iter() {
        let date = event.date("date");
        // An event without any date doesn't count (but an unknown country's does).
        if date == (None, None, None) {
            continue;
        }
        if let Some(release) = event.u32("release") {
            earlier(release, pack(date));
        }
    }
    for event in store.t("release_unknown_country").iter() {
        if let Some(release) = event.u32("release") {
            earlier(release, pack(event.date("date")));
        }
    }
    if let Some(cancelled) = cancelled {
        for (id, slot) in release_first.iter_mut().enumerate() {
            if *slot != NONE
                && store
                    .t("release")
                    .get(u32::try_from(id).unwrap_or(NONE))
                    .and_then(|r| r.u32("status"))
                    == Some(cancelled)
            {
                *slot = NONE;
            }
        }
    }
    write_array(&dir.join("release.first_date"), &release_first)?;

    let medium_release: Vec<u32> = {
        let mut v = vec![NONE; size(store, "medium")];
        for m in store.t("medium").iter() {
            if let Some(slot) = v.get_mut(m.id as usize) {
                *slot = m.u32("release").unwrap_or(NONE);
            }
        }
        v
    };
    let mut recording_first = vec![NONE; size(store, "recording")];
    for track in store.t("track").iter() {
        let (Some(recording), Some(medium)) = (track.u32("recording"), track.u32("medium")) else {
            continue;
        };
        let release = medium_release.get(medium as usize).copied().unwrap_or(NONE);
        let Some(&date) = release_first.get(release as usize) else {
            continue;
        };
        if date == NONE {
            continue;
        }
        if let Some(slot) = recording_first.get_mut(recording as usize)
            && (*slot == NONE || date < *slot)
        {
            *slot = date;
        }
    }
    write_array(&dir.join("recording.first_date"), &recording_first)
}

fn areas(store: &Store, dir: &Path) -> io::Result<()> {
    let part_of = store
        .t("link_type")
        .iter()
        .find(|t| t.gid() == PART_OF)
        .map(|t| t.id);
    // child -> parents
    let mut parents: HashMap<u32, Vec<u32>> = HashMap::new();
    for rel in store.t("l_area_area").iter() {
        let link = rel.u32("link").and_then(|l| store.t("link").get(l));
        if link.and_then(|l| l.u32("link_type")) == part_of
            && let (Some(parent), Some(child)) = (rel.u32("entity0"), rel.u32("entity1"))
        {
            parents.entry(child).or_default().push(parent);
        }
    }
    let areas = size(store, "area");
    let country_type = 1;
    let has_code = |area: u32| store.t("iso_3166_1").by("area", area).next().is_some();
    let mut country = vec![NONE; areas];
    let base = dir.join("area.descendants");
    let mut descendants = CsrWriter::create(&base)?;
    for area in store.t("area").iter() {
        // ancestors by depth
        let mut seen = vec![area.id];
        let mut frontier = vec![area.id];
        let mut nearest: Option<(usize, String, u32)> =
            has_code(area.id).then(|| (0, code(store, area.id), area.id));
        for depth in 1..=MAX_DEPTH {
            let mut next = Vec::new();
            for a in frontier {
                for &p in parents.get(&a).into_iter().flatten() {
                    if seen.contains(&p) {
                        continue;
                    }
                    seen.push(p);
                    next.push(p);
                    descendants.add(p, u32::try_from(depth).unwrap_or(0), area.id)?;
                    let is_country =
                        store.t("area").get(p).and_then(|r| r.u32("type")) == Some(country_type);
                    if is_country && has_code(p) {
                        let candidate = (depth, code(store, p), p);
                        if nearest
                            .as_ref()
                            .is_none_or(|n| (n.0, &n.1) > (candidate.0, &candidate.1))
                        {
                            nearest = Some(candidate);
                        }
                    }
                }
            }
            if next.is_empty() {
                break;
            }
            frontier = next;
        }
        if let (Some(slot), Some((_, _, c))) = (country.get_mut(area.id as usize), nearest) {
            *slot = c;
        }
    }
    descendants.write(&base)?;
    write_array(&dir.join("area.country"), &country)
}

/// An area's first ISO 3166-1 code.
#[must_use]
pub fn code(store: &Store, area: u32) -> String {
    let mut codes: Vec<&str> = store
        .t("iso_3166_1")
        .by("area", area)
        .filter_map(|r| r.str("code"))
        .collect();
    codes.sort_unstable();
    codes.first().map(|c| (*c).to_owned()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packs_dates() {
        assert_eq!(pack((Some(1973), Some(3), Some(24))), 1973_0324);
        assert_eq!(pack((Some(1973), None, None)), 1973_9999);
        assert_eq!(pack((None, None, None)), 9999_9999);
        assert!(pack((Some(1973), Some(3), None)) < pack((Some(1973), None, None)));
    }
}
