//! Keyboard shortcuts: the binding table the host hands to the toolkit, and what each command
//! asks of the player. The toolkit reports a fired command after a frame; a text field that has
//! focus takes its keys first, and the host tells when one has focus so nothing fires then.
//!
//! The media keys are not here: luneui has no key for them, so the host maps them to player
//! actions itself.

use lune_ui::input::{Key, NamedKey};
use lune_ui::shortcuts::{Binding, Chord, CommandId, Scope, Shortcuts};
use whipspot_core::player::{PlayerAction, PlayerState, RepeatMode};

/// One volume step of the Up and Down shortcuts, in volume units (0..=100).
const VOLUME_STEP: u8 = 5;

const PLAY_PAUSE: CommandId = CommandId::new("player.play-pause");
const PREVIOUS: CommandId = CommandId::new("player.previous");
const NEXT: CommandId = CommandId::new("player.next");
const VOLUME_UP: CommandId = CommandId::new("player.volume-up");
const VOLUME_DOWN: CommandId = CommandId::new("player.volume-down");
const SEARCH: CommandId = CommandId::new("view.search");
const DIAGNOSTICS: CommandId = CommandId::new("view.diagnostics");
const CYCLE_THEME: CommandId = CommandId::new("view.cycle-theme");
const BACK: CommandId = CommandId::new("nav.back");
const FORWARD: CommandId = CommandId::new("nav.forward");
const SETTINGS: CommandId = CommandId::new("view.settings");
const EQUALIZER: CommandId = CommandId::new("view.equalizer");
const TOGGLE_EQ: CommandId = CommandId::new("player.toggle-eq");
const QUEUE: CommandId = CommandId::new("view.queue");
const SHUFFLE: CommandId = CommandId::new("player.shuffle");
const REPEAT: CommandId = CommandId::new("player.repeat");

/// What a fired shortcut asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Command {
    TogglePlay,
    Previous,
    Next,
    VolumeUp,
    VolumeDown,
    /// Show the Search view with its field focused.
    FocusSearch,
    /// Open or close the diagnostics window.
    ToggleDiagnostics,
    /// Switch to the next theme (A, B, C, then A again; MD57).
    CycleTheme,
    /// The browse history one route back or forward (MD54).
    Back,
    Forward,
    /// Open the settings page (MD91).
    OpenSettings,
    /// Open the equalizer pane.
    OpenEqualizer,
    /// Switch the equalizer on or off.
    ToggleEq,
    /// Open the queue pane.
    OpenQueue,
    /// Switch shuffle on or off.
    ToggleShuffle,
    /// Repeat: Off, then All, then One, then Off again.
    CycleRepeat,
}

impl Command {
    /// The command a fired id names; `None` for an id this table never made.
    pub(crate) fn from_id(id: &CommandId) -> Option<Self> {
        [
            (PLAY_PAUSE, Command::TogglePlay),
            (PREVIOUS, Command::Previous),
            (NEXT, Command::Next),
            (VOLUME_UP, Command::VolumeUp),
            (VOLUME_DOWN, Command::VolumeDown),
            (SEARCH, Command::FocusSearch),
            (DIAGNOSTICS, Command::ToggleDiagnostics),
            (CYCLE_THEME, Command::CycleTheme),
            (BACK, Command::Back),
            (FORWARD, Command::Forward),
            (SETTINGS, Command::OpenSettings),
            (EQUALIZER, Command::OpenEqualizer),
            (TOGGLE_EQ, Command::ToggleEq),
            (QUEUE, Command::OpenQueue),
            (SHUFFLE, Command::ToggleShuffle),
            (REPEAT, Command::CycleRepeat),
        ]
        .into_iter()
        .find_map(|(command_id, command)| (command_id == *id).then_some(command))
    }

    /// The player action of this command against the player's state `state` now; `None` for a
    /// command that is not about the player.
    pub(crate) fn action(self, state: &PlayerState) -> Option<PlayerAction> {
        let volume = state.volume;
        match self {
            Command::TogglePlay => Some(PlayerAction::Toggle),
            Command::Previous => Some(PlayerAction::Previous),
            Command::Next => Some(PlayerAction::Next),
            Command::VolumeUp => Some(PlayerAction::SetVolume(
                volume.saturating_add(VOLUME_STEP).min(100),
            )),
            Command::VolumeDown => {
                Some(PlayerAction::SetVolume(volume.saturating_sub(VOLUME_STEP)))
            }
            Command::ToggleEq => Some(PlayerAction::SetEqEnabled(!state.eq.enabled)),
            Command::ToggleShuffle => Some(PlayerAction::SetShuffle(!state.shuffle)),
            Command::CycleRepeat => Some(PlayerAction::SetRepeat(next_repeat(state.repeat))),
            Command::FocusSearch
            | Command::ToggleDiagnostics
            | Command::CycleTheme
            | Command::Back
            | Command::Forward
            | Command::OpenSettings
            | Command::OpenEqualizer
            | Command::OpenQueue => None,
        }
    }
}

