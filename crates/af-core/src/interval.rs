//! Which intervals overlap a query window, without visiting all of them.
//!
//! Attribution asks, for every energy window, which process windows and
//! spans overlap it. Filtering every list for every window made a rebuild
//! quadratic in the session's length: an eight-hour session at five-second
//! sampling is 5,760 × 5,760 comparisons for the process windows alone.
//!
//! [`IntervalIndex`] answers the same question in `O(log n + k)`. It is a
//! candidate generator, not a second definition of "overlaps": it returns
//! every interval that *could* overlap (closed bounds, so no half-open edge
//! case is lost), in the caller's original order, and the caller applies its
//! own exact predicate to those. Results in original order keep every
//! downstream floating-point sum in the order it always had.

/// Intervals sorted by start, with a max-end segment tree over that order.
pub(crate) struct IntervalIndex {
    /// `(start, end, original index)`, sorted by start.
    sorted: Vec<(i64, i64, usize)>,
    /// `tree[node]` = the largest `end` in that node's range of `sorted`.
    max_end: Vec<i64>,
}

impl IntervalIndex {
    /// `intervals[i]` is `(start, end)` of the caller's item `i`. An end
    /// before its start is kept as given; it can only ever be a candidate
    /// for a window that also contains its start.
    pub(crate) fn new(intervals: impl IntoIterator<Item = (i64, i64)>) -> Self {
        let mut sorted: Vec<(i64, i64, usize)> = intervals
            .into_iter()
            .enumerate()
            .map(|(index, (start, end))| (start, end, index))
            .collect();
        sorted.sort_unstable();
        let mut index = IntervalIndex {
            max_end: vec![i64::MIN; sorted.len().max(1) * 4],
            sorted,
        };
        if !index.sorted.is_empty() {
            index.build(1, 0, index.sorted.len());
        }
        index
    }

    fn build(&mut self, node: usize, lo: usize, hi: usize) -> i64 {
        let max = if hi - lo == 1 {
            let (start, end, _) = self.sorted[lo];
            end.max(start)
        } else {
            let mid = (lo + hi) / 2;
            self.build(2 * node, lo, mid)
                .max(self.build(2 * node + 1, mid, hi))
        };
        self.max_end[node] = max;
        max
    }

    /// Original indices of every interval with `start <= t1` and
    /// `max(start, end) >= t0`, ascending. A superset of those that overlap
    /// `[t0, t1)` under any half-open or closed convention.
    pub(crate) fn candidates(&self, t0: i64, t1: i64) -> Vec<usize> {
        // Only intervals starting at or before `t1` can overlap.
        let limit = self.sorted.partition_point(|(start, _, _)| *start <= t1);
        let mut out = Vec::new();
        if limit > 0 {
            self.collect(1, 0, self.sorted.len(), limit, t0, &mut out);
        }
        out.sort_unstable();
        out
    }

    fn collect(
        &self,
        node: usize,
        lo: usize,
        hi: usize,
        limit: usize,
        t0: i64,
        out: &mut Vec<usize>,
    ) {
        if lo >= limit || self.max_end[node] < t0 {
            return;
        }
        if hi - lo == 1 {
            out.push(self.sorted[lo].2);
            return;
        }
        let mid = (lo + hi) / 2;
        self.collect(2 * node, lo, mid, limit, t0, out);
        self.collect(2 * node + 1, mid, hi, limit, t0, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small deterministic generator: no `rand` dependency for one test.
    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self, bound: i64) -> i64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((self.0 >> 33) % bound as u64) as i64
        }
    }

    fn half_open_overlap(start: i64, end: i64, t0: i64, t1: i64) -> bool {
        start < t1 && end > t0
    }

    #[test]
    fn candidates_contain_every_overlap_in_original_order() {
        let mut rng = Lcg(7);
        for round in 0..200 {
            let count = rng.next(60) as usize;
            let intervals: Vec<(i64, i64)> = (0..count)
                .map(|_| {
                    let start = rng.next(1_000);
                    // Mostly short, sometimes long, sometimes zero-length
                    // or inverted: every shape a span or window can take.
                    let len = match rng.next(10) {
                        0 => 0,
                        1 => -rng.next(20),
                        2 => rng.next(1_000),
                        _ => rng.next(30),
                    };
                    (start, start + len)
                })
                .collect();
            let index = IntervalIndex::new(intervals.iter().copied());
            for _ in 0..20 {
                let t0 = rng.next(1_100) - 50;
                let t1 = t0 + rng.next(60) - 5;
                let found = index.candidates(t0, t1);
                assert!(found.windows(2).all(|pair| pair[0] < pair[1]), "ascending");
                let exact: Vec<usize> = (0..count)
                    .filter(|&i| half_open_overlap(intervals[i].0, intervals[i].1, t0, t1))
                    .collect();
                let narrowed: Vec<usize> = found
                    .iter()
                    .copied()
                    .filter(|&i| half_open_overlap(intervals[i].0, intervals[i].1, t0, t1))
                    .collect();
                assert_eq!(narrowed, exact, "round {round}, window [{t0}, {t1})");
            }
        }
    }

    #[test]
    fn an_empty_index_has_no_candidates() {
        assert!(IntervalIndex::new(std::iter::empty())
            .candidates(0, 10)
            .is_empty());
    }

    #[test]
    fn far_away_intervals_are_not_visited() {
        let index = IntervalIndex::new((0..10_000).map(|i| (i * 10, i * 10 + 5)));
        assert_eq!(index.candidates(50_000, 50_020), vec![5_000, 5_001, 5_002]);
    }
}
