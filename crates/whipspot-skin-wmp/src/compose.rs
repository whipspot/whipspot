//! The static composition (D5): a VIEW painted once, as it looks before any script runs, into a
//! normal layer (and a pressed layer when some element has a `downImage`), plus the list of
//! elements that can become live widgets. A [`Scope`] paints part of the view: the body without
//! its drawers, or one drawer, closed or open (MD193).
//!
//! WMP's stacking: the children of an element are drawn in `zIndex` order, those with a negative
//! `zIndex` under the element's own background and the rest over it (a picture with a hole shows
//! what lies behind it). Both colour keys of an element, `transparencyColor` and
//! `clippingColor`, are left out of its picture.
//!
//! Everything is bounded: art decoded at most once per key and at most `MAX_ART_FILES` files, each
//! decode under the remaining RGBA budget, shared by every painter of one skin ([`Arts`]); total
//! pixel writes under `PAINT_BUDGET`, also shared; the tree depth is capped by the `.wms` reader,
//! so the recursion is shallow. No script file is ever read.

use std::collections::HashMap;

use whipspot_core::art::MAX_ART_SIDE;
use whipspot_core::skin::{
    Command, ImageId, MAX_IMAGE_BYTES_TOTAL, Rgb, Shape, SkinError, SkinImage, Value,
};
use whipspot_skin_kit::bmp;
use whipspot_skin_kit::font::{CELL_H, CELL_W, pixel_font};
use whipspot_skin_kit::image::{decode_jpeg, decode_png};
use whipspot_skin_kit::shape::from_alpha;
use whipspot_skin_kit::zip::{Archive, key_of};

use crate::geometry::{Prop, Resolver};
use crate::widgets::{
    is_group_element, normalise_onclick, onclick_command, parse_colour, slider_value, tag_command,
    uses_background_image,
};
use crate::wms::{Element, Tree};

/// Most distinct art files decoded for one skin; the rest read as missing.
pub(crate) const MAX_ART_FILES: usize = 256;
/// Most pixel writes (and mapping-image scans) for one skin.
pub(crate) const PAINT_BUDGET: u64 = 64 * 1024 * 1024;
/// Longest side of a slider thumb that is kept.
pub(crate) const MAX_THUMB_SIDE: u32 = 256;
/// Most characters of a static label that are drawn.
const MAX_LABEL_CHARS: usize = 64;

const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

/// The colour keys of an element: `transparencyColor` and `clippingColor`. A pixel equal to
/// either is not drawn.
pub(crate) type Keys = [Option<[u8; 3]>; 2];

/// The keys of `el`.
fn keys_of(el: &Element) -> Keys {
    let key = |name: &str| {
        el.attr(name)
            .and_then(parse_colour)
            .map(|c| [c.r, c.g, c.b])
    };
    [key("transparencycolor"), key("clippingcolor")]
}

/// Whether the straight RGBA pixel `p` is left out: alpha 0, or equal to a key.
pub(crate) fn left_out(p: &[u8], keys: Keys) -> bool {
    p[3] == 0 || keys.contains(&Some(rgb(p)))
}

/// `visible` is absent or `true`.
fn shown_attr(el: &Element) -> bool {
    el.attr("visible")
        .is_none_or(|v| v.trim().eq_ignore_ascii_case("true"))
}

/// `visible` is literally `false`: the state a script changes, as an open drawer does.
fn hidden_literal(el: &Element) -> bool {
    el.attr("visible")
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("false"))
}

/// A label colour's pixel font: its sheet and each glyph's top-left in it.
type LabelFont = (SkinImage, Vec<(char, (u32, u32))>);

/// A slider thumb: indexes into [`Arts::images`] and the colour keys to apply.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Thumb {
    pub normal: usize,
    pub down: Option<usize>,
    pub keys: Keys,
}

/// What an element may become.
#[derive(Debug, Clone)]
pub(crate) enum Kind {
    Button(Command),
    Slider {
        value: Value,
        vertical: bool,
        thumb: Option<Thumb>,
    },
    Visualiser,
    /// The skin's own playlist element: the play queue.
    List,
    Title {
        colour: Rgb,
    },
    Time {
        colour: Rgb,
    },
}

/// An element that may become a widget, in canvas coordinates (can lie outside the canvas).
#[derive(Debug, Clone)]
pub(crate) struct Cand {
    /// The paint position of the art the widget sits on.
    pub seq: u32,
    pub x: i64,
    pub y: i64,
    pub w: i64,
    pub h: i64,
    pub kind: Kind,
}

