//! The track table (MD59, amended by Q16): the rows of a list's loaded tracks, 50 px tall, drawn on
//! luneui's virtual list (`ui.table`'s rows are fixed at 28 px), with the table's header band over
//! it and the loading, empty and failed states of the list.
//!
//! The list gives scrolling, the keyboard cursor and the hover; this draws the columns (#, a cover
//! thumb and the Title with the artist under it, Album, the like heart, Time), the current
//! row and the hovered row's play glyph. A row plays on a press on its play glyph, on a double
//! click (two primary presses on one row within the double-click time, read from the frame's raw
//! events) or on Enter while the list is focused.

use std::time::Duration;

use lune_ui::color::Rgba;
use lune_ui::input::{CursorIcon, Event, Key, NamedKey, PointerButton};
use lune_ui::paint::{Rect, StrokeStyle, Xform};
use lune_ui::text::{Spec, Style};
use lune_ui::ui::{Sense, Ui};
use lune_ui::widgets::button::ButtonKind;
use lune_ui::widgets::virtual_list::{
    Heights, ListOptions, RowCtx, RowKey, SelectionMode, row_fill,
};
use whipspot_core::player::{PlayerAction, PlayerHandle};
use whipspot_core::provider::Track;

use crate::art::{Art, NoArt, cover};
use crate::draw::{P, alpha, caps, disc, led, mono, mono_width, text, text_c, text_r, width};
use crate::grid::{Covers, eq_bars};
use crate::icons::{Icon, icon};
use crate::library::{LOAD_FAILED, Slot};
use crate::likes::{LikeBook, heart};
use crate::now_playing::format_time;
use crate::theme::{Labels, Look};

/// Two presses within this time on one row are a double click (the toolkit's own interval).
const DOUBLE_CLICK: Duration = Duration::from_millis(400);
/// The strip under the table that tells a page failed.
const FAILED_STRIP: f32 = 44.0;
/// A row's pitch, and its drawn rect (4 px of air between filled rows).
pub const ROW_PITCH: f32 = 50.0;
const ROW_H: f32 = 46.0;
/// The header band: its labels' top is the area's top, the rule is `RULE_DY` under it and the
/// rows start `BAND_H` under it.
pub const RULE_DY: f32 = 30.0;
pub const BAND_H: f32 = 40.0;
/// The list is wider than the columns by this much on the left (the highlight's inset and the
/// drawn row's overhang) and on the right: the rows end 4 px past the columns and the scrollbar
/// has its own lane from 16 px past them, so its thumb never lies on a row. A press in the lane
/// is no row press.
const LIST_PAD_L: f32 = 8.0;
const LIST_PAD_R: f32 = 16.0;
/// A press this close to the table's left edge is on the row's play glyph.
const PLAY_ZONE: f32 = 44.0;
/// The album column needs a table at least this wide.
const ALBUM_MIN_W: f32 = 560.0;
/// A row's cover thumb (MD76): its side, and the space it takes in front of the title text.
const THUMB: f32 = 36.0;
const THUMB_SPACE: f32 = THUMB + 12.0;
/// The like heart's glyph, and the width of the square round it that a press counts for (the
/// heart's own hit area is the glyph grown by 7 px each side).
const HEART: f32 = 14.0;
const HEART_HIT: f32 = HEART + 14.0;
/// Edit mode (the queue, MD86). The grip's box is 6 x 10 with its left edge this far right of the
/// row fill's left; a press left of `GRIP_ZONE` px past the table's left edge is the grip's.
const GRIP_LEFT: f32 = 5.0;
const GRIP_ZONE: f32 = 10.0;
/// The remove button: its centre this far left of the heart's centre, the hit square's side, the
/// cross's span and the stroke (the heart outline's).
const REMOVE_DX: f32 = 34.0;
const REMOVE_HIT: f32 = 28.0;
const REMOVE_SPAN: f32 = 10.0;
const EDIT_STROKE: f32 = 1.5;
/// The "Next" tag: text size, padding each side, and the gap after the title.
const TAG_SIZE: f32 = 11.0;
const TAG_PAD: (f32, f32) = (6.0, 2.0);
const TAG_GAP: f32 = 8.0;
/// A picked-up row is drawn at this opacity.
const PICKED_ALPHA: f32 = 0.4;
/// The drop line's thickness, its inset from the row fill's ends and its end disc.
const DROP_H: f32 = 2.0;
const DROP_INSET: f32 = 8.0;
const DROP_DOT: f32 = 6.0;

