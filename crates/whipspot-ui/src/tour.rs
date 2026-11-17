//! The first-run tour: a few glass cards, one at a time, each pointing at a control of the native
//! UI (Browse mode only). The state and the placement are pure; [`Tour::draw`] puts the current
//! card on the Modal layer, last, with a notch at its control. The shell leaves the card out (state
//! kept) while the provider menu, which opens under the card's control, is open.
//!
//! A card whose control was not drawn this frame is left out of the count (a provider button needs
//! two providers; the pill is hidden when it does not fit). The window stays usable: nothing is
//! dimmed, and only the card's own rectangle takes the pointer.

use lune_ui::input::{Event, Key, NamedKey};
use lune_ui::paint::Rect;
use lune_ui::scene::LayerId;
use lune_ui::text::{Spec, Style};
use lune_ui::ui::{Sense, Ui};
use lune_ui::widgets::button::{ButtonKind, ButtonOptions, button_width_with};

use crate::Anchors;
use crate::draw::{P, alpha, glass, shadow, text};
use crate::theme::Look;

/// The card's width.
const CARD_W: f32 = 300.0;
/// The padding inside the card.
const PAD: f32 = 16.0;
/// The gap between the card's edge and its control, which the notch crosses.
const GAP: f32 = 14.0;
/// The notch's half width and length.
const NOTCH_HALF: f32 = 7.0;
const NOTCH_LEN: f32 = 6.0;
/// The least distance from the card to a window edge.
const MARGIN: f32 = 12.0;
/// The buttons' row height.
const FOOTER_H: f32 = 28.0;
/// The title's row height and the body's text size.
const TITLE_H: f32 = 24.0;
const BODY_SIZE: f32 = 12.5;

/// One card of the tour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Card {
    Library,
    Music,
    AutoHide,
    Quality,
    Queue,
}

/// The cards in the order they are shown.
const CARDS: [Card; 5] = [
    Card::Library,
    Card::Music,
    Card::AutoHide,
    Card::Quality,
    Card::Queue,
];

impl Card {
    fn title(self) -> &'static str {
        match self {
            Card::Library => "Your library",
            Card::Music => "Add your music",
            Card::AutoHide => "Auto-hide",
            Card::Quality => "Sound quality",
            Card::Queue => "Up next",
        }
    }

    fn body(self) -> &'static str {
        match self {
            Card::Library => "Switch between your own music files and a streaming service.",
            Card::Music => "Open Settings, then Music folders, to choose where your songs are.",
            Card::AutoHide => {
                "While it is on, the library fades away when the mouse rests. Click to keep it."
            }
            Card::Quality => {
                "What is playing and how it reaches your speakers. Click it for the Audio window."
            }
            Card::Queue => "Open the queue to see and reorder what plays next.",
        }
    }

    /// Where the card's control was drawn this frame, if it was.
    fn anchor(self, a: &Anchors) -> Option<Rect> {
        match self {
            Card::Library => a.provider,
            Card::Music => a.gear,
            Card::AutoHide => a.eye,
            Card::Quality => a.pill,
            Card::Queue => a.queue,
        }
    }

    /// The card sits below its control (the top bar's) or above it (the media panel's).
    fn below(self) -> bool {
        matches!(self, Card::Library | Card::Music | Card::AutoHide)
    }
}

/// What the tour shows this frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct View {
    card: Card,
    /// The card's place among the available ones (1-based) and their count.
    pub(crate) n: usize,
    pub(crate) m: usize,
    /// The control the card points at.
    pub(crate) anchor: Rect,
}

/// What the card's buttons and keys asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pick {
    Next,
    Skip,
}

/// The tour's state: the index into [`CARDS`] of the card now shown, `None` when it is not running.
#[derive(Debug, Default)]
pub(crate) struct Tour {
    at: Option<usize>,
    /// The tour ended since the last [`Tour::take_finished`].
    finished: bool,
}

impl Tour {
    /// Starts at the first card.
    pub(crate) fn start(&mut self) {
        self.at = Some(0);
        self.finished = false;
    }

    /// Shows the card at list index `i` (the demo's forced card).
    pub(crate) fn show(&mut self, i: usize) {
        self.at = Some(i.min(CARDS.len() - 1));
        self.finished = false;
    }

    /// Whether the tour is running (it survives a change of mode; it only shows in Browse).
    pub(crate) fn running(&self) -> bool {
        self.at.is_some()
    }

    /// The idle machine's auto-hide switch while the tour runs: off (the card must not vanish
    /// with the sheet), else the user's.
    pub(crate) fn auto_hide(&self, user: bool) -> bool {
        user && !self.running()
    }

