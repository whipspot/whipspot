//! Property test of the ring's flush protocol (H13, D3): random sequences of writes, flushes,
//! interrupts, track marks, pauses and render chunk sizes through the public `SourceWriter` and a
//! `FakeOutput`. Every written frame carries its write index in its samples, so the test reads
//! back order, duplicates and what survives a flush.
//!
//! Single-threaded and deterministic: the ring is sized so a write never blocks. A failure prints
//! the seed and iteration (the `fuzz` harness) and the full op list. The default case count is
//! 500; `WHIPSPOT_FUZZ_ITERS` and `WHIPSPOT_FUZZ_SEED` change it.
//!
//! Properties, for the frames the renderer outputs, in output order:
//! 1. their write indexes strictly increase (order, no duplicate);
//! 2. a frame written while an interrupt was unacknowledged (dropped) never plays;
//! 3. after a flush or an interrupt, no frame written before it plays later than the first
//!    callback following it plus 5 ms (the declick tail's length is not assumed);
//! 4. every other frame plays once the source is resumed and the ring is rendered out, except
//!    the one frame at the end of each pause fade-out that the fade scales to exactly zero.

use whipspot_core::engine::{
    DeviceFormat, Engine, EngineConfig, OutputMode, OutputRequest, SampleFormat, SourceInfo,
    SourceWriter, StreamFormat,
};
use whipspot_testkit::FakeOutput;
use whipspot_testkit::rng::{Case, SplitMix64, fuzz};

const RATE: u32 = 44_100;
/// 5 ms at 44.1 kHz, rounded up: the allowed tail after the first callback that follows a flush.
const TAIL: u64 = 221;
/// The tag arithmetic below needs the largest write index under this bound.
const TAG_LIMIT: usize = 63_000;

#[derive(Clone, Copy, Debug)]
enum Op {
    Write(usize),
    Render(usize),
    Flush,
    Interrupt,
    StartTrack,
    End,
    Finish,
    Pause,
    Resume,
}

fn format() -> StreamFormat {
    StreamFormat {
        rate: RATE,
        channels: 2,
        source: SourceInfo {
            codec: "pcm",
            bits: Some(24),
            bitrate_kbps: None,
            lossless: true,
            replay_gain: None,
        },
    }
}

/// The frame carrying write index `tag`: left is a constant, right a constant that grows with
/// the tag, so the tag survives a pause fade that scales both (the ratio is the tag).
fn frame(tag: usize) -> [f64; 2] {
    [0.5, 0.5 + tag as f64 / 131_072.0]
}

/// The write index in a rendered frame; `None` for silence.
fn tag_of(f: [f32; 2]) -> Option<usize> {
    if f[0] == 0.0 && f[1] == 0.0 {
        return None;
    }
    assert!(f[0] > 0.0, "rendered frame {f:?} has no left signal");
    let ratio = f64::from(f[1]) / f64::from(f[0]);
    let tag = (ratio - 1.0) * 65_536.0;
    let rounded = tag.round();
    assert!(
        (tag - rounded).abs() < 0.2 && rounded >= 0.0,
        "rendered frame {f:?} does not carry a write index (decoded {tag})"
    );
    Some(rounded as usize)
}

fn random_ops(rng: &mut SplitMix64) -> Vec<Op> {
    let n = 5 + rng.below(36);
    let mut written = 0;
    let mut ops = Vec::with_capacity(n);
    for _ in 0..n {
        let op = match rng.below(100) {
            0..=34 => {
                let len = if rng.one_in(4) {
                    1 + rng.below(8)
                } else {
                    1 + rng.below(1_500)
                };
                if written + len >= TAG_LIMIT {
                    Op::Render(1 + rng.below(1_500))
                } else {
                    written += len;
                    Op::Write(len)
                }
            }
            35..=64 => Op::Render(if rng.one_in(4) {
                1 + rng.below(8)
            } else {
                1 + rng.below(1_500)
            }),
            65..=72 => Op::Flush,
            73..=78 => Op::Interrupt,
            79..=84 => Op::StartTrack,
            85..=88 => Op::End,
            89..=91 => Op::Finish,
            92..=95 => Op::Pause,
            _ => Op::Resume,
        };
        ops.push(op);
    }
    ops
}

/// A flush or interrupt: frames below `boundary` must be gone by `deadline` (output frames).
struct Discard {
    boundary: usize,
    /// Set by the first render after the event.
    deadline: Option<u64>,
}

