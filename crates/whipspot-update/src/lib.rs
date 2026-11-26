//! The update adapter: [`LunepackUpdater`] implements the core's [`Updater`] on the lunepack
//! library, and [`lifecycle_startup`] and [`confirm_healthy`] are the two lifecycle calls the app
//! makes around its window. It is the only crate that names lunepack (boundary rule R1).
//!
//! An update applies only to a copy the lunepack installer put on the machine; a copy run from a
//! build folder (a development run) has no update config beside it, and every call answers
//! [`UpdateError::Unavailable`] or a quiet "development run" without any request. The feed, the
//! app id and the root key are read by lunepack from the installed copy's `lunepack-update.json`
//! at run time: nothing here compiles one in (D10).
//!
//! No text from lunepack, no URL, path or key ever leaves this crate: every failure becomes one
//! of the core's fixed [`UpdateError`]s, and the lifecycle calls return fixed words (hard rule 1).

use std::future::{Future, poll_fn};
use std::pin::pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::Poll;
use std::thread;
use std::time::Duration;

use lunepack::{
    ApplyOptions, CancelToken, CheckOptions, ConfirmOutcome, DownloadOptions, Error, Installation,
    Phase, StagedVersion, StartupAction, UpdateInfo, blocking,
};
use whipspot_core::update::{CancelFlag, UpdateError, UpdateOffer, Updater};

/// How often a running download looks at the core's cancel flag.
const CANCEL_POLL: Duration = Duration::from_millis(50);

