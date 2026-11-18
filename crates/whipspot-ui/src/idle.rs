//! The idle/browse state machine (C6, MD50). Every timing of the switch lives here and nowhere
//! else; the shell turns the frame's input into an [`Activity`] and draws what the machine says.
//!
//! Browse shows the sheet and the media control; Idle shows the now-playing view over the
//! visualiser and hides the media control with the sheet. Browse is the start state and returns at
//! once on pointer movement outside the neutral zones, a key press, a scroll or a click. The
//! neutral zones are the caption band and, only while the media control is shown, its panel: with
//! it hidden a pointer move anywhere wakes Browse. Idle comes after the pointer has left the window for [`IDLE_AFTER_LEAVE`] or
//! after [`IDLE_AFTER_STILL`] with no input (a pointer resting inside the window counts as no
//! input), and never while nothing is current, a text field has focus or a pointer button is
//! held, nor while the user's auto-hide switch is off.

use std::time::Duration;

use crate::layout::Layout;

/// App time since an app-chosen origin (what `Ui::time` gives).
pub type Time = Duration;

/// How long the pointer must be outside the window before the view goes idle.
pub const IDLE_AFTER_LEAVE: Duration = Duration::from_secs(2);
/// How long with no input at all (the pointer resting inside the window included) before the view
/// goes idle.
pub const IDLE_AFTER_STILL: Duration = Duration::from_secs(12);
/// The cross-fade between the two views.
pub const FADE: Duration = Duration::from_millis(250);

/// Which view is up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Browse,
    Idle,
}

/// What one frame's input and state say to the machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Activity {
    /// The pointer moved outside the neutral zones.
    pub moved: bool,
    /// The pointer left the window.
    pub left: bool,
    /// A key press, a scroll or a click outside the neutral zones.
    pub input: bool,
    /// The pointer moved, pressed or scrolled inside a neutral zone (the caption band, the media
    /// control while it is shown): it never wakes Browse, but it shows someone is there.
    pub neutral: bool,
    /// A text field has focus.
    pub text_focus: bool,
    /// A pointer button is held.
    pub button_held: bool,
    /// Something is current.
    pub has_current: bool,
    /// The user's auto-hide switch is on; when off, Browse never goes Idle on its own.
    pub auto_hide: bool,
}

/// Whether `pos` is in a neutral zone: the caption band always, the media control's panel only
/// while `media_shown` (it is hidden in the idle view, where any movement must wake Browse).
pub(crate) fn is_neutral(pos: (f32, f32), layout: &Layout, media_shown: bool) -> bool {
    layout.caption.contains(pos) || (media_shown && layout.media.contains(pos))
}

/// The machine: the mode, the two timers and the fade bookkeeping.
#[derive(Debug, Clone, Default)]
pub struct IdleMachine {
    mode: Mode,
    /// The last time something happened (the origin of the still timer).
    last_input: Option<Time>,
    /// When the pointer left the window, until it comes back.
    left_at: Option<Time>,
    /// Whether the last update allowed Idle at all.
    can_idle: bool,
    /// The fade value when the mode last changed, and when.
    fade_from: f32,
    changed_at: Time,
}

fn target(mode: Mode) -> f32 {
    match mode {
        Mode::Browse => 0.0,
        Mode::Idle => 1.0,
    }
}

impl IdleMachine {
    pub fn new() -> Self {
        Self::default()
    }

    /// The mode now.
    #[cfg(test)]
    fn mode(&self) -> Mode {
        self.mode
    }

    fn set_mode(&mut self, mode: Mode, now: Time) {
        if mode != self.mode {
            self.fade_from = self.fade(now);
            self.changed_at = now;
            self.mode = mode;
        }
    }

    /// Feeds one frame at app time `now`; returns the mode to draw.
    pub fn update(&mut self, now: Time, a: Activity) -> Mode {
        self.last_input.get_or_insert(now);
        if a.moved || a.input || a.neutral {
            self.last_input = Some(now);
            self.left_at = None;
        }
        if a.left {
            self.left_at = Some(now);
        }
        if a.moved || a.input {
            self.set_mode(Mode::Browse, now);
        }
        self.can_idle = a.auto_hide && a.has_current && !a.text_focus && !a.button_held;
        if !a.has_current || a.text_focus || !a.auto_hide {
            // Idle is not for this state (or the user switched it off): the sheet comes back, and
            // the timers start over.
            self.set_mode(Mode::Browse, now);
            self.last_input = Some(now);
            return self.mode;
        }
        if a.button_held {
            // A drag (the ring's seek) keeps whichever view it started on.
            self.last_input = Some(now);
            return self.mode;
        }
        if self.mode == Mode::Browse {
            let last = self.last_input.unwrap_or(now);
            let leave_due = self.left_at.is_some_and(|t| now >= t + IDLE_AFTER_LEAVE);
            let still_due = now >= last + IDLE_AFTER_STILL;
            if leave_due || still_due {
                self.set_mode(Mode::Idle, now);
            }
        }
        self.mode
    }

