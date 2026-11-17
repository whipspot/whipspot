//! A small seeded generator, a byte mutator and a fuzz-loop harness for the parser fuzz tests
//! (D2, H12, H31). Stable Rust, no dependency, not coverage-guided: random structure-aware
//! mutation of valid inputs, repeatable from a printed seed.
//!
//! Environment (read by [`fuzz`]):
//! - `WHIPSPOT_FUZZ_ITERS=<n>` replaces the test's default iteration count (long runs).
//! - `WHIPSPOT_FUZZ_SEED=<n>` is xor-ed into the test's fixed seed (varies the run; the same value
//!   repeats it).

use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};

/// SplitMix64: a 64-bit generator whose whole state is one counter. Not for anything secret.
#[derive(Debug, Clone)]
pub struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    pub fn new(seed: u64) -> SplitMix64 {
        SplitMix64 { state: seed }
    }

    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A value in `0..n`; `0` when `n` is `0`. The tiny modulo bias is irrelevant here.
    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next_u64() % n as u64) as usize
        }
    }

    /// `true` once in `n` calls on average (`n` of 0 or 1 is always true).
    pub fn one_in(&mut self, n: usize) -> bool {
        self.below(n) == 0
    }

    /// Fills `out` with random bytes.
    pub fn fill(&mut self, out: &mut [u8]) {
        for chunk in out.chunks_mut(8) {
            let word = self.next_u64().to_le_bytes();
            chunk.copy_from_slice(&word[..chunk.len()]);
        }
    }

    /// One element of a non-empty slice.
    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

/// Values that sit on the edge of a size, count or offset field.
const EDGE_U32: [u32; 16] = [
    0,
    1,
    2,
    0x7f,
    0x80,
    0xff,
    0x100,
    0x7fff,
    0x8000,
    0xffff,
    0x1_0000,
    0x00ff_ffff,
    0x7fff_ffff,
    0x8000_0000,
    0xffff_fffe,
    0xffff_ffff,
];

/// Applies one to four random mutations to `buf`: bit flips, a byte set to an edge value, an
/// insert, a delete, a truncation, a chunk duplicated, a 16- or 32-bit field overwritten with an
/// edge value (the length-field and count-field rewrite), or a token from `dict` inserted.
/// `buf` may end up empty. Never panics.
pub fn mutate(buf: &mut Vec<u8>, rng: &mut SplitMix64, dict: &[&[u8]]) {
    for _ in 0..=rng.below(4) {
        let len = buf.len();
        match rng.below(9) {
            0 if len > 0 => {
                let at = rng.below(len);
                buf[at] ^= 1 << rng.below(8);
            }
            1 if len > 0 => {
                let at = rng.below(len);
                buf[at] = *rng.pick(&[0u8, 1, 0x7f, 0x80, 0xff, b'\n', b'-', b'0', b',']);
            }
            2 => {
                let at = rng.below(len + 1);
                let n = 1 + rng.below(8);
                let mut bytes = vec![0u8; n];
                rng.fill(&mut bytes);
                buf.splice(at..at, bytes);
            }
            3 if len > 0 => {
                let at = rng.below(len);
                let n = (1 + rng.below(16)).min(len - at);
                buf.drain(at..at + n);
            }
            4 if len > 0 => {
                buf.truncate(rng.below(len));
            }
            5 if len > 0 => {
                let at = rng.below(len);
                let n = (1 + rng.below(64)).min(len - at);
                let chunk = buf[at..at + n].to_vec();
                let to = rng.below(len + 1);
                buf.splice(to..to, chunk);
            }
            6 if len >= 2 => {
                let at = rng.below(len - 1);
                let v = (*rng.pick(&EDGE_U32) as u16).to_le_bytes();
                buf[at..at + 2].copy_from_slice(&v);
            }
            7 if len >= 4 => {
                let at = rng.below(len - 3);
                let v = rng.pick(&EDGE_U32).to_le_bytes();
                buf[at..at + 4].copy_from_slice(&v);
            }
            8 if !dict.is_empty() => {
                let at = rng.below(len + 1);
                let token = rng.pick(dict).to_vec();
                buf.splice(at..at, token);
            }
            _ => {}
        }
    }
}

/// An edge value for a `u32` field.
pub fn edge_u32(rng: &mut SplitMix64) -> u32 {
    *rng.pick(&EDGE_U32)
}

/// One iteration of a [`fuzz`] loop: its generator and where to report the input length.
pub struct Case {
    pub rng: SplitMix64,
    /// The iteration number, from 0.
    pub iteration: u64,
    input_len: usize,
}

impl Case {
    /// Records the size of the input being tried, for the failure report.
    pub fn set_input_len(&mut self, len: usize) {
        self.input_len = len;
    }
}

