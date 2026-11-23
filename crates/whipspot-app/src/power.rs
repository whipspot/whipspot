//! The OS's power-saving modes (the efficiency-mode task): the [`PowerControl`] adapter and the
//! per-thread protocol.
//!
//! `OsPower::set` does the process-level part at once (Windows only: EcoQoS and the idle priority
//! class for `EcoIdle`, both handed back for `Normal` and `Eco`) and then publishes the thread
//! mode. Each marked thread applies that mode to itself in [`sync_current_thread`], which only
//! two places call (I5): the main event-loop thread from the host's `new_events` and `about_to_wait`, and the
//! workers of the app's tokio runtime through its `on_thread_unpark` hook. No audio thread runs
//! there, so none is ever marked. Windows marks a thread with thread EcoQoS (its priority is never
//! changed); macOS with QoS `utility`, restored to the class the thread had before. The FFI lives
//! in `power_windows.rs` and `power_macos.rs`; this file is safe.

#[cfg(any(windows, target_os = "macos"))]
use std::cell::Cell;
#[cfg(any(windows, target_os = "macos"))]
use std::sync::Mutex;
#[cfg(any(windows, target_os = "macos"))]
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use whipspot_core::power::{PowerControl, PowerError, PowerMode};

#[cfg(target_os = "macos")]
use crate::power_macos as os;
#[cfg(windows)]
use crate::power_windows as os;

/// Whether threads that sync should be marked (Eco or EcoIdle).
#[cfg(any(windows, target_os = "macos"))]
static THREAD_ECO: AtomicBool = AtomicBool::new(false);
/// Bumped on every change of [`THREAD_ECO`], so an unchanged mode costs a thread one load.
#[cfg(any(windows, target_os = "macos"))]
static GENERATION: AtomicU64 = AtomicU64::new(0);

#[cfg(any(windows, target_os = "macos"))]
thread_local! {
    /// The generation this thread last applied; 0 is the start (Normal, nothing to do).
    static SEEN: Cell<u64> = const { Cell::new(0) };
    /// What this thread saved when it was marked; `Some` only while it is marked.
    static SAVED: Cell<Option<os::Saved>> = const { Cell::new(None) };
}

/// The real power control, built once in the composition root.
pub struct OsPower {
    /// The process's priority class at construction, set back when leaving EcoIdle (Windows).
    #[cfg(windows)]
    original_class: u32,
    /// Keeps the process part and the published thread mode of one `set` together.
    #[cfg(any(windows, target_os = "macos"))]
    lock: Mutex<()>,
}

impl OsPower {
    /// Reads what must be restored later (on Windows, the priority class).
    pub fn new() -> Self {
        OsPower {
            #[cfg(windows)]
            original_class: os::priority_class(),
            #[cfg(any(windows, target_os = "macos"))]
            lock: Mutex::new(()),
        }
    }
}

impl Default for OsPower {
    fn default() -> Self {
        Self::new()
    }
}

impl PowerControl for OsPower {
    fn name(&self) -> &'static str {
        if cfg!(windows) {
            "Windows Efficiency mode"
        } else if cfg!(target_os = "macos") {
            "macOS QoS and App Nap"
        } else {
            "No power control"
        }
    }

    #[cfg(any(windows, target_os = "macos"))]
    fn set(&self, mode: PowerMode) -> Result<(), PowerError> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        // (1) The process level first: leaving EcoIdle restores the priority class before
        // anything else (I1). macOS has no process-level call.
        #[cfg(windows)]
        let result = os::set_process(mode == PowerMode::EcoIdle, self.original_class);
        #[cfg(target_os = "macos")]
        let result = Ok(());
        // (2) The thread mode, applied by each marked thread to itself.
        let eco = mode != PowerMode::Normal;
        if THREAD_ECO.swap(eco, Ordering::SeqCst) != eco {
            GENERATION.fetch_add(1, Ordering::SeqCst);
        }
        result
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    fn set(&self, _mode: PowerMode) -> Result<(), PowerError> {
        Err(PowerError::Unsupported)
    }
}

/// Applies the published thread mode to the calling thread. Only the main event-loop thread and
/// the runtime's workers call this (I5). One atomic load when nothing changed; never panics,
/// never logs, never changes a thread's priority on Windows. A thread is restored only if it was
/// marked.
pub fn sync_current_thread() {
    #[cfg(any(windows, target_os = "macos"))]
    {
        let generation = GENERATION.load(Ordering::SeqCst);
        // `try_with`: a thread being torn down has no locals left; it is skipped.
        let _ = SEEN.try_with(|seen| {
            if seen.replace(generation) == generation {
                return;
            }
            let _ = SAVED.try_with(|saved| {
                if THREAD_ECO.load(Ordering::SeqCst) {
                    // Save once, before the first mark; a thread whose state cannot be read, or
                    // whose mark fails, stays unmarked and is never "restored".
                    if saved.get().is_none() {
                        saved.set(os::save_current());
                        if saved.get().is_some() && !os::thread_eco() {
                            saved.set(None);
                        }
                    } else {
                        let _ = os::thread_eco();
                    }
                } else if let Some(before) = saved.take() {
                    let _ = os::thread_restore(before);
                }
            });
        });
    }
}

/// Whether the calling thread is marked as saving power, read back from the OS; `None` when the
/// OS will not say or has no such mark. For the tests.
#[doc(hidden)]
pub fn current_thread_saving() -> Option<bool> {
    #[cfg(any(windows, target_os = "macos"))]
    {
        os::thread_saving()
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        None
    }
}

/// The process's power throttling `(ControlMask, StateMask)` and priority class (Windows only).
/// For the tests.
#[cfg(windows)]
#[doc(hidden)]
pub fn current_process_state() -> Option<(u32, u32, u32)> {
    os::process_state()
}
