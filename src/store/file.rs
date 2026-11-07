//! The files of a store, and their writers. Everything is little endian.
//!
//! - **Rows** (`<table>.rows`): fixed-width records. Entity tables are indexed by MusicBrainz's own id (row `n` is
//!   the entity with id `n`; ids that don't exist are zeroes), the others by their position in the dump.
//! - **Pools** (`<table>.strings`): strings, each a varint length and UTF-8 bytes. A row holds a string's `u32`
//!   offset; offset 0 is NULL, offset 1 the empty string. Identical strings can share one copy.
//! - **CSR** (`<table>.<column>.keys`, `.offsets`, `.values`): the rows of a table by the value of one of its
//!   columns (an artist's aliases, a medium's tracks). Dense (an offset for every possible key) when most keys have
//!   rows, sparse (sorted keys with offsets, found by binary search) otherwise.
//! - **MBID index** (`<table>.gids`): the ids of a table's rows sorted by their MBID (which is in the rows).

use std::collections::HashMap;
use std::fs::File;
use std::hash::{BuildHasher as _, Hasher as _};
use std::io::{self, BufWriter, Write as _};
use std::path::{Path, PathBuf};

/// The NULL string's offset, and the empty string's.
pub const NULL: u32 = 0;
pub const EMPTY: u32 = 1;
/// A missing integer.
pub const NONE: u32 = u32::MAX;

/// A file mapped read-only (or nothing, for an empty or missing file).
pub struct Mapped(Option<memmap2::Mmap>);

impl Mapped {
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = match File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Self(None)),
            Err(e) => return Err(e),
        };
        if file.metadata()?.len() == 0 {
            return Ok(Self(None));
        }
        Ok(Self(Some(map(&file)?)))
    }

    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        self.0.as_deref().unwrap_or_default()
    }
}

/// Maps a store file. Store files are written once, by the importer, into a new directory (a build refuses an
/// existing one) that is only opened once it is complete, and are never written to again: an update builds a new
/// store beside the served one, and the replaced store is unlinked, which leaves open mappings intact. Files
/// rewritten during a build are written under new names and renamed over the old ones. So a mapping never sees its
/// file change, unless something outside this program writes into a store.
#[allow(unsafe_code)]
fn map(file: &File) -> io::Result<memmap2::Mmap> {
    // SAFETY: the file is never modified or truncated while mapped (see above).
    unsafe { memmap2::Mmap::map(file) }
}

pub fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    bytes
        .get(offset..offset + 4)
        .and_then(|b| b.try_into().ok())
        .map_or(NONE, u32::from_le_bytes)
}

pub fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    bytes
        .get(offset..offset + 2)
        .and_then(|b| b.try_into().ok())
        .map_or(0, u16::from_le_bytes)
}

pub fn u64_at(bytes: &[u8], offset: usize) -> u64 {
    bytes
        .get(offset..offset + 8)
        .and_then(|b| b.try_into().ok())
        .map_or(0, u64::from_le_bytes)
}

pub fn u128_at(bytes: &[u8], offset: usize) -> u128 {
    bytes
        .get(offset..offset + 16)
        .and_then(|b| b.try_into().ok())
        .map_or(0, u128::from_le_bytes)
}

/// The widest row of any table.
pub const MAX_ROW: usize = 96;
/// Rows per compressed block.
const BLOCK_ROWS: usize = 64;

/// A row's bytes (copied: compressed rows have no bytes to borrow).
#[derive(Clone, Copy)]
pub struct RowBuf {
    bytes: [u8; MAX_ROW],
    len: usize,
}

impl std::ops::Deref for RowBuf {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        self.bytes.get(..self.len).unwrap_or_default()
    }
}

/// Fixed-width rows. With an id list (`<rows>.ids`), they are the rows of those ids, in order (a sparse table).
///
/// Compressed (`<rows>.z`, `<rows>.zi`, `<rows>.gid` instead of `<rows>`): blocks of 64 rows, each a zstd frame of
/// the rows' bytes column by column (the first byte of every row, then the second...: values of a column are alike),
/// without their MBIDs, which are in a column of their own (they don't compress, and MBID lookups read them).
/// `.zi` holds the number of rows, then the offset of every block in `.z` and the end (`u64`s).
pub struct Rows {
    data: Mapped,
    ids: Mapped,
    width: usize,
    compressed: Option<Compressed>,
}

struct Compressed {
    /// Identifies these rows' blocks in the cache (unique for the process: a mapping's address can be reused).
    key: u64,
    index: Mapped,
    gids: Mapped,
    /// Where a row's MBID goes.
    gid_offset: Option<usize>,
    positions: usize,
}

thread_local! {
    /// Blocks decompressed lately: (the rows' key, block, the block's bytes), most recent first.
    static BLOCKS: std::cell::RefCell<Vec<(u64, usize, Vec<u8>)>> = const { std::cell::RefCell::new(Vec::new()) };
}
thread_local! {
    static DECOMPRESSOR: std::cell::RefCell<Option<zstd::bulk::Decompressor<'static>>> =
        const { std::cell::RefCell::new(None) };
}
/// The next key of compressed rows.
static NEXT_KEY: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Blocks kept per thread.
const CACHED_BLOCKS: usize = 32;

