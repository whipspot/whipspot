//! The artist page (D9: reached by links only) and the pieces of the explored album page that it
//! shares: the release tiles and the "More by" row.
//!
//! The artist page is one scroll area: breadcrumb, the hero row (round picture, ARTIST, the name,
//! a meta line, Play and Shuffle), Popular (the track table's own rows) and the Discography with
//! its chips over a grid of release tiles. Pure drawing: the request paths are `library.rs`'s and
//! what the page reports goes back to the sheet ([`ArtistOut`]).

use lune_ui::input::CursorIcon;
use lune_ui::paint::Rect;
use lune_ui::scene::LayerId;
use lune_ui::ui::{Sense, Ui};
use lune_ui::widgets::button::ButtonKind;
use lune_ui::widgets::scroll_area::{Bars, Extent, ScrollOptions};
use whipspot_core::provider::{Album, AlbumKind, ReleaseGroup};

use crate::art::{NoArt, cover};
use crate::browse::Show;
use crate::draw::{BoxKind, alpha, caps, page_box, page_boxes_scope, shadow, text, width};
use crate::grid::{self, Art, CardSpec, Covers, Status};
use crate::library::{ArtistOpen, GROUPS, LOAD_FAILED, Open, Slot};
use crate::likes::LikeBook;
use crate::link::{self, Face, Place};
use crate::pages::{self, Hero, HeroOut, Save};
use crate::theme::Look;
use crate::tracks::{BAND_H, Form, PaneResult, ROW_PITCH, TrackTable, play_visible};

/// The hero row's top below the content top (the breadcrumb is above it).
const HERO_TOP: f32 = 44.0;
/// The picture's widest and narrowest diameter; it shrinks with the content width before the
/// name does.
const PIC_MAX: f32 = 210.0;
const PIC_MIN: f32 = 110.0;
/// The name's size at the widest and the narrowest picture.
const NAME_MAX: f32 = 52.0;
const NAME_MIN: f32 = 32.0;
/// The gap between the picture and the text column.
const HERO_GAP: f32 = 36.0;
/// Section headings: their size, and the distance from one section's end to the next heading.
const HEADING: f32 = 22.0;
const SECTION_GAP: f32 = 30.0;
/// The Popular table's top below its heading.
const POPULAR_TOP: f32 = 36.0;
/// Chips: height, text size, padding each side and the gap between.
const CHIP_H: f32 = 32.0;
const CHIP_TEXT: f32 = 13.0;
const CHIP_PAD: f32 = 16.0;
const CHIP_GAP: f32 = 8.0;
/// The tiles' top below the chips, and the page's end padding.
const TILES_TOP: f32 = 24.0;
const BOTTOM_PAD: f32 = 36.0;
/// Paging: the next page is asked for when the view is within this many px of the end.
const NEAR_END: f32 = 420.0;
/// A tile row: caption height under the cover.
const TILE_TEXT: f32 = grid::TEXT_H;
/// The dark text of a selected chip (the chip itself is the light neutral).
const CHIP_DARK: [u8; 4] = [0x16, 0x10, 0x1E, 0xFF];

/// The artist page's retained state: which chip is selected and the Popular table's last press.
#[derive(Debug)]
pub(crate) struct ArtistView {
    uri: String,
    group: ReleaseGroup,
    table: TrackTable,
}

impl Default for ArtistView {
    fn default() -> Self {
        ArtistView {
            uri: String::new(),
            group: ReleaseGroup::All,
            table: TrackTable::default(),
        }
    }
}

/// What the page reported in a frame.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ArtistOut {
    /// The breadcrumb's link (back).
    pub crumb: bool,
    pub play: bool,
    pub shuffle: bool,
    /// The release tile (its index in the selected group's list) to open or play.
    pub open: Option<usize>,
    pub play_release: Option<usize>,
    /// The next page of the selected group is due.
    pub more: bool,
    /// The Retry of a failed header or group.
    pub retry: bool,
}

