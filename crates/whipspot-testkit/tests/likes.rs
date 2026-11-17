//! Likes scheduling of C1 (MD69, MD70) on FakeProvider. Every wait polls with a timeout; no
//! fixed sleep is an assertion.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::broadcast;
use whipspot_core::engine::{DeviceFormat, OutputMode, OutputRequest, SampleFormat};
use whipspot_core::events::UiEvent;
use whipspot_core::player::{
    self, EngineSetup, LibraryQuery, PlayState, Player, PlayerAction, PlayerConfig, PlayerHandle,
    PlayerNotice, PlayerState, RequestId, texts,
};
use whipspot_core::provider::{LibErrorKind, ProviderRegistry, Track};
use whipspot_testkit::{FakeOutput, FakeProvider, Gate};

const TIMEOUT: Duration = Duration::from_secs(5);

struct Rig {
    h: PlayerHandle,
    p: Arc<FakeProvider>,
    /// The providers' UI event sender (what a provider reports through).
    events: broadcast::Sender<UiEvent>,
    /// Every notice drained so far, in order.
    notices: Vec<PlayerNotice>,
    /// `notices` before this index were returned by [`Rig::notice`] or passed over by it.
    cursor: usize,
}

fn rig(make: impl FnOnce(FakeProvider) -> FakeProvider) -> Rig {
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
    let events = ctx.events.clone();
    let p = Arc::new(make(FakeProvider::new("fakea", true).with_context(ctx)));
    let mut reg = ProviderRegistry::new();
    reg.register(p.clone()).unwrap();
    let h = Player::start(
        reg,
        wiring,
        PlayerConfig::default(),
        &tokio::runtime::Handle::current(),
    );
    Rig {
        h,
        p,
        events,
        notices: vec![],
        cursor: 0,
    }
}

fn t(n: u32) -> String {
    format!("fakea:track:{n}")
}

fn writable(s: &PlayerState) -> bool {
    s.providers[0].likes_writable
}

impl Rig {
    /// Drain notices until one after the last returned matches `f`; returns it.
    async fn notice(&mut self, f: impl Fn(&PlayerNotice) -> bool) -> PlayerNotice {
        let start = Instant::now();
        loop {
            self.notices.extend(self.h.drain_notices());
            if let Some(i) = self.notices[self.cursor..].iter().position(&f) {
                self.cursor += i + 1;
                return self.notices[self.cursor - 1].clone();
            }
            assert!(
                start.elapsed() < TIMEOUT,
                "timed out waiting for a notice: {:?}",
                self.notices
            );
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }

    /// A notice the loop posts at once, after every action dispatched before it.
    async fn sentinel(&mut self) {
        self.h.dispatch(PlayerAction::Library {
            request: RequestId(77),
            provider: "nope".into(),
            query: LibraryQuery::Playlists { offset: 0 },
        });
        self.notice(
            |n| matches!(n, PlayerNotice::LibraryPage { request, .. } if *request == RequestId(77)),
        )
        .await;
    }

    async fn wait_state(&self, what: &str, f: impl Fn(&PlayerState) -> bool) {
        let start = Instant::now();
        while !f(&self.h.state()) {
            assert!(start.elapsed() < TIMEOUT, "timed out waiting for {what}");
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }

    fn set_liked_calls(&self) -> Vec<String> {
        self.p
            .calls()
            .into_iter()
            .filter(|c| c.starts_with("set_liked"))
            .collect()
    }

    fn errors(&self) -> Vec<&'static str> {
        self.notices
            .iter()
            .filter_map(|n| match n {
                PlayerNotice::Error(e) => Some(*e),
                _ => None,
            })
            .collect()
    }
}

fn is_like_notice(n: &PlayerNotice) -> bool {
    matches!(n, PlayerNotice::Liked(_) | PlayerNotice::LikeFailed { .. })
}

// K1
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn k1_request_liked_answers_pairs_in_request_order() {
    let mut r = rig(|p| p.with_likes(&["fakea:track:1", "fakea:track:3"], true));
    assert!(writable(&r.h.state()));
    r.h.dispatch(PlayerAction::RequestLiked(vec![
        t(3),
        t(2),
        "nope:track:x".into(),
        t(1),
    ]));
    let n = r.notice(is_like_notice).await;
    assert_eq!(
        n,
        PlayerNotice::Liked(vec![(t(3), true), (t(2), false), (t(1), true)])
    );
    assert_eq!(r.p.calls(), ["liked 3"]);
}

// K1 (bound)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn k1_request_liked_keeps_the_first_200_uris() {
    let mut r = rig(|p| p.with_likes(&[], true));
    r.h.dispatch(PlayerAction::RequestLiked((0..250).map(t).collect()));
    let PlayerNotice::Liked(pairs) = r.notice(is_like_notice).await else {
        panic!("liked expected")
    };
    assert_eq!(pairs.len(), 200);
    assert_eq!(pairs[199].0, t(199));
}

