//! The app: the window host, the app's files (paths, settings, log) and the pieces the
//! composition root in `main.rs` wires together.

pub mod host;
pub mod logging;
pub mod paths;
#[cfg(target_os = "macos")]
pub(crate) mod platform_macos;
#[cfg(windows)]
pub mod platform_windows;
pub mod power;
#[cfg(target_os = "macos")]
mod power_macos;
#[cfg(windows)]
mod power_windows;
pub mod selftest;
pub mod settings;

/// The app's own version (this crate's), which About shows and the updater reports.
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_version_is_the_app_crates_not_the_ui_crates() {
        assert_eq!(APP_VERSION, env!("CARGO_PKG_VERSION"));
        assert_ne!(APP_VERSION, "0.1.0");
        assert!(!APP_VERSION.is_empty());
    }
}
