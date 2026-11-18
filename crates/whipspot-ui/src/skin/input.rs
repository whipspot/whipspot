//! Pointer input of one skin panel: hit testing in skin pixels, the press, drag and release of
//! buttons, toggles and sliders (D7), and what each of them asks for. Pure over the toolkit's raw
//! events, so it is tested without a window; drawing reads [`PanelInput`] for the pressed look.
//!
//! One skin pixel is `k / scale` logical pixels (`k` the panel set's pixel scale, `scale` the
//! frame's); events come in logical pixels and are converted here.

use lune_ui::input::{Event, PointerButton};
use lune_ui::shortcuts::{Binding, Chord, CommandId, Scope, Shortcuts};
use whipspot_core::player::{PlayerAction, PlayerState};
use whipspot_core::skin::{
    Axis, Command, Panel, Rect, Size, Skin, Value, Widget, WidgetKind, WindowCommand,
};

use super::bind;

/// Shortcut ids of the skin windows that the native table does not have.
pub(crate) const ON_TOP: CommandId = CommandId::new("skin.on-top");
pub(crate) const NATIVE_LOOK: CommandId = CommandId::new("skin.native-look");
/// The ids the native table shares (so `crate::shortcuts::Command::from_id` reads them).
const PLAY_PAUSE: CommandId = CommandId::new("player.play-pause");
const PREVIOUS: CommandId = CommandId::new("player.previous");
const NEXT: CommandId = CommandId::new("player.next");
const VOLUME_UP: CommandId = CommandId::new("player.volume-up");
const VOLUME_DOWN: CommandId = CommandId::new("player.volume-down");
const SHUFFLE: CommandId = CommandId::new("player.shuffle");
const REPEAT: CommandId = CommandId::new("player.repeat");
const DIAGNOSTICS: CommandId = CommandId::new("view.diagnostics");

/// The keyboard table of the skin windows (D9): Space; Primary with Left, Right, Up, Down, S and R;
/// Primary+Shift with D (diagnostics), A (always on top) and N (the native look).
pub(crate) fn table() -> Shortcuts {
    let named = |k| Chord::key(lune_ui::input::Key::Named(k));
    let character = |c: &str| Chord::key(lune_ui::input::Key::Character(c.into()));
    let global = |chord, command| Binding::new(Scope::Global, chord, command);
    use lune_ui::input::NamedKey;
    let mut table = Shortcuts::default();
    table
        .add(global(named(NamedKey::Space), PLAY_PAUSE))
        .add(global(named(NamedKey::ArrowLeft).primary(), PREVIOUS))
        .add(global(named(NamedKey::ArrowRight).primary(), NEXT))
        .add(global(named(NamedKey::ArrowUp).primary(), VOLUME_UP).repeating())
        .add(global(named(NamedKey::ArrowDown).primary(), VOLUME_DOWN).repeating())
        .add(global(character("s").primary(), SHUFFLE))
        .add(global(character("r").primary(), REPEAT))
        .add(global(character("d").primary().shift(), DIAGNOSTICS))
        .add(global(character("a").primary().shift(), ON_TOP))
        .add(global(character("n").primary().shift(), NATIVE_LOOK));
    table
}

/// Logical pixels per skin pixel are `k / scale`: the conversion both ways.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Metrics {
    /// Physical pixels per skin pixel (the panel set's pixel scale).
    pub k: f32,
    /// Physical pixels per logical pixel (the frame's scale).
    pub scale: f32,
}

impl Metrics {
    /// A logical position as skin pixels.
    pub fn to_skin(self, pos: (f32, f32)) -> (f32, f32) {
        let f = self.scale / self.k;
        (pos.0 * f, pos.1 * f)
    }

    /// Logical pixels per skin pixel.
    pub fn logical_per_skin(self) -> f32 {
        self.k / self.scale
    }
}

