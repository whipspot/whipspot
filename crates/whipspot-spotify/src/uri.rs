//! Spotify track URI/id validation. Anchored, exact, no trimming: a mismatch is rejected, never repaired.

const TRACK_PREFIX: &str = "spotify:track:";
const ID_LEN: usize = 22;

fn is_base62_id(s: &str) -> bool {
    s.len() == ID_LEN && s.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// Accepts exactly `spotify:track:` + 22 base62 chars, or a bare 22-char base62 id.
/// Returns the canonical `spotify:track:<id>` form.
pub fn parse_track_uri(input: &str) -> Option<String> {
    let id = input.strip_prefix(TRACK_PREFIX).unwrap_or(input);
    is_base62_id(id).then(|| format!("{TRACK_PREFIX}{id}"))
}

/// The id of an exact `spotify:artist:<22 base62>` URI; anything else (a bare id, another type,
/// extra segments, whitespace) is `None`.
pub fn artist_uri_id(input: &str) -> Option<&str> {
    typed_uri_id(input, "spotify:artist:")
}

/// The id of an exact `spotify:album:<22 base62>` URI; see [`artist_uri_id`].
pub fn album_uri_id(input: &str) -> Option<&str> {
    typed_uri_id(input, "spotify:album:")
}

fn typed_uri_id<'a>(input: &'a str, prefix: &str) -> Option<&'a str> {
    input.strip_prefix(prefix).filter(|id| is_base62_id(id))
}

/// True for an exact `spotify:track:` or `spotify:album:` URI, the two kinds the library can save.
pub fn is_saveable_uri(input: &str) -> bool {
    album_uri_id(input).is_some() || parse_track_uri(input).is_some_and(|c| c == input)
}

/// True for a bare 22-char base62 id (playlist ids, track ids in the Web API).
pub fn is_spotify_id(input: &str) -> bool {
    is_base62_id(input)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "4uLU6hMCjMI75M1A2tKUQC";

    #[test]
    fn accepts_uri_and_bare_id() {
        assert_eq!(
            parse_track_uri(&format!("spotify:track:{ID}")).as_deref(),
            Some("spotify:track:4uLU6hMCjMI75M1A2tKUQC")
        );
        assert_eq!(
            parse_track_uri(ID).as_deref(),
            Some("spotify:track:4uLU6hMCjMI75M1A2tKUQC")
        );
    }

    #[test]
    fn rejects_bad_lengths() {
        assert_eq!(parse_track_uri(&ID[..21]), None);
        assert_eq!(parse_track_uri(&format!("{ID}A")), None);
        assert_eq!(
            parse_track_uri(&format!("spotify:track:{}", &ID[..21])),
            None
        );
        assert_eq!(parse_track_uri(&format!("spotify:track:{ID}A")), None);
    }

    #[test]
    fn rejects_trailing_newline_and_whitespace() {
        assert_eq!(parse_track_uri(&format!("{ID}\n")), None);
        assert_eq!(parse_track_uri(&format!("spotify:track:{ID}\n")), None);
        assert_eq!(parse_track_uri(&format!(" {ID}")), None);
    }

    #[test]
    fn rejects_extra_segments_and_other_types() {
        assert_eq!(parse_track_uri(&format!("spotify:track:{ID}:{ID}")), None);
        assert_eq!(parse_track_uri(&format!("spotify:track:..:{ID}")), None);
        assert_eq!(parse_track_uri(&format!("spotify:episode:{ID}")), None);
        assert_eq!(parse_track_uri(&format!("spotify:user:x:track:{ID}")), None);
    }

    #[test]
    fn rejects_empty_and_unicode() {
        assert_eq!(parse_track_uri(""), None);
        assert_eq!(parse_track_uri("spotify:track:"), None);
        // 22 chars but not ASCII alphanumerics (fullwidth digits are 3 bytes each: 22 chars != 22 bytes)
        assert_eq!(parse_track_uri(&"１".repeat(22)), None);
        // 22 bytes made of non-ASCII chars
        assert_eq!(parse_track_uri(&"é".repeat(11)), None);
        assert_eq!(parse_track_uri("4uLU6hMCjMI75M1A2tKUQ\u{0301}"), None);
    }

    #[test]
    fn rejects_non_alnum_ascii() {
        assert_eq!(parse_track_uri("4uLU6hMCjMI75M1A2tKUQ-"), None);
        assert_eq!(parse_track_uri("4uLU6hMCjMI75M1A2tKU/C"), None);
    }

    #[test]
    fn typed_uris_are_exact() {
        assert_eq!(artist_uri_id(&format!("spotify:artist:{ID}")), Some(ID));
        assert_eq!(album_uri_id(&format!("spotify:album:{ID}")), Some(ID));
        for bad in [
            ID.to_owned(),
            format!("spotify:album:{ID}"),
            format!("spotify:artist:{ID}\n"),
            format!("spotify:artist:{ID}:{ID}"),
            format!("spotify:artist:{}", &ID[..21]),
            format!("spotify:artist:../{}", &ID[3..]),
            "spotify:artist:".to_owned(),
            String::new(),
        ] {
            assert_eq!(artist_uri_id(&bad), None, "{bad:?}");
        }
        assert_eq!(album_uri_id(&format!("spotify:artist:{ID}")), None);
        assert!(is_saveable_uri(&format!("spotify:album:{ID}")));
        assert!(is_saveable_uri(&format!("spotify:track:{ID}")));
        assert!(!is_saveable_uri(&format!("spotify:artist:{ID}")));
        assert!(!is_saveable_uri(ID));
    }

    #[test]
    fn spotify_id_matches() {
        assert!(is_spotify_id(ID));
        assert!(!is_spotify_id("short"));
        assert!(!is_spotify_id(&format!("{ID}\n")));
    }
}
