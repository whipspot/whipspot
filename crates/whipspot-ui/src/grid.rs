//! The browse sheet's collection pages (MD55): the folder-explorer grid of cards (Playlists,
//! Albums) and Home's rows. Cards draw their cover (a quiet placeholder tile without one), grow
//! with the window (168 px at the least, between 2 and 8 columns) and the page scrolls inside a
//! clip below the top bar. Pure drawing: the caller (the sheet) builds the card specs from the
//! library and acts on what the page reports.

use std::cell::Cell;

use lune_ui::color::Rgba;
use lune_ui::input::CursorIcon;
use lune_ui::paint::Rect;
use lune_ui::scene::LayerId;
use lune_ui::ui::{Sense, Ui};
use lune_ui::widgets::button::ButtonKind;
use lune_ui::widgets::scroll_area::{Bars, Extent, ScrollOptions};
use whipspot_core::player::ArtUrgency;
use whipspot_core::provider::ImageRef;

use crate::art::{ArtCache, ArtPull, NoArt, Variant, cover};
use crate::draw::{P, alpha, disc, outline, shadow, text, width};
use crate::history::Tab;
use crate::icons::{Icon, icon, play_filled};
use crate::library::LOAD_FAILED;
use crate::scroll_memory;
use crate::theme::{Labels, Look, Pill};

/// The gap between cards, both ways.
pub const GAP: f32 = 32.0;
/// The narrowest a card gets.
pub const MIN_CARD: f32 = 168.0;
/// The fewest and the most columns.
pub const MIN_COLS: usize = 2;
pub const MAX_COLS: usize = 8;
/// Below the art: the title and the subtitle rows, so a card's height is its width plus this.
pub const TEXT_H: f32 = 70.0;
/// The page title's size and the cards' distance below the header.
const TITLE_SIZE: f32 = 34.0;
const CARDS_TOP: f32 = 72.0;
/// Home's section header size and the cards' distance below it.
const SECTION_SIZE: f32 = 22.0;
const SECTION_CARDS_TOP: f32 = 40.0;
/// The distance from one Home section's last row (its text included) to the next section's
/// header.
const SECTION_GAP: f32 = 12.0;
/// Most rows one Home section shows, however tall the window.
const MAX_HOME_ROWS: usize = 4;
/// The page keeps this much under the last row inside the scroll area.
const BOTTOM_PAD: f32 = 36.0;
/// Paging: the next page is asked for when the view is within this many px of the end.
const NEAR_END: f32 = 420.0;
/// The subtitle shown for a playlist the provider does not read.
pub const MAY_NOT_READ: &str = "May not be readable";
pub const NOT_READABLE: &str = "Not readable";

/// The content rect of a sheet `s`: where headers and cards start (88, 148 at 1440 x 900).
pub fn content_rect(s: Rect) -> Rect {
    Rect::new(s.x + 56.0, s.y + 96.0, s.w - 112.0, s.h - 124.0)
}

/// The scroll area of a sheet's page: the sheet below its top bar, kept off the rounded corners.
pub fn viewport(s: Rect) -> Rect {
    Rect::new(
        s.x + 12.0,
        s.y + 72.0,
        s.w - 24.0,
        (s.h - 72.0 - 16.0).max(0.0),
    )
}

/// The number of columns of a grid `content_w` wide: as many as fit cards of [`MIN_CARD`] with the
/// gap between, within [`MIN_COLS`]..=[`MAX_COLS`].
pub fn columns(content_w: f32) -> usize {
    (((content_w + GAP) / (MIN_CARD + GAP)).floor() as usize).clamp(MIN_COLS, MAX_COLS)
}

/// The width of one of `cols` cards in a grid `content_w` wide.
pub fn card_width(content_w: f32, cols: usize) -> f32 {
    ((content_w - GAP * (cols as f32 - 1.0)) / cols as f32).floor()
}

/// What a card's art slot shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Art {
    /// The placeholder tile with a note.
    Note,
    /// The liked-songs collection mark: the accent gradient with a heart (not a toggle).
    Liked,
}