impl Rows {
    /// `gid_offset`: where the rows have an MBID (kept apart when compressed).
    pub fn open(path: &Path, width: usize, gid_offset: Option<usize>) -> io::Result<Self> {
        let ids = Mapped::open(&with_ext(path, "ids"))?;
        let index_path = with_ext(path, "zi");
        if index_path.exists() {
            let index = Mapped::open(&index_path)?;
            let positions = usize::try_from(u64_at(index.bytes(), 0)).map_err(io::Error::other)?;
            return Ok(Self {
                data: Mapped::open(&with_ext(path, "z"))?,
                ids,
                width,
                compressed: Some(Compressed {
                    key: NEXT_KEY.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
                    index,
                    gids: Mapped::open(&with_ext(path, "gid"))?,
                    gid_offset,
                    positions,
                }),
            });
        }
        Ok(Self {
            data: Mapped::open(path)?,
            ids,
            width,
            compressed: None,
        })
    }

    /// The number of positions (the highest id plus one, or the number of rows of a sparse table).
    #[must_use]
    pub fn len(&self) -> usize {
        match &self.compressed {
            Some(c) => c.positions,
            None => self.data.bytes().len().checked_div(self.width).unwrap_or(0),
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The id of the row at a position (the position itself unless sparse).
    #[must_use]
    pub fn id_at(&self, position: u32) -> u32 {
        if self.ids.bytes().is_empty() {
            position
        } else {
            u32_at(self.ids.bytes(), position as usize * 4)
        }
    }

    fn position(&self, id: u32) -> Option<usize> {
        if self.ids.bytes().is_empty() {
            return Some(id as usize);
        }
        let ids = self.ids.bytes();
        let (mut lo, mut hi) = (0usize, ids.len() / 4);
        while lo < hi {
            let mid = usize::midpoint(lo, hi);
            if u32_at(ids, mid * 4) < id {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        (u32_at(ids, lo * 4) == id).then_some(lo)
    }

    /// The MBID of the row of an id (without reading the rest of a compressed row).
    #[must_use]
    pub fn gid(&self, id: u32, gid_offset: usize) -> u128 {
        let Some(position) = self.position(id) else {
            return 0;
        };
        match &self.compressed {
            Some(c) => u128_at(c.gids.bytes(), position * 16),
            None => self
                .data
                .bytes()
                .get(position * self.width..)
                .map_or(0, |row| u128_at(row, gid_offset)),
        }
    }

    /// The row of an id (by position for tables that aren't sparse).
    #[must_use]
    pub fn get(&self, id: u32) -> Option<RowBuf> {
        let position = self.position(id)?;
        let mut row = RowBuf {
            bytes: [0; MAX_ROW],
            len: self.width,
        };
        let Some(c) = &self.compressed else {
            let start = position.checked_mul(self.width)?;
            row.bytes
                .get_mut(..self.width)?
                .copy_from_slice(self.data.bytes().get(start..start + self.width)?);
            return Some(row);
        };
        if position >= c.positions {
            return None;
        }
        let packed = self.width - if c.gid_offset.is_some() { 16 } else { 0 };
        let (block, within) = (position / BLOCK_ROWS, position % BLOCK_ROWS);
        let rows_in_block = BLOCK_ROWS.min(c.positions - block * BLOCK_ROWS);
        let key = c.key;
        let mut packed_row = [0u8; MAX_ROW];
        let found = BLOCKS.with(|cell| {
            let mut cache = cell.borrow_mut();
            let hit = cache.iter().position(|(k, b, _)| *k == key && *b == block);
            let index = if let Some(i) = hit {
                i
            } else {
                let start = usize::try_from(u64_at(c.index.bytes(), 8 + block * 8)).ok()?;
                let end = usize::try_from(u64_at(c.index.bytes(), 16 + block * 8)).ok()?;
                let frame = self.data.bytes().get(start..end)?;
                let mut out = if cache.len() >= CACHED_BLOCKS {
                    cache.pop().map(|e| e.2).unwrap_or_default()
                } else {
                    Vec::new()
                };
                out.clear();
                out.reserve(packed * rows_in_block);
                DECOMPRESSOR.with(|d| {
                    let mut d = d.borrow_mut();
                    if d.is_none() {
                        *d = zstd::bulk::Decompressor::new().ok();
                    }
                    d.as_mut()?.decompress_to_buffer(frame, &mut out).ok()
                })?;
                cache.push((key, block, out));
                cache.len() - 1
            };
            // most recent first
            let entry = cache.remove(index);
            cache.insert(0, entry);
            let bytes = &cache.first()?.2;
            for (c, slot) in packed_row.iter_mut().take(packed).enumerate() {
                *slot = *bytes.get(c * rows_in_block + within)?;
            }
            Some(())
        });
        found?;
        // the row: its bytes around its MBID
        let unpacked = row.bytes.get_mut(..self.width)?;
        match c.gid_offset {
            Some(g) => {
                unpacked.get_mut(..g)?.copy_from_slice(packed_row.get(..g)?);
                unpacked
                    .get_mut(g..g + 16)?
                    .copy_from_slice(c.gids.bytes().get(position * 16..position * 16 + 16)?);
                unpacked
                    .get_mut(g + 16..)?
                    .copy_from_slice(packed_row.get(g..packed)?);
            }
            None => unpacked.copy_from_slice(packed_row.get(..packed)?),
        }
        Some(row)
    }
}

/// Compresses a rows file (see [`Rows`]), removing the plain one.
pub fn compress_rows(
    path: &Path,
    width: usize,
    gid_offset: Option<usize>,
    level: i32,
) -> io::Result<()> {
    let plain = Mapped::open(path)?;
    let bytes = plain.bytes();
    let positions = bytes.len().checked_div(width).unwrap_or(0);
    let packed = width - if gid_offset.is_some() { 16 } else { 0 };
    let mut data = BufWriter::with_capacity(1 << 20, File::create(with_ext(path, "z"))?);
    let mut index = BufWriter::with_capacity(1 << 20, File::create(with_ext(path, "zi"))?);
    let mut gids = match gid_offset {
        Some(_) => Some(BufWriter::with_capacity(
            1 << 20,
            File::create(with_ext(path, "gid"))?,
        )),
        None => None,
    };
    index.write_all(&(positions as u64).to_le_bytes())?;
    let mut compressor = zstd::bulk::Compressor::new(level)?;
    let mut columns = Vec::with_capacity(packed * BLOCK_ROWS);
    let mut written = 0u64;
    for block in bytes.chunks(width * BLOCK_ROWS) {
        let rows: Vec<&[u8]> = block.chunks_exact(width).collect();
        columns.clear();
        for c in 0..packed {
            // the column's byte of each row, skipping the MBID
            let source = match gid_offset {
                Some(g) if c >= g => c + 16,
                _ => c,
            };
            columns.extend(rows.iter().map(|r| r.get(source).copied().unwrap_or(0)));
        }
        if let (Some(g), Some(out)) = (gid_offset, &mut gids) {
            for r in &rows {
                out.write_all(r.get(g..g + 16).unwrap_or(&[0; 16]))?;
            }
        }
        let frame = compressor.compress(&columns)?;
        index.write_all(&written.to_le_bytes())?;
        data.write_all(&frame)?;
        written += frame.len() as u64;
    }
    index.write_all(&written.to_le_bytes())?;
    for w in [Some(data), Some(index), gids].into_iter().flatten() {
        w.into_inner().map_err(io::IntoInnerError::into_error)?;
    }
    drop(plain);
    std::fs::remove_file(path)
}

/// A string pool.
pub struct Pool(Mapped);

impl Pool {
    pub fn open(path: &Path) -> io::Result<Self> {
        Ok(Self(Mapped::open(path)?))
    }

    /// The string at `offset`; `None` for NULL.
    #[must_use]
    pub fn get(&self, offset: u32) -> Option<&str> {
        if offset == NULL {
            return None;
        }
        let bytes = self.0.bytes();
        let mut i = usize::try_from(offset).ok()?;
        let mut len = 0usize;
        let mut shift = 0;
        loop {
            let b = *bytes.get(i)?;
            i += 1;
            len |= usize::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                break;
            }
            shift += 7;
        }
        std::str::from_utf8(bytes.get(i..i + len)?).ok()
    }
}

/// The rows of a table by a key: CSR. `values` holds each key's rows as zigzag varints of the difference to the
/// previous row (mostly one byte: rows of a key are mostly close and ascending); `offsets` are byte offsets in it.
pub struct Csr {
    /// Sparse: the keys that have rows, sorted. Empty: dense (`offsets` has an entry per key, plus one).
    keys: Mapped,
    offsets: Mapped,
    values: Mapped,
}

impl Csr {
    pub fn open(base: &Path) -> io::Result<Self> {
        Ok(Self {
            keys: Mapped::open(&with_ext(base, "keys"))?,
            offsets: Mapped::open(&with_ext(base, "offsets"))?,
            values: Mapped::open(&with_ext(base, "values"))?,
        })
    }

    /// The rows with this key, in the order they were added for it.
    pub fn get(&self, key: u32) -> impl Iterator<Item = u32> + '_ {
        let index = if self.keys.bytes().is_empty() {
            Some(key as usize)
        } else {
            let keys = self.keys.bytes();
            let n = keys.len() / 4;
            // binary search
            let (mut lo, mut hi) = (0usize, n);
            while lo < hi {
                let mid = usize::midpoint(lo, hi);
                if u32_at(keys, mid * 4) < key {
                    lo = mid + 1;
                } else {
                    hi = mid;
                }
            }
            (lo < n && u32_at(keys, lo * 4) == key).then_some(lo)
        };
        let (start, end) = index.map_or((0, 0), |i| {
            let offsets = self.offsets.bytes();
            let start = u32_at(offsets, i * 4);
            let end = u32_at(offsets, i * 4 + 4);
            if start == NONE || end == NONE {
                (0, 0)
            } else {
                (start as usize, end as usize)
            }
        });
        let mut bytes = self.values.bytes().get(start..end).unwrap_or_default();
        let mut previous = 0i64;
        std::iter::from_fn(move || {
            let mut value = 0u64;
            let mut shift = 0;
            loop {
                let (&b, rest) = bytes.split_first()?;
                bytes = rest;
                value |= u64::from(b & 0x7f) << shift;
                shift += 7;
                if b & 0x80 == 0 {
                    break;
                }
            }
            previous += unzigzag(value);
            u32::try_from(previous).ok()
        })
    }
}

fn zigzag(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)).cast_unsigned()
}