/// A `downImage` blit waiting for the finished normal layer.
struct Pending {
    art: usize,
    x: i64,
    y: i64,
    w: i64,
    h: i64,
    tiled: bool,
    keys: Keys,
}

/// A candidate kept and moved into the cropped panel.
#[derive(Debug, Clone)]
pub(crate) struct Placed {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub kind: Kind,
}

/// A canvas box `(x0, y0, x1, y1)`, ends exclusive.
pub(crate) type CanvasBox = (i64, i64, i64, i64);

/// The finished composition of one panel.
pub(crate) struct Composition {
    pub normal: SkinImage,
    pub pressed: Option<SkinImage>,
    pub shape: Shape,
    pub widgets: Vec<Placed>,
    /// The art painted over the visualiser (the spectrum is drawn under it): its top-left in the
    /// panel and its picture, transparent where the spectrum shows.
    pub overlay: Option<(u32, u32, SkinImage)>,
    /// The panel's box on the canvas.
    pub bbox: CanvasBox,
    /// Per canvas pixel: something was painted there (before the crop).
    pub painted: Vec<bool>,
}

/// Which part of the canvas a composition keeps.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Crop {
    /// The box of everything painted, and the visualiser.
    Content,
    /// The painted pixels beside `main` (a canvas box), on its left or right side, from `main`'s
    /// top down: a panel that attaches to that side of the panel cropped to `main`. Nothing
    /// painted there leaves no composition.
    Beside { main: CanvasBox, left: bool },
}

/// Which part of a VIEW a painter paints, and what it knows beyond the fixed tables.
#[derive(Debug, Clone, Default)]
pub(crate) struct Scope {
    /// Elements left out, with everything under them.
    pub skip: Vec<usize>,
    /// One element painted at this `left` instead of its own.
    pub moved: Option<(usize, i64)>,
    /// Elements whose `visible` is literally `false` are painted too: the open state of a
    /// drawer, whose script would show them.
    pub reveal: bool,
    /// Commands for `onclick` texts outside the fixed table, by normalised text.
    pub calls: HashMap<String, Command>,
}

/// The decoded art and the paint budget of one skin, shared by its painters.
pub(crate) struct Arts<'a, 'z> {
    archive: &'a Archive<'z>,
    cache: HashMap<String, Option<usize>>,
    pub images: Vec<SkinImage>,
    attempts: usize,
    rgba_left: usize,
    budget: u64,
}

impl<'a, 'z> Arts<'a, 'z> {
    pub fn new(archive: &'a Archive<'z>) -> Arts<'a, 'z> {
        Arts {
            archive,
            cache: HashMap::new(),
            images: Vec::new(),
            attempts: 0,
            rgba_left: MAX_IMAGE_BYTES_TOTAL,
            budget: PAINT_BUDGET,
        }
    }

    fn charge(&mut self, area: u64) -> Result<(), SkinError> {
        self.budget = self.budget.checked_sub(area).ok_or(SkinError::TooLarge)?;
        Ok(())
    }

    /// The decoded art for an image attribute, or `None` when it is absent, not in the archive,
    /// not a BMP, PNG or JPEG (a GIF included), over a budget or damaged. A script file is never
    /// read. Each key is decoded once.
    fn get(&mut self, name: Option<&str>) -> Option<usize> {
        let key = key_of(name?.trim().as_bytes())?;
        if let Some(found) = self.cache.get(&key) {
            return *found;
        }
        let found = self.decode(&key);
        self.cache.insert(key, found);
        found
    }

    fn decode(&mut self, key: &str) -> Option<usize> {
        if self.attempts >= MAX_ART_FILES || key.ends_with(".js") || key.ends_with(".wms") {
            return None;
        }
        self.attempts += 1;
        let bytes = self.archive.read(key)?.ok()?;
        let budget = self.rgba_left;
        let img = if bytes.starts_with(b"BM") {
            bmp::decode(&bytes, budget).ok()?
        } else if bytes.starts_with(&PNG_SIGNATURE) {
            decode_png(&bytes, budget).ok()?
        } else if bytes.starts_with(&[0xff, 0xd8]) {
            decode_jpeg(&bytes, budget).ok()?
        } else {
            return None;
        };
        self.rgba_left = self.rgba_left.checked_sub(img.rgba().len())?;
        self.images.push(img);
        Some(self.images.len() - 1)
    }
}

