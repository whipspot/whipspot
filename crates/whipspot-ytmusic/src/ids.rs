//! The provider's ids: `ytmusic:track:<videoId>`, `ytmusic:playlist:<id>` and
//! `ytmusic:album:<browseId>`. An id from the outside is validated before it is used in a
//! request, and an id from a response is validated before it leaves as a URI (nothing else
//! reaches a request body or a URI).

pub const TRACK_PREFIX: &str = "ytmusic:track:";
pub const PLAYLIST_PREFIX: &str = "ytmusic:playlist:";
pub const ALBUM_PREFIX: &str = "ytmusic:album:";

/// The playlist of liked songs, as YouTube Music names it.
pub const LIKED_PLAYLIST: &str = "LM";

/// `[A-Za-z0-9_-]{1,64}`.
pub fn valid_id(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn with_prefix(prefix: &str, id: &str) -> Option<String> {
    valid_id(id).then(|| format!("{prefix}{id}"))
}

fn strip_valid<'a>(prefix: &str, uri: &'a str) -> Option<&'a str> {
    uri.strip_prefix(prefix).filter(|id| valid_id(id))
}

/// The URI of a video id, or `None` if the id is not valid.
pub fn track_uri(video_id: &str) -> Option<String> {
    with_prefix(TRACK_PREFIX, video_id)
}

/// The video id of a track URI.
pub fn parse_track_uri(uri: &str) -> Option<&str> {
    strip_valid(TRACK_PREFIX, uri)
}

/// The playlist id of a response, without the `VL` a browse id puts in front.
pub fn playlist_id_of_browse_id(browse_id: &str) -> Option<&str> {
    let id = browse_id.strip_prefix("VL").unwrap_or(browse_id);
    valid_id(id).then_some(id)
}

pub fn playlist_uri(playlist_id: &str) -> Option<String> {
    with_prefix(PLAYLIST_PREFIX, playlist_id)
}

pub fn parse_playlist_uri(uri: &str) -> Option<&str> {
    strip_valid(PLAYLIST_PREFIX, uri)
}

pub fn album_uri(browse_id: &str) -> Option<String> {
    with_prefix(ALBUM_PREFIX, browse_id)
}

pub fn parse_album_uri(uri: &str) -> Option<&str> {
    strip_valid(ALBUM_PREFIX, uri)
}
