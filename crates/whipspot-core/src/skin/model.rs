//! The plain data of a skin. Every type derives `Debug, Clone, PartialEq`; fields are public like
//! `provider::model`. Ids are newtypes so an index cannot be mixed up with a length.
//!
//! Coordinates and sizes are pixels at 1x. A front-end scales; an engine never does.

use super::{MAX_PANEL_SIDE, SkinImage};

/// A validated panel id, `^[a-z0-9][a-z0-9._-]{0,31}$`. Opaque: never a fixed enum, because
/// each engine declares its own panels and a front-end only compares and looks them up.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PanelId(String);

impl PanelId {
    /// `None` unless `s` matches `^[a-z0-9][a-z0-9._-]{0,31}$`.
    pub fn new(s: &str) -> Option<PanelId> {
        let b = s.as_bytes();
        let ok = (1..=32).contains(&b.len())
            && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
            && b[1..].iter().all(|c| {
                c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'.' | b'_' | b'-')
            });
        ok.then(|| PanelId(s.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// An index into [`Skin::images`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageId(pub u16);

/// An index into [`Skin::fonts`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FontId(pub u16);

/// A width and height in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Size {
    pub w: u32,
    pub h: u32,
}

/// A rectangle in pixels at 1x: x grows right, y grows down. A widget `Rect` is relative to its
/// panel's top-left at the panel's base `size`; the rect of a [`SpriteRect`] is relative to its
/// image's top-left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// A point in pixels at 1x, same axes as [`Rect`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Point {
    pub x: u32,
    pub y: u32,
}

/// An opaque colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

/// A part of one image: `rect` is relative to the image's top-left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpriteRect {
    pub image: ImageId,
    pub rect: Rect,
}

/// A whole skin as an engine built it. [`super::SkinRegistry::load`] validates it.
#[derive(Debug, Clone, PartialEq)]
pub struct Skin {
    /// The engine id and display name; the registry fills both.
    pub info: SkinInfo,
    /// The declared panels; the first one is the main panel.
    pub panels: Vec<Panel>,
    pub images: Vec<SkinImage>,
    pub fonts: Vec<BitmapFont>,
    /// Colours for the spectrum widget; `None` makes the front-end use its theme colours.
    pub visualiser: Option<VisPalette>,
    pub theme: SkinTheme,
}

/// Who built a skin and what it is called.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkinInfo {
    /// The id of the engine that built the skin (`SkinEngine::id`).
    pub engine: String,
    /// The display name, cleaned with `display_text` and at most 100 characters.
    pub name: String,
}

/// One OS window of a skin.
#[derive(Debug, Clone, PartialEq)]
pub struct Panel {
    pub id: PanelId,
    /// A display title, cleaned with `display_text`, at most 100 characters.
    pub title: String,
    /// The base size, `1..=MAX_PANEL_SIDE` per side.
    pub size: Size,
    /// `None` is a fixed size.
    pub resize: Option<Resize>,
    pub shape: Shape,
    pub dock: Dock,
    /// Whether the panel is shown when the skin is applied.
    pub visible_at_start: bool,
    /// Paint order: the first widget is at the back.
    pub widgets: Vec<Widget>,
}

/// `Resize { min, step }`: the allowed sizes are `min + k * step` per axis (k >= 0); a step of 0
/// fixes that axis. A panel with a `Resize` has `Shape::Rect` (a `validate` rule).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resize {
    pub min: Size,
    pub step: Size,
}

impl Resize {
    /// The allowed size nearest to `want`, per axis: a step of 0 gives `base` on that axis;
    /// otherwise `min + k * step` with k = round((want - min) / step), half rounding up, `want`
    /// below `min` giving `min`, and the largest lattice value not above
    /// `min(max, MAX_PANEL_SIDE)` as the upper bound (never below `min`).
    pub fn snap(&self, base: Size, want: Size, max: Size) -> Size {
        Size {
            w: snap_axis(self.min.w, self.step.w, base.w, want.w, max.w),
            h: snap_axis(self.min.h, self.step.h, base.h, want.h, max.h),
        }
    }
}

