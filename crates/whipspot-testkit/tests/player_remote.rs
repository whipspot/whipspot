//! The player's remote mode (C1, `UiEvent::RemoteStarted` .. `RemoteEnded`) on the testkit fakes:
//! provider A plays locally, provider R has a remote whose audio writes as `R_OWNER`; both write
//! into one engine. Every wait polls with a timeout; a pause before an assertion only gives a
//! wrong change the time to show.

use std::sync::Arc;
use std::time::{Duration, Instant};

use whipspot_core::engine::{
    DeviceFormat, OutputMode, OutputRequest, SampleFormat, SourceInfo, SourceWriter, StreamFormat,
};
use whipspot_core::events::{RemotePlay, UiEvent};
use whipspot_core::player::{
    self, EngineSetup, PlayState, Player, PlayerAction, PlayerConfig, PlayerHandle, PlayerNotice,
    PlayerState, texts,
};
use whipspot_core::provider::{ProviderContext, ProviderRegistry, Track};
use whipspot_testkit::{FakeOutput, FakeProvider};

const TIMEOUT: Duration = Duration::from_secs(5);
const RATE: u32 = 44_100;
const R: &str = "faker";
const R_OWNER: &str = "faker.remote";
const R_FORMAT: &str = "fake 320 kbps";

struct Rig {
    h: PlayerHandle,
    ctx: ProviderContext,
    a: Arc<FakeProvider>,
    r: Arc<FakeProvider>,
    fake: FakeOutput,
}

fn rig() -> Rig {
    let fake = FakeOutput::new(
        DeviceFormat {
            rate: RATE,
            channels: 2,
            sample: SampleFormat::F32,
            valid_bits: 32,
        },
        OutputMode::Shared,
    );
    let engine = EngineSetup::Output {
        output: Box::new(fake.clone()),
        request: OutputRequest::default(),
    };
    let (ctx, wiring) = player::wiring(engine, 64);
    let a = Arc::new(FakeProvider::new("fakea", true).with_context(ctx.clone()));
    let r = Arc::new(
        FakeProvider::new(R, true)
            .with_context(ctx.clone())
            .with_remote(R_OWNER)
            .with_source_format(R_FORMAT),
    );
    // Remotes the player must refuse: one writing as its own provider, one as another provider.
    let own = Arc::new(FakeProvider::new("fakec", true).with_remote("fakec"));
    let other = Arc::new(FakeProvider::new("faked", true).with_remote("fakea"));
    let mut reg = ProviderRegistry::new();
    reg.register(a.clone()).unwrap();
    reg.register(r.clone()).unwrap();
    reg.register(own).unwrap();
    reg.register(other).unwrap();
    let h = Player::start(
        reg,
        wiring,
        PlayerConfig::default(),
        &tokio::runtime::Handle::current(),
    );
    Rig { h, ctx, a, r, fake }
}

fn track(uri: &str) -> Track {
    Track {
        uri: uri.into(),
        name: uri.into(),
        artist: "artist".into(),
        album: "album".into(),
        duration_ms: 180_000,
        images: vec![],
        year: None,
    }
}

fn tracks(uris: &[&str]) -> Vec<Track> {
    uris.iter().map(|u| track(u)).collect()
}