/// How a caller edits the rows of one frame (the queue pane, MD86): set with
/// [`TrackTable::set_edit`] before [`TrackTable::frame`], which consumes it, so a frame that sets
/// nothing draws the plain table. Rows are named by their list index.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Edit {
    /// Names the list for its scroll position, so a change of its rows never resets it.
    pub key: u64,
    /// The list index of the row that plays now (by position, not by uri).
    pub current: Option<usize>,
    /// The list index that plays after it: tagged "Next".
    pub next: Option<usize>,
    /// Hovered rows show a grip (false while a filter is set).
    pub grip: bool,
    /// A row drawn as hovered whatever the pointer says (the demo's dumps).
    pub hover: Option<usize>,
    /// The row picked up by a drag: drawn faint with its grip lifted.
    pub dragging: Option<usize>,
    /// The slot a drag would drop into: a line above that list index (below the last row for
    /// the list's length).
    pub drop: Option<usize>,
}

/// What the edit controls of the last [`TrackTable::frame`] report.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct EditOut {
    /// The remove button was clicked on this row.
    pub remove: Option<usize>,
    /// A press landed on this row's grip, at this point.
    pub grip: Option<(usize, (f32, f32))>,
    /// A secondary click landed on this row.
    pub menu: Option<usize>,
}

/// What one drawn row shows besides its track.
#[derive(Debug, Clone, Copy, Default)]
struct RowMark {
    playing: bool,
    hovered: bool,
    /// The hover fill, drawn here when the list paints none (a forced hover).
    fill: Option<Rgba>,
    next: bool,
    dragging: bool,
}

/// The y of the line that marks `slot` among the drawn `rows`: the top of the row at `slot`, or
/// the bottom of the row above it; `None` when neither is drawn.
pub fn drop_y(rows: &[(usize, Rect)], slot: usize) -> Option<f32> {
    if let Some((_, r)) = rows.iter().find(|(i, _)| *i == slot) {
        return Some(r.y);
    }
    rows.iter()
        .find(|(i, _)| i + 1 == slot)
        .map(|(_, r)| r.y + ROW_PITCH)
}

/// The slot under height `y` among the drawn `rows` (in order): above a row's middle is a slot
/// before it, past the last row's middle the slot after it.
pub fn slot_at(rows: &[(usize, Rect)], y: f32) -> Option<usize> {
    let &(last, _) = rows.last()?;
    Some(
        rows.iter()
            .find(|(_, r)| y < r.y + ROW_PITCH / 2.0)
            .map_or(last + 1, |(i, _)| *i),
    )
}

/// What a frame of the table asks of its caller.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PaneResult {
    /// The next page is due: the last loaded row is in view, or nothing is loaded yet.
    pub more: bool,
    /// The Retry button was pressed.
    pub retry: bool,
    /// The row to play: its position in the visible rows.
    pub play: Option<usize>,
}

/// Which columns a page's table has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Form {
    /// Album pages: every row is one album, so the Album column and the artist line go.
    pub album_page: bool,
}

/// Where the columns sit, logical px.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cols {
    /// The centre of the # column.
    pub num: f32,
    /// The Title column's left edge: where the header label and the cover thumb start.
    pub title: f32,
    /// Where the title and artist text start: after the thumb, or at `title` without one.
    pub text: f32,
    /// The text's widest.
    pub title_w: f32,
    /// Rows carry a cover thumb at `title`.
    pub thumb: bool,
    pub album: Option<f32>,
    pub album_w: f32,
    /// The centre of the like heart.
    pub like: f32,
    /// The right edge of the durations.
    pub time_right: f32,
}

/// The columns of a table spanning `lx..right`. A `mixed` list (every row its own album) has the
/// cover thumb and, when the table is wide enough, the Album column; an album page has neither.
pub fn columns(lx: f32, right: f32, mixed: bool) -> Cols {
    let title = lx + 56.0;
    let end = right - 108.0;
    let span = (end - title).max(0.0);
    let album = mixed && right - lx >= ALBUM_MIN_W;
    let (title_w, album_x, album_w) = if album {
        let tw = span * 0.58;
        (tw - 12.0, Some(title + tw), span - tw)
    } else {
        (span, None, 0.0)
    };
    let space = if mixed { THUMB_SPACE } else { 0.0 };
    Cols {
        num: lx + 18.0,
        title,
        text: title + space,
        title_w: (title_w - space).max(0.0),
        thumb: mixed,
        album: album_x,
        album_w,
        like: right - 76.0,
        time_right: right - 8.0,
    }
}

