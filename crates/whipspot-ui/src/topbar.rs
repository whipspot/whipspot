//! The browse sheet's top bar (MD54): history buttons, the five tabs, the search field and the
//! settings icon, in the tab, button and field style of the theme's decor.

use std::time::Duration;

use lune_ui::input::{CursorIcon, Primary};
use lune_ui::paint::Rect;
use lune_ui::scene::LayerId;
use lune_ui::ui::{Sense, Ui};
use lune_ui::widgets::field::{FieldChrome, FieldOptions, Variant};
use lune_ui::widgets::menu::{MenuItem, MenuOptions, MenuPlace, MenuRow};
use lune_ui::widgets::popover::{PlaceAlign, Side};

use crate::anim;
use crate::draw::{alpha, caps, caps_width, hgradient, led, outline, text, well, width};
use crate::history::Tab;
use crate::icons::{Icon, icon};
use crate::theme::{Labels, Look, Tab as TabStyle};
use whipspot_core::player::ProviderEntry;
use whipspot_core::provider::ProviderGlyph;

/// The hairline stroke of the line themes' icons.
const HAIR: f32 = 1.3;
/// The row's centre below the sheet's top, and the history buttons' diameter.
const ROW_Y: f32 = 42.0;
const BUTTON_D: f32 = 32.0;
/// The tabs start this far from the sheet's left edge.
const TABS_X: f32 = 132.0;
/// The tab text size.
const TAB_SIZE: f32 = 14.5;
/// The search field's widest and narrowest size and its height.
const FIELD_W: f32 = 320.0;
const FIELD_MIN_W: f32 = 116.0;
/// The key hint inside the field is drawn only when the field is at least this wide.
const KEYCAP_MIN_FIELD_W: f32 = 190.0;
const FIELD_H: f32 = 38.0;
/// The clear button at the field's right end while it has text: its side, its icon's size, its
/// distance from the field's right edge, and the gap between it and the text area.
const CLEAR_BOX: f32 = 24.0;
const CLEAR_ICON: f32 = 12.0;
const CLEAR_INSET: f32 = 8.0;
const CLEAR_GAP: f32 = 4.0;
/// The gap between the last tab and the field at the least.
const FIELD_GAP: f32 = 20.0;
/// The tab gap in the roomy case and the narrowest it shrinks to.
const GAP_PILL: f32 = 40.0;
const GAP_OTHER: f32 = 34.0;
const GAP_MIN: f32 = 20.0;
/// The settings icon's centre from the sheet's right edge, and the field's right edge from that
/// centre.
const GEAR_INSET: f32 = 42.0;
const FIELD_FROM_GEAR: f32 = 30.0;
/// The auto-hide icon's centre sits this far left of the settings icon's; the field ends the
/// same distance (`FIELD_FROM_GEAR`) left of it.
const HIDE_FROM_GEAR: f32 = 36.0;
/// The tab gap's floor when the bar is so narrow that the field would go under its minimum.
const GAP_TIGHT: f32 = 10.0;
/// The provider button's icon size, and the gap between the button and its menu.
const PROVIDER_ICON: f32 = 18.0;
const MENU_GAP: f32 = 6.0;
/// The half side of the icon buttons' hit squares.
const HIT_HALF: f32 = 16.0;
/// The provider menu's hint on the provider on show.
const ACTIVE_HINT: &str = "Active";
/// The placeholder, as the field shows it while empty.
pub const PLACEHOLDER: &str = "Search artists, albums, songs";

/// The auto-hide icon's tooltip: what the switch does now and what a press does.
pub(crate) fn auto_hide_tip(on: bool) -> &'static str {
    if on {
        "Auto-hide is on: the library hides when the mouse rests. Click to keep it."
    } else {
        "Auto-hide is off: the library stays. Click to hide it when the mouse rests."
    }
}

