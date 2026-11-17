//! The engine: a `.wmz` archive in, the core's skin model out (D5, D6, D9, D14).
//!
//! The first VIEW of the skin's description is composed once into a static picture
//! (`compose`), the interactive elements it finds become widgets on top of that picture
//! (`widgets` holds the fixed tables), and the player's playlist and library windows are added
//! from the kit's composed panels. A drawer the skin's script would slide out from behind the
//! body becomes two more panels, closed and open, that its buttons swap (MD193). The archive is
//! untrusted input: every cap is in the kit readers, `wms` and `compose`; entry names are lookup
//! keys, never paths; no script is read, inflated or run.

use std::collections::HashMap;

use whipspot_core::skin::{
    Align, Anchor, Attach, Axis, BitmapFont, Command, Dock, Edge, Fill, FontId, FrameRule, ImageId,
    MAX_SKIN_BYTES, MAX_WIDGETS_PER_PANEL, Panel, PanelId, Rect, Rgb, Size, Skin, SkinEngine,
    SkinError, SkinImage, SkinInfo, SkinTheme, SpriteRect, TextRead, TimeDigit, VisPalette, Widget,
    WidgetKind, WindowCommand,
};
use whipspot_core::text::display_text;
use whipspot_skin_kit::font::{CELL_H, CELL_W, pixel_font};
use whipspot_skin_kit::panels::{ListKind, ListPanel, ListStyle, chrome_image, list_panel};
use whipspot_skin_kit::zip::{Archive, ZipError};

use crate::compose::{
    Arts, Composition, Crop, Keys, Kind, MAX_THUMB_SIDE, Painter, Placed, Scope, Thumb, left_out,
    view_size, zindex,
};
use crate::geometry::{Prop, Resolver};
use crate::widgets::{bare_call, library_id, parse_colour, playlist_id};
use crate::wms::{self, WmsError};

/// Turns a Windows Media Player skin archive into a [`Skin`].
#[derive(Debug, Clone, Copy, Default)]
pub struct WmpEngine;

impl SkinEngine for WmpEngine {
    fn id(&self) -> &'static str {
        "wmz"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["wmz"]
    }

    fn detect(&self, bytes: &[u8]) -> bool {
        Archive::parse(bytes).is_ok_and(|a| a.keys().iter().any(|k| k.ends_with(".wms")))
    }

    fn load(&self, bytes: &[u8]) -> Result<Skin, SkinError> {
        if bytes.len() as u64 > MAX_SKIN_BYTES {
            return Err(SkinError::TooLarge);
        }
        let archive = Archive::parse(bytes).map_err(zip_error)?;
        let key = archive
            .keys()
            .into_iter()
            .find(|k| k.ends_with(".wms"))
            .ok_or(SkinError::Malformed)?;
        let raw = archive
            .read(key)
            .ok_or(SkinError::Malformed)?
            .map_err(zip_error)?;
        let text = wms::decode_text(&raw).ok_or(SkinError::TooLarge)?;
        let tree = wms::parse(&text).map_err(|e| match e {
            WmsError::Malformed => SkinError::Malformed,
            WmsError::TooLarge => SkinError::TooLarge,
        })?;
        build(&archive, &tree)
    }
}

fn zip_error(e: ZipError) -> SkinError {
    match e {
        ZipError::Malformed => SkinError::Malformed,
        ZipError::TooLarge => SkinError::TooLarge,
        ZipError::Unsupported => SkinError::Unsupported,
    }
}

const START: Anchor = Anchor {
    h: Align::Start,
    v: Align::Start,
};

/// Most drawers a skin gets; any further candidate stays in the body.
const MAX_DRAWERS: usize = 4;

/// A drawer (MD193): a subview of the VIEW tucked behind the body (a negative `zIndex`) whose
/// buttons call script functions no other drawer calls. Its script would slide it out; here it is
/// two panels, closed and open, and every button calling one of its functions swaps them.
struct Drawer {
    el: usize,
    calls: Vec<String>,
    left: i64,
    width: i64,
}

