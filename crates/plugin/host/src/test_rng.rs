//! Deterministic randomness for tests.
//!
//! Randomized tests run [`cases`] cases, seeded upwards from a base seed:
//! `PLUGIN_TEST_SEED_BASE`, or a fixed one. A failing case prints the
//! `PLUGIN_TEST_SEED` that replays it alone. `PLUGIN_TEST_CASES` scales every
//! randomized test (e.g. `10` for ten times the default number of cases).
//!
//! Shared by the unit tests and, through `#[path]`, the integration tests.

#![allow(dead_code)]

use std::panic::AssertUnwindSafe;

/// SplitMix64.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Uniform in `0..n`; `n` must not be 0.
    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }

    /// Uniform in `lo..=hi`.
    pub fn range(&mut self, lo: usize, hi: usize) -> usize {
        lo + self.below(hi - lo + 1)
    }

    /// True with probability `percent` / 100.
    pub fn chance(&mut self, percent: u64) -> bool {
        self.next_u64() % 100 < percent
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }

    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            items.swap(i, self.below(i + 1));
        }
    }
}

fn env(name: &str) -> Option<u64> {
    std::env::var(name).ok().map(|v| {
        v.parse()
            .unwrap_or_else(|_| panic!("{name} must be a number, got {v:?}"))
    })
}

/// Number of cases for a test that runs `default` cases by default: one if
/// only `PLUGIN_TEST_SEED` is set (a replay), else `default` times
/// `PLUGIN_TEST_CASES`.
pub fn cases(default: usize) -> usize {
    match (env("PLUGIN_TEST_SEED"), env("PLUGIN_TEST_CASES")) {
        (Some(_), None) => 1,
        (_, factor) => default * factor.unwrap_or(1) as usize,
    }
}

/// Run `case` for [`cases`]`(default)` seeds.
pub fn for_each_seed(default: usize, mut case: impl FnMut(&mut Rng)) {
    let base = env("PLUGIN_TEST_SEED")
        .or_else(|| env("PLUGIN_TEST_SEED_BASE"))
        .unwrap_or(0x5eed);
    for i in 0..cases(default) {
        let seed = base.wrapping_add(i as u64);
        let result = std::panic::catch_unwind(AssertUnwindSafe(|| case(&mut Rng::new(seed))));
        if let Err(panic) = result {
            eprintln!("replay this case with PLUGIN_TEST_SEED={seed}");
            std::panic::resume_unwind(panic);
        }
    }
}