// K3
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn k3_a_new_current_item_reads_its_liked_state() {
    let mut r = rig(|p| p.with_likes(&["fakea:track:5"], true));
    let track = Track {
        uri: t(5),
        name: "five".into(),
        artist: "artist".into(),
        album: "album".into(),
        duration_ms: 180_000,
        images: vec![],
        year: None,
    };
    r.h.dispatch(PlayerAction::PlayItem {
        context: vec![track],
        index: 0,
    });
    let n = r.notice(is_like_notice).await;
    assert_eq!(n, PlayerNotice::Liked(vec![(t(5), true)]));
    r.wait_state("playing", |s| s.play_state == PlayState::Playing)
        .await;
    assert!(r.p.calls().contains(&"liked 1".to_owned()));
}

// K9 (F3): a read that answers after a change of the same uri never overwrites it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn k9_a_late_read_never_overwrites_a_newer_change() {
    let gate = Gate::new();
    let g = gate.clone();
    let mut r = rig(move |p| p.with_likes(&[], true).gate_liked(g));
    // The read sees track 1 unliked and t(2) unliked, then waits at the gate.
    r.h.dispatch(PlayerAction::RequestLiked(vec![t(1), t(2)]));
    let start = Instant::now();
    while !r.p.calls().contains(&"liked 2".to_owned()) {
        assert!(start.elapsed() < TIMEOUT, "the read never started");
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    // The user likes track 1; the write finishes before the read answers.
    r.h.dispatch(PlayerAction::SetLiked {
        uri: t(1),
        liked: true,
    });
    r.notice(|n| *n == PlayerNotice::Liked(vec![(t(1), true)]))
        .await;
    gate.open();
    // The read's answer keeps only the untouched uri.
    let n = r.notice(is_like_notice).await;
    assert_eq!(n, PlayerNotice::Liked(vec![(t(2), false)]));
    r.sentinel().await;
    let last = r.notices.iter().rev().find(|n| is_like_notice(n)).unwrap();
    assert_eq!(*last, PlayerNotice::Liked(vec![(t(2), false)]));
    assert!(!r.notices.iter().any(|n| matches!(
        n,
        PlayerNotice::Liked(p) if p.contains(&(t(1), false))
    )));
}

// K4
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn k4_set_liked_calls_the_provider_then_reports_liked() {
    let mut r = rig(|p| p.with_likes(&["fakea:track:1"], true));
    r.h.dispatch(PlayerAction::SetLiked {
        uri: t(1),
        liked: false,
    });
    let n = r.notice(is_like_notice).await;
    assert_eq!(n, PlayerNotice::Liked(vec![(t(1), false)]));
    assert_eq!(r.set_liked_calls(), ["set_liked fakea:track:1 false"]);
    assert!(r.errors().is_empty());
}

// A written like tells the front-end the provider's liked list changed; a failed one does not.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_written_like_says_the_likes_changed_and_a_failed_one_does_not() {
    let mut r = rig(|p| p.with_likes(&[], true));
    r.h.dispatch(PlayerAction::SetLiked {
        uri: t(1),
        liked: true,
    });
    r.notice(|n| *n == PlayerNotice::Liked(vec![(t(1), true)]))
        .await;
    let n = r
        .notice(|n| matches!(n, PlayerNotice::LikesChanged { .. }))
        .await;
    assert_eq!(
        n,
        PlayerNotice::LikesChanged {
            provider: "fakea".into()
        }
    );
    let mut r = rig(|p| p.with_likes(&[], false));
    r.h.dispatch(PlayerAction::SetLiked {
        uri: t(1),
        liked: true,
    });
    r.notice(|n| matches!(n, PlayerNotice::LikeFailed { .. }))
        .await;
    r.sentinel().await;
    assert!(
        !r.notices
            .iter()
            .any(|n| matches!(n, PlayerNotice::LikesChanged { .. })),
        "{:?}",
        r.notices
    );
}

// Any provider's library change reaches the front-end as a notice naming it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_provider_library_change_reaches_the_front_end() {
    let mut r = rig(|p| p);
    // The loop subscribes at start; a sentinel proves it runs before the event is sent.
    r.sentinel().await;
    r.events
        .send(UiEvent::LibraryChanged {
            provider: "fakea".into(),
        })
        .expect("the player listens");
    let n = r
        .notice(|n| matches!(n, PlayerNotice::LibraryChanged { .. }))
        .await;
    assert_eq!(
        n,
        PlayerNotice::LibraryChanged {
            provider: "fakea".into()
        }
    );
}

