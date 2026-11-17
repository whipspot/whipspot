//! Composed list panels: a playlist (`TrackList`) or library (`Library`) window an engine adds when
//! its format has no such window of its own. The panel is a plain tiled background and title bar
//! drawn from one small chrome sheet ([`chrome_image`]) in the skin's colours, a drag bar, and a
//! close button and a toggle button on the bar.

use whipspot_core::skin::{
    Align, Anchor, Attach, Command, Dock, Fill, ImageId, Panel, PanelId, Rect, Resize, Rgb, Shape,
    Size, SkinImage, SpriteRect, Widget, WidgetKind, WindowCommand,
};

/// The height of the title bar, in pixels.
pub const BAR_H: u32 = 12;
/// The side of a bar button, in pixels.
pub const BUTTON: u32 = 12;

/// The chrome sheet: `BG` (8 x 8) at x 0, `BAR` (8 x 12) at x 8, then four 12 x 12 button glyphs
/// at x 16, 28, 40, 52. The sheet is 64 x 12.
const BG: Rect = Rect {
    x: 0,
    y: 0,
    w: 8,
    h: 8,
};
const BAR: Rect = Rect {
    x: 8,
    y: 0,
    w: 8,
    h: BAR_H,
};
const CLOSE: [Rect; 2] = [button_cell(16), button_cell(28)];
const TOGGLE: [Rect; 2] = [button_cell(40), button_cell(52)];

const fn button_cell(x: u32) -> Rect {
    Rect {
        x,
        y: 0,
        w: BUTTON,
        h: BUTTON,
    }
}

/// The colours of a composed panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListStyle {
    pub background: Rgb,
    pub bar: Rgb,
    pub text: Rgb,
}

/// Which list a [`ListPanel`] shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListKind {
    Tracks,
    Library,
}

/// What [`list_panel`] builds a panel from. `title` must already be clean display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListPanel {
    pub id: PanelId,
    pub title: String,
    pub kind: ListKind,
    pub size: Size,
    pub attach: Option<Attach>,
    pub group: Option<String>,
    pub visible_at_start: bool,
    /// A close button on the bar.
    pub close: bool,
    /// A button on the bar that toggles this panel (the other list panel).
    pub toggles: Option<PanelId>,
}

/// The chrome sheet for [`list_panel`], 64 x 12 RGBA, drawn in `style`'s colours: a flat
/// background cell, a flat bar cell, and for the close and the toggle button a normal and a
/// pressed glyph (pressed swaps the glyph and bar colours).
pub fn chrome_image(style: &ListStyle) -> SkinImage {
    const W: u32 = 64;
    const H: u32 = BAR_H;
    let mut px = vec![[0u8; 4]; (W * H) as usize];
    let rgb = |c: Rgb| [c.r, c.g, c.b, 255];
    let fill = |px: &mut Vec<[u8; 4]>, r: Rect, c: [u8; 4]| {
        for y in r.y..r.y + r.h {
            for x in r.x..r.x + r.w {
                px[(y * W + x) as usize] = c;
            }
        }
    };
    fill(&mut px, BG, rgb(style.background));
    fill(&mut px, BAR, rgb(style.bar));
    for (i, cell) in CLOSE.iter().chain(&TOGGLE).enumerate() {
        let pressed = i % 2 == 1;
        let (back, glyph) = if pressed {
            (style.text, style.bar)
        } else {
            (style.bar, style.text)
        };
        fill(&mut px, *cell, rgb(back));
        let mut set =
            |dx: u32, dy: u32| px[((cell.y + dy) * W + cell.x + dx) as usize] = rgb(glyph);
        if i < 2 {
            // A cross.
            for k in 3..9 {
                set(k, k);
                set(k, 11 - k);
            }
        } else {
            // Three list lines.
            for dy in [3, 6, 9] {
                for dx in 2..10 {
                    set(dx, dy);
                }
            }
        }
    }
    let rgba: Vec<u8> = px.into_iter().flatten().collect();
    SkinImage::new(W, H, rgba)
        .unwrap_or_else(|| SkinImage::new(1, 1, vec![0; 4]).expect("a 1 x 1 image is always valid"))
}

const fn anchor(h: Align, v: Align) -> Anchor {
    Anchor { h, v }
}

fn sprite(image: ImageId, rect: Rect) -> SpriteRect {
    SpriteRect { image, rect }
}