fn unzigzag(v: u64) -> i64 {
    (v >> 1).cast_signed() ^ -((v & 1).cast_signed())
}

/// Writes a varint into `buf`, returning its length.
fn varint(mut v: u64, buf: &mut [u8; 5]) -> usize {
    let mut n = 0;
    for slot in buf.iter_mut() {
        let b = u8::try_from(v & 0x7f).unwrap_or(0);
        v >>= 7;
        n += 1;
        if v == 0 {
            *slot = b;
            break;
        }
        *slot = b | 0x80;
    }
    n
}

/// An MBID index: row ids sorted by the MBID at `gid_offset` of their rows.
pub struct GidIndex(Mapped);

impl GidIndex {
    pub fn open(path: &Path) -> io::Result<Self> {
        Ok(Self(Mapped::open(path)?))
    }

    #[must_use]
    pub fn find(&self, rows: &Rows, gid_offset: usize, gid: u128) -> Option<u32> {
        let ids = self.0.bytes();
        let (mut lo, mut hi) = (0usize, ids.len() / 4);
        while lo < hi {
            let mid = usize::midpoint(lo, hi);
            let id = u32_at(ids, mid * 4);
            let found = rows.gid(id, gid_offset);
            match found.cmp(&gid) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => return Some(id),
            }
        }
        None
    }
}

