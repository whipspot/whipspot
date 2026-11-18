//! The provider's ids: `ytmusic:track:<videoId>`, `ytmusic:playlist:<id>`,
//! `ytmusic:album:<MPRE browseId>` and `ytmusic:artist:<UC channelId>`. An id from the outside
//! is validated before it is used in a request, and an id from a response is validated before it
//! leaves as a URI (nothing else reaches a request body or a URI). Only a `UC` id is an artist
//! and only an `MPRE` id is an album.

pub const TRACK_PREFIX: &str = "ytmusic:track:";
pub const PLAYLIST_PREFIX: &str = "ytmusic:playlist:";
pub const ALBUM_PREFIX: &str = "ytmusic:album:";
pub const ARTIST_PREFIX: &str = "ytmusic:artist:";

/// What an album's browse id starts with.
const ALBUM_BROWSE_START: &str = "MPRE";
/// What an artist's channel id starts with.
const ARTIST_CHANNEL_START: &str = "UC";
/// What the id of an album's audio playlist (the one a save targets) starts with.
const AUDIO_PLAYLIST_START: &str = "OLAK5uy_";

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

/// The URI of an album's browse id: only an `MPRE` id is an album.
pub fn album_uri(browse_id: &str) -> Option<String> {
    browse_id
        .starts_with(ALBUM_BROWSE_START)
        .then(|| with_prefix(ALBUM_PREFIX, browse_id))
        .flatten()
}

pub fn parse_album_uri(uri: &str) -> Option<&str> {
    strip_valid(ALBUM_PREFIX, uri).filter(|id| id.starts_with(ALBUM_BROWSE_START))
}

/// The URI of an artist's channel id: only a `UC` id is an artist.
pub fn artist_uri(channel_id: &str) -> Option<String> {
    channel_id
        .starts_with(ARTIST_CHANNEL_START)
        .then(|| with_prefix(ARTIST_PREFIX, channel_id))
        .flatten()
}

pub fn parse_artist_uri(uri: &str) -> Option<&str> {
    strip_valid(ARTIST_PREFIX, uri).filter(|id| id.starts_with(ARTIST_CHANNEL_START))
}

/// The id of an album's audio playlist, or `None` if it is not one (a save never targets
/// anything else).
pub fn audio_playlist_id(id: &str) -> Option<&str> {
    (valid_id(id) && id.starts_with(AUDIO_PLAYLIST_START)).then_some(id)
}