async fn wait_for(h: &PlayerHandle, what: &str, f: impl Fn(&PlayerState) -> bool) {
    let start = Instant::now();
    while !f(&h.state()) {
        assert!(
            start.elapsed() < TIMEOUT,
            "timed out waiting for {what}: {:?}",
            h.state()
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

async fn wait_calls(p: &FakeProvider, what: &str, f: impl Fn(&[String]) -> bool) {
    let start = Instant::now();
    while !f(&p.calls()) {
        assert!(
            start.elapsed() < TIMEOUT,
            "timed out waiting for {what}: {:?}",
            p.calls()
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

fn current_uri(s: &PlayerState) -> Option<&str> {
    s.current.as_ref().map(|c| c.track.uri.as_str())
}

async fn play_item(r: &Rig, uris: &[&str], index: usize) {
    r.h.dispatch(PlayerAction::PlayItem {
        context: tracks(uris),
        index,
    });
    let want = uris[index].to_owned();
    wait_for(&r.h, "the item to play", |s| {
        current_uri(s) == Some(want.as_str()) && s.play_state == PlayState::Playing
    })
    .await;
}

/// Every provider event sent before this one has been handled: events arrive in order, and this
/// one posts a notice.
async fn settle(r: &Rig) {
    r.a.emit(UiEvent::LibraryChanged {
        provider: "settle".into(),
    });
    let start = Instant::now();
    loop {
        let seen = r.h.drain_notices().into_iter().any(
            |n| matches!(n, PlayerNotice::LibraryChanged { provider } if provider == "settle"),
        );
        if seen {
            return;
        }
        assert!(start.elapsed() < TIMEOUT, "timed out settling the events");
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

/// R's remote takes over; returns once the transport has run the takeover (the volume it sends
/// after it has reached R).
async fn take_over(r: &Rig) {
    let before = r.r.calls().len();
    r.r.emit(UiEvent::RemoteStarted { provider: R.into() });
    wait_for(&r.h, "remote mode", |s| s.remote == Some(R)).await;
    wait_calls(&r.r, "the takeover's volume", |c| {
        c[before..].iter().any(|l| l.starts_with("remote volume"))
    })
    .await;
}

fn remote_track(uri: &str) -> UiEvent {
    UiEvent::RemoteTrack {
        provider: R.into(),
        track: track(uri),
    }
}

fn remote_state(uri: &str, state: RemotePlay, position_ms: u32) -> UiEvent {
    UiEvent::RemoteState {
        provider: R.into(),
        uri: uri.into(),
        state,
        position_ms,
    }
}

/// R's remote plays `uri`.
async fn remote_plays(r: &Rig, uri: &str) {
    r.r.emit(remote_track(uri));
    r.r.emit(remote_state(uri, RemotePlay::Playing, 0));
    wait_for(&r.h, "the remote track to play", |s| {
        current_uri(s) == Some(uri) && s.play_state == PlayState::Playing
    })
    .await;
}

fn pcm(rate: u32) -> StreamFormat {
    StreamFormat {
        rate,
        channels: 2,
        source: SourceInfo {
            codec: "pcm",
            bits: Some(16),
            bitrate_kbps: None,
            lossless: true,
            replay_gain: None,
        },
    }
}

/// The source handle of engine owner `id`.
fn handle(r: &Rig, id: &str) -> SourceWriter {
    r.ctx.for_owner(id).source
}

/// Whether audio written through `w` (a track start and 100 ms of a level) comes out of the
/// output: the engine accepts only its owner's writes (D13), a dropped write renders silence.
async fn heard(r: &Rig, w: &SourceWriter) -> bool {
    let frames = RATE as usize / 10;
    let w = w.clone();
    let result = tokio::task::spawn_blocking(move || {
        w.start_track(pcm(RATE))?;
        w.write(&vec![0.5; 2 * frames])
    })
    .await
    .unwrap();
    assert_eq!(result, Ok(()));
    r.fake.render_f32(frames).iter().any(|x| x.abs() > 1e-4)
}

fn count(calls: &[String], line: &str) -> usize {
    calls.iter().filter(|l| *l == line).count()
}

// (1)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_takeover_stops_the_playing_provider_and_takes_the_engine() {
    let r = rig();
    play_item(&r, &["fakea:track:1"], 0).await;
    take_over(&r).await;
    wait_calls(&r.a, "A stopped", |c| c.last().is_some_and(|l| l == "stop")).await;
    assert_eq!(r.a.calls(), ["load fakea:track:1 true 0", "stop"]);
    let s = r.h.state();
    assert_eq!(s.remote, Some(R));
    // A's late writes are dropped; the remote's are heard.
    assert!(!heard(&r, &handle(&r, "fakea")).await);
    assert!(heard(&r, &handle(&r, R_OWNER)).await);
}

// (2)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn in_remote_mode_the_transport_buttons_go_to_the_remote() {
    let r = rig();
    play_item(&r, &["fakea:track:1", "fakea:track:2"], 0).await;
    take_over(&r).await;
    remote_plays(&r, "faker:track:7").await;
    for a in [
        PlayerAction::Play,
        PlayerAction::Pause,
        PlayerAction::Toggle,
        PlayerAction::Next,
        PlayerAction::Previous,
        PlayerAction::Seek { position_ms: 5000 },
        PlayerAction::SetVolume(40),
    ] {
        r.h.dispatch(a);
    }
    wait_calls(&r.r, "the volume forwarded", |c| {
        c.last().is_some_and(|l| l == "remote volume 40")
    })
    .await;
    assert_eq!(
        r.r.calls(),
        [
            "remote volume 100",
            "remote play",
            "remote pause",
            // Toggle while the remote plays.
            "remote pause",
            "remote next",
            "remote previous",
            "remote seek 5000",
            "remote volume 40",
        ]
    );
    assert_eq!(r.a.calls(), ["load fakea:track:1 true 0", "stop"]);
    let s = r.h.state();
    assert_eq!(s.volume, 40);
    assert_eq!(*s.queue.items, tracks(&["fakea:track:1", "fakea:track:2"]));
    assert_eq!(s.queue.current, None);
    assert_eq!(s.remote, Some(R));
}

// (3)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_events_set_what_shows_and_plain_events_are_ignored() {
    let r = rig();
    take_over(&r).await;
    r.r.emit(remote_track("faker:track:7"));
    wait_for(&r.h, "the remote track", |s| {
        current_uri(s) == Some("faker:track:7")
    })
    .await;
    let now = r.h.state().current.clone().unwrap();
    assert_eq!(now.provider, R);
    assert_eq!(now.source_format.as_deref(), Some(R_FORMAT));
    r.r.emit(remote_state("faker:track:7", RemotePlay::Playing, 1000));
    wait_for(&r.h, "playing", |s| s.play_state == PlayState::Playing).await;
    r.r.emit(remote_state("faker:track:7", RemotePlay::Paused, 1000));
    wait_for(&r.h, "paused", |s| s.play_state == PlayState::Paused).await;
    // The provider's plain events for the same uri change nothing; nor does a remote state for
    // another track.
    r.r.emit(UiEvent::Playing {
        uri: "faker:track:7".into(),
        position_ms: 2000,
    });
    r.r.emit(remote_state("faker:track:8", RemotePlay::Playing, 0));
    settle(&r).await;
    let s = r.h.state();
    assert_eq!(s.play_state, PlayState::Paused);
    assert_eq!(s.position_ms, 1000);
    assert_eq!(current_uri(&s), Some("faker:track:7"));
}

// (4)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_end_of_track_in_remote_mode_loads_nothing() {
    let r = rig();
    play_item(&r, &["fakea:track:1", "fakea:track:2"], 0).await;
    take_over(&r).await;
    // Before the remote's track shows, the local one still does.
    r.a.emit(UiEvent::EndOfTrack {
        uri: "fakea:track:1".into(),
    });
    r.a.emit(UiEvent::Unavailable {
        uri: "fakea:track:1".into(),
    });
    remote_plays(&r, "faker:track:7").await;
    r.r.emit(UiEvent::EndOfTrack {
        uri: "faker:track:7".into(),
    });
    r.r.emit(UiEvent::Unavailable {
        uri: "faker:track:7".into(),
    });
    settle(&r).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(r.a.calls(), ["load fakea:track:1 true 0", "stop"]);
    assert!(
        !r.r.calls().iter().any(|l| l.starts_with("load")),
        "{:?}",
        r.r.calls()
    );
    let s = r.h.state();
    assert_eq!(s.remote, Some(R));
    assert_eq!(s.play_state, PlayState::Playing);
    assert_eq!(current_uri(&s), Some("faker:track:7"));
    assert!(
        !r.h.drain_notices()
            .contains(&PlayerNotice::Error(texts::TRACK_UNAVAILABLE))
    );
}

// (5)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_local_play_releases_the_remote_before_it_loads() {
    let r = rig();
    take_over(&r).await;
    remote_plays(&r, "faker:track:7").await;
    // R's own track, played locally: the remote lets go first, on the same transport queue.
    play_item(&r, &["faker:track:1"], 0).await;
    let calls = r.r.calls();
    let release = calls.iter().position(|l| l == "release").unwrap();
    let load = calls
        .iter()
        .position(|l| l == "load faker:track:1 true 0")
        .unwrap();
    assert!(release < load, "{calls:?}");
    assert_eq!(count(&calls, "release"), 1);
    let s = r.h.state();
    assert_eq!(s.remote, None);
    assert_eq!(s.current.as_ref().unwrap().provider, R);
    // The engine is the loading provider's: the remote's late writes are dropped.
    assert!(!heard(&r, &handle(&r, R_OWNER)).await);
    assert!(heard(&r, &handle(&r, R)).await);

    // Again, with another provider's track.
    take_over(&r).await;
    play_item(&r, &["fakea:track:3"], 0).await;
    assert_eq!(count(&r.r.calls(), "release"), 2);
    assert!(!heard(&r, &handle(&r, R_OWNER)).await);
    assert!(heard(&r, &handle(&r, "fakea")).await);
    // Transport goes to A again.
    r.h.dispatch(PlayerAction::Pause);
    wait_calls(&r.a, "pause on A", |c| {
        c.last().is_some_and(|l| l == "pause")
    })
    .await;
}

// (6)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_ended_leaves_remote_mode_and_later_remote_events_are_ignored() {
    let r = rig();
    take_over(&r).await;
    remote_plays(&r, "faker:track:7").await;
    r.r.emit(UiEvent::RemoteEnded { provider: R.into() });
    wait_for(&r.h, "remote mode left", |s| {
        s.remote.is_none() && s.play_state == PlayState::Stopped
    })
    .await;
    r.r.emit(remote_track("faker:track:8"));
    r.r.emit(remote_state("faker:track:7", RemotePlay::Playing, 0));
    r.r.emit(UiEvent::RemoteVolume {
        provider: R.into(),
        volume: 10,
    });
    settle(&r).await;
    let s = r.h.state();
    assert_eq!(current_uri(&s), Some("faker:track:7"));
    assert_eq!(s.play_state, PlayState::Stopped);
    assert_eq!(s.volume, 100);
    // The remote ended by itself: nothing to release.
    assert_eq!(count(&r.r.calls(), "release"), 0);
    // A new takeover listens again.
    take_over(&r).await;
    remote_plays(&r, "faker:track:8").await;
}

// (7)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_takeover_during_a_pending_load_commits_nothing_and_stops_what_loaded() {
    let r = rig();
    r.a.set_load_delay(Some(Duration::from_millis(150)));
    r.h.dispatch(PlayerAction::PlayItem {
        context: tracks(&["fakea:track:1"]),
        index: 0,
    });
    wait_calls(&r.a, "the load to start", |c| !c.is_empty()).await;
    take_over(&r).await;
    wait_calls(&r.a, "A stopped", |c| c.last().is_some_and(|l| l == "stop")).await;
    // The load's report reached the loop before the takeover's volume reached R.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(r.a.calls(), ["load fakea:track:1 true 0", "stop"]);
    let s = r.h.state();
    assert_eq!(s.remote, Some(R));
    assert_eq!(s.current, None);
    assert!(s.queue.items.is_empty());
    assert_eq!(s.play_state, PlayState::Loading);
    assert!(!heard(&r, &handle(&r, "fakea")).await);
    assert!(heard(&r, &handle(&r, R_OWNER)).await);
}

// (8)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_remote_volume_is_applied_and_not_sent_back() {
    let r = rig();
    r.h.dispatch(PlayerAction::SetVolume(70));
    wait_for(&r.h, "volume 70", |s| s.volume == 70).await;
    take_over(&r).await;
    r.r.emit(UiEvent::RemoteVolume {
        provider: R.into(),
        volume: 30,
    });
    wait_for(&r.h, "volume 30", |s| s.volume == 30).await;
    settle(&r).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    let volumes: Vec<String> =
        r.r.calls()
            .into_iter()
            .filter(|l| l.starts_with("remote volume"))
            .collect();
    // Exactly one, Whipspot's level at the takeover (D5).
    assert_eq!(volumes, ["remote volume 70"]);
}