/// What a pointer event asks of the front-end.
#[derive(Clone)]
pub(crate) enum Effect {
    /// A player action to dispatch.
    Action(PlayerAction),
    /// Show or hide the panel with this index.
    TogglePanel(usize),
    /// Show the first panel and hide the second when the first is hidden, else the other way.
    SwapPanels(usize, usize),
    /// Hide the widget's own panel (the first panel quits).
    ClosePanel,
    /// Minimise the whole set.
    Minimize,
    /// The press of a drag area: the panel starts moving.
    DragPress,
    /// The pointer was released after a drag.
    DragRelease,
    /// The scroll bar's thumb was pressed, moved or let go.
    Scroll(ScrollOp),
    /// The press of a resize grip: the panel starts resizing.
    ResizePress,
    /// The pointer was released after a resize.
    ResizeRelease,
}

/// What the pointer does to a panel's scroll bar; `y` is the pointer's row in skin pixels. The
/// list owns the scroll position, so the lists apply it (`lists::RowScroll`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum ScrollOp {
    Press(f32),
    Move(f32),
    Release,
}

/// What a press captured and what it has sent so far.
struct Capture {
    widget: usize,
    /// The slider value under the pointer now.
    value: Option<f32>,
    /// The last value sent (quantised), so only a change dispatches.
    sent: Option<i32>,
}

/// One panel's pointer state.
#[derive(Default)]
pub(crate) struct PanelInput {
    /// The pointer in skin pixels, `None` once it left the window.
    pointer: Option<(f32, f32)>,
    capture: Option<Capture>,
    /// The panel's size now in skin pixels; the base size until set.
    size: Option<(u32, u32)>,
}

/// Whether skin pixel position `p` is inside `r` (right and bottom edges outside).
fn contains(r: Rect, p: (f32, f32)) -> bool {
    p.0 >= r.x as f32
        && p.0 < (u64::from(r.x) + u64::from(r.w)) as f32
        && p.1 >= r.y as f32
        && p.1 < (u64::from(r.y) + u64::from(r.h)) as f32
}

/// The widget's rect at the panel's size `size` (skin pixels).
fn rect_in(panel: &Panel, w: &Widget, size: (u32, u32)) -> Rect {
    w.rect_at(
        panel.size,
        Size {
            w: size.0,
            h: size.1,
        },
    )
}

/// Whether a widget takes the pointer (and so hides what is under it).
fn takes_pointer(kind: &WidgetKind) -> bool {
    matches!(
        kind,
        WidgetKind::Button { .. }
            | WidgetKind::Toggle { .. }
            | WidgetKind::Slider { .. }
            | WidgetKind::DragArea
            | WidgetKind::TrackList
            | WidgetKind::Library
            | WidgetKind::ListScroll { .. }
            | WidgetKind::ResizeArea
    )
}

/// The topmost widget that takes the pointer at `p` (skin pixels), the last in paint order, with
/// the panel at `size` (skin pixels).
pub(crate) fn hit(panel: &Panel, size: (u32, u32), p: (f32, f32)) -> Option<usize> {
    panel
        .widgets
        .iter()
        .enumerate()
        .rev()
        .find(|(_, w)| takes_pointer(&w.kind) && contains(rect_in(panel, w, size), p))
        .map(|(i, _)| i)
}

/// The slider value for a pointer at `p` (skin pixels) over the slider's rect `r`: the thumb is
/// centred under the pointer. `Horizontal` is 0 at the left, `Vertical` is 1 at the top.
pub(crate) fn slider_value(r: Rect, axis: Axis, thumb: u32, p: (f32, f32)) -> f32 {
    let (extent, coord) = match axis {
        Axis::Horizontal => (r.w, p.0 - r.x as f32),
        Axis::Vertical => (r.h, p.1 - r.y as f32),
    };
    let travel = extent.saturating_sub(thumb);
    if travel == 0 {
        return 0.0;
    }
    let frac = ((coord - thumb as f32 / 2.0) / travel as f32).clamp(0.0, 1.0);
    match axis {
        Axis::Horizontal => frac,
        Axis::Vertical => 1.0 - frac,
    }
}