/// The count of iterations to run: `WHIPSPOT_FUZZ_ITERS` when set and valid, else `default_iters`.
fn iterations(default_iters: u64) -> u64 {
    std::env::var("WHIPSPOT_FUZZ_ITERS")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default_iters)
}

fn seed_of(default_seed: u64) -> u64 {
    let extra: u64 = std::env::var("WHIPSPOT_FUZZ_SEED")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);
    default_seed ^ extra
}

/// The generator of iteration `iteration` under `seed`: each iteration has its own, so one
/// failing case repeats without running the ones before it.
pub fn case_rng(seed: u64, iteration: u64) -> SplitMix64 {
    SplitMix64::new(seed ^ iteration.wrapping_mul(0xD6E8_FEB8_6659_FD93))
}

/// Runs `body` for every iteration. A panic in `body` is reported (test name, seed, iteration,
/// input length) on stderr and then resumed, so the test fails with the original message. The
/// default iteration count should keep the test under about 10 s in debug.
pub fn fuzz(name: &str, default_seed: u64, default_iters: u64, mut body: impl FnMut(&mut Case)) {
    let seed = seed_of(default_seed);
    let iters = iterations(default_iters);
    for iteration in 0..iters {
        let mut case = Case {
            rng: case_rng(seed, iteration),
            iteration,
            input_len: 0,
        };
        let result = catch_unwind(AssertUnwindSafe(|| body(&mut case)));
        if let Err(payload) = result {
            eprintln!(
                "fuzz {name}: FAILED at seed={seed} iteration={iteration} input_len={} \
                 (repeat: WHIPSPOT_FUZZ_SEED={} WHIPSPOT_FUZZ_ITERS={})",
                case.input_len,
                seed ^ default_seed,
                iteration + 1
            );
            resume_unwind(payload);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitmix_matches_the_reference_vector() {
        // The published SplitMix64 outputs for seed 1234567.
        let mut r = SplitMix64::new(1_234_567);
        assert_eq!(r.next_u64(), 6_457_827_717_110_365_317);
        assert_eq!(r.next_u64(), 3_203_168_211_198_807_973);
        assert_eq!(r.next_u64(), 9_817_491_932_198_370_423);
    }

    #[test]
    fn below_and_fill_stay_in_range_and_repeat() {
        let mut a = SplitMix64::new(7);
        let mut b = SplitMix64::new(7);
        for n in [1usize, 2, 3, 10, 255, 1000] {
            for _ in 0..200 {
                let v = a.below(n);
                assert!(v < n);
                assert_eq!(v, b.below(n));
            }
        }
        assert_eq!(a.below(0), 0);
        let mut x = [0u8; 13];
        let mut y = [0u8; 13];
        SplitMix64::new(9).fill(&mut x);
        SplitMix64::new(9).fill(&mut y);
        assert_eq!(x, y);
        assert!(x.iter().any(|b| *b != 0));
    }

    #[test]
    fn mutate_is_seeded_and_survives_every_length() {
        let base: Vec<u8> = (0..64u8).collect();
        let mut changed = 0;
        for seed in 0..300u64 {
            let mut one = base.clone();
            let mut two = base.clone();
            mutate(&mut one, &mut SplitMix64::new(seed), &[b"tok"]);
            mutate(&mut two, &mut SplitMix64::new(seed), &[b"tok"]);
            assert_eq!(one, two, "seed {seed}");
            changed += usize::from(one != base);
        }
        assert!(changed > 200, "most seeds change the input ({changed})");
        for len in 0..6 {
            for seed in 0..100u64 {
                let mut tiny = vec![7u8; len];
                mutate(&mut tiny, &mut SplitMix64::new(seed), &[]);
            }
        }
    }

    #[test]
    fn fuzz_runs_the_requested_iterations_and_reports_a_panic() {
        let mut seen = 0u64;
        fuzz("count", 1, 25, |c| {
            assert_eq!(c.iteration, seen);
            seen += 1;
        });
        // The env knobs are not set by this test: the count is the default unless a long run
        // set it.
        assert_eq!(seen, iterations(25));

        let caught = catch_unwind(|| {
            fuzz("planted", 1, 50, |c| {
                c.set_input_len(3);
                assert!(c.iteration != 7, "planted failure");
            });
        });
        assert!(caught.is_err(), "the harness resumes the panic");
    }

    #[test]
    fn case_rngs_differ_per_iteration_and_repeat() {
        assert_ne!(case_rng(5, 0).next_u64(), case_rng(5, 1).next_u64());
        assert_eq!(case_rng(5, 3).next_u64(), case_rng(5, 3).next_u64());
    }
}