/// Where the pages get covers: the art cache and the player it asks.
pub struct Covers<'a> {
    pub cache: &'a mut ArtCache,
    pub pull: &'a dyn ArtPull,
}

impl Covers<'_> {
    /// The registered cover of the best of `images` for a slot `side` logical px wide at `scale`,
    /// asking for it when it is not there yet; `None` draws the placeholder.
    pub fn get(&mut self, images: &[ImageRef], side: f32, scale: f32) -> Option<crate::art::Art> {
        let phys = (side * scale).ceil().max(1.0) as u32;
        let image = ImageRef::pick(images, phys)?;
        self.cache
            .get(self.pull, image, Variant::Plain, ArtUrgency::Visible, phys)
    }
}

/// One card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardSpec {
    pub title: String,
    pub subtitle: String,
    /// The subtitle is a state note (dimmed).
    pub dim: bool,
    pub art: Art,
    /// The covers of a [`Art::Note`] card, as the provider listed them.
    pub images: Vec<ImageRef>,
    /// The card of the collection that plays now (albums only).
    pub playing: bool,
}

/// What a card reported in a frame.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CardOut {
    pub open: bool,
    pub play: bool,
}

/// What a page reported: the card to open or to play, and that the next page is due.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct PageOut {
    pub open: Option<usize>,
    pub play: Option<usize>,
    pub near_end: bool,
    /// The Retry button of a failed list was pressed.
    pub retry: bool,
    /// The page's scroll area: offset, extent and viewport heights (scroll memory, MD202).
    pub scrolled: Option<(f64, f64, f64)>,
}

thread_local! {
    /// The bars' clock for this frame (seconds, `f64` so it keeps its precision over days of
    /// uptime; M7-F7): `Some` while the current song plays, `None` while it is paused or stopped
    /// (the bars rest low). The UI is drawn on one thread.
    static EQ_CLOCK: Cell<Option<f64>> = const { Cell::new(None) };
    /// Whether a moving set of bars was drawn since the shell last asked.
    static EQ_MOVED: Cell<bool> = const { Cell::new(false) };
}

/// The frame's bars clock (see [`eq_bars`]); the shell sets it before it draws.
pub(crate) fn set_eq_clock(t: Option<f64>) {
    EQ_CLOCK.with(|c| c.set(t));
}

/// Whether bars that move were drawn since the last call; the shell then asks for another frame
/// (and only then, so a view without them, or a paused song, repaints nothing).
pub(crate) fn take_eq_moved() -> bool {
    EQ_MOVED.with(|m| m.replace(false))
}

/// The resting bar heights while nothing plays.
const EQ_REST: [f32; 4] = [5.0, 8.0, 3.5, 6.5];
/// The bounce: each bar's lowest and highest height, its rate (Hz) and its phase (turns).
const EQ_MIN: f32 = 3.0;
const EQ_MAX: f32 = 14.0;
const EQ_HZ: [f64; 4] = [1.15, 1.7, 1.35, 2.0];
const EQ_PHASE: [f64; 4] = [0.0, 0.31, 0.62, 0.87];

/// The heights of the four bars at clock `t` (`None`: at rest). A procedural bounce, not tied to
/// the audio: each bar follows a sine at its own rate between [`EQ_MIN`] and [`EQ_MAX`]. The phase
/// is worked out in `f64` and wrapped to one turn before it reaches `f32`, so a long-running app's
/// bars bounce as smoothly on day ten as on day one.
pub(crate) fn eq_heights(t: Option<f64>) -> [f32; 4] {
    let Some(t) = t else {
        return EQ_REST;
    };
    std::array::from_fn(|i| {
        let turns = (EQ_HZ[i] * t + EQ_PHASE[i]).fract() as f32;
        let s = (std::f32::consts::TAU * turns).sin();
        EQ_MIN + (EQ_MAX - EQ_MIN) * (0.5 + 0.5 * s)
    })
}

