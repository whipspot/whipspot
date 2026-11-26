//! The player's update flow on a scripted updater: the status table, the fixed failure texts, the
//! one-call-at-a-time rule, the worker thread, and the schedule (first check at 60 s, then every
//! 6 h, off with the setting). The clock is tokio's paused one; no network, file or process is
//! touched.

use std::sync::Arc;
use std::time::{Duration, Instant};

use whipspot_core::engine::{DeviceFormat, OutputMode, OutputRequest, SampleFormat};
use whipspot_core::player::{
    self, EngineSetup, Player, PlayerAction, PlayerConfig, PlayerHandle, PlayerState,
};
use whipspot_core::provider::ProviderRegistry;
use whipspot_core::update::{
    CHECK_EVERY, FIRST_CHECK_AFTER, UpdateError, UpdateOffer, UpdateStatus, UpdateStep, Updater,
    texts,
};
use whipspot_testkit::{FakeOutput, FakeProvider, FakeUpdater, UpdateCall};

/// How long a test waits for something that should happen at once.
const DEADLINE: Duration = Duration::from_secs(5);

fn start(config: PlayerConfig) -> PlayerHandle {
    let fake = FakeOutput::new(
        DeviceFormat {
            rate: 44_100,
            channels: 2,
            sample: SampleFormat::F32,
            valid_bits: 32,
        },
        OutputMode::Shared,
    );
    let setup = EngineSetup::Output {
        output: Box::new(fake),
        request: OutputRequest::default(),
    };
    let (ctx, wiring) = player::wiring(setup, 16);
    let mut registry = ProviderRegistry::new();
    registry
        .register(Arc::new(FakeProvider::new("fakea", true).with_context(ctx)))
        .unwrap();
    Player::start(registry, wiring, config, &tokio::runtime::Handle::current())
}

fn with_updater(updater: &Arc<FakeUpdater>, check: bool) -> PlayerHandle {
    start(PlayerConfig::default().with_updater(updater.clone() as Arc<dyn Updater>, check))
}

/// Yields, with the paused clock still, until `f` holds.
async fn until(h: &PlayerHandle, what: &str, f: impl Fn(&PlayerState) -> bool) {
    let begun = Instant::now();
    while !f(&h.state()) {
        assert!(
            begun.elapsed() < DEADLINE,
            "timed out waiting for {what}: {:?}",
            h.state().update
        );
        tokio::task::yield_now().await;
    }
}

async fn status(h: &PlayerHandle, what: &str, want: UpdateStatus) {
    until(h, what, |s| s.update == want).await;
}

/// Lets the player's tasks and a worker thread that was just started run what is ready; the
/// paused clock does not move. The real nap gives a worker the time to begin, so a test that
/// says "no call" is not passing because the call was merely late.
async fn settle() {
    for _ in 0..200 {
        tokio::task::yield_now().await;
    }
    std::thread::sleep(Duration::from_millis(30));
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
}

async fn advance(d: Duration) {
    tokio::time::advance(d).await;
    settle().await;
}

fn available(v: &str) -> UpdateStatus {
    UpdateStatus::Available {
        version: v.to_owned(),
    }
}

fn ready(v: &str) -> UpdateStatus {
    UpdateStatus::Ready {
        version: v.to_owned(),
    }
}

fn failed(text: &'static str) -> UpdateStatus {
    UpdateStatus::Failed { text, retry: None }
}

fn failed_retrying(text: &'static str, retry: UpdateStep) -> UpdateStatus {
    UpdateStatus::Failed {
        text,
        retry: Some(retry),
    }
}

fn download_step(v: &str) -> UpdateStep {
    UpdateStep::Download {
        version: v.to_owned(),
    }
}

fn install_step(v: &str) -> UpdateStep {
    UpdateStep::Install {
        version: v.to_owned(),
    }
}

#[tokio::test(start_paused = true)]
async fn the_state_starts_idle() {
    let u = Arc::new(FakeUpdater::new("1.2.3"));
    let h = with_updater(&u, true);
    let s = h.state();
    assert_eq!(s.update, UpdateStatus::Idle);
    assert!(s.update_check);
    assert!(u.calls().is_empty());
}