impl ArtistView {
    /// The group the chips have selected for `uri`.
    pub(crate) fn group(&self, uri: &str) -> ReleaseGroup {
        if self.uri == uri {
            self.group
        } else {
            ReleaseGroup::All
        }
    }

    /// Draws the page of `open` over `sheet`; `prev` is the breadcrumb's previous page ("" for
    /// none).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn frame(
        &mut self,
        ui: &mut Ui<'_>,
        sheet: Rect,
        open: &ArtistOpen,
        (prev, forced): (&str, Option<ReleaseGroup>),
        on: &Show<'_>,
        covers: &mut Covers<'_>,
        likes: &mut LikeBook,
    ) -> ArtistOut {
        if self.uri != open.uri {
            self.uri.clone_from(&open.uri);
            self.group = ReleaseGroup::All;
            self.table = TrackTable::default();
        }
        if let Some(g) = forced {
            self.group = g;
        }
        // A group that turned out empty is not offered: fall back to All.
        if self.group != ReleaseGroup::All && open.releases(self.group).items().is_empty() {
            self.group = ReleaseGroup::All;
        }
        let Show {
            look: k,
            state,
            player,
            ..
        } = *on;
        let (vp, c) = (grid::viewport(sheet), grid::content_rect(sheet));
        let scale = ui.painter().s;
        let mut out = ArtistOut::default();
        let group = self.group;
        let slot = open.releases(group);
        let head = open.head();
        let loaded = head.data().is_some();
        let popular_n = open.shown.items().len();
        let current = state.current.as_ref().map(|n| n.track.uri.as_str());
        let meta = meta_line(open);
        let tiles: Vec<CardSpec> = slot.items().iter().map(tile).collect();

        // Geometry (content space: y from the viewport's top).
        let geo = hero_geo(c.w);
        let name_size = geo.name_size;
        let hero_top = c.y + HERO_TOP;
        let text_h = geo.text_h;
        let hero_h = geo.pic.max(text_h);
        let hero_bottom = hero_top + hero_h;
        let mut y = hero_bottom + SECTION_GAP;
        let popular_y = y;
        if popular_n > 0 {
            y += POPULAR_TOP + BAND_H + popular_n as f32 * ROW_PITCH + SECTION_GAP;
        }
        let disco_y = y;
        let chip_list = if slot_known(open) {
            chips(open)
        } else {
            Vec::new()
        };
        let heading_w = {
            let p = ui.painter();
            width(p, "Discography", HEADING, k.decor().display_medium)
        };
        let chips_w: f32 = chip_list.iter().map(|ch| ch.w + CHIP_GAP).sum::<f32>() - CHIP_GAP;
        let beside = !chip_list.is_empty() && heading_w + 24.0 + chips_w <= c.w;
        let chips_y = if beside {
            disco_y - (CHIP_H - 26.0) / 2.0
        } else {
            disco_y + 36.0
        };
        let chips_bottom = if chip_list.is_empty() {
            disco_y + 30.0
        } else {
            chips_y + CHIP_H
        };
        let tiles_y = chips_bottom + TILES_TOP;
        let cols = grid::columns(c.w);
        let w = grid::card_width(c.w, cols);
        let pitch = w + TILE_TEXT;
        let rows = tiles.len().div_ceil(cols);
        let failed = slot.failed() || head.failed();
        let tail_h = if failed { 70.0 } else { 0.0 };
        let end = tiles_y + rows as f32 * pitch + tail_h;
        let content_h = end - vp.y + BOTTOM_PAD;
        let images = open.display_images();
        let pic = covers.get(images, geo.pic, scale);
        let name = open.display_name().to_owned();
        let table = &mut self.table;
        let mut chosen = None;

        let opts = ScrollOptions {
            bars: (Bars::Hidden, Bars::Auto),
            identity: Some(open.serial),
            ..ScrollOptions::default()
        };
        let area = ui.scroll_area(("artist-page", open.serial), vp, &opts, |ui, view| {
            let seen = Rect::new(vp.x, vp.y + view.offset.1 as f32, vp.w, vp.h);
            page_boxes_scope(true);
            ui.painter().on(LayerId::Popup);
            out.crumb = pages::crumb(ui, k, "artist-crumb", (c.x, c.y), (prev, &name), c.w);

            // The hero row: the picture, the text column bottom-aligned to it.
            let pic_r = Rect::new(c.x, hero_bottom - geo.pic, geo.pic, geo.pic);
            page_box(BoxKind::Control, pic_r);
            hero_picture(ui, k, pic_r, pic.is_some(), pic);
            let tx = c.x + geo.pic + HERO_GAP;
            let tw = (c.right() - tx).max(40.0);
            let top = hero_bottom - text_h;
            let ink = k.ink;
            let p = ui.painter();
            caps(p, "ARTIST", 10.5, ink.muted, tx, top, 1.8);
            let name_y = top + 14.0 + 8.0;
            text(
                p,
                &name,
                name_size,
                ink.text,
                k.decor().display_medium,
                tx,
                name_y,
                tw,
            );
            let meta_y = name_y + name_size * 1.15 + 10.0;
            if !meta.is_empty() {
                text(p, &meta, 14.0, ink.muted, false, tx, meta_y, tw);
            }
            let btn_y = hero_bottom - 26.0;
            let (play, shuffle) =
                pages::play_buttons(ui, k, "artist", (tx + 26.0, btn_y), (tx + 86.0, btn_y));
            out.play = play;
            out.shuffle = shuffle;

            // Popular.
            if popular_n > 0 {
                let p = ui.painter();
                text(
                    p,
                    "Popular",
                    HEADING,
                    ink.text,
                    k.decor().display_medium,
                    c.x,
                    popular_y,
                    0.0,
                );
                let area = Rect::new(
                    c.x,
                    popular_y + POPULAR_TOP,
                    c.w,
                    BAND_H + popular_n as f32 * ROW_PITCH,
                );
                let visible: Vec<usize> = (0..popular_n).collect();
                let form = Form {
                    bare: true,
                    ..Form::default()
                };
                let result = table.frame(
                    ui,
                    area,
                    k,
                    form,
                    &open.shown,
                    &visible,
                    "",
                    current,
                    covers,
                    likes,
                    player,
                    None,
                );
                if let Some(pos) = result.play {
                    play_visible(player, &open.shown, &visible, pos);
                }
                ui.painter().on(LayerId::Popup);
            }

            // Discography.
            let p = ui.painter();
            text(
                p,
                "Discography",
                HEADING,
                ink.text,
                k.decor().display_medium,
                c.x,
                disco_y,
                0.0,
            );
            let mut cx = if beside { c.x + heading_w + 24.0 } else { c.x };
            for ch in &chip_list {
                let r = Rect::new(cx, chips_y, ch.w, CHIP_H);
                page_box(BoxKind::Control, r);
                let resp = ui.interact(
                    ui.id(("artist-chip", ch.group as u8)),
                    r,
                    Sense::CLICK_FOCUS,
                );
                if resp.hovered {
                    ui.set_cursor(CursorIcon::Pointer);
                }
                let p = ui.painter();
                chip(p, k, r, &ch.label, ch.group == group, resp.hovered);
                if resp.focus_visible {
                    p.ring(r, CHIP_H / 2.0, 1.5, k.a, true);
                }
                if resp.clicked {
                    chosen = Some(ch.group);
                }
                cx += ch.w + CHIP_GAP;
            }

            // The release tiles.
            for (i, spec) in tiles.iter().enumerate() {
                let (col, row) = (i % cols, i / cols);
                let x = c.x + col as f32 * (w + grid::GAP);
                let ty = tiles_y + row as f32 * pitch;
                if !shown(seen, ty - 10.0, pitch + 10.0) {
                    continue;
                }
                page_box(BoxKind::Control, Rect::new(x, ty, w, w));
                let o = grid::card(ui, k, "artist-releases", i, (x, ty, w), spec, covers);
                if o.play {
                    out.play_release = Some(i);
                } else if o.open {
                    out.open = Some(i);
                }
            }
            let tail = tiles_y + rows as f32 * pitch;
            let p = ui.painter();
            if failed {
                text(p, LOAD_FAILED, 14.0, ink.muted, false, c.x, tail, 0.0);
                let r = Rect::new(c.x, tail + 28.0, 96.0, 32.0);
                out.retry = ui
                    .button("artist-retry", r, "Retry", ButtonKind::Secondary)
                    .clicked;
            } else if tiles.is_empty() {
                let (s, _) = status_text(open, slot);
                text(p, s, 14.0, ink.muted, false, c.x, tiles_y, 0.0);
            }
            page_boxes_scope(false);
            (
                (),
                Extent {
                    width: f64::from(vp.w),
                    height: f64::from(content_h),
                },
            )
        });
        if let Some(g) = chosen {
            self.group = g;
        }
        out.more = loaded
            && area.offset.1 + f64::from(vp.h) >= area.extent.1 - f64::from(NEAR_END)
            && slot.has_more();
        out
    }
}

