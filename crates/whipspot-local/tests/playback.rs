//! Local playback through the core engine on a fake output: the null test, gapless, seek,
//! errors, the format it reports, the provider contracts and the decode thread's shutdown.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tokio::sync::broadcast;
use whipspot_core::engine::{
    DeviceFormat, Engine, EngineConfig, OutputMode, OutputRequest, SampleFormat,
};
use whipspot_core::events::UiEvent;
use whipspot_core::provider::{LoginInput, Provider, ProviderContext, Track};
use whipspot_local::{LocalConfig, LocalProvider};
use whipspot_testkit::audio_files::{TempMusicDir, flac, signal, wav};
use whipspot_testkit::{
    FakeOutput, run_provider_contract, run_provider_playback_contract,
    run_provider_session_contract,
};

static NEXT: AtomicU32 = AtomicU32::new(0);

/// A fresh directory under the OS temp dir (checked absent first), removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(what: &str) -> Scratch {
        let p = std::env::temp_dir().join(format!(
            "whipspot-play-{what}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        assert!(!p.exists());
        std::fs::create_dir_all(&p).unwrap();
        Scratch(p)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Rig {
    engine: Engine,
    fake: FakeOutput,
    provider: Arc<LocalProvider>,
    events: broadcast::Receiver<UiEvent>,
    rendered: u64,
    _dirs: (Scratch, Scratch),
}

fn device(rate: u32) -> DeviceFormat {
    DeviceFormat {
        rate,
        channels: 2,
        sample: SampleFormat::F32,
        valid_bits: 32,
    }
}

/// A provider over `music`, scanned, on an engine with a fake F32 output at `rate`.
fn rig(music: &Path, rate: u32) -> Rig {
    let fake = FakeOutput::new(device(rate), OutputMode::Exclusive);
    let parts = Engine::new(EngineConfig::default(), Some(Box::new(fake.clone())));
    let mut engine = parts.engine;
    engine.open(&OutputRequest::default()).unwrap();
    let (tx, events) = broadcast::channel(1024);
    let (cache, data) = (Scratch::new("cache"), Scratch::new("data"));
    let provider = LocalProvider::new(
        LocalConfig {
            folders: vec![music.to_str().unwrap().to_owned()],
            cache_dir: cache.0.clone(),
            data_dir: data.0.clone(),
        },
        ProviderContext {
            source: parts.source,
            events: tx,
        },
    );
    assert!(provider.wait_idle(Duration::from_secs(20)));
    let provider = Arc::new(provider);
    Rig {
        engine,
        fake,
        provider,
        events,
        rendered: 0,
        _dirs: (cache, data),
    }
}

impl Rig {
    /// Render what the source has committed, until `frames` frames came out (stereo f32).
    fn collect(&mut self, frames: usize) -> Vec<f32> {
        let mut out = Vec::with_capacity(frames * 2);
        let deadline = Instant::now() + Duration::from_secs(20);
        while out.len() < frames * 2 {
            assert!(
                Instant::now() < deadline,
                "collect: only {} frames",
                out.len() / 2
            );
            let avail = self.engine.written_frames().saturating_sub(self.rendered);
            let want = (frames - out.len() / 2) as u64;
            let n = avail.min(want).min(4096) as usize;
            if n == 0 {
                std::thread::sleep(Duration::from_millis(1));
                continue;
            }
            out.extend(self.fake.render_f32(n));
            self.rendered += n as u64;
        }
        out
    }

    /// Render `frames` frames as callbacks of 1 024 (a device that keeps asking).
    fn render_frames(&mut self, frames: usize) -> Vec<f32> {
        let mut out = Vec::with_capacity(frames * 2);
        while out.len() < frames * 2 {
            out.extend(self.fake.render_f32(1_024));
            std::thread::sleep(Duration::from_micros(200));
        }
        out
    }

    /// Render until `want` (interleaved) has come out whole, at most `max_frames` frames.
    fn render_until(&mut self, want: &[f32], max_frames: usize) -> bool {
        let mut out: Vec<f32> = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(20);
        while out.len() < max_frames * 2 && Instant::now() < deadline {
            out.extend(self.fake.render_f32(1_024));
            let from = out.len().saturating_sub(2 * 1_024 + want.len());
            if out[from..]
                .windows(want.len())
                .any(|w| first_difference(w, want).is_none())
            {
                return true;
            }
            std::thread::sleep(Duration::from_micros(200));
        }
        false
    }

    /// Wait for an event matching `want`, skipping others; the events seen on the way.
    fn wait_event(&mut self, what: &str, want: impl Fn(&UiEvent) -> bool) -> Vec<UiEvent> {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut seen = Vec::new();
        loop {
            match self.events.try_recv() {
                Ok(e) => {
                    let hit = want(&e);
                    seen.push(e);
                    if hit {
                        return seen;
                    }
                }
                Err(_) => {
                    assert!(Instant::now() < deadline, "no {what} event; saw {seen:?}");
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
        }
    }

    fn tracks(&self) -> Vec<Track> {
        let lib = self.provider.library().unwrap();
        block(async {
            let mut out = Vec::new();
            for a in lib.albums(0).await.unwrap().items {
                out.extend(lib.album_tracks(&a.id, 0).await.unwrap().items);
            }
            out
        })
    }

    fn uri(&self, name: &str) -> String {
        self.tracks()
            .into_iter()
            .find(|t| t.name == name)
            .unwrap_or_else(|| panic!("track {name}"))
            .uri
    }
}

fn block<T>(f: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
        .block_on(f)
}

/// `samples` of `bits` bits as the f32 the output gets on a bit-perfect path.
fn expected(samples: &[i32], bits: u32) -> Vec<f32> {
    let scale = (1i64 << (bits - 1)) as f64;
    samples
        .iter()
        .map(|&s| (f64::from(s) / scale) as f32)
        .collect()
}

fn first_difference(a: &[f32], b: &[f32]) -> Option<usize> {
    a.iter()
        .zip(b)
        .position(|(x, y)| x.to_bits() != y.to_bits())
}

#[test]
fn a_24_192_flac_reaches_the_output_bit_exact() {
    let music = Scratch::new("music");
    let frames = 4096 * 3 + 501;
    let src = signal(frames, 2, 24);
    std::fs::write(
        music.0.join("hires.flac"),
        flac(192_000, 24, 2, &src, &[("TITLE", "Hires")], None),
    )
    .unwrap();
    let mut r = rig(&music.0, 192_000);
    let uri = r.uri("Hires");
    block(r.provider.playback().load(&uri, true, 0)).unwrap();
    let out = r.collect(frames);
    assert_eq!(first_difference(&out, &expected(&src, 24)), None);
    r.wait_event("end", |e| matches!(e, UiEvent::EndOfTrack { .. }));
    let d = r.engine.monitor().diagnostics();
    assert_eq!(d.source.codec.as_deref(), Some("FLAC"));
    assert_eq!(d.source.bit_depth, Some(24));
    assert_eq!(d.source.lossless, Some(true));
    assert_eq!(
        r.provider.playback().source_format().as_deref(),
        None,
        "nothing plays after the end"
    );
}

#[test]
fn a_16_bit_wav_reports_pcm_16_lossless() {
    let music = Scratch::new("music");
    // Longer than the engine's 250 ms ring, so the decoder is still mid-file (and the label still
    // set) when it is read below; a file that fits the ring can end before the read.
    let src = signal(44_100, 2, 16);
    std::fs::write(music.0.join("cd.wav"), wav(44_100, 16, 2, &src)).unwrap();
    let mut r = rig(&music.0, 44_100);
    let uri = r.uri("cd");
    block(r.provider.playback().load(&uri, true, 0)).unwrap();
    assert_eq!(
        r.provider.playback().source_format().as_deref(),
        Some("PCM 16-bit 44.1 kHz")
    );
    let out = r.collect(4410);
    assert_eq!(
        first_difference(&out, &expected(&src, 16)[..out.len()]),
        None
    );
    let d = r.engine.monitor().diagnostics();
    assert_eq!(d.source.codec.as_deref(), Some("PCM"));
    assert_eq!(d.source.bit_depth, Some(16));
    assert_eq!(d.source.lossless, Some(true));
}

/// Two FLAC tracks of one album at 44.1/16, A of `a_frames`.
fn two_tracks(music: &Path, a_frames: usize) -> (Vec<i32>, Vec<i32>) {
    let a = signal(a_frames, 2, 16);
    let b: Vec<i32> = signal(7_000, 2, 16).iter().map(|s| s / 2 + 3).collect();
    for (name, n, s) in [("a.flac", "1", &a), ("b.flac", "2", &b)] {
        let title = if n == "1" { "A" } else { "B" };
        std::fs::write(
            music.join(name),
            flac(
                44_100,
                16,
                2,
                s,
                &[("TITLE", title), ("ALBUM", "Two"), ("TRACKNUMBER", n)],
                None,
            ),
        )
        .unwrap();
    }
    (a, b)
}

#[test]
fn gapless_with_preload_is_frame_exact() {
    let music = Scratch::new("music");
    let (a, b) = two_tracks(&music.0, 10_000);
    let mut r = rig(&music.0, 44_100);
    let (ua, ub) = (r.uri("A"), r.uri("B"));
    // Both tracks are shorter than the ring: preload before A starts, as the player does
    // ten seconds before the end of a longer one.
    block(r.provider.playback().load(&ua, false, 0)).unwrap();
    block(r.provider.playback().preload(&ub)).unwrap();
    block(r.provider.playback().play()).unwrap();
    let out = r.collect(a.len() / 2 + b.len() / 2);
    let mut want = expected(&a, 16);
    want.extend(expected(&b, 16));
    assert_eq!(
        first_difference(&out, &want),
        None,
        "A then B, nothing between"
    );
    r.wait_event(
        "end of A",
        |e| matches!(e, UiEvent::EndOfTrack { uri } if *uri == ua),
    );
    // The player's load of B, which already plays: no interrupt, so nothing is lost.
    block(r.provider.playback().load(&ub, true, 0)).unwrap();
    r.wait_event(
        "B playing",
        |e| matches!(e, UiEvent::Playing { uri, .. } if *uri == ub),
    );
    r.wait_event(
        "end of B",
        |e| matches!(e, UiEvent::EndOfTrack { uri } if *uri == ub),
    );
    assert_eq!(r.engine.written_frames(), r.rendered, "no duplicate frame");
}

#[test]
fn without_preload_b_starts_after_an_interrupt() {
    let music = Scratch::new("music");
    // A outlasts the ring, so it still sounds when B is loaded.
    let (a, b) = two_tracks(&music.0, 44_100 * 20);
    let mut r = rig(&music.0, 44_100);
    let (ua, ub) = (r.uri("A"), r.uri("B"));
    block(r.provider.playback().load(&ua, true, 0)).unwrap();
    let head = r.collect(1_000);
    assert_eq!(first_difference(&head, &expected(&a[..2_000], 16)), None);
    // A still sounds: loading B interrupts it; B's first sample is the next one heard after
    // the flush's fade-in (5 ms).
    block(r.provider.playback().load(&ub, true, 0)).unwrap();
    r.wait_event(
        "B playing",
        |e| matches!(e, UiEvent::Playing { uri, .. } if *uri == ub),
    );
    // The ring still holds A's discarded frames: render until B has come out.
    let tail = expected(&b[2 * 221..2 * 3_000], 16);
    let rest_of_a_and_b = r.render_frames(60_000);
    let found = rest_of_a_and_b
        .windows(tail.len())
        .any(|w| first_difference(w, &tail).is_none());
    assert!(found, "B plays from its start after the interrupt");
    // A fades out over 5 ms after the interrupt; none of its later frames are heard.
    let a_later = expected(&a[2 * 1_500..2 * 1_520], 16);
    assert!(
        !rest_of_a_and_b
            .windows(a_later.len())
            .any(|w| first_difference(w, &a_later).is_none()),
        "A does not go on"
    );
}

#[test]
fn seek_lands_on_the_exact_frame_and_past_the_end_ends() {
    let music = Scratch::new("music");
    let rate = 8_000u32;
    let src = signal(rate as usize * 3, 2, 16);
    std::fs::write(music.0.join("three.wav"), wav(rate, 16, 2, &src)).unwrap();
    let mut r = rig(&music.0, rate);
    let uri = r.uri("three");
    let p = r.provider.clone();
    let pb = p.playback();
    block(pb.load(&uri, false, 0)).unwrap();
    block(pb.seek(1_500)).unwrap();
    r.wait_event("seeked", |e| {
        matches!(
            e,
            UiEvent::Seeked {
                position_ms: 1_500,
                ..
            }
        )
    });
    block(pb.play()).unwrap();
    let out = r.collect(2_000);
    let at = (1.5 * f64::from(rate)).round() as usize;
    let want = expected(&src[2 * at..2 * (at + 2_000)], 16);
    // The flush's 5 ms fade-in shapes the first frames: compare after it.
    let fade = 2 * (rate as usize * 5 / 1000);
    assert_eq!(first_difference(&out[fade..], &want[fade..]), None);
    block(pb.seek(5_000)).unwrap();
    r.wait_event("end", |e| matches!(e, UiEvent::EndOfTrack { .. }));
}

#[test]
fn unknown_and_vanished_files_and_a_corrupt_stream_fail_with_fixed_texts() {
    let music = Scratch::new("music");
    let good = signal(4_000, 2, 16);
    std::fs::write(music.0.join("good.wav"), wav(44_100, 16, 2, &good)).unwrap();
    std::fs::write(music.0.join("gone.wav"), wav(44_100, 16, 2, &good)).unwrap();
    let long = wav(44_100, 16, 2, &signal(200_000, 2, 16));
    std::fs::write(music.0.join("cut.wav"), &long).unwrap();
    let mut r = rig(&music.0, 44_100);
    // Cut after indexing: the header promises more than the file holds, so it ends mid-stream.
    std::fs::write(music.0.join("cut.wav"), &long[..long.len() / 2]).unwrap();
    let (good_uri, gone_uri, cut_uri) = (r.uri("good"), r.uri("gone"), r.uri("cut"));
    let p = r.provider.clone();
    let pb = p.playback();

    let e = block(pb.load("local:track:0000000000000000", true, 0)).unwrap_err();
    assert_eq!(e.0, "this track is not in the library");
    assert!(block(pb.load("../../etc/passwd", true, 0)).is_err());

    std::fs::remove_file(music.0.join("gone.wav")).unwrap();
    assert!(block(pb.load(&gone_uri, true, 0)).is_err());
    r.wait_event(
        "unavailable",
        |e| matches!(e, UiEvent::Unavailable { uri } if *uri == gone_uri),
    );

    block(pb.load(&cut_uri, true, 0)).unwrap();
    let mut seen = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    while !seen
        .iter()
        .any(|e| matches!(e, UiEvent::EndOfTrack { uri } if *uri == cut_uri))
    {
        assert!(Instant::now() < deadline, "the cut file ends: {seen:?}");
        let avail = r.engine.written_frames().saturating_sub(r.rendered);
        if avail > 0 {
            let n = avail.min(4096) as usize;
            r.fake.render_f32(n);
            r.rendered += n as u64;
        }
        while let Ok(e) = r.events.try_recv() {
            seen.push(e);
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        seen.iter().any(|e| matches!(e, UiEvent::Error { message } if message == "this file could not be decoded")),
        "a fixed text: {seen:?}"
    );

    // The thread lives: the next load plays.
    // The cut file's queued tail plays out first (it ended on its own: no interrupt).
    block(pb.load(&good_uri, true, 0)).unwrap();
    assert!(
        r.render_until(&expected(&good[..200], 16), 400_000),
        "the good file plays after the corrupt one"
    );
}

/// Renders the fake output continuously on a thread (a stand-in for a device).
struct Pump {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Pump {
    fn new(fake: FakeOutput) -> Pump {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                fake.render_f32(256);
                std::thread::sleep(Duration::from_millis(1));
            }
        });
        Pump {
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for Pump {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// The uri of the fixture's 40 s file, so a load at 30 s lands inside it.
async fn long_uri(p: &dyn Provider) -> String {
    let lib = p.library().unwrap();
    for a in lib.albums(0).await.unwrap().items {
        for t in lib.album_tracks(&a.id, 0).await.unwrap().items {
            if t.name == "long" {
                return t.uri;
            }
        }
    }
    panic!("long.wav");
}

#[tokio::test(flavor = "multi_thread")]
async fn provider_and_playback_contracts() {
    let music = TempMusicDir::new();
    let r = rig(music.path(), 44_100);
    let _pump = Pump::new(r.fake.clone());
    let p: Arc<dyn Provider> = r.provider.clone();
    let long = long_uri(&*p).await;
    run_provider_contract(p.clone()).await;
    run_provider_playback_contract(p.clone(), Some(&long), "local:track:ffffffffffffffff").await;
}

/// The session contract toggles a walked, not yet liked track (D4, D6: ids are library items only).
#[tokio::test(flavor = "multi_thread")]
async fn provider_session_contract() {
    let music = TempMusicDir::new();
    let r = rig(music.path(), 44_100);
    let p: Arc<dyn Provider> = r.provider.clone();
    let long = long_uri(&*p).await;
    // The session walk needs a liked track.
    p.library().unwrap().set_liked(&long, true).await.unwrap();
    run_provider_session_contract(p.clone(), &[], ("no-such-step", LoginInput::default())).await;
}

#[test]
fn dropping_the_provider_joins_the_decode_thread_within_a_second() {
    let music = Scratch::new("music");
    std::fs::write(
        music.0.join("long.wav"),
        wav(8_000, 16, 2, &signal(8_000 * 30, 2, 16)),
    )
    .unwrap();
    // Nothing renders: the thread blocks in `write` with the ring full.
    let r = rig(&music.0, 8_000);
    let uri = r.uri("long");
    block(r.provider.playback().load(&uri, true, 0)).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    // Drop waits at most 1 s for the thread and returns early only when it has ended.
    let start = Instant::now();
    drop(r.provider);
    assert!(
        start.elapsed() < Duration::from_millis(900),
        "{:?}",
        start.elapsed()
    );
}
