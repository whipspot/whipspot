//! `Skin::validate`: the caps and reference checks every skin passes before a front-end sees it.
//! All arithmetic on engine-supplied numbers is checked; nothing here panics on any input.

use super::{
    BitmapFont, Command, Flag, MAX_FONTS, MAX_GLYPHS, MAX_IMAGE_BYTES_TOTAL, MAX_IMAGES,
    MAX_PANEL_SIDE, MAX_PANELS, MAX_POINTS, MAX_POLYGONS, MAX_VIS_COLOURS, MAX_WIDGETS_PER_PANEL,
    Panel, PanelId, Rect, Shape, Size, Skin, SkinError, SpriteRect, StateRead, Value, Widget,
    WidgetKind, WindowCommand,
};
use crate::engine::BAND_COUNT;
use crate::text::display_text;

fn check(ok: bool) -> Result<(), SkinError> {
    if ok {
        Ok(())
    } else {
        Err(SkinError::Malformed)
    }
}

/// `s` is what `display_text` would leave of it and has at most `max` characters.
fn clean(s: &str, max: usize) -> bool {
    display_text(s, max) == s
}

/// `r` lies inside a `w` by `h` area, without wrapping.
fn rect_inside(r: Rect, w: u32, h: u32) -> bool {
    r.x.checked_add(r.w).is_some_and(|end| end <= w)
        && r.y.checked_add(r.h).is_some_and(|end| end <= h)
}

impl Skin {
    /// Checks every cap and reference rule; `Err(Malformed)` on the first that fails. Caps: panels
    /// `1..=MAX_PANELS`, images, total image bytes, fonts, glyphs, widgets per panel, polygons
    /// and points. References: unique panel ids, every `PanelId`, `ImageId` and `FontId`
    /// resolves, no self or cyclic attach. Geometry: panel sides `1..=MAX_PANEL_SIDE`,
    /// `resize.min` at least 1 and at most the size, a resizable panel has `Shape::Rect`, every
    /// widget rect inside its panel, every sprite non-empty and inside its image, polygons of
    /// 3 or more points inside the panel. Widgets: indicator sprite counts (flag 2, play state
    /// 3), sliders have a frame, `EqBand(b)` has `b < BAND_COUNT`, `ResizeArea` only in a
    /// resizable panel, at most one `ListScroll` per panel and then exactly one `TrackList` or
    /// `Library` beside it, both of its thumb sprites inside its rect. Text: names, titles and
    /// groups clean and short, `list_font` at most 64 characters. Visualiser palette: when
    /// present, `bars` and `scope` each hold `1..=256` colours, so a front-end never indexes an
    /// empty list or walks an unbounded one. First panel: `panels[0]` is visible at start, since
    /// it leads the others and a skin must not hide the window every other panel depends on.
    pub fn validate(&self) -> Result<(), SkinError> {
        check(clean(&self.info.name, 100))?;
        check((1..=MAX_PANELS).contains(&self.panels.len()))?;
        check(self.images.len() <= MAX_IMAGES)?;
        let bytes = self
            .images
            .iter()
            .fold(0usize, |acc, i| acc.saturating_add(i.rgba().len()));
        check(bytes <= MAX_IMAGE_BYTES_TOTAL)?;
        check(self.fonts.len() <= MAX_FONTS)?;
        for font in &self.fonts {
            self.validate_font(font)?;
        }
        if let Some(font) = &self.theme.list_font {
            check(font.chars().count() <= 64)?;
        }
        check(self.panels[0].visible_at_start)?;
        if let Some(vis) = &self.visualiser {
            check((1..=MAX_VIS_COLOURS).contains(&vis.bars.len()))?;
            check((1..=MAX_VIS_COLOURS).contains(&vis.scope.len()))?;
        }
        for (i, panel) in self.panels.iter().enumerate() {
            check(!self.panels[..i].iter().any(|p| p.id == panel.id))?;
        }
        for panel in &self.panels {
            self.validate_panel(panel)?;
        }
        self.validate_attach_graph()
    }

    fn has_panel(&self, id: &PanelId) -> bool {
        self.panels.iter().any(|p| &p.id == id)
    }

