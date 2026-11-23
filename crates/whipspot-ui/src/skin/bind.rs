//! The skin model's neutral bindings mapped onto the player (C1). The rustdoc of the core's skin
//! model is the spec; drawing and input call these, never a second copy of a mapping.

use whipspot_core::diagnostics::SourceDiagnostics;
use whipspot_core::engine::BAND_COUNT;
use whipspot_core::player::{PlayState, PlayerAction, PlayerState, RepeatMode};
use whipspot_core::skin::{Axis, Flag, FrameRule, PlayerCommand, Skin, TextRead, TimeDigit, Value};
use whipspot_core::text::display_text;

/// The longest track title a text widget gets, in characters.
const TITLE_CHARS: usize = 200;

/// The front-end's own state a [`Flag`] may read: which panels are shown and whether the
/// widget's own panel has the focus.
pub(crate) struct FrontState<'a> {
    pub skin: &'a Skin,
    /// Per panel index of `skin.panels`; a missing entry reads as hidden.
    pub visible: &'a [bool],
    /// The widget's own panel has the keyboard focus.
    pub focused: bool,
}

/// The current track's duration in ms, `None` when nothing is current or it is 0.
fn duration_ms(state: &PlayerState) -> Option<u64> {
    state
        .current
        .as_ref()
        .map(|c| c.track.duration_ms)
        .filter(|&d| d > 0)
}