// K5
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn k5_two_changes_at_once_run_in_order_and_the_last_wins() {
    let mut r = rig(|p| p.with_likes(&[], true));
    r.h.dispatch(PlayerAction::SetLiked {
        uri: t(2),
        liked: true,
    });
    r.h.dispatch(PlayerAction::SetLiked {
        uri: t(2),
        liked: false,
    });
    r.notice(|n| *n == PlayerNotice::Liked(vec![(t(2), false)]))
        .await;
    r.sentinel().await;
    let calls = r.set_liked_calls();
    assert!(
        calls
            == [
                "set_liked fakea:track:2 true",
                "set_liked fakea:track:2 false"
            ]
            || calls == ["set_liked fakea:track:2 false"],
        "{calls:?}"
    );
    let last = r.notices.iter().rev().find(|n| is_like_notice(n)).unwrap();
    assert_eq!(*last, PlayerNotice::Liked(vec![(t(2), false)]));
}

// K6
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn k6_a_read_only_provider_refuses_without_a_call() {
    let mut r = rig(|p| p.with_likes(&[], false));
    assert!(!writable(&r.h.state()));
    r.h.dispatch(PlayerAction::SetLiked {
        uri: t(1),
        liked: true,
    });
    let n = r.notice(is_like_notice).await;
    assert_eq!(
        n,
        PlayerNotice::LikeFailed {
            uri: t(1),
            liked: true
        }
    );
    assert!(r.set_liked_calls().is_empty());
}

// K7
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn k7_access_denied_turning_read_only_fails_without_an_error() {
    let mut r = rig(|p| {
        p.with_likes(&[], true)
            .fail_set_liked(LibErrorKind::AccessDenied, false)
    });
    assert!(writable(&r.h.state()));
    r.h.dispatch(PlayerAction::SetLiked {
        uri: t(1),
        liked: true,
    });
    let n = r.notice(is_like_notice).await;
    assert_eq!(
        n,
        PlayerNotice::LikeFailed {
            uri: t(1),
            liked: true
        }
    );
    r.wait_state("read-only", |s| !writable(s)).await;
    r.sentinel().await;
    assert!(r.errors().is_empty(), "{:?}", r.errors());
    // Read-only now: the next change makes no call.
    r.h.dispatch(PlayerAction::SetLiked {
        uri: t(2),
        liked: true,
    });
    r.notice(|n| matches!(n, PlayerNotice::LikeFailed { uri, .. } if *uri == t(2)))
        .await;
    assert_eq!(r.set_liked_calls().len(), 1);
}

// K7 (second half)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn k7_a_failure_leaving_it_writable_posts_like_failed() {
    let mut r = rig(|p| {
        p.with_likes(&[], true)
            .fail_set_liked(LibErrorKind::Network, true)
    });
    r.h.dispatch(PlayerAction::SetLiked {
        uri: t(1),
        liked: true,
    });
    let n = r.notice(is_like_notice).await;
    assert_eq!(
        n,
        PlayerNotice::LikeFailed {
            uri: t(1),
            liked: true
        }
    );
    r.notice(|n| matches!(n, PlayerNotice::Error(_))).await;
    assert_eq!(r.errors(), [texts::LIKE_FAILED]);
    assert!(writable(&r.h.state()));
}

// K7b
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn k7b_access_denied_while_still_writable_posts_like_refused() {
    let mut r = rig(|p| {
        p.with_likes(&[], true)
            .fail_set_liked(LibErrorKind::AccessDenied, true)
    });
    r.h.dispatch(PlayerAction::SetLiked {
        uri: t(1),
        liked: false,
    });
    let n = r.notice(is_like_notice).await;
    assert_eq!(
        n,
        PlayerNotice::LikeFailed {
            uri: t(1),
            liked: false
        }
    );
    r.notice(|n| matches!(n, PlayerNotice::Error(_))).await;
    assert_eq!(r.errors(), [texts::LIKE_REFUSED]);
    r.sentinel().await;
    assert!(writable(&r.h.state()));
}

// K8
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn k8_a_provider_without_likes_fails_and_answers_nothing() {
    let mut r = rig(|p| p);
    assert!(!writable(&r.h.state()));
    r.h.dispatch(PlayerAction::SetLiked {
        uri: t(1),
        liked: true,
    });
    let n = r.notice(is_like_notice).await;
    assert_eq!(
        n,
        PlayerNotice::LikeFailed {
            uri: t(1),
            liked: true
        }
    );
    r.h.dispatch(PlayerAction::RequestLiked(vec![t(1)]));
    r.sentinel().await;
    assert_eq!(r.notices.iter().filter(|n| is_like_notice(n)).count(), 1);
    assert!(r.p.calls().is_empty(), "{:?}", r.p.calls());
}
