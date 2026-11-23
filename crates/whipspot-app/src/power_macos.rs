//! macOS calls for efficiency mode (the efficiency-mode task, R1-D3, R1-D4): a thread's own QoS
//! class, read and set by that thread.
//!
//! A granted exception to hard rule 7: FFI calls that have no safe binding. `whipspot-app`'s lint
//! table denies `unsafe_code`; the allow below is on this module alone. The calls act on the
//! calling thread only (`pthread_self`, `pthread_set_qos_class_self_np`); the only pointers passed
//! are to live stack locals of the right type, written during the call and never kept.
#![allow(unsafe_code)]

use std::ffi::c_int;

use libc::qos_class_t;

/// A thread's QoS class and relative priority before it was marked.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Saved {
    class: u32,
    relative: c_int,
}

/// The calling thread's QoS class as a raw number and its relative priority.
fn current() -> Option<(u32, c_int)> {
    // The class is read into a `u32`, not into `qos_class_t`: a value the enum does not list then
    // never becomes an invalid enum value. `qos_class_t` is `#[repr(u32)]`.
    let mut class: u32 = 0;
    let mut relative: c_int = 0;
    // SAFETY: `pthread_self` is the calling thread, alive for the call. Both pointers are to live,
    // writable stack locals; `class` has the size and alignment of `qos_class_t` (`repr(u32)`).
    // Nothing is kept after the call.
    let rc = unsafe {
        libc::pthread_get_qos_class_np(
            libc::pthread_self(),
            (&raw mut class).cast::<qos_class_t>(),
            &raw mut relative,
        )
    };
    (rc == 0).then_some((class, relative))
}

/// The class to set back for a saved raw value; one the OS cannot take back (UNSPECIFIED) or an
/// unknown one is DEFAULT.
fn class_from(raw: u32) -> qos_class_t {
    use qos_class_t::*;
    [
        QOS_CLASS_USER_INTERACTIVE,
        QOS_CLASS_USER_INITIATED,
        QOS_CLASS_DEFAULT,
        QOS_CLASS_UTILITY,
        QOS_CLASS_BACKGROUND,
    ]
    .into_iter()
    .find(|c| *c as u32 == raw)
    .unwrap_or(QOS_CLASS_DEFAULT)
}

fn set_self(class: qos_class_t, relative: c_int) -> bool {
    // SAFETY: acts on the calling thread only; both arguments are by value, no pointer is passed.
    unsafe { libc::pthread_set_qos_class_self_np(class, relative) == 0 }
}

/// The calling thread's class before marking; `None` when the OS will not say (the thread is then
/// not marked).
pub(crate) fn save_current() -> Option<Saved> {
    current().map(|(class, relative)| Saved { class, relative })
}

/// Mark the calling thread: QoS `utility`.
pub(crate) fn thread_eco() -> bool {
    set_self(qos_class_t::QOS_CLASS_UTILITY, 0)
}

/// Set the calling thread back to its saved class. An UNSPECIFIED saved class goes back as
/// DEFAULT with relative priority 0.
pub(crate) fn thread_restore(saved: Saved) -> bool {
    let class = class_from(saved.class);
    let relative = if class as u32 == saved.class {
        saved.relative
    } else {
        0
    };
    set_self(class, relative)
}

/// Whether the calling thread is at QoS `utility`; `None` when the OS will not say.
pub(crate) fn thread_saving() -> Option<bool> {
    current().map(|(class, _)| class == qos_class_t::QOS_CLASS_UTILITY as u32)
}