    fn sprite_ok(&self, s: &SpriteRect) -> Result<(), SkinError> {
        let image = self.images.get(usize::from(s.image.0));
        let ok = image.is_some_and(|img| {
            s.rect.w > 0 && s.rect.h > 0 && rect_inside(s.rect, img.width(), img.height())
        });
        check(ok)
    }

    fn validate_font(&self, font: &BitmapFont) -> Result<(), SkinError> {
        check(font.cell.w > 0 && font.cell.h > 0)?;
        check(font.glyphs.len() <= MAX_GLYPHS)?;
        font.glyphs.iter().try_for_each(|(_, s)| self.sprite_ok(s))
    }

    fn validate_attach_graph(&self) -> Result<(), SkinError> {
        let index_of = |id: &PanelId| self.panels.iter().position(|p| &p.id == id);
        for (start, panel) in self.panels.iter().enumerate() {
            let mut at = start;
            let mut next = panel.dock.attach.as_ref();
            // Each panel attaches to at most one other, so a walk longer than the panel count
            // has repeated a panel: a cycle (a self-attach included).
            let mut steps = 0;
            while let Some(attach) = next {
                check(self.has_panel(&attach.to))?;
                let to = index_of(&attach.to).ok_or(SkinError::Malformed)?;
                check(to != at)?;
                steps += 1;
                check(steps <= self.panels.len())?;
                at = to;
                next = self.panels[to].dock.attach.as_ref();
            }
        }
        Ok(())
    }

    fn validate_panel(&self, panel: &Panel) -> Result<(), SkinError> {
        check(clean(&panel.title, 100))?;
        if let Some(group) = &panel.dock.group {
            check(clean(group, 64))?;
        }
        let Size { w, h } = panel.size;
        let side = 1..=MAX_PANEL_SIDE;
        check(side.contains(&w) && side.contains(&h))?;
        if let Some(resize) = &panel.resize {
            check(resize.min.w >= 1 && resize.min.h >= 1)?;
            check(resize.min.w <= w && resize.min.h <= h)?;
            check(panel.shape == Shape::Rect)?;
        }
        if let Shape::Polygons(polys) = &panel.shape {
            check((1..=MAX_POLYGONS).contains(&polys.len()))?;
            let points = polys.iter().fold(0usize, |n, p| n.saturating_add(p.len()));
            check(points <= MAX_POINTS)?;
            for poly in polys {
                check(poly.len() >= 3)?;
                check(poly.iter().all(|p| p.x <= w && p.y <= h))?;
            }
        }
        check(panel.widgets.len() <= MAX_WIDGETS_PER_PANEL)?;
        panel
            .widgets
            .iter()
            .try_for_each(|widget| self.validate_widget(panel, widget))?;
        let count =
            |f: fn(&WidgetKind) -> bool| panel.widgets.iter().filter(|w| f(&w.kind)).count();
        let scrolls = count(|k| matches!(k, WidgetKind::ListScroll { .. }));
        check(scrolls <= 1)?;
        if scrolls == 1 {
            check(count(|k| matches!(k, WidgetKind::TrackList | WidgetKind::Library)) == 1)?;
        }
        Ok(())
    }

    fn validate_flag(&self, flag: &Flag) -> Result<(), SkinError> {
        match flag {
            Flag::PanelVisible(id) => check(self.has_panel(id)),
            _ => Ok(()),
        }
    }

    fn validate_command(&self, command: &Command) -> Result<(), SkinError> {
        match command {
            Command::Window(WindowCommand::TogglePanel(id)) => check(self.has_panel(id)),
            Command::Window(WindowCommand::SwapPanels(a, b)) => {
                let first = &self.panels[0].id;
                check(self.has_panel(a) && self.has_panel(b) && a != b && a != first && b != first)
            }
            _ => Ok(()),
        }
    }