/// Equaliser bars: the playing marker, centred on `c`. They bounce while the frame's clock runs
/// ([`set_eq_clock`]) and rest low otherwise.
pub(crate) fn eq_bars(p: &mut P<'_>, c: (f32, f32), color: Rgba) {
    let clock = EQ_CLOCK.with(Cell::get);
    if clock.is_some() {
        EQ_MOVED.with(|m| m.set(true));
    }
    for (i, h) in eq_heights(clock).iter().enumerate() {
        let x = c.0 - 7.0 + i as f32 * 4.0;
        p.fill(Rect::new(x, c.1 + 6.5 - h, 2.0, *h), 1.0, color);
    }
}

/// The liked-songs mark: the accent gradient with a heart.
fn liked_art(p: &mut P<'_>, k: &Look, r: Rect, radius: f32) {
    p.push_clip_rounded(r, radius);
    // Each strip runs to the bottom and the next one covers it: no seams between strips.
    let n = 32;
    for i in 0..n {
        let t = (i as f32 + 0.5) / n as f32;
        let y = r.y + r.h * i as f32 / n as f32;
        p.fill(
            Rect::new(r.x, y, r.w, r.bottom() - y),
            0.0,
            lune_ui::color::mix(k.a, k.b, t),
        );
    }
    p.pop_clip();
    icon(
        p,
        Icon::Heart,
        (r.x + r.w / 2.0, r.y + r.h / 2.0),
        r.w * 0.28,
        k.on,
        2.0,
    );
}

/// Draws the card `spec` with its art square at `(x, y)`, `w` wide; `scope` and `i` name it.
pub fn card(
    ui: &mut Ui<'_>,
    k: &Look,
    scope: &str,
    i: usize,
    (x, y, w): (f32, f32, f32),
    spec: &CardSpec,
    covers: &mut Covers<'_>,
) -> CardOut {
    let ink = k.ink;
    let rad = k.radii();
    let art = Rect::new(x, y, w, w);
    let tile = Rect::new(x - 10.0, y - 10.0, w + 20.0, w + 72.0);
    let pc = (art.right() - 32.0, art.bottom() - 32.0);
    let mut out = CardOut::default();

    ui.painter().on(LayerId::Popup);
    let resp = ui.interact(ui.id(("card", scope, i)), tile, Sense::CLICK_FOCUS);
    let hover = resp.hovered || ui.pointer_in(tile);
    if hover {
        let disc_r = Rect::new(pc.0 - 23.0, pc.1 - 23.0, 46.0, 46.0);
        let play = ui.interact(ui.id(("card-play", scope, i)), disc_r, Sense::CLICK);
        out.play = play.clicked;
        out.open = resp.clicked && !play.clicked;
        ui.set_cursor(CursorIcon::Pointer);
    } else {
        out.open = resp.clicked;
    }

    let scale = ui.painter().s;
    let image = match spec.art {
        Art::Note => covers.get(&spec.images, w, scale),
        Art::Liked => None,
    };
    let p = ui.painter();
    if hover {
        match k.decor().pill {
            Pill::Glass => p.fill(tile, rad.card + 6.0, alpha(ink.light, 0x10)),
            Pill::Led => outline(p, tile, rad.card, alpha(ink.light, 0x0C), ink.line),
            Pill::Dot => shadow(p, art, rad.card, 22.0, 0.9),
        }
    }
    if rad.art < 6.0 || hover {
        shadow(p, art, rad.card, 10.0, 0.6);
    }
    match spec.art {
        Art::Note => cover(p, k, art, rad.card, image, NoArt::Card),
        Art::Liked => liked_art(p, k, art, rad.card),
    }

    let ty = art.bottom() + 14.0;
    let mut tx = x;
    let mut title_c = ink.text;
    if spec.playing {
        match k.decor().labels {
            Labels::Marked => {
                title_c = k.a;
                eq_bars(p, (x + w - 8.0, ty + 10.0), k.a);
            }
            Labels::Tracked => {
                p.on(LayerId::Modal);
                let bw = crate::draw::caps_width(p, "PLAYING", 8.5, 1.6) + 30.0;
                let b = Rect::new(art.x + 10.0, art.y + 10.0, bw, 20.0);
                p.fill(b, 10.0, [0x0A, 0x08, 0x06, 0xC8]);
                crate::draw::led(p, (b.x + 10.0, b.y + 10.0), 4.0, k.a, k.hi, true);
                crate::draw::caps(p, "PLAYING", 8.5, k.hi, b.x + 20.0, b.y + 4.5, 1.6);
                p.on(LayerId::Popup);
            }
            Labels::Plain => {
                eq_bars(p, (x + 7.0, ty + 10.0), k.a);
                tx += 20.0;
                title_c = k.hi;
            }
        }
    }
    text(
        p,
        &spec.title,
        14.5,
        title_c,
        true,
        tx,
        ty,
        w - (tx - x) - 20.0,
    );
    let sub = if spec.dim { ink.faint } else { ink.muted };
    text(p, &spec.subtitle, 13.0, sub, false, x, ty + 22.0, w);

    if hover {
        match k.decor().pill {
            Pill::Dot => {
                p.ring(art, rad.card, 1.5, alpha(k.a, 0xE0), true);
                p.on(LayerId::Modal);
                disc(p, pc, 44.0, [0x08, 0x08, 0x0C, 0xA0]);
                p.ring(
                    Rect::new(pc.0 - 22.0, pc.1 - 22.0, 44.0, 44.0),
                    22.0,
                    1.0,
                    [0xFF, 0xFF, 0xFF, 0x50],
                    false,
                );
                play_filled(p, (pc.0 + 1.5, pc.1), 15.0, ink.text);
            }
            _ => {
                p.on(LayerId::Modal);
                disc(p, (pc.0, pc.1 + 3.0), 50.0, [0, 0, 0, 0x40]);
                disc(p, pc, 46.0, k.a);
                play_filled(p, (pc.0 + 2.0, pc.1), 17.0, k.on);
            }
        }
        p.on(LayerId::Popup);
    }
    if resp.focus_visible {
        p.ring(tile, rad.card + 6.0, 1.5, k.a, true);
    }
    out
}