/// What the explored album page is given besides the album.
#[derive(Debug, Clone, Copy)]
pub(crate) struct AlbumArgs<'a> {
    /// The breadcrumb's previous page; empty when there is none (the crumb then reads "Albums").
    pub prev: &'a str,
    /// The window's width (it decides the hero column's size).
    pub window_w: f32,
    /// The Save to library button, when the provider can save albums.
    pub save: Option<Save>,
    /// The artist the route knew, until the lookup names the album's own.
    pub route_artist: &'a str,
    /// The rows of the track table (every loaded track).
    pub visible: &'a [usize],
}

/// What the explored album page reported in a frame.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AlbumOut {
    /// The filter field has keyboard focus.
    pub filter_focused: bool,
    pub hero: HeroOut,
    pub pane: PaneResult,
    /// The "More by" tile (its index in [`more_by`]) to open or to play.
    pub open: Option<usize>,
    pub play_release: Option<usize>,
}

/// The releases the "More by" row shows: the first artist's releases but this album, at most one
/// row's worth is drawn.
pub(crate) fn more_by(open: &Open) -> Vec<&Album> {
    let Some(e) = &open.explore else {
        return Vec::new();
    };
    e.more
        .items()
        .iter()
        .filter(|a| a.uri != open.id && a.kind != AlbumKind::AppearsOn)
        .collect()
}

