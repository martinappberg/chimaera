//! Sparse row-offset index for `fs/table`: the byte offset of every
//! `stride`-th data row, so a deep page seeks next to its target instead of
//! re-parsing a plain file from byte 0 on every request.
//!
//! An index is keyed by the canonical path plus the parse options that change
//! row numbering (delimiter, quoting, comment prefixes, header), and is only
//! valid for the file version it was built against. Scans extend it as they
//! pass rows, so a jump deep into a 2M-row file costs one budgeted scan per
//! [`super::MAX_TABLE_SCAN_BYTES`] the first time and a short seek after.
//!
//! Bounded (login-node rule): at most [`MAX_CHECKPOINTS`] offsets per file —
//! past that the stride doubles and every other checkpoint is dropped, so
//! coverage of the whole file survives at half the density — and an LRU of
//! [`MAX_FILES`] files, ~4 MB in the worst case. Gzip streams cannot seek, so
//! they never get an index.

use std::path::PathBuf;
use std::sync::Mutex;

/// Data rows between checkpoints until [`MAX_CHECKPOINTS`] forces a doubling.
pub(super) const FIRST_STRIDE: usize = 1000;
/// Offsets kept per file (8 bytes each).
pub(super) const MAX_CHECKPOINTS: usize = 16_384;
/// Files indexed at once; the least recently used one is dropped.
pub(super) const MAX_FILES: usize = 32;

/// What an index describes: one file, parsed one way.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) struct IndexKey {
    pub(super) path: PathBuf,
    /// The row-numbering options, flattened (see `TableOpts::index_key`).
    pub(super) opts: String,
}

#[derive(Clone, Debug)]
pub(super) struct RowIndex {
    /// The file version (`mtime_token`) the offsets were measured against.
    pub(super) version: String,
    stride: usize,
    /// `checkpoints[i]` is the byte offset where data row `i * stride` starts;
    /// `checkpoints[0]` is where data begins (after any header row).
    checkpoints: Vec<u64>,
    /// The data-row count, once a scan has reached EOF.
    pub(super) total: Option<usize>,
}

impl RowIndex {
    pub(super) fn new(version: String, data_start: u64) -> Self {
        RowIndex {
            version,
            stride: FIRST_STRIDE,
            checkpoints: vec![data_start],
            total: None,
        }
    }

    /// The deepest known point at or before `row`: (its row number, its byte
    /// offset). A scan resumes there and counts rows forward from it.
    pub(super) fn seek_point(&self, row: usize) -> (usize, u64) {
        let k = (row / self.stride).min(self.checkpoints.len() - 1);
        (k * self.stride, self.checkpoints[k])
    }

    /// Record that data row `row` starts at byte `at`. Called for every row a
    /// scan passes; only the next missing checkpoint is ever appended, so a
    /// scan that resumed mid-index never rewrites what is already known.
    pub(super) fn observe(&mut self, row: usize, at: u64) {
        if !row.is_multiple_of(self.stride) || row / self.stride != self.checkpoints.len() {
            return;
        }
        self.checkpoints.push(at);
        if self.checkpoints.len() > MAX_CHECKPOINTS {
            let mut i = 0usize;
            self.checkpoints.retain(|_| {
                let keep = i.is_multiple_of(2);
                i += 1;
                keep
            });
            self.stride *= 2;
        }
    }

    /// Rows covered by checkpoints (the deepest checkpoint's row number).
    pub(super) fn indexed_rows(&self) -> usize {
        (self.checkpoints.len() - 1) * self.stride
    }

    #[cfg(test)]
    pub(super) fn stride(&self) -> usize {
        self.stride
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.checkpoints.len()
    }
}

/// The LRU of indexes. Lookups hand out clones, so no lock is held while a
/// scan runs (a scan can take a second on a slow filesystem).
#[derive(Default)]
pub(super) struct IndexCache {
    tick: u64,
    slots: Vec<(IndexKey, RowIndex, u64)>,
}

impl IndexCache {
    /// The index for `key` at `version`; a stale version is dropped.
    pub(super) fn lookup(&mut self, key: &IndexKey, version: &str) -> Option<RowIndex> {
        let i = self.slots.iter().position(|(k, _, _)| k == key)?;
        if self.slots[i].1.version != version {
            self.slots.swap_remove(i);
            return None;
        }
        self.tick += 1;
        self.slots[i].2 = self.tick;
        Some(self.slots[i].1.clone())
    }

