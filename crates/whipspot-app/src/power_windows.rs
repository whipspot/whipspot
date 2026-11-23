//! Windows calls for efficiency mode (the efficiency-mode task, R1-D3): process and thread
//! power throttling (EcoQoS) and the process priority class.
//!
//! A granted exception to hard rule 7: FFI calls that have no safe binding. `whipspot-app`'s lint
//! table denies `unsafe_code`; the allow below is on this module alone. Every call takes a
//! pseudo-handle (`GetCurrentProcess`, `GetCurrentThread`, which need no closing) and, where it
//! takes a pointer, a live stack struct of exactly the size passed; none keeps a pointer.
#![allow(unsafe_code)]

use std::ffi::c_void;

use whipspot_core::power::PowerError;
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentThread, GetPriorityClass, GetProcessInformation,
    GetThreadInformation, IDLE_PRIORITY_CLASS, NORMAL_PRIORITY_CLASS,
    PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
    PROCESS_POWER_THROTTLING_STATE, ProcessPowerThrottling, SetPriorityClass,
    SetProcessInformation, SetThreadInformation, THREAD_POWER_THROTTLING_CURRENT_VERSION,
    THREAD_POWER_THROTTLING_EXECUTION_SPEED, THREAD_POWER_THROTTLING_STATE, ThreadPowerThrottling,
};

/// What a thread saves before it is marked: nothing, the unmark is `ControlMask = 0`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Saved;

/// The process's priority class now; a failed read (0) is `NORMAL_PRIORITY_CLASS`, so 0 is never
/// set back.
pub(crate) fn priority_class() -> u32 {
    // SAFETY: `GetCurrentProcess` returns a pseudo-handle that is always valid for the calling
    // process and needs no closing; `GetPriorityClass` reads no pointer of ours.
    let class = unsafe { GetPriorityClass(GetCurrentProcess()) };
    if class == 0 {
        NORMAL_PRIORITY_CLASS
    } else {
        class
    }
}

/// `idle`: process EcoQoS on and the idle priority class. Otherwise: power throttling handed back
/// to the system and `class` restored. When leaving, the priority class goes back first (the idle
/// class must never outlive EcoIdle, I1); both calls are always tried, and either failing is
/// `Refused`.
pub(crate) fn set_process(idle: bool, class: u32) -> Result<(), PowerError> {
    let mask = if idle {
        PROCESS_POWER_THROTTLING_EXECUTION_SPEED
    } else {
        0
    };
    let state = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: mask,
        StateMask: mask,
    };
    let throttle = || {
        // SAFETY: a pseudo-handle for this process; the pointer is to `state`, a live stack struct
        // whose exact size is passed, read during the call only.
        unsafe {
            SetProcessInformation(
                GetCurrentProcess(),
                ProcessPowerThrottling,
                (&raw const state).cast::<c_void>(),
                size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
            ) != 0
        }
    };
    let set_class = || {
        let class = if idle { IDLE_PRIORITY_CLASS } else { class };
        // SAFETY: a pseudo-handle for this process and a flag word by value; no pointer is read.
        unsafe { SetPriorityClass(GetCurrentProcess(), class) != 0 }
    };
    let (first, second) = if idle {
        (throttle(), set_class())
    } else {
        let restored = set_class();
        (restored, throttle())
    };
    if first && second {
        Ok(())
    } else {
        Err(PowerError::Refused)
    }
}

/// Nothing to save on Windows; always `Some`.
pub(crate) fn save_current() -> Option<Saved> {
    Some(Saved)
}

/// Mark the calling thread: thread EcoQoS on. Its priority is not touched.
pub(crate) fn thread_eco() -> bool {
    set_thread_mask(THREAD_POWER_THROTTLING_EXECUTION_SPEED)
}

/// Unmark the calling thread: thread throttling handed back to the system.
pub(crate) fn thread_restore(_saved: Saved) -> bool {
    set_thread_mask(0)
}

fn set_thread_mask(mask: u32) -> bool {
    let state = THREAD_POWER_THROTTLING_STATE {
        Version: THREAD_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: mask,
        StateMask: mask,
    };
    // SAFETY: `GetCurrentThread` is a pseudo-handle for the calling thread, valid for the call and
    // needing no closing; the pointer is to `state`, a live stack struct whose exact size is
    // passed, read during the call only.
    unsafe {
        SetThreadInformation(
            GetCurrentThread(),
            ThreadPowerThrottling,
            (&raw const state).cast::<c_void>(),
            size_of::<THREAD_POWER_THROTTLING_STATE>() as u32,
        ) != 0
    }
}

/// Whether the calling thread has thread EcoQoS on; `None` when the OS will not say.
pub(crate) fn thread_saving() -> Option<bool> {
    let mut state = THREAD_POWER_THROTTLING_STATE {
        Version: THREAD_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: 0,
        StateMask: 0,
    };
    // SAFETY: a pseudo-handle for the calling thread; the pointer is to `state`, a live, writable
    // stack struct whose exact size is passed, written during the call only.
    let ok = unsafe {
        GetThreadInformation(
            GetCurrentThread(),
            ThreadPowerThrottling,
            (&raw mut state).cast::<c_void>(),
            size_of::<THREAD_POWER_THROTTLING_STATE>() as u32,
        ) != 0
    };
    ok.then_some(state.StateMask & THREAD_POWER_THROTTLING_EXECUTION_SPEED != 0)
}

/// The process's power throttling `(ControlMask, StateMask)` and priority class; `None` when the
/// OS will not say.
pub(crate) fn process_state() -> Option<(u32, u32, u32)> {
    let mut state = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: 0,
        StateMask: 0,
    };
    // SAFETY: a pseudo-handle for this process; the pointer is to `state`, a live, writable stack
    // struct whose exact size is passed, written during the call only.
    let ok = unsafe {
        GetProcessInformation(
            GetCurrentProcess(),
            ProcessPowerThrottling,
            (&raw mut state).cast::<c_void>(),
            size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        ) != 0
    };
    // SAFETY: as in `priority_class`.
    let class = unsafe { GetPriorityClass(GetCurrentProcess()) };
    (ok && class != 0).then_some((state.ControlMask, state.StateMask, class))
}