/// The first word of an album's facts line.
fn kind_word(kind: AlbumKind) -> &'static str {
    match kind {
        AlbumKind::Single => "Single",
        AlbumKind::Compilation => "Compilation",
        _ => "Album",
    }
}

/// The explored album page: the hero column (cover, name, artist links, facts, Play, Shuffle and
/// Save to library), the track table with the Artist column and, under both, "More by <artist>"
/// with that artist's other releases. One scroll area; the table is as tall as its rows so the
/// page scrolls as one.
#[allow(clippy::too_many_arguments)]
pub(crate) fn album_page(
    ui: &mut Ui<'_>,
    sheet: Rect,
    open: &Open,
    a: &AlbumArgs<'_>,
    on: &Show<'_>,
    covers: &mut Covers<'_>,
    likes: &mut LikeBook,
    (table, filter): (&mut TrackTable, &mut String),
) -> AlbumOut {
    let Show {
        look: k,
        state,
        player,
        hearts,
    } = *on;
    let (vp, c) = (grid::viewport(sheet), grid::content_rect(sheet));
    let scale = ui.painter().s;
    let album = open.explore.as_ref().and_then(|e| e.album.as_ref());
    let slot = &open.tracks;
    let name = album.map_or(open.name.as_str(), |al| al.name.as_str());
    let by = album
        .map(|al| al.artist.as_str())
        .filter(|s| !s.is_empty())
        .or(Some(a.route_artist).filter(|s| !s.is_empty()));
    let by_refs = album.map_or(&[][..], |al| al.artists.as_slice());
    let items = slot.items();
    let count = slot
        .total()
        .or_else(|| (!slot.has_more() && !items.is_empty()).then_some(items.len() as u32));
    let meta = pages::meta_with(
        kind_word(album.map_or(AlbumKind::Unknown, |al| al.kind)),
        true,
        open.year,
        count,
        items,
        !slot.has_more(),
    );
    let art = covers.get(&open.images, 300.0, scale);

    // The hero column's width and where the table starts.
    let tall = Rect::new(c.x, c.y, c.w, 10_000.0);
    let geo = pages::geometry(a.window_w, tall, true);
    let (side, lx, tw) = match geo.hero_side {
        Some(s) => (s, geo.table.x, geo.table.w),
        None => {
            let lx = c.x + NARROW_SIDE + 48.0;
            (NARROW_SIDE, lx, (c.right() - lx).max(0.0))
        }
    };
    let art = if geo.hero_side.is_some() || art.is_none() {
        art
    } else {
        covers.get(&open.images, side, scale)
    };
    let filter_r = match geo.hero_side {
        Some(_) => geo.filter,
        None => Rect::new(lx, c.y - 4.0, pages::FILTER_W.min(tw), pages::FILTER_H),
    };
    let n = a.visible.len();
    let table_h = if n == 0 {
        EMPTY_TABLE_H
    } else {
        BAND_H + n as f32 * ROW_PITCH + if slot.failed() { 44.0 } else { 0.0 }
    };
    let table_r = Rect::new(lx, c.y + TABLE_TOP, tw, table_h);
    let save_extra = if a.save.is_some() { 44.0 } else { 0.0 };
    let hero_h = TABLE_TOP + side + 174.0 + save_extra;
    let top_h = hero_h.max(TABLE_TOP + table_h);

    let more = more_by(open);
    let cols = grid::columns(c.w);
    let w = grid::card_width(c.w, cols);
    let tiles: Vec<CardSpec> = more.iter().take(cols).map(|al| tile(al)).collect();
    let more_y = c.y + top_h + SECTION_GAP;
    let tiles_y = more_y + MORE_TOP;
    let end = if tiles.is_empty() {
        c.y + top_h
    } else {
        tiles_y + w + TILE_TEXT
    };
    let content_h = end - vp.y + BOTTOM_PAD;
    let current = state.current.as_ref().map(|nw| nw.track.uri.as_str());
    let crumb = if a.prev.is_empty() { "Albums" } else { a.prev };
    let mut out = AlbumOut::default();
    let first = album.and_then(|al| al.artists.first());

    let opts = ScrollOptions {
        bars: (Bars::Hidden, Bars::Auto),
        identity: Some(slot.serial()),
        ..ScrollOptions::default()
    };
    ui.scroll_area(("album-page", slot.serial()), vp, &opts, |ui, view| {
        let seen = Rect::new(vp.x, vp.y + view.offset.1 as f32, vp.w, vp.h);
        page_boxes_scope(true);
        ui.painter().on(LayerId::Popup);
        let spec = Hero {
            crumb,
            name,
            by,
            by_refs,
            meta: &meta,
            side,
            art,
            save: a.save,
        };
        out.hero = pages::hero(ui, c, k, &spec);
        ui.painter().on(LayerId::Popup);
        out.filter_focused = !items.is_empty() && pages::filter_bar(ui, filter_r, filter);
        if !items.is_empty() {
            page_box(BoxKind::Control, filter_r);
        }
        let form = Form {
            album_page: true,
            artist_col: true,
            ..Form::default()
        };
        out.pane = table.frame(
            ui,
            table_r,
            k,
            form,
            slot,
            a.visible,
            "No songs.",
            current,
            covers,
            likes,
            player,
            hearts,
        );
        ui.painter().on(LayerId::Popup);
        if let (Some(artist), false) = (first, tiles.is_empty()) {
            let p = ui.painter();
            let medium = k.decor().display_medium;
            let lead = "More by ";
            let lw = text(p, lead, MORE_SIZE, k.ink.text, medium, c.x, more_y, 0.0);
            let face = Face {
                size: MORE_SIZE,
                medium,
                color: k.ink.text,
                shadow: false,
            };
            let at = Place {
                x: c.x + lw,
                y: more_y,
                centre: false,
                max_w: (c.w - lw).max(40.0),
            };
            link::line(
                ui,
                "album-more-by",
                k,
                &artist.name,
                std::slice::from_ref(artist),
                face,
                at,
            );
            for (i, spec) in tiles.iter().enumerate() {
                let x = c.x + i as f32 * (w + grid::GAP);
                if !shown(seen, tiles_y - 10.0, w + TILE_TEXT + 10.0) {
                    continue;
                }
                page_box(BoxKind::Control, Rect::new(x, tiles_y, w, w));
                let o = grid::card(ui, k, "album-more", i, (x, tiles_y, w), spec, covers);
                if o.play {
                    out.play_release = Some(i);
                } else if o.open {
                    out.open = Some(i);
                }
            }
        }
        page_boxes_scope(false);
        (
            (),
            Extent {
                width: f64::from(vp.w),
                height: f64::from(content_h),
            },
        )
    });
    out
}