pub(crate) struct Painter<'p, 'a, 'z> {
    arts: &'p mut Arts<'a, 'z>,
    tree: &'a Tree,
    res: Resolver<'a>,
    scope: Scope,
    cw: i64,
    ch: i64,
    normal: Vec<u8>,
    owner: Vec<u32>,
    pending: Vec<Pending>,
    seq: u32,
    cands: Vec<Cand>,
    title_taken: bool,
    /// The pixel font of each label colour drawn so far.
    fonts: HashMap<[u8; 3], LabelFont>,
}

/// The pixel (r, g, b) of a straight RGBA slice.
fn rgb(p: &[u8]) -> [u8; 3] {
    [p[0], p[1], p[2]]
}

/// The area of the rectangle `(x, y, w, h)` clipped to a `cw` x `ch` canvas: the pixel writes a
/// fill, blit or owner stamp over it costs. Callers charge it to the budget before the loop runs.
fn clipped_area((cw, ch): (i64, i64), (x, y, w, h): (i64, i64, i64, i64)) -> u64 {
    let (x0, y0, x1, y1) = (x.max(0), y.max(0), (x + w).min(cw), (y + h).min(ch));
    if x1 <= x0 || y1 <= y0 {
        return 0;
    }
    ((x1 - x0) * (y1 - y0)) as u64
}

/// Copies `img` into `dst` (a `cw` x `ch` RGBA canvas) over the rectangle `(x, y, w, h)`, clipped
/// to the canvas, tiled or once from the rectangle's top-left. Pixels with alpha 0 and pixels
/// equal to a key are skipped. `owner`, when given, takes `seq` for every written pixel. The
/// caller has charged `clipped_area` already.
#[allow(clippy::too_many_arguments)]
fn blit(
    dst: &mut [u8],
    mut owner: Option<&mut [u32]>,
    (cw, ch): (i64, i64),
    img: &SkinImage,
    (x, y, w, h): (i64, i64, i64, i64),
    tiled: bool,
    keys: Keys,
    seq: u32,
) {
    let (iw, ih) = (i64::from(img.width()), i64::from(img.height()));
    let (x0, y0, x1, y1) = (x.max(0), y.max(0), (x + w).min(cw), (y + h).min(ch));
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let src = img.rgba();
    for cy in y0..y1 {
        let sy = if tiled {
            (cy - y).rem_euclid(ih)
        } else {
            cy - y
        };
        if sy >= ih {
            continue;
        }
        for cx in x0..x1 {
            let sx = if tiled {
                (cx - x).rem_euclid(iw)
            } else {
                cx - x
            };
            if sx >= iw {
                continue;
            }
            let s = ((sy * iw + sx) * 4) as usize;
            let p = &src[s..s + 4];
            if left_out(p, keys) {
                continue;
            }
            let d = ((cy * cw + cx) * 4) as usize;
            dst[d..d + 4].copy_from_slice(p);
            if let Some(o) = owner.as_deref_mut() {
                o[(cy * cw + cx) as usize] = seq;
            }
        }
    }
}

/// A static label's text: a literal `value` (no `wmpprop:` or `jscript:` binding) of printable
/// ASCII, cut to `MAX_LABEL_CHARS`; `None` when there is nothing to draw.
fn label_text(value: Option<&str>) -> Option<String> {
    let v = value?.trim();
    let lower = v.to_ascii_lowercase();
    if v.is_empty() || lower.starts_with("wmpprop:") || lower.starts_with("jscript:") {
        return None;
    }
    if !v.chars().all(|c| c.is_ascii_graphic() || c == ' ') {
        return None;
    }
    Some(v.chars().take(MAX_LABEL_CHARS).collect())
}