/// The thumb's extent along the slider's axis (0 without a thumb).
fn thumb_extent(axis: Axis, thumb: &Option<[whipspot_core::skin::SpriteRect; 2]>) -> u32 {
    thumb.map_or(0, |t| match axis {
        Axis::Horizontal => t[0].rect.w,
        Axis::Vertical => t[0].rect.h,
    })
}

/// A value the action sets, as a whole number, so a repeated value is not sent twice.
fn sent_key(action: &PlayerAction) -> Option<i32> {
    match action {
        PlayerAction::SetVolume(v) => Some(i32::from(*v)),
        PlayerAction::SetBalance(v) => Some(i32::from(*v)),
        PlayerAction::SetEqPreamp(db) => Some((db * 2.0).round() as i32),
        PlayerAction::SetEqBand { gain_db, .. } => Some((gain_db * 2.0).round() as i32),
        PlayerAction::Seek { position_ms } => Some(*position_ms as i32),
        _ => None,
    }
}

impl PanelInput {
    /// Sets the panel's size now in skin pixels (call it before the frame's events).
    pub fn set_size(&mut self, size: (u32, u32)) {
        self.size = Some(size);
    }

    fn size_of(&self, panel: &Panel) -> (u32, u32) {
        self.size.unwrap_or((panel.size.w, panel.size.h))
    }

    /// The rect of `w` of `panel` at the panel's size now.
    fn rect(&self, panel: &Panel, w: &Widget) -> Rect {
        rect_in(panel, w, self.size_of(panel))
    }

    /// Drops a held press without its release effects (its release was lost).
    pub fn cancel(&mut self) {
        self.capture = None;
    }

    /// Whether a press is held (the panel then draws at the pointer's pace).
    pub fn held(&self) -> bool {
        self.capture.is_some()
    }

    /// Whether `widget` is held with the pointer inside it: it draws its pressed sprite.
    pub fn pressed(&self, panel: &Panel, widget: usize) -> bool {
        let (Some(c), Some(p)) = (&self.capture, self.pointer) else {
            return false;
        };
        c.widget == widget
            && panel
                .widgets
                .get(widget)
                .is_some_and(|w| contains(self.rect(panel, w), p))
    }

    /// Whether `widget` is the one held (a slider's thumb shows pressed).
    pub fn held_widget(&self, widget: usize) -> bool {
        self.capture.as_ref().is_some_and(|c| c.widget == widget)
    }

    /// The value a held slider shows instead of the player's (what the pointer set).
    pub fn dragged_value(&self, widget: usize) -> Option<f32> {
        self.capture
            .as_ref()
            .filter(|c| c.widget == widget)
            .and_then(|c| c.value)
    }

