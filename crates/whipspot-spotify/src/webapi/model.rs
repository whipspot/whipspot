//! Response parsing for the library endpoints. Everything is defensive: unknown fields are
//! ignored, strings are capped, and an entry that does not validate is skipped and counted (fail
//! closed), never repaired. The page shape itself (`items` array) is required.
//!
//! Fixtures in the tests are synthetic, modelled on Spotify's docs and on the key names the live
//! probe reported; no real response body was ever captured.

use serde_json::Value;

use crate::uri::{is_spotify_id, parse_track_uri};

/// Longest string kept for a name, artist or album.
pub const MAX_TEXT_CHARS: usize = 200;
/// Rows loaded per source are capped here; paging stops and the page says `truncated`.
pub const MAX_ROWS: u32 = 2000;
/// Spotify serves a search only up to this offset + limit.
pub const SEARCH_MAX_ROWS: u32 = 1000;

pub use whipspot_core::provider::model::{
    Album, AlbumPage, ImageRef, MAX_IMAGES, Playlist, PlaylistPage, Readable, Track, TrackPage,
};
use whipspot_core::text::display_text;

/// The response did not have the shape of a page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnexpectedShape;

/// A string value made safe to draw (`display_text`: control characters become spaces, bidi
/// controls and zero-width characters are removed), capped to [`MAX_TEXT_CHARS`] chars. A missing
/// or non-string value is the empty string.
fn text(v: Option<&Value>) -> String {
    v.and_then(Value::as_str)
        .map(|s| {
            // Bound the work on a hostile giant string before cleaning it: removed characters
            // only shorten the result, so a prefix of 4x the cap is always enough.
            let prefix: String = s.chars().take(MAX_TEXT_CHARS * 4).collect();
            display_text(&prefix, MAX_TEXT_CHARS)
        })
        .unwrap_or_default()
}

/// Longest image URL kept (NetArt re-checks every URL; this is a prefilter).
const MAX_IMAGE_URL_BYTES: usize = 2048;
/// Largest image side kept as known; a bigger or non-integer value becomes unknown.
const MAX_IMAGE_SIDE: u64 = 10_000;

fn image_side(v: Option<&Value>) -> Option<u32> {
    v.and_then(Value::as_u64)
        .filter(|n| (1..=MAX_IMAGE_SIDE).contains(n))
        .and_then(|n| u32::try_from(n).ok())
}

/// Cover refs out of an `images` array: entries whose `url` is a string of 1..=2,048 bytes
/// beginning `https://`, at most [`MAX_IMAGES`], in order. Anything else gives an empty list.
fn parse_images(v: Option<&Value>) -> Vec<ImageRef> {
    let Some(arr) = v.and_then(Value::as_array) else {
        return vec![];
    };
    arr.iter()
        .filter_map(|e| {
            let url = e.get("url")?.as_str()?;
            if url.is_empty() || url.len() > MAX_IMAGE_URL_BYTES || !url.starts_with("https://") {
                return None;
            }
            Some(ImageRef {
                url: url.to_owned(),
                width: image_side(e.get("width")),
                height: image_side(e.get("height")),
            })
        })
        .take(MAX_IMAGES)
        .collect()
}

/// The year out of a `release_date` (`1981-12-15`, `1981-12`, `1981`): the leading four ASCII
/// digits when they make 1..=9999, else None.
fn parse_year(v: Option<&Value>) -> Option<u16> {
    let b = v.and_then(Value::as_str)?.as_bytes();
    let digits = b.get(..4)?;
    if !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let year = digits
        .iter()
        .fold(0u16, |acc, d| acc * 10 + u16::from(d - b'0'));
    (year >= 1).then_some(year)
}

fn as_u32(v: Option<&Value>) -> Option<u32> {
    v.and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
}

/// The strict form: exactly `spotify:track:<22 base62>` (a bare id is not accepted here).
fn strict_track_uri(v: Option<&Value>) -> Option<String> {
    let s = v.and_then(Value::as_str)?;
    parse_track_uri(s).filter(|canon| canon == s)
}