/// Maps a lunepack error onto the core's closed set. Total: an error this match does not know
/// (the type is `#[non_exhaustive]`) is [`UpdateError::Other`].
fn map_error(error: &Error) -> UpdateError {
    match error {
        // No update config, no install record, a place lunepack does not update, an install the
        // administrator or the scope puts out of this app's reach: this copy cannot update.
        Error::NoUpdateConfig
        | Error::NotInstalled
        | Error::UpdateConfig(_)
        | Error::UnsupportedLocation { .. }
        | Error::Unsupported
        | Error::DisabledByPolicy
        | Error::MachineScope => UpdateError::Unavailable,
        Error::Busy => UpdateError::Busy,
        Error::Cancelled => UpdateError::Cancelled,
        Error::Transport(_) | Error::Incomplete | Error::NeedFetchExhausted(_) => {
            UpdateError::Network
        }
        Error::Verify(_) | Error::PublisherMismatch { .. } => UpdateError::Verify,
        _ => UpdateError::Other,
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// Runs `future` to completion on the calling thread, like lunepack's blocking face, and also:
/// sets `token` when `cancel` is set (a thread looks every [`CANCEL_POLL`]), and calls
/// `after_poll` on the calling thread after every poll, so a caller whose progress callback is
/// not `'static` can forward what the operation's own callback stored.
fn drive<T>(
    token: &CancelToken,
    cancel: &CancelFlag,
    mut after_poll: impl FnMut(),
    future: impl Future<Output = Result<T, Error>>,
) -> Result<T, Error> {
    let done = AtomicBool::new(false);
    thread::scope(|scope| {
        // Without the thread (it could not start) a cancel is seen only at the next progress.
        let _ = thread::Builder::new()
            .name("whipspot-update-cancel".into())
            .spawn_scoped(scope, || {
                while !done.load(Ordering::Relaxed) {
                    if cancel.is_cancelled() {
                        token.cancel();
                        break;
                    }
                    thread::sleep(CANCEL_POLL);
                }
            });
        let mut future = pin!(future);
        let result = lunepack::block_on(poll_fn(|cx| {
            if token.is_cancelled() || cancel.is_cancelled() {
                return Poll::Ready(Err(Error::Cancelled));
            }
            token.register_waker(cx.waker());
            let polled = future.as_mut().poll(cx);
            after_poll();
            polled
        }));
        done.store(true, Ordering::Relaxed);
        result
    })
}

/// The core's [`Updater`] on lunepack. Built once by the composition root; it reads the install
/// it runs from at each call, so a copy that is moved or adopted is always looked at as it is.
pub struct LunepackUpdater {
    version: String,
    /// What the last check found: `download` takes the offer's version and needs the verified
    /// documents behind it.
    found: Mutex<Option<UpdateInfo>>,
    /// The version `download` staged, which `apply_and_restart` expects.
    staged: Mutex<Option<StagedVersion>>,
}

impl LunepackUpdater {
    /// An updater for the running copy, whose version is `version` (the app crate's).
    pub fn new(version: &str) -> Self {
        Self {
            version: version.to_owned(),
            found: Mutex::new(None),
            staged: Mutex::new(None),
        }
    }

    /// Whether this copy sits in a place lunepack updates (has an update config beside it). A
    /// development run, or a copy run from a mounted disk image, does not. Reads files only.
    pub fn available(&self) -> bool {
        Installation::from_current_exe().is_ok()
    }

    /// The lunepack updater of the running install, built fresh each call: an adoption (macOS)
    /// changes what the next one must be.
    fn updater(&self) -> Result<lunepack::Updater, UpdateError> {
        let install = Installation::from_current_exe().map_err(|e| map_error(&e))?;
        install
            .updater_builder()
            .and_then(|builder| builder.build())
            .map_err(|e| map_error(&e))
    }
}

impl Updater for LunepackUpdater {
    fn current_version(&self) -> &str {
        &self.version
    }

    fn check(&self) -> Result<Option<UpdateOffer>, UpdateError> {
        let updater = blocking::Updater::from(self.updater()?);
        let outcome = updater
            .check_with(CheckOptions::default(), &CancelToken::new())
            .map_err(|e| map_error(&e))?;
        let mut found = lock(&self.found);
        *found = outcome.update;
        Ok(found.as_ref().map(|info| UpdateOffer {
            version: info.version_text().to_owned(),
        }))
    }

    fn download(
        &self,
        offer: &UpdateOffer,
        progress: &dyn Fn(u64, u64),
        cancel: &CancelFlag,
    ) -> Result<(), UpdateError> {
        let info = lock(&self.found)
            .clone()
            .filter(|info| info.version_text() == offer.version)
            .ok_or(UpdateError::Other)?;
        let updater = self.updater()?;
        let token = CancelToken::new();
        // The callback runs inside a poll and must be `'static`: it only stores the numbers, and
        // the polling loop forwards them to `progress` on this thread.
        let latest: Arc<Mutex<Option<(u64, u64)>>> = Arc::new(Mutex::new(None));
        let store = Arc::clone(&latest);
        let options = || DownloadOptions {
            progress: {
                let store = Arc::clone(&store);
                Some(Box::new(move |p: &lunepack::Progress| {
                    if p.phase == Phase::Download {
                        *lock(&store) = Some((p.done, p.total));
                    }
                }))
            },
            cancel: Some(token.clone()),
        };
        let forward = || {
            if let Some((done, total)) = lock(&latest).take() {
                progress(done, total);
            }
        };
        let downloaded = drive(&token, cancel, forward, updater.download(&info, options()))
            .map_err(|e| map_error(&e))?;
        let staged = drive(
            &token,
            cancel,
            forward,
            updater.stage(downloaded.with_options(options())),
        )
        .map_err(|e| map_error(&e))?;
        *lock(&self.staged) = Some(staged);
        Ok(())
    }

    fn apply_and_restart(&self) -> Result<(), UpdateError> {
        let expect = lock(&self.staged)
            .as_ref()
            .map(|staged| staged.version().clone());
        lunepack::apply_and_restart(ApplyOptions {
            expect_version: expect,
            ..ApplyOptions::default()
        })
        .map_err(|e| map_error(&e))?;
        // The runtime waits for this process to end, then swaps the version and starts the new
        // one: leave now. Nothing the app holds needs a clean close (settings are written at
        // each change that matters, the output device is the OS's to release).
        std::process::exit(0)
    }
}

/// What [`lifecycle_startup`] tells the app to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Start {
    /// Go on starting.
    Continue,
    /// Exit now: this version was rolled back and the previous one runs in its place.
    Exit,
}

/// What the start found, as a fixed word the app logs once its log is up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartNote {
    /// An installed copy started normally.
    Installed,
    /// Not an installed copy (a development run, or a copy run from a disk image).
    DevRun,
    /// This version was rolled back after it never confirmed.
    RolledBack,
    /// The start call failed; the app goes on.
    Failed,
}

impl StartNote {
    /// The fixed text for the log.
    pub fn text(self) -> &'static str {
        match self {
            StartNote::Installed => "update lifecycle: installed copy started",
            StartNote::DevRun => "update lifecycle: not an installed copy",
            StartNote::RolledBack => "update lifecycle: this version was rolled back",
            StartNote::Failed => "update lifecycle: the start call failed",
        }
    }
}

/// Whether `error` means "this is not an installed copy" rather than a failure.
fn is_dev_run(error: &Error) -> bool {
    map_error(error) == UpdateError::Unavailable
}

/// The first call of `main`: lets lunepack see what happened since the last start (a pending
/// version counts an attempt, a failed one rolls back) and, on Windows, set the process's app
/// user model id before a window exists. Never fails the start.
pub fn lifecycle_startup() -> (Start, StartNote) {
    match lunepack::startup() {
        Ok(started) if started.action == StartupAction::Exit => {
            (Start::Exit, StartNote::RolledBack)
        }
        Ok(_) => (Start::Continue, StartNote::Installed),
        Err(e) if is_dev_run(&e) => (Start::Continue, StartNote::DevRun),
        Err(_) => (Start::Continue, StartNote::Failed),
    }
}