#[tokio::test(start_paused = true)]
async fn the_status_walks_idle_checking_up_to_date_or_available_downloading_ready_then_apply() {
    let u = Arc::new(FakeUpdater::new("1.0.0"));
    let h = with_updater(&u, false);
    u.hold(true);
    h.dispatch(PlayerAction::CheckForUpdate);
    status(&h, "checking", UpdateStatus::Checking).await;
    u.hold(false);
    until(&h, "up to date", |s| {
        matches!(s.update, UpdateStatus::UpToDate { .. })
    })
    .await;

    u.offer("1.1.0");
    h.dispatch(PlayerAction::CheckForUpdate);
    status(&h, "available", available("1.1.0")).await;

    u.hold(true);
    u.script_progress(&[(25, 100), (50, 100)]);
    h.dispatch(PlayerAction::InstallUpdate);
    status(
        &h,
        "downloading at 50 %",
        UpdateStatus::Downloading {
            version: "1.1.0".to_owned(),
            done: 50,
            total: 100,
        },
    )
    .await;
    u.hold(false);
    status(&h, "ready", ready("1.1.0")).await;
    assert!(!u.calls().contains(&UpdateCall::Apply));

    h.dispatch(PlayerAction::InstallUpdate);
    until(&h, "apply called", |_| {
        u.calls().contains(&UpdateCall::Apply)
    })
    .await;
    settle().await;
    assert_eq!(
        u.calls(),
        [
            UpdateCall::Check,
            UpdateCall::Check,
            UpdateCall::Download("1.1.0".to_owned()),
            UpdateCall::Apply
        ]
    );
    // Handed over: the status accepts nothing more and the front-end ends the app.
    status(&h, "restarting", UpdateStatus::Restarting).await;
}