/// Draws a header: the title at `c.x - 2`, `c.y`, and the muted `sub` text beside it.
pub fn header(p: &mut P<'_>, k: &Look, c: Rect, title: &str, sub: Option<&str>) {
    let hw = text(
        p,
        title,
        TITLE_SIZE,
        k.ink.text,
        k.decor().display_medium,
        c.x - 2.0,
        c.y,
        0.0,
    );
    if let Some(sub) = sub {
        text(
            p,
            sub,
            14.0,
            k.ink.muted,
            false,
            c.x + hw + 16.0,
            c.y + 18.0,
            0.0,
        );
    }
}

/// A line of secondary text with the page's measure at `(x, y)`.
fn note_at(p: &mut P<'_>, k: &Look, s: &str, x: f32, y: f32) {
    text(p, s, 14.0, k.ink.muted, false, x, y, 0.0);
}

/// Whether a card row at `y` (content coordinates) of height `h` shows in `view`.
fn shows(view: Rect, y: f32, h: f32) -> bool {
    y + h >= view.y && y <= view.bottom()
}

/// One Playlists or Albums page.
#[derive(Debug, Clone, Copy)]
pub struct Page<'a> {
    /// Names the page's widgets.
    pub scope: &'static str,
    pub title: &'a str,
    /// The "N in your library" count, when known.
    pub count: Option<u32>,
    pub cards: &'a [CardSpec],
    pub status: Status,
}

