//! An in-memory [`MusicFolders`]: the list lives in a field, a rescan "runs" for a few
//! milliseconds, and nothing is read from the disk beyond the directory check of `add`.

use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use whipspot_core::folders::{FolderError, MusicFolders, ScanStatus};

/// How long a scan reports `scanning` after `add`, `remove` or `rescan`.
const SCAN_TIME: Duration = Duration::from_millis(20);

/// Tracks the fake claims per folder.
const TRACKS_PER_FOLDER: u32 = 10;

#[derive(Default)]
struct State {
    folders: Vec<String>,
    scan_until: Option<Instant>,
}

/// A fake music folders list. `add` accepts an existing directory; two paths are the same folder
/// when they differ only by trailing separators or, on Windows, case and slash direction.
#[derive(Default)]
pub struct FakeFolders {
    state: Mutex<State>,
}

impl FakeFolders {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start a scan: it reads as running for [`SCAN_TIME`].
    fn start_scan(state: &mut State) {
        state.scan_until = Some(Instant::now() + SCAN_TIME);
    }
}

/// The comparison key of a folder path.
fn key(path: &str) -> String {
    let mut k = path.to_owned();
    if cfg!(windows) {
        k = k.replace('/', "\\").to_lowercase();
    }
    while k.len() > 1 && k.ends_with(std::path::is_separator) {
        // A drive root ("c:\") keeps its separator.
        if k.len() == 3 && k.as_bytes()[1] == b':' {
            break;
        }
        k.pop();
    }
    k
}

impl MusicFolders for FakeFolders {
    fn folders(&self) -> Vec<String> {
        self.state.lock().unwrap().folders.clone()
    }

    fn add(&self, path: String) -> Result<(), FolderError> {
        if path.is_empty() || !Path::new(&path).is_dir() {
            return Err(FolderError::NotAFolder);
        }
        let mut state = self.state.lock().unwrap();
        let wanted = key(&path);
        if state.folders.iter().any(|f| key(f) == wanted) {
            return Err(FolderError::AlreadyAdded);
        }
        state.folders.push(path);
        Self::start_scan(&mut state);
        Ok(())
    }

    fn remove(&self, path: &str) {
        let mut state = self.state.lock().unwrap();
        let wanted = key(path);
        let before = state.folders.len();
        state.folders.retain(|f| key(f) != wanted);
        if state.folders.len() != before {
            Self::start_scan(&mut state);
        }
    }

    fn rescan(&self) {
        Self::start_scan(&mut self.state.lock().unwrap());
    }

    fn status(&self) -> ScanStatus {
        let state = self.state.lock().unwrap();
        let tracks = state.folders.len() as u32 * TRACKS_PER_FOLDER;
        ScanStatus {
            scanning: state.scan_until.is_some_and(|t| Instant::now() < t),
            files_seen: tracks,
            tracks,
        }
    }
}