    /// Handles one raw event of the panel `index` of `skin`, appending what it asks for to `out`.
    pub fn event(
        &mut self,
        event: &Event,
        m: Metrics,
        skin: &Skin,
        index: usize,
        state: &PlayerState,
        out: &mut Vec<Effect>,
    ) {
        let Some(panel) = skin.panels.get(index) else {
            return;
        };
        match event {
            Event::PointerMoved { pos } => {
                let p = m.to_skin(*pos);
                self.pointer = Some(p);
                self.update_slider(panel, p, state, out);
                self.update_scroll(panel, p, out);
            }
            Event::PointerLeft => self.pointer = None,
            Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed: true,
                ..
            } => {
                let p = m.to_skin(*pos);
                self.pointer = Some(p);
                self.press(panel, p, state, out);
            }
            Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed: false,
                ..
            } => {
                let p = m.to_skin(*pos);
                self.pointer = Some(p);
                self.release(skin, panel, p, state, out);
            }
            _ => {}
        }
    }

    fn press(&mut self, panel: &Panel, p: (f32, f32), state: &PlayerState, out: &mut Vec<Effect>) {
        self.capture = None;
        let Some(i) = hit(panel, self.size_of(panel), p) else {
            return;
        };
        let widget = &panel.widgets[i];
        match &widget.kind {
            WidgetKind::Button { .. } | WidgetKind::Toggle { .. } => {
                self.capture = Some(Capture {
                    widget: i,
                    value: None,
                    sent: None,
                });
            }
            WidgetKind::Slider { .. } => {
                self.capture = Some(Capture {
                    widget: i,
                    value: None,
                    sent: None,
                });
                self.update_slider(panel, p, state, out);
            }
            WidgetKind::DragArea => {
                self.capture = Some(Capture {
                    widget: i,
                    value: None,
                    sent: None,
                });
                out.push(Effect::DragPress);
            }
            WidgetKind::ResizeArea => {
                self.capture = Some(Capture {
                    widget: i,
                    value: None,
                    sent: None,
                });
                out.push(Effect::ResizePress);
            }
            WidgetKind::ListScroll { .. } => {
                self.capture = Some(Capture {
                    widget: i,
                    value: None,
                    sent: None,
                });
                out.push(Effect::Scroll(ScrollOp::Press(p.1)));
            }
            // The lists take their own input; a press on one only blocks what is under it.
            _ => {}
        }
    }

    /// Moves a held slider to the pointer; dispatches the live ones when the value changed.
    fn update_slider(
        &mut self,
        panel: &Panel,
        p: (f32, f32),
        state: &PlayerState,
        out: &mut Vec<Effect>,
    ) {
        let size = self.size_of(panel);
        let Some(c) = &mut self.capture else {
            return;
        };
        let Some(w) = panel.widgets.get(c.widget) else {
            return;
        };
        let WidgetKind::Slider {
            value, axis, thumb, ..
        } = &w.kind
        else {
            return;
        };
        // A position with nothing to seek in shows no drag.
        if *value == Value::Position && bind::action_for(*value, 0.0, state).is_none() {
            return;
        }
        let v = slider_value(
            rect_in(panel, w, size),
            *axis,
            thumb_extent(*axis, thumb),
            p,
        );
        c.value = Some(v);
        if *value == Value::Position {
            return;
        }
        if let Some(action) = bind::action_for(*value, v, state) {
            let key = sent_key(&action);
            if key.is_none() || key != c.sent {
                c.sent = key;
                out.push(Effect::Action(action));
            }
        }
    }

    /// Tells the list where a held scroll-bar thumb is now.
    fn update_scroll(&self, panel: &Panel, p: (f32, f32), out: &mut Vec<Effect>) {
        let held = self
            .capture
            .as_ref()
            .and_then(|c| panel.widgets.get(c.widget))
            .is_some_and(|w| matches!(w.kind, WidgetKind::ListScroll { .. }));
        if held {
            out.push(Effect::Scroll(ScrollOp::Move(p.1)));
        }
    }

    fn release(
        &mut self,
        skin: &Skin,
        panel: &Panel,
        p: (f32, f32),
        state: &PlayerState,
        out: &mut Vec<Effect>,
    ) {
        let Some(c) = self.capture.take() else {
            return;
        };
        let Some(w) = panel.widgets.get(c.widget) else {
            return;
        };
        match &w.kind {
            WidgetKind::Button { command, .. } | WidgetKind::Toggle { command, .. } => {
                if contains(self.rect(panel, w), p) {
                    out.extend(command_effect(command, skin, state));
                }
            }
            WidgetKind::Slider {
                value: Value::Position,
                ..
            } => {
                if let Some(v) = c.value
                    && let Some(action) = bind::action_for(Value::Position, v, state)
                {
                    out.push(Effect::Action(action));
                }
            }
            WidgetKind::DragArea => out.push(Effect::DragRelease),
            WidgetKind::ListScroll { .. } => out.push(Effect::Scroll(ScrollOp::Release)),
            WidgetKind::ResizeArea => out.push(Effect::ResizeRelease),
            _ => {}
        }
    }
}

