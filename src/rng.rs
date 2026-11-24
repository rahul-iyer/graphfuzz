//! A small deterministic generator (SplitMix64), so a seed reproduces a run exactly.

#[derive(Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// A uniform integer in [low, high].
    pub fn range(&mut self, low: i64, high: i64) -> i64 {
        let span = (high - low + 1) as u64;
        low + (self.next_u64() % span) as i64
    }

    pub fn below(&mut self, count: usize) -> usize {
        (self.next_u64() % count as u64) as usize
    }

    pub fn chance(&mut self, probability: f64) -> bool {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64 <= probability
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}
