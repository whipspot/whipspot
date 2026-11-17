//! Drawing one skin panel: every widget in paint order, each sprite nearest-sampled into a whole
//! number of physical pixels per skin pixel (D4). One skin pixel is `s = k / scale` logical
//! pixels, `k` the panel set's pixel scale and `scale` the frame's, so with the window sized
//! `size * k` physical pixels every edge lands on a pixel boundary.
//!
//! The geometry (`dest`, `crop`) and the colour choices are plain functions with tests; the
//! painting calls are thin.

use std::time::Duration;

use lune_ui::color::premul;
use lune_ui::image::{ImageDraw, ImageId as LuneImage, ImagePart, Sampling};
use lune_ui::paint::Rect as LRect;
use lune_ui::theme::Theme;
use lune_ui::ui::Ui;
use whipspot_core::diagnostics::SourceDiagnostics;
use whipspot_core::engine::BAND_COUNT;
use whipspot_core::player::PlayerState;
use whipspot_core::skin::{
    Fill, Panel, Rect, Rgb, Size, Skin, SpriteRect, StateRead, TextRead, Widget, WidgetKind,
};

use super::bind::{self, FrontState};
use super::input::PanelInput;
use super::lists::{self, Lists, RowsGeom};
use super::text;
use super::vis::{self, Colours, VisState};
use crate::library::Access;

/// The logical rect of a `w` by `h` skin-pixel block at skin position (`x`, `y`).
pub(super) fn dest(s: f32, x: i64, y: i64, w: u32, h: u32) -> LRect {
    LRect::new(x as f32 * s, y as f32 * s, w as f32 * s, h as f32 * s)
}

/// A clip in skin pixels: `x0..x1` by `y0..y1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Clip {
    pub x0: i64,
    pub y0: i64,
    pub x1: i64,
    pub y1: i64,
}

impl Clip {
    pub fn of_rect(r: Rect) -> Clip {
        Clip {
            x0: i64::from(r.x),
            y0: i64::from(r.y),
            x1: i64::from(r.x) + i64::from(r.w),
            y1: i64::from(r.y) + i64::from(r.h),
        }
    }

    pub fn of_panel(size: (u32, u32)) -> Clip {
        Clip {
            x0: 0,
            y0: 0,
            x1: i64::from(size.0),
            y1: i64::from(size.1),
        }
    }
}

/// The part of a sprite rect `part` drawn at skin position (`x`, `y`) that lies inside `clip`:
/// the cropped part (relative to the same image) and where it goes. `None` when nothing is left.
pub(super) fn crop(part: Rect, x: i64, y: i64, clip: Clip) -> Option<(Rect, i64, i64)> {
    let (w, h) = (i64::from(part.w), i64::from(part.h));
    let (nx0, ny0) = (x.max(clip.x0), y.max(clip.y0));
    let (nx1, ny1) = ((x + w).min(clip.x1), (y + h).min(clip.y1));
    if nx1 <= nx0 || ny1 <= ny0 {
        return None;
    }
    Some((
        Rect {
            x: part.x + (nx0 - x) as u32,
            y: part.y + (ny0 - y) as u32,
            w: (nx1 - nx0) as u32,
            h: (ny1 - ny0) as u32,
        },
        nx0,
        ny0,
    ))
}

fn rgba(c: Rgb) -> [u8; 4] {
    [c.r, c.g, c.b, 255]
}

fn rgb_of(c: [u8; 4]) -> Rgb {
    Rgb {
        r: c[0],
        g: c[1],
        b: c[2],
    }
}

/// How long each text widget has shown its current text: scrolling starts from the beginning when
/// the text changes.
#[derive(Default)]
pub(super) struct Scrolls(Vec<(usize, String, Duration)>);

impl Scrolls {
    fn elapsed(&mut self, widget: usize, text: &str, now: Duration) -> Duration {
        match self.0.iter_mut().find(|e| e.0 == widget) {
            Some(e) => {
                if e.1 != text {
                    e.1 = text.to_owned();
                    e.2 = now;
                }
                now.saturating_sub(e.2)
            }
            None => {
                self.0.push((widget, text.to_owned(), now));
                Duration::ZERO
            }
        }
    }
}

