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
    fn spotify_id_matches() {
        assert!(is_spotify_id(ID));
        assert!(!is_spotify_id("short"));
        assert!(!is_spotify_id(&format!("{ID}\n")));
    }
}