#[must_use]
pub fn with_ext(base: &Path, ext: &str) -> PathBuf {
    let mut name = base.as_os_str().to_owned();
    name.push(".");
    name.push(ext);
    PathBuf::from(name)
}

// Writers. They keep memory bounded: what has to be reordered goes through temporary files next to the output.

/// Rows held per temporary bucket of a [`RowsWriter`] by id (64 MB of rows).
const BUCKET_BYTES: usize = 64 << 20;

/// Writes fixed-width rows: in order (`sequential`), or by id in any order. Rows by id go to temporary files by
/// id range first; `finish` lays each range out in memory and appends it.
pub struct RowsWriter {
    path: PathBuf,
    width: usize,
    /// In order: the output, and the next row.
    file: Option<BufWriter<File>>,
    next: u64,
    /// By id: rows per bucket, the buckets (row within bucket, then the row's bytes), the highest row.
    bucket_rows: u64,
    buckets: Vec<Option<BufWriter<File>>>,
    max: Option<u64>,
    count: u64,
}

impl RowsWriter {
    pub fn create(path: &Path, width: usize, sequential: bool) -> io::Result<Self> {
        Ok(Self {
            path: path.to_owned(),
            width,
            file: if sequential {
                Some(BufWriter::with_capacity(1 << 20, File::create(path)?))
            } else {
                None
            },
            next: 0,
            bucket_rows: (BUCKET_BYTES / width.max(1)).max(1) as u64,
            buckets: Vec::new(),
            max: None,
            count: 0,
        })
    }

    /// Writes the row at position `row` (by id), or the next row (in order: `row` is ignored).
    pub fn put(&mut self, row: u64, bytes: &[u8]) -> io::Result<()> {
        debug_assert_eq!(bytes.len(), self.width);
        if let Some(file) = &mut self.file {
            file.write_all(bytes)?;
            self.next += 1;
            return Ok(());
        }
        let bucket = usize::try_from(row / self.bucket_rows).map_err(io::Error::other)?;
        if self.buckets.len() <= bucket {
            self.buckets.resize_with(bucket + 1, || None);
        }
        let slot = self
            .buckets
            .get_mut(bucket)
            .ok_or_else(|| io::Error::other("bucket"))?;
        if slot.is_none() {
            *slot = Some(BufWriter::with_capacity(
                256 << 10,
                File::create(with_ext(&self.path, &format!("tmp{bucket}")))?,
            ));
        }
        if let Some(w) = slot {
            let within = u32::try_from(row % self.bucket_rows).map_err(io::Error::other)?;
            w.write_all(&within.to_le_bytes())?;
            w.write_all(bytes)?;
        }
        self.max = Some(self.max.map_or(row, |m| m.max(row)));
        self.count += 1;
        Ok(())
    }

    /// The next position (in order).
    #[must_use]
    pub fn len(&self) -> u64 {
        self.next
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.next == 0
    }