/// What a panel is drawn from.
pub(super) struct Ctx<'a> {
    pub skin: &'a Skin,
    pub panel: usize,
    /// The toolkit image per skin image; `None` when it could not be registered.
    pub ids: &'a [Option<LuneImage>],
    /// Logical pixels per skin pixel.
    pub s: f32,
    /// The panel's size now in skin pixels (the base size unless it was resized).
    pub size: (u32, u32),
    pub state: &'a PlayerState,
    pub source: &'a SourceDiagnostics,
    pub visible: &'a [bool],
    pub focused: bool,
    pub input: &'a PanelInput,
    /// A text that replaces the track title (an error, the hint).
    pub title: Option<&'a str>,
    pub now: Duration,
    pub vis: &'a VisState,
    pub theme: Theme,
    /// The library provider the user chose (D11); `None` is the first with a library.
    pub selected: Option<&'a str>,
}

/// What drawing found out.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Drawn {
    /// Something on the panel moves by itself (scrolling text, a moving spectrum) or is held.
    pub animating: bool,
    /// The panel has a visualiser.
    pub has_vis: bool,
}

fn fill(ui: &mut Ui<'_>, s: f32, x: i64, y: i64, w: u32, h: u32, c: Rgb) {
    ui.painter()
        .fill_over(dest(s, x, y, w, h), 0.0, premul(rgba(c)));
}

fn blit(ui: &mut Ui<'_>, ctx: &Ctx<'_>, sprite: SpriteRect, x: i64, y: i64, clip: Clip) {
    let Some(Some(id)) = ctx.ids.get(usize::from(sprite.image.0)) else {
        return;
    };
    let Some((part, x, y)) = crop(sprite.rect, x, y, clip) else {
        return;
    };
    let draw = ImageDraw {
        part: Some(ImagePart {
            x: part.x,
            y: part.y,
            w: part.w,
            h: part.h,
        }),
        sampling: Sampling::Nearest,
        ..ImageDraw::default()
    };
    ui.painter()
        .image_with(dest(ctx.s, x, y, part.w, part.h), *id, &draw);
}

/// Draws every widget of the panel in paint order.
pub(super) fn draw_panel(
    ui: &mut Ui<'_>,
    ctx: &Ctx<'_>,
    scrolls: &mut Scrolls,
    lists: &mut Lists,
) -> Drawn {
    let Some(panel) = ctx.skin.panels.get(ctx.panel) else {
        return Drawn::default();
    };
    let mut out = Drawn {
        animating: ctx.input.held(),
        has_vis: false,
    };
    let front = FrontState {
        skin: ctx.skin,
        visible: ctx.visible,
        focused: ctx.focused,
    };
    // Every rect is resolved once, at the panel's size now.
    let size = Size {
        w: ctx.size.0,
        h: ctx.size.1,
    };
    let rects: Vec<Rect> = panel
        .widgets
        .iter()
        .map(|w| w.rect_at(panel.size, size))
        .collect();
    // The scroll bar, when the panel has one: its list then scrolls in whole rows.
    let bar = panel
        .widgets
        .iter()
        .enumerate()
        .find_map(|(i, w)| match &w.kind {
            WidgetKind::ListScroll { thumb } => Some((rects[i], thumb[0].rect.h)),
            _ => None,
        });
    for (i, w) in panel.widgets.iter().enumerate() {
        let geo = Geo { r: rects[i], bar };
        draw_widget(
            ui,
            ctx,
            (panel, i, w),
            geo,
            &front,
            (scrolls, lists),
            &mut out,
        );
    }
    out
}

/// A widget's rect at the panel's size now, and the panel's scroll bar (its rect and its thumb's
/// height) if it has one.
#[derive(Clone, Copy)]
struct Geo {
    r: Rect,
    bar: Option<(Rect, u32)>,
}

impl Geo {
    /// What a list widget under this scroll bar needs.
    fn rows(&self) -> Option<RowsGeom> {
        self.bar.map(|(bar, thumb_h)| RowsGeom {
            bar,
            thumb_h,
            list_h: self.r.h,
        })
    }
}

