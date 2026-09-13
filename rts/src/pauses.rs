//! How long the program was stopped for the collector: every pause, kept as a
//! histogram so a run of millions of collections costs a few hundred bytes.
//!
//! Buckets are quarter octaves of nanoseconds -- each about 19% wider than the
//! one before -- so a percentile is exact to within that, and the longest pause
//! is kept exactly. Low latency is about the tail, and the tail is what an
//! average hides, so the report is percentiles and the maximum.

/// Buckets per doubling.
const STEPS: u32 = 4;

#[derive(Debug, Clone, Default)]
pub struct Pauses {
    /// Pauses in each bucket, grown only as far as the longest pause needs.
    counts: Vec<u64>,
    pub count: u64,
    pub total_nanos: u64,
    pub max_nanos: u64,
}

/// Everything up to four nanoseconds shares the first bucket.
const FIRST_OCTAVE: u32 = 2;

fn bucket(nanos: u64) -> usize {
    let n = nanos.max(1 << FIRST_OCTAVE);
    let octave = 63 - n.leading_zeros();
    // The two bits after the leading one say where in the octave it falls.
    let frac = (n >> (octave - 2)) & 3;
    ((octave - FIRST_OCTAVE) * STEPS + frac as u32) as usize
}

/// The longest pause bucket `i` holds.
fn upper(i: usize) -> u64 {
    let octave = i as u32 / STEPS + FIRST_OCTAVE;
    let frac = i as u64 % STEPS as u64;
    let base = 1u64 << octave;
    base + (base * (frac + 1)) / STEPS as u64 - 1
}

impl Pauses {
    pub fn record(&mut self, nanos: u64) {
        let b = bucket(nanos);
        if self.counts.len() <= b {
            self.counts.resize(b + 1, 0);
        }
        self.counts[b] += 1;
        self.count += 1;
        self.total_nanos += nanos;
        self.max_nanos = self.max_nanos.max(nanos);
    }

    pub fn merge(&mut self, other: &Pauses) {
        if self.counts.len() < other.counts.len() {
            self.counts.resize(other.counts.len(), 0);
        }
        for (i, c) in other.counts.iter().enumerate() {
            self.counts[i] += c;
        }
        self.count += other.count;
        self.total_nanos += other.total_nanos;
        self.max_nanos = self.max_nanos.max(other.max_nanos);
    }

    /// The pause `q` of all pauses were no longer than, `q` in `0.0..=1.0`:
    /// the top of the bucket it falls in, and never more than the maximum.
    pub fn percentile(&self, q: f64) -> u64 {
        if self.count == 0 {
            return 0;
        }
        let rank = ((q * self.count as f64).ceil() as u64).clamp(1, self.count);
        let mut seen = 0;
        for (i, c) in self.counts.iter().enumerate() {
            seen += c;
            if seen >= rank {
                return upper(i).min(self.max_nanos);
            }
        }
        self.max_nanos
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets_are_ordered_and_hold_what_they_say() {
        let mut last = 0;
        for n in [
            1u64,
            2,
            3,
            5,
            8,
            100,
            1000,
            4096,
            5000,
            1 << 20,
            123_456_789,
        ] {
            let b = bucket(n);
            assert!(b >= last, "{n} went backwards");
            assert!(n <= upper(b), "{n} is above its bucket's top {}", upper(b));
            if b > 0 {
                assert!(n > upper(b - 1), "{n} belongs in an earlier bucket");
            }
            last = b;
        }
    }

    #[test]
    fn percentiles_find_the_tail() {
        let mut p = Pauses::default();
        for _ in 0..990 {
            p.record(10_000);
        }
        for _ in 0..10 {
            p.record(2_000_000);
        }
        assert!(p.percentile(0.5) < 13_000);
        assert!(p.percentile(0.99) < 13_000);
        assert!(p.percentile(0.999) >= 2_000_000);
        assert_eq!(p.max_nanos, 2_000_000);
        let mut q = Pauses::default();
        q.record(5);
        q.merge(&p);
        assert_eq!(q.count, 1001);
    }
}