fn snap_axis(min: u32, step: u32, base: u32, want: u32, max: u32) -> u32 {
    if step == 0 {
        return base;
    }
    let (min, step) = (u64::from(min), u64::from(step));
    let cap = u64::from(max.min(MAX_PANEL_SIDE));
    let top = if cap <= min {
        min
    } else {
        min + (cap - min) / step * step
    };
    let want = u64::from(want);
    if want <= min {
        return min as u32;
    }
    // round-half-up of (want - min) / step.
    let k = ((want - min) * 2 + step) / (step * 2);
    // `top` is at most MAX_PANEL_SIDE or `min`, so it fits a u32.
    (min + k * step).min(top) as u32
}

/// The outline of a panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shape {
    /// The whole `size` rectangle.
    Rect,
    /// The union of the polygons at the base size, points in panel pixels (inside `0..=w` by
    /// `0..=h`), edges between consecutive points and from the last back to the first; outside
    /// the union is see-through and click-through.
    Polygons(Vec<Vec<Point>>),
}

/// How a panel docks with others.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Dock {
    /// Panels with the same group move together while docked.
    pub group: Option<String>,
    /// Where the panel sits when the skin is applied.
    pub attach: Option<Attach>,
    /// The panel snaps to the screen edges and to other panels.
    pub snap: bool,
}

/// `Attach { to, edge }`: at start this panel sits against `to`'s given edge, the touching edges
/// aligned at their left (`Top`, `Bottom`) or top (`Left`, `Right`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attach {
    pub to: PanelId,
    pub edge: Edge,
}

/// A side of a panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Top,
    Bottom,
    Left,
    Right,
}

/// One element of a panel.
#[derive(Debug, Clone, PartialEq)]
pub struct Widget {
    /// Relative to the panel's top-left at its base size; inside the panel (a `validate` rule).
    pub rect: Rect,
    pub anchor: Anchor,
    pub kind: WidgetKind,
}

/// How a widget rect follows a panel resize by (dw, dh) from the base size, per axis: `Start`
/// keeps x (y); `End` adds dw (dh) to x (y); `Center` adds dw / 2 (dh / 2, rounded down);
/// `Stretch` adds dw (dh) to w (h). Fixed panels use `Start` on both axes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Anchor {
    pub h: Align,
    pub v: Align,
}

/// One axis of an [`Anchor`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Start,
    End,
    Center,
    Stretch,
}

impl Widget {
    /// The widget's rect when its panel is `size` instead of the base size `base`: per axis
    /// with d = `size - base` (signed), `Start` keeps the position, `End` adds d to it, `Center`
    /// adds d / 2 rounded toward negative infinity, `Stretch` adds d to the length (floored at
    /// 0). The result is clipped to `0..size`, so a rect pushed fully outside has a width or
    /// height of 0. At `size == base` a widget inside the panel (a `validate` rule) gets its
    /// `rect` back unchanged.
    pub fn rect_at(&self, base: Size, size: Size) -> Rect {
        let (x, w) = axis_at(self.rect.x, self.rect.w, self.anchor.h, base.w, size.w);
        let (y, h) = axis_at(self.rect.y, self.rect.h, self.anchor.v, base.h, size.h);
        Rect { x, y, w, h }
    }
}

/// One axis of [`Widget::rect_at`]: `(position, length)`, all arithmetic in i64.
fn axis_at(pos: u32, len: u32, align: Align, base: u32, size: u32) -> (u32, u32) {
    let d = i64::from(size) - i64::from(base);
    let (mut p, mut l) = (i64::from(pos), i64::from(len));
    match align {
        Align::Start => {}
        Align::End => p += d,
        Align::Center => p += d.div_euclid(2),
        Align::Stretch => l = (l + d).max(0),
    }
    let limit = i64::from(size);
    let start = p.clamp(0, limit);
    let end = (p + l).clamp(0, limit);
    // Both are in `0..=size`, so they fit a u32.
    (start as u32, (end - start).max(0) as u32)
}

