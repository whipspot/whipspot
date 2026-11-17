//! The Account view: for each provider with login steps, its steps in order with their state,
//! hint, a Copy button for a public value, one text field per input and the Log in and Log out
//! buttons. Everything goes through the player's `Login`, `Logout` and `Prefill` actions.
//!
//! Field contents live here and nowhere else: never logged, never stored beyond building the
//! action, cleared after the step's login worked.

use std::collections::BTreeMap;

use lune_ui::color::{Rgba, mix};
use lune_ui::paint::{Painter, Rect};
use lune_ui::scene::LayerId;
use lune_ui::text::{Spec, Style};
use lune_ui::ui::{Sense, Ui};
use lune_ui::widgets::button::ButtonKind;
use lune_ui::widgets::field::{FieldOptions, Variant};
use lune_ui::widgets::scroll_area::Extent;
use whipspot_core::player::{PlayerAction, PlayerHandle, PlayerState, ProviderEntry, RequestId};
use whipspot_core::provider::model::LoginState;
use whipspot_core::provider::{AuthStepInfo, AuthStepStatus, LoginInput};

use crate::draw::{P, disc, led};
use crate::grid;
use crate::history::Route;
use crate::layout::heading_slot;
use crate::library::LibraryModel;
use crate::settings::{BODY_Y, CARD_GAP, PAD_BOTTOM, PAD_X, card, column, scroll_options};
use crate::theme::{Labels, Look};
use crate::tracks::note;

/// The gap above and below the hairline between two steps of a card.
const BLOCK_GAP: f32 = 12.0;
/// A field's height and the buttons'.
const FIELD_H: f32 = 32.0;
const BUTTON_H: f32 = 32.0;
const BUTTON_W: f32 = 104.0;
/// The extra height of a step's status detail line, under the title row.
const DETAIL_H: f32 = 18.0;

/// The text of the logged-out library views.
const LOGIN_PROMPT: &str = "Log in to see your library.";

/// Draws the logged-out prompt centred in `rect`; whether its button to the Account view was
/// pressed. The button is filled in the look's accent.
pub fn login_prompt(ui: &mut Ui<'_>, rect: Rect, look: &Look) -> bool {
    let theme = *ui.theme();
    let spec = Spec::plain(LOGIN_PROMPT, Style::ui(theme.font_m, theme.fg_secondary));
    let (w, h) = ui.painter().measure(&spec);
    let y = rect.y + (rect.h - h - 14.0 - BUTTON_H) / 2.0;
    ui.painter().text(&spec, rect.x + (rect.w - w) / 2.0, y);
    let button = Rect::new(
        rect.x + (rect.w - 140.0) / 2.0,
        y + h + 14.0,
        140.0,
        BUTTON_H,
    );
    let resp = ui.interact(ui.id("to-account"), button, Sense::CLICK_FOCUS);
    let fill = if resp.hovered {
        mix(look.a, look.ink.light, 0.18)
    } else {
        look.a
    };
    let label = Spec::plain(
        "Go to Account",
        Style::ui(theme.font_base, look.on).medium(),
    );
    let p = ui.painter();
    p.fill(button, look.radii().thumb, fill);
    p.text_centered(&label, button);
    if resp.focus_visible {
        p.ring(button, look.radii().thumb, 1.5, look.hi, true);
    }
    resp.clicked
}

/// The state texts (never the error's own message).
const STATE_IN: &str = "Logged in";
const STATE_OUT: &str = "Logged out";
const STATE_ERROR: &str = "Login error";
const STATE_UNKNOWN: &str = "Checking\u{2026}";

/// A status dot of diameter 7 at `c`: the LED of a look with tracked labels (unlit when `lit`
/// is false), else a plain disc.
fn status_dot(p: &mut P<'_>, k: &Look, c: (f32, f32), color: Rgba, lit: bool) {
    if k.decor().labels == Labels::Tracked {
        led(p, c, 7.0, color, k.hi, lit);
    } else {
        disc(p, c, 7.0, color);
    }
}

/// The hint's text spec at width `w`.
fn hint_spec(text: &str, theme: &lune_ui::theme::Theme, w: f32) -> Spec {
    Spec::plain(text, Style::ui(theme.font_base, theme.fg_secondary)).wrapped(w)
}

/// A step's status as the provider last reported it.
fn step_status<'a>(entry: &'a ProviderEntry, step: &AuthStepInfo) -> Option<&'a AuthStepStatus> {
    entry
        .auth
        .as_ref()
        .and_then(|a| a.steps.iter().find(|s| s.id == step.id))
}