    pub fn finish(self) -> io::Result<()> {
        if let Some(file) = self.file {
            return file
                .into_inner()
                .map_err(io::IntoInnerError::into_error)?
                .sync_all();
        }
        let mut out = BufWriter::with_capacity(1 << 20, File::create(&self.path)?);
        let Some(max) = self.max else { return Ok(()) };
        let total = (max + 1) * self.width as u64;
        // Ids used for less than two thirds of their range: only the rows, and their ids (found by binary search).
        let sparse = (max + 1) * 2 > self.count * 3;
        let mut ids = if sparse {
            Some(BufWriter::with_capacity(
                1 << 20,
                File::create(with_ext(&self.path, "ids"))?,
            ))
        } else {
            let _ = std::fs::remove_file(with_ext(&self.path, "ids"));
            None
        };
        let mut window =
            vec![0u8; usize::try_from(self.bucket_rows).map_err(io::Error::other)? * self.width];
        let mut written = 0u64;
        for (bucket, slot) in self.buckets.into_iter().enumerate() {
            window.fill(0);
            let mut present = Vec::new();
            if let Some(w) = slot {
                drop(w.into_inner().map_err(io::IntoInnerError::into_error)?);
                let tmp = with_ext(&self.path, &format!("tmp{bucket}"));
                let data = std::fs::read(&tmp)?;
                for record in data.chunks_exact(4 + self.width) {
                    let within = u32_at(record, 0);
                    let at = within as usize * self.width;
                    if let (Some(dst), Some(src)) =
                        (window.get_mut(at..at + self.width), record.get(4..))
                    {
                        dst.copy_from_slice(src);
                    }
                    present.push(within);
                }
                std::fs::remove_file(&tmp)?;
            }
            if let Some(ids) = &mut ids {
                present.sort_unstable();
                present.dedup();
                for within in present {
                    let at = within as usize * self.width;
                    out.write_all(window.get(at..at + self.width).unwrap_or_default())?;
                    let id = u32::try_from(bucket as u64 * self.bucket_rows + u64::from(within))
                        .map_err(io::Error::other)?;
                    ids.write_all(&id.to_le_bytes())?;
                }
                continue;
            }
            let take = usize::try_from((total - written).min(window.len() as u64))
                .map_err(io::Error::other)?;
            out.write_all(window.get(..take).unwrap_or_default())?;
            written += take as u64;
        }
        if let Some(ids) = ids {
            ids.into_inner().map_err(io::IntoInnerError::into_error)?;
        }
        out.into_inner()
            .map_err(io::IntoInnerError::into_error)?
            .sync_all()
    }
}

/// Writes a string pool; with `intern`, identical strings share one copy.
pub struct PoolWriter {
    file: BufWriter<File>,
    len: u64,
    seen: Option<HashMap<u128, u32>>,
    hasher: (std::hash::RandomState, std::hash::RandomState),
}

#[derive(Debug, thiserror::Error)]
#[error("a string pool is over 4 GB")]
pub struct PoolFull;

impl PoolWriter {
    pub fn create(path: &Path, intern: bool) -> io::Result<Self> {
        let mut file = BufWriter::with_capacity(1 << 20, File::create(path)?);
        // offset 0: NULL (never read), offset 1: the empty string
        file.write_all(&[0, 0])?;
        Ok(Self {
            file,
            len: 2,
            seen: intern.then(HashMap::new),
            hasher: (std::hash::RandomState::new(), std::hash::RandomState::new()),
        })
    }

    /// Adds a string; with `intern` (and a pool created with interning), an identical earlier string is reused.
    pub fn add(&mut self, s: Option<&str>, intern: bool) -> io::Result<u32> {
        let Some(s) = s else { return Ok(NULL) };
        if s.is_empty() {
            return Ok(EMPTY);
        }
        let key = self.seen.as_ref().filter(|_| intern).map(|_| {
            // Two independent 64-bit hashes: a collision of both is never going to happen.
            let mut a = self.hasher.0.build_hasher();
            a.write(s.as_bytes());
            let mut b = self.hasher.1.build_hasher();
            b.write(s.as_bytes());
            (u128::from(a.finish()) << 64) | u128::from(b.finish())
        });
        if let (Some(seen), Some(key)) = (&self.seen, key)
            && let Some(&offset) = seen.get(&key)
        {
            return Ok(offset);
        }
        let offset = u32::try_from(self.len).map_err(|_| io::Error::other(PoolFull))?;
        let mut len = s.len();
        let mut header = Vec::with_capacity(10);
        loop {
            let b = u8::try_from(len & 0x7f).unwrap_or(0);
            len >>= 7;
            if len == 0 {
                header.push(b);
                break;
            }
            header.push(b | 0x80);
        }
        let n = header.len();
        self.file.write_all(&header)?;
        self.file.write_all(s.as_bytes())?;
        self.len += (n + s.len()) as u64;
        if let (Some(seen), Some(key)) = (&mut self.seen, key) {
            seen.insert(key, offset);
        }
        Ok(offset)
    }

    pub fn finish(self) -> io::Result<()> {
        self.file
            .into_inner()
            .map_err(io::IntoInnerError::into_error)?
            .sync_all()
    }
}

/// Collects (key, order, row) and writes them as a CSR: each key's rows sorted by `order`. The triples go to a
/// temporary file; `write` counts them per key, then places them (memory: 8 bytes per row and 4 per possible key).
pub struct CsrWriter {
    tmp: PathBuf,
    file: BufWriter<File>,
    count: u64,
    max_key: u32,
}

impl CsrWriter {
    pub fn create(base: &Path) -> io::Result<Self> {
        let tmp = with_ext(base, "tmp");
        Ok(Self {
            file: BufWriter::with_capacity(256 << 10, File::create(&tmp)?),
            tmp,
            count: 0,
            max_key: 0,
        })
    }

    pub fn add(&mut self, key: u32, order: u32, row: u32) -> io::Result<()> {
        if key == NONE {
            return Ok(());
        }
        self.file.write_all(&key.to_le_bytes())?;
        self.file.write_all(&order.to_le_bytes())?;
        self.file.write_all(&row.to_le_bytes())?;
        self.count += 1;
        self.max_key = self.max_key.max(key);
        Ok(())
    }