/// The table's retained state: the last row press, for the double click.
#[derive(Debug, Default)]
pub struct TrackTable {
    last_press: Option<(u64, usize, Duration)>,
    edit: Option<Edit>,
    out: EditOut,
    /// The rows drawn in the last frame (position in the visible rows, slot) and the list's rect.
    rows: Vec<(usize, Rect)>,
    list: Rect,
}

impl TrackTable {
    /// Edits the next frame's rows as `edit` says (see [`Edit`]).
    pub fn set_edit(&mut self, edit: Edit) {
        self.edit = Some(edit);
    }

    /// What the edit controls of the last frame reported.
    pub fn edit_out(&self) -> EditOut {
        self.out
    }

    /// The rows drawn in the last frame: their position in the visible rows and their slot.
    pub fn rows(&self) -> &[(usize, Rect)] {
        &self.rows
    }

    /// The list's rect in the last frame.
    pub fn list_rect(&self) -> Rect {
        self.list
    }

    /// Draws the table in `area` (its header band at the top, the rows under it) for the
    /// `visible` rows of `slot`; `empty` is what a complete, empty list says and `current` the uri
    /// of the track that plays.
    ///
    /// `covers` gives the rows' thumbs. `hearts` is `Some(writable)` when the provider has likes:
    /// the rows then draw the like heart and a click on it goes to `likes` (a flip when `writable`,
    /// else the hint); the book dispatches through `player`.
    #[allow(clippy::too_many_arguments)]
    pub fn frame(
        &mut self,
        ui: &mut Ui<'_>,
        area: Rect,
        k: &Look,
        form: Form,
        slot: &Slot<Track>,
        visible: &[usize],
        empty: &str,
        current: Option<&str>,
        covers: &mut Covers<'_>,
        likes: &mut LikeBook,
        player: &PlayerHandle,
        hearts: Option<bool>,
    ) -> PaneResult {
        let mut result = PaneResult::default();
        let edit = self.edit.take();
        self.out = EditOut::default();
        self.rows.clear();
        let items = slot.items();
        if items.is_empty() {
            if slot.failed() {
                result.retry = failed_note(ui, area);
            } else if slot.loading() || slot.has_more() {
                note(ui, area, "Loading\u{2026}");
                result.more = true;
            } else {
                note(ui, area, empty);
            }
            return result;
        }

        let strip_h = if slot.failed() { FAILED_STRIP } else { 0.0 };
        let list_top = area.y + BAND_H;
        let list_h = (area.bottom() - strip_h - list_top).max(0.0);
        if slot.failed() {
            let strip = Rect::new(area.x, area.bottom() - strip_h, area.w, strip_h);
            result.retry = failed_strip(ui, strip);
        }
        let cols = columns(area.x, area.right(), !form.album_page);
        header(ui.painter(), k, &cols, area);
        if visible.is_empty() {
            // Loaded rows, none of them matching: more pages may still bring some.
            let rest = Rect::new(area.x, list_top, area.w, list_h);
            let theme = *ui.theme();
            let spec = Spec::plain(
                "No matching songs.",
                Style::ui(theme.font_base, k.ink.text2),
            );
            ui.painter().text_centered(&spec, rest);
            result.more = slot.has_more();
            return result;
        }

        let list = Rect::new(
            area.x - LIST_PAD_L,
            list_top,
            area.w + LIST_PAD_L + LIST_PAD_R,
            list_h,
        );
        let opts = ListOptions {
            count: visible.len(),
            heights: Heights::Fixed(ROW_PITCH),
            selection: SelectionMode::None,
            index_of: None,
            label: "Tracks",
        };
        let last = visible.len() - 1;
        let mut rows: Vec<(usize, Rect)> = Vec::new();
        let mut out = EditOut::default();
        let key = match edit {
            Some(e) => ("queue", e.key),
            None => ("tracks", slot.serial()),
        };
        let resp = ui.virtual_list(
            key,
            list,
            &opts,
            |i| RowKey(visible[i] as u64),
            |ui, ctx| {
                let li = visible[ctx.index];
                let track = &items[li];
                rows.push((ctx.index, ctx.rect));
                if ctx.index == last {
                    result.more = true;
                }
                let thumb = if cols.thumb {
                    let scale = ui.painter().s;
                    covers.get(&track.images, THUMB, scale)
                } else {
                    None
                };
                let mut mark = RowMark {
                    playing: current == Some(track.uri.as_str()),
                    hovered: ctx.hovered,
                    ..RowMark::default()
                };
                let mut tools = None;
                if let Some(e) = edit {
                    let forced = e.hover == Some(li);
                    let dragging = e.dragging == Some(li);
                    mark.playing = e.current == Some(li);
                    mark.next = e.next == Some(li);
                    mark.dragging = dragging;
                    mark.hovered = ctx.hovered || forced || dragging;
                    if forced && !ctx.hovered {
                        mark.fill = Some(row_fill(ui.theme(), true, false));
                    }
                    tools = mark.hovered.then_some((e.grip, dragging));
                }
                draw_row(ui.painter(), k, &cols, area, ctx, track, (mark, thumb));
                if let Some((grip, dragging)) = tools {
                    let my = ctx.rect.y + 2.0 + ROW_H / 2.0;
                    if grip {
                        let hit = Rect::new(area.x - 4.0, ctx.rect.y + 2.0, GRIP_ZONE + 4.0, ROW_H);
                        let id = ui.id(format!("row-grip-{}", ctx.key.0));
                        let r = ui.interact(id, hit, Sense::DRAG);
                        if r.hovered || r.pressed {
                            ui.set_cursor(CursorIcon::Grab);
                        }
                        if r.press_started {
                            out.grip = Some((li, r.press_pos.unwrap_or((hit.x, hit.y))));
                        }
                        let c = if r.hovered || r.pressed || dragging {
                            k.ink.text
                        } else {
                            k.ink.muted
                        };
                        let gx = area.x - 4.0 + GRIP_LEFT;
                        for dx in [1.0, 5.0] {
                            for dy in [-4.0, 0.0, 4.0] {
                                disc(ui.painter(), (gx + dx, my + dy), 2.0, c);
                            }
                        }
                    }
                    let cx = cols.like - REMOVE_DX;
                    let hit = Rect::new(
                        cx - REMOVE_HIT / 2.0,
                        my - REMOVE_HIT / 2.0,
                        REMOVE_HIT,
                        REMOVE_HIT,
                    );
                    let id = ui.id(format!("row-remove-{}", ctx.key.0));
                    let r = ui.interact(id, hit, Sense::CLICK);
                    if r.hovered {
                        ui.set_cursor(CursorIcon::Pointer);
                    }
                    if r.clicked {
                        out.remove = Some(li);
                    }
                    let c = if r.hovered { k.ink.text } else { k.ink.muted };
                    let h = REMOVE_SPAN / 2.0;
                    let st = StrokeStyle::solid(c, EDIT_STROKE);
                    let p = ui.painter();
                    p.stroke(&[[cx - h, my - h], [cx + h, my + h]], false, st);
                    p.stroke(&[[cx - h, my + h], [cx + h, my - h]], false, st);
                    ui.tooltip(&r, "Remove from queue");
                }
                if let Some(writable) = hearts {
                    let glyph = Rect::new(
                        cols.like - HEART / 2.0,
                        ctx.rect.y + 2.0 + (ROW_H - HEART) / 2.0,
                        HEART,
                        HEART,
                    );
                    let key = format!("row-like-{}", ctx.key.0);
                    let liked = likes.state(&track.uri);
                    if heart(ui, &key, glyph, k, liked, writable, ctx.hovered) {
                        likes.click(&track.uri, writable, player);
                    }
                }
                ROW_PITCH
            },
        );

        if let Some(e) = edit {
            if let Some(slot) = e.drop
                && let Some(y) = drop_y(&rows, slot)
                && y >= list.y
                && y <= list.bottom()
            {
                let p = ui.painter();
                let x = area.x - 4.0 + DROP_INSET;
                let w = area.w + 8.0 - 2.0 * DROP_INSET;
                p.fill(Rect::new(x, y - DROP_H / 2.0, w, DROP_H), DROP_H / 2.0, k.a);
                disc(p, (x, y), DROP_DOT, k.a);
            }
            out.menu = resp.context.map(|r| r.0 as usize);
            self.out = out;
        }
        self.rows.extend_from_slice(&rows);
        self.list = list;

        let now = ui.time();
        let body = Rect::new(list.x, list.y, area.w + 2.0 * LIST_PAD_L, list.h);
        let mut play = None;
        for event in &ui.input().events {
            match event {
                Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed: true,
                    ..
                } if body.contains(*pos) => {
                    // A press on a heart is the heart's, not a row press.
                    if hearts.is_some() && (pos.0 - cols.like).abs() <= HEART_HIT / 2.0 {
                        continue;
                    }
                    // Nor is a press on the edit controls: the grip and the remove button.
                    if let Some(e) = edit
                        && ((e.grip && pos.0 < area.x + GRIP_ZONE)
                            || (pos.0 - (cols.like - REMOVE_DX)).abs() <= REMOVE_HIT / 2.0)
                    {
                        continue;
                    }
                    let Some(&(row, _)) = rows
                        .iter()
                        .find(|(_, r)| pos.1 >= r.y && pos.1 < r.y + ROW_PITCH)
                    else {
                        continue;
                    };
                    if pos.0 < area.x + PLAY_ZONE {
                        play = Some(row);
                        self.last_press = None;
                        continue;
                    }
                    let serial = slot.serial();
                    match self.last_press {
                        Some((s, r, at))
                            if s == serial && r == row && now.saturating_sub(at) < DOUBLE_CLICK =>
                        {
                            play = Some(row);
                            self.last_press = None;
                        }
                        _ => self.last_press = Some((serial, row, now)),
                    }
                }
                Event::Key {
                    key: Key::Named(NamedKey::Enter),
                    pressed: true,
                    repeat: false,
                    ..
                } if resp.response.focused => {
                    let cursor = ui.list_state(key).cursor.map(|c| c.0 as usize);
                    play = cursor.and_then(|orig| visible.iter().position(|&i| i == orig));
                }
                _ => {}
            }
        }
        result.play = play;
        result
    }
}