/// The drawers of `view`, in document order, at most [`MAX_DRAWERS`].
fn find_drawers(tree: &wms::Tree, view: usize, res: &mut Resolver) -> Vec<Drawer> {
    let Some(v) = tree.get(view) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for &c in &v.children {
        let Some(el) = tree.get(c) else { continue };
        let shown = el
            .attr("visible")
            .is_none_or(|v| v.trim().eq_ignore_ascii_case("true"));
        if el.tag != "subview" || el.attr("id").is_none() || zindex(tree, c) >= 0 || !shown {
            continue;
        }
        let (Some(left), Some(width)) = (res.value(c, Prop::Left), res.value(c, Prop::Width))
        else {
            continue;
        };
        if width < 1 {
            continue;
        }
        // Every element is some element's child at most once, so the walk ends.
        let mut calls = Vec::new();
        let mut stack = vec![c];
        while let Some(e) = stack.pop() {
            let Some(el) = tree.get(e) else { continue };
            if matches!(el.tag.as_str(), "button" | "buttonelement")
                && let Some(call) = el.attr("onclick").and_then(bare_call)
                && !calls.contains(&call)
            {
                calls.push(call);
            }
            stack.extend(el.children.iter().copied());
        }
        if !calls.is_empty() {
            found.push(Drawer {
                el: c,
                calls,
                left,
                width,
            });
        }
    }
    // A function two drawers call names neither.
    let all: Vec<String> = found.iter().flat_map(|d| d.calls.clone()).collect();
    for d in &mut found {
        d.calls
            .retain(|c| all.iter().filter(|a| *a == c).count() == 1);
    }
    found.retain(|d| !d.calls.is_empty());
    found.truncate(MAX_DRAWERS);
    found
}

/// The ids of drawer `i`'s two panels: closed, open.
fn drawer_ids(i: usize) -> Result<(PanelId, PanelId), SkinError> {
    let id = |s: String| PanelId::new(&s).ok_or(SkinError::Malformed);
    Ok((
        id(format!("drawer-{}", i + 1))?,
        id(format!("drawer-{}-open", i + 1))?,
    ))
}

/// One panel built from a composition.
struct Part {
    id: PanelId,
    title: String,
    comp: Composition,
    attach: Option<Attach>,
    visible: bool,
    /// The leader: it gets the drag area.
    leader: bool,
}

fn build(archive: &Archive, tree: &wms::Tree) -> Result<Skin, SkinError> {
    let theme = tree.get(tree.root).ok_or(SkinError::Malformed)?;
    let view = theme
        .children
        .iter()
        .copied()
        .find(|c| tree.get(*c).is_some_and(|e| e.tag == "view"))
        .ok_or(SkinError::Malformed)?;
    let title = display_text(theme.attr("title").unwrap_or(""), 100);
    let title = if title.is_empty() {
        "Player".to_owned()
    } else {
        title
    };

    let mut res = Resolver::new(tree);
    let (vw, _) = view_size(&mut res, view)?;
    let (vx, vy) = (
        res.value(view, Prop::Left).unwrap_or(0),
        res.value(view, Prop::Top).unwrap_or(0),
    );
    let mut drawers = find_drawers(tree, view, &mut res);
    let mut arts = Arts::new(archive);
    let main_id = PanelId::new("main").ok_or(SkinError::Malformed)?;

    // A drawer that does not come out beside the body stays in it: the body is painted again
    // without that drawer, at most MAX_DRAWERS more times.
    loop {
        let mut calls = HashMap::new();
        for (i, d) in drawers.iter().enumerate() {
            let (closed, open) = drawer_ids(i)?;
            for call in &d.calls {
                calls.insert(
                    call.clone(),
                    Command::Window(WindowCommand::SwapPanels(open.clone(), closed.clone())),
                );
            }
        }
        let scope = Scope {
            skip: drawers.iter().map(|d| d.el).collect(),
            calls: calls.clone(),
            ..Scope::default()
        };
        let mut painter = Painter::new(&mut arts, tree, view, scope)?;
        painter.paint(view, 0, 0)?;
        let main = painter
            .finish(None, Crop::Content)?
            .ok_or(SkinError::Malformed)?;

        let mut parts = Vec::new();
        let mut failed = None;
        for (i, d) in drawers.iter().enumerate() {
            let (mx0, _, mx1, _) = main.bbox;
            let left = 2 * d.left + d.width < mx0 + mx1;
            let open_left = if left { 0 } else { vw - d.width };
            let crop = Crop::Beside {
                main: main.bbox,
                left,
            };
            let mut state = |moved: Option<(usize, i64)>, reveal: bool| {
                let scope = Scope {
                    moved,
                    reveal,
                    calls: calls.clone(),
                    ..Scope::default()
                };
                let mut painter = Painter::new(&mut arts, tree, view, scope)?;
                painter.paint(d.el, vx, vy)?;
                painter.finish(Some(&main.painted), crop)
            };
            let closed = state(None, false)?;
            let open = if open_left == d.left {
                None
            } else {
                state(Some((d.el, open_left)), true)?
            };
            let (Some(closed), Some(open)) = (closed, open) else {
                failed = Some(i);
                break;
            };
            let (closed_id, open_id) = drawer_ids(i)?;
            let attach = Some(Attach {
                to: main_id.clone(),
                edge: if left { Edge::Left } else { Edge::Right },
            });
            for (id, comp, visible) in [(closed_id, closed, true), (open_id, open, false)] {
                parts.push(Part {
                    id,
                    title: title.clone(),
                    comp,
                    attach: attach.clone(),
                    visible,
                    leader: false,
                });
            }
        }
        if let Some(i) = failed {
            drawers.remove(i);
            continue;
        }
        parts.insert(
            0,
            Part {
                id: main_id.clone(),
                title: title.clone(),
                comp: main,
                attach: None,
                visible: true,
                leader: true,
            },
        );
        return assemble(tree, &arts.images, parts);
    }
}