    fn validate_widget(&self, panel: &Panel, widget: &Widget) -> Result<(), SkinError> {
        check(rect_inside(widget.rect, panel.size.w, panel.size.h))?;
        let sprite = |s: &SpriteRect| self.sprite_ok(s);
        match &widget.kind {
            WidgetKind::Image { sprite: s, .. } => sprite(s),
            WidgetKind::Indicator { state, sprites } => {
                let want = match state {
                    StateRead::Flag(flag) => {
                        self.validate_flag(flag)?;
                        2
                    }
                    StateRead::PlayState => 3,
                };
                check(sprites.len() == want)?;
                sprites.iter().try_for_each(sprite)
            }
            WidgetKind::Button {
                command,
                normal,
                pressed,
            } => {
                self.validate_command(command)?;
                sprite(normal)?;
                sprite(pressed)
            }
            WidgetKind::Toggle {
                command,
                state,
                off,
                on,
            } => {
                self.validate_command(command)?;
                self.validate_flag(state)?;
                off.iter().chain(on).try_for_each(sprite)
            }
            WidgetKind::Slider {
                value,
                frames,
                thumb,
                ..
            } => {
                if let Value::EqBand(b) = value {
                    check(usize::from(*b) < BAND_COUNT)?;
                }
                check(!frames.is_empty())?;
                frames.iter().try_for_each(sprite)?;
                thumb.iter().flatten().try_for_each(sprite)
            }
            WidgetKind::Text { font, .. } => check(usize::from(font.0) < self.fonts.len()),
            WidgetKind::EqCurve { colours } => colours.iter().try_for_each(sprite),
            WidgetKind::ListScroll { thumb } => {
                thumb.iter().try_for_each(sprite)?;
                check(
                    thumb
                        .iter()
                        .all(|t| t.rect.w <= widget.rect.w && t.rect.h <= widget.rect.h),
                )
            }
            WidgetKind::ResizeArea => check(panel.resize.is_some()),
            WidgetKind::Visualiser
            | WidgetKind::TrackList
            | WidgetKind::Library
            | WidgetKind::DragArea => Ok(()),
        }
    }
}

