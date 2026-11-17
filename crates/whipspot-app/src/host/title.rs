//! The OS window title and its taskbar marquee (MD52), pure: the host feeds it the view's proposal
//! and the time, and applies what `poll` returns with `Window::set_title`.

use std::time::{Duration, Instant};

use whipspot_ui::title::WindowTitle;

/// One character of rotation per this long (at most 4 title changes a second).
pub(super) const STEP: Duration = Duration::from_millis(250);
/// Between the end of the text and its next start while rotating.
const GAP: &str = "   ";

/// What the window title shows now: the proposed text, rotated one character per [`STEP`] while
/// the proposal scrolls and the OS has a marquee.
pub(super) struct TitleMarquee {
    /// Whether this OS rotates the title at all (macOS sets the static text only).
    marquee: bool,
    /// The proposal, with the time it was first seen (the rotation starts there).
    proposal: Option<(WindowTitle, Instant)>,
    /// The last time given to `set` or `poll`.
    seen: Instant,
    /// What the window currently shows.
    applied: String,
}

impl TitleMarquee {
    /// A marquee over a window that shows `initial` (its creation title).
    pub(super) fn new(marquee: bool, initial: &str, now: Instant) -> Self {
        Self {
            marquee,
            proposal: None,
            seen: now,
            applied: initial.to_string(),
        }
    }

    /// The view's current proposal. The same proposal again changes nothing; a new text (or a
    /// change of `scroll`) restarts the rotation at `now`.
    pub(super) fn set(&mut self, title: WindowTitle, now: Instant) {
        self.seen = self.seen.max(now);
        if self.proposal.as_ref().is_some_and(|(t, _)| *t == title) {
            return;
        }
        self.proposal = Some((title, now));
    }

    fn rotating(&self) -> bool {
        self.marquee && self.proposal.as_ref().is_some_and(|(t, _)| t.scroll)
    }

    /// The step of the rotation at `now`.
    fn step_at(&self, now: Instant) -> u128 {
        match &self.proposal {
            Some((_, start)) => {
                now.saturating_duration_since(*start).as_millis() / STEP.as_millis()
            }
            None => 0,
        }
    }

    /// When the text next rotates: only while a scrolling proposal has a marquee.
    pub(super) fn due(&self) -> Option<Instant> {
        if !self.rotating() {
            return None;
        }
        let (_, start) = self.proposal.as_ref()?;
        let next = u32::try_from(self.step_at(self.seen) + 1).ok()?;
        start.checked_add(STEP.checked_mul(next)?)
    }

