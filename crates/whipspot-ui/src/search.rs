//! The Search route's notes: the top bar owns the search field (MD54) and the track page draws
//! what the last query found; this tells when there is nothing to list yet.

use crate::library::LibraryModel;

/// What the Search route says instead of a list: the provider cannot search, or no query has run.
/// `None` once there are results (or a pending search) to list.
pub fn message(supported: bool, library: &LibraryModel) -> Option<&'static str> {
    if !supported {
        Some("Search is not available.")
    } else if library.search.is_none() {
        Some("Search for tracks by title, artist or album.")
    } else {
        None
    }
}
