//! The like book (MD69, MD70, MD77): what the view knows of which tracks are saved, the heart
//! that shows and flips it, and the one hint a read-only login gets.
//!
//! The core answers `RequestLiked` with a `Liked` notice and a failed `SetLiked` with
//! `LikeFailed`. The view flips a heart at once and the book puts it back when the write fails.

use std::collections::{HashMap, HashSet};

use lune_ui::color::Rgba;
use lune_ui::input::CursorIcon;
use lune_ui::paint::Rect;
use lune_ui::ui::{Sense, Ui};
use whipspot_core::player::{PlayerAction, PlayerHandle, PlayerNotice};

use crate::icons::{Icon, heart_filled, icon};
use crate::theme::Look;

/// The text of the dimmed heart's tooltip and of the toast a click on it gives (MD70).
pub const LIKE_HINT: &str = "Log in to the library again to like songs";

/// Most uris asked in one `RequestLiked`.
const MAX_PER_ASK: usize = 50;
/// The book is cleared when it grows past this many uris.
const MAX_BOOK: usize = 20_000;
/// The line weight of the outline heart.
const HAIR: f32 = 1.5;
/// How far a heart's press area reaches past its glyph.
const HIT_GROW: f32 = 7.0;

/// Where the book sends its actions: the player.
pub trait Dispatch {
    fn dispatch(&self, action: PlayerAction);
}

impl Dispatch for PlayerHandle {
    fn dispatch(&self, action: PlayerAction) {
        PlayerHandle::dispatch(self, action);
    }
}

/// Which tracks are saved, as far as this session knows, and the hint a read-only click asks for.
#[derive(Debug, Default)]
pub struct LikeBook {
    state: HashMap<String, bool>,
    /// Uris a `RequestLiked` was sent for this session.
    asked: HashSet<String>,
    hint: bool,
}

impl LikeBook {
    /// Whether `uri` is saved; `None` when unknown.
    pub fn state(&self, uri: &str) -> Option<bool> {
        self.state.get(uri).copied()
    }

    /// Records that `uri` is `liked`.
    pub fn note(&mut self, uri: &str, liked: bool) {
        self.clear_if_full();
        self.state.insert(uri.to_owned(), liked);
    }

    /// Asks the player for the saved state of the uris not known and not asked before this
    /// session (at most 50 per action).
    pub fn want<'a>(&mut self, uris: impl IntoIterator<Item = &'a str>, player: &dyn Dispatch) {
        self.clear_if_full();
        let mut ask = Vec::new();
        for uri in uris {
            if ask.len() >= MAX_PER_ASK {
                break;
            }
            if self.state.contains_key(uri)
                || self.asked.contains(uri)
                || ask.iter().any(|u| u == uri)
            {
                continue;
            }
            ask.push(uri.to_owned());
        }
        if ask.is_empty() {
            return;
        }
        self.asked.extend(ask.iter().cloned());
        player.dispatch(PlayerAction::RequestLiked(ask));
    }

    /// Flips `uri` at once (unknown becomes liked) and asks the player to save or remove it.
    pub fn toggle(&mut self, uri: &str, player: &dyn Dispatch) {
        let liked = !self.state(uri).unwrap_or(false);
        self.note(uri, liked);
        player.dispatch(PlayerAction::SetLiked {
            uri: uri.to_owned(),
            liked,
        });
    }

    /// A click on the heart of `uri`: a flip when likes can change, else the hint.
    pub fn click(&mut self, uri: &str, writable: bool, player: &dyn Dispatch) {
        if writable {
            self.toggle(uri, player);
        } else {
            self.ask_hint();
        }
    }

    /// Takes what this book consumes from a notice: the saved states a `Liked` brings, and the
    /// state a failed write puts back. Whether the notice was the book's.
    pub fn on_notice(&mut self, notice: &PlayerNotice) -> bool {
        match notice {
            PlayerNotice::Liked(pairs) => {
                for (uri, liked) in pairs {
                    self.note(uri, *liked);
                }
                true
            }
            PlayerNotice::LikeFailed { uri, liked } => {
                self.note(uri, !*liked);
                true
            }
            _ => false,
        }
    }

    /// A read-only heart was clicked: the shell shows [`LIKE_HINT`] and opens the Account route.
    pub fn ask_hint(&mut self) {
        self.hint = true;
    }

    /// Whether a hint was asked since the last call.
    pub fn take_hint(&mut self) -> bool {
        std::mem::take(&mut self.hint)
    }

    fn clear_if_full(&mut self) {
        if self.state.len() > MAX_BOOK || self.asked.len() > MAX_BOOK {
            self.state.clear();
            self.asked.clear();
        }
    }
}

/// `c` at `k` of its alpha.
fn faded(c: Rgba, k: f32) -> Rgba {
    [
        c[0],
        c[1],
        c[2],
        (f32::from(c[3]) * k).round().clamp(0.0, 255.0) as u8,
    ]
}

