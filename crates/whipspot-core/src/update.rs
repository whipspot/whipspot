//! The update seam: whether a newer version of the app exists, getting it ready and restarting
//! into it. The player's control task runs an [`Updater`]'s calls on a worker thread, one at a
//! time, and keeps their outcome as an [`UpdateStatus`] in the state; the adapter owns every
//! network, file and process call. Nothing here names a packaging tool, a server or an OS.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

/// How long after start the first scheduled check runs.
pub const FIRST_CHECK_AFTER: Duration = Duration::from_secs(60);

/// How long after a scheduled check the next one runs.
pub const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);

/// A newer version the updater found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateOffer {
    /// The version text of the newer release.
    pub version: String,
}

/// Why an [`Updater`] call failed. A closed set: the adapter maps whatever went wrong onto one
/// of these, so no library or server text ever reaches the state or a log (hard rule 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateError {
    /// This copy of the app cannot update itself (it was not installed by the installer).
    Unavailable,
    /// The update server could not be reached or answered badly.
    Network,
    /// What was downloaded did not verify (signature, size or hash).
    Verify,
    /// Another update or an installer is working.
    Busy,
    /// The download was cancelled through its [`CancelFlag`].
    Cancelled,
    /// Anything else.
    Other,
}

impl UpdateError {
    /// The fixed text a front-end shows for this failure.
    pub fn text(self) -> &'static str {
        match self {
            UpdateError::Unavailable => texts::UNAVAILABLE,
            UpdateError::Network => texts::NETWORK,
            UpdateError::Verify => texts::VERIFY,
            UpdateError::Busy => texts::BUSY,
            UpdateError::Cancelled => texts::CANCELLED,
            UpdateError::Other => texts::OTHER,
        }
    }
}

/// The fixed texts of [`UpdateStatus::Failed`].
pub mod texts {
    pub const UNAVAILABLE: &str = "this copy cannot update itself";
    pub const NETWORK: &str = "could not reach the update server";
    pub const VERIFY: &str = "the update did not verify";
    pub const BUSY: &str = "another update is running";
    pub const CANCELLED: &str = "the update was cancelled";
    pub const OTHER: &str = "the update failed";
}

/// A flag a download looks at between chunks; clones share it.
#[derive(Clone, Debug, Default)]
pub struct CancelFlag(Arc<AtomicBool>);

impl CancelFlag {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ask the download to stop.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// Where an update stands, for the front-end.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum UpdateStatus {
    /// This copy cannot update itself (no updater, or not an installed copy). Final.
    #[default]
    Unavailable,
    /// Nothing checked yet.
    Idle,
    /// Asking the server.
    Checking,
    /// The newest version is the one running; `checked_at` is when that was found.
    UpToDate { checked_at: SystemTime },
    /// `version` is newer and not downloaded.
    Available { version: String },
    /// `version` is downloading: `done` of `total` bytes (`total` 0 while unknown).
    Downloading {
        version: String,
        done: u64,
        total: u64,
    },
    /// `version` is downloaded and staged: installing restarts into it.
    Ready { version: String },
    /// The last step failed; `text` is one of [`texts`].
    Failed { text: &'static str },
}

/// Updates the running app. Held as `Arc<dyn Updater>` by the player.
///
/// Every method blocks and may take long: the player calls them only on a worker thread, one at
/// a time. None panics for an expected failure; each returns only the fixed [`UpdateError`]s. A
/// copy that cannot update itself answers [`UpdateError::Unavailable`] from `check`.
pub trait Updater: Send + Sync {
    /// The running version's text.
    fn current_version(&self) -> &str;
    /// Ask the server for a newer version than the running one; `None` when there is none.
    fn check(&self) -> Result<Option<UpdateOffer>, UpdateError>;
    /// Download and stage `offer`, calling `progress(done, total)` as bytes arrive (`total` 0
    /// while unknown) and stopping with [`UpdateError::Cancelled`] once `cancel` is set.
    fn download(
        &self,
        offer: &UpdateOffer,
        progress: &dyn Fn(u64, u64),
        cancel: &CancelFlag,
    ) -> Result<(), UpdateError>;
    /// Install the staged update and restart the app. Returns only when it did not restart.
    fn apply_and_restart(&self) -> Result<(), UpdateError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_error_has_its_own_fixed_text() {
        let all = [
            UpdateError::Unavailable,
            UpdateError::Network,
            UpdateError::Verify,
            UpdateError::Busy,
            UpdateError::Cancelled,
            UpdateError::Other,
        ];
        let texts: Vec<_> = all.iter().map(|e| e.text()).collect();
        for (i, a) in texts.iter().enumerate() {
            assert!(!a.is_empty());
            assert!(texts[i + 1..].iter().all(|b| a != b), "{a}");
        }
    }

    #[test]
    fn a_cancel_flag_is_shared_by_its_clones() {
        let flag = CancelFlag::new();
        let other = flag.clone();
        assert!(!other.is_cancelled());
        flag.cancel();
        assert!(other.is_cancelled());
    }

    #[test]
    fn the_status_defaults_to_unavailable() {
        assert_eq!(UpdateStatus::default(), UpdateStatus::Unavailable);
    }
}
