//! The text the window title shows (MD52): what the view proposes, the host sets.

use whipspot_core::player::{PlayState, PlayerState};
use whipspot_core::text::display_text;

/// The product name, always the last thing in the title.
const PRODUCT: &str = "Whipspot";
/// Most characters of the artist and track part.
const MAX_CHARS: usize = 200;

/// The proposed window title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowTitle {
    pub text: String,
    /// The text may scroll (a taskbar marquee) while it plays.
    pub scroll: bool,
}

/// `"<artist> - <title> · Whipspot"` while something is current, else "Whipspot". Control
/// characters become spaces and the artist and title part is cut at 200 characters, so the product
/// name always shows. Bidi controls and zero-width characters are removed (`display_text`).
pub fn window_title(state: &PlayerState) -> WindowTitle {
    let Some(current) = &state.current else {
        return WindowTitle {
            text: PRODUCT.to_string(),
            scroll: false,
        };
    };
    let track = &current.track;
    let line = display_text(&format!("{} - {}", track.artist, track.name), MAX_CHARS);
    WindowTitle {
        text: format!("{line} \u{b7} {PRODUCT}"),
        scroll: state.play_state == PlayState::Playing,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Instant;

    use whipspot_core::engine::BAND_COUNT;
    use whipspot_core::player::{
        EqState, LyricsState, NowPlaying, OutputStatus, QueueState, RepeatMode,
    };
    use whipspot_core::provider::Track;

    use super::*;

    fn state(current: Option<(&str, &str)>, play_state: PlayState) -> PlayerState {
        PlayerState {
            revision: 0,
            play_state,
            current: current.map(|(artist, name)| NowPlaying {
                track: Track {
                    uri: "demo:1".into(),
                    name: name.into(),
                    artist: artist.into(),
                    album: "Album".into(),
                    duration_ms: 1000,
                    images: vec![],
                    year: None,
                    album_ref: None,
                    artists: vec![],
                },
                provider: "demo",
                source_format: None,
            }),
            position_ms: 0,
            position_at: Instant::now(),
            volume: 100,
            balance: 0,
            replay_gain: Default::default(),
            eq: EqState {
                enabled: false,
                preamp_db: 0.0,
                bands_db: [0.0; BAND_COUNT],
            },
            shuffle: false,
            repeat: RepeatMode::Off,
            queue: QueueState {
                items: Arc::new(Vec::new()),
                current: None,
                next: None,
            },
            providers: Vec::new(),
            output_summary: String::new(),
            output: OutputStatus::default(),
            lyrics: LyricsState::Off,
            lyrics_enabled: false,
            remote: None,
            power: Default::default(),
            update: Default::default(),
            app_version: String::new(),
            update_check: true,
        }
    }

    #[test]
    fn t7_nothing_current_is_the_product_name() {
        let t = window_title(&state(None, PlayState::Stopped));
        assert_eq!(
            t,
            WindowTitle {
                text: "Whipspot".into(),
                scroll: false
            }
        );
    }

    #[test]
    fn t7_playing_scrolls_and_paused_does_not() {
        let t = window_title(&state(Some(("B", "A")), PlayState::Playing));
        assert_eq!(t.text, "B - A \u{b7} Whipspot");
        assert!(t.scroll);
        let t = window_title(&state(Some(("B", "A")), PlayState::Paused));
        assert_eq!(t.text, "B - A \u{b7} Whipspot");
        assert!(!t.scroll);
    }

    #[test]
    fn t7_control_characters_become_spaces() {
        let t = window_title(&state(Some(("B", "A\nB\tC\r")), PlayState::Playing));
        // `display_text` trims, so the trailing carriage return leaves no extra space.
        assert_eq!(t.text, "B - A B C \u{b7} Whipspot");
    }

    #[test]
    fn t7_bidi_controls_and_zero_width_characters_are_removed() {
        let t = window_title(&state(
            Some(("B", "A\u{202E}B\u{200B}C")),
            PlayState::Playing,
        ));
        assert_eq!(t.text, "B - ABC \u{b7} Whipspot");
    }

    #[test]
    fn t7_a_long_title_is_capped_and_keeps_the_product_name() {
        let long = "x".repeat(500);
        let t = window_title(&state(Some(("B", &long)), PlayState::Playing));
        assert!(t.text.ends_with(" \u{b7} Whipspot"));
        assert_eq!(
            t.text.chars().count(),
            MAX_CHARS + " \u{b7} Whipspot".chars().count()
        );
        // Multi-byte characters are cut on a character boundary.
        let wide = "\u{e9}".repeat(500);
        let t = window_title(&state(Some(("B", &wide)), PlayState::Playing));
        assert!(t.text.ends_with(" \u{b7} Whipspot"));
        assert_eq!(
            t.text.chars().count(),
            MAX_CHARS + " \u{b7} Whipspot".chars().count()
        );
    }
}