/// What a button's command asks for.
fn command_effect(command: &Command, skin: &Skin, state: &PlayerState) -> Option<Effect> {
    match command {
        Command::Player(c) => Some(Effect::Action(bind::command_action(*c, state))),
        Command::Window(WindowCommand::TogglePanel(id)) => skin
            .panels
            .iter()
            .position(|p| &p.id == id)
            .map(Effect::TogglePanel),
        Command::Window(WindowCommand::SwapPanels(a, b)) => {
            let at =
                |id: &whipspot_core::skin::PanelId| skin.panels.iter().position(|p| &p.id == id);
            Some(Effect::SwapPanels(at(a)?, at(b)?))
        }
        Command::Window(WindowCommand::ClosePanel) => Some(Effect::ClosePanel),
        Command::Window(WindowCommand::Minimize) => Some(Effect::Minimize),
    }
}

#[cfg(test)]
mod tests {
    use lune_ui::input::Modifiers;
    use whipspot_core::player::PlayerAction;
    use whipspot_core::skin::{
        Align, Anchor, Fill, ImageId, PlayerCommand, Rect, SkinInfo, SkinTheme, SpriteRect,
    };

    use super::*;
    use crate::skin::fake::{panel, player_state, with_current};

    fn sprite() -> SpriteRect {
        SpriteRect {
            image: ImageId(0),
            rect: Rect {
                x: 0,
                y: 0,
                w: 4,
                h: 4,
            },
        }
    }

    fn widget(x: u32, y: u32, w: u32, h: u32, kind: WidgetKind) -> Widget {
        Widget {
            rect: Rect { x, y, w, h },
            anchor: Anchor {
                h: Align::Start,
                v: Align::Start,
            },
            kind,
        }
    }

    fn button(x: u32, y: u32, command: Command) -> Widget {
        widget(
            x,
            y,
            20,
            10,
            WidgetKind::Button {
                command,
                normal: sprite(),
                pressed: sprite(),
            },
        )
    }

    fn slider(value: Value, thumb: bool) -> Widget {
        widget(
            0,
            20,
            100,
            10,
            WidgetKind::Slider {
                value,
                axis: Axis::Horizontal,
                frames: vec![sprite()],
                frame_rule: whipspot_core::skin::FrameRule::Linear,
                thumb: thumb.then_some(
                    [SpriteRect {
                        image: ImageId(0),
                        rect: Rect {
                            x: 0,
                            y: 0,
                            w: 10,
                            h: 10,
                        },
                    }; 2],
                ),
            },
        )
    }

    fn skin(widgets: Vec<Widget>) -> Skin {
        let mut p = panel("main", 100, 50, None);
        p.widgets = widgets;
        let mut other = panel("equalizer", 100, 50, None);
        other.visible_at_start = true;
        Skin {
            info: SkinInfo {
                engine: String::new(),
                name: String::new(),
            },
            panels: vec![p, other],
            images: Vec::new(),
            fonts: Vec::new(),
            visualiser: None,
            theme: SkinTheme::default(),
        }
    }

    const M: Metrics = Metrics { k: 1.0, scale: 1.0 };

    fn down(x: f32, y: f32) -> Event {
        Event::PointerButton {
            pos: (x, y),
            button: PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::default(),
        }
    }

    fn up(x: f32, y: f32) -> Event {
        Event::PointerButton {
            pos: (x, y),
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::default(),
        }
    }

    fn mv(x: f32, y: f32) -> Event {
        Event::PointerMoved { pos: (x, y) }
    }

    fn run(
        input: &mut PanelInput,
        skin: &Skin,
        state: &PlayerState,
        events: &[Event],
    ) -> Vec<Effect> {
        let mut out = Vec::new();
        for e in events {
            input.event(e, M, skin, 0, state, &mut out);
        }
        out
    }