// (9)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_in_remote_mode_releases_the_remote() {
    let r = rig();
    take_over(&r).await;
    remote_plays(&r, "faker:track:7").await;
    r.h.dispatch(PlayerAction::Stop);
    wait_for(&r.h, "stopped, remote mode left", |s| {
        s.remote.is_none() && s.play_state == PlayState::Stopped
    })
    .await;
    wait_calls(&r.r, "the release", |c| {
        c.last().is_some_and(|l| l == "release")
    })
    .await;
    assert_eq!(count(&r.r.calls(), "release"), 1);
    assert!(!r.r.calls().iter().any(|l| l == "stop"));
}

// (10)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_remote_start_the_player_cannot_honour_changes_nothing() {
    let r = rig();
    play_item(&r, &["fakea:track:1"], 0).await;
    let before = r.h.state();
    for provider in ["fakea", "nope", "fakec", "faked"] {
        r.a.emit(UiEvent::RemoteStarted {
            provider: provider.into(),
        });
    }
    // Remote events with no remote mode.
    r.r.emit(remote_track("faker:track:7"));
    r.r.emit(UiEvent::RemoteVolume {
        provider: R.into(),
        volume: 5,
    });
    settle(&r).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    let s = r.h.state();
    assert_eq!(s.revision, before.revision);
    assert_eq!(s.remote, None);
    assert_eq!(r.a.calls(), ["load fakea:track:1 true 0"]);
    assert!(r.r.calls().is_empty(), "{:?}", r.r.calls());
    assert!(heard(&r, &handle(&r, "fakea")).await);
}