/// What the top bar's frame asks of the sheet.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct TopOut {
    pub back: bool,
    pub forward: bool,
    /// A tab was pressed.
    pub tab: Option<Tab>,
    /// The settings icon was pressed.
    pub settings: bool,
    /// The auto-hide icon was pressed.
    pub auto_hide: bool,
    /// Enter was pressed in the search field with a query typed.
    pub submit: Option<String>,
    /// A provider was picked in the provider menu: its id.
    pub provider: Option<&'static str>,
    /// The settings icon's hit rect.
    pub gear_rect: Rect,
    /// The auto-hide icon's hit rect.
    pub hide_rect: Rect,
    /// The provider button's hit rect; `None` when the button is not drawn.
    pub provider_rect: Option<Rect>,
}

/// The hit rect of an icon button centred at `c`.
fn hit_rect(c: (f32, f32)) -> Rect {
    Rect::new(
        c.0 - HIT_HALF,
        c.1 - HIT_HALF,
        2.0 * HIT_HALF,
        2.0 * HIT_HALF,
    )
}

/// The clear button's box in the search field `field`: right-aligned, centred vertically.
fn clear_box(field: Rect) -> Rect {
    Rect::new(
        field.right() - CLEAR_INSET - CLEAR_BOX,
        field.y + (field.h - CLEAR_BOX) / 2.0,
        CLEAR_BOX,
        CLEAR_BOX,
    )
}

/// The providers the library menu offers: shown only with two or more that have a library.
#[derive(Debug, Clone, Copy)]
pub struct Switch<'a> {
    pub providers: &'a [ProviderEntry],
    /// The provider whose library shows now.
    pub current: &'a str,
}

impl<'a> Switch<'a> {
    /// The switch for `providers`, when two or more have a library.
    pub fn of(providers: &'a [ProviderEntry], current: &'a str) -> Option<Switch<'a>> {
        (providers.iter().filter(|p| p.info.caps.library).count() >= 2)
            .then_some(Switch { providers, current })
    }

    /// The (id, name, glyph) of each menu row, in registration order.
    fn options(&self) -> impl Iterator<Item = Segment> + '_ {
        self.providers
            .iter()
            .filter(|p| p.info.caps.library)
            .map(|p| (p.info.id, p.info.name, p.info.glyph))
    }
}

/// One provider of the menu: its id, its name and its glyph.
type Segment = (&'static str, &'static str, ProviderGlyph);

/// The Codicon (in the bundled Nerd Font) a menu row shows for a provider's glyph:
/// `cod-broadcast`, `cod-folder`, `cod-record`.
fn glyph_menu(glyph: ProviderGlyph) -> &'static str {
    match glyph {
        ProviderGlyph::Stream => "\u{eaad}",
        ProviderGlyph::Files => "\u{ea83}",
        ProviderGlyph::Other => "\u{eba7}",
    }
}

/// The icon a provider's glyph is drawn as.
fn glyph_icon(glyph: ProviderGlyph) -> Icon {
    match glyph {
        ProviderGlyph::Stream => Icon::Stream,
        ProviderGlyph::Files => Icon::Folder,
        ProviderGlyph::Other => Icon::Disc,
    }
}

/// What the bar needs of the history and the route on show.
#[derive(Debug, Clone, Copy)]
pub struct TopState<'a> {
    /// The tab the route on show belongs to.
    pub tab: Option<Tab>,
    pub can_back: bool,
    pub can_forward: bool,
    /// The user's auto-hide switch is on.
    pub auto_hide: bool,
    /// The provider switch, when there is one.
    pub switch: Option<Switch<'a>>,
    /// The demo holds the provider menu open.
    pub menu_forced: bool,
}

/// The positions of the bar's parts in one sheet, from the widths of the labels.
#[derive(Debug, Clone, PartialEq)]
pub struct Metrics {
    pub cy: f32,
    pub back: (f32, f32),
    pub forward: (f32, f32),
    /// Each tab's left edge and its gap to the next.
    pub tab_x: [f32; 5],
    pub tab_gap: f32,
    pub gear: (f32, f32),
    /// The auto-hide icon, left of the settings icon.
    pub hide: (f32, f32),
    pub field: Rect,
    /// The provider button's centre, between the auto-hide and the settings icon; `None` when
    /// there is no switch.
    pub provider: Option<(f32, f32)>,
}