    /// The title to set at `now`, or `None` when it would not differ from what is shown.
    pub(super) fn poll(&mut self, now: Instant) -> Option<String> {
        self.seen = self.seen.max(now);
        let (title, _) = self.proposal.as_ref()?;
        let text = if self.rotating() {
            let chars: Vec<char> = title.text.chars().chain(GAP.chars()).collect();
            let k = (self.step_at(now) % chars.len() as u128) as usize;
            chars[k..].iter().chain(&chars[..k]).collect()
        } else {
            title.text.clone()
        };
        if text == self.applied {
            return None;
        }
        self.applied.clone_from(&text);
        Some(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn title(text: &str, scroll: bool) -> WindowTitle {
        WindowTitle {
            text: text.into(),
            scroll,
        }
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn m1_a_static_text_is_applied_once() {
        let t0 = Instant::now();
        let mut m = TitleMarquee::new(true, "", t0);
        m.set(title("Whipspot", false), t0);
        assert_eq!(m.poll(t0).as_deref(), Some("Whipspot"));
        assert_eq!(m.poll(t0 + ms(10)), None);
        m.set(title("Whipspot", false), t0 + ms(20));
        assert_eq!(m.poll(t0 + ms(20)), None);
    }

    #[test]
    fn a_text_equal_to_the_creation_title_is_not_applied() {
        let t0 = Instant::now();
        let mut m = TitleMarquee::new(true, "Whipspot", t0);
        m.set(title("Whipspot", false), t0);
        assert_eq!(m.poll(t0), None);
    }

    #[test]
    fn m2_scrolling_rotates_one_char_per_step() {
        let t0 = Instant::now();
        let mut m = TitleMarquee::new(true, "", t0);
        m.set(title("abc", true), t0);
        assert_eq!(m.due(), Some(t0 + ms(250)));
        let a = m.poll(t0).expect("first");
        assert_eq!(a, "abc   ");
        assert_eq!(m.poll(t0 + ms(100)), None);
        let b = m.poll(t0 + ms(250)).expect("second");
        assert_eq!(b, "bc   a");
        assert_eq!(m.due(), Some(t0 + ms(500)));
        let c = m.poll(t0 + ms(500)).expect("third");
        assert_eq!(c, "c   ab");
        assert!(a != b && b != c && a != c);
    }

    #[test]
    fn rotation_wraps_over_text_and_gap() {
        let t0 = Instant::now();
        let mut m = TitleMarquee::new(true, "", t0);
        m.set(title("ab", true), t0);
        let first = m.poll(t0).expect("first");
        assert_eq!(first, "ab   ");
        // Five characters per turn: step 5 shows the text again.
        assert_eq!(m.poll(t0 + STEP * 5).as_deref(), None);
        assert_eq!(m.poll(t0 + STEP * 6).as_deref(), Some("b   a"));
    }

    #[test]
    fn m3_a_new_text_resets_the_rotation() {
        let t0 = Instant::now();
        let mut m = TitleMarquee::new(true, "", t0);
        m.set(title("abc", true), t0);
        m.poll(t0);
        m.poll(t0 + ms(500));
        let t1 = t0 + ms(600);
        m.set(title("xyz", true), t1);
        assert_eq!(m.poll(t1).as_deref(), Some("xyz   "));
        assert_eq!(m.due(), Some(t1 + ms(250)));
        assert_eq!(m.poll(t1 + ms(250)).as_deref(), Some("yz   x"));
    }

    #[test]
    fn m4_not_scrolling_shows_the_static_text_and_has_no_deadline() {
        let t0 = Instant::now();
        let mut m = TitleMarquee::new(true, "", t0);
        m.set(title("abc", true), t0);
        m.poll(t0 + ms(250));
        m.set(title("abc", false), t0 + ms(300));
        assert_eq!(m.poll(t0 + ms(300)).as_deref(), Some("abc"));
        assert_eq!(m.due(), None);
        assert_eq!(m.poll(t0 + ms(900)), None);
    }

    #[test]
    fn m5_without_a_marquee_the_text_stays_static() {
        let t0 = Instant::now();
        let mut m = TitleMarquee::new(false, "", t0);
        m.set(title("abc", true), t0);
        assert_eq!(m.poll(t0).as_deref(), Some("abc"));
        assert_eq!(m.due(), None);
        assert_eq!(m.poll(t0 + ms(750)), None);
    }

    #[test]
    fn m6_multibyte_text_rotates_on_char_boundaries() {
        let t0 = Instant::now();
        let mut m = TitleMarquee::new(true, "", t0);
        m.set(title("Beyonc\u{e9} - \u{591c}", true), t0);
        let mut prev = String::new();
        for i in 0..40 {
            if let Some(s) = m.poll(t0 + STEP * i) {
                assert_ne!(s, prev);
                assert_eq!(
                    s.chars().count(),
                    "Beyonc\u{e9} - \u{591c}".chars().count() + 3
                );
                prev = s;
            }
        }
        assert!(!prev.is_empty());
    }

    #[test]
    fn nothing_proposed_changes_nothing() {
        let t0 = Instant::now();
        let mut m = TitleMarquee::new(true, "Whipspot", t0);
        assert_eq!(m.poll(t0), None);
        assert_eq!(m.due(), None);
    }
}