/// The pixels of `img` with the colour keys and alpha-0 pixels made transparent.
fn keyed(img: &SkinImage, keys: Keys) -> Vec<u8> {
    let mut px = img.rgba().to_vec();
    for p in px.chunks_exact_mut(4) {
        if left_out(p, keys) {
            p.copy_from_slice(&[0, 0, 0, 0]);
        }
    }
    px
}

/// One slider thumb placed in the atlas: `[normal, pressed]` as `(y, w, h)`.
type Placement = [(u32, u32, u32); 2];

/// The sheet of slider thumbs: each kept thumb (at most `MAX_THUMB_SIDE` a side, inside its
/// slider's rectangle) stacked top to bottom. Returns the image and, per widget, its placement.
fn build_atlas(
    widgets: &[Placed],
    arts: &[SkinImage],
) -> (Option<SkinImage>, Vec<Option<Placement>>) {
    const MAX_ATLAS_SIDE: u32 = 2048;
    let fits = |img: &SkinImage, p: &Placed| {
        img.width() <= MAX_THUMB_SIDE
            && img.height() <= MAX_THUMB_SIDE
            && img.width() <= p.w
            && img.height() <= p.h
    };
    let mut pieces: Vec<(Vec<u8>, u32, u32, u32)> = Vec::new();
    let mut placements = Vec::with_capacity(widgets.len());
    let mut y = 0u32;
    for p in widgets {
        let Kind::Slider {
            thumb: Some(Thumb { normal, down, keys }),
            ..
        } = &p.kind
        else {
            placements.push(None);
            continue;
        };
        let (Some(n), d) = (arts.get(*normal), down.and_then(|i| arts.get(i))) else {
            placements.push(None);
            continue;
        };
        if !fits(n, p) {
            placements.push(None);
            continue;
        }
        let d = d.filter(|d| fits(d, p));
        let need = n.height() + d.map_or(0, SkinImage::height);
        if y + need > MAX_ATLAS_SIDE {
            placements.push(None);
            continue;
        }
        let at_n = (y, n.width(), n.height());
        pieces.push((keyed(n, *keys), n.width(), n.height(), y));
        y += n.height();
        let at_d = match d {
            Some(d) => {
                let at = (y, d.width(), d.height());
                pieces.push((keyed(d, *keys), d.width(), d.height(), y));
                y += d.height();
                at
            }
            None => at_n,
        };
        placements.push(Some([at_n, at_d]));
    }
    if pieces.is_empty() {
        return (None, placements);
    }
    let width = pieces.iter().map(|p| p.1).max().unwrap_or(1);
    let mut px = vec![0u8; width as usize * y as usize * 4];
    for (data, w, h, top) in &pieces {
        for row in 0..*h {
            let from = (row * w * 4) as usize;
            let to = (((top + row) * width) * 4) as usize;
            px[to..to + (*w * 4) as usize].copy_from_slice(&data[from..from + (*w * 4) as usize]);
        }
    }
    (SkinImage::new(width, y, px), placements)
}