/// `item` if it is an object, else `track` if it is an object (`item` wins when both exist).
fn entry_object(entry: &Value) -> Option<&Value> {
    ["item", "track"]
        .iter()
        .filter_map(|k| entry.get(k))
        .find(|v| v.is_object())
}

fn parse_track(obj: &Value) -> Option<Track> {
    if obj.get("is_local").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    if let Some(t) = obj.get("type") {
        if t.as_str() != Some("track") {
            return None;
        }
    }
    let uri = strict_track_uri(obj.get("uri"))?;
    let artist = obj
        .get("artists")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|x| text(x.get("name")))
                .filter(|n| !n.is_empty())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    let duration_ms = obj
        .get("duration_ms")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(u64::from(u32::MAX));
    Some(Track {
        uri,
        name: text(obj.get("name")),
        artist: artist.chars().take(MAX_TEXT_CHARS).collect(),
        album: text(obj.get("album").and_then(|a| a.get("name"))),
        duration_ms,
        images: parse_images(obj.get("album").and_then(|a| a.get("images"))),
        year: parse_year(obj.get("album").and_then(|a| a.get("release_date"))),
    })
}

/// The part shared by every page: the `items` array, the next offset and the total.
struct Frame<'a> {
    entries: &'a [Value],
    next_offset: Option<u32>,
    total: Option<u32>,
    truncated: bool,
}

fn page_frame(page: &Value, offset: u32) -> Result<Frame<'_>, UnexpectedShape> {
    let items = page
        .get("items")
        .and_then(Value::as_array)
        .ok_or(UnexpectedShape)?;
    // The response's `next` URL is only read as "there is more": paging is by offset, computed
    // here, so a hostile `next` can never redirect a request that carries the bearer token.
    let has_more = page.get("next").is_some_and(Value::is_string);
    let raw = u32::try_from(items.len()).unwrap_or(u32::MAX);
    let candidate = offset.saturating_add(raw);
    // An empty page that claims more would loop forever.
    let next = (has_more && raw > 0).then_some(candidate);
    let truncated = next.is_some_and(|n| n >= MAX_ROWS);
    Ok(Frame {
        entries: items,
        next_offset: next.filter(|n| *n < MAX_ROWS),
        total: as_u32(page.get("total")),
        truncated,
    })
}

/// Parse a page of playlist items or liked songs (`item` / `track` keys both accepted).
pub fn parse_track_page(page: &Value, offset: u32) -> Result<TrackPage, UnexpectedShape> {
    parse_tracks(page, offset, entry_object)
}

/// Parse a page whose entries are track objects themselves (`/albums/{id}/tracks`). A simplified
/// track has no `album` object, so `album` comes back empty; the caller knows the album.
pub fn parse_album_tracks_page(page: &Value, offset: u32) -> Result<TrackPage, UnexpectedShape> {
    parse_tracks(page, offset, |e| e.is_object().then_some(e))
}

/// Parse `/search?type=track`: the page is the response's `tracks` object, entries are tracks.
/// Spotify serves at most [`SEARCH_MAX_ROWS`] results for a query, so paging stops there.
pub fn parse_search_page(resp: &Value, offset: u32) -> Result<TrackPage, UnexpectedShape> {
    let page = resp.get("tracks").ok_or(UnexpectedShape)?;
    let mut r = parse_tracks(page, offset, |e| e.is_object().then_some(e))?;
    if r.next_offset.is_some_and(|n| n >= SEARCH_MAX_ROWS) {
        r.next_offset = None;
        r.truncated = true;
    }
    Ok(r)
}

fn parse_tracks(
    page: &Value,
    offset: u32,
    object: impl Fn(&Value) -> Option<&Value>,
) -> Result<TrackPage, UnexpectedShape> {
    let Frame {
        entries,
        next_offset,
        total,
        truncated,
    } = page_frame(page, offset)?;
    let mut items = Vec::new();
    let mut skipped = 0u32;
    for e in entries {
        match object(e).and_then(parse_track) {
            Some(t) => items.push(t),
            None => skipped += 1,
        }
    }
    Ok(TrackPage {
        items,
        skipped,
        next_offset,
        total,
        truncated,
    })
}

