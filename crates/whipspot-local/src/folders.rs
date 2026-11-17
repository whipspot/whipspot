//! [`MusicFolders`] over the provider's scanner (D10). Adding or removing a folder changes only
//! the list and starts a scan; nothing here writes, moves or deletes a file in a folder.

use std::sync::Arc;

use whipspot_core::folders::{FolderError, MusicFolders, ScanStatus};

use crate::Shared;
use crate::ids::path_key;

/// The music folders of a [`crate::LocalProvider`]; get one with
/// [`crate::LocalProvider::folders`].
pub struct LocalFolders {
    pub(crate) shared: Arc<Shared>,
}

/// `path` without trailing separators (a root such as `/` or `C:\` keeps its own).
fn trimmed(path: &str) -> String {
    let t = path.trim_end_matches(['/', '\\']);
    if t.is_empty() || t.ends_with(':') {
        path.to_owned()
    } else {
        t.to_owned()
    }
}

impl MusicFolders for LocalFolders {
    fn folders(&self) -> Vec<String> {
        self.shared.scan_state().folders.clone()
    }

    fn add(&self, path: String) -> Result<(), FolderError> {
        if path.trim().is_empty() || path.chars().any(char::is_control) {
            return Err(FolderError::NotAFolder);
        }
        if !std::fs::metadata(&path).is_ok_and(|m| m.is_dir()) {
            return Err(FolderError::NotAFolder);
        }
        let path = trimmed(&path);
        let key = path_key(&path);
        {
            let mut state = self.shared.scan_state();
            if state.folders.iter().any(|f| path_key(f) == key) {
                return Err(FolderError::AlreadyAdded);
            }
            state.folders.push(path);
        }
        crate::rescan(&self.shared);
        Ok(())
    }

    fn remove(&self, path: &str) {
        let key = path_key(path);
        let removed = {
            let mut state = self.shared.scan_state();
            let before = state.folders.len();
            state.folders.retain(|f| path_key(f) != key);
            state.folders.len() != before
        };
        if removed {
            crate::rescan(&self.shared);
        }
    }

    fn rescan(&self) {
        crate::rescan(&self.shared);
    }

    fn status(&self) -> ScanStatus {
        crate::scan_status(&self.shared)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_trailing_separators_but_keeps_roots() {
        assert_eq!(trimmed("/music/"), "/music");
        assert_eq!(trimmed("/"), "/");
        assert_eq!(trimmed(r"C:\Music\\"), r"C:\Music");
        assert_eq!(trimmed(r"C:\"), r"C:\");
    }
}