impl Metrics {
    /// The settings icon's hit rect.
    pub fn gear_hit(&self) -> Rect {
        hit_rect(self.gear)
    }

    /// The auto-hide icon's hit rect.
    pub fn hide_hit(&self) -> Rect {
        hit_rect(self.hide)
    }

    /// The provider button's hit rect; `None` when the bar has no switch.
    pub fn provider_hit(&self) -> Option<Rect> {
        self.provider.map(hit_rect)
    }
}

/// Places the bar's parts for a sheet `s` given the tab widths `tw`: the tab gap shrinks first
/// (to [`GAP_MIN`]), then the field (to [`FIELD_MIN_W`]). With a provider button
/// (`with_provider`) the button takes the auto-hide icon's place left of the settings icon and
/// the auto-hide icon and the field move one step left; on a bar too narrow for the field's
/// minimum the tab gap gives way further, to [`GAP_TIGHT`].
pub fn metrics(s: Rect, tw: [f32; 5], roomy_gap: f32, with_provider: bool) -> Metrics {
    let cy = s.y + ROW_Y;
    let gear = (s.right() - GEAR_INSET, cy);
    let provider = with_provider.then_some((gear.0 - HIDE_FROM_GEAR, cy));
    let steps = if with_provider { 2.0 } else { 1.0 };
    let hide = (gear.0 - steps * HIDE_FROM_GEAR, cy);
    let right = hide.0 - FIELD_FROM_GEAR;
    let x0 = s.x + TABS_X;
    let tabs_w: f32 = tw.iter().sum();
    let room = right - x0 - tabs_w - FIELD_GAP;
    let floor = if room - 4.0 * GAP_MIN >= FIELD_MIN_W {
        GAP_MIN
    } else {
        ((room - FIELD_MIN_W) / 4.0).clamp(GAP_TIGHT, GAP_MIN)
    };
    let gap = roomy_gap
        .min(((room - FIELD_W) / 4.0).max(floor))
        .max(floor);
    let field_w = (room - 4.0 * gap).clamp(FIELD_MIN_W, FIELD_W);
    let mut tab_x = [0.0; 5];
    let mut x = x0;
    for (i, w) in tw.iter().enumerate() {
        tab_x[i] = x;
        x += w + gap;
    }
    Metrics {
        cy,
        back: (s.x + 40.0, cy),
        forward: (s.x + 80.0, cy),
        tab_x,
        tab_gap: gap,
        gear,
        hide,
        field: Rect::new(right - field_w, cy - FIELD_H / 2.0, field_w, FIELD_H),
        provider,
    }
}

/// The top bar's retained state: the search field's text and focus.
#[derive(Debug, Default)]
pub struct TopBar {
    text: String,
    /// The field takes focus at its next frame.
    want_focus: bool,
    /// The field had keyboard focus in the last frame.
    focused: bool,
    /// The provider menu is open.
    menu: bool,
}

impl TopBar {
    /// Asks for the search field to take keyboard focus the next time the bar is drawn.
    pub fn focus(&mut self) {
        self.want_focus = true;
    }

    /// Whether the provider menu is open (as of the last frame).
    pub fn menu_open(&self) -> bool {
        self.menu
    }

    /// Whether the search field had keyboard focus in the last frame.
    pub fn focused(&self) -> bool {
        self.focused
    }

    /// Puts `query` in the field.
    pub fn set_text(&mut self, query: &str) {
        self.text = query.to_owned();
    }