/// How an `Image` widget fills its rect: `Once` draws the sprite once at the rect's top-left,
/// clipped to the rect; `Tile` repeats it from the top-left across the rect, clipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fill {
    Once,
    Tile,
}

/// What a widget is and does. Bindings use the neutral enums below, never player action values:
/// an action's payload (a seek target, a volume) is decided at click time by the front-end.
#[derive(Debug, Clone, PartialEq)]
pub enum WidgetKind {
    /// A still sprite.
    Image { sprite: SpriteRect, fill: Fill },
    /// A sprite chosen by a state. With `StateRead::Flag`, `sprites[0]` is drawn when false and
    /// `sprites[1]` when true (exactly 2 sprites). With `StateRead::PlayState`, `sprites[0]` is
    /// stopped, `sprites[1]` playing (Loading shows playing) and `sprites[2]` paused (exactly 3).
    Indicator {
        state: StateRead,
        sprites: Vec<SpriteRect>,
    },
    /// Draws `normal`, or `pressed` while the pointer holds it; dispatches `command` on release.
    Button {
        command: Command,
        normal: SpriteRect,
        pressed: SpriteRect,
    },
    /// Dispatches `command` on release. Draws `off[0]` / `off[1]` (normal / pressed) while `state`
    /// is false and `on[0]` / `on[1]` while it is true.
    Toggle {
        command: Command,
        state: Flag,
        off: [SpriteRect; 2],
        on: [SpriteRect; 2],
    },
    /// A value in `0.0..=1.0`; see [`Value`] for what it maps to and [`Axis`] for direction.
    /// `frames` is the drawn background, chosen by `frame_rule` (at least one frame). The `thumb`,
    /// when present as `[normal, pressed]`, moves inside the rect: its offset from the v = 0.0 end
    /// is `round(v * (rect extent - thumb extent))` along the axis, centred across it.
    Slider {
        value: Value,
        axis: Axis,
        frames: Vec<SpriteRect>,
        frame_rule: FrameRule,
        thumb: Option<[SpriteRect; 2]>,
    },
    /// Text drawn with a bitmap font. Text shorter than the rect is left-aligned for `TextRead::TrackTitle`
    /// and right-aligned for every other read (the numbers); longer text scrolls when `scroll`
    /// is set and is clipped otherwise. Glyphs missing from the font draw
    /// the font's `fallback` (nothing when `None`).
    Text {
        text: TextRead,
        font: FontId,
        scroll: bool,
    },
    /// The equalizer response line across the rect from band 0 (left) to band 9 (right), +12 dB
    /// at the top. With `colours`, the line's colour at a row is the pixel of that row in the
    /// 1-px-wide strip.
    EqCurve { colours: Option<SpriteRect> },
    /// The spectrum in the rect, drawn with `Skin::visualiser` (bars grow from the bottom;
    /// `bars[0]` is the top colour), or with theme colours when that is `None`.
    Visualiser,
    /// The play queue, drawn with the theme colours.
    TrackList,
    /// The library browse view.
    Library,
    /// Dragging it moves the panel.
    DragArea,
    /// The scroll bar of the panel's one list widget (`TrackList` or `Library`; a `validate`
    /// rule). The rect is the track the bar moves in; `thumb` is `[normal, pressed]`, which move
    /// inside it along its height: the offset from the top is `round(v * (rect height - thumb
    /// height))`, centred across, `v` being the list's scroll position in `0.0..=1.0`. The list
    /// scrolls in whole rows, so the front-end owns the position. Both thumb sprites fit inside
    /// the rect.
    ListScroll { thumb: [SpriteRect; 2] },
    /// Dragging it resizes the panel along its [`Resize`] lattice (only in a panel with
    /// `resize: Some`; a `validate` rule). Draws nothing itself: the grip's art is an `Image`
    /// under it.
    ResizeArea,
}