/// The spectrum's bar colour when the skin has no title text colour to borrow.
const VIS_DEFAULT_BAR: Rgb = Rgb {
    r: 50,
    g: 205,
    b: 50,
};

/// The visualiser's background: the composed art's pixel at the centre of its rectangle (black
/// when there is none or the pixel is transparent), so the live spectrum sits on the skin's own
/// colour instead of the theme's input colour.
fn vis_background(normal: &SkinImage, widgets: &[Placed]) -> Rgb {
    let black = Rgb { r: 0, g: 0, b: 0 };
    let Some(v) = widgets.iter().find(|p| matches!(p.kind, Kind::Visualiser)) else {
        return black;
    };
    let (cx, cy) = (v.x + v.w / 2, v.y + v.h / 2);
    if cx >= normal.width() || cy >= normal.height() {
        return black;
    }
    let at = (cy as usize * normal.width() as usize + cx as usize) * 4;
    match normal.rgba().get(at..at + 4) {
        Some(&[r, g, b, a]) if a != 0 => Rgb { r, g, b },
        _ => black,
    }
}

/// The images of one part: its layers, thumb atlas and visualiser overlay.
struct PartImages {
    layer: ImageId,
    pressed: ImageId,
    atlas: Option<ImageId>,
    placements: Vec<Option<Placement>>,
    overlay: Option<ImageId>,
}

fn image_id(images: &[SkinImage]) -> Result<ImageId, SkinError> {
    u16::try_from(images.len())
        .map(ImageId)
        .map_err(|_| SkinError::Malformed)
}

/// Copies a part's pictures into `images`.
fn part_images(
    comp: &Composition,
    arts: &[SkinImage],
    images: &mut Vec<SkinImage>,
) -> Result<PartImages, SkinError> {
    let layer = image_id(images)?;
    images.push(comp.normal.clone());
    let pressed = match &comp.pressed {
        Some(p) => {
            let id = image_id(images)?;
            images.push(p.clone());
            id
        }
        None => layer,
    };
    let (atlas, placements) = build_atlas(&comp.widgets, arts);
    let atlas = match atlas {
        Some(a) => {
            let id = image_id(images)?;
            images.push(a);
            Some(id)
        }
        None => None,
    };
    let overlay = match &comp.overlay {
        Some((_, _, img)) => {
            let id = image_id(images)?;
            images.push(img.clone());
            Some(id)
        }
        None => None,
    };
    Ok(PartImages {
        layer,
        pressed,
        atlas,
        placements,
        overlay,
    })
}