/// The hero column's width when the window is too narrow for the full one, the table's top below
/// the content top, the table's height without rows (its loading and empty notes), and the
/// "More by" heading's size and the tiles' distance under it.
const NARROW_SIDE: f32 = 160.0;
const TABLE_TOP: f32 = 44.0;
const EMPTY_TABLE_H: f32 = 140.0;
const MORE_SIZE: f32 = 20.0;
const MORE_TOP: f32 = 40.0;

/// Whether the releases are far enough along to offer chips: the header is in.
fn slot_known(open: &ArtistOpen) -> bool {
    open.head().data().is_some() && !open.releases(ReleaseGroup::All).items().is_empty()
}

/// The text under the Discography heading while no tile shows.
fn status_text(open: &ArtistOpen, slot: &Slot<Album>) -> (&'static str, Status) {
    if open.head().loading() || slot.loading() || slot.has_more() {
        ("Loading\u{2026}", Status::Loading)
    } else {
        ("No releases.", Status::Ready)
    }
}

fn shown(view: Rect, y: f32, h: f32) -> bool {
    y + h >= view.y && y <= view.bottom()
}

/// The hero row's sizes for a content `w` wide.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct HeroGeo {
    pub pic: f32,
    pub name_size: f32,
    /// The text column's height: label, name, meta and the buttons.
    pub text_h: f32,
}