/// `Horizontal`: v = 0.0 at the rect's left, 1.0 at its right. `Vertical`: v = 1.0 at the top,
/// 0.0 at the bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    Horizontal,
    Vertical,
}

/// Which frame of a slider is drawn. `Linear`: `frames[round(v * (n - 1))]`, so `frames[0]` is
/// v = 0.0. `FromCentre`: `frames[round(|v - 0.5| * 2 * (n - 1))]`, so `frames[0]` is the
/// centre and `frames[n - 1]` either end. The engine orders `frames` to fit the rule whatever
/// the order in its source sheet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameRule {
    Linear,
    FromCentre,
}

/// What a button or toggle does on release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Player(PlayerCommand),
    /// Handled by the front-end; there is no player state for it.
    Window(WindowCommand),
}

/// A player command. `Play`, `Pause`, `Stop`, `Next` and `Previous` dispatch the same-named
/// player action; `TogglePlay` dispatches `Toggle`; `ToggleShuffle` dispatches
/// `SetShuffle(!shuffle)`; `CycleRepeat` dispatches `SetRepeat` stepping Off, All, One, Off;
/// `ToggleEq` dispatches `SetEqEnabled(!enabled)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerCommand {
    Play,
    Pause,
    TogglePlay,
    Stop,
    Next,
    Previous,
    ToggleShuffle,
    CycleRepeat,
    ToggleEq,
}

/// A window command, handled by the front-end: `TogglePanel(id)` shows or hides that panel,
/// `SwapPanels(a, b)` shows `a` and hides `b` when `a` is hidden, else hides `a` and shows `b`
/// (two pictures of one thing, such as a closed and an open drawer, of which one is shown; the
/// two ids differ and neither is the first panel, a `validate` rule), `ClosePanel` hides the
/// widget's own panel (on the first panel the front-end decides), `Minimize` minimizes the panel
/// group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowCommand {
    TogglePanel(PanelId),
    SwapPanels(PanelId, PanelId),
    ClosePanel,
    Minimize,
}

/// A slider value v in `0.0..=1.0`, mapped by the front-end. `Position`: v = position_ms /
/// duration_ms, release dispatches `Seek { position_ms: round(v * duration_ms) }`; with nothing
/// current or duration 0 the slider shows v = 0 and does nothing. `Volume`: v = volume / 100,
/// dispatch `SetVolume(round(v * 100))`. `Balance`: v = (balance + 100) / 200 (0.0 full left,
/// 0.5 centre, 1.0 full right), dispatch `SetBalance(round(v * 200) - 100)`. `EqPreamp` and
/// `EqBand(b)`: v = (dB + 12) / 24 (0.0 is -12 dB, 0.5 is 0 dB, 1.0 is +12 dB), dispatch
/// `SetEqPreamp` / `SetEqBand { index: b, .. }`. `b` is below the engine's band count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Value {
    Position,
    Volume,
    Balance,
    EqPreamp,
    EqBand(u8),
}

/// A boolean the front-end reads. `Shuffle` = state.shuffle; `Repeat` = repeat is not `Off`;
/// `EqEnabled` = eq.enabled; `PanelVisible(id)` and `PanelFocused` (the widget's own panel) are
/// front-end state; `Stereo` = the source reports 2 or more channels; `Mono` = it reports
/// exactly 1 (both false when unknown).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Flag {
    Shuffle,
    Repeat,
    EqEnabled,
    PanelVisible(PanelId),
    PanelFocused,
    Stereo,
    Mono,
}

/// What an `Indicator` reads: a [`Flag`], or the play state (see `WidgetKind::Indicator` for
/// the sprite order).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateRead {
    Flag(Flag),
    PlayState,
}