/// What [`confirm_healthy`] found, as a fixed word for the log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Health {
    /// This version was waiting for its confirmation and has it now.
    Confirmed,
    /// Nothing was waiting (an ordinary start).
    NothingPending,
    /// Not an installed copy.
    DevRun,
    /// The call failed.
    Failed,
}

impl Health {
    /// The fixed text for the log.
    pub fn text(self) -> &'static str {
        match self {
            Health::Confirmed => "update lifecycle: this version is confirmed",
            Health::NothingPending => "update lifecycle: nothing to confirm",
            Health::DevRun => "update lifecycle: not an installed copy",
            Health::Failed => "update lifecycle: the confirm call failed",
        }
    }
}

/// Tells lunepack this version works. Called once the first frame is on screen and the player is
/// built (D9): a version that never gets here is rolled back.
pub fn confirm_healthy() -> Health {
    match lunepack::confirm_healthy() {
        Ok(ConfirmOutcome::Confirmed) => Health::Confirmed,
        Ok(_) => Health::NothingPending,
        Err(e) if is_dev_run(&e) => Health::DevRun,
        Err(_) => Health::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::pending;
    use std::sync::atomic::AtomicU32;

    #[test]
    fn a_copy_run_from_a_build_folder_cannot_update_and_asks_nobody() {
        // The test binary has no update config in any folder above it, so lunepack decides
        // "not installed" from files alone, before any request.
        let updater = LunepackUpdater::new("1.2.3");
        assert_eq!(updater.current_version(), "1.2.3");
        assert!(!updater.available());
        assert_eq!(updater.check(), Err(UpdateError::Unavailable));
        let offer = UpdateOffer {
            version: "1.2.4".to_owned(),
        };
        assert_eq!(
            updater.download(&offer, &|_, _| {}, &CancelFlag::new()),
            // No check found it: nothing to download, whatever the copy is.
            Err(UpdateError::Other)
        );
        assert_eq!(updater.apply_and_restart(), Err(UpdateError::Unavailable));
    }

    #[test]
    fn the_lifecycle_calls_of_a_build_folder_copy_continue_quietly() {
        assert_eq!(lifecycle_startup(), (Start::Continue, StartNote::DevRun));
        assert_eq!(confirm_healthy(), Health::DevRun);
    }

    #[test]
    fn every_lunepack_error_lands_on_a_fixed_variant() {
        assert_eq!(map_error(&Error::NoUpdateConfig), UpdateError::Unavailable);
        assert_eq!(map_error(&Error::NotInstalled), UpdateError::Unavailable);
        assert_eq!(map_error(&Error::Unsupported), UpdateError::Unavailable);
        assert_eq!(
            map_error(&Error::DisabledByPolicy),
            UpdateError::Unavailable
        );
        assert_eq!(map_error(&Error::MachineScope), UpdateError::Unavailable);
        assert_eq!(map_error(&Error::Busy), UpdateError::Busy);
        assert_eq!(map_error(&Error::Cancelled), UpdateError::Cancelled);
        assert_eq!(map_error(&Error::Incomplete), UpdateError::Network);
        assert_eq!(
            map_error(&Error::NeedFetchExhausted(Vec::new())),
            UpdateError::Network
        );
        assert_eq!(map_error(&Error::NothingStaged), UpdateError::Other);
        assert_eq!(map_error(&Error::StateTooNew), UpdateError::Other);
        assert_eq!(map_error(&Error::WorkerPanicked), UpdateError::Other);
    }

    #[test]
    fn the_cancel_flag_stops_a_run_that_never_finishes() {
        let token = CancelToken::new();
        let flag = CancelFlag::new();
        let trip = flag.clone();
        let setter = thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            trip.cancel();
        });
        let polls = AtomicU32::new(0);
        let result: Result<(), Error> = drive(
            &token,
            &flag,
            || {
                polls.fetch_add(1, Ordering::Relaxed);
            },
            pending(),
        );
        setter.join().unwrap();
        assert!(matches!(result, Err(Error::Cancelled)));
        assert!(token.is_cancelled());
        assert!(polls.load(Ordering::Relaxed) >= 1);
    }

    #[test]
    fn a_finished_run_returns_its_value_and_forwards_after_each_poll() {
        let token = CancelToken::new();
        let flag = CancelFlag::new();
        let forwarded = AtomicU32::new(0);
        let result = drive(
            &token,
            &flag,
            || {
                forwarded.fetch_add(1, Ordering::Relaxed);
            },
            async { Ok::<_, Error>(7) },
        );
        assert!(matches!(result, Ok(7)));
        assert_eq!(forwarded.load(Ordering::Relaxed), 1);
        assert!(!token.is_cancelled());
    }
}