/// The status detail line: one line, cut with an ellipsis to `w`.
fn detail_spec<H: Copy + PartialEq>(p: &mut Painter<'_, H>, text: &str, k: &Look, w: f32) -> Spec {
    p.elide(&Spec::plain(text, Style::ui(12.0, k.ink.faint)), w)
}

/// The height of a step's block at inner width `w`: the title row, the detail line, the hint
/// and its Copy row, one label and field per input, and the buttons. It mirrors
/// [`AccountView::step`].
fn step_height(
    ui: &mut Ui<'_>,
    step: &AuthStepInfo,
    status: Option<&AuthStepStatus>,
    w: f32,
) -> f32 {
    let theme = *ui.theme();
    let mut h = 30.0;
    if status.is_some_and(|s| s.detail.is_some()) {
        h += DETAIL_H;
    }
    if let Some(hint) = &step.hint {
        h += ui.painter().measure(&hint_spec(hint.text, &theme, w)).1 + 8.0;
        if hint.copy.is_some() {
            h += BUTTON_H + 8.0;
        }
    }
    h += step.inputs.len() as f32 * (20.0 + FIELD_H + 12.0);
    h + BUTTON_H
}

/// One field's place: provider, step, input.
type FieldKey = (String, String, String);

/// The account view's retained state.
#[derive(Debug, Default)]
pub struct AccountView {
    /// The view was on show last frame: a prefill goes out once per visit.
    visiting: bool,
    fields: BTreeMap<FieldKey, String>,
    /// Prefill requests in flight and the field each fills.
    prefills: Vec<(RequestId, FieldKey)>,
    /// Logins in flight: provider and step.
    pending: Vec<(String, String)>,
    /// A field had keyboard focus in the last frame drawn.
    focused: bool,
}

impl AccountView {
    /// The view is not on show.
    pub fn leave(&mut self) {
        self.visiting = false;
        self.focused = false;
        self.prefills.clear();
    }

    /// Whether one of the view's text fields had keyboard focus in the last frame.
    pub fn focused(&self) -> bool {
        self.focused
    }

    /// A login ended: the step is no longer pending, and a login that worked clears its fields.
    pub fn login_result(&mut self, provider: &str, step: &str, ok: bool) {
        self.pending.retain(|(p, s)| !(p == provider && s == step));
        if ok {
            for (key, value) in &mut self.fields {
                if key.0 == provider && key.1 == step {
                    value.clear();
                }
            }
        }
    }

    /// Draws the view in `rect` (its glass is the caller's): the heading, then one card per
    /// provider with its steps.
    pub fn frame(
        &mut self,
        ui: &mut Ui<'_>,
        rect: Rect,
        k: &Look,
        state: &PlayerState,
        library: &mut LibraryModel,
        player: &PlayerHandle,
    ) {
        let providers: Vec<&ProviderEntry> = state
            .providers
            .iter()
            .filter(|p| !p.info.auth_steps.is_empty())
            .collect();
        if !self.visiting {
            self.visiting = true;
            self.prefill(&providers, library, player);
        }
        self.take_prefills(library);
        self.focused = false;

        if providers.is_empty() {
            note(ui, rect, "No accounts to log in to.");
            return;
        }
        let (x, w, top) = column(rect);
        ui.scroll_area("accounts-page", rect, &scroll_options(21), |ui, _| {
            ui.painter().on(LayerId::Popup);
            grid::header(
                ui.painter(),
                k,
                heading_slot(rect),
                Route::Account.title(),
                None,
            );
            let inner_w = (w - 2.0 * PAD_X).max(0.0);
            let mut y = top;
            for entry in providers {
                let steps = &entry.info.auth_steps;
                let blocks: f32 = steps
                    .iter()
                    .map(|s| step_height(ui, s, step_status(entry, s), inner_w))
                    .sum();
                let gaps = steps.len().saturating_sub(1) as f32 * (2.0 * BLOCK_GAP + 1.0);
                let r = Rect::new(x, y, w, BODY_Y + blocks + gaps + PAD_BOTTOM);
                card(ui.painter(), k, r, entry.info.name);
                let mut by = r.y + BODY_Y;
                for (i, step) in steps.iter().enumerate() {
                    if i > 0 {
                        by += BLOCK_GAP;
                        ui.painter().fill(
                            Rect::new(r.x + 1.0, by, r.w - 2.0, 1.0),
                            0.0,
                            k.ink.line_soft,
                        );
                        by += 1.0 + BLOCK_GAP;
                    }
                    by = self.step(ui, k, entry, step, (x + PAD_X, by, inner_w), player);
                }
                y = r.bottom() + CARD_GAP;
            }
            (
                (),
                Extent::new(f64::from(rect.w), f64::from(y + 36.0 - rect.y)),
            )
        });
    }