/// What a `Text` widget shows. `TrackTitle` = "artist - title" of the current track, empty when
/// none; `BitrateKbps` = the source's nominal kbps in decimal, empty when unknown;
/// `SampleRateKhz` = the source rate in kHz rounded to an integer, empty when unknown;
/// `Time(digit)` = one digit of the elapsed time as mm:ss (minutes modulo 100), blank when
/// stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextRead {
    TrackTitle,
    BitrateKbps,
    SampleRateKhz,
    Time(TimeDigit),
}

/// One digit of the elapsed time as mm:ss.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeDigit {
    MinutesTens,
    MinutesUnits,
    SecondsTens,
    SecondsUnits,
}

/// A bitmap font: one sprite per character, all cells of one size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BitmapFont {
    /// The size of one glyph cell; non-zero.
    pub cell: Size,
    pub glyphs: Vec<(char, SpriteRect)>,
    /// The character drawn for a character the font lacks; nothing when `None`.
    pub fallback: Option<char>,
}

/// Colours for the spectrum widget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisPalette {
    pub background: Rgb,
    pub grid: Rgb,
    /// Bar colours from the top down: `bars[0]` is the top colour.
    pub bars: Vec<Rgb>,
    pub scope: Vec<Rgb>,
    pub peak: Rgb,
}

/// Colours and a font name for lists, as far as the skin sets them; `None` leaves the
/// front-end's own.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SkinTheme {
    pub list_text: Option<Rgb>,
    pub list_text_current: Option<Rgb>,
    pub list_background: Option<Rgb>,
    pub list_selection: Option<Rgb>,
    /// A font name the skin asks for, at most 64 characters; the front-end may ignore it.
    pub list_font: Option<String>,
}
#[cfg(test)]
mod tests {
    use super::*;

    fn size(w: u32, h: u32) -> Size {
        Size { w, h }
    }

    fn rect(x: u32, y: u32, w: u32, h: u32) -> Rect {
        Rect { x, y, w, h }
    }

    fn at(rect: Rect, h: Align, v: Align) -> Widget {
        Widget {
            rect,
            anchor: Anchor { h, v },
            kind: WidgetKind::DragArea,
        }
    }

    const ALIGNS: [Align; 4] = [Align::Start, Align::End, Align::Center, Align::Stretch];

    #[test]
    fn rect_at_the_base_size_is_the_rect_for_every_anchor() {
        let r = rect(10, 20, 30, 40);
        for h in ALIGNS {
            for v in ALIGNS {
                assert_eq!(
                    at(r, h, v).rect_at(size(100, 100), size(100, 100)),
                    r,
                    "{h:?} {v:?}"
                );
            }
        }
    }

    #[test]
    fn rect_at_follows_each_align_on_a_grow() {
        let r = rect(10, 20, 30, 40);
        // dw = 50, dh = 29 (odd: Center adds 14).
        let got = |h, v| at(r, h, v).rect_at(size(100, 100), size(150, 129));
        assert_eq!(got(Align::Start, Align::Start), r);
        assert_eq!(got(Align::End, Align::End), rect(60, 49, 30, 40));
        assert_eq!(got(Align::Center, Align::Center), rect(35, 34, 30, 40));
        assert_eq!(got(Align::Stretch, Align::Stretch), rect(10, 20, 80, 69));
        // The axes are independent.
        assert_eq!(got(Align::End, Align::Stretch), rect(60, 20, 30, 69));
    }

    #[test]
    fn rect_at_follows_each_align_on_a_shrink() {
        let r = rect(10, 20, 30, 40);
        // dw = -10, dh = -9 (odd: Center adds -5, rounding toward negative infinity).
        let got = |h, v| at(r, h, v).rect_at(size(100, 100), size(90, 91));
        assert_eq!(got(Align::Start, Align::Start), r);
        assert_eq!(got(Align::End, Align::End), rect(0, 11, 30, 40));
        assert_eq!(got(Align::Center, Align::Center), rect(5, 15, 30, 40));
        assert_eq!(got(Align::Stretch, Align::Stretch), rect(10, 20, 20, 31));
    }