/// The repeat mode after `mode`: Off, All, One, Off.
pub(crate) fn next_repeat(mode: RepeatMode) -> RepeatMode {
    match mode {
        RepeatMode::Off => RepeatMode::All,
        RepeatMode::All => RepeatMode::One,
        RepeatMode::One => RepeatMode::Off,
    }
}

/// The table of the diagnostics window: the toggle alone (its own player controls are the main
/// window's).
pub fn diagnostics_table() -> Shortcuts {
    let mut table = Shortcuts::default();
    table.add(Binding::new(
        Scope::Global,
        Chord::key(Key::Character("d".into())).primary().shift(),
        DIAGNOSTICS,
    ));
    table
}

/// The binding table: Space; the platform's primary modifier (Ctrl; Command on macOS) with F or
/// K (search), Comma (settings), E (equalizer), U (queue), S (shuffle), R (repeat), Left, Right,
/// Up and Down, and with Shift and D (the diagnostics window), T (the next theme) or E (the EQ on
/// or off); Alt with Left and Right (history back and forward). All global; none preempts a
/// focused widget. [`labels`] lists the same bindings in the same order.
pub fn table() -> Shortcuts {
    let named = |k| Chord::key(Key::Named(k));
    let character = |c: &str| Chord::key(Key::Character(c.into()));
    let global = |chord, command| Binding::new(Scope::Global, chord, command);
    let mut table = Shortcuts::default();
    table
        .add(global(named(NamedKey::Space), PLAY_PAUSE))
        .add(global(character("f").primary(), SEARCH))
        .add(global(character("k").primary(), SEARCH))
        .add(global(character("d").primary().shift(), DIAGNOSTICS))
        .add(global(character("t").primary().shift(), CYCLE_THEME))
        .add(global(named(NamedKey::ArrowLeft).alt(), BACK))
        .add(global(named(NamedKey::ArrowRight).alt(), FORWARD))
        .add(global(named(NamedKey::ArrowLeft).primary(), PREVIOUS))
        .add(global(named(NamedKey::ArrowRight).primary(), NEXT))
        .add(global(named(NamedKey::ArrowUp).primary(), VOLUME_UP).repeating())
        .add(global(named(NamedKey::ArrowDown).primary(), VOLUME_DOWN).repeating())
        .add(global(character(",").primary(), SETTINGS))
        .add(global(character("e").primary(), EQUALIZER))
        .add(global(character("e").primary().shift(), TOGGLE_EQ))
        .add(global(character("u").primary(), QUEUE))
        .add(global(character("s").primary(), SHUFFLE))
        .add(global(character("r").primary(), REPEAT));
    table
}

/// The label table for a primary modifier named `$primary`.
macro_rules! label_table {
    ($primary:literal) => {
        &[
            ("Space", "Play or pause"),
            (concat!($primary, "+F"), "Search"),
            (concat!($primary, "+K"), "Search"),
            (concat!($primary, "+Shift+D"), "Audio diagnostics"),
            (concat!($primary, "+Shift+T"), "Next look"),
            ("Alt+Left", "Back"),
            ("Alt+Right", "Forward"),
            (concat!($primary, "+Left"), "Previous track"),
            (concat!($primary, "+Right"), "Next track"),
            (concat!($primary, "+Up"), "Volume up"),
            (concat!($primary, "+Down"), "Volume down"),
            (concat!($primary, "+Comma"), "Settings"),
            (concat!($primary, "+E"), "Equalizer"),
            (concat!($primary, "+Shift+E"), "Equalizer on or off"),
            (concat!($primary, "+U"), "Queue"),
            (concat!($primary, "+S"), "Shuffle on or off"),
            (concat!($primary, "+R"), "Repeat: off, all, one"),
        ]
    };
}

#[cfg(target_os = "macos")]
static LABELS: &[(&str, &str)] = label_table!("Cmd");
#[cfg(not(target_os = "macos"))]
static LABELS: &[(&str, &str)] = label_table!("Ctrl");