fn draw_widget(
    ui: &mut Ui<'_>,
    ctx: &Ctx<'_>,
    (panel, i, w): (&Panel, usize, &Widget),
    geo: Geo,
    front: &FrontState<'_>,
    (scrolls, lists): (&mut Scrolls, &mut Lists),
    out: &mut Drawn,
) {
    let r = geo.r;
    let (rx, ry) = (i64::from(r.x), i64::from(r.y));
    let own = Clip::of_rect(r);
    match &w.kind {
        WidgetKind::Image { sprite, fill } => match fill {
            Fill::Once => blit(ui, ctx, *sprite, rx, ry, own),
            Fill::Tile => {
                let (sw, sh) = (sprite.rect.w.max(1), sprite.rect.h.max(1));
                for ty in (0..r.h).step_by(sh as usize) {
                    for tx in (0..r.w).step_by(sw as usize) {
                        blit(
                            ui,
                            ctx,
                            *sprite,
                            rx + i64::from(tx),
                            ry + i64::from(ty),
                            own,
                        );
                    }
                }
            }
        },
        WidgetKind::Indicator { state, sprites } => {
            let index = match state {
                StateRead::Flag(f) => usize::from(bind::flag(f, ctx.state, ctx.source, front)),
                StateRead::PlayState => bind::play_frame(ctx.state.play_state),
            };
            if let Some(s) = sprites.get(index) {
                blit(ui, ctx, *s, rx, ry, own);
            }
        }
        WidgetKind::Button {
            normal, pressed, ..
        } => {
            let s = if ctx.input.pressed(panel, i) {
                pressed
            } else {
                normal
            };
            blit(ui, ctx, *s, rx, ry, own);
        }
        WidgetKind::Toggle { state, off, on, .. } => {
            let set = if bind::flag(state, ctx.state, ctx.source, front) {
                on
            } else {
                off
            };
            let s = set[usize::from(ctx.input.pressed(panel, i))];
            blit(ui, ctx, s, rx, ry, own);
        }
        WidgetKind::Slider {
            value,
            axis,
            frames,
            frame_rule,
            thumb,
        } => {
            let v = ctx
                .input
                .dragged_value(i)
                .unwrap_or_else(|| bind::value_of(*value, ctx.state));
            if let Some(f) = frames.get(bind::frame_index(*frame_rule, v, frames.len())) {
                blit(ui, ctx, *f, rx, ry, own);
            }
            if let Some(t) = thumb {
                let sprite = t[usize::from(ctx.input.held_widget(i))];
                let (tw, th) = (sprite.rect.w, sprite.rect.h);
                let (x, y) = match axis {
                    whipspot_core::skin::Axis::Horizontal => (
                        rx + i64::from(bind::thumb_start(*axis, v, r.w, tw)),
                        ry + (i64::from(r.h) - i64::from(th)) / 2,
                    ),
                    whipspot_core::skin::Axis::Vertical => (
                        rx + (i64::from(r.w) - i64::from(tw)) / 2,
                        ry + i64::from(bind::thumb_start(*axis, v, r.h, th)),
                    ),
                };
                blit(ui, ctx, sprite, x, y, Clip::of_panel(ctx.size));
            }
        }
        WidgetKind::Text {
            text: read,
            font,
            scroll,
        } => {
            let Some(font) = ctx.skin.fonts.get(usize::from(font.0)) else {
                return;
            };
            let string = match (read, ctx.title) {
                (TextRead::TrackTitle, Some(t)) => t.to_owned(),
                _ => bind::text(*read, ctx.state, ctx.source),
            };
            let t = scrolls.elapsed(i, &string, ctx.now);
            let width = string.chars().count() as u64 * u64::from(font.cell.w);
            if *scroll && width > u64::from(r.w) {
                out.animating = true;
            }
            // The title starts at the left edge; the numbers sit against the right one.
            let fit = match read {
                TextRead::TrackTitle => text::Fit::Left,
                _ => text::Fit::Right,
            };
            for (x, sprite) in text::layout(font, &string, r.w, fit, *scroll, t) {
                blit(ui, ctx, sprite, rx + i64::from(x), ry, own);
            }
        }
        WidgetKind::EqCurve { colours } => draw_eq_curve(ui, ctx, r, *colours),
        WidgetKind::Visualiser => {
            out.has_vis = true;
            if ctx.vis.moving() {
                out.animating = true;
            }
            let colours = match &ctx.skin.visualiser {
                Some(p) => Colours::palette(p),
                None => Colours::theme(
                    rgb_of(ctx.theme.surface_input),
                    rgb_of(ctx.theme.accent),
                    rgb_of(ctx.theme.fg_primary),
                ),
            };
            fill(ui, ctx.s, rx, ry, r.w, r.h, colours.background);
            let (levels, caps) = ctx.vis.bars(r.w);
            for c in vis::cells(r.h, &levels, &caps, &colours) {
                fill(
                    ui,
                    ctx.s,
                    rx + i64::from(c.x),
                    ry + i64::from(c.y),
                    c.w,
                    c.h,
                    c.colour,
                );
            }
        }
        WidgetKind::TrackList => {
            let th = lists::list_theme(&ctx.skin.theme);
            let at = dest(ctx.s, rx, ry, r.w, r.h);
            lists.track_list(
                ui,
                (ctx.panel, i),
                (at, ctx.s),
                &th,
                &ctx.state.queue,
                geo.rows(),
            );
        }
        WidgetKind::Library => {
            let th = lists::list_theme(&ctx.skin.theme);
            let at = dest(ctx.s, rx, ry, r.w, r.h);
            let access = Access::of(&ctx.state.providers, ctx.selected);
            lists.library(ui, access, (ctx.panel, i), (at, ctx.s), &th, geo.rows());
        }
        WidgetKind::ListScroll { thumb } => {
            // The thumb sits at the list's scroll position, centred across the bar; it shows its
            // pressed sprite while held.
            let sprite = thumb[usize::from(ctx.input.held_widget(i))];
            let x = rx + (i64::from(r.w) - i64::from(sprite.rect.w)).div_euclid(2);
            let y = lists::thumb_y(r.y, r.h, thumb[0].rect.h, lists.scroll_fraction(ctx.panel));
            blit(ui, ctx, sprite, x, y, own);
        }
        // The grip gets its behaviour in a later step.
        WidgetKind::DragArea | WidgetKind::ResizeArea => {}
    }
}