/// Cosmetic prediction (U2): owner is the current user or the list is collaborative -> readable;
/// both fields present and neither holds -> unreadable; anything missing -> unknown.
pub fn predict_readable(entry: &Value, me: &str) -> Readable {
    let owner = entry
        .get("owner")
        .and_then(|o| o.get("id"))
        .and_then(Value::as_str);
    let collaborative = entry.get("collaborative").and_then(Value::as_bool);
    if owner == Some(me) || collaborative == Some(true) {
        Readable::Readable
    } else if owner.is_some() && collaborative.is_some() {
        Readable::Unreadable
    } else {
        Readable::Unknown
    }
}

pub fn parse_playlist_page(
    page: &Value,
    offset: u32,
    me: &str,
) -> Result<PlaylistPage, UnexpectedShape> {
    let Frame {
        entries,
        next_offset,
        total,
        ..
    } = page_frame(page, offset)?;
    let mut items = Vec::new();
    let mut skipped = 0u32;
    for e in entries {
        let id = e
            .get("id")
            .and_then(Value::as_str)
            .filter(|i| is_spotify_id(i));
        let Some(id) = id else {
            skipped += 1;
            continue;
        };
        // Spotify's migration renames `tracks` to `items` in places; accept either, cosmetic only.
        let track_count = as_u32(e.get("tracks").and_then(|t| t.get("total")))
            .or_else(|| as_u32(e.get("items").and_then(|t| t.get("total"))));
        items.push(Playlist {
            id: id.to_owned(),
            name: text(e.get("name")),
            readable: predict_readable(e, me),
            track_count,
            images: parse_images(e.get("images")),
        });
    }
    Ok(PlaylistPage {
        items,
        skipped,
        next_offset,
        total,
    })
}

/// `/me/albums`: entries are `{added_at, album: {id, name, artists, total_tracks}}`. An entry
/// without an `album` object or with a bad id is skipped and counted.
pub fn parse_album_page(page: &Value, offset: u32) -> Result<AlbumPage, UnexpectedShape> {
    let Frame {
        entries,
        next_offset,
        total,
        ..
    } = page_frame(page, offset)?;
    let mut items = Vec::new();
    let mut skipped = 0u32;
    for e in entries {
        let album = e.get("album").filter(|a| a.is_object());
        let id = album
            .and_then(|a| a.get("id"))
            .and_then(Value::as_str)
            .filter(|i| is_spotify_id(i));
        let (Some(album), Some(id)) = (album, id) else {
            skipped += 1;
            continue;
        };
        let artist = album
            .get("artists")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|x| text(x.get("name")))
                    .filter(|n| !n.is_empty())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        items.push(Album {
            id: id.to_owned(),
            name: text(album.get("name")),
            artist: artist.chars().take(MAX_TEXT_CHARS).collect(),
            track_count: as_u32(album.get("total_tracks")),
            images: parse_images(album.get("images")),
            year: parse_year(album.get("release_date")),
        });
    }
    Ok(AlbumPage {
        items,
        skipped,
        next_offset,
        total,
    })
}

/// What a look at the user's library compares: the playlists' version stamps and the liked
/// songs' total and newest `added_at`. Opaque strings, kept only to be compared (never shown).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprint {
    /// `(playlist id, snapshot_id)`, sorted by id; an entry with a bad id is left out.
    pub playlists: Vec<PlaylistStamp>,
    pub liked_total: u32,
    /// The `added_at` of the newest liked song; `None` when there is none.
    pub liked_newest: Option<String>,
}

/// `(playlist id, snapshot_id)`.
pub type PlaylistStamp = (String, String);

/// Longest stamp kept (a `snapshot_id` is about 60 characters).
const MAX_STAMP_CHARS: usize = 128;

fn stamp(v: Option<&Value>) -> String {
    v.and_then(Value::as_str)
        .map(|s| s.chars().take(MAX_STAMP_CHARS).collect())
        .unwrap_or_default()
}

