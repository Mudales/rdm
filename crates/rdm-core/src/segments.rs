//! Dynamic segmentation.
//!
//! The file is not split into a fixed N parts up front. Each worker asks for
//! work with [`SegmentMap::assign`]: it first gets an idle gap; when none is
//! left it *steals* the second half of the largest range another worker is
//! still downloading, as long as both halves stay at least `min_split` long.
//!
//! Workers [`claim`](SegmentMap::claim) bytes before writing them, so a
//! stealer always splits strictly after the victim's claimed position and
//! two workers never write the same byte. Bytes are only added to the
//! completed set by [`commit`](SegmentMap::commit) once they are on disk.

use crate::ranges::RangeSet;

pub(crate) type SegmentId = usize;

#[derive(Debug, Clone)]
struct Segment {
    origin: u64,
    /// Next byte to claim. Everything before it is claimed (and, between
    /// chunks, written).
    cursor: u64,
    /// Exclusive end. Only ever shrinks (when another worker steals).
    end: u64,
    active: bool,
}

impl Segment {
    fn remaining(&self) -> u64 {
        self.end.saturating_sub(self.cursor)
    }
}

/// Snapshot of one segment, for progress reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentInfo {
    /// First byte of this segment.
    pub start: u64,
    /// Next byte to be downloaded.
    pub position: u64,
    /// Exclusive end.
    pub end: u64,
    /// Whether a worker currently owns this segment.
    pub active: bool,
}

/// Result of [`SegmentMap::claim`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Claim {
    pub offset: u64,
    pub len: u64,
    /// The segment has no bytes left after this claim.
    pub exhausted: bool,
}

#[derive(Debug)]
pub(crate) struct SegmentMap {
    total: u64,
    min_split: u64,
    segments: Vec<Segment>,
    completed: RangeSet,
}

impl SegmentMap {
    pub fn new(total: u64, min_split: u64, completed: RangeSet) -> Self {
        let mut map = Self {
            total,
            min_split: min_split.max(1),
            segments: Vec::new(),
            completed,
        };
        map.reset();
        map
    }

    /// Drops all segments and recreates them from the gaps in the completed
    /// set. Used when (re)starting workers after a pause.
    pub fn reset(&mut self) {
        self.segments = self
            .completed
            .gaps(self.total)
            .into_iter()
            .map(|(s, e)| Segment {
                origin: s,
                cursor: s,
                end: e,
                active: false,
            })
            .collect();
    }

    /// Forgets all progress (used when restarting from zero).
    pub fn clear(&mut self, total: u64) {
        self.total = total;
        self.completed = RangeSet::new();
        self.reset();
    }

    /// Hands out work: the largest idle gap, or else half of the largest
    /// active segment. `None` means there is nothing worth taking.
    pub fn assign(&mut self) -> Option<SegmentId> {
        let idle = self
            .segments
            .iter()
            .enumerate()
            .filter(|(_, s)| !s.active && s.remaining() > 0)
            .max_by_key(|(_, s)| s.remaining())
            .map(|(i, _)| i);
        if let Some(i) = idle {
            if let Some(seg) = self.segments.get_mut(i) {
                seg.active = true;
            }
            return Some(i);
        }

        let (victim, remaining) = self
            .segments
            .iter()
            .enumerate()
            .filter(|(_, s)| s.active)
            .map(|(i, s)| (i, s.remaining()))
            .max_by_key(|&(_, r)| r)?;
        if remaining < self.min_split.saturating_mul(2) {
            return None;
        }
        let seg = self.segments.get_mut(victim)?;
        let mid = seg.cursor + remaining / 2;
        let old_end = seg.end;
        seg.end = mid;
        self.segments.push(Segment {
            origin: mid,
            cursor: mid,
            end: old_end,
            active: true,
        });
        Some(self.segments.len() - 1)
    }

    /// `(cursor, end)` of a segment.
    pub fn bounds(&self, id: SegmentId) -> Option<(u64, u64)> {
        self.segments.get(id).map(|s| (s.cursor, s.end))
    }

    /// Reserves up to `len` bytes at the segment's cursor.
    pub fn claim(&mut self, id: SegmentId, len: u64) -> Claim {
        match self.segments.get_mut(id) {
            Some(seg) => {
                let offset = seg.cursor;
                let len = len.min(seg.remaining());
                seg.cursor += len;
                Claim {
                    offset,
                    len,
                    exhausted: seg.remaining() == 0,
                }
            }
            None => Claim {
                offset: 0,
                len: 0,
                exhausted: true,
            },
        }
    }

    /// Records `[offset, offset + len)` as written to disk.
    pub fn commit(&mut self, offset: u64, len: u64) {
        self.completed.insert(offset, offset + len);
    }