/// A valid one-panel skin, for the tests of this module and of the registry.
#[cfg(test)]
pub(crate) fn minimal_skin() -> Skin {
    use super::{
        Align, Anchor, Dock, Fill, ImageId, Panel, PanelId, Rect, Shape, Size, SkinImage, SkinInfo,
        SkinTheme, SpriteRect, WidgetKind,
    };
    let image = SkinImage::new(16, 16, vec![0; 16 * 16 * 4]).unwrap();
    let rect = Rect {
        x: 0,
        y: 0,
        w: 16,
        h: 16,
    };
    Skin {
        info: SkinInfo {
            engine: String::new(),
            name: String::new(),
        },
        panels: vec![Panel {
            id: PanelId::new("main").unwrap(),
            title: "Main".to_owned(),
            size: Size { w: 16, h: 16 },
            resize: None,
            shape: Shape::Rect,
            dock: Dock::default(),
            visible_at_start: true,
            widgets: vec![Widget {
                rect,
                anchor: Anchor {
                    h: Align::Start,
                    v: Align::Start,
                },
                kind: WidgetKind::Image {
                    sprite: SpriteRect {
                        image: ImageId(0),
                        rect,
                    },
                    fill: Fill::Once,
                },
            }],
        }],
        images: vec![image],
        fonts: Vec::new(),
        visualiser: None,
        theme: SkinTheme::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skin::{
        Align, Anchor, Attach, Axis, BitmapFont, Dock, Edge, Fill, FontId, FrameRule, ImageId,
        PlayerCommand, Point, Resize, Rgb, SkinImage, TextRead, TimeDigit, VisPalette,
    };

    fn rect(x: u32, y: u32, w: u32, h: u32) -> Rect {
        Rect { x, y, w, h }
    }

    fn sprite(x: u32, y: u32, w: u32, h: u32) -> SpriteRect {
        SpriteRect {
            image: ImageId(0),
            rect: rect(x, y, w, h),
        }
    }

    fn widget(r: Rect, kind: WidgetKind) -> Widget {
        Widget {
            rect: r,
            anchor: Anchor {
                h: Align::Start,
                v: Align::Start,
            },
            kind,
        }
    }

    fn panel(id: &str) -> Panel {
        Panel {
            id: PanelId::new(id).unwrap(),
            title: id.to_owned(),
            size: Size { w: 16, h: 16 },
            resize: None,
            shape: Shape::Rect,
            dock: Dock::default(),
            visible_at_start: false,
            widgets: Vec::new(),
        }
    }

    fn id(s: &str) -> PanelId {
        PanelId::new(s).unwrap()
    }

    /// Every widget kind, a font, a second panel: all valid.
    fn rich_skin() -> Skin {
        let mut skin = minimal_skin();
        skin.fonts.push(BitmapFont {
            cell: Size { w: 4, h: 4 },
            glyphs: vec![('a', sprite(0, 0, 4, 4)), ('b', sprite(4, 0, 4, 4))],
            fallback: Some('a'),
        });
        let mut eq = panel("eq");
        eq.dock.attach = Some(Attach {
            to: id("main"),
            edge: Edge::Bottom,
        });
        eq.dock.group = Some("main".to_owned());
        let s = sprite(0, 0, 4, 4);
        eq.widgets = vec![
            widget(
                rect(0, 0, 4, 4),
                WidgetKind::Indicator {
                    state: StateRead::PlayState,
                    sprites: vec![s, s, s],
                },
            ),
            widget(
                rect(0, 0, 4, 4),
                WidgetKind::Indicator {
                    state: StateRead::Flag(Flag::PanelVisible(id("main"))),
                    sprites: vec![s, s],
                },
            ),
            widget(
                rect(4, 0, 4, 4),
                WidgetKind::Button {
                    command: Command::Player(PlayerCommand::Play),
                    normal: s,
                    pressed: s,
                },
            ),
            widget(
                rect(8, 0, 4, 4),
                WidgetKind::Toggle {
                    command: Command::Window(WindowCommand::TogglePanel(id("main"))),
                    state: Flag::PanelVisible(id("eq")),
                    off: [s, s],
                    on: [s, s],
                },
            ),
            widget(
                rect(0, 4, 16, 4),
                WidgetKind::Slider {
                    value: Value::EqBand(9),
                    axis: Axis::Vertical,
                    frames: vec![s, s],
                    frame_rule: FrameRule::FromCentre,
                    thumb: Some([s, s]),
                },
            ),
            widget(
                rect(0, 8, 16, 4),
                WidgetKind::Text {
                    text: TextRead::Time(TimeDigit::SecondsUnits),
                    font: FontId(0),
                    scroll: true,
                },
            ),
            widget(
                rect(0, 12, 16, 4),
                WidgetKind::EqCurve {
                    colours: Some(sprite(0, 0, 1, 16)),
                },
            ),
            widget(rect(0, 0, 16, 16), WidgetKind::Visualiser),
            widget(rect(0, 0, 16, 16), WidgetKind::TrackList),
            widget(rect(0, 0, 16, 16), WidgetKind::Library),
            widget(rect(0, 0, 16, 16), WidgetKind::DragArea),
        ];
        skin.panels.push(eq);
        skin.theme.list_text = Some(Rgb { r: 1, g: 2, b: 3 });
        skin.theme.list_font = Some("x".repeat(64));
        skin
    }

    fn rejects(mutate: impl FnOnce(&mut Skin)) {
        let mut skin = rich_skin();
        mutate(&mut skin);
        assert_eq!(skin.validate(), Err(SkinError::Malformed));
    }

    fn eq_widgets(skin: &mut Skin) -> &mut Vec<Widget> {
        &mut skin.panels[1].widgets
    }

    #[test]
    fn panel_id_table() {
        for ok in ["main", "p0", "a.b-c_d", "0", &"a".repeat(32)] {
            assert!(PanelId::new(ok).is_some(), "{ok}");
            assert_eq!(PanelId::new(ok).unwrap().as_str(), ok);
        }
        for bad in [
            "",
            "Main",
            "-x",
            ".x",
            "_x",
            &"a".repeat(33),
            "a/b",
            "ä",
            "a b",
            "a\n",
        ] {
            assert!(PanelId::new(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn accepts_minimal_and_rich_skins() {
        assert_eq!(minimal_skin().validate(), Ok(()));
        assert_eq!(rich_skin().validate(), Ok(()));
    }

    #[test]
    fn rejects_duplicate_panel_id() {
        rejects(|s| s.panels[1].id = id("main"));
    }

    #[test]
    fn rejects_dangling_self_and_cyclic_attach() {
        rejects(|s| s.panels[1].dock.attach.as_mut().unwrap().to = id("nope"));
        rejects(|s| s.panels[1].dock.attach.as_mut().unwrap().to = id("eq"));
        rejects(|s| {
            s.panels[0].dock.attach = Some(Attach {
                to: id("eq"),
                edge: Edge::Top,
            })
        });
        // A chain of three that closes on the first.
        rejects(|s| {
            let mut c = panel("c");
            c.dock.attach = Some(Attach {
                to: id("main"),
                edge: Edge::Left,
            });
            s.panels.push(c);
            s.panels[0].dock.attach = Some(Attach {
                to: id("c"),
                edge: Edge::Left,
            });
        });
    }

    #[test]
    fn accepts_a_long_attach_chain() {
        let mut skin = rich_skin();
        let mut c = panel("c");
        c.dock.attach = Some(Attach {
            to: id("eq"),
            edge: Edge::Right,
        });
        skin.panels.push(c);
        assert_eq!(skin.validate(), Ok(()));
    }

    #[test]
    fn rejects_sprite_problems() {
        // Outside the image (16x16), empty, and an image id out of range.
        rejects(|s| {
            s.panels[0].widgets[0].kind = WidgetKind::Image {
                sprite: sprite(10, 0, 7, 4),
                fill: Fill::Tile,
            }
        });
        rejects(|s| {
            s.panels[0].widgets[0].kind = WidgetKind::Image {
                sprite: sprite(0, 0, 0, 4),
                fill: Fill::Once,
            }
        });
        rejects(|s| {
            s.panels[0].widgets[0].kind = WidgetKind::Image {
                sprite: SpriteRect {
                    image: ImageId(1),
                    rect: rect(0, 0, 1, 1),
                },
                fill: Fill::Once,
            }
        });
        rejects(|s| {
            s.panels[0].widgets[0].kind = WidgetKind::Image {
                sprite: sprite(u32::MAX, 0, 1, 1),
                fill: Fill::Once,
            }
        });
        rejects(|s| {
            s.panels[0].widgets[0].kind = WidgetKind::Image {
                sprite: sprite(1, 0, u32::MAX, 1),
                fill: Fill::Once,
            }
        });
        rejects(|s| s.fonts[0].glyphs[0].1 = sprite(14, 14, 4, 4));
    }

    #[test]
    fn rejects_widget_outside_panel_and_wrapping_rects() {
        rejects(|s| s.panels[0].widgets[0].rect = rect(1, 0, 16, 16));
        rejects(|s| s.panels[0].widgets[0].rect = rect(0, 1, 16, 16));
        rejects(|s| s.panels[0].widgets[0].rect = rect(u32::MAX, 0, 1, 1));
        rejects(|s| s.panels[0].widgets[0].rect = rect(0, 0, u32::MAX, 1));
        rejects(|s| s.panels[0].widgets[0].rect = rect(2, 0, u32::MAX - 1, 1));
    }

    #[test]
    fn rejects_polygon_problems() {
        let tri = |x: u32| {
            vec![
                Point { x: 0, y: 0 },
                Point { x, y: 0 },
                Point { x: 0, y: 16 },
            ]
        };
        let mut ok = rich_skin();
        ok.panels[0].shape = Shape::Polygons(vec![tri(16)]);
        assert_eq!(ok.validate(), Ok(()));
        rejects(|s| s.panels[0].shape = Shape::Polygons(vec![tri(17)]));
        rejects(|s| s.panels[0].shape = Shape::Polygons(vec![tri(u32::MAX)]));
        rejects(|s| s.panels[0].shape = Shape::Polygons(vec![tri(16)[..2].to_vec()]));
        rejects(|s| s.panels[0].shape = Shape::Polygons(Vec::new()));
        rejects(|s| s.panels[0].shape = Shape::Polygons(vec![tri(16); MAX_POLYGONS + 1]));
        rejects(|s| {
            let big = vec![Point { x: 1, y: 1 }; MAX_POINTS / 2 + 1];
            s.panels[0].shape = Shape::Polygons(vec![big.clone(), big]);
        });
    }

    #[test]
    fn rejects_eq_band_out_of_range() {
        rejects(|s| {
            eq_widgets(s)[4].kind = WidgetKind::Slider {
                value: Value::EqBand(10),
                axis: Axis::Vertical,
                frames: vec![sprite(0, 0, 4, 4)],
                frame_rule: FrameRule::Linear,
                thumb: None,
            }
        });
    }

    #[test]
    fn rejects_panel_count_and_sizes() {
        rejects(|s| s.panels.clear());
        rejects(|s| {
            while s.panels.len() < MAX_PANELS + 1 {
                let n = s.panels.len();
                s.panels.push(panel(&format!("p{n}")));
            }
        });
        let mut at_cap = rich_skin();
        while at_cap.panels.len() < MAX_PANELS {
            let n = at_cap.panels.len();
            at_cap.panels.push(panel(&format!("p{n}")));
        }
        assert_eq!(at_cap.validate(), Ok(()));
        rejects(|s| s.panels[1].size = Size { w: 0, h: 16 });
        rejects(|s| s.panels[1].size = Size { w: 16, h: 0 });
        rejects(|s| {
            s.panels[1].size = Size {
                w: MAX_PANEL_SIDE + 1,
                h: 16,
            }
        });
        let mut tall = rich_skin();
        tall.panels[1].widgets.clear();
        tall.panels[1].size = Size {
            w: MAX_PANEL_SIDE,
            h: MAX_PANEL_SIDE,
        };
        assert_eq!(tall.validate(), Ok(()));
    }

    #[test]
    fn resizable_panels_need_a_rect_shape_and_a_sane_minimum() {
        let resize = |min_w, min_h| Resize {
            min: Size { w: min_w, h: min_h },
            step: Size { w: 4, h: 0 },
        };
        let mut ok = rich_skin();
        ok.panels[0].resize = Some(resize(8, 16));
        assert_eq!(ok.validate(), Ok(()));
        rejects(|s| s.panels[0].resize = Some(resize(0, 16)));
        rejects(|s| s.panels[0].resize = Some(resize(17, 16)));
        rejects(|s| s.panels[0].resize = Some(resize(8, 17)));
        rejects(|s| {
            s.panels[0].resize = Some(resize(8, 16));
            s.panels[0].shape = Shape::Polygons(vec![vec![
                Point { x: 0, y: 0 },
                Point { x: 16, y: 0 },
                Point { x: 0, y: 16 },
            ]]);
        });
    }

    #[test]
    fn resize_area_needs_a_resizable_panel() {
        let mut ok = rich_skin();
        ok.panels[0].resize = Some(Resize {
            min: Size { w: 8, h: 8 },
            step: Size { w: 4, h: 4 },
        });
        ok.panels[0]
            .widgets
            .push(widget(rect(12, 12, 4, 4), WidgetKind::ResizeArea));
        assert_eq!(ok.validate(), Ok(()));
        // The same widget in a fixed panel.
        rejects(|s| {
            s.panels[0]
                .widgets
                .push(widget(rect(12, 12, 4, 4), WidgetKind::ResizeArea))
        });
    }

    #[test]
    fn list_scroll_rules() {
        let scroll = |w: u32, h: u32| {
            widget(
                rect(12, 0, 4, 16),
                WidgetKind::ListScroll {
                    thumb: [sprite(0, 0, w, h), sprite(4, 0, w, h)],
                },
            )
        };
        let list = || widget(rect(0, 0, 12, 16), WidgetKind::TrackList);
        let with = |widgets: Vec<Widget>| {
            let mut s = rich_skin();
            s.panels[1].widgets = widgets;
            s
        };
        // A list and one scroll bar whose thumbs fit (exactly, too).
        assert_eq!(with(vec![list(), scroll(4, 8)]).validate(), Ok(()));
        assert_eq!(with(vec![list(), scroll(4, 16)]).validate(), Ok(()));
        let library = widget(rect(0, 0, 12, 16), WidgetKind::Library);
        assert_eq!(with(vec![library.clone(), scroll(4, 8)]).validate(), Ok(()));
        let bad = |widgets: Vec<Widget>| {
            assert_eq!(with(widgets).validate(), Err(SkinError::Malformed));
        };
        // No list, two lists, two scroll bars.
        bad(vec![scroll(4, 8)]);
        bad(vec![list(), library, scroll(4, 8)]);
        bad(vec![list(), list(), scroll(4, 8)]);
        bad(vec![list(), scroll(4, 8), scroll(4, 8)]);
        // A thumb wider or taller than the rect, or outside its image.
        bad(vec![list(), scroll(5, 8)]);
        bad(vec![list(), scroll(4, 17)]);
        bad(vec![
            list(),
            widget(
                rect(12, 0, 4, 16),
                WidgetKind::ListScroll {
                    thumb: [sprite(0, 0, 4, 8), sprite(14, 0, 4, 8)],
                },
            ),
        ]);
        // Without a scroll bar one or more lists stay valid (the older rule).
        assert_eq!(with(vec![list(), list()]).validate(), Ok(()));
    }

    #[test]
    fn rejects_dangling_panel_references() {
        rejects(|s| {
            eq_widgets(s)[3].kind = WidgetKind::Toggle {
                command: Command::Window(WindowCommand::TogglePanel(id("nope"))),
                state: Flag::Shuffle,
                off: [sprite(0, 0, 4, 4); 2],
                on: [sprite(0, 0, 4, 4); 2],
            }
        });
        rejects(|s| {
            eq_widgets(s)[3].kind = WidgetKind::Toggle {
                command: Command::Player(PlayerCommand::ToggleEq),
                state: Flag::PanelVisible(id("nope")),
                off: [sprite(0, 0, 4, 4); 2],
                on: [sprite(0, 0, 4, 4); 2],
            }
        });
        rejects(|s| {
            eq_widgets(s)[1].kind = WidgetKind::Indicator {
                state: StateRead::Flag(Flag::PanelVisible(id("nope"))),
                sprites: vec![sprite(0, 0, 4, 4); 2],
            }
        });
    }

    #[test]
    fn swap_panels_names_two_other_panels() {
        let swap = |a: &str, b: &str| {
            let mut skin = rich_skin();
            skin.panels.push(panel("open"));
            eq_widgets(&mut skin)[2].kind = WidgetKind::Button {
                command: Command::Window(WindowCommand::SwapPanels(id(a), id(b))),
                normal: sprite(0, 0, 4, 4),
                pressed: sprite(0, 0, 4, 4),
            };
            skin.validate()
        };
        assert_eq!(swap("eq", "open"), Ok(()));
        assert_eq!(swap("open", "eq"), Ok(()));
        // A missing panel, the same panel twice, and the first panel (never hidden) are refused.
        assert_eq!(swap("eq", "nope"), Err(SkinError::Malformed));
        assert_eq!(swap("eq", "eq"), Err(SkinError::Malformed));
        assert_eq!(swap("main", "eq"), Err(SkinError::Malformed));
        assert_eq!(swap("eq", "main"), Err(SkinError::Malformed));
    }

    #[test]
    fn rejects_widget_shape_problems() {
        let s4 = sprite(0, 0, 4, 4);
        rejects(|s| {
            eq_widgets(s)[0].kind = WidgetKind::Indicator {
                state: StateRead::PlayState,
                sprites: vec![s4, s4],
            }
        });
        rejects(|s| {
            eq_widgets(s)[1].kind = WidgetKind::Indicator {
                state: StateRead::Flag(Flag::Mono),
                sprites: vec![s4; 3],
            }
        });
        rejects(|s| {
            eq_widgets(s)[4].kind = WidgetKind::Slider {
                value: Value::Volume,
                axis: Axis::Horizontal,
                frames: Vec::new(),
                frame_rule: FrameRule::Linear,
                thumb: None,
            }
        });
        rejects(|s| {
            eq_widgets(s)[4].kind = WidgetKind::Slider {
                value: Value::Volume,
                axis: Axis::Horizontal,
                frames: vec![s4],
                frame_rule: FrameRule::Linear,
                thumb: Some([s4, sprite(15, 0, 4, 4)]),
            }
        });
        rejects(|s| {
            eq_widgets(s)[5].kind = WidgetKind::Text {
                text: TextRead::TrackTitle,
                font: FontId(1),
                scroll: false,
            }
        });
        rejects(|s| {
            eq_widgets(s)[6].kind = WidgetKind::EqCurve {
                colours: Some(sprite(0, 0, 1, 17)),
            }
        });
    }

    #[test]
    fn rejects_cap_overruns() {
        rejects(|s| {
            let w = s.panels[0].widgets[0].clone();
            s.panels[0].widgets = vec![w; MAX_WIDGETS_PER_PANEL + 1];
        });
        let mut at_cap = rich_skin();
        let w = at_cap.panels[0].widgets[0].clone();
        at_cap.panels[0].widgets = vec![w; MAX_WIDGETS_PER_PANEL];
        assert_eq!(at_cap.validate(), Ok(()));
        rejects(|s| {
            let img = SkinImage::new(1, 1, vec![0; 4]).unwrap();
            s.images.resize(MAX_IMAGES + 1, img);
        });
        rejects(|s| {
            let big = SkinImage::new(2048, 2048, vec![0; 2048 * 2048 * 4]).unwrap();
            s.images = vec![big; 5];
        });
        rejects(|s| {
            let g = s.fonts[0].glyphs[0];
            s.fonts[0].glyphs = vec![g; MAX_GLYPHS + 1];
        });
        rejects(|s| {
            let f = s.fonts[0].clone();
            s.fonts = vec![f; MAX_FONTS + 1];
        });
        rejects(|s| s.fonts[0].cell = Size { w: 0, h: 4 });
    }

    #[test]
    fn accepts_images_at_the_total_cap() {
        let mut skin = rich_skin();
        let big = SkinImage::new(2048, 2048, vec![0; 2048 * 2048 * 4]).unwrap();
        skin.images = vec![big; 4];
        assert_eq!(
            skin.images.iter().map(|i| i.rgba().len()).sum::<usize>(),
            MAX_IMAGE_BYTES_TOTAL
        );
        assert_eq!(skin.validate(), Ok(()));
    }

    #[test]
    fn rejects_unclean_or_long_text() {
        rejects(|s| s.panels[0].title = "a\nb".to_owned());
        rejects(|s| s.panels[0].title = "a".repeat(101));
        rejects(|s| s.panels[0].title = " padded".to_owned());
        rejects(|s| s.info.name = "a\u{202E}b".to_owned());
        rejects(|s| s.panels[1].dock.group = Some("g".repeat(65)));
        rejects(|s| s.theme.list_font = Some("x".repeat(65)));
        let mut ok = rich_skin();
        ok.panels[0].title = "a".repeat(100);
        assert_eq!(ok.validate(), Ok(()));
    }

    fn palette(bars: usize, scope: usize) -> VisPalette {
        let c = Rgb { r: 1, g: 2, b: 3 };
        VisPalette {
            background: c,
            grid: c,
            bars: vec![c; bars],
            scope: vec![c; scope],
            peak: c,
        }
    }

    #[test]
    fn visualiser_palette_lists_are_non_empty_and_capped() {
        let mut ok = rich_skin();
        ok.visualiser = Some(palette(24, 5));
        assert_eq!(ok.validate(), Ok(()));
        ok.visualiser = Some(palette(256, 256));
        assert_eq!(ok.validate(), Ok(()));
        ok.visualiser = None;
        assert_eq!(ok.validate(), Ok(()));
        rejects(|s| s.visualiser = Some(palette(0, 5)));
        rejects(|s| s.visualiser = Some(palette(24, 0)));
        rejects(|s| s.visualiser = Some(palette(257, 5)));
        rejects(|s| s.visualiser = Some(palette(24, 257)));
    }

    #[test]
    fn the_first_panel_is_visible_at_start() {
        rejects(|s| s.panels[0].visible_at_start = false);
        // The second panel (hidden in the fixture) may be hidden; showing it is fine too.
        assert!(!rich_skin().panels[1].visible_at_start);
        assert_eq!(rich_skin().validate(), Ok(()));
        let mut shown = rich_skin();
        shown.panels[1].visible_at_start = true;
        assert_eq!(shown.validate(), Ok(()));
    }
}