/// The picture shrinks with the width from [`PIC_MAX`] to [`PIC_MIN`]; the name follows it, from
/// [`NAME_MAX`] to [`NAME_MIN`], and is cut with an ellipsis when it still does not fit.
pub(crate) fn hero_geo(content_w: f32) -> HeroGeo {
    let pic = (content_w / 6.0).clamp(PIC_MIN, PIC_MAX);
    let t = (pic - PIC_MIN) / (PIC_MAX - PIC_MIN);
    let name_size = (NAME_MIN + (NAME_MAX - NAME_MIN) * t).round();
    // label (14), gap 8, the name's line, gap 10, meta (18), gap 12, the buttons (52).
    let text_h = 14.0 + 8.0 + (name_size * 1.15).ceil() + 10.0 + 18.0 + 12.0 + 52.0;
    HeroGeo {
        pic,
        name_size,
        text_h,
    }
}

/// The artist's round picture in `r`, or the placeholder in a circle.
fn hero_picture(ui: &mut Ui<'_>, k: &Look, r: Rect, has_art: bool, art: Option<crate::art::Art>) {
    let p = ui.painter();
    let radius = r.w / 2.0;
    if has_art {
        shadow(p, r, radius, 22.0, 0.9);
    } else {
        shadow(p, r, radius, 10.0, 0.6);
    }
    cover(p, k, r, radius, art, NoArt::Card);
}