fn check(ops: &[Op]) -> Result<(), String> {
    // A ring deep enough that no write in a case (under TAG_LIMIT frames) ever blocks.
    let config = EngineConfig {
        ring_ms: 1_500,
        ..EngineConfig::default()
    };
    let fake = FakeOutput::new(
        DeviceFormat {
            rate: RATE,
            channels: 2,
            sample: SampleFormat::F32,
            valid_bits: 32,
        },
        OutputMode::Exclusive,
    );
    let parts = Engine::new(config, Some(Box::new(fake.clone())));
    let mut engine = parts.engine;
    engine.open(&OutputRequest::default()).unwrap();
    let src: SourceWriter = parts.source;
    src.start_track(format()).map_err(|e| format!("{e:?}"))?;

    let mut issued = 0usize;
    let mut interrupted = false;
    let mut paused = false;
    // Pause fade-outs that started: each may swallow the one frame its gain reaches zero on.
    let mut fade_outs = 0usize;
    // Whether each issued index was dropped by an unacknowledged interrupt.
    let mut dropped: Vec<bool> = Vec::new();
    let mut discards: Vec<Discard> = Vec::new();
    // Rendered frames: (output position, write index).
    let mut played: Vec<(u64, usize)> = Vec::new();
    let mut out_pos = 0u64;

    let mut render = |n: usize, discards: &mut Vec<Discard>, played: &mut Vec<(u64, usize)>| {
        for d in discards.iter_mut().filter(|d| d.deadline.is_none()) {
            d.deadline = Some(out_pos + n as u64 + TAIL);
        }
        let out = fake.render_f32(n);
        for (i, f) in out.chunks_exact(2).enumerate() {
            if let Some(tag) = tag_of([f[0], f[1]]) {
                played.push((out_pos + i as u64, tag));
            }
        }
        out_pos += n as u64;
    };

    for op in ops {
        match *op {
            Op::Write(len) => {
                let data: Vec<f64> = (issued..issued + len).flat_map(frame).collect();
                src.write(&data).map_err(|e| format!("write: {e:?}"))?;
                dropped.extend(std::iter::repeat_n(interrupted, len));
                issued += len;
            }
            Op::Render(n) => render(n, &mut discards, &mut played),
            Op::Flush => {
                src.flush();
                interrupted = false;
                discards.push(Discard {
                    boundary: issued,
                    deadline: None,
                });
            }
            Op::Interrupt => {
                src.interrupt();
                interrupted = true;
                discards.push(Discard {
                    boundary: issued,
                    deadline: None,
                });
            }
            Op::StartTrack => {
                src.start_track(format()).map_err(|e| format!("{e:?}"))?;
                // Acknowledges an interrupt in band (a flush); otherwise a gapless track mark.
                if interrupted {
                    interrupted = false;
                    discards.push(Discard {
                        boundary: issued,
                        deadline: None,
                    });
                }
            }
            Op::End => src.end(),
            Op::Finish => src.finish(),
            Op::Pause => {
                src.pause();
                fade_outs += usize::from(!paused);
                paused = true;
            }
            Op::Resume => {
                src.resume();
                paused = false;
            }
        }
    }
    // Let everything out: resume, then render the ring dry.
    src.resume();
    for _ in 0..6 {
        render(2_000, &mut discards, &mut played);
    }
    // Trailing renders give frames queued at the end time to play before property 4 is judged.
    for _ in 0..40 {
        render(2_000, &mut discards, &mut played);
    }

    // 1. strictly increasing write indexes.
    for w in played.windows(2) {
        if w[1].1 <= w[0].1 {
            return Err(format!(
                "write index {} played at {} after {} at {} (order or duplicate)",
                w[1].1, w[1].0, w[0].1, w[0].0
            ));
        }
    }
    for &(pos, tag) in &played {
        // 2. dropped frames never play.
        if dropped[tag] {
            return Err(format!(
                "write index {tag} was dropped by an interrupt but played at {pos}"
            ));
        }
        // 3. nothing from before a flush plays past its deadline.
        for d in &discards {
            let deadline = d.deadline.expect("a render followed every event");
            if pos >= deadline && tag < d.boundary {
                return Err(format!(
                    "write index {tag} (written before a discard at index {}) played at {pos}, \
                     past its deadline {deadline}",
                    d.boundary
                ));
            }
        }
    }
    // 4. every frame at or after the last discard boundary, not dropped, plays.
    let floor = discards.iter().map(|d| d.boundary).max().unwrap_or(0);
    let mut seen = vec![false; issued];
    for &(_, tag) in &played {
        seen[tag] = true;
    }
    let missing: Vec<usize> = (floor..issued)
        .filter(|&t| !dropped[t] && !seen[t])
        .collect();
    if missing.len() > fade_outs {
        return Err(format!(
            "{} write indexes after the last discard at {floor} never played (first {:?}),              with {fade_outs} pause fade-outs to blame",
            missing.len(),
            &missing[..missing.len().min(8)]
        ));
    }
    Ok(())
}

#[test]
fn flush_and_interrupt_protocol_holds_for_random_sequences() {
    fuzz("prop_flush", 0x00F1_054A, 500, |case: &mut Case| {
        let ops = random_ops(&mut case.rng);
        if let Err(msg) = check(&ops) {
            panic!(
                "flush property failed (iteration {}): {msg}\nops ({}): {ops:?}",
                case.iteration,
                ops.len()
            );
        }
    });
}

/// The checker itself: a hand-built sequence it must accept, and tag decoding under a fade.
#[test]
fn the_checker_accepts_a_known_good_sequence_and_decodes_scaled_tags() {
    use Op::*;
    let ops = [
        Write(1_000),
        Render(300),
        Flush,
        Write(500),
        Render(1_500),
        Interrupt,
        Write(200),
        StartTrack,
        Write(300),
        Pause,
        Render(500),
        Resume,
        Render(1_000),
    ];
    check(&ops).unwrap();
    for tag in [0usize, 1, 62_999] {
        let f = frame(tag);
        for g in [1.0, 0.37, 0.0045] {
            let scaled = [(f[0] * g) as f32, (f[1] * g) as f32];
            assert_eq!(tag_of(scaled), Some(tag));
        }
    }
    assert_eq!(tag_of([0.0, 0.0]), None);
}
