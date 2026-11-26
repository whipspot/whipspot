//! The control task's update flow: the updater's calls on a worker thread, one at a time, the
//! status they leave in the state and the schedule of the periodic check. Only the control task
//! calls into [`Updates`]; the workers report back through [`Internal`] messages, so the state
//! has one writer.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use tokio::sync::mpsc::UnboundedSender;
use tokio::time::Instant;

use super::Shared;
use super::control::Internal;
use super::state::PlayerState;
use crate::update::{
    CHECK_EVERY, CancelFlag, FIRST_CHECK_AFTER, UpdateError, UpdateOffer, UpdateStatus, UpdateStep,
    Updater,
};

/// What [`super::PlayerConfig`] hands the flow.
pub(crate) struct UpdateSetup {
    pub(crate) updater: Arc<dyn Updater>,
    /// The scheduled-checks setting at start.
    pub(crate) check: bool,
}

pub(crate) struct Updates {
    updater: Arc<dyn Updater>,
    shared: Arc<Shared>,
    internal: UnboundedSender<Internal>,
    /// The scheduled-checks setting.
    check: bool,
    /// When the next scheduled check is due; `None` while the setting is off or the copy cannot
    /// update.
    deadline: Option<Instant>,
    /// A worker call is running: every other update action is ignored until it reports.
    busy: bool,
    /// The version the last check offered (kept until the next check starts).
    offer: Option<UpdateOffer>,
    /// The offered version is downloaded and staged.
    staged: bool,
    /// Stops the running download.
    cancel: CancelFlag,
}

impl Updates {
    pub(crate) fn new(
        setup: UpdateSetup,
        shared: Arc<Shared>,
        internal: UnboundedSender<Internal>,
    ) -> Self {
        Updates {
            updater: setup.updater,
            shared,
            internal,
            check: setup.check,
            deadline: setup.check.then(|| Instant::now() + FIRST_CHECK_AFTER),
            busy: false,
            offer: None,
            staged: false,
            cancel: CancelFlag::new(),
        }
    }

    /// When the next scheduled check is due.
    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    /// The control task ended: stop a running download.
    pub(crate) fn stop(&self) {
        self.cancel.cancel();
    }

    fn status(&self) -> UpdateStatus {
        self.shared.state().update.clone()
    }

    fn set_status(&self, status: UpdateStatus) {
        self.shared.update(|s| s.update = status);
    }

    /// Whether a check may start now: nothing runs and the status allows one.
    fn can_check(&self) -> bool {
        !self.busy && self.status().can_check()
    }

    /// `CheckForUpdate`.
    pub(crate) fn check_now(&mut self) {
        if !self.can_check() {
            return;
        }
        self.busy = true;
        self.offer = None;
        self.staged = false;
        self.set_status(UpdateStatus::Checking);
        let (updater, tx) = (self.updater.clone(), self.internal.clone());
        tokio::task::spawn_blocking(move || {
            let result = catch_unwind(AssertUnwindSafe(|| updater.check()))
                .unwrap_or(Err(UpdateError::Other));
            let _ = tx.send(Internal::UpdateChecked(result));
        });
    }

    /// `InstallUpdate`: download the offer, or install what is staged.
    pub(crate) fn install(&mut self) {
        if self.busy {
            return;
        }
        match self.status().next_step() {
            Some(UpdateStep::Install { .. }) => self.apply(),
            Some(UpdateStep::Download { .. }) => self.download(),
            None => {}
        }
    }

    fn download(&mut self) {
        let Some(offer) = self.offer.clone() else {
            return;
        };
        self.busy = true;
        self.cancel = CancelFlag::new();
        self.set_status(UpdateStatus::Downloading {
            version: offer.version.clone(),
            done: 0,
            total: 0,
        });
        let (updater, tx, cancel) = (
            self.updater.clone(),
            self.internal.clone(),
            self.cancel.clone(),
        );
        tokio::task::spawn_blocking(move || {
            // A progress message only when the whole percent (or, with no total, the 256 KiB step)
            // changes, so a chatty updater does not publish a state per chunk.
            let last = AtomicU64::new(u64::MAX);
            let progress = |done: u64, total: u64| {
                let key = if total > 0 {
                    done.saturating_mul(100) / total
                } else {
                    done / (256 * 1024)
                };
                if last.swap(key, Ordering::Relaxed) != key {
                    let _ = tx.send(Internal::UpdateProgress { done, total });
                }
            };
            let result = catch_unwind(AssertUnwindSafe(|| {
                updater.download(&offer, &progress, &cancel)
            }))
            .unwrap_or(Err(UpdateError::Other));
            let _ = tx.send(Internal::UpdateDownloaded(result));
        });
    }