    /// Keep `index` for `key`. Two scans of the same version may race; the
    /// one that reaches deeper wins, and a known total is never forgotten.
    pub(super) fn store(&mut self, key: IndexKey, mut index: RowIndex) {
        self.tick += 1;
        if let Some(slot) = self.slots.iter_mut().find(|(k, _, _)| *k == key) {
            let held = &slot.1;
            if held.version == index.version {
                if held.indexed_rows() > index.indexed_rows() {
                    let total = index.total.or(held.total);
                    index = held.clone();
                    index.total = total;
                } else {
                    index.total = index.total.or(held.total);
                }
            }
            slot.1 = index;
            slot.2 = self.tick;
            return;
        }
        if self.slots.len() >= MAX_FILES {
            if let Some(oldest) = self
                .slots
                .iter()
                .enumerate()
                .min_by_key(|(_, (_, _, used))| *used)
                .map(|(i, _)| i)
            {
                self.slots.swap_remove(oldest);
            }
        }
        self.slots.push((key, index, self.tick));
    }
}

static CACHE: Mutex<IndexCache> = Mutex::new(IndexCache {
    tick: 0,
    slots: Vec::new(),
});

pub(super) fn lookup(key: &IndexKey, version: &str) -> Option<RowIndex> {
    crate::lock(&CACHE).lookup(key, version)
}

pub(super) fn store(key: IndexKey, index: RowIndex) {
    crate::lock(&CACHE).store(key, index);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(name: &str) -> IndexKey {
        IndexKey {
            path: PathBuf::from(format!("/t/{name}")),
            opts: "tab".into(),
        }
    }

    #[test]
    fn observe_appends_only_the_next_checkpoint() {
        let mut ix = RowIndex::new("v1".into(), 10);
        assert_eq!(ix.seek_point(5_000), (0, 10));
        // Rows between checkpoints and repeats are ignored.
        ix.observe(0, 10);
        ix.observe(999, 50);
        ix.observe(2_000, 90); // out of order: row 1000 is still missing
        assert_eq!(ix.len(), 1);
        ix.observe(1_000, 60);
        ix.observe(2_000, 110);
        assert_eq!(ix.len(), 3);
        assert_eq!(ix.indexed_rows(), 2_000);
        assert_eq!(ix.seek_point(1_999), (1_000, 60));
        assert_eq!(ix.seek_point(2_500), (2_000, 110));
        assert_eq!(ix.seek_point(1_000_000), (2_000, 110));
    }

    #[test]
    fn stride_doubles_past_the_checkpoint_cap() {
        let mut ix = RowIndex::new("v".into(), 0);
        let mut row = 0usize;
        // Feed every row a scan would pass, well past the cap.
        while ix.stride() == FIRST_STRIDE {
            ix.observe(row, row as u64 * 10);
            row += 1;
        }
        assert_eq!(ix.stride(), FIRST_STRIDE * 2);
        assert!(ix.len() <= MAX_CHECKPOINTS);
        // Offsets still land on the right rows at the coarser stride, and the
        // index keeps growing afterwards.
        let (at_row, at) = ix.seek_point(4_500);
        assert_eq!((at_row, at), (4_000, 40_000));
        let before = ix.indexed_rows();
        for r in row..row + FIRST_STRIDE * 4 {
            ix.observe(r, r as u64 * 10);
        }
        assert!(ix.indexed_rows() > before);
        let (deep_row, deep_at) = ix.seek_point(usize::MAX / 2);
        assert_eq!(deep_at, deep_row as u64 * 10);
    }

    #[test]
    fn cache_invalidates_on_version_and_evicts_lru() {
        let mut cache = IndexCache::default();
        let mut ix = RowIndex::new("v1".into(), 0);
        ix.observe(1_000, 5);
        cache.store(key("a"), ix);
        assert!(cache.lookup(&key("a"), "v1").is_some());
        // A new file version drops the stale offsets.
        assert!(cache.lookup(&key("a"), "v2").is_none());
        assert!(cache.lookup(&key("a"), "v1").is_none());

        for n in 0..MAX_FILES {
            cache.store(key(&n.to_string()), RowIndex::new("v".into(), 0));
        }
        // Touch "0" so "1" is the least recently used when one more arrives.
        assert!(cache.lookup(&key("0"), "v").is_some());
        cache.store(key("new"), RowIndex::new("v".into(), 0));
        assert_eq!(cache.slots.len(), MAX_FILES);
        assert!(cache.lookup(&key("1"), "v").is_none());
        assert!(cache.lookup(&key("0"), "v").is_some());
        assert!(cache.lookup(&key("new"), "v").is_some());
    }

    #[test]
    fn racing_stores_keep_the_deeper_index_and_the_total() {
        let mut cache = IndexCache::default();
        let mut deep = RowIndex::new("v".into(), 0);
        deep.observe(1_000, 1);
        deep.observe(2_000, 2);
        cache.store(key("r"), deep);
        let mut shallow = RowIndex::new("v".into(), 0);
        shallow.total = Some(2_345);
        cache.store(key("r"), shallow);
        let got = cache.lookup(&key("r"), "v").unwrap();
        assert_eq!(got.indexed_rows(), 2_000);
        assert_eq!(got.total, Some(2_345));
    }
}