    pub fn write(self, base: &Path) -> io::Result<()> {
        drop(
            self.file
                .into_inner()
                .map_err(io::IntoInnerError::into_error)?,
        );
        let data = std::fs::read(&self.tmp)?;
        std::fs::remove_file(&self.tmp)?;
        let n = usize::try_from(self.count).map_err(io::Error::other)?;
        let keys = self.max_key as usize + 1;
        // counts, then offsets
        let mut starts = vec![0u32; keys + 1];
        for t in data.as_chunks::<12>().0 {
            if let Some(c) = starts.get_mut(u32_at(t, 0) as usize + 1) {
                *c += 1;
            }
        }
        let mut sum = 0;
        for s in &mut starts {
            sum += *s;
            *s = sum;
        }
        let mut next = starts.clone();
        let mut placed = vec![(0u32, 0u32); n];
        for t in data.as_chunks::<12>().0 {
            if let Some(slot) = next.get_mut(u32_at(t, 0) as usize) {
                if let Some(p) = placed.get_mut(*slot as usize) {
                    *p = (u32_at(t, 4), u32_at(t, 8));
                }
                *slot += 1;
            }
        }
        drop(data);
        for w in starts.windows(2) {
            if let [a, b] = *w
                && let Some(slice) = placed.get_mut(a as usize..b as usize)
            {
                // stable: rows of the same order stay in the order they came
                slice.sort_by_key(|p| p.0);
            }
        }
        // Each key's rows as varints of the difference to the previous row (zigzag: rows in another order
        // than by id go back too); the offsets are of bytes.
        let mut values = BufWriter::with_capacity(1 << 20, File::create(with_ext(base, "values"))?);
        let mut byte_starts = Vec::with_capacity(starts.len());
        let mut written = 0u64;
        let mut buf = [0u8; 5];
        for w in starts.windows(2) {
            byte_starts.push(u32::try_from(written).map_err(io::Error::other)?);
            let (a, b) = (
                w.first().copied().unwrap_or(0),
                w.last().copied().unwrap_or(0),
            );
            let mut previous = 0i64;
            for p in placed.get(a as usize..b as usize).unwrap_or_default() {
                let delta = i64::from(p.1) - previous;
                previous = i64::from(p.1);
                let n = varint(zigzag(delta), &mut buf);
                values.write_all(buf.get(..n).unwrap_or_default())?;
                written += n as u64;
            }
        }
        byte_starts.push(u32::try_from(written).map_err(io::Error::other)?);
        values
            .into_inner()
            .map_err(io::IntoInnerError::into_error)?;
        let distinct = starts.windows(2).filter(|w| w.first() < w.last()).count();
        let mut offsets =
            BufWriter::with_capacity(1 << 20, File::create(with_ext(base, "offsets"))?);
        // Dense when that is no bigger than sparse (4 bytes per possible key, against 8 per key with rows).
        if keys < distinct * 2 {
            let _ = std::fs::remove_file(with_ext(base, "keys"));
            for o in &byte_starts {
                offsets.write_all(&o.to_le_bytes())?;
            }
        } else {
            let mut keys_file =
                BufWriter::with_capacity(1 << 20, File::create(with_ext(base, "keys"))?);
            for ((k, w), offset) in (0u32..).zip(starts.windows(2)).zip(&byte_starts) {
                if let [a, b] = *w
                    && b > a
                {
                    keys_file.write_all(&k.to_le_bytes())?;
                    offsets.write_all(&offset.to_le_bytes())?;
                }
            }
            offsets.write_all(&byte_starts.last().copied().unwrap_or(0).to_le_bytes())?;
            keys_file
                .into_inner()
                .map_err(io::IntoInnerError::into_error)?;
        }
        offsets
            .into_inner()
            .map_err(io::IntoInnerError::into_error)?;
        Ok(())
    }
}

/// Sorts fixed-size records through 256 temporary files split by their first byte: records must sort by their
/// bytes (big-endian keys first), and their first byte must be spread (a hash or an MBID).
pub struct RadixSorter<const N: usize> {
    base: PathBuf,
    files: Vec<Option<BufWriter<File>>>,
}

impl<const N: usize> RadixSorter<N> {
    #[must_use]
    pub fn new(base: &Path) -> Self {
        Self {
            base: base.to_owned(),
            files: (0..256).map(|_| None).collect(),
        }
    }

    pub fn add(&mut self, record: [u8; N]) -> io::Result<()> {
        let bucket = usize::from(record.first().copied().unwrap_or(0));
        let slot = self
            .files
            .get_mut(bucket)
            .ok_or_else(|| io::Error::other("bucket"))?;
        if slot.is_none() {
            *slot = Some(BufWriter::with_capacity(
                64 << 10,
                File::create(with_ext(&self.base, &format!("tmp{bucket}")))?,
            ));
        }
        if let Some(w) = slot {
            w.write_all(&record)?;
        }
        Ok(())
    }

    /// Calls `emit` with every record, in order.
    pub fn finish(self, mut emit: impl FnMut(&[u8; N]) -> io::Result<()>) -> io::Result<()> {
        for (bucket, slot) in self.files.into_iter().enumerate() {
            let Some(w) = slot else { continue };
            drop(w.into_inner().map_err(io::IntoInnerError::into_error)?);
            let tmp = with_ext(&self.base, &format!("tmp{bucket}"));
            let data = std::fs::read(&tmp)?;
            std::fs::remove_file(&tmp)?;
            let mut records: Vec<[u8; N]> = data.as_chunks::<N>().0.to_vec();
            records.sort_unstable();
            for r in &records {
                emit(r)?;
            }
        }
        Ok(())
    }
}