/// One page of `/me/playlists` reduced to `(id, snapshot_id)` pairs, and the offset of the next
/// page if there is one. Same page rules as [`parse_playlist_page`]; an entry whose id is not a
/// Spotify id is skipped.
pub fn parse_playlist_stamps(
    page: &Value,
    offset: u32,
) -> Result<(Vec<PlaylistStamp>, Option<u32>), UnexpectedShape> {
    let Frame {
        entries,
        next_offset,
        ..
    } = page_frame(page, offset)?;
    let stamps = entries
        .iter()
        .filter_map(|e| {
            let id = e
                .get("id")
                .and_then(Value::as_str)
                .filter(|i| is_spotify_id(i))?;
            Some((id.to_owned(), stamp(e.get("snapshot_id"))))
        })
        .collect();
    Ok((stamps, next_offset))
}

/// `/me/tracks?limit=1`: the liked total (required) and the newest entry's `added_at`.
pub fn parse_liked_stamp(page: &Value) -> Result<(u32, Option<String>), UnexpectedShape> {
    let items = page
        .get("items")
        .and_then(Value::as_array)
        .ok_or(UnexpectedShape)?;
    let total = as_u32(page.get("total")).ok_or(UnexpectedShape)?;
    let newest = items
        .first()
        .and_then(|e| e.get("added_at"))
        .and_then(Value::as_str)
        .map(|s| s.chars().take(MAX_STAMP_CHARS).collect());
    Ok((total, newest))
}

