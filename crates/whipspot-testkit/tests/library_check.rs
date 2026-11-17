//! `PlayerAction::CheckLibrary` (spotify-connect step 5) on FakeProvider. Every wait polls with a
//! timeout; no fixed sleep is an assertion.

use std::sync::Arc;
use std::time::{Duration, Instant};

use whipspot_core::engine::{DeviceFormat, OutputMode, OutputRequest, SampleFormat};
use whipspot_core::player::{
    self, EngineSetup, LibraryQuery, PlayState, Player, PlayerAction, PlayerConfig, PlayerHandle,
    PlayerNotice, RequestId,
};
use whipspot_core::provider::{ProviderRegistry, Track};
use whipspot_testkit::{FakeOutput, FakeProvider, Gate};

const TIMEOUT: Duration = Duration::from_secs(5);

fn start(providers: Vec<Arc<FakeProvider>>) -> PlayerHandle {
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
    let (_ctx, wiring) = player::wiring(engine, 64);
    let mut reg = ProviderRegistry::new();
    for p in providers {
        reg.register(p).unwrap();
    }
    Player::start(
        reg,
        wiring,
        PlayerConfig::default(),
        &tokio::runtime::Handle::current(),
    )
}

async fn until(what: &str, f: impl Fn() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(start.elapsed() < TIMEOUT, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

fn count(p: &FakeProvider) -> usize {
    p.calls().iter().filter(|c| *c == "check_changes").count()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn check_library_calls_each_library_once_and_skips_the_rest() {
    let a = Arc::new(FakeProvider::new("fakea", true));
    let b = Arc::new(FakeProvider::new("fakeb", true));
    let none = Arc::new(FakeProvider::new("fakec", false));
    let h = start(vec![a.clone(), b.clone(), none.clone()]);
    h.dispatch(PlayerAction::CheckLibrary);
    until("both checks", || count(&a) == 1 && count(&b) == 1).await;
    // A sentinel after the action: the loop has handled everything before it.
    h.dispatch(PlayerAction::Library {
        request: RequestId(1),
        provider: "nope".into(),
        query: LibraryQuery::Playlists { offset: 0 },
    });
    let start = Instant::now();
    loop {
        if h.drain_notices().iter().any(
            |n| matches!(n, PlayerNotice::LibraryPage { request, .. } if *request == RequestId(1)),
        ) {
            break;
        }
        assert!(start.elapsed() < TIMEOUT, "sentinel never came");
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    assert_eq!((count(&a), count(&b)), (1, 1));
    assert!(
        none.calls().is_empty(),
        "a provider with no library is skipped"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_check_that_never_ends_does_not_delay_a_play() {
    let gate = Gate::new();
    let a = Arc::new(FakeProvider::new("fakea", true).gate_check_changes(gate.clone()));
    let h = start(vec![a.clone()]);
    h.dispatch(PlayerAction::CheckLibrary);
    until("the check started", || count(&a) == 1).await;
    h.dispatch(PlayerAction::PlayItem {
        context: vec![Track {
            uri: "fakea:track:1".into(),
            name: "one".into(),
            artist: "artist".into(),
            album: "album".into(),
            duration_ms: 180_000,
            images: vec![],
            year: None,
        }],
        index: 0,
    });
    until("playing", || h.state().play_state == PlayState::Playing).await;
    // The gate stayed shut the whole time.
    assert!(!gate.is_open());
    gate.open();
}