/// One chip of the Discography row.
#[derive(Debug, Clone, PartialEq)]
struct Chip {
    group: ReleaseGroup,
    label: String,
    w: f32,
}

/// The chips on offer: All, then each other group that has something (its first page is in and
/// not empty), with the count where the list says it.
fn chips(open: &ArtistOpen) -> Vec<Chip> {
    GROUPS
        .into_iter()
        .filter(|g| *g == ReleaseGroup::All || !open.releases(*g).items().is_empty())
        .map(|g| {
            let name = group_label(g);
            let label = match count(open.releases(g)) {
                Some(n) => format!("{name} \u{b7} {n}"),
                None => name.to_owned(),
            };
            // The text's width is measured at draw time; a close estimate keeps the row's layout
            // (wrap or not) stable and the pill snug.
            let w = estimate_width(&label) + 2.0 * CHIP_PAD;
            Chip { group: g, label, w }
        })
        .collect()
}

/// A chip label's width at [`CHIP_TEXT`] (medium), estimated from its characters.
fn estimate_width(label: &str) -> f32 {
    label
        .chars()
        .map(|c| match c {
            ' ' | '\u{b7}' => CHIP_TEXT * 0.34,
            'i' | 'l' | 'j' | 't' | 'f' | 'I' | '&' => CHIP_TEXT * 0.34,
            'm' | 'w' | 'M' | 'W' => CHIP_TEXT * 0.9,
            c if c.is_ascii_uppercase() => CHIP_TEXT * 0.66,
            c if c.is_ascii_digit() => CHIP_TEXT * 0.58,
            _ => CHIP_TEXT * 0.56,
        })
        .sum::<f32>()
        .ceil()
}

/// A chip's name.
pub(crate) fn group_label(g: ReleaseGroup) -> &'static str {
    match g {
        ReleaseGroup::All => "All",
        ReleaseGroup::Albums => "Albums",
        ReleaseGroup::Singles => "Singles & EPs",
        ReleaseGroup::AppearsOn => "Appears on",
    }
}

/// How many releases a list has: the answered total, else what loaded once complete.
fn count(slot: &Slot<Album>) -> Option<u32> {
    slot.total().or_else(|| {
        (!slot.has_more() && !slot.items().is_empty()).then_some(slot.items().len() as u32)
    })
}

/// A chip: the selected one a light pill with dark text, the others glass with a thin line.
fn chip(p: &mut crate::draw::P<'_>, k: &Look, r: Rect, label: &str, selected: bool, hovered: bool) {
    let ink = k.ink;
    let radius = r.h / 2.0;
    let tc = if selected {
        p.fill(r, radius, alpha(ink.light, 0xE6));
        CHIP_DARK
    } else {
        p.fill(
            r,
            radius,
            alpha(ink.light, if hovered { 0x1E } else { 0x12 }),
        );
        p.ring(r, radius, 1.0, ink.line, false);
        if hovered { ink.text } else { ink.text2 }
    };
    let tw = width(p, label, CHIP_TEXT, true);
    text(
        p,
        label,
        CHIP_TEXT,
        tc,
        true,
        r.x + (r.w - tw) / 2.0,
        r.y + (r.h - 17.0) / 2.0,
        0.0,
    );
}

/// A release's tile: its cover with the name and "year \u{b7} kind" under it (captions are plain,
/// D8).
pub(crate) fn tile(a: &Album) -> CardSpec {
    CardSpec {
        title: a.name.clone(),
        subtitle: release_meta(a),
        dim: false,
        art: Art::Note,
        images: a.images.clone(),
        playing: false,
    }
}

/// "1993 \u{b7} Album": the year and the kind, whichever is known.
pub(crate) fn release_meta(a: &Album) -> String {
    let kind = match a.kind {
        AlbumKind::Album => Some("Album"),
        AlbumKind::Single => Some("Single"),
        AlbumKind::Compilation => Some("Compilation"),
        AlbumKind::AppearsOn => Some("Appears on"),
        AlbumKind::Unknown => None,
    };
    match (a.year, kind) {
        (Some(y), Some(kind)) => format!("{y} \u{b7} {kind}"),
        (Some(y), None) => y.to_string(),
        (None, Some(kind)) => kind.to_owned(),
        (None, None) => String::new(),
    }
}