    /// Marks the segment as no longer owned by a worker.
    pub fn release(&mut self, id: SegmentId) {
        if let Some(seg) = self.segments.get_mut(id) {
            seg.active = false;
        }
    }

    pub fn completed(&self) -> &RangeSet {
        &self.completed
    }

    pub fn is_complete(&self) -> bool {
        self.completed.covers(0, self.total)
    }

    /// Unfinished or active segments, ordered by offset.
    pub fn snapshot(&self) -> Vec<SegmentInfo> {
        let mut out: Vec<SegmentInfo> = self
            .segments
            .iter()
            .filter(|s| s.active || s.remaining() > 0)
            .map(|s| SegmentInfo {
                start: s.origin,
                position: s.cursor,
                end: s.end,
                active: s.active,
            })
            .collect();
        out.sort_by_key(|s| s.start);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MB: u64 = 1 << 20;

    #[test]
    fn first_worker_takes_everything_then_others_split() {
        let mut m = SegmentMap::new(8 * MB, MB, RangeSet::new());
        let a = m.assign().unwrap();
        assert_eq!(m.bounds(a), Some((0, 8 * MB)));
        let b = m.assign().unwrap();
        assert_eq!(m.bounds(a), Some((0, 4 * MB)));
        assert_eq!(m.bounds(b), Some((4 * MB, 8 * MB)));
        let c = m.assign().unwrap();
        let d = m.assign().unwrap();
        let mut all: Vec<_> = [a, b, c, d].iter().map(|&i| m.bounds(i).unwrap()).collect();
        all.sort();
        assert_eq!(
            all,
            [
                (0, 2 * MB),
                (2 * MB, 4 * MB),
                (4 * MB, 6 * MB),
                (6 * MB, 8 * MB)
            ]
        );
    }

    #[test]
    fn stops_splitting_below_min_split() {
        let mut m = SegmentMap::new(3 * MB, MB, RangeSet::new());
        let a = m.assign().unwrap();
        let _b = m.assign().unwrap(); // splits 3 MB into 1.5 + 1.5
        assert_eq!(m.bounds(a), Some((0, 3 * MB / 2)));
        assert!(m.assign().is_none(), "1.5 MB halves would be below 1 MB");
    }

    #[test]
    fn steal_happens_after_claimed_bytes() {
        let mut m = SegmentMap::new(10 * MB, MB, RangeSet::new());
        let a = m.assign().unwrap();
        let c = m.claim(a, 6 * MB);
        assert_eq!((c.offset, c.len, c.exhausted), (0, 6 * MB, false));
        let b = m.assign().unwrap();
        // 4 MB remain after the claim; the thief takes the back 2 MB.
        assert_eq!(m.bounds(a), Some((6 * MB, 8 * MB)));
        assert_eq!(m.bounds(b), Some((8 * MB, 10 * MB)));
        // The victim's next claim is truncated at the new end.
        let c = m.claim(a, 5 * MB);
        assert_eq!((c.offset, c.len, c.exhausted), (6 * MB, 2 * MB, true));
    }

    #[test]
    fn resume_only_schedules_gaps() {
        let done = RangeSet::from(vec![(0, 2 * MB), (5 * MB, 6 * MB)]);
        let mut m = SegmentMap::new(8 * MB, MB, done);
        let a = m.assign().unwrap();
        let b = m.assign().unwrap();
        assert_eq!(m.bounds(a), Some((2 * MB, 5 * MB)));
        assert_eq!(m.bounds(b), Some((6 * MB, 8 * MB)));
    }

    #[test]
    fn simulated_workers_cover_file_exactly_once() {
        // Drive several "workers" round-robin with uneven chunk sizes and check
        // that every byte is claimed exactly once.
        let total = 37 * MB + 12345;
        let mut m = SegmentMap::new(total, MB, RangeSet::new());
        let mut owned: Vec<Option<SegmentId>> = vec![None; 6];
        let mut claimed = RangeSet::new();
        let mut claimed_bytes = 0;
        let mut round = 0u64;
        loop {
            let mut progressed = false;
            for (w, slot) in owned.iter_mut().enumerate() {
                if slot.is_none() {
                    *slot = m.assign();
                }
                let Some(id) = *slot else { continue };
                let step = 200_000 + (w as u64) * 150_000 + (round % 7) * 10_000;
                let c = m.claim(id, step);
                if c.len > 0 {
                    assert!(!claimed.covers(c.offset, c.offset + 1), "double claim");
                    claimed.insert(c.offset, c.offset + c.len);
                    claimed_bytes += c.len;
                    m.commit(c.offset, c.len);
                    progressed = true;
                }
                if c.exhausted {
                    m.release(id);
                    *slot = None;
                }
            }
            round += 1;
            if !progressed && owned.iter().all(Option::is_none) {
                break;
            }
        }
        assert_eq!(claimed_bytes, total);
        assert!(m.is_complete());
    }
}