impl<'p, 'a, 'z> Painter<'p, 'a, 'z> {
    /// A painter for `view`, whose width and height must resolve to `1..=MAX_ART_SIDE`.
    pub fn new(
        arts: &'p mut Arts<'a, 'z>,
        tree: &'a Tree,
        view: usize,
        scope: Scope,
    ) -> Result<Painter<'p, 'a, 'z>, SkinError> {
        let mut res = Resolver::new(tree);
        let (cw, ch) = view_size(&mut res, view)?;
        let pixels = (cw * ch) as usize;
        Ok(Painter {
            arts,
            tree,
            res,
            scope,
            cw,
            ch,
            normal: vec![0; pixels * 4],
            owner: vec![0; pixels],
            pending: Vec::new(),
            seq: 0,
            cands: Vec::new(),
            title_taken: false,
            fonts: HashMap::new(),
        })
    }

    /// The literal `zIndex` of an element, 0 when absent or not a plain integer.
    fn zindex(&self, e: usize) -> i64 {
        zindex(self.tree, e)
    }

    /// The command of an `onclick`: the fixed table, else the scope's script calls.
    fn onclick(&self, text: Option<&str>) -> Option<Command> {
        let text = text?;
        onclick_command(text).or_else(|| self.scope.calls.get(&normalise_onclick(text)).cloned())
    }

    pub fn paint(&mut self, e: usize, ox: i64, oy: i64) -> Result<(), SkinError> {
        let tree = self.tree;
        let Some(el) = tree.get(e) else {
            return Ok(());
        };
        if self.scope.skip.contains(&e) {
            return Ok(());
        }
        let tag = el.tag.as_str();
        let effects = matches!(tag, "effects" | "wmpeffects");
        let shown = shown_attr(el) || (self.scope.reveal && hidden_literal(el));
        if !shown && !effects {
            return Ok(());
        }
        let left = match self.scope.moved {
            Some((m, left)) if m == e => Some(left),
            _ => self.res.value(e, Prop::Left),
        };
        let (Some(left), Some(top)) = (left, self.res.value(e, Prop::Top)) else {
            return Ok(());
        };
        let art_attr = if uses_background_image(tag) {
            "backgroundimage"
        } else {
            "image"
        };
        let art = self.arts.get(el.attr(art_attr));
        let image_size = art.map(|i| {
            let img = &self.arts.images[i];
            (img.width(), img.height())
        });
        let label = (tag == "text")
            .then(|| label_text(el.attr("value")))
            .flatten();
        let w = self
            .res
            .value(e, Prop::Width)
            .or(image_size.map(|s| i64::from(s.0)))
            .or(label.as_ref().map(|l| (l.len() as i64) * i64::from(CELL_W)));
        let mut h = self
            .res
            .value(e, Prop::Height)
            .or(image_size.map(|s| i64::from(s.1)));
        if h.is_none() && tag == "text" {
            h = Some(i64::from(CELL_H));
        }
        let (Some(w), Some(h)) = (w, h) else {
            return Ok(());
        };
        if w < 1 || h < 1 {
            return Ok(());
        }
        let (x, y) = (ox + left, oy + top);

        // Children with a negative zIndex lie under this element's own picture.
        let mut children = el.children.clone();
        children.sort_by_key(|c| self.zindex(*c));
        let under = children.partition_point(|c| self.zindex(*c) < 0);
        for &c in &children[..under] {
            self.paint(c, x, y)?;
        }

        self.seq += 1;
        let me = self.seq;
        let keys = keys_of(el);
        let slider = matches!(
            tag,
            "slider" | "seekslider" | "volumeslider" | "balanceslider" | "customslider"
        );
        let tiled = el
            .attr("backgroundtiled")
            .or(if slider { el.attr("tiled") } else { None })
            .is_some_and(|v| v.trim().eq_ignore_ascii_case("true"));

        if let Some(c) = el.attr("backgroundcolor").and_then(parse_colour) {
            self.fill(x, y, w, h, c, me)?;
        }
        if let Some(i) = art {
            let (cw, ch) = (self.cw, self.ch);
            self.arts.charge(clipped_area((cw, ch), (x, y, w, h)))?;
            blit(
                &mut self.normal,
                Some(self.owner.as_mut_slice()),
                (cw, ch),
                &self.arts.images[i],
                (x, y, w, h),
                tiled,
                keys,
                me,
            );
        }
        if let Some(d) = self.arts.get(el.attr("downimage")) {
            self.pending.push(Pending {
                art: d,
                x,
                y,
                w,
                h,
                tiled,
                keys,
            });
        }
        if let Some(text) = &label {
            self.label(el, text, (x, y, w, h), me)?;
        }

        self.candidates(e, (x, y, w, h), me, keys)?;

        for &c in &children[under..] {
            self.paint(c, x, y)?;
        }
        Ok(())
    }

    fn fill(&mut self, x: i64, y: i64, w: i64, h: i64, c: Rgb, seq: u32) -> Result<(), SkinError> {
        let (x0, y0, x1, y1) = (
            x.max(0),
            y.max(0),
            (x + w).min(self.cw),
            (y + h).min(self.ch),
        );
        if x1 <= x0 || y1 <= y0 {
            return Ok(());
        }
        self.arts.charge(((x1 - x0) * (y1 - y0)) as u64)?;
        for cy in y0..y1 {
            for cx in x0..x1 {
                let at = (cy * self.cw + cx) as usize;
                self.normal[at * 4..at * 4 + 4].copy_from_slice(&[c.r, c.g, c.b, 255]);
                self.owner[at] = seq;
            }
        }
        Ok(())
    }

    /// Draws a static label's `text` with the pixel font in the element's `foregroundColor`
    /// (white when absent), from the rectangle's top-left, clipped to it.
    fn label(
        &mut self,
        el: &Element,
        text: &str,
        rect: (i64, i64, i64, i64),
        seq: u32,
    ) -> Result<(), SkinError> {
        let (x, y, w, h) = rect;
        let colour = el
            .attr("foregroundcolor")
            .and_then(parse_colour)
            .unwrap_or(Rgb {
                r: 255,
                g: 255,
                b: 255,
            });
        let key = [colour.r, colour.g, colour.b];
        self.arts
            .charge(clipped_area((self.cw, self.ch), (x, y, w, h)))?;
        let (img, glyphs) = self.fonts.entry(key).or_insert_with(|| {
            let (img, font) = pixel_font(colour, ImageId(0));
            let glyphs = font
                .glyphs
                .iter()
                .map(|(c, s)| (*c, (s.rect.x, s.rect.y)))
                .collect();
            (img, glyphs)
        });
        let iw = i64::from(img.width());
        let (cell_w, cell_h) = (i64::from(CELL_W), i64::from(CELL_H));
        for (i, c) in text.chars().enumerate() {
            let Some(&(gx, gy)) = glyphs.iter().find(|(g, _)| *g == c).map(|(_, at)| at) else {
                continue;
            };
            let gx0 = x + i as i64 * cell_w;
            for dy in 0..cell_h.min(h) {
                for dx in 0..cell_w {
                    let (cx, cy) = (gx0 + dx, y + dy);
                    if cx >= x + w || cx < 0 || cy < 0 || cx >= self.cw || cy >= self.ch {
                        continue;
                    }
                    let s = (((i64::from(gy) + dy) * iw + i64::from(gx) + dx) * 4) as usize;
                    let p = &img.rgba()[s..s + 4];
                    if p[3] == 0 {
                        continue;
                    }
                    let d = (cy * self.cw + cx) as usize;
                    self.normal[d * 4..d * 4 + 4].copy_from_slice(p);
                    self.owner[d] = seq;
                }
            }
        }
        Ok(())
    }

    /// Marks `rect` as drawn live by the element `seq`: what is under it is hidden.
    fn stamp(&mut self, (x, y, w, h): (i64, i64, i64, i64), seq: u32) -> Result<(), SkinError> {
        let (x0, y0) = (x.max(0), y.max(0));
        let (x1, y1) = ((x + w).min(self.cw), (y + h).min(self.ch));
        self.arts
            .charge(clipped_area((self.cw, self.ch), (x, y, w, h)))?;
        for cy in y0..y1.max(y0) {
            for cx in x0..x1.max(x0) {
                self.owner[(cy * self.cw + cx) as usize] = seq;
            }
        }
        Ok(())
    }

    /// Records what element `e` may become, painted at `rect` as paint position `seq`.
    fn candidates(
        &mut self,
        e: usize,
        rect: (i64, i64, i64, i64),
        seq: u32,
        keys: Keys,
    ) -> Result<(), SkinError> {
        let tree = self.tree;
        let Some(el) = tree.get(e) else {
            return Ok(());
        };
        let tag = el.tag.as_str();
        let (x, y, w, h) = rect;
        let push = |this: &mut Self, kind: Kind| {
            this.cands.push(Cand {
                seq,
                x,
                y,
                w,
                h,
                kind,
            });
        };
        match tag {
            "playbutton" | "pausebutton" | "stopbutton" | "nextbutton" | "prevbutton" => {
                if let Some(c) = tag_command(tag) {
                    push(self, Kind::Button(c));
                }
            }
            "button" => {
                if let Some(c) = self.onclick(el.attr("onclick")) {
                    push(self, Kind::Button(c));
                }
            }
            "seekslider" | "volumeslider" | "balanceslider" | "slider" => {
                if let Some(value) = slider_value(tag, el.attr("value")) {
                    let vertical = el
                        .attr("direction")
                        .is_some_and(|d| d.trim().eq_ignore_ascii_case("vertical"));
                    let thumb = self.arts.get(el.attr("thumbimage")).map(|normal| Thumb {
                        normal,
                        down: self.arts.get(el.attr("thumbdownimage")),
                        keys,
                    });
                    push(
                        self,
                        Kind::Slider {
                            value,
                            vertical,
                            thumb,
                        },
                    );
                }
            }
            "effects" | "wmpeffects" => {
                push(self, Kind::Visualiser);
                // The visualiser is drawn live over this rectangle, so it hides what is under it.
                self.stamp(rect, seq)?;
            }
            "playlist" => {
                push(self, Kind::List);
                self.stamp(rect, seq)?;
            }
            "text" => {
                let value = el.attr("value").map(|v| v.trim().to_ascii_lowercase());
                let colour = el
                    .attr("foregroundcolor")
                    .and_then(parse_colour)
                    .unwrap_or(Rgb {
                        r: 255,
                        g: 255,
                        b: 255,
                    });
                match value.as_deref() {
                    Some(v) if v.contains("currentpositionstring") => {
                        push(self, Kind::Time { colour });
                    }
                    v if !self.title_taken
                        && v.is_none_or(|v| {
                            v.contains("currentmedia") || v.contains("metadata")
                        }) =>
                    {
                        self.title_taken = true;
                        push(self, Kind::Title { colour });
                    }
                    _ => {}
                }
            }
            "buttongroup" => self.group_elements(e, (x, y), seq)?,
            _ => {}
        }
        Ok(())
    }

    /// The elements of a button group: each one's rectangle is the bounding box of its
    /// `mappingColor` pixels in the group's mapping image, placed at the group's origin.
    fn group_elements(&mut self, group: usize, at: (i64, i64), seq: u32) -> Result<(), SkinError> {
        let tree = self.tree;
        let Some(el) = tree.get(group) else {
            return Ok(());
        };
        let mut children = el.children.clone();
        children.sort_by_key(|c| self.zindex(*c));
        let mut map: Option<Option<usize>> = None;
        for c in children {
            let Some(ce) = tree.get(c) else { continue };
            if !is_group_element(&ce.tag) {
                continue;
            }
            let shown = shown_attr(ce) || (self.scope.reveal && hidden_literal(ce));
            let command = if ce.tag == "buttonelement" {
                self.onclick(ce.attr("onclick"))
            } else {
                tag_command(&ce.tag)
            };
            let colour = ce.attr("mappingcolor").and_then(parse_colour);
            let (true, Some(command), Some(colour)) = (shown, command, colour) else {
                continue;
            };
            let map_idx = *map.get_or_insert_with(|| self.arts.get(el.attr("mappingimage")));
            let Some(mi) = map_idx else { continue };
            let (mw, mh) = (self.arts.images[mi].width(), self.arts.images[mi].height());
            self.arts.charge(u64::from(mw) * u64::from(mh))?;
            let Some((bx, by, bw, bh)) =
                bbox_of_colour(&self.arts.images[mi], [colour.r, colour.g, colour.b])
            else {
                continue;
            };
            self.cands.push(Cand {
                seq,
                x: at.0 + i64::from(bx),
                y: at.1 + i64::from(by),
                w: i64::from(bw),
                h: i64::from(bh),
                kind: Kind::Button(command),
            });
        }
        Ok(())
    }

    /// Finishes: the pressed layer, the crop, the kept widgets, the art over the visualiser and
    /// the window shape. `cover` marks canvas pixels another panel draws over this one (they are
    /// cleared first). `Ok(None)` only for a [`Crop::Beside`] with nothing beside.
    pub fn finish(
        mut self,
        cover: Option<&[bool]>,
        crop: Crop,
    ) -> Result<Option<Composition>, SkinError> {
        let (cw, ch) = (self.cw, self.ch);

        if let Some(cover) = cover {
            for (at, covered) in cover.iter().enumerate().take(self.owner.len()) {
                if *covered {
                    self.normal[at * 4..at * 4 + 4].copy_from_slice(&[0; 4]);
                    self.owner[at] = u32::MAX;
                }
            }
        }

        // The first visualiser, clipped to the canvas, and its paint position.
        let mut vis: Option<((i64, i64, i64, i64), u32)> = None;
        for c in &self.cands {
            if matches!(c.kind, Kind::Visualiser) {
                let (x0, y0, x1, y1) = (
                    c.x.max(0),
                    c.y.max(0),
                    (c.x + c.w).min(cw),
                    (c.y + c.h).min(ch),
                );
                if x1 > x0 && y1 > y0 {
                    vis = Some(((x0, y0, x1 - x0, y1 - y0), c.seq));
                }
                break;
            }
        }

        // The content box: every pixel with alpha, and the visualiser.
        let (mut bx0, mut by0, mut bx1, mut by1) = (cw, ch, 0i64, 0i64);
        for cy in 0..ch {
            let row = &self.normal[(cy * cw * 4) as usize..((cy + 1) * cw * 4) as usize];
            let first = row.chunks_exact(4).position(|p| p[3] != 0);
            if let Some(first) = first {
                let last = row
                    .chunks_exact(4)
                    .rposition(|p| p[3] != 0)
                    .unwrap_or(first);
                bx0 = bx0.min(first as i64);
                bx1 = bx1.max(last as i64 + 1);
                by0 = by0.min(cy);
                by1 = by1.max(cy + 1);
            }
        }
        if let Some(((x, y, w, h), _)) = vis {
            bx0 = bx0.min(x);
            by0 = by0.min(y);
            bx1 = bx1.max(x + w);
            by1 = by1.max(y + h);
        }
        match crop {
            Crop::Content => {
                if bx1 <= bx0 || by1 <= by0 {
                    (bx0, by0, bx1, by1) = (0, 0, cw, ch);
                }
            }
            Crop::Beside { main, left } => {
                let (mx0, my0, mx1, _) = main;
                if bx1 <= bx0 || by1 <= my0 {
                    return Ok(None);
                }
                if left {
                    if bx0 >= mx0 {
                        return Ok(None);
                    }
                    bx1 = mx0;
                } else {
                    if bx1 <= mx1 {
                        return Ok(None);
                    }
                    bx0 = mx1;
                }
                by0 = my0;
            }
        }
        let (bw, bh) = (bx1 - bx0, by1 - by0);

        // The pressed layer: the finished normal layer with every downImage on top.
        let pressed_full = if self.pending.is_empty() {
            None
        } else {
            let mut layer = self.normal.clone();
            let pending = std::mem::take(&mut self.pending);
            for p in &pending {
                self.arts
                    .charge(clipped_area((cw, ch), (p.x, p.y, p.w, p.h)))?;
                blit(
                    &mut layer,
                    None,
                    (cw, ch),
                    &self.arts.images[p.art],
                    (p.x, p.y, p.w, p.h),
                    p.tiled,
                    p.keys,
                    0,
                );
            }
            if let Some(cover) = cover {
                for (at, covered) in cover.iter().enumerate().take(self.owner.len()) {
                    if *covered {
                        layer[at * 4..at * 4 + 4].copy_from_slice(&[0; 4]);
                    }
                }
            }
            Some(layer)
        };

        let crop_px = |full: &[u8]| -> Vec<u8> {
            let mut out = Vec::with_capacity((bw * bh * 4) as usize);
            for cy in by0..by1 {
                let from = ((cy * cw + bx0) * 4) as usize;
                out.extend_from_slice(&full[from..from + (bw * 4) as usize]);
            }
            out
        };
        let normal_px = crop_px(&self.normal);
        let pressed_px = pressed_full.as_deref().map(crop_px);
        let side = |v: i64| u32::try_from(v).map_err(|_| SkinError::Malformed);

        // The visualiser kept in the box, and the art painted over it after it.
        let vis =
            vis.filter(|((x, y, w, h), _)| *x >= bx0 && *y >= by0 && x + w <= bx1 && y + h <= by1);
        let mut overlay = None;
        if let Some(((x, y, w, h), seq)) = vis {
            let mut px = vec![0u8; (w * h * 4) as usize];
            let mut any = false;
            for cy in y..y + h {
                for cx in x..x + w {
                    let at = (cy * cw + cx) as usize;
                    if self.owner[at] > seq && self.owner[at] != u32::MAX {
                        let d = (((cy - y) * w + (cx - x)) * 4) as usize;
                        px[d..d + 4].copy_from_slice(&self.normal[at * 4..at * 4 + 4]);
                        any = true;
                    }
                }
            }
            if any {
                let img = SkinImage::new(side(w)?, side(h)?, px).ok_or(SkinError::Malformed)?;
                overlay = Some((side(x - bx0)?, side(y - by0)?, img));
            }
        }

        // The window shape: the cropped alpha. The visualiser shows through the art painted under
        // it (a screen clipped by its own colour key); with nothing painted under it, its whole
        // rectangle is forced opaque.
        let mut mask = normal_px.clone();
        let backdrop = vis.is_some_and(|((x, y, w, h), seq)| {
            (y..y + h).any(|cy| {
                (x..x + w).any(|cx| {
                    let at = (cy * cw + cx) as usize;
                    self.owner[at] <= seq && self.normal[at * 4 + 3] != 0
                })
            })
        });
        if let (Some(((x, y, w, h), _)), false) = (vis, backdrop) {
            for cy in y..y + h {
                for cx in x..x + w {
                    let at = (((cy - by0) * bw + (cx - bx0)) * 4 + 3) as usize;
                    mask[at] = 255;
                }
            }
        }
        let (bw32, bh32) = (side(bw)?, side(bh)?);
        let mask_img = SkinImage::new(bw32, bh32, mask).ok_or(SkinError::Malformed)?;
        let shape = from_alpha(&mask_img);
        let normal = SkinImage::new(bw32, bh32, normal_px).ok_or(SkinError::Malformed)?;
        let pressed = match pressed_px {
            Some(px) => Some(SkinImage::new(bw32, bh32, px).ok_or(SkinError::Malformed)?),
            None => None,
        };

        // Kept widgets: not covered by later art, wholly inside the box.
        let mut widgets = Vec::new();
        let mut seen_vis = false;
        for c in &self.cands {
            let (x, y, w, h) = match &c.kind {
                Kind::Visualiser => {
                    if seen_vis {
                        continue;
                    }
                    seen_vis = true;
                    match vis {
                        Some((v, _)) => v,
                        None => continue,
                    }
                }
                _ => (c.x, c.y, c.w, c.h),
            };
            if w < 1 || h < 1 || x < bx0 || y < by0 || x + w > bx1 || y + h > by1 {
                continue;
            }
            if matches!(c.kind, Kind::Button(_) | Kind::Slider { .. } | Kind::List) {
                let (mx, my) = (x + w / 2, y + h / 2);
                if self.owner[(my * cw + mx) as usize] > c.seq {
                    continue;
                }
            }
            widgets.push(Placed {
                x: (x - bx0) as u32,
                y: (y - by0) as u32,
                w: w as u32,
                h: h as u32,
                kind: c.kind.clone(),
            });
        }

        let painted = self.normal.chunks_exact(4).map(|p| p[3] != 0).collect();
        Ok(Some(Composition {
            normal,
            pressed,
            shape,
            widgets,
            overlay,
            bbox: (bx0, by0, bx1, by1),
            painted,
        }))
    }
}