/// The panel for `spec`, whose chrome sheet is `chrome` (the image from [`chrome_image`]).
/// `Shape::Rect`, resizable from `spec.size`; every widget rect lies inside `spec.size` (a size
/// under 24 x 24 clamps the bar and the buttons to it, never past it).
pub fn list_panel(spec: ListPanel, chrome: ImageId) -> Panel {
    let Size { w, h } = spec.size;
    let bar_h = BAR_H.min(h);
    let btn = |slot: u32| -> Rect {
        // The right-most button is slot 0. A button that does not fit is squeezed to what is left.
        let right = w.saturating_sub(slot * BUTTON);
        let x = right.saturating_sub(BUTTON);
        Rect {
            x,
            y: 0,
            w: right - x,
            h: bar_h,
        }
    };
    let mut widgets = vec![
        Widget {
            rect: Rect { x: 0, y: 0, w, h },
            anchor: anchor(Align::Stretch, Align::Stretch),
            kind: WidgetKind::Image {
                sprite: sprite(chrome, BG),
                fill: Fill::Tile,
            },
        },
        Widget {
            rect: Rect {
                x: 0,
                y: 0,
                w,
                h: bar_h,
            },
            anchor: anchor(Align::Stretch, Align::Start),
            kind: WidgetKind::Image {
                sprite: sprite(chrome, BAR),
                fill: Fill::Tile,
            },
        },
        Widget {
            rect: Rect {
                x: 0,
                y: bar_h,
                w,
                h: h - bar_h,
            },
            anchor: anchor(Align::Stretch, Align::Stretch),
            kind: match spec.kind {
                ListKind::Tracks => WidgetKind::TrackList,
                ListKind::Library => WidgetKind::Library,
            },
        },
        Widget {
            rect: Rect {
                x: 0,
                y: 0,
                w,
                h: bar_h,
            },
            anchor: anchor(Align::Stretch, Align::Start),
            kind: WidgetKind::DragArea,
        },
    ];
    let bar_button = |rect_slot: u32, command: WindowCommand, cells: [Rect; 2]| Widget {
        rect: btn(rect_slot),
        anchor: anchor(Align::End, Align::Start),
        kind: WidgetKind::Button {
            command: Command::Window(command),
            normal: sprite(chrome, cells[0]),
            pressed: sprite(chrome, cells[1]),
        },
    };
    // The close button is right-most; the toggle sits left of it, or at the end without one.
    let toggle_slot = u32::from(spec.close);
    if let Some(target) = spec.toggles {
        widgets.push(bar_button(
            toggle_slot,
            WindowCommand::TogglePanel(target),
            TOGGLE,
        ));
    }
    if spec.close {
        widgets.push(bar_button(0, WindowCommand::ClosePanel, CLOSE));
    }
    Panel {
        id: spec.id,
        title: spec.title,
        size: spec.size,
        resize: Some(Resize {
            min: spec.size,
            step: Size { w: 1, h: 1 },
        }),
        shape: Shape::Rect,
        dock: Dock {
            group: spec.group,
            attach: spec.attach,
            snap: true,
        },
        visible_at_start: spec.visible_at_start,
        widgets,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use whipspot_core::skin::{Edge, MAX_WIDGETS_PER_PANEL, Skin, SkinInfo, SkinTheme};

    fn style() -> ListStyle {
        ListStyle {
            background: Rgb { r: 1, g: 2, b: 3 },
            bar: Rgb {
                r: 40,
                g: 50,
                b: 60,
            },
            text: Rgb {
                r: 200,
                g: 210,
                b: 220,
            },
        }
    }

    fn id(s: &str) -> PanelId {
        PanelId::new(s).unwrap()
    }

    /// A one-image skin: image 0 is the 1 x 1 stand-in the minimal main panel needs none of, so
    /// the chrome is image 0.
    fn skin_with(panels: Vec<Panel>) -> Skin {
        Skin {
            info: SkinInfo {
                engine: "test".to_owned(),
                name: "t".to_owned(),
            },
            panels,
            images: vec![chrome_image(&style())],
            fonts: vec![],
            visualiser: None,
            theme: SkinTheme::default(),
        }
    }

    fn main_panel() -> Panel {
        Panel {
            id: id("main"),
            title: "Main".to_owned(),
            size: Size { w: 100, h: 50 },
            resize: None,
            shape: Shape::Rect,
            dock: Dock::default(),
            visible_at_start: true,
            widgets: vec![],
        }
    }

    fn spec(kind: ListKind, name: &str, close: bool, toggles: Option<&str>) -> ListPanel {
        ListPanel {
            id: id(name),
            title: name.to_owned(),
            kind,
            size: Size { w: 120, h: 80 },
            attach: Some(Attach {
                to: id("main"),
                edge: Edge::Bottom,
            }),
            group: Some("main".to_owned()),
            visible_at_start: kind == ListKind::Tracks,
            close,
            toggles: toggles.map(id),
        }
    }

    #[test]
    fn the_chrome_sheet_has_every_cell_inside_it() {
        let img = chrome_image(&style());
        assert_eq!((img.width(), img.height()), (64, 12));
        for r in [BG, BAR, CLOSE[0], CLOSE[1], TOGGLE[0], TOGGLE[1]] {
            assert!(r.x + r.w <= 64 && r.y + r.h <= 12);
        }
        assert_eq!(&img.rgba()[..4], &[1, 2, 3, 255]);
    }

    #[test]
    fn both_list_panels_pass_validation() {
        let skin = skin_with(vec![
            main_panel(),
            list_panel(
                spec(ListKind::Tracks, "playlist", true, Some("library")),
                ImageId(0),
            ),
            list_panel(
                spec(ListKind::Library, "library", true, Some("playlist")),
                ImageId(0),
            ),
        ]);
        assert_eq!(skin.validate(), Ok(()));
    }

    #[test]
    fn widgets_follow_the_contract_order_and_stay_inside() {
        let p = list_panel(
            spec(ListKind::Tracks, "playlist", true, Some("library")),
            ImageId(0),
        );
        let kinds: Vec<&str> = p
            .widgets
            .iter()
            .map(|w| match &w.kind {
                WidgetKind::Image { .. } => "image",
                WidgetKind::TrackList => "tracks",
                WidgetKind::Library => "library",
                WidgetKind::DragArea => "drag",
                WidgetKind::Button { command, .. } => match command {
                    Command::Window(WindowCommand::TogglePanel(_)) => "toggle",
                    Command::Window(WindowCommand::ClosePanel) => "close",
                    _ => "other",
                },
                _ => "?",
            })
            .collect();
        assert_eq!(
            kinds,
            ["image", "image", "tracks", "drag", "toggle", "close"]
        );
        assert_eq!(p.shape, Shape::Rect);
        assert_eq!(p.resize.unwrap().min, Size { w: 120, h: 80 });
        for w in &p.widgets {
            assert!(w.rect.x + w.rect.w <= 120 && w.rect.y + w.rect.h <= 80);
        }
        // Close is right-most, the toggle just left of it, both on the bar.
        let x_of = |k: usize| p.widgets[k].rect.x;
        assert_eq!(x_of(5), 120 - BUTTON);
        assert_eq!(x_of(4), 120 - 2 * BUTTON);
        assert_eq!(p.widgets[2].rect.y, BAR_H);
    }

    #[test]
    fn without_close_there_is_no_close_widget_and_the_toggle_takes_the_end() {
        let p = list_panel(
            spec(ListKind::Tracks, "playlist", false, Some("library")),
            ImageId(0),
        );
        let has_close = p.widgets.iter().any(|w| {
            matches!(
                &w.kind,
                WidgetKind::Button {
                    command: Command::Window(WindowCommand::ClosePanel),
                    ..
                }
            )
        });
        assert!(!has_close);
        assert_eq!(p.widgets.last().unwrap().rect.x, 120 - BUTTON);
    }

    #[test]
    fn a_tiny_size_stays_inside_and_validates() {
        let mut s = spec(ListKind::Library, "library", true, Some("playlist"));
        s.size = Size { w: 5, h: 3 };
        let p = list_panel(s, ImageId(0));
        assert!(p.widgets.len() <= MAX_WIDGETS_PER_PANEL);
        for w in &p.widgets {
            assert!(w.rect.x + w.rect.w <= 5 && w.rect.y + w.rect.h <= 3);
        }
        let skin = skin_with(vec![
            main_panel(),
            p,
            list_panel(spec(ListKind::Tracks, "playlist", true, None), ImageId(0)),
        ]);
        assert_eq!(skin.validate(), Ok(()));
    }
}