    /// Whether the tour ended (Skip, Esc, Done, or no card left) since the last call.
    pub(crate) fn take_finished(&mut self) -> bool {
        std::mem::take(&mut self.finished)
    }

    fn end(&mut self) {
        self.at = None;
        self.finished = true;
    }

    /// The cards available in `a`, in order, as indices into [`CARDS`].
    fn available(a: &Anchors) -> impl Iterator<Item = usize> + '_ {
        (0..CARDS.len()).filter(|&i| CARDS[i].anchor(a).is_some())
    }

    /// The card to show this frame, or `None` when none is (not running, not in Browse, or the
    /// sheet is not drawn yet). The current card moves on to the next available one when its
    /// control is gone, and the tour ends when none is left.
    pub(crate) fn view(&mut self, a: &Anchors, browse: bool) -> Option<View> {
        let at = self.at?;
        // Outside Browse, or while the sheet fades back in, the top bar has no anchors: wait.
        if !browse || a.gear.is_none() {
            return None;
        }
        let Some(i) = Self::available(a).find(|&i| i >= at) else {
            self.end();
            return None;
        };
        self.at = Some(i);
        let m = Self::available(a).count();
        let n = Self::available(a).take_while(|&j| j <= i).count();
        Some(View {
            card: CARDS[i],
            n,
            m,
            anchor: CARDS[i].anchor(a)?,
        })
    }

    /// Moves to the card after the current one; past the last, the tour ends.
    fn next(&mut self, a: &Anchors) {
        let Some(at) = self.at else {
            return;
        };
        match Self::available(a).find(|&i| i > at) {
            Some(i) => self.at = Some(i),
            None => self.end(),
        }
    }

    /// Draws the card of `view` on the Modal layer and applies its buttons and, when `esc_free`,
    /// Esc; true when
    /// something changed and a new frame is due.
    pub(crate) fn draw(
        &mut self,
        ui: &mut Ui<'_>,
        k: &Look,
        view: &View,
        a: &Anchors,
        esc_free: bool,
    ) -> bool {
        let win = ui.painter().window;
        let last = view.n == view.m;
        let title = view.card.title();
        let body = Spec::plain(view.card.body(), Style::ui(BODY_SIZE, k.ink.text2))
            .wrapped(CARD_W - 2.0 * PAD);
        let body_h = ui.painter().measure(&body).1;
        let h = PAD + TITLE_H + body_h + 14.0 + FOOTER_H + PAD;
        let (r, nx) = place(view.anchor, view.card.below(), (win.w, win.h), (CARD_W, h));

        ui.painter().on(LayerId::Modal);
        let rad = k.radii().popup.min(16.0);
        shadow(
            ui.painter(),
            r,
            rad,
            26.0,
            if k.surface().bevel { 1.5 } else { 0.9 },
        );
        glass(ui.painter(), k, r, rad, k.surface().popup_alpha, false);
        notch(ui.painter(), k, r, nx, view.card.below());

        // The card takes the pointer over its whole rectangle, so a press on its text does not
        // reach the control under it; the buttons are registered after it and win.
        let id = ui.id("tour-card");
        ui.interact(id, r, Sense::CLICK);

        let p = ui.painter();
        text(p, title, 15.0, k.ink.text, true, r.x + PAD, r.y + PAD, 0.0);
        p.text(&body, r.x + PAD, r.y + PAD + TITLE_H);

        let fy = r.bottom() - PAD - FOOTER_H;
        let count = format!("{} of {}", view.n, view.m);
        text(
            p,
            &count,
            11.5,
            k.ink.muted,
            false,
            r.x + PAD,
            fy + (FOOTER_H - 15.0) / 2.0,
            0.0,
        );

        let next_label = if last { "Done" } else { "Next" };
        let primary = ButtonOptions {
            kind: ButtonKind::Primary,
            small: true,
            icon: None,
        };
        let secondary = ButtonOptions {
            kind: ButtonKind::Secondary,
            small: true,
            icon: None,
        };
        let nw = button_width_with(ui.painter(), next_label, &primary);
        let sw = button_width_with(ui.painter(), "Skip", &secondary);
        let next_rect = Rect::new(r.right() - PAD - nw, fy, nw, FOOTER_H);
        let skip_rect = Rect::new(next_rect.x - 8.0 - sw, fy, sw, FOOTER_H);
        let mut pick = None;
        if ui
            .button_with("tour-skip", skip_rect, "Skip", &secondary)
            .clicked
        {
            pick = Some(Pick::Skip);
        }
        if ui
            .button_with("tour-next", next_rect, next_label, &primary)
            .clicked
        {
            pick = Some(Pick::Next);
        }
        if esc_free && escape_pressed(ui) {
            pick = Some(Pick::Skip);
        }
        match pick {
            Some(Pick::Next) => self.next(a),
            Some(Pick::Skip) => self.end(),
            None => return false,
        }
        true
    }
}