// After remote mode the shown track is loaded nowhere: the transport buttons still work.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn after_remote_ended_play_loads_the_track_shown() {
    let r = rig();
    take_over(&r).await;
    remote_plays(&r, "faker:track:7").await;
    r.r.emit(UiEvent::RemoteEnded { provider: R.into() });
    wait_for(&r.h, "remote mode left", |s| {
        s.remote.is_none() && s.play_state == PlayState::Stopped
    })
    .await;
    // Pause has nothing to pause; Play loads the shown track from the start.
    r.h.dispatch(PlayerAction::Pause);
    r.h.dispatch(PlayerAction::Play);
    wait_for(&r.h, "the shown track to play", |s| {
        current_uri(s) == Some("faker:track:7") && s.play_state == PlayState::Playing
    })
    .await;
    let loads: Vec<String> =
        r.r.calls()
            .into_iter()
            .filter(|l| l.starts_with("load") || l == "pause")
            .collect();
    assert_eq!(loads, ["load faker:track:7 true 0"]);
    // Loaded locally: the provider owns the engine, the remote's writes are dropped.
    assert!(heard(&r, &handle(&r, R)).await);
    assert!(!heard(&r, &handle(&r, R_OWNER)).await);
    // And the buttons reach it as usual.
    r.h.dispatch(PlayerAction::Toggle);
    wait_for(&r.h, "paused", |s| s.play_state == PlayState::Paused).await;
    wait_calls(&r.r, "the pause", |c| {
        c.last().is_some_and(|l| l == "pause")
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn after_stop_in_remote_mode_seek_then_toggle_plays_from_there() {
    let r = rig();
    take_over(&r).await;
    remote_plays(&r, "faker:track:7").await;
    r.h.dispatch(PlayerAction::Stop);
    wait_for(&r.h, "stopped, remote mode left", |s| {
        s.remote.is_none() && s.play_state == PlayState::Stopped
    })
    .await;
    r.h.dispatch(PlayerAction::Seek { position_ms: 5000 });
    wait_for(&r.h, "the position shown", |s| s.position_ms == 5000).await;
    assert_eq!(r.h.state().play_state, PlayState::Stopped);
    r.h.dispatch(PlayerAction::Toggle);
    wait_for(&r.h, "the shown track to play", |s| {
        current_uri(s) == Some("faker:track:7") && s.play_state == PlayState::Playing
    })
    .await;
    assert_eq!(
        count(&r.r.calls(), "load faker:track:7 true 5000"),
        1,
        "{:?}",
        r.r.calls()
    );
    assert!(!r.r.calls().iter().any(|l| l.starts_with("seek")));
}

/// A provider's own status change (`UiEvent::AuthChanged`, the Connect speaker's phase) makes the
/// player read that provider's status again, and only that provider's.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auth_changed_reads_that_providers_status_again() {
    let r = rig();
    let auth_of = |s: &PlayerState, id: &str| {
        s.providers
            .iter()
            .find(|e| e.info.id == id)
            .is_some_and(|e| e.auth.is_some())
    };
    assert!(!auth_of(&r.h.state(), R), "no status read before");
    let _ = r
        .ctx
        .events
        .send(UiEvent::AuthChanged { provider: R.into() });
    wait_for(&r.h, "the remote provider's status", |s| auth_of(s, R)).await;
    assert!(
        !auth_of(&r.h.state(), "fakea"),
        "the other provider is not read"
    );
}