    /// Draws the bar for the sheet `s` and reports what was pressed.
    pub fn frame(&mut self, ui: &mut Ui<'_>, s: Rect, k: &Look, st: TopState<'_>) -> TopOut {
        let mut out = TopOut::default();
        let ink = k.ink;
        let style = k.decor().tab;
        let mac = ui.keymap().primary == Primary::Logo;
        let labels = Tab::ALL;

        // The tab widths: the active one in the medium weight.
        let mut tw = [0.0; 5];
        {
            let p = ui.painter();
            p.on(LayerId::Popup);
            for (i, t) in labels.iter().enumerate() {
                tw[i] = width(p, t.label(), TAB_SIZE, Some(*t) == st.tab);
            }
        }
        let roomy = if style == TabStyle::Pill {
            GAP_PILL
        } else {
            GAP_OTHER
        };
        let m = metrics(s, tw, roomy, st.switch.is_some());
        let cy = m.cy;

        // History buttons.
        for (c, can, back) in [
            (m.back, st.can_back, true),
            (m.forward, st.can_forward, false),
        ] {
            let r = Rect::new(
                c.0 - BUTTON_D / 2.0,
                c.1 - BUTTON_D / 2.0,
                BUTTON_D,
                BUTTON_D,
            );
            let id = ui.id(if back {
                "history-back"
            } else {
                "history-forward"
            });
            let resp = ui.interact(id, r, Sense::CLICK_FOCUS);
            let lit = can && resp.hovered;
            let p = ui.painter();
            match style {
                TabStyle::Pill => p.fill(
                    r,
                    BUTTON_D / 2.0,
                    alpha(
                        ink.light,
                        if can {
                            if lit { 0x24 } else { 0x18 }
                        } else {
                            0x0A
                        },
                    ),
                ),
                TabStyle::Led => well(p, r, BUTTON_D / 2.0, true),
                TabStyle::Underline => p.ring(
                    r,
                    BUTTON_D / 2.0,
                    1.0,
                    if can { ink.line } else { ink.line_soft },
                    false,
                ),
            }
            let w = if style == TabStyle::Underline {
                HAIR
            } else {
                2.0
            };
            icon(
                p,
                if back { Icon::ChevL } else { Icon::ChevR },
                c,
                14.0,
                if can { ink.text } else { ink.faint },
                w,
            );
            if can {
                if resp.hovered {
                    ui.set_cursor(CursorIcon::Pointer);
                }
                if resp.clicked {
                    if back {
                        out.back = true;
                    } else {
                        out.forward = true;
                    }
                }
            }
            ui.tooltip(&resp, if back { "Back" } else { "Forward" });
        }

        // Tabs. The active tab's pill or underline glides to the next one.
        let pad = (m.tab_gap / 2.0).min(16.0);
        if let Some(i) = labels.iter().position(|t| Some(*t) == st.tab) {
            let (x, w) = (m.tab_x[i], tw[i]);
            let x = anim::glide(ui, "tab-mark-x", x);
            let w = anim::glide(ui, "tab-mark-w", w);
            let p = ui.painter();
            match style {
                TabStyle::Pill => p.fill(
                    Rect::new(x - pad, cy - 17.0, w + 2.0 * pad, 34.0),
                    17.0,
                    alpha(ink.light, 0x1A),
                ),
                TabStyle::Underline => hgradient(p, Rect::new(x, cy + 14.0, w, 2.0), k.a, k.b),
                TabStyle::Led => {}
            }
        }
        for (i, t) in labels.iter().enumerate() {
            let active = Some(*t) == st.tab;
            let x = m.tab_x[i];
            let hit = Rect::new(x - pad, cy - 17.0, tw[i] + 2.0 * pad, 34.0);
            let resp = ui.interact(ui.id(("tab", t.key())), hit, Sense::CLICK_FOCUS);
            let p = ui.painter();
            let hovered = resp.hovered && !active;
            let col = if active {
                ink.text
            } else if hovered {
                ink.text2
            } else {
                ink.muted
            };
            if style == TabStyle::Led {
                led(p, (x + tw[i] / 2.0, cy - 19.0), 4.0, k.a, k.hi, active);
            }
            text(p, t.label(), TAB_SIZE, col, active, x, cy - 10.0, 0.0);
            if resp.hovered {
                ui.set_cursor(CursorIcon::Pointer);
            }
            if resp.clicked {
                out.tab = Some(*t);
            }
        }

        // Settings.
        let gear_hit = m.gear_hit();
        out.gear_rect = gear_hit;
        let gear = ui.interact(ui.id("settings"), gear_hit, Sense::CLICK_FOCUS);
        let gcol = if gear.hovered { ink.text } else { ink.text2 };
        icon(ui.painter(), Icon::Gear, m.gear, 19.0, gcol, HAIR);
        if gear.hovered {
            ui.set_cursor(CursorIcon::Pointer);
        }
        out.settings = gear.clicked;
        ui.tooltip(&gear, "Settings");

        // Auto-hide: the same ink as the gear; the open or struck eye says the state.
        let hide_hit = m.hide_hit();
        out.hide_rect = hide_hit;
        let hide = ui.interact(ui.id("auto-hide"), hide_hit, Sense::CLICK_FOCUS);
        let hcol = if hide.hovered { ink.text } else { ink.text2 };
        let hicon = if st.auto_hide {
            Icon::Eye
        } else {
            Icon::EyeOff
        };
        icon(ui.painter(), hicon, m.hide, 19.0, hcol, HAIR);
        if hide.hovered {
            ui.set_cursor(CursorIcon::Pointer);
        }
        out.auto_hide = hide.clicked;
        ui.tooltip(&hide, auto_hide_tip(st.auto_hide));

        // The provider button between the auto-hide and settings icons, and its menu.
        if let (Some(sw), Some(c)) = (st.switch, m.provider) {
            out.provider_rect = m.provider_hit();
            out.provider = self.provider(ui, k, c, sw, st.menu_forced);
        }

        // The search field: the box in the style of the theme, the toolkit's field inside it.
        let f = m.field;
        // While the field has text its right end holds the clear button instead of the key hint.
        let clear = (!self.text.is_empty()).then(|| clear_box(f));
        let keycap =
            (k.decor().labels == Labels::Tracked && f.w >= KEYCAP_MIN_FIELD_W && clear.is_none())
                .then(|| {
                    let label = if mac { "CMD K" } else { "CTRL K" };
                    let p = ui.painter();
                    let kw = caps_width(p, label, 8.0, 1.0);
                    (
                        label,
                        Rect::new(f.right() - 14.0 - kw - 12.0, cy - 9.0, kw + 12.0, 18.0),
                    )
                });
        {
            let p = ui.painter();
            match style {
                TabStyle::Pill => outline(p, f, 19.0, [0x00, 0x00, 0x00, 0x30], ink.line_soft),
                TabStyle::Led => well(p, f, 19.0, true),
                TabStyle::Underline => outline(p, f, 19.0, [0xFF, 0xFF, 0xFF, 0x08], ink.line),
            }
            icon(p, Icon::Search, (f.x + 22.0, cy), 14.0, ink.muted, HAIR);
        }
        let mut clear_hover = false;
        if let Some(r) = clear {
            // Before the field: a click empties it, and the field takes the focus in this frame.
            let resp = ui.interact(ui.id("topbar-clear"), r, Sense::CLICK);
            clear_hover = resp.hovered;
            if resp.hovered {
                ui.set_cursor(CursorIcon::Pointer);
            }
            if resp.clicked {
                self.text.clear();
                self.want_focus = true;
            }
            ui.tooltip(&resp, "Clear");
        }
        out.submit = self.field(ui, f, keycap.map(|(_, r)| r), clear);
        if let Some(r) = clear.filter(|_| !self.text.is_empty()) {
            let col = if clear_hover { ink.text } else { ink.muted };
            icon(
                ui.painter(),
                Icon::Close,
                (r.x + r.w / 2.0, cy),
                CLEAR_ICON,
                col,
                HAIR,
            );
        }
        if let Some((label, r)) = keycap {
            let p = ui.painter();
            p.ring(r, 5.0, 1.0, ink.line, false);
            caps(p, label, 8.0, ink.muted, r.x + 6.0, cy - 5.5, 1.0);
        }
        out
    }