    /// The next time the mode may switch to Idle on its own; `None` when it is Idle already or
    /// cannot go Idle in the state the last update saw. The shell asks for a repaint then.
    pub fn deadline(&self) -> Option<Time> {
        if self.mode == Mode::Idle || !self.can_idle {
            return None;
        }
        let still = self.last_input.map(|t| t + IDLE_AFTER_STILL);
        let leave = self.left_at.map(|t| t + IDLE_AFTER_LEAVE);
        match (still, leave) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// The cross-fade at `now`: 0 is Browse fully shown, 1 is Idle fully shown.
    pub fn fade(&self, now: Time) -> f32 {
        let to = target(self.mode);
        let t = if FADE.is_zero() {
            1.0
        } else {
            (now.saturating_sub(self.changed_at).as_secs_f32() / FADE.as_secs_f32()).clamp(0.0, 1.0)
        };
        let eased = t * t * (3.0 - 2.0 * t);
        self.fade_from + (to - self.fade_from) * eased
    }

    /// Whether the fade is still moving at `now` (the shell then repaints).
    pub fn fading(&self, now: Time) -> bool {
        (self.fade(now) - target(self.mode)).abs() > 0.001
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: fn(u64) -> Time = Duration::from_secs;

    /// The still delay in whole seconds (the tests step in seconds).
    fn still() -> u64 {
        IDLE_AFTER_STILL.as_secs()
    }

    fn playing() -> Activity {
        Activity {
            has_current: true,
            auto_hide: true,
            ..Activity::default()
        }
    }

    #[test]
    fn i6_the_media_control_is_neutral_only_while_shown() {
        use lune_ui::paint::Rect;
        let l = Layout::new(Rect::new(0.0, 0.0, 1440.0, 900.0));
        let in_media = (l.media.x + 10.0, l.media.y + 10.0);
        let in_caption = (200.0, 10.0);
        let in_sheet = (400.0, 300.0);
        assert!(is_neutral(in_media, &l, true));
        assert!(
            !is_neutral(in_media, &l, false),
            "hidden: a move wakes Browse"
        );
        assert!(is_neutral(in_caption, &l, true) && is_neutral(in_caption, &l, false));
        assert!(!is_neutral(in_sheet, &l, true) && !is_neutral(in_sheet, &l, false));
        // Through the machine: a non-neutral move in Idle wakes it.
        let mut m = IdleMachine::new();
        m.update(S(0), playing());
        assert_eq!(m.update(S(still()), playing()), Mode::Idle);
        let moved = Activity {
            moved: !is_neutral(in_media, &l, false),
            neutral: is_neutral(in_media, &l, false),
            ..playing()
        };
        assert_eq!(m.update(S(still() + 1), moved), Mode::Browse);
    }

    #[test]
    fn i1_the_delays_are_twelve_and_two_seconds() {
        // Twelve seconds with the pointer resting inside; leaving is quicker still.
        assert_eq!(IDLE_AFTER_STILL, S(12));
        assert_eq!(IDLE_AFTER_LEAVE, S(2));
        assert!(IDLE_AFTER_LEAVE < IDLE_AFTER_STILL);
        assert_eq!(
            IDLE_AFTER_STILL.subsec_nanos(),
            0,
            "the tests step in seconds"
        );
    }

    #[test]
    fn i1_starts_in_browse_and_idles_after_still() {
        let mut m = IdleMachine::new();
        assert_eq!(m.mode(), Mode::Browse);
        assert_eq!(m.update(S(0), playing()), Mode::Browse);
        assert_eq!(m.update(S(still() - 1), playing()), Mode::Browse);
        assert_eq!(m.deadline(), Some(S(still())));
        assert_eq!(m.update(S(still()), playing()), Mode::Idle);
        assert_eq!(m.deadline(), None);
    }

    #[test]
    fn i1_a_pointer_resting_inside_the_window_idles_after_the_delay() {
        // One move, then nothing (the pointer stays inside: no `left`): idle `still` seconds on.
        let mut m = IdleMachine::new();
        let moved = Activity {
            moved: true,
            ..playing()
        };
        assert_eq!(m.update(S(10), moved), Mode::Browse);
        assert_eq!(m.update(S(10 + still() - 1), playing()), Mode::Browse);
        assert_eq!(m.deadline(), Some(S(10 + still())));
        assert_eq!(m.update(S(10 + still()), playing()), Mode::Idle);
        // A fresh move wakes it at once.
        assert_eq!(m.update(S(20 + still()), moved), Mode::Browse);
    }

    #[test]
    fn i1_leaving_idles_after_two_seconds() {
        let mut m = IdleMachine::new();
        m.update(S(0), playing());
        let left = Activity {
            left: true,
            ..playing()
        };
        assert_eq!(m.update(S(1), left), Mode::Browse);
        let due = 1 + IDLE_AFTER_LEAVE.as_secs();
        assert_eq!(m.deadline(), Some(S(due)));
        assert_eq!(m.update(S(due - 1), playing()), Mode::Browse);
        assert_eq!(m.update(S(due), playing()), Mode::Idle);
    }

    #[test]
    fn i1_a_move_wakes_at_once_and_resets() {
        let mut m = IdleMachine::new();
        m.update(S(0), playing());
        assert_eq!(m.update(S(still() + 1), playing()), Mode::Idle);
        let moved = Activity {
            moved: true,
            ..playing()
        };
        let t = still() + 2;
        assert_eq!(m.update(S(t), moved), Mode::Browse);
        assert_eq!(m.update(S(t + still() - 1), playing()), Mode::Browse);
        assert_eq!(m.update(S(t + still()), playing()), Mode::Idle);
        // A key press and a click or scroll (`input`) wake too.
        let key = Activity {
            input: true,
            ..playing()
        };
        assert_eq!(m.update(S(t + still() + 1), key), Mode::Browse);
    }

    #[test]
    fn i1_neutral_moves_do_not_wake() {
        let mut m = IdleMachine::new();
        m.update(S(0), playing());
        assert_eq!(m.update(S(still() + 1), playing()), Mode::Idle);
        let neutral = Activity {
            neutral: true,
            ..playing()
        };
        assert_eq!(m.update(S(still() + 2), neutral), Mode::Idle);
        assert_eq!(m.update(S(still() + 10), neutral), Mode::Idle);
    }

    #[test]
    fn i1_neutral_activity_restarts_the_still_timer_in_browse() {
        let mut m = IdleMachine::new();
        m.update(S(0), playing());
        let neutral = Activity {
            neutral: true,
            ..playing()
        };
        let t = still() - 1;
        assert_eq!(m.update(S(t), neutral), Mode::Browse);
        assert_eq!(m.update(S(t + still() - 1), playing()), Mode::Browse);
        assert_eq!(m.update(S(t + still()), playing()), Mode::Idle);
    }

    #[test]
    fn i1_blockers_never_go_idle() {
        let blocked = [
            Activity {
                text_focus: true,
                ..playing()
            },
            Activity {
                button_held: true,
                ..playing()
            },
            Activity {
                auto_hide: false,
                ..playing()
            },
            Activity::default(),
        ];
        for a in blocked {
            let mut m = IdleMachine::new();
            m.update(S(0), a);
            assert_eq!(m.update(S(100), a), Mode::Browse, "{a:?}");
            assert_eq!(m.deadline(), None, "{a:?}");
            let left = Activity { left: true, ..a };
            m.update(S(101), left);
            assert_eq!(m.update(S(200), a), Mode::Browse, "{a:?}");
        }
    }

    #[test]
    fn i1_switching_auto_hide_off_brings_the_sheet_back_and_on_starts_the_timer() {
        let mut m = IdleMachine::new();
        m.update(S(0), playing());
        assert_eq!(m.update(S(still()), playing()), Mode::Idle);
        let off = Activity {
            auto_hide: false,
            ..playing()
        };
        assert_eq!(m.update(S(still() + 1), off), Mode::Browse);
        assert_eq!(m.deadline(), None);
        // Switched on again: the delay counts from the last frame it was off.
        let t = 1000;
        assert_eq!(m.update(S(t), off), Mode::Browse);
        assert_eq!(m.update(S(t + 1), playing()), Mode::Browse);
        assert_eq!(m.update(S(t + still() - 1), playing()), Mode::Browse);
        assert_eq!(m.update(S(t + still()), playing()), Mode::Idle);
    }

    #[test]
    fn i1_nothing_current_pulls_idle_back() {
        let mut m = IdleMachine::new();
        m.update(S(0), playing());
        assert_eq!(m.update(S(still() + 1), playing()), Mode::Idle);
        assert_eq!(m.update(S(still() + 2), Activity::default()), Mode::Browse);
    }

    #[test]
    fn i1_a_held_button_keeps_the_view_it_started_on() {
        let mut m = IdleMachine::new();
        m.update(S(0), playing());
        assert_eq!(m.update(S(still() + 1), playing()), Mode::Idle);
        let held = Activity {
            button_held: true,
            ..playing()
        };
        assert_eq!(m.update(S(still() + 2), held), Mode::Idle);
    }

    #[test]
    fn i1_the_fade_runs_over_fade_and_reverses_from_where_it_is() {
        let t0 = S(still());
        let mut m = IdleMachine::new();
        m.update(S(0), playing());
        assert_eq!(m.fade(S(1)), 0.0);
        m.update(t0, playing());
        assert_eq!(m.mode(), Mode::Idle);
        assert!(m.fading(t0));
        assert_eq!(m.fade(t0), 0.0);
        let half = t0 + FADE / 2;
        assert!((m.fade(half) - 0.5).abs() < 1e-3);
        assert_eq!(m.fade(t0 + FADE), 1.0);
        assert!(!m.fading(t0 + S(1)));
        // A wake half way back starts from the half-way value.
        let mut m = IdleMachine::new();
        m.update(S(0), playing());
        m.update(t0, playing());
        let moved = Activity {
            moved: true,
            ..playing()
        };
        m.update(half, moved);
        assert!((m.fade(half) - 0.5).abs() < 1e-3);
        assert_eq!(m.fade(half + FADE), 0.0);
    }
}
