//! The app: the window host, the app's files (paths, settings, log) and the pieces the
//! composition root in `main.rs` wires together.

pub mod host;
pub mod logging;
pub mod paths;
#[cfg(windows)]
pub mod platform_windows;
pub mod selftest;
pub mod settings;