    /// The provider button at `c`: the icon of the provider on show; a press opens a menu of
    /// every provider (its icon and name, the one on show marked). Returns the id picked when it
    /// is another provider.
    fn provider(
        &mut self,
        ui: &mut Ui<'_>,
        k: &Look,
        c: (f32, f32),
        sw: Switch<'_>,
        forced: bool,
    ) -> Option<&'static str> {
        let ink = k.ink;
        let options: Vec<Segment> = sw.options().collect();
        let current = options.iter().find(|o| o.0 == sw.current);
        let glyph = current.map_or(ProviderGlyph::Other, |o| o.2);
        let name = current.map_or("", |o| o.1);
        let hit = hit_rect(c);
        let id = ui.id("provider-menu-button");
        let resp = ui.interact(id, hit, Sense::CLICK_FOCUS);
        let open = self.menu || forced;
        let col = if open || resp.hovered {
            ink.text
        } else {
            ink.text2
        };
        icon(ui.painter(), glyph_icon(glyph), c, PROVIDER_ICON, col, HAIR);
        if resp.hovered {
            ui.set_cursor(CursorIcon::Pointer);
        }
        if !open {
            ui.tooltip(&resp, &format!("Library: {name}"));
        }
        // A press on the button while the menu is open is the menu's outside press: it closes.
        if resp.clicked {
            self.menu = true;
        }
        if !(self.menu || forced) {
            return None;
        }
        let rows: Vec<MenuRow<'_>> = options
            .iter()
            .map(|(pid, pname, g)| {
                MenuRow::Entry(MenuItem {
                    glyph: Some(glyph_menu(*g)),
                    text: pname,
                    shortcut: if *pid == sw.current { ACTIVE_HINT } else { "" },
                    ..MenuItem::default()
                })
            })
            .collect();
        let menu = MenuOptions {
            place: MenuPlace::Anchor {
                rect: hit,
                side: Side::Bottom,
                align: PlaceAlign::End,
                gap: MENU_GAP,
            },
            anchor_id: Some(id),
            name: "Library",
        };
        let r = ui.menu_with("provider-menu", &rows, &menu);
        if let Some(i) = r.picked {
            self.menu = false;
            return options.get(i).map(|o| o.0).filter(|pid| *pid != sw.current);
        }
        if r.dismissed {
            self.menu = false;
        }
        None
    }

    /// The toolkit's text field inside the box `f` (no chrome of its own); the query when Enter
    /// was pressed with one typed.
    fn field(
        &mut self,
        ui: &mut Ui<'_>,
        f: Rect,
        keycap: Option<Rect>,
        clear: Option<Rect>,
    ) -> Option<String> {
        if std::mem::take(&mut self.want_focus) {
            // The focus lands at the next frame's start, so one more frame is asked for.
            let id = ui.id("topbar-search");
            ui.request_focus(id);
            let at = ui.time() + Duration::from_millis(16);
            ui.request_repaint_at(at);
        }
        let right = match (clear, keycap) {
            (Some(c), _) => c.x - CLEAR_GAP,
            (None, Some(r)) => r.x - 8.0,
            (None, None) => f.right() - 14.0,
        };
        let area = Rect::new(f.x + 38.0, f.y, (right - f.x - 38.0).max(0.0), f.h);
        let opts = FieldOptions {
            placeholder: PLACEHOLDER,
            variant: Variant::Plain,
            chrome: FieldChrome::Bare,
            label: Some("Search"),
            ..FieldOptions::default()
        };
        let field = ui.text_field_with("topbar-search", area, &mut self.text, &opts);
        self.focused = field.response.focused;
        let query = self.text.trim();
        (field.submitted && !query.is_empty()).then(|| query.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::theme::{ThemeId, theme};
    use lune_ui::input::PointerButton;
    use lune_ui::testing::UiHarness;

    fn field_at(w: f32) -> Rect {
        Rect::new(400.0, 40.0, w, FIELD_H)
    }

    #[test]
    fn the_clear_box_is_24_px_at_the_fields_right_end_and_centred() {
        for w in [FIELD_W, FIELD_MIN_W] {
            let f = field_at(w);
            let b = clear_box(f);
            assert_eq!((b.w, b.h), (24.0, 24.0));
            assert_eq!(b.right(), f.right() - 8.0);
            assert_eq!(b.y + b.h / 2.0, f.y + f.h / 2.0);
        }
    }

    #[test]
    fn at_the_narrowest_field_the_box_is_inside_it_right_of_the_search_icon_with_room_to_type() {
        let f = field_at(FIELD_MIN_W);
        let b = clear_box(f);
        assert!(b.x >= f.x && b.right() <= f.right() && b.y >= f.y && b.bottom() <= f.bottom());
        // The search icon is centred at f.x + 22 and 14 wide.
        assert!(b.x > f.x + 22.0 + 7.0 + CLEAR_GAP);
        // The text area runs from f.x + 38 to the box less the gap: room for a few characters.
        let text_w = (b.x - CLEAR_GAP) - (f.x + 38.0);
        assert!(text_w >= 40.0, "{text_w}");
    }

    fn state<'a>() -> TopState<'a> {
        TopState {
            tab: Some(Tab::Home),
            can_back: false,
            can_forward: false,
            auto_hide: false,
            switch: None,
            menu_forced: false,
        }
    }

    #[test]
    fn a_click_on_the_clear_box_empties_the_field_and_focuses_it_without_a_search() {
        let k = Look::new(theme(ThemeId::A), None);
        let s = Rect::new(32.0, 8.0, 1000.0, 200.0);
        // The field's right edge and centre do not depend on the tab widths.
        let right = s.right() - GEAR_INSET - HIDE_FROM_GEAR - FIELD_FROM_GEAR;
        let cy = s.y + ROW_Y;
        let b = clear_box(Rect::new(
            right - FIELD_W,
            cy - FIELD_H / 2.0,
            FIELD_W,
            FIELD_H,
        ));
        let at = (b.x + b.w / 2.0, b.y + b.h / 2.0);

        let mut h = UiHarness::new((1100.0, 260.0));
        let mut bar = TopBar::default();
        bar.set_text("abc");
        let frame = |h: &mut UiHarness, bar: &mut TopBar, ms: u64| {
            h.at_ms(ms);
            let mut out = TopOut::default();
            h.frame(|ui| out = bar.frame(ui, s, &k, state()));
            out
        };
        frame(&mut h, &mut bar, 0);
        h.input.pointer_to(at.0, at.1);
        frame(&mut h, &mut bar, 16);
        h.input.click(PointerButton::Primary);
        let out = frame(&mut h, &mut bar, 32);
        assert!(bar.text.is_empty(), "the field was emptied: {:?}", bar.text);
        assert_eq!(out.submit, None, "no search is submitted");
        frame(&mut h, &mut bar, 48);
        frame(&mut h, &mut bar, 64);
        assert!(bar.focused(), "the field has the focus");

        // A click at the same spot with the field empty hits nothing: no box, no change.
        h.input.click(PointerButton::Primary);
        frame(&mut h, &mut bar, 80);
        assert!(bar.text.is_empty());
    }

    #[test]
    fn a_click_beside_the_box_leaves_the_text() {
        let k = Look::new(theme(ThemeId::A), None);
        let s = Rect::new(32.0, 8.0, 1000.0, 200.0);
        let right = s.right() - GEAR_INSET - HIDE_FROM_GEAR - FIELD_FROM_GEAR;
        let cy = s.y + ROW_Y;
        let b = clear_box(Rect::new(
            right - FIELD_W,
            cy - FIELD_H / 2.0,
            FIELD_W,
            FIELD_H,
        ));
        let mut h = UiHarness::new((1100.0, 260.0));
        let mut bar = TopBar::default();
        bar.set_text("abc");
        for (ms, at) in [(0, None), (16, Some((b.x - 40.0, b.y + 12.0)))] {
            h.at_ms(ms);
            if let Some((x, y)) = at {
                h.input.pointer_to(x, y);
                h.input.click(PointerButton::Primary);
            }
            h.frame(|ui| {
                bar.frame(ui, s, &k, state());
            });
        }
        assert_eq!(bar.text, "abc");
    }

    const TW: [f32; 5] = [40.0, 62.0, 50.0, 40.0, 48.0];

    #[test]
    fn the_bar_keeps_its_gaps_when_there_is_room() {
        let m = metrics(Rect::new(32.0, 52.0, 1376.0, 724.0), TW, GAP_PILL, false);
        assert_eq!(m.tab_gap, GAP_PILL);
        assert_eq!(m.field.w, FIELD_W);
        assert_eq!(m.tab_x[0], 32.0 + TABS_X);
    }

    #[test]
    fn a_narrow_sheet_shrinks_the_gap_then_the_field_without_overlap() {
        let m = metrics(Rect::new(32.0, 52.0, 696.0, 400.0), TW, GAP_PILL, false);
        assert!(m.tab_gap >= GAP_MIN && m.tab_gap < GAP_PILL);
        assert!(m.field.w >= FIELD_MIN_W);
        let last_end = m.tab_x[4] + TW[4];
        assert!(
            last_end + FIELD_GAP <= m.field.x + 0.01,
            "{last_end} {:?}",
            m.field
        );
        assert!(m.field.right() <= m.hide.0 - 16.0);
        assert!(
            m.hide.0 + 16.0 <= m.gear.0 - 16.0,
            "the icons do not overlap"
        );
    }

    #[test]
    fn without_a_switch_the_bar_is_what_it_was() {
        for w in [696.0, 900.0, 1376.0] {
            let s = Rect::new(32.0, 52.0, w, 400.0);
            let m = metrics(s, TW, GAP_PILL, false);
            assert_eq!(m.provider, None);
            // The field and the gaps follow the rules of the bar without a switch.
            let room = s.right()
                - GEAR_INSET
                - HIDE_FROM_GEAR
                - FIELD_FROM_GEAR
                - (s.x + TABS_X)
                - TW.iter().sum::<f32>()
                - FIELD_GAP;
            let gap = GAP_PILL
                .min(((room - FIELD_W) / 4.0).max(GAP_MIN))
                .max(GAP_MIN);
            assert_eq!(m.tab_gap, gap);
            assert_eq!(m.field.w, (room - 4.0 * gap).clamp(FIELD_MIN_W, FIELD_W));
        }
    }

    #[test]
    fn the_provider_button_sits_between_the_eye_and_the_gear_without_overlap() {
        for w in [1376.0, 900.0, 696.0] {
            let s = Rect::new(32.0, 52.0, w, 400.0);
            let m = metrics(s, TW, GAP_PILL, true);
            let c = m.provider.expect("a button");
            assert_eq!(c.1, m.cy);
            assert_eq!(c.0 - m.hide.0, m.gear.0 - c.0, "midway between the icons");
            assert!(m.hide.0 + HIT_HALF <= c.0 - HIT_HALF);
            assert!(c.0 + HIT_HALF <= m.gear.0 - HIT_HALF);
            assert!(m.field.right() <= m.hide.0 - HIT_HALF);
            let last_end = m.tab_x[4] + TW[4];
            assert!(
                last_end + FIELD_GAP <= m.field.x + 0.01,
                "{w}: {last_end} {:?}",
                m.field
            );
            assert!(m.field.w >= FIELD_MIN_W && m.tab_gap >= GAP_TIGHT, "{w}");
        }
    }

    #[test]
    fn the_reported_rects_are_the_hit_rects_for_a_wide_and_a_narrow_bar() {
        for (w, with_provider) in [
            (1376.0, true),
            (1376.0, false),
            (696.0, true),
            (696.0, false),
        ] {
            let m = metrics(Rect::new(32.0, 52.0, w, 400.0), TW, GAP_PILL, with_provider);
            let around = |c: (f32, f32)| Rect::new(c.0 - 16.0, c.1 - 16.0, 32.0, 32.0);
            assert_eq!(m.gear_hit(), around(m.gear), "{w}");
            assert_eq!(m.hide_hit(), around(m.hide), "{w}");
            assert_eq!(m.provider_hit(), m.provider.map(around), "{w}");
            assert_eq!(m.provider_hit().is_some(), with_provider, "{w}");
        }
    }

    #[test]
    fn the_auto_hide_tooltip_names_the_state() {
        assert!(auto_hide_tip(true).contains("on"));
        assert!(auto_hide_tip(false).contains("off"));
    }
}