/// The widgets of one part over its pictures.
fn part_widgets(
    part: &Part,
    ids: &PartImages,
    fonts: &[BitmapFont],
    colours: &[Rgb],
) -> Vec<Widget> {
    let (bw, bh) = (part.comp.normal.width(), part.comp.normal.height());
    let font_of = |colour: &Rgb| colours.iter().position(|c| c == colour).unwrap_or(0) as u16;
    let sprite = |image: ImageId, p: &Placed| SpriteRect {
        image,
        rect: Rect {
            x: p.x,
            y: p.y,
            w: p.w,
            h: p.h,
        },
    };
    let whole = Rect {
        x: 0,
        y: 0,
        w: bw,
        h: bh,
    };
    let mut out = Vec::new();
    if part.leader {
        out.push(Widget {
            rect: whole,
            anchor: START,
            kind: WidgetKind::DragArea,
        });
    }
    // The body art itself: the composed layer drawn first, under every widget (the front-end
    // paints the panel's widgets in order and the DragArea draws nothing).
    out.push(Widget {
        rect: whole,
        anchor: START,
        kind: WidgetKind::Image {
            sprite: SpriteRect {
                image: ids.layer,
                rect: whole,
            },
            fill: Fill::Once,
        },
    });
    for (i, p) in part.comp.widgets.iter().enumerate() {
        let rect = Rect {
            x: p.x,
            y: p.y,
            w: p.w,
            h: p.h,
        };
        let widget = |kind: WidgetKind| Widget {
            rect,
            anchor: START,
            kind,
        };
        match &p.kind {
            Kind::Button(command) => out.push(widget(WidgetKind::Button {
                command: command.clone(),
                normal: sprite(ids.layer, p),
                pressed: sprite(ids.pressed, p),
            })),
            Kind::Slider {
                value, vertical, ..
            } => {
                let thumb = match (ids.atlas, ids.placements.get(i).copied().flatten()) {
                    (Some(id), Some([n, d])) => {
                        let at = |(y, w, h): (u32, u32, u32)| SpriteRect {
                            image: id,
                            rect: Rect { x: 0, y, w, h },
                        };
                        Some([at(n), at(d)])
                    }
                    _ => None,
                };
                out.push(widget(WidgetKind::Slider {
                    value: *value,
                    axis: if *vertical {
                        Axis::Vertical
                    } else {
                        Axis::Horizontal
                    },
                    frames: vec![sprite(ids.layer, p)],
                    frame_rule: FrameRule::Linear,
                    thumb,
                }));
            }
            Kind::Visualiser => {
                out.push(widget(WidgetKind::Visualiser));
                // The art the skin paints over the visualiser (the frame round a screen) on top.
                if let (Some(id), Some((x, y, img))) = (ids.overlay, part.comp.overlay.as_ref()) {
                    let (w, h) = (img.width(), img.height());
                    out.push(Widget {
                        rect: Rect { x: *x, y: *y, w, h },
                        anchor: START,
                        kind: WidgetKind::Image {
                            sprite: SpriteRect {
                                image: id,
                                rect: Rect { x: 0, y: 0, w, h },
                            },
                            fill: Fill::Once,
                        },
                    });
                }
            }
            Kind::List => out.push(widget(WidgetKind::TrackList)),
            Kind::Title { colour } => out.push(widget(WidgetKind::Text {
                text: TextRead::TrackTitle,
                font: FontId(font_of(colour)),
                scroll: true,
            })),
            Kind::Time { colour } => {
                let font_index = font_of(colour);
                let colon = fonts
                    .get(usize::from(font_index))
                    .and_then(|f| f.glyphs.iter().find(|(c, _)| *c == ':'))
                    .map(|(_, s)| *s);
                // Five cells (mm:ss) packed against the right edge of the element.
                let width = 5 * CELL_W;
                let (Some(colon), true) = (colon, p.w >= width) else {
                    continue;
                };
                let y = p.y + p.h.saturating_sub(CELL_H) / 2;
                // The cells must lie inside the panel (`Skin::validate` refuses the whole skin
                // otherwise): a clock field too close to the bottom edge is left out.
                if y + CELL_H > bh {
                    continue;
                }
                let end = p.x + p.w;
                let cell = |slot: u32| Rect {
                    x: end - slot * CELL_W,
                    y,
                    w: CELL_W,
                    h: CELL_H,
                };
                let digits = [
                    (5, TimeDigit::MinutesTens),
                    (4, TimeDigit::MinutesUnits),
                    (2, TimeDigit::SecondsTens),
                    (1, TimeDigit::SecondsUnits),
                ];
                for (slot, digit) in digits {
                    out.push(Widget {
                        rect: cell(slot),
                        anchor: START,
                        kind: WidgetKind::Text {
                            text: TextRead::Time(digit),
                            font: FontId(font_index),
                            scroll: false,
                        },
                    });
                }
                out.push(Widget {
                    rect: cell(3),
                    anchor: START,
                    kind: WidgetKind::Image {
                        sprite: colon,
                        fill: Fill::Once,
                    },
                });
            }
        }
    }
    out.truncate(MAX_WIDGETS_PER_PANEL);
    out
}