/// Writes the MBID index of a rows file: the ids of rows with an MBID, sorted by it.
pub fn write_gid_index(rows: &Rows, gid_offset: usize, path: &Path) -> io::Result<()> {
    let mut sorter = RadixSorter::<20>::new(path);
    for position in 0..u32::try_from(rows.len()).unwrap_or(u32::MAX) {
        let id = rows.id_at(position);
        let gid = rows.gid(id, gid_offset);
        if gid != 0 {
            let mut record = [0u8; 20];
            record[..16].copy_from_slice(&gid.to_be_bytes());
            record[16..].copy_from_slice(&id.to_le_bytes());
            sorter.add(record)?;
        }
    }
    let mut out = BufWriter::with_capacity(1 << 20, File::create(path)?);
    sorter.finish(|r| out.write_all(r.get(16..).unwrap_or_default()))?;
    out.into_inner().map_err(io::IntoInnerError::into_error)?;
    Ok(())
}

/// A sorted string index: rows by the (stable) hash of a string field. Lookups find the hash, then compare the
/// strings of the rows that have it.
pub struct HashIndex(Mapped);

impl HashIndex {
    pub fn open(path: &Path) -> io::Result<Self> {
        Ok(Self(Mapped::open(path)?))
    }

    /// The rows whose string has this hash.
    pub fn rows(&self, hash: u32) -> impl Iterator<Item = u32> + '_ {
        let data = self.0.bytes();
        let n = data.len() / 8;
        let (mut lo, mut hi) = (0usize, n);
        while lo < hi {
            let mid = usize::midpoint(lo, hi);
            if u32::from_be_bytes(
                data.get(mid * 8..mid * 8 + 4)
                    .and_then(|b| b.try_into().ok())
                    .unwrap_or([0; 4]),
            ) < hash
            {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        (lo..n)
            .take_while(move |i| {
                data.get(i * 8..i * 8 + 4)
                    .and_then(|b| b.try_into().ok())
                    .map(u32::from_be_bytes)
                    == Some(hash)
            })
            .map(move |i| u32_at(data, i * 8 + 4))
    }
}

/// Writes a hash index from (hash, row) pairs.
pub struct HashIndexWriter {
    path: PathBuf,
    sorter: RadixSorter<8>,
}

impl HashIndexWriter {
    #[must_use]
    pub fn create(path: &Path) -> Self {
        Self {
            path: path.to_owned(),
            sorter: RadixSorter::new(path),
        }
    }

    pub fn add(&mut self, hash: u32, row: u32) -> io::Result<()> {
        let mut record = [0u8; 8];
        record[..4].copy_from_slice(&hash.to_be_bytes());
        record[4..].copy_from_slice(&row.to_le_bytes());
        self.sorter.add(record)
    }

