//! Where the app keeps its files (D33, MD173: `~/.whipspot`). Nothing here creates a directory.

use std::path::{Path, PathBuf};

/// The app's three directories. [`AppPaths::from_os`] gives the real layout; [`AppPaths::under`]
/// a scratch layout for tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    /// Holds `settings.json`.
    pub config_dir: PathBuf,
    /// Holds `whipspot.log` and the previous run's `whipspot.1.log`.
    pub log_dir: PathBuf,
    /// Disposable downloads (cover art lives in `art` below it, MD80); deleting it is always safe.
    pub cache_dir: PathBuf,
}

impl AppPaths {
    /// The real layout for this OS, or `None` when a root directory is unknown.
    pub fn from_os() -> Option<Self> {
        let (config_dir, log_dir, cache_dir) = os_roots()?;
        Some(Self {
            config_dir,
            log_dir,
            cache_dir,
        })
    }

    /// A layout entirely below `base` (tests).
    pub fn under(base: &Path) -> Self {
        Self {
            config_dir: base.join("config"),
            log_dir: base.join("logs"),
            cache_dir: base.join("cache"),
        }
    }

    /// `config_dir/settings.json`.
    pub fn settings_file(&self) -> PathBuf {
        self.config_dir.join("settings.json")
    }

    /// `config_dir/tools`: the stream helper's downloads (yt-dlp and the script runtime), its
    /// scratch cookie files and its cache. The helper creates what it needs below it.
    pub fn tools_dir(&self) -> PathBuf {
        self.config_dir.join("tools")
    }

    /// `config_dir/skins`: the user's skins folder (D33, MD100). The app creates it and only
    /// lists and reads what is in it.
    pub fn skins_dir(&self) -> PathBuf {
        self.config_dir.join("skins")
    }
}

/// The OS Music folder (D12): the first-start music folder. `None` when the OS has none or it
/// does not exist; nothing is created.
pub fn os_music_dir() -> Option<PathBuf> {
    dirs::audio_dir().filter(|p| p.is_dir())
}

/// The app's folder in the user's home: `~/.whipspot` (`%USERPROFILE%\.whipspot` on Windows),
/// with `logs` and `cache` below it (MD173).
const HOME_DIR: &str = ".whipspot";

/// The layout below `home`: settings, favourites and skins in `home/.whipspot`, the logs in its
/// `logs` and the cache in its `cache`.
fn in_home(home: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let root = home.join(HOME_DIR);
    (root.clone(), root.join("logs"), root.join("cache"))
}

/// The same layout on every OS, in the user's home directory.
fn os_roots() -> Option<(PathBuf, PathBuf, PathBuf)> {
    Some(in_home(&dirs::home_dir()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn under_puts_every_directory_below_the_base() {
        let base = Path::new("scratch");
        let paths = AppPaths::under(base);
        assert_eq!(paths.config_dir, base.join("config"));
        assert_eq!(paths.log_dir, base.join("logs"));
        assert_eq!(paths.cache_dir, base.join("cache"));
    }

    #[test]
    fn the_real_layout_is_one_dot_folder_in_the_home() {
        let home = Path::new("home");
        let (config, logs, cache) = in_home(home);
        assert_eq!(config, home.join(".whipspot"));
        assert_eq!(logs, home.join(".whipspot").join("logs"));
        assert_eq!(cache, home.join(".whipspot").join("cache"));
    }

    #[test]
    fn tools_dir_is_below_the_config_dir_and_in_the_real_layout_is_dot_whipspot_tools() {
        let paths = AppPaths::under(Path::new("scratch"));
        assert_eq!(
            paths.tools_dir(),
            Path::new("scratch").join("config").join("tools")
        );
        let (config, _, _) = in_home(Path::new("home"));
        let real = AppPaths {
            config_dir: config,
            log_dir: PathBuf::new(),
            cache_dir: PathBuf::new(),
        };
        assert_eq!(
            real.tools_dir(),
            Path::new("home").join(".whipspot").join("tools")
        );
    }

    #[test]
    fn skins_dir_is_below_the_config_dir() {
        let paths = AppPaths::under(Path::new("scratch"));
        assert_eq!(
            paths.skins_dir(),
            Path::new("scratch").join("config").join("skins")
        );
    }
}