/// The row of the equalizer line at column `col` of `cols`: the bands linearly interpolated
/// across the width, +12 dB at row 0 and -12 dB at the last row.
pub(super) fn curve_row(bands: &[f32], col: u32, cols: u32, rows: u32) -> u32 {
    let last = bands.len().saturating_sub(1);
    let pos = if cols > 1 {
        col as f32 * last as f32 / (cols - 1) as f32
    } else {
        0.0
    };
    let lo = (pos.floor() as usize).min(last);
    let hi = (lo + 1).min(last);
    let at = |i: usize| {
        bands
            .get(i)
            .copied()
            .filter(|d| d.is_finite())
            .unwrap_or(0.0)
    };
    let db = (at(lo) + (at(hi) - at(lo)) * (pos - lo as f32)).clamp(-12.0, 12.0);
    let span = rows.saturating_sub(1) as f32;
    (((12.0 - db) * span / 24.0).round() as u32).min(rows.saturating_sub(1))
}

fn draw_eq_curve(ui: &mut Ui<'_>, ctx: &Ctx<'_>, r: Rect, strip: Option<SpriteRect>) {
    if r.w == 0 || r.h == 0 {
        return;
    }
    let bands: [f32; BAND_COUNT] = ctx.state.eq.bands_db;
    let fallback = ctx
        .skin
        .theme
        .list_text
        .unwrap_or_else(|| rgb_of(ctx.theme.fg_primary));
    let mut prev: Option<u32> = None;
    for col in 0..r.w {
        let row = curve_row(&bands, col, r.w, r.h);
        let (top, bottom) = match prev {
            Some(p) => (p.min(row), p.max(row)),
            None => (row, row),
        };
        prev = Some(row);
        let colour = strip
            .and_then(|s| strip_colour(ctx.skin, s, row))
            .unwrap_or(fallback);
        fill(
            ui,
            ctx.s,
            i64::from(r.x) + i64::from(col),
            i64::from(r.y) + i64::from(top),
            1,
            bottom - top + 1,
            colour,
        );
    }
}

/// The colour at `row` of the 1-pixel-wide strip `s` (its last row for a row beyond it).
fn strip_colour(skin: &Skin, s: SpriteRect, row: u32) -> Option<Rgb> {
    let image = skin.images.get(usize::from(s.image.0))?;
    let row = row.min(s.rect.h.checked_sub(1)?);
    let (x, y) = (s.rect.x, s.rect.y + row);
    if x >= image.width() || y >= image.height() {
        return None;
    }
    let at = (y as usize * image.width() as usize + x as usize) * 4;
    let px = image.rgba().get(at..at + 3)?;
    Some(Rgb {
        r: px[0],
        g: px[1],
        b: px[2],
    })
}

#[cfg(test)]
mod tests {
    use whipspot_core::skin::ImageId;

    use super::*;

    fn r(x: u32, y: u32, w: u32, h: u32) -> Rect {
        Rect { x, y, w, h }
    }

    #[test]
    fn a_sprite_of_23_by_18_at_k_2_and_scale_1_5_is_46_by_36_physical_px() {
        // s = k / scale = 4/3 logical px per skin px.
        let s = 2.0 / 1.5;
        let d = dest(s, 0, 0, 23, 18);
        assert!((d.w - 23.0 * 4.0 / 3.0).abs() < 1e-4, "{}", d.w);
        assert!((d.h - 24.0).abs() < 1e-4, "{}", d.h);
        // In physical pixels (logical * scale): exactly 46 by 36.
        assert!((d.w * 1.5 - 46.0).abs() < 1e-3);
        assert!((d.h * 1.5 - 36.0).abs() < 1e-3);
    }