/// A Playlists or Albums page: the header and the cards in rows, inside a scroll area over `vp`.
/// `c` is the content rect (its x and top place the header).
pub fn collection(
    ui: &mut Ui<'_>,
    vp: Rect,
    c: Rect,
    k: &Look,
    page: &Page<'_>,
    covers: &mut Covers<'_>,
    want: Option<f64>,
) -> PageOut {
    let Page {
        scope,
        title,
        count,
        cards,
        status,
    } = *page;
    let cols = columns(c.w);
    let w = card_width(c.w, cols);
    let pitch = w + TEXT_H;
    let opts = ScrollOptions {
        bars: (Bars::Hidden, Bars::Auto),
        identity: Some(0),
        ..ScrollOptions::default()
    };
    let mut out = PageOut::default();
    let rows = cards.len().div_ceil(cols);
    let tail_h = if status == Status::Failed { 70.0 } else { 0.0 };
    let content_h = c.y - vp.y + CARDS_TOP + rows as f32 * pitch + tail_h + BOTTOM_PAD;
    // Back or forward asked for the offset the page was left at (MD202).
    if let Some(y) = want {
        scroll_memory::reveal_area(
            ui,
            (scope, "page"),
            y,
            f64::from(vp.h),
            f64::from(content_h),
        );
    }
    let area = ui.scroll_area((scope, "page"), vp, &opts, |ui, view| {
        let seen = Rect::new(vp.x, vp.y + view.offset.1 as f32, vp.w, vp.h);
        ui.painter().on(LayerId::Popup);
        let sub = count.map(|n| format!("{n} in your library"));
        header(ui.painter(), k, c, title, sub.as_deref());
        let y0 = c.y + CARDS_TOP;
        for (i, spec) in cards.iter().enumerate() {
            let (col, row) = (i % cols, i / cols);
            let x = c.x + col as f32 * (w + GAP);
            let y = y0 + row as f32 * pitch;
            if !shows(seen, y - 10.0, pitch + 10.0) {
                continue;
            }
            let o = card(ui, k, scope, i, (x, y, w), spec, covers);
            if o.play {
                out.play = Some(i);
            } else if o.open {
                out.open = Some(i);
            }
        }
        let tail = y0 + rows as f32 * pitch;
        match status {
            Status::Loading if cards.is_empty() => {
                note_at(ui.painter(), k, "Loading\u{2026}", c.x, y0);
            }
            Status::Empty(s) if cards.is_empty() => note_at(ui.painter(), k, s, c.x, y0),
            Status::Failed => {
                note_at(ui.painter(), k, LOAD_FAILED, c.x, tail);
                let r = Rect::new(c.x, tail + 28.0, 96.0, 32.0);
                out.retry = ui
                    .button((scope, "retry"), r, "Retry", ButtonKind::Secondary)
                    .clicked;
            }
            _ => {}
        }
        (
            (),
            Extent {
                width: f64::from(vp.w),
                height: f64::from(content_h),
            },
        )
    });
    out.near_end = area.offset.1 + f64::from(vp.h) >= area.extent.1 - f64::from(NEAR_END);
    out.scrolled = Some((area.offset.1, area.extent.1, f64::from(area.viewport.h)));
    out
}

/// What a list is doing when it has no card to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Cards are shown (or it does not matter).
    Ready,
    Loading,
    Failed,
    /// Complete and empty: the text to say.
    Empty(&'static str),
}

/// One of Home's sections: a header that opens its tab and one row of cards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section<'a> {
    pub title: &'static str,
    pub tab: Tab,
    pub cards: &'a [CardSpec],
    pub status: Status,
}

/// What Home reported: a section header pressed, a card to open or play (section, card), a
/// Retry (section).
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct HomeOut {
    pub header: Option<Tab>,
    pub open: Option<(usize, usize)>,
    pub play: Option<(usize, usize)>,
    pub retry: Option<usize>,
    /// The page's scroll area: offset, extent and viewport heights (scroll memory, MD202).
    pub scrolled: Option<(f64, f64, f64)>,
}

/// The height Home's section of `rows` rows of cards `w` wide takes, header and gap included.
fn section_height(rows: usize, w: f32) -> f32 {
    SECTION_CARDS_TOP + rows as f32 * (w + TEXT_H) + SECTION_GAP
}