/// A heart in `rect`: filled in the accent when `liked`, the outline when not liked and `outline`
/// asks for it or the pointer is over it, nothing otherwise. Not `writable` it is dimmed to half
/// and its tooltip is [`LIKE_HINT`]. Returns whether it was clicked (the caller gives that to
/// [`LikeBook::click`], which flips it or asks for the hint).
pub(crate) fn heart(
    ui: &mut Ui<'_>,
    key: &str,
    rect: Rect,
    look: &Look,
    liked: Option<bool>,
    writable: bool,
    outline: bool,
) -> bool {
    let id = ui.id(key);
    // The glyph is small: the press area is the glyph grown by `HIT_GROW` on each side.
    let hit = Rect::new(
        rect.x - HIT_GROW,
        rect.y - HIT_GROW,
        rect.w + 2.0 * HIT_GROW,
        rect.h + 2.0 * HIT_GROW,
    );
    let response = ui.interact(id, hit, Sense::CLICK);
    if response.hovered {
        ui.set_cursor(CursorIcon::Pointer);
    }
    let k = if writable { 1.0 } else { 0.5 };
    let c = (rect.x + rect.w / 2.0, rect.y + rect.h / 2.0);
    let size = rect.w.min(rect.h);
    let p = ui.painter();
    if liked == Some(true) {
        heart_filled(p, c, size, faded(look.a, k));
    } else if outline || response.hovered {
        let tint = if response.hovered {
            look.ink.text
        } else {
            look.ink.text2
        };
        icon(p, Icon::Heart, c, size, faded(tint, k), HAIR);
    }
    if !writable {
        ui.tooltip(&response, LIKE_HINT);
    }
    response.clicked
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    #[derive(Default)]
    struct Sink(RefCell<Vec<PlayerAction>>);

    impl Dispatch for Sink {
        fn dispatch(&self, action: PlayerAction) {
            self.0.borrow_mut().push(action);
        }
    }

    fn asked(sink: &Sink) -> Vec<Vec<String>> {
        sink.0
            .borrow()
            .iter()
            .filter_map(|a| match a {
                PlayerAction::RequestLiked(u) => Some(u.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn u3_want_asks_only_unknown_uris_once() {
        let (mut book, sink) = (LikeBook::default(), Sink::default());
        book.note("known", true);
        book.want(["known", "a", "b", "a"], &sink);
        assert_eq!(asked(&sink), vec![vec!["a".to_owned(), "b".to_owned()]]);
        // Asked once this session: not again, whatever the answer.
        book.want(["a", "b", "known"], &sink);
        assert_eq!(asked(&sink).len(), 1);
        book.want(["c"], &sink);
        assert_eq!(asked(&sink).len(), 2);
    }

    #[test]
    fn u3_want_asks_at_most_50_per_action() {
        let (mut book, sink) = (LikeBook::default(), Sink::default());
        let uris: Vec<String> = (0..120).map(|i| format!("u{i}")).collect();
        book.want(uris.iter().map(String::as_str), &sink);
        let sent = asked(&sink);
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].len(), 50);
        // The rest goes with the next call.
        book.want(uris.iter().map(String::as_str), &sink);
        let sent = asked(&sink);
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[1].first().map(String::as_str), Some("u50"));
        assert_eq!(sent[1].len(), 50);
    }

    #[test]
    fn u3_toggle_flips_and_dispatches() {
        let (mut book, sink) = (LikeBook::default(), Sink::default());
        book.toggle("t", &sink);
        assert_eq!(book.state("t"), Some(true));
        book.toggle("t", &sink);
        assert_eq!(book.state("t"), Some(false));
        let sent = sink.0.borrow();
        assert!(matches!(
            &sent[0],
            PlayerAction::SetLiked { uri, liked: true } if uri == "t"
        ));
        assert!(matches!(
            &sent[1],
            PlayerAction::SetLiked { uri, liked: false } if uri == "t"
        ));
    }

    #[test]
    fn u3_like_failed_reverts_and_liked_notes() {
        let (mut book, sink) = (LikeBook::default(), Sink::default());
        book.toggle("t", &sink);
        assert!(book.on_notice(&PlayerNotice::LikeFailed {
            uri: "t".into(),
            liked: true
        }));
        assert_eq!(book.state("t"), Some(false));
        assert!(book.on_notice(&PlayerNotice::Liked(vec![
            ("a".into(), true),
            ("b".into(), false)
        ])));
        assert_eq!(
            (book.state("a"), book.state("b")),
            (Some(true), Some(false))
        );
        assert!(!book.on_notice(&PlayerNotice::Error("x")));
    }

    #[test]
    fn u3_a_read_only_click_sets_the_hint_and_sends_nothing() {
        let (mut book, sink) = (LikeBook::default(), Sink::default());
        book.click("t", false, &sink);
        assert!(sink.0.borrow().is_empty());
        assert_eq!(book.state("t"), None);
        assert!(book.take_hint());
        assert!(!book.take_hint());
        book.click("t", true, &sink);
        assert_eq!(sink.0.borrow().len(), 1);
        assert!(!book.take_hint());
    }

    #[test]
    fn u3_the_book_is_cleared_when_it_grows_past_the_cap() {
        let mut book = LikeBook::default();
        for i in 0..=MAX_BOOK {
            book.state.insert(format!("u{i}"), true);
        }
        book.note("fresh", true);
        assert_eq!(book.state.len(), 1);
        assert_eq!(book.state("fresh"), Some(true));
    }
}