    #[test]
    fn rect_at_clips_to_the_panel_and_floors_a_stretch_at_zero() {
        let r = rect(10, 0, 30, 10);
        // Stretched to nothing and below.
        let w = at(r, Align::Stretch, Align::Start);
        assert_eq!(w.rect_at(size(100, 10), size(70, 10)).w, 0);
        assert_eq!(w.rect_at(size(100, 10), size(20, 10)).w, 0);
        // Pushed fully outside: a width of 0, the origin on the edge.
        let out = at(r, Align::End, Align::Start).rect_at(size(100, 10), size(5, 10));
        assert_eq!((out.x, out.w), (0, 0));
        // Partly outside: clipped at the right edge.
        let part = at(r, Align::Start, Align::Start).rect_at(size(100, 10), size(25, 10));
        assert_eq!((part.x, part.w), (10, 15));
        // The panel shrunk to nothing.
        let none = at(r, Align::End, Align::End).rect_at(size(100, 10), size(0, 0));
        assert_eq!(none, rect(0, 0, 0, 0));
    }

    #[test]
    fn rect_at_survives_extreme_numbers() {
        let r = rect(u32::MAX, u32::MAX, u32::MAX, u32::MAX);
        for h in ALIGNS {
            for v in ALIGNS {
                let got = at(r, h, v).rect_at(size(0, u32::MAX), size(u32::MAX, 0));
                assert_eq!(got.h, 0, "{h:?} {v:?}");
            }
        }
    }

    fn playlist() -> Resize {
        Resize {
            min: size(275, 116),
            step: size(25, 29),
        }
    }

    #[test]
    fn snap_rounds_to_the_nearest_step_half_up() {
        let (r, base, max) = (playlist(), size(275, 116), size(4096, 4096));
        assert_eq!(r.snap(base, size(300, 145), max), size(300, 145));
        assert_eq!(r.snap(base, size(288, 131), max), size(300, 145));
        assert_eq!(r.snap(base, size(287, 130), max), size(275, 116));
        assert_eq!(r.snap(base, size(275, 116), max), size(275, 116));
    }

    #[test]
    fn snap_clamps_to_the_minimum_and_the_maximum() {
        let (r, base) = (playlist(), size(275, 116));
        let big = size(4096, 4096);
        assert_eq!(r.snap(base, size(100, 50), big), size(275, 116));
        assert_eq!(r.snap(base, size(0, 0), big), size(275, 116));
        assert_eq!(
            r.snap(base, size(1000, 1000), size(400, 200)),
            size(400, 174)
        );
        // A maximum below the minimum gives the minimum; the side cap holds too.
        assert_eq!(r.snap(base, size(1000, 1000), size(10, 10)), size(275, 116));
        let max = size(u32::MAX, u32::MAX);
        let s = r.snap(base, max, max);
        assert!(s.w <= MAX_PANEL_SIDE && s.h <= MAX_PANEL_SIDE);
        assert_eq!((s.w - 275) % 25, 0);
        assert_eq!((s.h - 116) % 29, 0);
        assert!(s.w > MAX_PANEL_SIDE - 25 && s.h > MAX_PANEL_SIDE - 29);
    }

    #[test]
    fn snap_keeps_base_on_a_fixed_axis() {
        let big = size(4096, 4096);
        let r = Resize {
            min: size(275, 116),
            step: size(25, 0),
        };
        assert_eq!(r.snap(size(275, 150), size(330, 500), big), size(325, 150));
        let r = Resize {
            min: size(275, 116),
            step: size(0, 29),
        };
        assert_eq!(r.snap(size(280, 116), size(330, 150), big), size(280, 145));
    }
}
