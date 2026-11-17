//! The music folders seam (C8): the list of folders a file-based provider scans, and its scan
//! progress. The UI drives it; the adapter that scans implements it. Removing a folder only
//! forgets it: nothing here ever touches the user's files.

use std::fmt;

/// Why [`MusicFolders::add`] refused a folder. The texts are fixed and name no path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderError {
    /// The path does not exist or is not a directory.
    NotAFolder,
    /// The folder is in the list already (after normalising a trailing separator, and case on
    /// Windows).
    AlreadyAdded,
}

impl FolderError {
    pub const fn text(self) -> &'static str {
        match self {
            FolderError::NotAFolder => "this is not a folder",
            FolderError::AlreadyAdded => "this folder is already in the list",
        }
    }
}

impl fmt::Display for FolderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.text())
    }
}

impl std::error::Error for FolderError {}

/// Where a scan stands.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScanStatus {
    /// A scan is running (or queued).
    pub scanning: bool,
    /// Audio files looked at so far in the running scan; the last scan's total when idle.
    pub files_seen: u32,
    /// Tracks in the library now.
    pub tracks: u32,
}

/// The folders a file-based provider scans. Held as `Arc<dyn MusicFolders>` and shared between
/// the UI and the provider; every method returns at once and never blocks on disk work (a scan
/// runs in the background).
///
/// Never panics; paths are text as the OS gave them.
pub trait MusicFolders: Send + Sync {
    /// The folders, in the order they were added.
    fn folders(&self) -> Vec<String>;
    /// Add a folder and start a scan. [`FolderError::NotAFolder`] unless `path` is an existing
    /// directory; [`FolderError::AlreadyAdded`] for a folder already listed.
    fn add(&self, path: String) -> Result<(), FolderError>;
    /// Forget a folder and drop its tracks from the library. An unknown folder is ignored.
    fn remove(&self, path: &str);
    /// Start a scan of every folder. Returns at once; see [`MusicFolders::status`].
    fn rescan(&self);
    fn status(&self) -> ScanStatus;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_error_texts_are_fixed() {
        for e in [FolderError::NotAFolder, FolderError::AlreadyAdded] {
            assert_eq!(e.to_string(), e.text());
            assert!(!e.text().is_empty());
        }
        assert_ne!(
            FolderError::NotAFolder.text(),
            FolderError::AlreadyAdded.text()
        );
    }

    #[test]
    fn scan_status_starts_idle_and_empty() {
        assert_eq!(
            ScanStatus::default(),
            ScanStatus {
                scanning: false,
                files_seen: 0,
                tracks: 0
            }
        );
    }
}