/// The literal `zIndex` of an element, 0 when absent or not a plain integer.
pub(crate) fn zindex(tree: &Tree, e: usize) -> i64 {
    tree.get(e)
        .and_then(|el| el.attr("zindex"))
        .and_then(|s| s.trim().parse::<i64>().ok())
        .unwrap_or(0)
}

/// The width and height of `view`, each in `1..=MAX_ART_SIDE`.
pub(crate) fn view_size(res: &mut Resolver, view: usize) -> Result<(i64, i64), SkinError> {
    let side = |v: Option<i64>| {
        v.filter(|v| (1..=i64::from(MAX_ART_SIDE)).contains(v))
            .ok_or(SkinError::Unsupported)
    };
    Ok((
        side(res.value(view, Prop::Width))?,
        side(res.value(view, Prop::Height))?,
    ))
}

/// The bounding box `(x, y, w, h)` of the pixels of `img` whose RGB equals `colour`.
fn bbox_of_colour(img: &SkinImage, colour: [u8; 3]) -> Option<(u32, u32, u32, u32)> {
    let w = img.width() as usize;
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0usize, 0usize);
    let mut any = false;
    for (i, p) in img.rgba().chunks_exact(4).enumerate() {
        if rgb(p) == colour {
            let (x, y) = (i % w, i / w);
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x + 1);
            y1 = y1.max(y + 1);
            any = true;
        }
    }
    any.then(|| (x0 as u32, y0 as u32, (x1 - x0) as u32, (y1 - y0) as u32))
}