/// The id out of a `/v1/me` response, capped; `None` if absent or empty.
pub fn parse_me(v: &Value) -> Option<String> {
    let id: String = text(v.get("id"));
    (!id.is_empty()).then_some(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const A: &str = "4uLU6hMCjMI75M1A2tKUQC";
    const B: &str = "0VjIjW4GlUZAMYd2vXMi3b";

    fn track(id: &str, name: &str) -> Value {
        json!({
            "type": "track", "uri": format!("spotify:track:{id}"), "name": name,
            "duration_ms": 215_000, "is_local": false,
            "artists": [{"name": "Artist One"}, {"name": "Artist Two"}],
            "album": {"name": "The Album"},
        })
    }

    fn page(entries: Vec<Value>, next: Value) -> Value {
        json!({ "items": entries, "total": 3, "next": next })
    }

    #[test]
    fn item_only_track_only_and_both() {
        let p = page(
            vec![
                json!({"added_at": "x", "item": track(A, "via item")}),
                json!({"added_at": "x", "track": track(B, "via track")}),
                json!({"item": track(A, "item wins"), "track": track(B, "track loses")}),
            ],
            Value::Null,
        );
        let r = parse_track_page(&p, 0).unwrap();
        assert_eq!(r.skipped, 0);
        let names: Vec<_> = r.items.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["via item", "via track", "item wins"]);
        assert_eq!(r.items[2].uri, format!("spotify:track:{A}"));
        assert_eq!(r.items[0].artist, "Artist One, Artist Two");
        assert_eq!(r.items[0].album, "The Album");
        assert_eq!(r.items[0].duration_ms, 215_000);
        assert_eq!(r.total, Some(3));
        assert_eq!(r.next_offset, None);
    }

    #[test]
    fn non_object_item_falls_back_to_track() {
        let p = page(
            vec![json!({"item": null, "track": track(A, "fallback")})],
            Value::Null,
        );
        let r = parse_track_page(&p, 0).unwrap();
        assert_eq!(r.items.len(), 1);
        assert_eq!(r.items[0].name, "fallback");
    }

    #[test]
    fn nulls_episodes_locals_and_bad_uris_are_skipped_and_counted() {
        let mut episode = track(A, "ep");
        episode["type"] = json!("episode");
        episode["uri"] = json!(format!("spotify:episode:{A}"));
        let mut local = track(A, "local");
        local["is_local"] = json!(true);
        let mut local_uri = track(A, "local uri");
        local_uri["uri"] = json!("spotify:local:Artist:Album:Title:215");
        let bare = json!({"item": {"type": "track", "uri": A, "name": "bare id"}});
        let long = json!({"item": {"type": "track", "uri": format!("spotify:track:{A}x"), "name": "long"}});
        let newline =
            json!({"item": {"type": "track", "uri": format!("spotify:track:{A}\n"), "name": "nl"}});
        let no_uri = json!({"item": {"type": "track", "name": "no uri"}});
        let uri_number = json!({"item": {"type": "track", "uri": 5, "name": "num"}});
        let p = page(
            vec![
                json!({"item": null}),
                json!({"track": null}),
                json!({}),
                json!("string entry"),
                json!({"item": episode}),
                json!({"item": local}),
                json!({"item": local_uri}),
                bare,
                long,
                newline,
                no_uri,
                uri_number,
                json!({"item": track(B, "good")}),
            ],
            Value::Null,
        );
        let r = parse_track_page(&p, 0).unwrap();
        assert_eq!(r.items.len(), 1);
        assert_eq!(r.items[0].name, "good");
        assert_eq!(r.skipped, 12);
    }

    #[test]
    fn missing_fields_default_and_text_is_capped() {
        let long = "x".repeat(500);
        let mut t = track(A, &long);
        t["artists"] = json!([{"name": long}, {"name": long}, {"nope": 1}, {"name": ""}]);
        t["album"] = json!({"name": "a\u{0}b\nc"});
        t["duration_ms"] = json!(-4);
        let r = parse_track_page(&page(vec![json!({"item": t})], Value::Null), 0).unwrap();
        let t = &r.items[0];
        assert_eq!(t.name.chars().count(), MAX_TEXT_CHARS);
        assert_eq!(t.artist.chars().count(), MAX_TEXT_CHARS);
        assert_eq!(t.album, "a b c");
        assert_eq!(t.duration_ms, 0);
        let bare = json!({"item": {"type": "track", "uri": format!("spotify:track:{A}")}});
        let r = parse_track_page(&page(vec![bare], Value::Null), 0).unwrap();
        assert_eq!(
            (
                r.items[0].name.as_str(),
                r.items[0].artist.as_str(),
                r.items[0].album.as_str(),
                r.items[0].duration_ms
            ),
            ("", "", "", 0)
        );
    }

    #[test]
    fn a_type_less_track_object_is_accepted_but_a_wrong_type_is_not() {
        let mut t = track(A, "x");
        t.as_object_mut().unwrap().remove("type");
        let r = parse_track_page(&page(vec![json!({"item": t})], Value::Null), 0).unwrap();
        assert_eq!(r.items.len(), 1);
        let mut t = track(A, "x");
        t["type"] = json!(5);
        let r = parse_track_page(&page(vec![json!({"item": t})], Value::Null), 0).unwrap();
        assert_eq!((r.items.len(), r.skipped), (0, 1));
    }

    #[test]
    fn a_page_without_an_items_array_is_not_an_empty_list() {
        for bad in [
            json!({}),
            json!({"items": null}),
            json!({"items": {}}),
            json!([]),
            json!("x"),
        ] {
            assert_eq!(parse_track_page(&bad, 0), Err(UnexpectedShape), "{bad}");
        }
        let empty = parse_track_page(&json!({"items": []}), 0).unwrap();
        assert!(empty.items.is_empty());
    }

    #[test]
    fn paging_uses_offsets_and_ignores_the_next_url() {
        let entries: Vec<Value> = (0..50).map(|_| json!({"item": track(A, "t")})).collect();
        let hostile = json!("https://evil.example/steal?x=1");
        let r = parse_track_page(&page(entries.clone(), hostile), 100).unwrap();
        assert_eq!(r.next_offset, Some(150));
        assert!(!r.truncated);
        // `next` null or missing: no more
        let r = parse_track_page(&page(entries.clone(), Value::Null), 100).unwrap();
        assert_eq!(r.next_offset, None);
        let r = parse_track_page(&json!({"items": entries}), 0).unwrap();
        assert_eq!(r.next_offset, None);
        // next present but not a string: not "more"
        let r = parse_track_page(&page(entries, json!(7)), 0).unwrap();
        assert_eq!(r.next_offset, None);
    }

    #[test]
    fn skipped_entries_still_advance_the_offset() {
        let p = page(
            vec![json!({"item": null}), json!({"item": track(A, "t")})],
            json!("https://api.spotify.com/x"),
        );
        let r = parse_track_page(&p, 50).unwrap();
        assert_eq!((r.items.len(), r.skipped, r.next_offset), (1, 1, Some(52)));
    }

    #[test]
    fn an_empty_page_that_claims_more_does_not_loop() {
        let r = parse_track_page(&page(vec![], json!("https://api.spotify.com/x")), 50).unwrap();
        assert_eq!(r.next_offset, None);
    }

    #[test]
    fn the_row_cap_stops_paging_and_says_so() {
        let entries: Vec<Value> = (0..50).map(|_| json!({"item": track(A, "t")})).collect();
        let more = json!("https://api.spotify.com/x");
        let r = parse_track_page(&page(entries.clone(), more.clone()), 1950).unwrap();
        assert_eq!((r.next_offset, r.truncated), (None, true));
        let r = parse_track_page(&page(entries, more), 1900).unwrap();
        assert_eq!((r.next_offset, r.truncated), (Some(1950), false));
    }

    // ---- playlists

    fn playlist(id: &str, extra: Value) -> Value {
        let mut v = json!({"id": id, "name": "My list", "tracks": {"total": 12}});
        for (k, val) in extra.as_object().unwrap() {
            v[k] = val.clone();
        }
        v
    }

    #[test]
    fn readability_prediction_table() {
        let me = "me123";
        let cases = [
            (
                json!({"owner": {"id": "me123"}, "collaborative": false}),
                Readable::Readable,
            ),
            (json!({"owner": {"id": "me123"}}), Readable::Readable),
            (
                json!({"owner": {"id": "other"}, "collaborative": true}),
                Readable::Readable,
            ),
            (json!({"collaborative": true}), Readable::Readable),
            (
                json!({"owner": {"id": "other"}, "collaborative": false}),
                Readable::Unreadable,
            ),
            (json!({"owner": {"id": "other"}}), Readable::Unknown),
            (json!({"collaborative": false}), Readable::Unknown),
            (json!({}), Readable::Unknown),
            (
                json!({"owner": {}, "collaborative": false}),
                Readable::Unknown,
            ),
            (
                json!({"owner": {"id": 5}, "collaborative": "yes"}),
                Readable::Unknown,
            ),
        ];
        for (entry, want) in cases {
            assert_eq!(predict_readable(&entry, me), want, "{entry}");
        }
    }

    #[test]
    fn playlist_page_validates_ids_and_caps_names() {
        let long = "n".repeat(300);
        let p = json!({
            "items": [
                playlist(A, json!({"name": long, "owner": {"id": "me"}})),
                playlist("short", json!({})),
                playlist("../etc/passwd/../../xxxx", json!({})),
                playlist(&format!("{A}x"), json!({})),
                json!({"name": "no id"}),
                json!(null),
                json!({"id": B, "items": {"total": 4}}),
            ],
            "total": 70,
            "next": "https://api.spotify.com/v1/me/playlists?offset=50",
        });
        let r = parse_playlist_page(&p, 0, "me").unwrap();
        assert_eq!(r.items.len(), 2);
        assert_eq!(r.skipped, 5);
        assert_eq!(r.items[0].id, A);
        assert_eq!(r.items[0].name.chars().count(), MAX_TEXT_CHARS);
        assert_eq!(r.items[0].readable, Readable::Readable);
        assert_eq!(r.items[0].track_count, Some(12));
        assert_eq!(r.items[1].track_count, Some(4));
        assert_eq!(r.items[1].readable, Readable::Unknown);
        assert_eq!(r.next_offset, Some(7));
        assert_eq!(r.total, Some(70));
        assert_eq!(
            parse_playlist_page(&json!({}), 0, "me"),
            Err(UnexpectedShape)
        );
    }

    #[test]
    fn me_needs_a_non_empty_id() {
        assert_eq!(parse_me(&json!({"id": "abc"})).as_deref(), Some("abc"));
        assert_eq!(parse_me(&json!({"id": ""})), None);
        assert_eq!(parse_me(&json!({})), None);
        assert_eq!(parse_me(&json!({"id": 3})), None);
    }

    fn simple_track(id: &str, name: &str) -> Value {
        json!({"type": "track", "uri": format!("spotify:track:{id}"), "name": name,
               "duration_ms": 1000, "artists": [{"name": "Ar"}]})
    }

    #[test]
    fn album_tracks_entries_are_track_objects_with_no_album() {
        let p = page(
            vec![
                simple_track(A, "one"),
                json!({"item": track(B, "wrapped")}),
                json!(null),
                json!("x"),
            ],
            json!("https://api.spotify.com/x"),
        );
        let r = parse_album_tracks_page(&p, 50).unwrap();
        // The wrapped entry has no `uri` at the top level, so it is skipped like null and a string.
        assert_eq!((r.items.len(), r.skipped, r.next_offset), (1, 3, Some(54)));
        assert_eq!(r.items[0].album, "");
        assert_eq!(r.items[0].artist, "Ar");
        assert!(parse_album_tracks_page(&json!({"total": 1}), 0).is_err());
    }

    #[test]
    fn search_needs_a_tracks_page_and_stops_at_spotifys_limit() {
        assert!(parse_search_page(&json!({"albums": {"items": []}}), 0).is_err());
        assert!(parse_search_page(&json!({"tracks": {"total": 1}}), 0).is_err());
        let entries: Vec<Value> = (0..10).map(|_| track(A, "t")).collect();
        let resp = json!({"tracks": {"items": entries, "total": 4000, "next": "https://x"}});
        let r = parse_search_page(&resp, 990).unwrap();
        assert_eq!(
            (r.items.len(), r.next_offset, r.truncated),
            (10, None, true)
        );
        let r = parse_search_page(&resp, 0).unwrap();
        assert_eq!(
            (r.next_offset, r.truncated, r.total),
            (Some(10), false, Some(4000))
        );
    }

    #[test]
    fn album_page_validates_ids_shape_and_caps_text() {
        let long = "x".repeat(500);
        let entry = |id: &str| {
            json!({"added_at": "x", "album": {"id": id, "name": long, "total_tracks": 12,
                   "artists": [{"name": "One"}, {"name": "Two"}]}})
        };
        let p = page(
            vec![
                entry(A),
                entry("short"),
                json!({"added_at": "x"}),
                json!({"album": "nope"}),
                json!({"album": {"name": "no id"}}),
                json!(null),
            ],
            json!("https://api.spotify.com/x"),
        );
        let r = parse_album_page(&p, 0).unwrap();
        assert_eq!((r.items.len(), r.skipped, r.next_offset), (1, 5, Some(6)));
        assert_eq!(r.items[0].id, A);
        assert_eq!(r.items[0].name.chars().count(), MAX_TEXT_CHARS);
        assert_eq!(r.items[0].artist, "One, Two");
        assert_eq!(r.items[0].track_count, Some(12));
        assert!(parse_album_page(&json!({"albums": []}), 0).is_err());
    }

    fn url_of(refs: &[ImageRef]) -> Vec<&str> {
        refs.iter().map(|r| r.url.as_str()).collect()
    }

    /// I1
    #[test]
    fn track_reads_album_images_and_year() {
        let mut t = track(A, "T");
        t["album"] = json!({
            "name": "The Album",
            "release_date": "1981-12-15",
            "images": [
                {"url": "https://i.scdn.co/image/big", "width": 640, "height": 640},
                {"url": "https://i.scdn.co/image/mid", "width": 300, "height": 300},
                {"url": "https://i.scdn.co/image/small", "width": null, "height": 64},
            ],
        });
        let r = parse_track_page(&page(vec![json!({"item": t})], Value::Null), 0).unwrap();
        let t = &r.items[0];
        assert_eq!(
            url_of(&t.images),
            [
                "https://i.scdn.co/image/big",
                "https://i.scdn.co/image/mid",
                "https://i.scdn.co/image/small"
            ]
        );
        assert_eq!(
            (t.images[0].width, t.images[0].height),
            (Some(640), Some(640))
        );
        assert_eq!((t.images[2].width, t.images[2].height), (None, Some(64)));
        assert_eq!(t.year, Some(1981));
    }

    /// I2
    #[test]
    fn images_are_filtered_and_capped() {
        let long = format!("https://i.scdn.co/{}", "x".repeat(2049));
        assert!(long.len() > 2048);
        let exact = format!("https://i.scdn.co/{}", "y".repeat(2048 - 18));
        assert_eq!(exact.len(), 2048);
        let v = json!([
            {"url": "http://i.scdn.co/plain"},
            {"url": ""},
            {"url": long},
            {"url": 5},
            {"width": 640},
            "text",
            null,
            {"url": "https://i.scdn.co/ok", "width": 0, "height": 10_001},
            {"url": "https://i.scdn.co/ok2", "width": 640.5, "height": "64"},
            {"url": exact.clone()},
        ]);
        let r = parse_images(Some(&v));
        assert_eq!(
            url_of(&r),
            [
                "https://i.scdn.co/ok",
                "https://i.scdn.co/ok2",
                exact.as_str()
            ]
        );
        // Out-of-range or non-integer sides become unknown, not a rejection.
        assert_eq!((r[0].width, r[0].height), (None, None));
        assert_eq!((r[1].width, r[1].height), (None, None));
        let six: Vec<Value> = (0..6)
            .map(|i| json!({"url": format!("https://i.scdn.co/{i}")}))
            .collect();
        let r = parse_images(Some(&Value::Array(six)));
        assert_eq!(r.len(), MAX_IMAGES);
        assert_eq!(r[3].url, "https://i.scdn.co/3");
        assert!(parse_images(None).is_empty());
        assert!(parse_images(Some(&json!("x"))).is_empty());
        assert!(parse_images(Some(&json!({"url": "https://i.scdn.co/a"}))).is_empty());
        // A track whose album has no `images` has none.
        let r =
            parse_track_page(&page(vec![json!({"item": track(A, "T")})], Value::Null), 0).unwrap();
        assert!(r.items[0].images.is_empty());
    }

    /// I3
    #[test]
    fn release_date_gives_a_year() {
        let y = |v: Value| parse_year(Some(&v));
        assert_eq!(y(json!("1999")), Some(1999));
        assert_eq!(y(json!("2024-05")), Some(2024));
        assert_eq!(y(json!("0001-01-01")), Some(1));
        assert_eq!(y(json!("0000")), None);
        assert_eq!(y(json!("x")), None);
        assert_eq!(y(json!("19x9")), None);
        assert_eq!(y(json!("199")), None);
        assert_eq!(y(json!("١٩٩٩")), None);
        assert_eq!(y(json!(1999)), None);
        assert_eq!(parse_year(None), None);
    }

    /// I4
    #[test]
    fn album_and_playlist_pages_read_images() {
        let img = json!([{"url": "https://i.scdn.co/a", "width": 300, "height": 300}]);
        let p = page(
            vec![
                json!({"added_at": "x", "album": {"id": A, "name": "Al", "release_date": "1977-01-01", "images": img}}),
            ],
            Value::Null,
        );
        let r = parse_album_page(&p, 0).unwrap();
        assert_eq!(url_of(&r.items[0].images), ["https://i.scdn.co/a"]);
        assert_eq!(r.items[0].year, Some(1977));
        let p = page(
            vec![
                json!({"id": A, "name": "Mine", "images": img, "owner": {"id": "me"}, "collaborative": false}),
                json!({"id": B, "name": "No images", "images": null}),
            ],
            Value::Null,
        );
        let r = parse_playlist_page(&p, 0, "me").unwrap();
        assert_eq!(url_of(&r.items[0].images), ["https://i.scdn.co/a"]);
        assert!(r.items[1].images.is_empty());
    }

    /// I5
    #[test]
    fn names_come_back_without_bidi_and_zero_width_characters() {
        let mut t = track(A, "Ti\u{202E}tle\u{200B}");
        t["artists"] = json!([{"name": "Ar\u{200B}t\u{200D}ist"}]);
        t["album"] = json!({"name": "\u{202E}Alb\u{85}um"});
        let r = parse_track_page(&page(vec![json!({"item": t})], Value::Null), 0).unwrap();
        assert_eq!(r.items[0].name, "Title");
        // U+200D (a joiner) stays.
        assert_eq!(r.items[0].artist, "Art\u{200D}ist");
        assert_eq!(r.items[0].album, "Alb um");
    }
}