/// The header band at the top of `area`: the column labels, the clock over the durations and a
/// rule under them.
fn header(p: &mut P<'_>, k: &Look, cols: &Cols, area: Rect) {
    let ink = k.ink;
    let y = area.y;
    if k.decor().labels == Labels::Tracked {
        caps(p, "#", 9.5, ink.faint, cols.num - 3.0, y + 2.0, 1.6);
        caps(p, "TITLE", 9.5, ink.faint, cols.title, y + 2.0, 1.6);
        if let Some(x) = cols.album {
            caps(p, "ALBUM", 9.5, ink.faint, x, y + 2.0, 1.6);
        }
    } else {
        text_c(p, "#", 12.5, ink.faint, false, cols.num, y);
        text(p, "Title", 12.5, ink.faint, false, cols.title, y, 0.0);
        if let Some(x) = cols.album {
            text(p, "Album", 12.5, ink.faint, false, x, y, 0.0);
        }
    }
    icon(
        p,
        Icon::Clock,
        (cols.time_right - 12.0, y + 9.0),
        13.0,
        ink.faint,
        1.2,
    );
    p.fill(
        Rect::new(area.x, y + RULE_DY, area.w, 1.0),
        0.0,
        ink.line_soft,
    );
}

/// One row: the current or hovered fill, the # slot (number, play glyph or the playing mark), the
/// cover thumb (`mark.playing` and the registered cover in `thumb`), the title (with the artist
/// under it and the "Next" tag after it when `mark.next`), the album and the time. The like heart
/// is drawn by the caller; `mark.dragging` draws the whole row faint.
fn draw_row(
    p: &mut P<'_>,
    k: &Look,
    cols: &Cols,
    area: Rect,
    ctx: &RowCtx,
    track: &Track,
    (mark, thumb): (RowMark, Option<Art>),
) {
    let ink = k.ink;
    let d = k.decor();
    let playing = mark.playing;
    let rr = Rect::new(area.x - 4.0, ctx.rect.y + 2.0, area.w + 8.0, ROW_H);
    let radius = k.radii().card.min(14.0);
    let my = rr.y + rr.h / 2.0;
    if mark.dragging {
        p.push_xform(Xform {
            origin: (0.0, 0.0),
            scale: 1.0,
            dx: 0.0,
            dy: 0.0,
            alpha: PICKED_ALPHA,
        });
    }
    if let Some(fill) = mark.fill {
        p.fill(rr, radius, fill);
    }
    if playing {
        match d.labels {
            Labels::Marked => p.fill(rr, radius, alpha(k.a, 0x22)),
            Labels::Tracked => {
                p.fill(rr, radius, alpha(k.a, 0x14));
                p.ring(rr, radius, 1.0, alpha(k.a, 0x40), false);
            }
            Labels::Plain => {
                p.fill(rr, radius, alpha(k.a, 0x1C));
                p.ring(rr, radius, 1.0, alpha(k.a, 0x40), false);
            }
        }
        if d.labels == Labels::Tracked {
            led(p, (cols.num, my), 6.0, k.a, k.hi, true);
        } else {
            eq_bars(p, (cols.num, my), k.a);
        }
    } else if mark.hovered {
        icon(p, Icon::Play, (cols.num + 1.0, my), 11.0, ink.text, 1.4);
    } else {
        let n = (ctx.key.0 + 1).to_string();
        if d.mono_numbers {
            let nw = mono_width(p, &n, 12.5);
            mono(p, &n, 12.5, ink.faint, cols.num - nw / 2.0, my - 8.0);
        } else {
            text_c(p, &n, 13.5, ink.faint, false, cols.num, my - 9.0);
        }
    }

    let (tc, medium) = if playing {
        (k.hi_or_text(), true)
    } else {
        (ink.text, false)
    };
    let tag_w = if mark.next {
        width(p, "Next", TAG_SIZE, true) + 2.0 * TAG_PAD.0
    } else {
        0.0
    };
    // The title stops short of the tag.
    let tw = if mark.next {
        (cols.title_w - tag_w - TAG_GAP).max(0.0)
    } else {
        cols.title_w
    };
    let title_y = if cols.thumb { my - 18.0 } else { my - 10.0 };
    if cols.thumb {
        let r = Rect::new(cols.title, my - THUMB / 2.0, THUMB, THUMB);
        let radius = k.radii().thumb * THUMB / 56.0;
        cover(p, k, r, radius, thumb, NoArt::Thumb);
    }
    let name_w = text(p, &track.name, 15.0, tc, medium, cols.text, title_y, tw);
    if cols.thumb {
        let artist = &track.artist;
        text(
            p,
            artist,
            13.0,
            ink.muted,
            false,
            cols.text,
            my + 2.0,
            cols.title_w,
        );
    }
    if mark.next {
        let h = TAG_SIZE + 2.0 * TAG_PAD.1;
        let r = Rect::new(cols.text + name_w + TAG_GAP, title_y + 1.0, tag_w, h);
        p.ring(r, h / 2.0, 1.0, alpha(k.a, 0x99), false);
        text(
            p,
            "Next",
            TAG_SIZE,
            k.a,
            true,
            r.x + TAG_PAD.0,
            r.y + TAG_PAD.1,
            0.0,
        );
    }
    if let Some(x) = cols.album {
        let aw = cols.album_w - 12.0;
        text(p, &track.album, 13.5, ink.muted, false, x, my - 9.0, aw);
    }
    let time = format_time(track.duration_ms);
    if d.mono_numbers {
        let w = mono_width(p, &time, 12.5);
        mono(p, &time, 12.5, ink.muted, cols.time_right - w, my - 8.0);
    } else {
        text_r(p, &time, 13.5, ink.muted, false, cols.time_right, my - 9.0);
    }
    if mark.dragging {
        p.pop_xform();
    }
}