#[tokio::test(start_paused = true)]
async fn failures_show_their_fixed_texts() {
    let u = Arc::new(FakeUpdater::new("1.0.0"));
    let h = with_updater(&u, false);
    u.script_check(Err(UpdateError::Network));
    h.dispatch(PlayerAction::CheckForUpdate);
    status(&h, "check failed", failed(texts::NETWORK)).await;

    // A failed check can be run again.
    u.offer("2.0.0");
    h.dispatch(PlayerAction::CheckForUpdate);
    status(&h, "available", available("2.0.0")).await;

    u.script_download(Err(UpdateError::Verify));
    h.dispatch(PlayerAction::InstallUpdate);
    status(
        &h,
        "download failed",
        failed_retrying(texts::VERIFY, download_step("2.0.0")),
    )
    .await;

    // Install after a failed download tries the same offer again.
    u.script_download(Ok(()));
    h.dispatch(PlayerAction::InstallUpdate);
    status(&h, "ready", ready("2.0.0")).await;

    u.script_apply(Err(UpdateError::Other));
    h.dispatch(PlayerAction::InstallUpdate);
    let apply_failed = failed_retrying(texts::OTHER, install_step("2.0.0"));
    status(&h, "apply failed", apply_failed.clone()).await;
    // The status offers the install again and no check; a Check does not drop the staged update.
    assert_eq!(apply_failed.next_step(), Some(install_step("2.0.0")));
    assert!(!apply_failed.can_check());
    h.dispatch(PlayerAction::CheckForUpdate);
    settle().await;
    assert_eq!(h.state().update, apply_failed);
    // Install after a failed apply applies again.
    h.dispatch(PlayerAction::InstallUpdate);
    until(&h, "second apply", |_| {
        u.calls()
            .iter()
            .filter(|c| **c == UpdateCall::Apply)
            .count()
            == 2
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_cancelled_download_goes_back_to_available() {
    let u = Arc::new(FakeUpdater::new("1.0.0"));
    let h = with_updater(&u, false);
    u.offer("1.1.0");
    h.dispatch(PlayerAction::CheckForUpdate);
    status(&h, "available", available("1.1.0")).await;
    u.script_download(Err(UpdateError::Cancelled));
    h.dispatch(PlayerAction::InstallUpdate);
    until(&h, "download tried", |_| {
        u.calls()
            .contains(&UpdateCall::Download("1.1.0".to_owned()))
    })
    .await;
    status(&h, "available again", available("1.1.0")).await;
}

#[tokio::test(start_paused = true)]
async fn a_second_action_while_one_runs_is_ignored() {
    let u = Arc::new(FakeUpdater::new("1.0.0"));
    let h = with_updater(&u, false);
    u.hold(true);
    h.dispatch(PlayerAction::CheckForUpdate);
    status(&h, "checking", UpdateStatus::Checking).await;
    let revision = h.state().revision;
    h.dispatch(PlayerAction::CheckForUpdate);
    h.dispatch(PlayerAction::InstallUpdate);
    settle().await;
    assert_eq!(h.state().update, UpdateStatus::Checking);
    assert_eq!(h.state().revision, revision, "the state did not change");
    u.hold(false);
    until(&h, "up to date", |s| {
        matches!(s.update, UpdateStatus::UpToDate { .. })
    })
    .await;
    assert_eq!(u.calls(), [UpdateCall::Check]);

    // The same during a download: a check and a second install change nothing.
    u.offer("1.1.0");
    h.dispatch(PlayerAction::CheckForUpdate);
    status(&h, "available", available("1.1.0")).await;
    u.hold(true);
    h.dispatch(PlayerAction::InstallUpdate);
    until(&h, "downloading", |s| {
        matches!(s.update, UpdateStatus::Downloading { .. })
    })
    .await;
    h.dispatch(PlayerAction::InstallUpdate);
    h.dispatch(PlayerAction::CheckForUpdate);
    settle().await;
    u.hold(false);
    status(&h, "ready", ready("1.1.0")).await;
    assert_eq!(
        u.calls()
            .iter()
            .filter(|c| matches!(c, UpdateCall::Download(_)))
            .count(),
        1
    );

    // And during the apply: the status stays Ready, so only the running call stops a second one.
    // Once it returned, Restarting takes no more.
    u.hold(true);
    h.dispatch(PlayerAction::InstallUpdate);
    until(&h, "apply running", |_| {
        u.calls().contains(&UpdateCall::Apply)
    })
    .await;
    h.dispatch(PlayerAction::InstallUpdate);
    h.dispatch(PlayerAction::CheckForUpdate);
    settle().await;
    u.hold(false);
    settle().await;
    let applies = u
        .calls()
        .iter()
        .filter(|c| **c == UpdateCall::Apply)
        .count();
    assert_eq!(applies, 1, "apply called once");
    assert_eq!(h.state().update, UpdateStatus::Restarting);
    h.dispatch(PlayerAction::InstallUpdate);
    h.dispatch(PlayerAction::CheckForUpdate);
    settle().await;
    assert_eq!(h.state().update, UpdateStatus::Restarting);
    assert_eq!(
        u.calls()
            .iter()
            .filter(|c| **c == UpdateCall::Apply)
            .count(),
        1,
        "no second apply once restarting"
    );
}

#[tokio::test(start_paused = true)]
async fn a_copy_that_cannot_update_stays_unavailable_and_asks_no_more() {
    // No updater wired: every action leaves the state alone.
    let h = start(PlayerConfig::default());
    assert_eq!(h.state().update, UpdateStatus::Unavailable);
    let revision = h.state().revision;
    h.dispatch(PlayerAction::CheckForUpdate);
    h.dispatch(PlayerAction::InstallUpdate);
    settle().await;
    assert_eq!(h.state().update, UpdateStatus::Unavailable);
    assert_eq!(h.state().revision, revision);

    // An updater that says Unavailable: the status turns final, the schedule stops.
    let u = Arc::new(FakeUpdater::new("1.0.0"));
    let h = with_updater(&u, true);
    u.script_check(Err(UpdateError::Unavailable));
    h.dispatch(PlayerAction::CheckForUpdate);
    status(&h, "unavailable", UpdateStatus::Unavailable).await;
    let calls = u.calls();
    for action in [
        PlayerAction::CheckForUpdate,
        PlayerAction::InstallUpdate,
        PlayerAction::SetUpdateCheck(false),
        PlayerAction::SetUpdateCheck(true),
    ] {
        h.dispatch(action);
        settle().await;
        assert_eq!(h.state().update, UpdateStatus::Unavailable);
    }
    advance(CHECK_EVERY * 2).await;
    assert_eq!(h.state().update, UpdateStatus::Unavailable);
    assert_eq!(u.calls(), calls, "no further updater call");
}

#[tokio::test(start_paused = true)]
async fn the_schedule_checks_at_60_s_then_every_6_h_and_the_setting_stops_it() {
    let u = Arc::new(FakeUpdater::new("1.0.0"));
    let h = with_updater(&u, true);
    advance(FIRST_CHECK_AFTER - Duration::from_secs(1)).await;
    assert!(u.calls().is_empty(), "before 60 s");
    advance(Duration::from_secs(1)).await;
    until(&h, "first check", |_| u.calls().len() == 1).await;
    advance(CHECK_EVERY - Duration::from_secs(1)).await;
    assert_eq!(u.calls().len(), 1, "before 6 h");
    advance(Duration::from_secs(1)).await;
    until(&h, "second check", |_| u.calls().len() == 2).await;

    // Off: the schedule stops, a manual check still works.
    h.dispatch(PlayerAction::SetUpdateCheck(false));
    until(&h, "off", |s| !s.update_check).await;
    advance(CHECK_EVERY * 3).await;
    assert_eq!(u.calls().len(), 2, "no scheduled check while off");
    h.dispatch(PlayerAction::CheckForUpdate);
    until(&h, "manual check", |_| u.calls().len() == 3).await;

    // On again: the first check comes after the start delay.
    h.dispatch(PlayerAction::SetUpdateCheck(true));
    until(&h, "on", |s| s.update_check).await;
    advance(FIRST_CHECK_AFTER).await;
    until(&h, "check after on", |_| u.calls().len() == 4).await;
}

#[tokio::test(start_paused = true)]
async fn a_player_started_with_the_setting_off_never_checks_by_itself() {
    let u = Arc::new(FakeUpdater::new("1.0.0"));
    let h = with_updater(&u, false);
    assert!(!h.state().update_check);
    advance(CHECK_EVERY * 2).await;
    assert!(u.calls().is_empty());
    assert_eq!(h.state().update, UpdateStatus::Idle);
}

#[tokio::test(start_paused = true)]
async fn the_schedule_skips_while_an_offer_waits() {
    let u = Arc::new(FakeUpdater::new("1.0.0"));
    u.offer("1.1.0");
    let h = with_updater(&u, true);
    advance(FIRST_CHECK_AFTER).await;
    status(&h, "available", available("1.1.0")).await;
    advance(CHECK_EVERY * 2).await;
    assert_eq!(
        u.calls(),
        [UpdateCall::Check],
        "an offer is not asked again"
    );
    assert_eq!(h.state().update, available("1.1.0"));
}

#[tokio::test(start_paused = true)]
async fn no_updater_call_runs_on_the_dispatching_thread() {
    let u = Arc::new(FakeUpdater::new("1.0.0"));
    let h = with_updater(&u, false);
    u.offer("1.1.0");
    h.dispatch(PlayerAction::CheckForUpdate);
    status(&h, "available", available("1.1.0")).await;
    h.dispatch(PlayerAction::InstallUpdate);
    status(&h, "ready", ready("1.1.0")).await;
    h.dispatch(PlayerAction::InstallUpdate);
    until(&h, "apply", |_| u.calls().contains(&UpdateCall::Apply)).await;
    let mine = std::thread::current().id();
    let threads = u.call_threads();
    assert_eq!(threads.len(), 3);
    assert!(
        threads.iter().all(|t| *t != mine),
        "an updater call ran on the dispatching thread"
    );
}

#[tokio::test(start_paused = true)]
async fn an_updater_that_panics_fails_the_step_not_the_player() {
    struct Panicky;
    impl Updater for Panicky {
        fn current_version(&self) -> &str {
            "1.0.0"
        }
        fn check(&self) -> Result<Option<UpdateOffer>, UpdateError> {
            panic!("a check that panics");
        }
        fn download(
            &self,
            _: &UpdateOffer,
            _: &dyn Fn(u64, u64),
            _: &whipspot_core::update::CancelFlag,
        ) -> Result<(), UpdateError> {
            Ok(())
        }
        fn apply_and_restart(&self) -> Result<(), UpdateError> {
            Ok(())
        }
    }
    let h = start(PlayerConfig::default().with_updater(Arc::new(Panicky), false));
    h.dispatch(PlayerAction::CheckForUpdate);
    status(&h, "failed", failed(texts::OTHER)).await;
    // The player still answers.
    h.dispatch(PlayerAction::SetVolume(40));
    until(&h, "volume", |s| s.volume == 40).await;
}