/// How many rows each Home section shows: every section gets one, and when `avail` px of height is
/// left after those, the sections that have more cards than they show take one more row in turn
/// (the first section first) while a whole row fits. `counts` is the number of cards each
/// section has; `cols` the columns and `w` the cards' width.
pub(crate) fn home_rows(avail: f32, w: f32, cols: usize, counts: &[usize]) -> Vec<usize> {
    let mut rows = vec![1usize; counts.len()];
    let mut left = avail - counts.len() as f32 * section_height(1, w);
    let row_h = w + TEXT_H;
    let mut grew = true;
    while grew {
        grew = false;
        for (r, &n) in rows.iter_mut().zip(counts) {
            if *r < MAX_HOME_ROWS && *r * cols < n && left >= row_h {
                *r += 1;
                left -= row_h;
                grew = true;
            }
        }
    }
    rows
}

/// The narrowest and widest a Home card is chosen to be (the layout may go a little past them
/// when no column count lands inside).
const HOME_CARD_MIN: f32 = 132.0;
const HOME_CARD_MAX: f32 = 200.0;
/// The most columns Home may use.
const HOME_MAX_COLS: usize = 12;

/// Home's layout: the columns, the card width and each section's rows for a content `content_w`
/// wide with `avail` px of height under the top. Every column count whose cards come out between
/// [`HOME_CARD_MIN`] and [`HOME_CARD_MAX`] wide is tried, with the rows of [`home_rows`]; the one
/// that leaves the least height unused wins (the larger cards on a tie), and a count whose one row
/// per section does not fit is passed over. With none left, the grid pages' own columns apply.
pub(crate) fn home_layout(
    content_w: f32,
    avail: f32,
    counts: &[usize],
) -> (usize, f32, Vec<usize>) {
    let used = |rows: &[usize], w: f32| -> f32 { rows.iter().map(|&r| section_height(r, w)).sum() };
    let mut best: Option<(f32, usize, f32, Vec<usize>)> = None;
    for cols in MIN_COLS..=HOME_MAX_COLS {
        let w = card_width(content_w, cols);
        if !(HOME_CARD_MIN..=HOME_CARD_MAX).contains(&w) {
            continue;
        }
        let rows = home_rows(avail, w, cols, counts);
        let left = avail - used(&rows, w);
        if left < 0.0 {
            continue;
        }
        // Strictly less leftover wins; columns run upwards, so a tie keeps the larger cards.
        if best.as_ref().is_none_or(|(l, ..)| left < *l - 0.5) {
            best = Some((left, cols, w, rows));
        }
    }
    match best {
        Some((_, cols, w, rows)) => (cols, w, rows),
        None => {
            let cols = columns(content_w);
            let w = card_width(content_w, cols);
            (cols, w, home_rows(avail, w, cols, counts))
        }
    }
}