/// The skin from its parts (the body first, then each drawer closed and open), plus the composed
/// playlist and library panels.
fn assemble(tree: &wms::Tree, arts: &[SkinImage], parts: Vec<Part>) -> Result<Skin, SkinError> {
    let main = parts.first().ok_or(SkinError::Malformed)?;
    let main_id = main.id.clone();
    let main_w = main.comp.normal.width();
    let vis_background = vis_background(&main.comp.normal, &main.comp.widgets);

    let mut images = Vec::new();
    let mut ids = Vec::with_capacity(parts.len());
    for part in &parts {
        ids.push(part_images(&part.comp, arts, &mut images)?);
    }

    // One pixel font per text colour, at most two; later colours use the first.
    let mut colours: Vec<Rgb> = Vec::new();
    for p in parts.iter().flat_map(|p| &p.comp.widgets) {
        if let Kind::Title { colour } | Kind::Time { colour } = &p.kind
            && colours.len() < 2
            && !colours.contains(colour)
        {
            colours.push(*colour);
        }
    }
    let mut fonts = Vec::new();
    for colour in &colours {
        let (img, font) = pixel_font(*colour, image_id(&images)?);
        images.push(img);
        fonts.push(font);
    }

    let mut panels = Vec::with_capacity(parts.len() + 2);
    for (part, ids) in parts.iter().zip(&ids) {
        panels.push(Panel {
            id: part.id.clone(),
            title: part.title.clone(),
            size: Size {
                w: part.comp.normal.width(),
                h: part.comp.normal.height(),
            },
            resize: None,
            shape: part.comp.shape.clone(),
            dock: Dock {
                group: Some("main".to_owned()),
                attach: part.attach.clone(),
                snap: true,
            },
            visible_at_start: part.visible,
            widgets: part_widgets(part, ids, &fonts, &colours),
        });
    }

    let all_widgets = || panels.iter().flat_map(|p| &p.widgets);
    let toggles_playlist = all_widgets().any(|w| {
        matches!(
            &w.kind,
            WidgetKind::Button {
                command: Command::Window(WindowCommand::TogglePanel(id)),
                ..
            } if *id == playlist_id()
        )
    });
    // The skin shows the queue itself (in a drawer, say): the composed playlist starts hidden
    // and can be closed.
    let own_list = all_widgets().any(|w| matches!(w.kind, WidgetKind::TrackList));

    // The list panels' colours: the skin's own playlist element, else dark defaults.
    let playlist = tree.elements.iter().find(|e| e.tag == "playlist");
    let colour_of = |name: &str| playlist.and_then(|e| e.attr(name)).and_then(parse_colour);
    let list_background = colour_of("backgroundcolor");
    let list_text = colour_of("foregroundcolor");
    let title_colour = parts
        .iter()
        .flat_map(|p| &p.comp.widgets)
        .find_map(|p| match &p.kind {
            Kind::Title { colour } => Some(*colour),
            _ => None,
        });
    let vis_bar = title_colour
        .filter(|c| *c != vis_background)
        .unwrap_or(VIS_DEFAULT_BAR);
    let style = ListStyle {
        background: list_background.unwrap_or(Rgb { r: 0, g: 0, b: 0 }),
        bar: Rgb {
            r: 40,
            g: 40,
            b: 40,
        },
        text: list_text.or(title_colour).unwrap_or(Rgb {
            r: 255,
            g: 255,
            b: 255,
        }),
    };
    let chrome = image_id(&images)?;
    images.push(chrome_image(&style));

    panels.push(list_panel(
        ListPanel {
            id: playlist_id(),
            title: "Playlist".to_owned(),
            kind: ListKind::Tracks,
            size: Size {
                w: main_w.clamp(240, 600),
                h: 232,
            },
            attach: Some(Attach {
                to: main_id.clone(),
                edge: Edge::Bottom,
            }),
            group: Some("main".to_owned()),
            visible_at_start: !own_list,
            close: toggles_playlist || own_list,
            toggles: Some(library_id()),
        },
        chrome,
    ));
    panels.push(list_panel(
        ListPanel {
            id: library_id(),
            title: "Library".to_owned(),
            kind: ListKind::Library,
            size: Size { w: 400, h: 300 },
            attach: Some(Attach {
                to: main_id,
                edge: Edge::Right,
            }),
            group: Some("main".to_owned()),
            visible_at_start: false,
            close: true,
            toggles: None,
        },
        chrome,
    ));

    Ok(Skin {
        info: SkinInfo {
            engine: "wmz".to_owned(),
            name: String::new(),
        },
        panels,
        images,
        fonts,
        visualiser: Some(VisPalette {
            background: vis_background,
            grid: vis_background,
            bars: vec![vis_bar],
            scope: vec![vis_bar],
            peak: vis_bar,
        }),
        theme: SkinTheme {
            list_text,
            list_background,
            ..SkinTheme::default()
        },
    })
}