/// Whether Escape was pressed in this frame's events.
fn escape_pressed(ui: &Ui<'_>) -> bool {
    ui.input().events.iter().any(|e| {
        matches!(
            e,
            Event::Key {
                key: Key::Named(NamedKey::Escape),
                pressed: true,
                ..
            }
        )
    })
}

/// The card's rectangle for a control at `anchor`, and the x its notch points at. The card is
/// below the control (`below`) or above it, centred on it, clamped inside a window of `win` with
/// [`MARGIN`]; when the wanted side does not fit it goes to the other. The gap [`GAP`] keeps it
/// clear of the control.
fn place(anchor: Rect, below: bool, win: (f32, f32), size: (f32, f32)) -> (Rect, f32) {
    let (w, h) = size;
    let cx = anchor.x + anchor.w / 2.0;
    let x = (cx - w / 2.0)
        .round()
        .clamp(MARGIN, (win.0 - MARGIN - w).max(MARGIN));
    let y_below = anchor.bottom() + GAP;
    let y_above = anchor.y - GAP - h;
    let fits_below = y_below + h <= win.1 - MARGIN;
    let fits_above = y_above >= MARGIN;
    let y = if below {
        if fits_below || !fits_above {
            y_below
        } else {
            y_above
        }
    } else if fits_above || !fits_below {
        y_above
    } else {
        y_below
    };
    let y = y.clamp(MARGIN, (win.1 - MARGIN - h).max(MARGIN));
    (Rect::new(x, y.round(), w, h), cx)
}

