//! The lyrics seam (C7): what a lyrics adapter returns and the trait it implements. The player
//! asks only for the current item and sanitises every answer ([`Lyrics::sanitized`]).

use std::fmt;

use crate::provider::BoxFuture;
use crate::provider::model::Track;
use crate::text::display_text;

/// The most lines kept of any lyrics.
pub const MAX_LYRIC_LINES: usize = 1000;
/// The most characters kept of any line.
pub const MAX_LYRIC_CHARS: usize = 300;

/// What a lyrics source is asked: the track's identity as the provider names it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LyricsQuery {
    pub artist: String,
    pub title: String,
    pub album: String,
    pub duration_ms: u64,
}

impl LyricsQuery {
    pub fn of(track: &Track) -> LyricsQuery {
        LyricsQuery {
            artist: track.artist.clone(),
            title: track.name.clone(),
            album: track.album.clone(),
            duration_ms: track.duration_ms,
        }
    }
}

/// One timed lyric line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LyricLine {
    pub at_ms: u64,
    pub text: String,
}

/// What was found for a track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lyrics {
    /// Timed lines, in time order after [`Lyrics::sanitized`].
    Synced(Vec<LyricLine>),
    Plain(Vec<String>),
    Instrumental,
    None,
}

impl Lyrics {
    /// Safe to draw: at most [`MAX_LYRIC_LINES`] lines of at most [`MAX_LYRIC_CHARS`] characters
    /// each, text through [`display_text`], synced lines stably sorted by time, an empty synced or
    /// plain list turned into [`Lyrics::None`].
    pub fn sanitized(self) -> Lyrics {
        match self {
            Lyrics::Synced(mut lines) => {
                lines.sort_by_key(|l| l.at_ms);
                lines.truncate(MAX_LYRIC_LINES);
                if lines.is_empty() {
                    return Lyrics::None;
                }
                Lyrics::Synced(
                    lines
                        .into_iter()
                        .map(|l| LyricLine {
                            at_ms: l.at_ms,
                            text: display_text(&l.text, MAX_LYRIC_CHARS),
                        })
                        .collect(),
                )
            }
            Lyrics::Plain(mut lines) => {
                lines.truncate(MAX_LYRIC_LINES);
                if lines.is_empty() {
                    return Lyrics::None;
                }
                Lyrics::Plain(
                    lines
                        .iter()
                        .map(|l| display_text(l, MAX_LYRIC_CHARS))
                        .collect(),
                )
            }
            other => other,
        }
    }
}

/// Why a lyrics lookup failed. The texts are fixed and name no service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LyricsError {
    /// The service did not answer (network, status, timeout, refused address).
    Unavailable,
    /// The service asked for fewer requests; none was made.
    RateLimited,
    /// The answer was not usable.
    BadResponse,
}

impl LyricsError {
    pub const fn text(self) -> &'static str {
        match self {
            LyricsError::Unavailable => "lyrics are unavailable",
            LyricsError::RateLimited => "the lyrics service asked for fewer requests",
            LyricsError::BadResponse => "the lyrics answer could not be used",
        }
    }
}

impl fmt::Display for LyricsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.text())
    }
}

impl std::error::Error for LyricsError {}

/// Looks lyrics up. Never panics; every failure is a [`LyricsError`]; "nothing found" is
/// `Ok(Lyrics::None)`.
pub trait LyricsSource: Send + Sync {
    fn lookup<'a>(&'a self, query: &'a LyricsQuery) -> BoxFuture<'a, Result<Lyrics, LyricsError>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(at_ms: u64, text: &str) -> LyricLine {
        LyricLine {
            at_ms,
            text: text.into(),
        }
    }

    #[test]
    fn synced_is_sorted_stably() {
        let l = Lyrics::Synced(vec![
            line(30, "c"),
            line(10, "a1"),
            line(20, "b"),
            line(10, "a2"),
        ])
        .sanitized();
        assert_eq!(
            l,
            Lyrics::Synced(vec![
                line(10, "a1"),
                line(10, "a2"),
                line(20, "b"),
                line(30, "c")
            ])
        );
    }

    #[test]
    fn long_lists_keep_the_first_thousand_by_time() {
        let lines: Vec<_> = (0..1500u64).rev().map(|i| line(i, "x")).collect();
        let Lyrics::Synced(out) = Lyrics::Synced(lines).sanitized() else {
            panic!("synced expected")
        };
        assert_eq!(out.len(), MAX_LYRIC_LINES);
        assert_eq!(out.first().unwrap().at_ms, 0);
        assert_eq!(out.last().unwrap().at_ms, 999);
        let Lyrics::Plain(p) = Lyrics::Plain(vec!["y".into(); 1500]).sanitized() else {
            panic!("plain expected")
        };
        assert_eq!(p.len(), MAX_LYRIC_LINES);
    }

    #[test]
    fn lines_are_cut_cleaned_and_boundary_safe() {
        let Lyrics::Plain(p) = Lyrics::Plain(vec![
            "x".repeat(1000),
            "a\tb\r\nc".into(),
            "é".repeat(400),
            "a\u{202E}b\u{200B}\u{200D}c\u{85}d".into(),
        ])
        .sanitized() else {
            panic!("plain expected")
        };
        assert_eq!(p[0].chars().count(), 300);
        assert_eq!(p[1], "a b  c");
        assert_eq!(p[2].chars().count(), 300);
        assert_eq!(p[3], "ab\u{200D}c d");
    }

    #[test]
    fn empty_lists_become_none_and_the_rest_pass() {
        assert_eq!(Lyrics::Synced(vec![]).sanitized(), Lyrics::None);
        assert_eq!(Lyrics::Plain(vec![]).sanitized(), Lyrics::None);
        assert_eq!(Lyrics::Instrumental.sanitized(), Lyrics::Instrumental);
        assert_eq!(Lyrics::None.sanitized(), Lyrics::None);
    }

    #[test]
    fn query_of_copies_the_identity() {
        let t = Track {
            uri: "u".into(),
            name: "T".into(),
            artist: "A".into(),
            album: "B".into(),
            duration_ms: 1234,
            images: vec![],
            year: None,
            album_ref: None,
            artists: vec![],
        };
        assert_eq!(
            LyricsQuery::of(&t),
            LyricsQuery {
                artist: "A".into(),
                title: "T".into(),
                album: "B".into(),
                duration_ms: 1234
            }
        );
    }
}