/// Plays `slot`'s loaded tracks from `index`.
pub fn play_from(player: &PlayerHandle, slot: &Slot<Track>, index: usize) {
    player.dispatch(PlayerAction::PlayItem {
        context: slot.items().to_vec(),
        index,
    });
}

/// A line of secondary text centred in `rect`.
pub fn note(ui: &mut Ui<'_>, rect: Rect, text: &str) {
    let theme = *ui.theme();
    let spec = Spec::plain(text, Style::ui(theme.font_base, theme.fg_tertiary));
    ui.painter().text_centered(&spec, rect);
}

/// The failure text and a Retry button, centred in `rect`; whether Retry was pressed.
fn failed_note(ui: &mut Ui<'_>, rect: Rect) -> bool {
    let theme = *ui.theme();
    let spec = Spec::plain(LOAD_FAILED, Style::ui(theme.font_base, theme.fg_secondary));
    let (w, h) = ui.painter().measure(&spec);
    let y = rect.y + (rect.h - h - 12.0 - 32.0) / 2.0;
    ui.painter().text(&spec, rect.x + (rect.w - w) / 2.0, y);
    let button = Rect::new(rect.x + (rect.w - 96.0) / 2.0, y + h + 12.0, 96.0, 32.0);
    ui.button("retry", button, "Retry", ButtonKind::Secondary)
        .clicked
}

