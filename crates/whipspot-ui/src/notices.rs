//! Notices as toasts: the player's error texts, the login result and the host's own fixed texts.
//! The library answers never reach here: the browse sheet takes them first.

use lune_ui::ui::Ui;
use lune_ui::widgets::toast::{ToastKind, ToastSpec, Toasts};
use whipspot_core::player::PlayerNotice;

/// The toast after a login that worked.
pub const LOGGED_IN: &str = "Logged in";
/// The toast after a login that did not.
pub const LOGIN_FAILED: &str = "Login failed";
/// Toasts waiting for a frame; older ones are dropped.
const MAX_QUEUED: usize = 16;

/// The toast stack and the toasts waiting for the next frame (a toast needs the frame's time).
#[derive(Debug, Default)]
pub struct Notices {
    toasts: Toasts,
    queued: Vec<(ToastKind, &'static str)>,
}

impl Notices {
    /// Queues a toast of fixed `text`.
    pub fn push(&mut self, kind: ToastKind, text: &'static str) {
        if self.queued.len() >= MAX_QUEUED {
            self.queued.remove(0);
        }
        self.queued.push((kind, text));
    }

    /// Routes one player notice: an error or a login result becomes a toast. A library page or a
    /// prefill is the browse sheet's, not a toast's.
    pub fn route(&mut self, notice: PlayerNotice) {
        match notice {
            PlayerNotice::Error(text) => self.push(ToastKind::Error, text),
            PlayerNotice::LoginResult { ok: true, .. } => self.push(ToastKind::Success, LOGGED_IN),
            PlayerNotice::LoginResult { ok: false, .. } => {
                self.push(ToastKind::Error, LOGIN_FAILED);
            }
            PlayerNotice::Prefill { .. }
            | PlayerNotice::LibraryPage { .. }
            | PlayerNotice::Liked(_)
            | PlayerNotice::LikeFailed { .. }
            | PlayerNotice::LikesChanged { .. }
            | PlayerNotice::LibraryChanged { .. } => {}
        }
    }

    /// Shows the queued toasts and draws the stack.
    pub fn draw(&mut self, ui: &mut Ui<'_>) {
        let now = ui.time();
        for (kind, text) in self.queued.drain(..) {
            self.toasts.push(ToastSpec::new(kind, text), now);
        }
        // The toasts act on their own model (dismiss, expand); no answer is needed.
        let _ = ui.toasts("toasts", &mut self.toasts);
    }
}