    #[test]
    fn positions_scale_like_sizes() {
        let d = dest(2.0, 3, 5, 4, 6);
        assert_eq!((d.x, d.y, d.w, d.h), (6.0, 10.0, 8.0, 12.0));
    }

    #[test]
    fn a_sprite_inside_the_clip_is_not_cropped() {
        let clip = Clip::of_rect(r(0, 0, 100, 100));
        assert_eq!(
            crop(r(10, 20, 5, 6), 30, 40, clip),
            Some((r(10, 20, 5, 6), 30, 40))
        );
    }

    #[test]
    fn a_sprite_past_the_clip_loses_the_outside_part() {
        let clip = Clip::of_rect(r(10, 10, 20, 20));
        // Cut on the left and top: 3 px from the left, 2 from the top.
        let (part, x, y) = crop(r(100, 50, 8, 8), 7, 8, clip).unwrap();
        assert_eq!((x, y), (10, 10));
        assert_eq!(part, r(103, 52, 5, 6));
        // Cut on the right and bottom.
        let (part, x, y) = crop(r(0, 0, 8, 8), 26, 27, clip).unwrap();
        assert_eq!((x, y), (26, 27));
        assert_eq!(part, r(0, 0, 4, 3));
        // Wholly outside.
        assert_eq!(crop(r(0, 0, 4, 4), 30, 10, clip), None);
        assert_eq!(crop(r(0, 0, 4, 4), 6, 10, clip), None);
    }

    #[test]
    fn the_eq_line_runs_from_the_top_at_plus_12_to_the_bottom_at_minus_12() {
        let bands = [12.0, 0.0, -12.0];
        assert_eq!(curve_row(&bands, 0, 11, 19), 0);
        assert_eq!(curve_row(&bands, 5, 11, 19), 9);
        assert_eq!(curve_row(&bands, 10, 11, 19), 18);
        // Linear between bands: a quarter of the way from +12 to 0 is +6 dB.
        assert_eq!(curve_row(&bands, 1, 5, 25), 6);
    }

    #[test]
    fn the_eq_line_of_the_demo_curve_hits_the_set_bands_at_their_columns() {
        let bands = [-12.0, -6.0, 0.0, 6.0, 12.0, 12.0, 6.0, 0.0, -6.0, -12.0];
        // 113 columns, 19 rows: band b at column round(b * 112 / 9).
        for (b, db) in bands.iter().enumerate() {
            let col = (b as f32 * 112.0 / 9.0).round() as u32;
            let want = ((12.0f32 - db) * 18.0 / 24.0).round() as i32;
            let got = curve_row(&bands, col, 113, 19) as i32;
            assert!((got - want).abs() <= 1, "band {b}: {got} vs {want}");
        }
    }

    #[test]
    fn a_text_scrolls_from_its_start_whenever_the_text_changes() {
        let mut s = Scrolls::default();
        let t = |ms| Duration::from_millis(ms);
        assert_eq!(s.elapsed(0, "a", t(1000)), Duration::ZERO);
        assert_eq!(s.elapsed(0, "a", t(1500)), t(500));
        assert_eq!(s.elapsed(0, "b", t(1600)), Duration::ZERO);
        assert_eq!(s.elapsed(1, "a", t(1700)), Duration::ZERO);
        assert_eq!(s.elapsed(0, "b", t(2600)), t(1000));
    }

    #[test]
    fn a_strip_colour_reads_the_row_of_its_pixel_column() {
        use whipspot_core::skin::{SkinImage, SkinInfo, SkinTheme};
        let mut px = vec![0u8; 3 * 4 * 4];
        // Column 1, row 2 is pure red.
        let at = (2 * 3 + 1) * 4;
        px[at..at + 4].copy_from_slice(&[200, 10, 20, 255]);
        let skin = Skin {
            info: SkinInfo {
                engine: String::new(),
                name: String::new(),
            },
            panels: Vec::new(),
            images: vec![SkinImage::new(3, 4, px).unwrap()],
            fonts: Vec::new(),
            visualiser: None,
            theme: SkinTheme::default(),
        };
        let s = SpriteRect {
            image: ImageId(0),
            rect: r(1, 0, 1, 4),
        };
        assert_eq!(
            strip_colour(&skin, s, 2),
            Some(Rgb {
                r: 200,
                g: 10,
                b: 20
            })
        );
        // A row past the strip reads its last row.
        assert_eq!(strip_colour(&skin, s, 9), strip_colour(&skin, s, 3));
    }
}