    /// Asks for the stored value of every input, once per visit.
    fn prefill(
        &mut self,
        providers: &[&ProviderEntry],
        library: &mut LibraryModel,
        player: &PlayerHandle,
    ) {
        for entry in providers {
            for step in &entry.info.auth_steps {
                for input in &step.inputs {
                    let request = library.next_request();
                    self.prefills.push((
                        request,
                        (
                            entry.info.id.to_owned(),
                            step.id.to_owned(),
                            input.name.to_owned(),
                        ),
                    ));
                    player.dispatch(PlayerAction::Prefill {
                        request,
                        provider: entry.info.id.to_owned(),
                        step: step.id.to_owned(),
                        input: input.name.to_owned(),
                    });
                }
            }
        }
    }

    /// Fills the fields the answered prefills name (a field the user already typed in stays).
    fn take_prefills(&mut self, library: &mut LibraryModel) {
        let mut waiting = Vec::new();
        for (request, key) in std::mem::take(&mut self.prefills) {
            match library.take_prefill(request) {
                Some(Some(value)) => {
                    let field = self.fields.entry(key).or_default();
                    if field.is_empty() {
                        *field = value;
                    }
                }
                Some(None) => {}
                None => waiting.push((request, key)),
            }
        }
        self.prefills = waiting;
    }

    /// Draws one step at `(x, y, w)`; returns the y under it.
    fn step(
        &mut self,
        ui: &mut Ui<'_>,
        k: &Look,
        entry: &ProviderEntry,
        step: &AuthStepInfo,
        (x, mut y, w): (f32, f32, f32),
        player: &PlayerHandle,
    ) -> f32 {
        let theme = *ui.theme();
        let status = step_status(entry, step);
        let (state_text, state_color, lit, can_logout) =
            match (&entry.auth, status.map(|s| &s.state)) {
                (None, _) => (STATE_UNKNOWN, k.ink.faint, false, false),
                (_, Some(LoginState::LoggedIn)) => (STATE_IN, k.a, true, true),
                (_, Some(LoginState::Error { .. })) => (STATE_ERROR, theme.err, true, true),
                _ => (STATE_OUT, k.ink.faint, false, false),
            };
        let action = status.and_then(|s| s.action.as_deref());
        let detail = status.and_then(|s| s.detail.as_deref());

        // The step's title, and at the right edge its status: a dot and the state text.
        {
            let p = ui.painter();
            let label = Spec::plain(step.label, Style::ui(13.5, k.ink.text).medium());
            p.text_in(&label, Rect::new(x, y, w, 24.0), 0.0);
            let state = Spec::plain(state_text, Style::ui(12.5, k.ink.text2));
            let (sw, _) = p.measure(&state);
            p.text_in(&state, Rect::new(x + w - sw, y, sw, 24.0), 0.0);
            status_dot(p, k, (x + w - sw - 12.0, y + 12.0), state_color, lit);
        }
        y += 30.0;
        // The provider's status line, right-aligned under the state text.
        if let Some(detail) = detail {
            let p = ui.painter();
            let spec = detail_spec(p, detail, k, w);
            let (dw, _) = p.measure(&spec);
            p.text_in(&spec, Rect::new(x + w - dw, y - 6.0, dw, DETAIL_H), 0.0);
            y += DETAIL_H;
        }

        if let Some(hint) = &step.hint {
            let spec = hint_spec(hint.text, &theme, w);
            let p = ui.painter();
            let (_, h) = p.measure(&spec);
            p.text(&spec, x, y);
            y += h + 8.0;
            if let Some(value) = hint.copy {
                let box_w = (w - BUTTON_W - 8.0).max(0.0);
                let box_rect = Rect::new(x, y, box_w, BUTTON_H);
                let p = ui.painter();
                p.fill(box_rect, theme.item_radius, theme.surface_input);
                let mono = Spec::plain(value, Style::mono(theme.font_base, 20.0, theme.fg_primary));
                let mono = p.elide(&mono, (box_w - 16.0).max(0.0));
                p.text_in(&mono, box_rect, 8.0);
                let copy = Rect::new(x + w - BUTTON_W, y, BUTTON_W, BUTTON_H);
                if ui
                    .button(
                        ("copy", entry.info.id, step.id),
                        copy,
                        "Copy",
                        ButtonKind::Primary,
                    )
                    .clicked
                {
                    ui.clipboard_write(value.to_owned());
                }
                y += BUTTON_H + 8.0;
            }
        }

        // One field per input.
        let mut empty = false;
        let mut submitted = false;
        for input in &step.inputs {
            let key = (
                entry.info.id.to_owned(),
                step.id.to_owned(),
                input.name.to_owned(),
            );
            let text = self.fields.entry(key).or_default();
            let label = Spec::plain(input.label, Style::ui(theme.font_s, theme.fg_secondary));
            ui.painter().text(&label, x, y);
            y += 20.0;
            let opts = FieldOptions {
                placeholder: input.format,
                variant: Variant::Mono,
                max_chars: Some(input.max_len as usize),
                label: Some(input.label),
                ..FieldOptions::default()
            };
            let field = ui.text_field_with(
                ("field", entry.info.id, step.id, input.name),
                Rect::new(x, y, w, FIELD_H),
                text,
                &opts,
            );
            submitted |= field.submitted;
            self.focused |= field.response.focused;
            empty |= text.trim().is_empty();
            y += FIELD_H + 12.0;
        }

        // Log in and Log out.
        let pending = self
            .pending
            .iter()
            .any(|(p, s)| p == entry.info.id && s == step.id);
        let can_login = !pending && !empty;
        let action_pending = action.map(|a| format!("{a}\u{2026}"));
        let login_label = match (pending, action, &action_pending) {
            (true, _, Some(label)) => label.as_str(),
            (true, _, None) => "Logging in\u{2026}",
            (false, Some(label), _) => label,
            (false, None, _) => "Log in",
        };
        let login = ui.with_enabled(can_login, |ui| {
            ui.button(
                ("login", entry.info.id, step.id),
                Rect::new(x, y, BUTTON_W, BUTTON_H),
                login_label,
                ButtonKind::Primary,
            )
            .clicked
        });
        let logout = ui.with_enabled(can_logout, |ui| {
            ui.button(
                ("logout", entry.info.id, step.id),
                Rect::new(x + BUTTON_W + 8.0, y, BUTTON_W, BUTTON_H),
                "Log out",
                ButtonKind::Secondary,
            )
            .clicked
        });
        if can_login && (login || submitted) {
            self.login(entry, step, player);
        }
        if logout {
            player.dispatch(PlayerAction::Logout {
                provider: entry.info.id.to_owned(),
                step: step.id.to_owned(),
            });
        }
        y + BUTTON_H
    }