/// "4 albums \u{b7} 2 singles \u{b7} 1989 \u{2013} 2004": the counts of the albums and singles lists
/// and the years of the releases loaded (guest appearances are not the artist's years); a part
/// with no data is left out.
pub(crate) fn meta_line(open: &ArtistOpen) -> String {
    let mut parts = Vec::new();
    for (g, one, many) in [
        (ReleaseGroup::Albums, "album", "albums"),
        (ReleaseGroup::Singles, "single", "singles"),
    ] {
        match count(open.releases(g)) {
            Some(1) => parts.push(format!("1 {one}")),
            Some(n) if n > 1 => parts.push(format!("{n} {many}")),
            _ => {}
        }
    }
    let years = open
        .releases(ReleaseGroup::All)
        .items()
        .iter()
        .filter(|a| a.kind != AlbumKind::AppearsOn)
        .chain(open.releases(ReleaseGroup::Albums).items())
        .chain(open.releases(ReleaseGroup::Singles).items())
        .filter_map(|a| a.year);
    let (lo, hi) = years.fold((u16::MAX, 0u16), |(lo, hi), y| (lo.min(y), hi.max(y)));
    if hi != 0 {
        parts.push(if lo == hi {
            lo.to_string()
        } else {
            format!("{lo} \u{2013} {hi}")
        });
    }
    parts.join(" \u{b7} ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn album(year: Option<u16>, kind: AlbumKind) -> Album {
        Album {
            id: "a".into(),
            uri: "demo:album:a".into(),
            name: "A".into(),
            artist: "B".into(),
            track_count: None,
            images: vec![],
            year,
            artists: vec![],
            kind,
        }
    }

    #[test]
    fn a_release_says_its_year_and_kind_whichever_it_knows() {
        let m = |y, k| release_meta(&album(y, k));
        assert_eq!(m(Some(1993), AlbumKind::Album), "1993 \u{b7} Album");
        assert_eq!(m(Some(2001), AlbumKind::Single), "2001 \u{b7} Single");
        assert_eq!(m(Some(2001), AlbumKind::Unknown), "2001");
        assert_eq!(m(None, AlbumKind::Compilation), "Compilation");
        assert_eq!(m(None, AlbumKind::Unknown), "");
        assert_eq!(m(Some(1), AlbumKind::AppearsOn), "1 \u{b7} Appears on");
    }

    #[test]
    fn the_picture_shrinks_before_the_name_and_the_text_column_always_fits_the_hero() {
        let wide = hero_geo(1264.0);
        assert_eq!((wide.pic, wide.name_size), (PIC_MAX, NAME_MAX));
        let narrow = hero_geo(584.0);
        assert_eq!((narrow.pic, narrow.name_size), (PIC_MIN, NAME_MIN));
        for w in [584.0, 700.0, 900.0, 1100.0, 1264.0, 2000.0] {
            let g = hero_geo(w);
            assert!((PIC_MIN..=PIC_MAX).contains(&g.pic), "{w}");
            assert!((NAME_MIN..=NAME_MAX).contains(&g.name_size), "{w}");
            assert!(g.text_h >= 14.0 + 52.0, "{w}");
        }
        // The picture never shrinks slower than the name (it falls first as the width does).
        let a = hero_geo(900.0);
        let b = hero_geo(1000.0);
        assert!(b.pic >= a.pic && b.name_size >= a.name_size);
    }

    #[test]
    fn chip_labels_name_their_group_and_the_count_when_known() {
        assert_eq!(group_label(ReleaseGroup::Singles), "Singles & EPs");
        assert_eq!(group_label(ReleaseGroup::AppearsOn), "Appears on");
        assert!(estimate_width("All \u{b7} 6") < estimate_width("Singles & EPs \u{b7} 12"));
    }
}
