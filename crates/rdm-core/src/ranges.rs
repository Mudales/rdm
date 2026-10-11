//! A set of half-open byte ranges, used to track which parts of the output
//! file have been written.

use serde::{Deserialize, Serialize};

/// Sorted, disjoint, non-adjacent half-open ranges `[start, end)`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "Vec<(u64, u64)>", into = "Vec<(u64, u64)>")]
pub struct RangeSet {
    ranges: Vec<(u64, u64)>,
    covered: u64,
}

impl RangeSet {
    /// An empty set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds `[start, end)`, merging with overlapping or adjacent ranges.
    pub fn insert(&mut self, start: u64, end: u64) {
        if start >= end {
            return;
        }
        // First range that ends at or after `start` (i.e. touches or overlaps).
        let first = self.ranges.partition_point(|&(_, e)| e < start);
        let mut last = first;
        let (mut lo, mut hi) = (start, end);
        let mut removed = 0;
        while let Some(&(s, e)) = self.ranges.get(last) {
            if s > end {
                break;
            }
            lo = lo.min(s);
            hi = hi.max(e);
            removed += e - s;
            last += 1;
        }
        self.ranges.splice(first..last, [(lo, hi)]);
        self.covered = self.covered - removed + (hi - lo);
    }

    /// Total number of bytes covered.
    pub fn covered(&self) -> u64 {
        self.covered
    }

    /// Whether `[start, end)` is entirely covered.
    pub fn covers(&self, start: u64, end: u64) -> bool {
        if start >= end {
            return true;
        }
        let i = self.ranges.partition_point(|&(_, e)| e <= start);
        self.ranges
            .get(i)
            .is_some_and(|&(s, e)| s <= start && e >= end)
    }

    /// The uncovered parts of `[0, total)`.
    pub fn gaps(&self, total: u64) -> Vec<(u64, u64)> {
        let mut gaps = Vec::new();
        let mut cursor = 0;
        for &(s, e) in &self.ranges {
            if cursor >= total {
                break;
            }
            if s > cursor {
                gaps.push((cursor, s.min(total)));
            }
            cursor = cursor.max(e);
        }
        if cursor < total {
            gaps.push((cursor, total));
        }
        gaps
    }

    /// Iterates over the covered ranges in ascending order.
    pub fn iter(&self) -> impl Iterator<Item = (u64, u64)> + '_ {
        self.ranges.iter().copied()
    }

    /// Whether nothing is covered.
    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }
}

impl From<Vec<(u64, u64)>> for RangeSet {
    fn from(ranges: Vec<(u64, u64)>) -> Self {
        let mut set = RangeSet::new();
        for (s, e) in ranges {
            set.insert(s, e);
        }
        set
    }
}

impl From<RangeSet> for Vec<(u64, u64)> {
    fn from(set: RangeSet) -> Self {
        set.ranges
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ranges(set: &RangeSet) -> Vec<(u64, u64)> {
        set.iter().collect()
    }

    #[test]
    fn insert_merges_overlapping_and_adjacent() {
        let mut s = RangeSet::new();
        s.insert(10, 20);
        s.insert(30, 40);
        assert_eq!(ranges(&s), [(10, 20), (30, 40)]);
        s.insert(20, 25); // adjacent to the first
        assert_eq!(ranges(&s), [(10, 25), (30, 40)]);
        s.insert(5, 35); // swallows both
        assert_eq!(ranges(&s), [(5, 40)]);
        assert_eq!(s.covered(), 35);
        s.insert(0, 0); // empty is ignored
        s.insert(7, 9); // already covered
        assert_eq!(ranges(&s), [(5, 40)]);
        assert_eq!(s.covered(), 35);
    }

    #[test]
    fn out_of_order_inserts() {
        let mut s = RangeSet::new();
        for (a, b) in [(50, 60), (0, 10), (20, 30), (10, 20), (40, 50)] {
            s.insert(a, b);
        }
        assert_eq!(ranges(&s), [(0, 30), (40, 60)]);
        assert_eq!(s.covered(), 50);
    }

    #[test]
    fn gaps_and_covers() {
        let s = RangeSet::from(vec![(10, 20), (30, 40)]);
        assert_eq!(s.gaps(50), [(0, 10), (20, 30), (40, 50)]);
        assert_eq!(s.gaps(35), [(0, 10), (20, 30)]);
        assert_eq!(RangeSet::new().gaps(5), [(0, 5)]);
        assert!(s.covers(12, 18));
        assert!(s.covers(10, 20));
        assert!(!s.covers(15, 31));
        assert!(!s.covers(0, 1));
        assert!(RangeSet::from(vec![(0, 100)]).gaps(100).is_empty());
    }

    #[test]
    fn serde_roundtrip_normalizes() {
        let s: RangeSet = serde_json::from_str("[[30,40],[10,20],[15,30]]").unwrap();
        assert_eq!(ranges(&s), [(10, 40)]);
        assert_eq!(s.covered(), 30);
        assert_eq!(serde_json::to_string(&s).unwrap(), "[[10,40]]");
    }
}
