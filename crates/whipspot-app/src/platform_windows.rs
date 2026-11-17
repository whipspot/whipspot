//! Windows start-up hardening (M2-P1, D5): restrict where DLLs are searched for before the GPU
//! stack loads anything.
//!
//! This is the only `unsafe` in our crates (hard rule 7): one FFI call that has no safe binding.
//! `whipspot-app`'s lint table denies `unsafe_code` (a `forbid` cannot be relaxed per module);
//! the allow below is on this module alone.
#![allow(unsafe_code)]

use windows_sys::Win32::System::LibraryLoader::{
    LOAD_LIBRARY_SEARCH_DEFAULT_DIRS, SetDefaultDllDirectories,
};

/// Limits the process's DLL search to the application directory, `System32` and directories
/// added on purpose, so the current directory and `PATH` are never searched for a DLL that a
/// later load (wgpu's graphics backends, the audio stack) asks for by name. Call it first in
/// `main`, before any window or GPU use. Returns whether the OS accepted it; a failure leaves
/// the default search order in place and the app runs on.
pub fn restrict_dll_search() -> bool {
    // SAFETY: `SetDefaultDllDirectories` takes one flag word by value, reads no pointer and
    // keeps no reference to our memory; `LOAD_LIBRARY_SEARCH_DEFAULT_DIRS` is a valid flag for
    // it. A nonzero result means success; zero is handled by the caller.
    unsafe { SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_DEFAULT_DIRS) != 0 }
}