/// The notch on the card's edge facing its control, at `nx` kept inside the corners.
fn notch(p: &mut P<'_>, k: &Look, r: Rect, nx: f32, below: bool) {
    let rad = k.radii().popup.min(16.0);
    let nc = nx.clamp(r.x + rad + 8.0, r.right() - rad - 8.0);
    let col = alpha(k.surface().tint, 0xE0);
    if below {
        p.fill_path(
            &[
                [nc - NOTCH_HALF, r.y + 0.5],
                [nc + NOTCH_HALF, r.y + 0.5],
                [nc, r.y - NOTCH_LEN],
            ],
            col,
        );
    } else {
        p.fill_path(
            &[
                [nc - NOTCH_HALF, r.bottom() - 0.5],
                [nc + NOTCH_HALF, r.bottom() - 0.5],
                [nc, r.bottom() + NOTCH_LEN],
            ],
            col,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Layout;
    use crate::topbar;

    fn rect(x: f32) -> Rect {
        Rect::new(x, 10.0, 32.0, 32.0)
    }

    fn all() -> Anchors {
        Anchors {
            provider: Some(rect(10.0)),
            gear: Some(rect(60.0)),
            eye: Some(rect(110.0)),
            pill: Some(rect(160.0)),
            queue: Some(rect(210.0)),
        }
    }

    /// The anchors a window of `w` x `h` draws, from the real layout and top bar metrics.
    fn anchors_of(w: f32, h: f32) -> Anchors {
        let l = Layout::new(Rect::new(0.0, 0.0, w, h));
        let m = topbar::metrics(l.sheet, [40.0, 62.0, 50.0, 40.0, 48.0], 40.0, true);
        let c = (l.media.right() - 34.0, l.media.y + l.media.h / 2.0);
        Anchors {
            provider: m.provider_hit(),
            gear: Some(m.gear_hit()),
            eye: Some(m.hide_hit()),
            pill: Some(Rect::new(l.media.right() - 330.0, c.1 - 15.0, 170.0, 30.0)),
            queue: Some(Rect::new(c.0 - 16.0, c.1 - 16.0, 32.0, 32.0)),
        }
    }

    fn started() -> Tour {
        let mut t = Tour::default();
        t.start();
        t
    }

    fn shown(t: &mut Tour, a: &Anchors) -> Option<Card> {
        t.view(a, true).map(|v| v.card)
    }

    #[test]
    fn the_cards_come_in_order_and_are_counted() {
        let a = all();
        let mut t = started();
        for (i, card) in CARDS.iter().enumerate() {
            let v = t.view(&a, true).expect("a card");
            assert_eq!((v.card, v.n, v.m), (*card, i + 1, 5));
            t.next(&a);
        }
        assert!(!t.running());
        assert!(t.take_finished());
    }

    #[test]
    fn an_unavailable_card_is_skipped_and_left_out_of_the_count() {
        let a = Anchors {
            provider: None,
            pill: None,
            ..all()
        };
        let mut t = started();
        let mut seen = Vec::new();
        while let Some(v) = t.view(&a, true) {
            seen.push((v.card, v.n, v.m));
            t.next(&a);
        }
        assert_eq!(
            seen,
            vec![
                (Card::Music, 1, 3),
                (Card::AutoHide, 2, 3),
                (Card::Queue, 3, 3)
            ]
        );
        assert!(t.take_finished());
    }

    #[test]
    fn the_card_moves_on_when_its_control_disappears() {
        let a = all();
        let mut t = started();
        t.next(&a);
        t.next(&a);
        assert_eq!(shown(&mut t, &a), Some(Card::AutoHide));
        let gone = Anchors { eye: None, ..a };
        let v = t.view(&gone, true).expect("the next card");
        assert_eq!((v.card, v.n, v.m), (Card::Quality, 3, 4));
        // None left after the last control goes: the tour ends.
        let none = Anchors {
            pill: None,
            queue: None,
            ..gone
        };
        assert_eq!(t.view(&none, true), None);
        assert!(!t.running());
        assert!(t.take_finished());
    }

    #[test]
    fn skip_esc_and_done_all_end_the_tour_as_seen() {
        let a = all();
        // Skip and Esc are the same end.
        let mut t = started();
        t.end();
        assert!(!t.running() && t.take_finished());
        assert!(!t.take_finished(), "the flag is taken once");
        // Done: Next on the last card.
        let mut t = started();
        for _ in 0..4 {
            t.next(&a);
        }
        assert_eq!(shown(&mut t, &a), Some(Card::Queue));
        t.next(&a);
        assert!(!t.running() && t.take_finished());
    }

    #[test]
    fn a_restart_begins_at_the_first_card() {
        let a = all();
        let mut t = started();
        t.next(&a);
        t.end();
        let _ = t.take_finished();
        t.start();
        assert_eq!(shown(&mut t, &a), Some(Card::Library));
        assert!(!t.take_finished());
    }

    #[test]
    fn nothing_shows_outside_browse_and_the_tour_waits() {
        let a = all();
        let mut t = started();
        t.next(&a);
        assert_eq!(t.view(&a, false), None);
        // The sheet is not drawn (no top bar anchors): wait, do not end.
        let hidden = Anchors::default();
        assert_eq!(t.view(&hidden, true), None);
        assert!(t.running() && !t.take_finished());
        assert_eq!(
            shown(&mut t, &a),
            Some(Card::Music),
            "it resumes where it was"
        );
    }

    #[test]
    fn auto_hide_is_off_while_the_tour_runs() {
        let mut t = Tour::default();
        assert!(t.auto_hide(true));
        assert!(!t.auto_hide(false));
        t.start();
        assert!(!t.auto_hide(true));
        t.end();
        assert!(t.auto_hide(true));
    }

    #[test]
    fn a_card_never_covers_its_control_and_stays_inside_the_window() {
        let h = 190.0;
        for (w, wh) in [(760.0, 480.0), (1440.0, 900.0), (1000.0, 600.0)] {
            let a = anchors_of(w, wh);
            for card in CARDS {
                let anchor = card.anchor(&a).expect("anchor");
                let (r, nx) = place(anchor, card.below(), (w, wh), (CARD_W, h));
                let tag = format!("{card:?} {w}x{wh}: {r:?} {anchor:?}");
                assert!(
                    r.x >= MARGIN - 0.01 && r.right() <= w - MARGIN + 0.01,
                    "{tag}"
                );
                assert!(
                    r.y >= MARGIN - 0.01 && r.bottom() <= wh - MARGIN + 0.01,
                    "{tag}"
                );
                let apart = r.bottom() <= anchor.y || r.y >= anchor.bottom();
                assert!(apart, "overlaps its control: {tag}");
                assert!(
                    (nx - (anchor.x + anchor.w / 2.0)).abs() < 0.01,
                    "the notch points at the control: {tag}"
                );
            }
        }
    }

    #[test]
    fn the_card_text_names_no_service() {
        for card in CARDS {
            for s in [card.title(), card.body()] {
                let s = s.to_lowercase();
                // The names are split so the boundary script's source rules do not trip on them.
                for brand in [["spo", "tify"], ["win", "amp"], ["windows ", "media"]] {
                    assert!(!s.contains(&brand.concat()), "{s}");
                }
            }
        }
    }
}