/// Home: the sections one under the other, each as many rows of cards as the height allows.
pub fn home(
    ui: &mut Ui<'_>,
    vp: Rect,
    c: Rect,
    k: &Look,
    sections: &[Section<'_>],
    covers: &mut Covers<'_>,
    want: Option<f64>,
) -> HomeOut {
    let opts = ScrollOptions {
        bars: (Bars::Hidden, Bars::Auto),
        identity: Some(1),
        ..ScrollOptions::default()
    };
    let counts: Vec<usize> = sections.iter().map(|s| s.cards.len()).collect();
    let avail = vp.h - (c.y - vp.y) - BOTTOM_PAD;
    let (cols, w, rows) = home_layout(c.w, avail, &counts);
    let heights: Vec<f32> = rows.iter().map(|&r| section_height(r, w)).collect();
    let content_h = c.y - vp.y + heights.iter().sum::<f32>() + BOTTOM_PAD;
    let mut out = HomeOut::default();
    if let Some(y) = want {
        scroll_memory::reveal_area(ui, "home-page", y, f64::from(vp.h), f64::from(content_h));
    }
    let area = ui.scroll_area("home-page", vp, &opts, |ui, _view| {
        ui.painter().on(LayerId::Popup);
        let mut y = c.y;
        for (s, section) in sections.iter().enumerate() {
            let top = y;
            y += heights[s];
            let y = top;
            // The header row is one press target.
            let tw = {
                let p = ui.painter();
                width(p, section.title, SECTION_SIZE, k.decor().display_medium)
            };
            let hit = Rect::new(c.x - 4.0, y - 4.0, tw + 8.0 + 12.0 + 12.0, 34.0);
            let resp = ui.interact(ui.id(("home-header", s)), hit, Sense::CLICK_FOCUS);
            if resp.hovered {
                ui.set_cursor(CursorIcon::Pointer);
            }
            let p = ui.painter();
            text(
                p,
                section.title,
                SECTION_SIZE,
                k.ink.text,
                k.decor().display_medium,
                c.x,
                y,
                0.0,
            );
            icon(
                p,
                Icon::ChevR,
                (c.x + tw + 14.0, y + 14.0),
                12.0,
                if resp.hovered {
                    k.ink.text
                } else {
                    k.ink.text2
                },
                1.4,
            );
            if resp.clicked {
                out.header = Some(section.tab);
            }

            let cy = y + SECTION_CARDS_TOP;
            for (i, spec) in section.cards.iter().take(cols * rows[s]).enumerate() {
                let (col, row) = (i % cols, i / cols);
                let x = c.x + col as f32 * (w + GAP);
                let o = card(
                    ui,
                    k,
                    section.title,
                    i,
                    (x, cy + row as f32 * (w + TEXT_H), w),
                    spec,
                    covers,
                );
                if o.play {
                    out.play = Some((s, i));
                } else if o.open {
                    out.open = Some((s, i));
                }
            }
            if section.cards.is_empty() {
                let p = ui.painter();
                match section.status {
                    Status::Loading => note_at(p, k, "Loading\u{2026}", c.x, cy),
                    Status::Empty(t) => note_at(p, k, t, c.x, cy),
                    Status::Failed => {
                        note_at(p, k, LOAD_FAILED, c.x, cy);
                        let r = Rect::new(c.x, cy + 28.0, 96.0, 32.0);
                        if ui
                            .button(("home-retry", s), r, "Retry", ButtonKind::Secondary)
                            .clicked
                        {
                            out.retry = Some(s);
                        }
                    }
                    Status::Ready => {}
                }
            }
        }
        (
            (),
            Extent {
                width: f64::from(vp.w),
                height: f64::from(content_h),
            },
        )
    });
    out.scrolled = Some((area.offset.1, area.extent.1, f64::from(area.viewport.h)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The content width of a window `window_w` wide (the sheet's inset 32 + 56 each side).
    fn content_w(window_w: f32) -> f32 {
        window_w - 2.0 * 88.0
    }

    #[test]
    fn h1_home_rows_fill_the_height_in_whole_rows_and_start_at_one() {
        // 8 columns of 200 px cards (2000 px wide): a row is 270 px, a section with one row 322.
        let (cols, w) = (8, 200.0);
        let many = [40, 40];
        // The default window: nothing spare, one row each (never fewer).
        assert_eq!(home_rows(300.0, w, cols, &many), [1, 1]);
        assert_eq!(home_rows(644.0, w, cols, &many), [1, 1]);
        // 2000 x 1076: 128 px spare after one row each, not a whole row.
        assert_eq!(home_rows(772.0, w, cols, &many), [1, 1]);
        // A whole row spare goes to the first section, then the next.
        assert_eq!(home_rows(644.0 + 270.0, w, cols, &many), [2, 1]);
        assert_eq!(home_rows(644.0 + 540.0, w, cols, &many), [2, 2]);
        // Capped, and never more rows than the cards need.
        assert_eq!(home_rows(9000.0, w, cols, &many), [4, 4]);
        assert_eq!(home_rows(9000.0, w, cols, &[5, 12]), [1, 2]);
        assert_eq!(home_rows(9000.0, w, cols, &[0, 0]), [1, 1]);
        assert!(home_rows(500.0, w, cols, &[]).is_empty());
    }

    #[test]
    fn h2_home_picks_the_card_size_that_leaves_the_least_height_unused() {
        let many = [40, 40];
        // 2000 x 1076 (content 1824 wide, 772 high): 8 columns of 200 px would leave 128 px
        // unused; 11 columns of 136 px leave 50 and give the first section a second row.
        let (cols, w, rows) = home_layout(1824.0, 772.0, &many);
        assert_eq!((cols, w, rows), (11, 136.0, vec![2, 1]));
        // Whatever it picks, the cards stay in range and everything fits.
        for (cw, avail) in [
            (1264.0, 596.0),
            (1824.0, 1100.0),
            (584.0, 200.0),
            (900.0, 5000.0),
        ] {
            let (cols, w, rows) = home_layout(cw, avail, &many);
            assert!(
                (HOME_CARD_MIN..=HOME_CARD_MAX).contains(&w) || cols == columns(cw),
                "{cw}"
            );
            assert!(rows.iter().all(|r| (1..=MAX_HOME_ROWS).contains(r)));
            assert_eq!(w, card_width(cw, cols));
        }
        // Few cards: no row is reserved that they would not fill.
        let (cols, _, rows) = home_layout(1824.0, 5000.0, &[5, 3]);
        assert_eq!(rows, vec![1, 1]);
        assert!(cols >= MIN_COLS);
        // Nothing fits (a tiny window): the page grid's columns, one row each, it scrolls.
        let (cols, w, rows) = home_layout(584.0, 100.0, &many);
        assert_eq!((cols, w, rows), (3, card_width(584.0, 3), vec![1, 1]));
    }

    #[test]
    fn eq_bars_bounce_smoothly_inside_their_range_and_rest_without_a_clock() {
        assert_eq!(eq_heights(None), EQ_REST);
        // From a fresh start, and after ten days of uptime (M7-F7: the clock keeps its precision).
        for start in [0.0, 864_000.0] {
            eq_bars_bounce_from(start);
        }
    }

    fn eq_bars_bounce_from(start: f64) {
        let mut last = eq_heights(Some(start));
        let mut moved = [false; 4];
        // 30 fps for a minute of synthetic time.
        for i in 1..=1800 {
            let h = eq_heights(Some(start + f64::from(i) / 30.0));
            for b in 0..4 {
                assert!(
                    (EQ_MIN - 1e-3..=EQ_MAX + 1e-3).contains(&h[b]),
                    "{i}: {h:?}"
                );
                // At most 2 Hz over an 11 px span: under 2.5 px per frame.
                assert!((h[b] - last[b]).abs() < 2.5, "{i}: {h:?} vs {last:?}");
                moved[b] |= (h[b] - last[b]).abs() > 0.05;
            }
            last = h;
        }
        assert!(moved.iter().all(|m| *m), "every bar moves from {start}");
    }

    #[test]
    fn g1_columns_fill_the_width_within_two_to_eight() {
        for (window, want) in [(760.0, 3), (1180.0, 5), (1440.0, 6), (2560.0, 8)] {
            let w = content_w(window);
            let n = columns(w);
            assert_eq!(n, want, "{window}");
            assert!((MIN_COLS..=MAX_COLS).contains(&n));
            assert!(
                card_width(w, n) >= MIN_CARD,
                "{window}: {}",
                card_width(w, n)
            );
            let used = n as f32 * card_width(w, n) + (n as f32 - 1.0) * GAP;
            assert!(used <= w && w - used < n as f32, "{window}");
        }
    }

    #[test]
    fn g1_a_card_is_184_wide_at_1440() {
        assert_eq!(card_width(content_w(1440.0), 6), 184.0);
        assert_eq!(content_rect(Rect::new(32.0, 52.0, 1376.0, 724.0)).x, 88.0);
    }
}