/// The failure text and a Retry button in a strip under loaded rows.
fn failed_strip(ui: &mut Ui<'_>, strip: Rect) -> bool {
    let theme = *ui.theme();
    let spec = Spec::plain(LOAD_FAILED, Style::ui(theme.font_base, theme.fg_secondary));
    ui.painter().text_in(
        &spec,
        Rect::new(strip.x + 16.0, strip.y, strip.w - 128.0, strip.h),
        0.0,
    );
    let button = Rect::new(strip.right() - 96.0 - 16.0, strip.y + 6.0, 96.0, 32.0);
    ui.button("retry", button, "Retry", ButtonKind::Secondary)
        .clicked
}

/// Plays the `visible` rows of `slot` from position `pos`: the context is what the table shows.
pub fn play_visible(player: &PlayerHandle, slot: &Slot<Track>, visible: &[usize], pos: usize) {
    let items = slot.items();
    player.dispatch(PlayerAction::PlayItem {
        context: visible
            .iter()
            .filter_map(|&i| items.get(i).cloned())
            .collect(),
        index: pos,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r2_columns_at_1440_with_and_without_the_thumb_column() {
        // A mixed list: thumb, then the text 48 px in, and the Album column.
        let c = columns(460.0, 1352.0, true);
        assert_eq!(
            (c.num, c.title, c.time_right, c.like),
            (478.0, 516.0, 1344.0, 1276.0)
        );
        assert!(c.thumb);
        assert_eq!(
            c.text,
            c.title + 48.0,
            "the text starts at thumb.right + 12"
        );
        let album = c.album.expect("a 892 px table has the album column");
        assert!(c.text + c.title_w < album && album + c.album_w <= 1352.0 - 100.0);
        // The thumb takes its 48 px from the title text, not from the other columns.
        let wide = columns(460.0, 1352.0, false);
        let span = (1352.0 - 108.0) - 516.0;
        assert_eq!(c.album, Some(c.title + span * 0.58));
        assert_eq!(c.title_w, span * 0.58 - 12.0 - 48.0);
        // An album page: no thumb, no album column, the text at the title column as in M5.
        assert!(!wide.thumb);
        assert_eq!((wide.album, wide.text), (None, wide.title));
        assert_eq!(wide.title_w, 1352.0 - 108.0 - 516.0);
        assert_eq!(
            (wide.num, wide.like, wide.time_right),
            (c.num, c.like, c.time_right)
        );
        // Too narrow for an album column: the thumb stays, the text is what is left.
        let narrow = columns(360.0, 812.0, true);
        assert_eq!(narrow.album, None);
        assert!(narrow.thumb);
        assert_eq!(narrow.title_w, (812.0 - 108.0) - 416.0 - 48.0);
        // A table too narrow for the thumb's space never gets a negative width.
        assert!(columns(0.0, 120.0, true).title_w >= 0.0);
    }

    #[test]
    fn drop_slots_follow_the_drawn_rows() {
        // Rows 3..6 are drawn, row 3's slot at y = 100.
        let rows: Vec<(usize, Rect)> = (3..6)
            .map(|i| {
                (
                    i,
                    Rect::new(0.0, 100.0 + (i - 3) as f32 * ROW_PITCH, 100.0, ROW_PITCH),
                )
            })
            .collect();
        assert_eq!(slot_at(&rows, 0.0), Some(3), "above the first row");
        assert_eq!(slot_at(&rows, 110.0), Some(3), "upper half of a row");
        assert_eq!(slot_at(&rows, 130.0), Some(4), "lower half: after it");
        assert_eq!(slot_at(&rows, 1000.0), Some(6), "below the last row");
        assert_eq!(slot_at(&[], 5.0), None);
        assert_eq!(drop_y(&rows, 4), Some(150.0));
        assert_eq!(drop_y(&rows, 6), Some(250.0), "under the last drawn row");
        assert_eq!(drop_y(&rows, 2), None);
        assert_eq!(drop_y(&rows, 7), None);
    }
}