    #[test]
    fn hit_testing_picks_the_topmost_widget_that_takes_the_pointer() {
        let under = button(0, 0, Command::Player(PlayerCommand::Play));
        let over = button(10, 0, Command::Player(PlayerCommand::Stop));
        let image = widget(
            0,
            0,
            100,
            50,
            WidgetKind::Image {
                sprite: sprite(),
                fill: Fill::Once,
            },
        );
        let s = skin(vec![under, over, image]);
        // Where the two overlap, the later one; the still image above them takes nothing.
        assert_eq!(hit(&s.panels[0], (100, 50), (15.0, 5.0)), Some(1));
        assert_eq!(hit(&s.panels[0], (100, 50), (5.0, 5.0)), Some(0));
        assert_eq!(hit(&s.panels[0], (100, 50), (60.0, 5.0)), None);
        // The rect's right and bottom edges are outside.
        assert_eq!(hit(&s.panels[0], (100, 50), (30.0, 5.0)), None);
    }

    #[test]
    fn a_press_and_release_inside_a_button_dispatches_once() {
        let s = skin(vec![button(0, 0, Command::Player(PlayerCommand::Stop))]);
        let st = player_state();
        let mut input = PanelInput::default();
        let out = run(&mut input, &s, &st, &[down(5.0, 5.0), up(6.0, 6.0)]);
        assert_eq!(out.len(), 1);
        assert!(matches!(out[0], Effect::Action(PlayerAction::Stop)));
        assert!(!input.held());
    }

