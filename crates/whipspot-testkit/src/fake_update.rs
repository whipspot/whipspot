//! A scripted [`Updater`]: it answers from queues, records every call with the thread it ran on,
//! and can hold a call until the test lets it go. It touches no network, file or process.

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex, MutexGuard};
use std::thread::ThreadId;
use std::time::Duration;

use whipspot_core::update::{CancelFlag, UpdateError, UpdateOffer, Updater};

/// One call the fake answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpdateCall {
    Check,
    /// `download` of this version.
    Download(String),
    Apply,
}

#[derive(Default)]
struct State {
    checks: VecDeque<Result<Option<UpdateOffer>, UpdateError>>,
    downloads: VecDeque<Result<(), UpdateError>>,
    applies: VecDeque<Result<(), UpdateError>>,
    progress: Vec<(u64, u64)>,
    calls: Vec<(UpdateCall, ThreadId)>,
    /// Calls wait on this before they answer.
    held: bool,
    /// How many calls wait on the hold now.
    waiting: usize,
}

/// A fake updater. Answers not scripted are the quiet ones: a check finds nothing, a download and
/// an apply succeed.
pub struct FakeUpdater {
    version: String,
    state: Mutex<State>,
    wake: Condvar,
}

impl FakeUpdater {
    pub fn new(version: &str) -> Self {
        Self {
            version: version.to_owned(),
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The next check answers `result` (queued after any earlier script).
    pub fn script_check(&self, result: Result<Option<UpdateOffer>, UpdateError>) {
        self.lock().checks.push_back(result);
    }

    /// The next check offers `version`.
    pub fn offer(&self, version: &str) {
        self.script_check(Ok(Some(UpdateOffer {
            version: version.to_owned(),
        })));
    }

    /// The next download answers `result`.
    pub fn script_download(&self, result: Result<(), UpdateError>) {
        self.lock().downloads.push_back(result);
    }

    /// The next apply answers `result`.
    pub fn script_apply(&self, result: Result<(), UpdateError>) {
        self.lock().applies.push_back(result);
    }

    /// Every download reports these `(done, total)` pairs first.
    pub fn script_progress(&self, steps: &[(u64, u64)]) {
        self.lock().progress = steps.to_vec();
    }

    /// While held, a call waits before it answers (a download reports its progress first, and
    /// leaves with `Cancelled` when its flag is set).
    pub fn hold(&self, held: bool) {
        self.lock().held = held;
        self.wake.notify_all();
    }

    /// How many calls wait on the hold now.
    pub fn waiting(&self) -> usize {
        self.lock().waiting
    }

    /// Every call so far, in order.
    pub fn calls(&self) -> Vec<UpdateCall> {
        self.lock().calls.iter().map(|(c, _)| c.clone()).collect()
    }

    /// The thread each call ran on, in order.
    pub fn call_threads(&self) -> Vec<ThreadId> {
        self.lock().calls.iter().map(|(_, t)| *t).collect()
    }

    fn record(&self, call: UpdateCall) {
        self.lock().calls.push((call, std::thread::current().id()));
    }

    /// Waits while held; true when `cancel` ended the wait.
    fn wait_held(&self, cancel: Option<&CancelFlag>) -> bool {
        let mut st = self.lock();
        if !st.held {
            return false;
        }
        st.waiting += 1;
        let mut cancelled = false;
        while st.held {
            if cancel.is_some_and(CancelFlag::is_cancelled) {
                cancelled = true;
                break;
            }
            st = self
                .wake
                .wait_timeout(st, Duration::from_millis(2))
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        st.waiting -= 1;
        cancelled
    }
}

impl Updater for FakeUpdater {
    fn current_version(&self) -> &str {
        &self.version
    }

    fn check(&self) -> Result<Option<UpdateOffer>, UpdateError> {
        self.record(UpdateCall::Check);
        self.wait_held(None);
        self.lock().checks.pop_front().unwrap_or(Ok(None))
    }

    fn download(
        &self,
        offer: &UpdateOffer,
        progress: &dyn Fn(u64, u64),
        cancel: &CancelFlag,
    ) -> Result<(), UpdateError> {
        self.record(UpdateCall::Download(offer.version.clone()));
        let steps = self.lock().progress.clone();
        for (done, total) in steps {
            progress(done, total);
        }
        if self.wait_held(Some(cancel)) || cancel.is_cancelled() {
            return Err(UpdateError::Cancelled);
        }
        self.lock().downloads.pop_front().unwrap_or(Ok(()))
    }

    fn apply_and_restart(&self) -> Result<(), UpdateError> {
        self.record(UpdateCall::Apply);
        self.wait_held(None);
        self.lock().applies.pop_front().unwrap_or(Ok(()))
    }
}
