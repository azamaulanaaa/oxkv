//! A deterministic pseudo-random generator with a fixed seed.
//!
//! The suite never draws operating-system randomness. A scenario derives its
//! fault plan and its write order from one seed. The generator is `SplitMix64`.

/// A deterministic generator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FixedSeedRng {
    state: u64,
    seed: u64,
}

impl FixedSeedRng {
    /// Creates a generator from one fixed seed.
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self { state: seed, seed }
    }

    /// Returns the seed.
    #[must_use]
    pub const fn seed(self) -> u64 {
        self.seed
    }

    /// Returns the next unsigned 64-bit value.
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut value = self.state;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^ (value >> 31)
    }

    /// Returns a value in `0..bound`.
    ///
    /// # Panics
    ///
    /// Panics when `bound` is zero.
    #[must_use]
    pub fn next_bounded(&mut self, bound: u64) -> u64 {
        assert!(bound > 0, "the bound must be greater than zero");
        self.next_u64() % bound
    }
}

#[cfg(test)]
mod tests {
    use super::FixedSeedRng;

    #[test]
    fn one_seed_produces_one_sequence() {
        let mut left = FixedSeedRng::new(7);
        let mut right = FixedSeedRng::new(7);
        for _ in 0..32 {
            assert_eq!(left.next_u64(), right.next_u64());
        }
    }

    #[test]
    fn a_different_seed_produces_a_different_sequence() {
        let mut left = FixedSeedRng::new(7);
        let mut right = FixedSeedRng::new(8);
        assert_ne!(left.next_u64(), right.next_u64());
    }

    #[test]
    fn a_bounded_value_stays_inside_the_bound() {
        let mut rng = FixedSeedRng::new(11);
        for _ in 0..64 {
            assert!(rng.next_bounded(3) < 3);
        }
    }
}