    fn apply(&mut self) {
        self.busy = true;
        let (updater, tx) = (self.updater.clone(), self.internal.clone());
        tokio::task::spawn_blocking(move || {
            let result = catch_unwind(AssertUnwindSafe(|| updater.apply_and_restart()))
                .unwrap_or(Err(UpdateError::Other));
            let _ = tx.send(Internal::UpdateApplied(result));
        });
    }

    /// `SetUpdateCheck`: the schedule starts (first check after the start delay) or stops. A
    /// check already running or a manual one is not touched.
    pub(crate) fn set_check(&mut self, on: bool) {
        self.check = on;
        self.shared
            .update(|s: &mut PlayerState| s.update_check = on);
        if !on {
            self.deadline = None;
        } else if self.deadline.is_none() && self.status() != UpdateStatus::Unavailable {
            self.deadline = Some(Instant::now() + FIRST_CHECK_AFTER);
        }
    }

    /// The schedule's deadline passed: arm the next one and check if nothing else is going on.
    pub(crate) fn timed_out(&mut self) {
        self.deadline = self.check.then(|| Instant::now() + CHECK_EVERY);
        self.check_now();
    }

    /// The copy cannot update: the status is final and nothing is scheduled.
    fn unavailable(&mut self) {
        self.deadline = None;
        self.offer = None;
        self.staged = false;
        self.set_status(UpdateStatus::Unavailable);
    }

    fn failed(&mut self, error: UpdateError) {
        if error == UpdateError::Unavailable {
            self.unavailable();
        } else {
            // What a second `InstallUpdate` may take again: installing when the download is
            // staged, downloading while the offer stands.
            let version = self.offer.as_ref().map(|o| o.version.clone());
            let retry = match version {
                Some(version) if self.staged => Some(UpdateStep::Install { version }),
                Some(version) => Some(UpdateStep::Download { version }),
                None => None,
            };
            self.set_status(UpdateStatus::Failed {
                text: error.text(),
                retry,
            });
        }
    }

    pub(crate) fn on_checked(&mut self, result: Result<Option<UpdateOffer>, UpdateError>) {
        self.busy = false;
        match result {
            Ok(None) => self.set_status(UpdateStatus::UpToDate {
                checked_at: SystemTime::now(),
            }),
            Ok(Some(offer)) => {
                self.set_status(UpdateStatus::Available {
                    version: offer.version.clone(),
                });
                self.offer = Some(offer);
            }
            Err(e) => self.failed(e),
        }
    }

    pub(crate) fn on_progress(&mut self, done: u64, total: u64) {
        if !self.busy {
            return;
        }
        if let UpdateStatus::Downloading { version, .. } = self.status() {
            self.set_status(UpdateStatus::Downloading {
                version,
                done,
                total,
            });
        }
    }

    pub(crate) fn on_downloaded(&mut self, result: Result<(), UpdateError>) {
        self.busy = false;
        let version = self.offer.as_ref().map(|o| o.version.clone());
        match (result, version) {
            (Ok(()), Some(version)) => {
                self.staged = true;
                self.set_status(UpdateStatus::Ready { version });
            }
            // Cancelled: the offer stands and can be tried again.
            (Err(UpdateError::Cancelled), Some(version)) => {
                self.set_status(UpdateStatus::Available { version })
            }
            (Ok(()), None) => self.failed(UpdateError::Other),
            (Err(e), _) => self.failed(e),
        }
    }

    pub(crate) fn on_applied(&mut self, result: Result<(), UpdateError>) {
        self.busy = false;
        // Handed over: the app is about to end and the installer waits for that. The status
        // accepts nothing more, so a second click cannot hand over a second time.
        match result {
            Ok(()) => self.set_status(UpdateStatus::Restarting),
            Err(e) => self.failed(e),
        }
    }
}