    #[test]
    fn a_release_outside_the_button_dispatches_nothing() {
        let s = skin(vec![button(0, 0, Command::Player(PlayerCommand::Stop))]);
        let st = player_state();
        let mut input = PanelInput::default();
        let out = run(&mut input, &s, &st, &[down(5.0, 5.0), mv(60.0, 40.0)]);
        assert!(out.is_empty());
        assert!(!input.pressed(&s.panels[0], 0), "pressed only inside");
        let out = run(&mut input, &s, &st, &[up(60.0, 40.0)]);
        assert!(out.is_empty());
        // Back inside before the release: pressed again and it fires.
        let out = run(
            &mut input,
            &s,
            &st,
            &[down(5.0, 5.0), mv(60.0, 40.0), mv(8.0, 8.0)],
        );
        assert!(out.is_empty());
        assert!(input.pressed(&s.panels[0], 0));
        let out = run(&mut input, &s, &st, &[up(8.0, 8.0)]);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn a_volume_drag_dispatches_only_when_the_value_changes() {
        let s = skin(vec![slider(Value::Volume, true)]);
        let st = player_state();
        let mut input = PanelInput::default();
        // Thumb 10 wide in 100: travel 90; the pointer at 5 + 45 centres the thumb at v = 0.5.
        let out = run(&mut input, &s, &st, &[down(50.0, 25.0)]);
        let vols: Vec<u8> = out
            .iter()
            .filter_map(|e| match e {
                Effect::Action(PlayerAction::SetVolume(v)) => Some(*v),
                _ => None,
            })
            .collect();
        assert_eq!(vols, [50]);
        // Moving within the same value sends nothing; a new value sends once.
        let out = run(&mut input, &s, &st, &[mv(50.2, 25.0), mv(50.3, 26.0)]);
        assert!(out.is_empty());
        let out = run(&mut input, &s, &st, &[mv(95.0, 25.0), mv(95.0, 25.0)]);
        assert_eq!(out.len(), 1);
        assert!(matches!(
            out[0],
            Effect::Action(PlayerAction::SetVolume(100))
        ));
        // The release sends nothing more for a live slider.
        assert!(run(&mut input, &s, &st, &[up(95.0, 25.0)]).is_empty());
    }

    #[test]
    fn the_position_dispatches_one_seek_on_release() {
        let s = skin(vec![slider(Value::Position, true)]);
        let st = with_current(player_state(), 200_000);
        let mut input = PanelInput::default();
        let out = run(
            &mut input,
            &s,
            &st,
            &[down(50.0, 25.0), mv(70.0, 25.0), mv(55.0, 25.0)],
        );
        assert!(out.is_empty(), "no seek while dragging");
        assert!(input.dragged_value(0).is_some());
        let out = run(&mut input, &s, &st, &[up(55.0, 25.0)]);
        assert_eq!(out.len(), 1);
        let Effect::Action(PlayerAction::Seek { position_ms }) = out[0] else {
            panic!("not a seek");
        };
        // The pointer at 55 centres the thumb: v = (55 - 5) / 90.
        let v = 50.0 / 90.0;
        assert_eq!(position_ms, (v * 200_000.0_f64).round() as u32);
    }

    #[test]
    fn a_position_with_nothing_current_does_nothing() {
        let s = skin(vec![slider(Value::Position, true)]);
        let st = player_state();
        let mut input = PanelInput::default();
        let out = run(&mut input, &s, &st, &[down(50.0, 25.0), up(50.0, 25.0)]);
        assert!(out.is_empty());
    }

    #[test]
    fn a_press_on_a_drag_area_starts_and_ends_a_drag() {
        let s = skin(vec![widget(0, 0, 100, 14, WidgetKind::DragArea)]);
        let st = player_state();
        let mut input = PanelInput::default();
        let out = run(&mut input, &s, &st, &[down(5.0, 5.0)]);
        assert!(matches!(out[..], [Effect::DragPress]));
        let out = run(&mut input, &s, &st, &[up(40.0, 90.0)]);
        assert!(matches!(out[..], [Effect::DragRelease]));
    }

    #[test]
    fn window_commands_resolve_to_effects() {
        let eq = whipspot_core::skin::PanelId::new("equalizer").unwrap();
        let ghost = whipspot_core::skin::PanelId::new("ghost").unwrap();
        let s = skin(vec![
            button(0, 0, Command::Window(WindowCommand::TogglePanel(eq))),
            button(20, 0, Command::Window(WindowCommand::TogglePanel(ghost))),
            button(40, 0, Command::Window(WindowCommand::ClosePanel)),
            button(60, 0, Command::Window(WindowCommand::Minimize)),
        ]);
        let st = player_state();
        let mut input = PanelInput::default();
        let click =
            |input: &mut PanelInput, x: f32| run(input, &s, &st, &[down(x, 5.0), up(x, 5.0)]);
        assert!(matches!(
            click(&mut input, 5.0)[..],
            [Effect::TogglePanel(1)]
        ));
        assert!(click(&mut input, 25.0).is_empty());
        assert!(matches!(click(&mut input, 45.0)[..], [Effect::ClosePanel]));
        assert!(matches!(click(&mut input, 65.0)[..], [Effect::Minimize]));
    }

    #[test]
    fn a_swap_resolves_both_panels_or_nothing() {
        let id = |s: &str| whipspot_core::skin::PanelId::new(s).unwrap();
        let swap = |a: &str, b: &str| Command::Window(WindowCommand::SwapPanels(id(a), id(b)));
        let s = skin(vec![
            button(0, 0, swap("equalizer", "main")),
            button(20, 0, swap("equalizer", "ghost")),
        ]);
        let st = player_state();
        let mut input = PanelInput::default();
        let click =
            |input: &mut PanelInput, x: f32| run(input, &s, &st, &[down(x, 5.0), up(x, 5.0)]);
        assert!(matches!(
            click(&mut input, 5.0)[..],
            [Effect::SwapPanels(1, 0)]
        ));
        assert!(click(&mut input, 25.0).is_empty());
    }

    #[test]
    fn positions_convert_through_the_pixel_scale() {
        // k 2 at a frame scale of 1.5: one skin pixel is 4/3 logical pixels.
        let m = Metrics { k: 2.0, scale: 1.5 };
        let (x, y) = m.to_skin((40.0, 20.0));
        assert!((x - 30.0).abs() < 1e-4 && (y - 15.0).abs() < 1e-4);
        assert!((m.logical_per_skin() - 4.0 / 3.0).abs() < 1e-6);
    }

    #[test]
    fn a_vertical_slider_is_one_at_the_top() {
        let mut w = slider(Value::EqPreamp, true);
        w.rect = Rect {
            x: 0,
            y: 0,
            w: 10,
            h: 100,
        };
        // Thumb 10 high: travel 90.
        assert_eq!(slider_value(w.rect, Axis::Vertical, 10, (5.0, 5.0)), 1.0);
        assert_eq!(slider_value(w.rect, Axis::Vertical, 10, (5.0, 95.0)), 0.0);
        let mid = slider_value(w.rect, Axis::Vertical, 10, (5.0, 50.0));
        assert!((mid - 0.5).abs() < 1e-6);
    }
    fn scroll_bar() -> Widget {
        widget(
            90,
            14,
            8,
            30,
            WidgetKind::ListScroll {
                thumb: [sprite(); 2],
            },
        )
    }

    #[test]
    fn a_press_on_the_scroll_bar_captures_it_and_reports_the_pointer_row() {
        let s = skin(vec![
            widget(0, 0, 100, 50, WidgetKind::DragArea),
            widget(0, 14, 90, 30, WidgetKind::TrackList),
            scroll_bar(),
        ]);
        let st = player_state();
        let mut input = PanelInput::default();
        // The bar takes the press: no drag starts under it.
        let out = run(&mut input, &s, &st, &[down(94.0, 20.0)]);
        assert!(matches!(out[..], [Effect::Scroll(ScrollOp::Press(y))] if y == 20.0));
        assert!(input.held_widget(2));
        let out = run(&mut input, &s, &st, &[mv(95.0, 33.0)]);
        assert!(matches!(out[..], [Effect::Scroll(ScrollOp::Move(y))] if y == 33.0));
        let out = run(&mut input, &s, &st, &[up(95.0, 33.0)]);
        assert!(matches!(out[..], [Effect::Scroll(ScrollOp::Release)]));
        assert!(!input.held());
        // A move with nothing held says nothing.
        assert!(run(&mut input, &s, &st, &[mv(95.0, 40.0)]).is_empty());
    }

    #[test]
    fn hit_testing_follows_the_rects_at_a_grown_size() {
        // A close button 10 px from the right edge of a 100-wide panel, anchored right.
        let mut close = button(70, 0, Command::Window(WindowCommand::ClosePanel));
        close.anchor.h = Align::End;
        let s = skin(vec![close]);
        let p = &s.panels[0];
        // At the base size it sits at 70..90; grown by 50 it sits at 120..140.
        assert_eq!(hit(p, (100, 50), (75.0, 5.0)), Some(0));
        assert_eq!(hit(p, (150, 50), (75.0, 5.0)), None);
        assert_eq!(hit(p, (150, 50), (125.0, 5.0)), Some(0));
        // The input reads the size it was given: a click on the moved button closes the panel.
        let st = player_state();
        let mut input = PanelInput::default();
        input.set_size((150, 50));
        let out = run(&mut input, &s, &st, &[down(125.0, 5.0), up(125.0, 5.0)]);
        assert!(matches!(out[..], [Effect::ClosePanel]));
        let out = run(&mut input, &s, &st, &[down(75.0, 5.0), up(75.0, 5.0)]);
        assert!(out.is_empty());
    }

    #[test]
    fn a_stretched_slider_takes_its_value_from_the_grown_rect() {
        let mut w = slider(Value::Volume, true);
        w.anchor.h = Align::Stretch;
        let s = skin(vec![w]);
        let st = player_state();
        let mut input = PanelInput::default();
        input.set_size((200, 50));
        // The slider is 200 wide now: thumb 10, travel 190, the pointer at 100 is the middle.
        let out = run(&mut input, &s, &st, &[down(100.0, 25.0)]);
        assert!(matches!(
            out[..],
            [Effect::Action(PlayerAction::SetVolume(50))]
        ));
    }
}