fn clamp01(v: f32) -> f32 {
    if v.is_finite() {
        v.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// dB (-12..=12) as a slider value.
fn db_value(db: f32) -> f32 {
    clamp01((db + 12.0) / 24.0)
}

/// A slider value as dB, snapped to 0.5 dB (D7, as the native faders).
fn value_db(v: f32) -> f32 {
    let db = v * 24.0 - 12.0;
    ((db * 2.0).round() / 2.0).clamp(-12.0, 12.0)
}

/// The slider value `value` shows now, in `0.0..=1.0`.
pub(crate) fn value_of(value: Value, state: &PlayerState) -> f32 {
    match value {
        Value::Position => match duration_ms(state) {
            Some(d) => clamp01((f64::from(state.position_now()) / d as f64) as f32),
            None => 0.0,
        },
        Value::Volume => clamp01(f32::from(state.volume) / 100.0),
        Value::Balance => clamp01((f32::from(state.balance) + 100.0) / 200.0),
        Value::EqPreamp => db_value(state.eq.preamp_db),
        Value::EqBand(b) => db_value(
            state
                .eq
                .bands_db
                .get(usize::from(b))
                .copied()
                .unwrap_or(0.0),
        ),
    }
}

/// The action that sets `value` to `v` (clamped to `0.0..=1.0`); `None` for a non-finite `v`,
/// a position with nothing current or a zero duration, or a band at or above `BAND_COUNT`.
pub(crate) fn action_for(value: Value, v: f32, state: &PlayerState) -> Option<PlayerAction> {
    if !v.is_finite() {
        return None;
    }
    let v = v.clamp(0.0, 1.0);
    match value {
        Value::Position => {
            let d = duration_ms(state)?;
            let ms = (f64::from(v) * d as f64).round();
            Some(PlayerAction::Seek {
                position_ms: ms.min(f64::from(u32::MAX)) as u32,
            })
        }
        Value::Volume => Some(PlayerAction::SetVolume((v * 100.0).round() as u8)),
        Value::Balance => Some(PlayerAction::SetBalance(
            ((v * 200.0).round() as i32 - 100) as i8,
        )),
        Value::EqPreamp => Some(PlayerAction::SetEqPreamp(value_db(v))),
        Value::EqBand(b) => (usize::from(b) < BAND_COUNT).then(|| PlayerAction::SetEqBand {
            index: usize::from(b),
            gain_db: value_db(v),
        }),
    }
}

/// The player action a command dispatches on release.
pub(crate) fn command_action(command: PlayerCommand, state: &PlayerState) -> PlayerAction {
    match command {
        PlayerCommand::Play => PlayerAction::Play,
        PlayerCommand::Pause => PlayerAction::Pause,
        PlayerCommand::TogglePlay => PlayerAction::Toggle,
        PlayerCommand::Stop => PlayerAction::Stop,
        PlayerCommand::Next => PlayerAction::Next,
        PlayerCommand::Previous => PlayerAction::Previous,
        PlayerCommand::ToggleShuffle => PlayerAction::SetShuffle(!state.shuffle),
        PlayerCommand::CycleRepeat => PlayerAction::SetRepeat(match state.repeat {
            RepeatMode::Off => RepeatMode::All,
            RepeatMode::All => RepeatMode::One,
            RepeatMode::One => RepeatMode::Off,
        }),
        PlayerCommand::ToggleEq => PlayerAction::SetEqEnabled(!state.eq.enabled),
    }
}

/// The value of a boolean binding.
pub(crate) fn flag(
    flag: &Flag,
    state: &PlayerState,
    source: &SourceDiagnostics,
    front: &FrontState<'_>,
) -> bool {
    match flag {
        Flag::Shuffle => state.shuffle,
        Flag::Repeat => state.repeat != RepeatMode::Off,
        Flag::EqEnabled => state.eq.enabled,
        Flag::PanelVisible(id) => front
            .skin
            .panels
            .iter()
            .position(|p| &p.id == id)
            .and_then(|i| front.visible.get(i).copied())
            .unwrap_or(false),
        Flag::PanelFocused => front.focused,
        Flag::Stereo => source.channels.is_some_and(|c| c >= 2),
        Flag::Mono => source.channels == Some(1),
    }
}

/// The sprite index of a play-state indicator: stopped 0, playing (and loading) 1, paused 2.
pub(crate) fn play_frame(play_state: PlayState) -> usize {
    match play_state {
        PlayState::Stopped => 0,
        PlayState::Playing | PlayState::Loading => 1,
        PlayState::Paused => 2,
    }
}

/// What a text widget shows now.
pub(crate) fn text(read: TextRead, state: &PlayerState, source: &SourceDiagnostics) -> String {
    match read {
        TextRead::TrackTitle => match &state.current {
            None => String::new(),
            Some(c) if c.track.artist.trim().is_empty() => display_text(&c.track.name, TITLE_CHARS),
            Some(c) => display_text(
                &format!("{} - {}", c.track.artist, c.track.name),
                TITLE_CHARS,
            ),
        },
        TextRead::BitrateKbps => source
            .bitrate_nominal_kbps
            .map(|k| k.to_string())
            .unwrap_or_default(),
        TextRead::SampleRateKhz => source
            .sample_rate_hz
            .map(|hz| ((u64::from(hz) + 500) / 1000).to_string())
            .unwrap_or_default(),
        TextRead::Time(digit) => {
            if state.play_state == PlayState::Stopped {
                return String::new();
            }
            let secs = state.position_now() / 1000;
            let (m, s) = ((secs / 60) % 100, secs % 60);
            let d = match digit {
                TimeDigit::MinutesTens => m / 10,
                TimeDigit::MinutesUnits => m % 10,
                TimeDigit::SecondsTens => s / 10,
                TimeDigit::SecondsUnits => s % 10,
            };
            d.to_string()
        }
    }
}

/// Which of `n` slider frames shows `v`: `Linear` `round(v * (n - 1))`, `FromCentre`
/// `round(|v - 0.5| * 2 * (n - 1))`, clamped to `n - 1` (0 when `n` is 0).
pub(crate) fn frame_index(rule: FrameRule, v: f32, n: usize) -> usize {
    let last = n.saturating_sub(1);
    let v = clamp01(v);
    let t = match rule {
        FrameRule::Linear => v,
        FrameRule::FromCentre => ((v - 0.5).abs() * 2.0).min(1.0),
    };
    ((t * last as f32).round() as usize).min(last)
}

/// The thumb's offset from the slider's v = 0 end: `round(v * (extent - thumb))`, 0 when the
/// thumb does not fit.
pub(crate) fn thumb_offset(v: f32, extent: u32, thumb: u32) -> u32 {
    let travel = extent.saturating_sub(thumb);
    ((clamp01(v) * travel as f32).round() as u32).min(travel)
}

/// The thumb's offset from the rect's left (`Horizontal`) or top (`Vertical`, where v = 1.0 is
/// the top).
pub(crate) fn thumb_start(axis: Axis, v: f32, extent: u32, thumb: u32) -> u32 {
    let off = thumb_offset(v, extent, thumb);
    match axis {
        Axis::Horizontal => off,
        Axis::Vertical => extent.saturating_sub(thumb) - off,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use whipspot_core::player::{EqState, LyricsState, NowPlaying, OutputStatus, QueueState};
    use whipspot_core::provider::Track;
    use whipspot_core::skin::{PanelId, SkinInfo, SkinTheme};

    use super::*;
    use crate::skin::fake::panel;

    fn state() -> PlayerState {
        PlayerState {
            revision: 0,
            play_state: PlayState::Stopped,
            current: None,
            position_ms: 0,
            position_at: Instant::now(),
            volume: 50,
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
        }
    }

    fn track(artist: &str, name: &str, duration_ms: u64) -> NowPlaying {
        NowPlaying {
            track: Track {
                uri: "test:1".to_owned(),
                name: name.to_owned(),
                artist: artist.to_owned(),
                album: String::new(),
                duration_ms,
                images: Vec::new(),
                year: None,
                album_ref: None,
                artists: vec![],
            },
            provider: "test",
            source_format: None,
        }
    }

    fn paused_at(ms: u32, duration: u64) -> PlayerState {
        let mut s = state();
        s.play_state = PlayState::Paused;
        s.position_ms = ms;
        s.current = Some(track("A", "T", duration));
        s
    }

    fn seek(a: Option<PlayerAction>) -> Option<u32> {
        match a {
            Some(PlayerAction::Seek { position_ms }) => Some(position_ms),
            _ => None,
        }
    }

    #[test]
    fn position_reads_and_seeks() {
        let s = paused_at(30_000, 120_000);
        assert_eq!(value_of(Value::Position, &s), 0.25);
        assert_eq!(seek(action_for(Value::Position, 0.5, &s)), Some(60_000));
        assert_eq!(seek(action_for(Value::Position, 0.33333, &s)), Some(40_000));
        assert_eq!(seek(action_for(Value::Position, 2.0, &s)), Some(120_000));
        assert!(action_for(Value::Position, f32::NAN, &s).is_none());
        // Nothing current, or a zero duration: v = 0 and no action.
        let none = state();
        assert_eq!(value_of(Value::Position, &none), 0.0);
        assert!(action_for(Value::Position, 0.5, &none).is_none());
        let zero = paused_at(1000, 0);
        assert_eq!(value_of(Value::Position, &zero), 0.0);
        assert!(action_for(Value::Position, 0.5, &zero).is_none());
    }

    #[test]
    fn volume_reads_and_sets() {
        let mut s = state();
        s.volume = 30;
        assert!((value_of(Value::Volume, &s) - 0.3).abs() < 1e-6);
        assert!(matches!(
            action_for(Value::Volume, 0.574, &s),
            Some(PlayerAction::SetVolume(57))
        ));
        assert!(matches!(
            action_for(Value::Volume, -1.0, &s),
            Some(PlayerAction::SetVolume(0))
        ));
        assert!(action_for(Value::Volume, f32::INFINITY, &s).is_none());
    }

    #[test]
    fn balance_reads_and_sets() {
        let mut s = state();
        s.balance = -100;
        assert_eq!(value_of(Value::Balance, &s), 0.0);
        s.balance = 0;
        assert_eq!(value_of(Value::Balance, &s), 0.5);
        s.balance = 100;
        assert_eq!(value_of(Value::Balance, &s), 1.0);
        assert!(matches!(
            action_for(Value::Balance, 0.0, &s),
            Some(PlayerAction::SetBalance(-100))
        ));
        assert!(matches!(
            action_for(Value::Balance, 0.75, &s),
            Some(PlayerAction::SetBalance(50))
        ));
        assert!(matches!(
            action_for(Value::Balance, 1.0, &s),
            Some(PlayerAction::SetBalance(100))
        ));
    }

    #[test]
    fn eq_preamp_and_bands_read_and_snap() {
        let mut s = state();
        s.eq.preamp_db = 6.0;
        s.eq.bands_db[3] = -12.0;
        assert_eq!(value_of(Value::EqPreamp, &s), 0.75);
        assert_eq!(value_of(Value::EqBand(3), &s), 0.0);
        assert_eq!(value_of(Value::EqBand(0), &s), 0.5);
        // 0.6 -> 2.4 dB -> 2.5 dB.
        match action_for(Value::EqPreamp, 0.6, &s) {
            Some(PlayerAction::SetEqPreamp(db)) => assert_eq!(db, 2.5),
            _ => panic!("preamp action"),
        }
        match action_for(Value::EqBand(9), 0.0, &s) {
            Some(PlayerAction::SetEqBand { index, gain_db }) => {
                assert_eq!(index, 9);
                assert_eq!(gain_db, -12.0);
            }
            _ => panic!("band action"),
        }
        assert!(action_for(Value::EqBand(BAND_COUNT as u8), 0.5, &s).is_none());
        assert!(action_for(Value::EqBand(255), 0.5, &s).is_none());
    }

    #[test]
    fn commands_map_to_actions() {
        let mut s = state();
        assert!(matches!(
            command_action(PlayerCommand::Play, &s),
            PlayerAction::Play
        ));
        assert!(matches!(
            command_action(PlayerCommand::Pause, &s),
            PlayerAction::Pause
        ));
        assert!(matches!(
            command_action(PlayerCommand::TogglePlay, &s),
            PlayerAction::Toggle
        ));
        assert!(matches!(
            command_action(PlayerCommand::Stop, &s),
            PlayerAction::Stop
        ));
        assert!(matches!(
            command_action(PlayerCommand::Next, &s),
            PlayerAction::Next
        ));
        assert!(matches!(
            command_action(PlayerCommand::Previous, &s),
            PlayerAction::Previous
        ));
        assert!(matches!(
            command_action(PlayerCommand::ToggleShuffle, &s),
            PlayerAction::SetShuffle(true)
        ));
        s.shuffle = true;
        assert!(matches!(
            command_action(PlayerCommand::ToggleShuffle, &s),
            PlayerAction::SetShuffle(false)
        ));
        assert!(matches!(
            command_action(PlayerCommand::ToggleEq, &s),
            PlayerAction::SetEqEnabled(true)
        ));
        let mut cycle = Vec::new();
        for _ in 0..3 {
            match command_action(PlayerCommand::CycleRepeat, &s) {
                PlayerAction::SetRepeat(r) => {
                    cycle.push(r);
                    s.repeat = r;
                }
                _ => panic!("repeat action"),
            }
        }
        assert_eq!(cycle, [RepeatMode::All, RepeatMode::One, RepeatMode::Off]);
    }

    fn two_panel_skin() -> Skin {
        Skin {
            info: SkinInfo {
                engine: String::new(),
                name: String::new(),
            },
            panels: vec![panel("main", 10, 10, None), panel("side", 10, 10, None)],
            images: Vec::new(),
            fonts: Vec::new(),
            visualiser: None,
            theme: SkinTheme::default(),
        }
    }

    #[test]
    fn flags_read_player_source_and_front_state() {
        let skin = two_panel_skin();
        let front = FrontState {
            skin: &skin,
            visible: &[true, false],
            focused: true,
        };
        let mut s = state();
        let mut src = SourceDiagnostics::default();
        assert!(!flag(&Flag::Shuffle, &s, &src, &front));
        assert!(!flag(&Flag::Repeat, &s, &src, &front));
        assert!(!flag(&Flag::EqEnabled, &s, &src, &front));
        s.shuffle = true;
        s.repeat = RepeatMode::One;
        s.eq.enabled = true;
        assert!(flag(&Flag::Shuffle, &s, &src, &front));
        assert!(flag(&Flag::Repeat, &s, &src, &front));
        assert!(flag(&Flag::EqEnabled, &s, &src, &front));
        let main = PanelId::new("main").unwrap();
        let side = PanelId::new("side").unwrap();
        let other = PanelId::new("nope").unwrap();
        assert!(flag(&Flag::PanelVisible(main), &s, &src, &front));
        assert!(!flag(&Flag::PanelVisible(side), &s, &src, &front));
        assert!(!flag(&Flag::PanelVisible(other), &s, &src, &front));
        assert!(flag(&Flag::PanelFocused, &s, &src, &front));
        // Channels: unknown, mono, stereo, more.
        assert!(!flag(&Flag::Stereo, &s, &src, &front));
        assert!(!flag(&Flag::Mono, &s, &src, &front));
        src.channels = Some(1);
        assert!(!flag(&Flag::Stereo, &s, &src, &front));
        assert!(flag(&Flag::Mono, &s, &src, &front));
        src.channels = Some(2);
        assert!(flag(&Flag::Stereo, &s, &src, &front));
        assert!(!flag(&Flag::Mono, &s, &src, &front));
        src.channels = Some(6);
        assert!(flag(&Flag::Stereo, &s, &src, &front));
    }

    #[test]
    fn play_frames() {
        assert_eq!(play_frame(PlayState::Stopped), 0);
        assert_eq!(play_frame(PlayState::Playing), 1);
        assert_eq!(play_frame(PlayState::Loading), 1);
        assert_eq!(play_frame(PlayState::Paused), 2);
    }

    #[test]
    fn title_text() {
        let src = SourceDiagnostics::default();
        let mut s = state();
        assert_eq!(text(TextRead::TrackTitle, &s, &src), "");
        s.current = Some(track("Artist", "Song", 1000));
        assert_eq!(text(TextRead::TrackTitle, &s, &src), "Artist - Song");
        s.current = Some(track("", "Song\u{200B}\tTwo", 1000));
        assert_eq!(text(TextRead::TrackTitle, &s, &src), "Song Two");
    }

    #[test]
    fn bitrate_and_rate_texts() {
        let s = state();
        let mut src = SourceDiagnostics::default();
        assert_eq!(text(TextRead::BitrateKbps, &s, &src), "");
        assert_eq!(text(TextRead::SampleRateKhz, &s, &src), "");
        src.bitrate_nominal_kbps = Some(320);
        src.sample_rate_hz = Some(44_100);
        assert_eq!(text(TextRead::BitrateKbps, &s, &src), "320");
        assert_eq!(text(TextRead::SampleRateKhz, &s, &src), "44");
        src.sample_rate_hz = Some(22_500);
        assert_eq!(text(TextRead::SampleRateKhz, &s, &src), "23");
    }

    #[test]
    fn time_digits() {
        let src = SourceDiagnostics::default();
        let digits = |s: &PlayerState| -> String {
            [
                TimeDigit::MinutesTens,
                TimeDigit::MinutesUnits,
                TimeDigit::SecondsTens,
                TimeDigit::SecondsUnits,
            ]
            .into_iter()
            .map(|d| text(TextRead::Time(d), s, &src))
            .collect()
        };
        // 12:34.
        let s = paused_at((12 * 60 + 34) * 1000 + 999, 0);
        assert_eq!(digits(&s), "1234");
        // Minutes modulo 100: 101:05 shows 01:05.
        let s = paused_at((101 * 60 + 5) * 1000, 0);
        assert_eq!(digits(&s), "0105");
        // Stopped: blank.
        let mut s = paused_at(5000, 0);
        s.play_state = PlayState::Stopped;
        assert_eq!(digits(&s), "");
        // Playing advances with the clock.
        let mut s = paused_at(0, 0);
        s.play_state = PlayState::Playing;
        s.position_at = Instant::now() - Duration::from_secs(61);
        assert_eq!(&digits(&s)[..3], "010");
    }

    #[test]
    fn frame_indices() {
        assert_eq!(frame_index(FrameRule::Linear, 0.0, 28), 0);
        assert_eq!(frame_index(FrameRule::Linear, 1.0, 28), 27);
        assert_eq!(frame_index(FrameRule::Linear, 0.5, 28), 14);
        assert_eq!(frame_index(FrameRule::Linear, 2.0, 28), 27);
        assert_eq!(frame_index(FrameRule::Linear, f32::NAN, 28), 0);
        assert_eq!(frame_index(FrameRule::Linear, 0.7, 1), 0);
        assert_eq!(frame_index(FrameRule::FromCentre, 0.5, 28), 0);
        assert_eq!(frame_index(FrameRule::FromCentre, 0.0, 28), 27);
        assert_eq!(frame_index(FrameRule::FromCentre, 1.0, 28), 27);
        assert_eq!(frame_index(FrameRule::FromCentre, 0.75, 28), 14);
        assert_eq!(frame_index(FrameRule::Linear, 0.5, 0), 0);
    }

    #[test]
    fn thumb_offsets() {
        assert_eq!(thumb_offset(0.0, 100, 20), 0);
        assert_eq!(thumb_offset(1.0, 100, 20), 80);
        assert_eq!(thumb_offset(0.5, 100, 20), 40);
        assert_eq!(thumb_offset(0.5, 10, 20), 0);
        assert_eq!(thumb_offset(-3.0, 100, 20), 0);
        // Vertical: v = 1.0 is the top.
        assert_eq!(thumb_start(Axis::Vertical, 1.0, 100, 20), 0);
        assert_eq!(thumb_start(Axis::Vertical, 0.0, 100, 20), 80);
        assert_eq!(thumb_start(Axis::Vertical, 0.25, 100, 20), 60);
        assert_eq!(thumb_start(Axis::Horizontal, 0.25, 100, 20), 20);
    }
}