    /// Dispatches the step's login from the field contents; the input lives only in the action.
    fn login(&mut self, entry: &ProviderEntry, step: &AuthStepInfo, player: &PlayerHandle) {
        let mut input = LoginInput::default();
        for spec in &step.inputs {
            let key = (
                entry.info.id.to_owned(),
                step.id.to_owned(),
                spec.name.to_owned(),
            );
            let value = self.fields.get(&key).map(|v| v.trim().to_owned());
            input
                .0
                .insert(spec.name.to_owned(), value.unwrap_or_default());
        }
        self.pending
            .push((entry.info.id.to_owned(), step.id.to_owned()));
        player.dispatch(PlayerAction::Login {
            provider: entry.info.id.to_owned(),
            step: step.id.to_owned(),
            input,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{ThemeId, theme};
    use lune_ui::testing::FakeText;
    use lune_ui::theme::DARK;

    #[test]
    fn a_detail_longer_than_the_card_is_cut_to_one_line_with_an_ellipsis() {
        let k = Look::new(theme(ThemeId::C), None);
        let mut text = FakeText;
        let mut p: Painter<'_, ()> = Painter::new(
            1.0,
            Rect::new(0.0, 0.0, 4096.0, 4096.0),
            &mut text,
            None,
            DARK,
        );
        let long = "Helper 2026.10.26, runtime 0.18.0 ".repeat(20);
        let w = 240.0;
        let spec = detail_spec(&mut p, &long, &k, w);
        let (cut_w, _) = p.measure(&spec);
        assert!(cut_w <= w, "{cut_w}");
        let shown = lune_ui::text::joined_text(&spec);
        assert!(shown.ends_with('\u{2026}'), "{shown}");
        assert!(!shown.contains('\n'));
        assert_eq!(p.text_lines(&spec).len(), 1);
        // A short one is left alone.
        let short = detail_spec(&mut p, "Helper 1", &k, w);
        assert_eq!(lune_ui::text::joined_text(&short), "Helper 1");
    }
}