/// The labels of the bindings of [`table`], in its order: (keys, what it does). The primary
/// modifier reads "Cmd" on macOS and "Ctrl" elsewhere. The Settings page lists them and so does
/// the README.
pub(crate) fn labels() -> &'static [(&'static str, &'static str)] {
    LABELS
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Instant;

    use super::*;
    use whipspot_core::engine::BAND_COUNT;
    use whipspot_core::player::{EqState, LyricsState, OutputStatus, PlayState, QueueState};

    fn state(volume: u8) -> PlayerState {
        PlayerState {
            revision: 0,
            play_state: PlayState::Stopped,
            current: None,
            position_ms: 0,
            position_at: Instant::now(),
            volume,
            balance: 0,
            replay_gain: Default::default(),
            eq: EqState {
                enabled: true,
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

    const ALL: [Command; 16] = [
        Command::TogglePlay,
        Command::Previous,
        Command::Next,
        Command::VolumeUp,
        Command::VolumeDown,
        Command::FocusSearch,
        Command::ToggleDiagnostics,
        Command::CycleTheme,
        Command::Back,
        Command::Forward,
        Command::OpenSettings,
        Command::OpenEqualizer,
        Command::ToggleEq,
        Command::OpenQueue,
        Command::ToggleShuffle,
        Command::CycleRepeat,
    ];

    #[test]
    fn s1_every_binding_names_a_command_and_every_command_has_a_binding() {
        let table = table();
        assert_eq!(table.bindings().len(), 17);
        let bound: Vec<Command> = table
            .bindings()
            .iter()
            .map(|b| Command::from_id(&b.command).unwrap_or_else(|| panic!("{:?}", b.command)))
            .collect();
        for command in ALL {
            assert!(bound.contains(&command), "{command:?} has no binding");
        }
        assert_eq!(Command::from_id(&CommandId::new("other")), None);
        assert_eq!(labels().len(), table.bindings().len());
        // The label of Ctrl+Shift+T is the documented one.
        let prefix = if cfg!(target_os = "macos") {
            "Cmd"
        } else {
            "Ctrl"
        };
        let next_look = format!("{prefix}+Shift+T");
        assert!(
            labels()
                .iter()
                .any(|(keys, what)| *keys == next_look && *what == "Next look")
        );
    }

    #[test]
    fn s2_repeat_cycles_off_all_one() {
        let repeat = |mode| {
            let mut s = state(50);
            s.repeat = mode;
            match Command::CycleRepeat.action(&s) {
                Some(PlayerAction::SetRepeat(next)) => next,
                _ => panic!("CycleRepeat is not a repeat action"),
            }
        };
        assert_eq!(repeat(RepeatMode::Off), RepeatMode::All);
        assert_eq!(repeat(RepeatMode::All), RepeatMode::One);
        assert_eq!(repeat(RepeatMode::One), RepeatMode::Off);
    }

    #[test]
    fn toggles_flip_what_the_state_says() {
        let mut s = state(50);
        assert!(matches!(
            Command::ToggleEq.action(&s),
            Some(PlayerAction::SetEqEnabled(false))
        ));
        assert!(matches!(
            Command::ToggleShuffle.action(&s),
            Some(PlayerAction::SetShuffle(true))
        ));
        s.eq.enabled = false;
        s.shuffle = true;
        assert!(matches!(
            Command::ToggleEq.action(&s),
            Some(PlayerAction::SetEqEnabled(true))
        ));
        assert!(matches!(
            Command::ToggleShuffle.action(&s),
            Some(PlayerAction::SetShuffle(false))
        ));
    }

    #[test]
    fn volume_steps_stay_in_range() {
        let volume = |c: Command, now| match c.action(&state(now)) {
            Some(PlayerAction::SetVolume(v)) => Some(v),
            _ => None,
        };
        assert_eq!(volume(Command::VolumeUp, 98), Some(100));
        assert_eq!(volume(Command::VolumeUp, 70), Some(75));
        assert_eq!(volume(Command::VolumeDown, 3), Some(0));
        assert_eq!(volume(Command::TogglePlay, 50), None);
        let s = state(50);
        assert!(Command::FocusSearch.action(&s).is_none());
        assert!(Command::ToggleDiagnostics.action(&s).is_none());
        for c in [
            Command::CycleTheme,
            Command::Back,
            Command::Forward,
            Command::OpenSettings,
            Command::OpenEqualizer,
            Command::OpenQueue,
        ] {
            assert!(c.action(&s).is_none(), "{c:?}");
        }
    }
}
