//! The art pull of C1 (MD62, MD63) on FakeArt. Every wait polls with a timeout; no fixed sleep
//! is an assertion.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use whipspot_core::engine::{DeviceFormat, OutputMode, OutputRequest, SampleFormat};
use whipspot_core::player::{
    self, ArtStatus, ArtUrgency, EngineSetup, Player, PlayerConfig, PlayerHandle,
};
use whipspot_core::provider::{ImageRef, ProviderRegistry};
use whipspot_testkit::{FakeArt, FakeOutput, FakeProvider, cover, demo_images};

const TIMEOUT: Duration = Duration::from_secs(5);

struct Rig {
    h: PlayerHandle,
    art: Arc<FakeArt>,
    wakes: Arc<AtomicUsize>,
}

fn start(art: Option<Arc<FakeArt>>) -> (PlayerHandle, Arc<AtomicUsize>) {
    let fake = FakeOutput::new(
        DeviceFormat {
            rate: 44_100,
            channels: 2,
            sample: SampleFormat::F32,
            valid_bits: 32,
        },
        OutputMode::Shared,
    );
    let engine = EngineSetup::Output {
        output: Box::new(fake),
        request: OutputRequest::default(),
    };
    let (ctx, wiring) = player::wiring(engine, 64);
    let mut reg = ProviderRegistry::new();
    reg.register(Arc::new(FakeProvider::new("fakea", true).with_context(ctx)))
        .unwrap();
    let mut config = PlayerConfig::default();
    if let Some(art) = art {
        config = config.with_art(art);
    }
    let h = Player::start(reg, wiring, config, &tokio::runtime::Handle::current());
    let wakes = Arc::new(AtomicUsize::new(0));
    let w = wakes.clone();
    h.set_wake(Arc::new(move || {
        w.fetch_add(1, Ordering::SeqCst);
    }));
    (h, wakes)
}

fn rig() -> Rig {
    let art = Arc::new(FakeArt::new());
    let (h, wakes) = start(Some(art.clone()));
    Rig { h, art, wakes }
}

/// The 64 px demo ref of `key`.
fn small(key: &str) -> ImageRef {
    demo_images(key).pop().unwrap()
}

async fn wait(what: &str, f: impl Fn() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(start.elapsed() < TIMEOUT, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

/// Ask for `image` (as the UI does each frame) until it is no longer Pending.
async fn pull(h: &PlayerHandle, image: &ImageRef, urgency: ArtUrgency) -> ArtStatus {
    let start = Instant::now();
    loop {
        match h.art(image, urgency) {
            ArtStatus::Pending => {}
            done => return done,
        }
        assert!(start.elapsed() < TIMEOUT, "timed out pulling {}", image.url);
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

fn expected(key: &str) -> ArtStatus {
    let rgba = cover::render(key, 64);
    ArtStatus::Ready(Arc::new(
        whipspot_core::art::ArtImage::new(64, 64, rgba).unwrap(),
    ))
}

// A1
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a1_no_art_source_answers_failed() {
    let (h, _) = start(None);
    assert_eq!(h.art(&small("k"), ArtUrgency::Current), ArtStatus::Failed);
    assert_eq!(h.art(&small("k"), ArtUrgency::Visible), ArtStatus::Failed);
}

// A2
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a2_pending_then_ready_after_the_wake_then_handed_out_once() {
    let r = rig();
    let image = small("cover-a");
    let gate = r.art.gate();
    assert_eq!(r.h.art(&image, ArtUrgency::Current), ArtStatus::Pending);
    wait("the fetch to start", || r.art.calls().len() == 1).await;
    let before = r.wakes.load(Ordering::SeqCst);
    gate.open();
    wait("the wake", || r.wakes.load(Ordering::SeqCst) > before).await;
    assert_eq!(
        pull(&r.h, &image, ArtUrgency::Current).await,
        expected("cover-a")
    );
    assert_eq!(r.h.art(&image, ArtUrgency::Current), ArtStatus::Pending);
}

// A3
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a3_ten_asks_before_completion_make_one_fetch() {
    let r = rig();
    let gate = r.art.gate();
    let image = small("once");
    for _ in 0..10 {
        assert_eq!(r.h.art(&image, ArtUrgency::Visible), ArtStatus::Pending);
    }
    wait("the fetch to start", || r.art.calls().len() == 1).await;
    for _ in 0..10 {
        assert_eq!(r.h.art(&image, ArtUrgency::Visible), ArtStatus::Pending);
    }
    gate.open();
    assert_eq!(
        pull(&r.h, &image, ArtUrgency::Visible).await,
        expected("once")
    );
    assert_eq!(r.art.calls(), [image.url]);
}

// A4
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a4_at_most_four_fetches_at_once() {
    let r = rig();
    let gate = r.art.gate();
    let images: Vec<_> = (0..12).map(|n| small(&format!("k{n}"))).collect();
    for i in &images {
        assert_eq!(r.h.art(i, ArtUrgency::Visible), ArtStatus::Pending);
    }
    wait("four fetches", || r.art.calls().len() == 4).await;
    // Renew every want for a while: no fifth fetch starts behind the closed gate.
    for _ in 0..20 {
        for i in &images {
            assert_eq!(r.h.art(i, ArtUrgency::Visible), ArtStatus::Pending);
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    assert_eq!(r.art.calls().len(), 4);
    assert_eq!(r.art.max_in_flight(), 4);
    gate.open();
    let mut ready = vec![false; images.len()];
    let start = Instant::now();
    while ready.iter().any(|r| !r) {
        for (n, i) in images.iter().enumerate() {
            if !ready[n] {
                match r.h.art(i, ArtUrgency::Visible) {
                    ArtStatus::Pending => {}
                    ArtStatus::Ready(img) => {
                        assert_eq!(img.rgba(), cover::render(&format!("k{n}"), 64));
                        ready[n] = true;
                    }
                    ArtStatus::Failed => panic!("k{n} failed"),
                }
            }
        }
        assert!(start.elapsed() < TIMEOUT, "timed out: {ready:?}");
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    assert_eq!(r.art.calls().len(), 12);
    assert_eq!(r.art.max_in_flight(), 4);
}

// A5. On one thread: FakeArt records a fetch when its task is first polled, and fetches the
// worker starts together would be polled in any order by several threads; on one they run in the
// order the worker started them.
#[tokio::test(flavor = "current_thread")]
async fn a5_a_current_want_is_fetched_before_visible_ones() {
    let r = rig();
    let gate = r.art.gate();
    let first: Vec<_> = (0..4).map(|n| small(&format!("f{n}"))).collect();
    for i in &first {
        r.h.art(i, ArtUrgency::Visible);
    }
    wait("four fetches", || r.art.calls().len() == 4).await;
    let visible: Vec<_> = (0..5).map(|n| small(&format!("v{n}"))).collect();
    for i in &visible {
        r.h.art(i, ArtUrgency::Visible);
    }
    let current = small("current");
    r.h.art(&current, ArtUrgency::Current);
    gate.open();
    wait("ten fetches", || r.art.calls().len() == 10).await;
    assert_eq!(r.art.calls()[4], current.url);
}