    pub fn finish(self) -> io::Result<()> {
        let mut out = BufWriter::with_capacity(1 << 20, File::create(&self.path)?);
        self.sorter.finish(|r| out.write_all(r))?;
        out.into_inner().map_err(io::IntoInnerError::into_error)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("mbm-file-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn row(gid: u128, value: u32) -> [u8; 20] {
        let mut row = [0u8; 20];
        row[..16].copy_from_slice(&gid.to_le_bytes());
        row[16..].copy_from_slice(&value.to_le_bytes());
        row
    }

    #[test]
    fn rows_by_id_in_any_order_dense_and_sparse() {
        let d = dir("rows");
        let mut dense = RowsWriter::create(&d.join("d.rows"), 20, false).unwrap();
        for id in [3u64, 1, 2, 0, 5, 4] {
            dense
                .put(
                    id,
                    &row(u128::from(id) + 100, u32::try_from(id).unwrap() * 2),
                )
                .unwrap();
        }
        dense.finish().unwrap();
        let rows = Rows::open(&d.join("d.rows"), 20, None).unwrap();
        assert!(!d.join("d.rows.ids").exists());
        assert_eq!(rows.len(), 6);
        assert_eq!(u32_at(&rows.get(3).unwrap(), 16), 6);
        assert!(rows.get(6).is_none());

        let mut sparse = RowsWriter::create(&d.join("s.rows"), 20, false).unwrap();
        for id in [3_000_000u64, 7, 9_000_000] {
            sparse.put(id, &row(u128::from(id), 1)).unwrap();
        }
        sparse.finish().unwrap();
        let rows = Rows::open(&d.join("s.rows"), 20, None).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(u128_at(&rows.get(3_000_000).unwrap(), 0), 3_000_000);
        assert_eq!(rows.id_at(0), 7);
        assert!(rows.get(8).is_none());
        write_gid_index(&rows, 0, &d.join("s.gids")).unwrap();
        let gids = GidIndex::open(&d.join("s.gids")).unwrap();
        assert_eq!(gids.find(&rows, 0, 9_000_000), Some(9_000_000));
        assert_eq!(gids.find(&rows, 0, 8), None);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn compressed_rows_read_as_plain() {
        let d = dir("zrows");
        // width 24: a u32, the MBID, a u32 (the MBID in the middle); 300 rows over 5 blocks, one hole
        let make = |id: u64| {
            let mut r = [0u8; 24];
            r[..4].copy_from_slice(&u32::try_from(id * 3).unwrap().to_le_bytes());
            if id != 77 {
                r[4..20].copy_from_slice(&(u128::from(id) << 64 | 0xabcd).to_le_bytes());
            }
            r[20..].copy_from_slice(&u32::try_from(id + 7).unwrap().to_le_bytes());
            r
        };
        for (name, ids) in [
            ("dense", (0..300).collect::<Vec<u64>>()),
            ("sparse", (0..300).map(|i| i * 1000).collect()),
        ] {
            let path = d.join(format!("{name}.rows"));
            let mut w = RowsWriter::create(&path, 24, false).unwrap();
            for &id in ids.iter().rev() {
                w.put(id, &make(id)).unwrap();
            }
            w.finish().unwrap();
            let plain = Rows::open(&path, 24, Some(4)).unwrap();
            let expected: Vec<Vec<u8>> = ids
                .iter()
                .map(|&id| plain.get(u32::try_from(id).unwrap()).unwrap().to_vec())
                .collect();
            write_gid_index(&plain, 4, &d.join(format!("{name}.gids"))).unwrap();
            drop(plain);
            compress_rows(&path, 24, Some(4), 3).unwrap();
            assert!(!path.exists());
            let rows = Rows::open(&path, 24, Some(4)).unwrap();
            assert_eq!(rows.len(), ids.len());
            // out of order too (blocks come and go from the cache)
            for (i, &id) in ids.iter().enumerate().rev().chain(ids.iter().enumerate()) {
                let id = u32::try_from(id).unwrap();
                assert_eq!(
                    &*rows.get(id).unwrap(),
                    expected[i].as_slice(),
                    "{name} {id}"
                );
                assert_eq!(rows.gid(id, 4), u128_at(&expected[i], 4));
            }
            let gids = GidIndex::open(&d.join(format!("{name}.gids"))).unwrap();
            let last = *ids.last().unwrap();
            assert_eq!(
                gids.find(&rows, 4, u128::from(last) << 64 | 0xabcd),
                Some(u32::try_from(last).unwrap())
            );
        }
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn csr_rows_far_apart_both_ways() {
        let d = dir("csr-far");
        let mut csr = CsrWriter::create(&d.join("t.k")).unwrap();
        for (order, row) in [(0, 4_000_000_000u32), (1, 7), (2, u32::MAX - 1), (3, 0)] {
            csr.add(1, order, row).unwrap();
        }
        csr.write(&d.join("t.k")).unwrap();
        let csr = Csr::open(&d.join("t.k")).unwrap();
        assert_eq!(
            csr.get(1).collect::<Vec<_>>(),
            vec![4_000_000_000, 7, u32::MAX - 1, 0]
        );
    }

    #[test]
    fn pools_csr_hashes() {
        let d = dir("pools");
        let mut pool = PoolWriter::create(&d.join("t.strings"), true).unwrap();
        let a = pool.add(Some("hello"), true).unwrap();
        assert_eq!(pool.add(Some("hello"), true).unwrap(), a);
        assert_ne!(pool.add(Some("hello"), false).unwrap(), a);
        assert_eq!(pool.add(None, true).unwrap(), NULL);
        assert_eq!(pool.add(Some(""), true).unwrap(), EMPTY);
        let long = "x".repeat(300);
        let c = pool.add(Some(&long), false).unwrap();
        pool.finish().unwrap();
        let pool = Pool::open(&d.join("t.strings")).unwrap();
        assert_eq!(pool.get(a), Some("hello"));
        assert_eq!(pool.get(NULL), None);
        assert_eq!(pool.get(EMPTY), Some(""));
        assert_eq!(pool.get(c), Some(long.as_str()));

        let mut csr = CsrWriter::create(&d.join("t.k")).unwrap();
        for (key, row) in [(5, 0), (2, 1), (5, 2), (5, 3)] {
            csr.add(key, 10 - row, row).unwrap();
        }
        csr.write(&d.join("t.k")).unwrap();
        let mut sparse = CsrWriter::create(&d.join("t.s")).unwrap();
        sparse.add(1_000_000, 0, 9).unwrap();
        sparse.write(&d.join("t.s")).unwrap();
        let csr = Csr::open(&d.join("t.k")).unwrap();
        assert_eq!(csr.get(5).collect::<Vec<_>>(), vec![3, 2, 0]);
        assert_eq!(csr.get(2).collect::<Vec<_>>(), vec![1]);
        assert_eq!(csr.get(4).count(), 0);
        assert_eq!(csr.get(99).count(), 0);
        let sparse = Csr::open(&d.join("t.s")).unwrap();
        assert!(d.join("t.s.keys").exists());
        assert_eq!(sparse.get(1_000_000).collect::<Vec<_>>(), vec![9]);
        assert_eq!(sparse.get(5).count(), 0);

        let mut hashes = HashIndexWriter::create(&d.join("t.h"));
        for (hash, row) in [(7, 1), (u32::MAX, 2), (7, 3), (0x0100_0000, 4)] {
            hashes.add(hash, row).unwrap();
        }
        hashes.finish().unwrap();
        let hashes = HashIndex::open(&d.join("t.h")).unwrap();
        let mut sevens: Vec<u32> = hashes.rows(7).collect();
        sevens.sort_unstable();
        assert_eq!(sevens, vec![1, 3]);
        assert_eq!(hashes.rows(u32::MAX).collect::<Vec<_>>(), vec![2]);
        assert_eq!(hashes.rows(8).count(), 0);
        std::fs::remove_dir_all(&d).unwrap();
    }
}
