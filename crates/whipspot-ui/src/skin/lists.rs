//! The skin's track list and library (step 5): luneui's virtualized list with the skin's list
//! colours. Both lists draw rows of 11 skin pixels with 9 skin pixel text in the app's UI font
//! (`list_font` is ignored); no header, hover fill or search field (D6 as amended).
//!
//! The list functions return what they want done (player actions, pages to ask for, a failed
//! list to retry) in a [`ListOut`] and the caller sends it, so the click, key and navigation
//! logic is tested on the toolkit's scripted frames without a player.
//!
//! The toolkit paints its own row highlight in the host's theme under each row; every row here
//! first covers its slot with the list's opaque background, so only the skin's colours show.

use std::collections::BTreeSet;
use std::time::Duration;

use lune_ui::input::{Event, Key, NamedKey, PointerButton, ScrollDelta};
use lune_ui::paint::Rect;
use lune_ui::text::{Spec, Style};
use lune_ui::theme::{DARK, Theme};
use lune_ui::ui::Ui;
use lune_ui::widgets::virtual_list::{Heights, ListOptions, RowCtx, RowKey, SelectionMode};
use whipspot_core::player::{PlayerAction, QueueState};
use whipspot_core::provider::{Album, Playlist, Track};
use whipspot_core::skin::{Rect as SkinRect, Rgb, SkinTheme};
use whipspot_core::text::display_text;

use super::input::ScrollOp;
use crate::library::{Access, LOAD_FAILED, LibraryModel, ListKind, Slot};
use crate::now_playing::format_time;

/// A row's pitch, the text's size and the padding at each side, in skin pixels.
const ROW: f32 = 11.0;
const TEXT: f32 = 9.0;
const PAD: f32 = 2.0;
/// The least space between a name and the duration after it, in skin pixels.
const GAP: f32 = 3.0;
/// Two presses within this time on one row are a double click (the toolkit's own interval).
const DOUBLE_CLICK: Duration = Duration::from_millis(400);
/// Longest name drawn, in characters.
const MAX_NAME: usize = 200;

/// The list theme of a skin: the dark preset with the colours the skin sets. The list background
/// is `surface_input`, the text `fg_primary`, the current row's text `fg_bright` and the selected
/// row's fill `surface_selected`; the background is always opaque.
pub(super) fn list_theme(skin: &SkinTheme) -> Theme {
    let solid = |c: [u8; 4]| [c[0], c[1], c[2], 255];
    let opaque = |c: Rgb| [c.r, c.g, c.b, 255];
    let mut t = DARK;
    t.surface_input = skin
        .list_background
        .map_or(solid(DARK.surface_input), opaque);
    t.fg_primary = skin.list_text.map_or(DARK.fg_primary, opaque);
    t.fg_bright = skin.list_text_current.map_or(DARK.fg_bright, opaque);
    t.surface_selected = skin.list_selection.map_or(DARK.surface_selected, opaque);
    t
}

/// What the lists of one frame ask for.
#[derive(Default)]
pub(super) struct ListOut {
    pub actions: Vec<PlayerAction>,
    /// Lists that want their next page.
    pub more: Vec<ListKind>,
    /// Lists whose failed page the user asked to retry.
    pub retry: Vec<ListKind>,
}

/// The last primary press on a row, for double clicks.
#[derive(Default)]
struct Presses {
    last: Option<(usize, Duration)>,
}

impl Presses {
    /// Takes a press on `row` at `now`; whether it completes a double click.
    fn press(&mut self, row: usize, now: Duration) -> bool {
        match self.last {
            Some((r, at)) if r == row && now.saturating_sub(at) < DOUBLE_CLICK => {
                self.last = None;
                true
            }
            _ => {
                self.last = Some((row, now));
                false
            }
        }
    }
}

/// What the queue list keeps between frames.
#[derive(Default)]
pub(super) struct TrackListView {
    presses: Presses,
}

/// Where the library is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Nav {
    #[default]
    Top,
    Liked,
    Playlists,
    Albums,
    Playlist,
    Album,
}

impl Nav {
    fn kind(self) -> Option<ListKind> {
        match self {
            Nav::Top => None,
            Nav::Liked => Some(ListKind::Liked),
            Nav::Playlists => Some(ListKind::Playlists),
            Nav::Albums => Some(ListKind::Albums),
            Nav::Playlist => Some(ListKind::OpenPlaylist),
            Nav::Album => Some(ListKind::OpenAlbum),
        }
    }
}

/// What the library list keeps between frames.
#[derive(Default)]
pub(super) struct LibraryView {
    nav: Nav,
    presses: Presses,
}

/// Everything the skin's lists keep: the library model (its own, fed by the notices the skin
/// front-end drains while it runs) and the views.
#[derive(Default)]
pub(super) struct Lists {
    pub model: LibraryModel,
    tracks: TrackListView,
    library: LibraryView,
    pub out: ListOut,
    /// The scroll position of each panel that runs its list in rows (it has a scroll bar).
    scrolls: RowScrolls,
}

impl Lists {
    /// Draws the queue list (see [`track_list`]); `rows` is set when the panel has a scroll bar.
    pub fn track_list(
        &mut self,
        ui: &mut Ui<'_>,
        key: (usize, usize),
        (rect, s): (Rect, f32),
        th: &Theme,
        queue: &QueueState,
        rows: Option<RowsGeom>,
    ) {
        let rows = rows.map(|g| (self.scrolls.of(key.0), g));
        track_list(
            ui,
            &mut self.tracks,
            key,
            (rect, s),
            th,
            queue,
            &mut self.out,
            rows,
        );
    }

    /// Draws the library list (see [`library`]); `rows` is set when the panel has a scroll bar.
    pub fn library(
        &mut self,
        ui: &mut Ui<'_>,
        access: Access<'_>,
        key: (usize, usize),
        (rect, s): (Rect, f32),
        th: &Theme,
        rows: Option<RowsGeom>,
    ) {
        let rows = rows.map(|g| (self.scrolls.of(key.0), g));
        library(
            ui,
            &mut self.library,
            &mut self.model,
            access,
            key,
            (rect, s),
            th,
            &mut self.out,
            rows,
        );
    }

    /// The scroll position of `panel`'s list in `0.0..=1.0` (0 when everything fits or the panel
    /// has no scroll bar).
    pub fn scroll_fraction(&self, panel: usize) -> f32 {
        self.scrolls.get(panel).map_or(0.0, RowScroll::fraction)
    }

    /// Applies what the pointer did to `panel`'s scroll bar.
    pub fn scroll_op(&mut self, panel: usize, op: ScrollOp) {
        self.scrolls.of(panel).op(op);
    }

    /// Forgets every scroll position (a skin was loaded or dropped).
    pub fn reset_scrolls(&mut self) {
        self.scrolls = RowScrolls::default();
    }
}

/// A panel's scroll bar and list in skin pixels, as the rows mode needs them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RowsGeom {
    /// The bar's rect: the track the thumb moves in.
    pub bar: SkinRect,
    /// The thumb's height.
    pub thumb_h: u32,
    /// The list's height.
    pub list_h: u32,
}

/// The top of the thumb in skin pixels for the scroll position `f` in `0.0..=1.0`:
/// `bar_y + round(f * (bar_h - thumb_h))`.
pub(super) fn thumb_y(bar_y: u32, bar_h: u32, thumb_h: u32, f: f32) -> i64 {
    let travel = bar_h.saturating_sub(thumb_h);
    i64::from(bar_y) + (f.clamp(0.0, 1.0) * travel as f32).round() as i64
}

/// The list of a panel with a scroll bar scrolls in whole rows: the toolkit's list is given only
/// the rows in view, so its content fits and it never scrolls or draws a bar of its own. This is
/// the position the keys, the wheel and the thumb share.
#[derive(Default)]
pub(super) struct RowScroll {
    /// The row at the top.
    first: usize,
    /// The rows and the whole rows in view, as of the last frame of the list.
    count: usize,
    n: usize,
    /// The list's identity (the library level): a new one starts at the top.
    serial: u64,
    /// Wheel rows not yet applied (a touchpad gives fractions).
    acc: f32,
    /// The list had the keyboard focus last frame.
    focused: bool,
    geom: Option<RowsGeom>,
    /// Where on the thumb it was grabbed, in skin pixels, while it is held.
    grab: Option<f32>,
}

impl RowScroll {
    fn max_top(&self) -> usize {
        self.count.saturating_sub(self.n)
    }

    /// `first / (count - n)`, 0 when everything fits.
    pub fn fraction(&self) -> f32 {
        match self.max_top() {
            0 => 0.0,
            m => self.first.min(m) as f32 / m as f32,
        }
    }

    fn op(&mut self, op: ScrollOp) {
        match op {
            ScrollOp::Press(y) => {
                let Some(g) = self.geom else {
                    return;
                };
                let top = thumb_y(g.bar.y, g.bar.h, g.thumb_h, self.fraction()) as f32;
                // A press on the thumb keeps its grab; on the bar it centres the thumb.
                self.grab = Some(if y >= top && y < top + g.thumb_h as f32 {
                    y - top
                } else {
                    g.thumb_h as f32 / 2.0
                });
                self.drag(y);
            }
            ScrollOp::Move(y) => self.drag(y),
            ScrollOp::Release => self.grab = None,
        }
    }

    /// Moves the held thumb to the pointer row `y`.
    fn drag(&mut self, y: f32) {
        let (Some(g), Some(grab)) = (self.geom, self.grab) else {
            return;
        };
        let travel = g.bar.h.saturating_sub(g.thumb_h);
        let max_top = self.max_top();
        if travel == 0 || max_top == 0 {
            return;
        }
        let f = ((y - grab - g.bar.y as f32) / travel as f32).clamp(0.0, 1.0);
        self.first = (f * max_top as f32).round() as usize;
    }
}

/// The scroll positions by panel index.
#[derive(Default)]
struct RowScrolls(Vec<(usize, RowScroll)>);

impl RowScrolls {
    fn get(&self, panel: usize) -> Option<&RowScroll> {
        self.0.iter().find(|e| e.0 == panel).map(|e| &e.1)
    }

    fn of(&mut self, panel: usize) -> &mut RowScroll {
        let at = match self.0.iter().position(|e| e.0 == panel) {
            Some(at) => at,
            None => {
                self.0.push((panel, RowScroll::default()));
                self.0.len() - 1
            }
        };
        &mut self.0[at].1
    }
}

/// One row's text.
struct RowText {
    left: String,
    right: Option<String>,
    current: bool,
}

impl RowText {
    fn plain(left: impl Into<String>) -> Self {
        RowText {
            left: left.into(),
            right: None,
            current: false,
        }
    }
}

/// `Playing from index` of `items`: the action a double click on a track row dispatches.
fn play(items: &[Track], index: usize) -> Option<PlayerAction> {
    (index < items.len()).then(|| PlayerAction::PlayItem {
        context: items.to_vec(),
        index,
    })
}

/// A track row: `n. artist - title` and the duration.
fn track_text(n: usize, t: &Track, current: bool) -> RowText {
    RowText {
        left: display_text(&format!("{n}. {} - {}", t.artist, t.name), MAX_NAME),
        right: Some(format_time(t.duration_ms)),
        current,
    }
}

/// What a frame of a list found.
#[derive(Default)]
struct Run {
    /// Rows pressed with the primary button, in order.
    pressed: Vec<usize>,
    /// Delete was pressed while the list had the keyboard focus.
    delete: bool,
    /// The selected row, if any.
    selected: Option<usize>,
    /// The last row was built: the list is scrolled to its end.
    at_end: bool,
}

/// Draws one row: the slot covered in the list background, the selection fill, the text (a long
/// name elided before the duration).
fn paint_row(ui: &mut Ui<'_>, ctx: &RowCtx, s: f32, th: &Theme, row: &RowText) {
    let p = ui.painter();
    p.fill(ctx.rect, 0.0, th.surface_input);
    if ctx.selected {
        p.fill(ctx.rect, 0.0, th.surface_selected);
    }
    let colour = if row.current {
        th.fg_bright
    } else {
        th.fg_primary
    };
    let style = Style::ui(TEXT * s, colour);
    let mut room = ctx.rect.w - 2.0 * PAD * s;
    if let Some(right) = &row.right {
        let spec = Spec::plain(right.as_str(), style);
        let (w, _) = p.measure(&spec);
        let x = ctx.rect.right() - PAD * s - w;
        p.text_in(&spec, Rect::new(x, ctx.rect.y, w, ctx.rect.h), 0.0);
        room -= w + GAP * s;
    }
    let spec = p.elide(&Spec::plain(row.left.as_str(), style), room.max(0.0));
    p.text_in(&spec, ctx.rect, PAD * s);
}

/// Runs one list of `count` rows over `rect`: the background, the rows (`text` gives a row's
/// text), and the frame's presses and Delete key. `key` names the list's retained state (its
/// scroll position and selection).
///
/// With `rows` (the panel has a scroll bar) the list runs in whole rows, see [`run_rows`].
fn run_list(
    ui: &mut Ui<'_>,
    key: (usize, usize, u64),
    (rect, s): (Rect, f32),
    th: &Theme,
    count: usize,
    text: &dyn Fn(usize) -> RowText,
    rows: Option<(&mut RowScroll, RowsGeom)>,
) -> Run {
    if let Some(rows) = rows {
        return run_rows(ui, key, (rect, s), th, count, text, rows);
    }
    ui.painter().fill(rect, 0.0, th.surface_input);
    let mut run = Run::default();
    if count == 0 || rect.w <= 0.0 || rect.h <= 0.0 {
        return run;
    }
    let k = ("skin-list", key.0, key.1, key.2);
    let opts = ListOptions {
        count,
        heights: Heights::Fixed(ROW * s),
        selection: SelectionMode::Single,
        index_of: None,
        label: "List",
    };
    let mut rows: Vec<(usize, Rect)> = Vec::new();
    let resp = ui.virtual_list(
        k,
        rect,
        &opts,
        |i| RowKey(i as u64),
        |ui, ctx| {
            rows.push((ctx.index, ctx.rect));
            paint_row(ui, ctx, s, th, &text(ctx.index));
            ROW * s
        },
    );
    run.at_end = rows.iter().any(|(i, _)| *i + 1 == count);
    for event in &ui.input().events {
        match event {
            Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed: true,
                ..
            } if rect.contains(*pos) => {
                if let Some(&(row, _)) =
                    rows.iter().find(|(_, r)| pos.1 >= r.y && pos.1 < r.y + r.h)
                {
                    run.pressed.push(row);
                }
            }
            Event::Key {
                key: Key::Named(NamedKey::Delete),
                pressed: true,
                repeat: false,
                ..
            } if resp.response.focused => run.delete = true,
            _ => {}
        }
    }
    let state = ui.list_state(k);
    state.retain(|key| (key.0 as usize) < count);
    run.selected = state.selection.iter().next().map(|key| key.0 as usize);
    run
}

/// The height of `shown` rows of `row_h` logical pixels, never less than the toolkit's own sum, so
/// its content never reads as taller than the list and no bar of its own appears.
fn rows_height(shown: usize, row_h: f32) -> f32 {
    let total = shown as f64 * f64::from(row_h);
    let h = total as f32;
    if f64::from(h) < total && h.is_finite() && h >= 0.0 {
        // The next float up (the bits of a non-negative float count upward).
        f32::from_bits(h.to_bits() + 1)
    } else {
        h
    }
}

/// The rows mode of [`run_list`] (the panel has a scroll bar): `scroll.first` is the top row and
/// the toolkit gets only the rows in view, under absolute keys. The selection is the front-end's
/// own (the toolkit's list runs without one), so a key that scrolls and selects at once shows its
/// row in the frame it came.
fn run_rows(
    ui: &mut Ui<'_>,
    key: (usize, usize, u64),
    (rect, s): (Rect, f32),
    th: &Theme,
    count: usize,
    text: &dyn Fn(usize) -> RowText,
    (scroll, geom): (&mut RowScroll, RowsGeom),
) -> Run {
    ui.painter().fill(rect, 0.0, th.surface_input);
    let mut run = Run::default();
    scroll.geom = Some(geom);
    if scroll.serial != key.2 {
        scroll.serial = key.2;
        scroll.first = 0;
        scroll.acc = 0.0;
    }
    let n = (geom.list_h / ROW as u32) as usize;
    scroll.count = count;
    scroll.n = n;
    let max_top = scroll.max_top();
    scroll.first = scroll.first.min(max_top);
    if count == 0 || n == 0 || rect.w <= 0.0 || rect.h <= 0.0 {
        scroll.focused = false;
        return run;
    }
    let row_h = ROW * s;
    let k = ("skin-list", key.0, key.1, key.2);

    // What this frame did to the list. A press lands on the row the user saw: the top row as the
    // frame began.
    let first0 = scroll.first;
    let shown0 = n.min(count - first0);
    let bar = Rect::new(
        geom.bar.x as f32 * s,
        geom.bar.y as f32 * s,
        geom.bar.w as f32 * s,
        geom.bar.h as f32 * s,
    );
    let mut wheel = 0.0f32;
    let mut pressed = Vec::new();
    let mut keys = Vec::new();
    for event in &ui.input().events {
        match event {
            Event::Scroll { pos, delta, .. } if rect.contains(*pos) || bar.contains(*pos) => {
                let rows = match *delta {
                    ScrollDelta::Lines { y, .. } => -y,
                    ScrollDelta::Pixels { y, .. } => -y / row_h,
                };
                if rows.is_finite() {
                    wheel += rows;
                }
            }
            Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed: true,
                ..
            } if rect.contains(*pos) => {
                let at = ((pos.1 - rect.y) / row_h).floor();
                if at >= 0.0 && (at as usize) < shown0 {
                    pressed.push(first0 + at as usize);
                }
            }
            Event::Key {
                key: Key::Named(named),
                pressed: true,
                modifiers,
                ..
            } if scroll.focused
                && !(modifiers.control || modifiers.alt || modifiers.logo)
                && matches!(
                    named,
                    NamedKey::ArrowUp
                        | NamedKey::ArrowDown
                        | NamedKey::PageUp
                        | NamedKey::PageDown
                        | NamedKey::Home
                        | NamedKey::End
                ) =>
            {
                keys.push(*named);
            }
            _ => {}
        }
    }

    // The wheel: one row per line, toward row 0 for wheel up.
    if wheel != 0.0 {
        scroll.acc += wheel;
        let step = scroll.acc.trunc();
        scroll.acc -= step;
        let want = scroll.first as i64 + step as i64;
        let got = want.clamp(0, max_top as i64);
        if got != want {
            scroll.acc = 0.0;
        }
        scroll.first = got as usize;
    }

    // The selection and the keyboard cursor, both an absolute row.
    let mut cursor = ui
        .list_state(k)
        .cursor
        .map(|c| c.0 as usize)
        .filter(|c| *c < count);
    let mut moved = false;
    for &row in &pressed {
        cursor = Some(row);
        moved = true;
        run.pressed.push(row);
    }
    let last = count - 1;
    for named in keys {
        let from = cursor.unwrap_or(scroll.first);
        let to = match named {
            NamedKey::ArrowUp if cursor.is_some() => from.saturating_sub(1),
            NamedKey::ArrowDown if cursor.is_some() => (from + 1).min(last),
            NamedKey::ArrowUp | NamedKey::ArrowDown => from,
            NamedKey::PageUp => match from.saturating_sub(n) {
                to if to == from => from.saturating_sub(1),
                to => to,
            },
            NamedKey::PageDown => match (from + n).min(last) {
                to if to == from => (from + 1).min(last),
                to => to,
            },
            NamedKey::Home => 0,
            _ => last,
        };
        // Scroll just far enough to show the row.
        if to < scroll.first {
            scroll.first = to;
        } else if to >= scroll.first + n {
            scroll.first = to + 1 - n;
        }
        cursor = Some(to);
        moved = true;
    }
    if moved && let Some(c) = cursor {
        let st = ui.list_state(k);
        st.cursor = Some(RowKey(c as u64));
        st.anchor = st.cursor;
        st.selection = BTreeSet::from([RowKey(c as u64)]);
    }
    // The toolkit's list still handles the arrow keys of a focused list and moves its cursor
    // again, so what this function decided is put back once the list has run.
    let own = {
        let st = ui.list_state(k);
        (st.cursor, st.anchor, st.selection.clone())
    };

    let first = scroll.first.min(max_top);
    scroll.first = first;
    let shown = n.min(count - first);
    let at = Rect::new(rect.x, rect.y, rect.w, rows_height(shown, row_h));
    let in_view = |key: RowKey| (key.0 as usize).checked_sub(first).filter(|i| *i < shown);
    let opts = ListOptions {
        count: shown,
        heights: Heights::Fixed(row_h),
        selection: SelectionMode::None,
        index_of: Some(&in_view),
        label: "List",
    };
    let mut rows: Vec<(usize, Rect)> = Vec::new();
    let resp = ui.virtual_list(
        k,
        at,
        &opts,
        |i| RowKey((first + i) as u64),
        |ui, ctx| {
            rows.push((first + ctx.index, ctx.rect));
            paint_row(ui, ctx, s, th, &text(first + ctx.index));
            row_h
        },
    );
    {
        let st = ui.list_state(k);
        (st.cursor, st.anchor, st.selection) = own;
    }
    run.at_end = rows.iter().any(|(i, _)| *i + 1 == count);
    run.delete = resp.response.focused
        && ui.input().events.iter().any(|e| {
            matches!(
                e,
                Event::Key {
                    key: Key::Named(NamedKey::Delete),
                    pressed: true,
                    repeat: false,
                    ..
                }
            )
        });
    scroll.focused = resp.response.focused;
    let state = ui.list_state(k);
    state.retain(|key| (key.0 as usize) < count);
    run.selected = state.selection.iter().next().map(|key| key.0 as usize);
    run
}

/// The play queue as a list: `n. artist - title` and the duration, the current item in the
/// current colour. A click selects, a double click plays the queue from that row, Delete removes
/// the selected row from the queue.
#[allow(clippy::too_many_arguments)]
pub(super) fn track_list(
    ui: &mut Ui<'_>,
    view: &mut TrackListView,
    key: (usize, usize),
    (rect, s): (Rect, f32),
    th: &Theme,
    queue: &QueueState,
    out: &mut ListOut,
    rows: Option<(&mut RowScroll, RowsGeom)>,
) {
    let items = &queue.items;
    let current = queue.current;
    let now = ui.time();
    let run = run_list(
        ui,
        (key.0, key.1, 0),
        (rect, s),
        th,
        items.len(),
        &|i| track_text(i + 1, &items[i], current == Some(i)),
        rows,
    );
    for row in run.pressed {
        if view.presses.press(row, now)
            && let Some(action) = play(items, row)
        {
            out.actions.push(action);
        }
    }
    if run.delete
        && let Some(index) = run.selected
    {
        out.actions.push(PlayerAction::RemoveFromQueue { index });
    }
}

/// What the library's top level offers, in order.
const TOP: [(&str, Nav); 3] = [
    ("Liked", Nav::Liked),
    ("Playlists", Nav::Playlists),
    ("Albums", Nav::Albums),
];

/// The rows of a library level: the back row, the items and a trailing note.
enum Body<'a> {
    /// The top level, or the note that stands for it (no provider, logging in, logged out).
    Top(Option<&'static str>),
    Tracks(&'a Slot<Track>),
    Playlists(&'a Slot<Playlist>),
    Albums(&'a Slot<Album>),
}

/// The trailing note of a list that has not (or no longer) got rows to show.
enum Tail {
    None,
    Loading,
    Failed,
    Empty,
}

impl Body<'_> {
    fn len(&self) -> usize {
        match self {
            Body::Top(None) => TOP.len(),
            Body::Top(Some(_)) => 1,
            Body::Tracks(s) => s.items().len(),
            Body::Playlists(s) => s.items().len(),
            Body::Albums(s) => s.items().len(),
        }
    }

    /// The slot's own state, for the trailing note.
    fn tail(&self) -> Tail {
        let (n, loading, failed, more) = match self {
            Body::Top(_) => return Tail::None,
            Body::Tracks(s) => (s.items().len(), s.loading(), s.failed(), s.has_more()),
            Body::Playlists(s) => (s.items().len(), s.loading(), s.failed(), s.has_more()),
            Body::Albums(s) => (s.items().len(), s.loading(), s.failed(), s.has_more()),
        };
        if failed {
            Tail::Failed
        } else if n == 0 && (loading || more) {
            Tail::Loading
        } else if n == 0 {
            Tail::Empty
        } else {
            Tail::None
        }
    }

    fn can_ask(&self) -> bool {
        match self {
            Body::Top(_) => false,
            Body::Tracks(s) => s.has_more() && !s.loading() && !s.failed(),
            Body::Playlists(s) => s.has_more() && !s.loading() && !s.failed(),
            Body::Albums(s) => s.has_more() && !s.loading() && !s.failed(),
        }
    }

    fn serial(&self) -> u64 {
        match self {
            Body::Top(_) => 0,
            Body::Tracks(s) => s.serial(),
            Body::Playlists(s) => s.serial(),
            Body::Albums(s) => s.serial(),
        }
    }
}

/// The library as a list. The top level offers Liked, Playlists and Albums; a level shows its
/// items under a first row `< name` that returns to the top level. A click on an entry opens it,
/// a double click on a track plays the loaded tracks from it.
#[allow(clippy::too_many_arguments)]
pub(super) fn library(
    ui: &mut Ui<'_>,
    view: &mut LibraryView,
    model: &mut LibraryModel,
    access: Access<'_>,
    key: (usize, usize),
    (rect, s): (Rect, f32),
    th: &Theme,
    out: &mut ListOut,
    rows: Option<(&mut RowScroll, RowsGeom)>,
) {
    model.sync(access.provider());
    if model.provider().is_none() || view.nav == Nav::Playlist && model.open_playlist.is_none() {
        view.nav = Nav::Top;
    }
    if view.nav == Nav::Album && model.open_album.is_none() {
        view.nav = Nav::Top;
    }
    let now = ui.time();
    let nav = view.nav;
    let body = match nav {
        Nav::Top => Body::Top(match access {
            Access::NoProvider => Some("No library provider"),
            Access::Checking => Some("Checking your login\u{2026}"),
            Access::LoggedOut => Some("Not logged in"),
            Access::Ready(_) => None,
        }),
        Nav::Liked => Body::Tracks(&model.liked),
        Nav::Playlists => Body::Playlists(&model.playlists),
        Nav::Albums => Body::Albums(&model.albums),
        Nav::Playlist => Body::Tracks(&model.open_playlist.as_ref().expect("checked").tracks),
        Nav::Album => Body::Tracks(&model.open_album.as_ref().expect("checked").tracks),
    };
    let back_label = match nav {
        Nav::Top => None,
        Nav::Liked => Some("Liked".to_owned()),
        Nav::Playlists => Some("Playlists".to_owned()),
        Nav::Albums => Some("Albums".to_owned()),
        Nav::Playlist => model
            .open_playlist
            .as_ref()
            .map(|o| display_text(&o.name, MAX_NAME)),
        Nav::Album => model
            .open_album
            .as_ref()
            .map(|o| display_text(&o.name, MAX_NAME)),
    };
    let back = usize::from(back_label.is_some());
    let tail = body.tail();
    let items = body.len();
    let count = back + items + usize::from(!matches!(tail, Tail::None));
    let text = |i: usize| -> RowText {
        if let Some(label) = &back_label
            && i == 0
        {
            return RowText::plain(format!("< {label}"));
        }
        let j = i - back;
        if j >= items {
            return RowText::plain(match tail {
                Tail::Failed => LOAD_FAILED,
                Tail::Loading => "Loading\u{2026}",
                _ => "Nothing here.",
            });
        }
        match &body {
            Body::Top(Some(note)) => RowText::plain(*note),
            Body::Top(None) => RowText::plain(TOP[j].0),
            Body::Tracks(slot) => track_text(j + 1, &slot.items()[j], false),
            Body::Playlists(slot) => RowText::plain(display_text(&slot.items()[j].name, MAX_NAME)),
            Body::Albums(slot) => RowText::plain(display_text(&slot.items()[j].name, MAX_NAME)),
        }
    };
    let run = run_list(
        ui,
        (key.0, key.1, body.serial()),
        (rect, s),
        th,
        count,
        &text,
        rows,
    );
    let kind = nav.kind();
    if let Some(kind) = kind
        && body.can_ask()
        && (run.at_end || items == 0)
    {
        out.more.push(kind);
    }

    // What the presses do (the body's borrow of the model ends here).
    enum Go {
        Nav(Nav),
        Play(usize),
        Retry,
        Playlist(usize),
        Album(usize),
    }
    let mut gos = Vec::new();
    for row in run.pressed {
        let go = if back == 1 && row == 0 {
            Go::Nav(Nav::Top)
        } else {
            let j = row - back;
            match &body {
                _ if j >= items && matches!(tail, Tail::Failed) => Go::Retry,
                _ if j >= items => Go::Nav(nav),
                Body::Top(Some(_)) => Go::Nav(Nav::Top),
                Body::Top(None) => Go::Nav(TOP[j].1),
                Body::Tracks(_) if view.presses.press(row, now) => Go::Play(j),
                Body::Tracks(_) => Go::Nav(nav),
                Body::Playlists(_) => Go::Playlist(j),
                Body::Albums(_) => Go::Album(j),
            }
        };
        gos.push(go);
    }
    let tracks = match &body {
        Body::Tracks(slot) => Some(slot.items().to_vec()),
        _ => None,
    };
    let opened_playlist = match &body {
        Body::Playlists(slot) => Some(slot.items().to_vec()),
        _ => None,
    };
    let opened_album = match &body {
        Body::Albums(slot) => Some(slot.items().to_vec()),
        _ => None,
    };
    for go in gos {
        match go {
            Go::Nav(n) => view.nav = n,
            Go::Play(j) => {
                if let Some(action) = tracks.as_deref().and_then(|t| play(t, j)) {
                    out.actions.push(action);
                }
            }
            Go::Retry => {
                if let Some(kind) = kind {
                    out.retry.push(kind);
                }
            }
            Go::Playlist(j) => {
                if let Some(p) = opened_playlist.as_deref().and_then(|l| l.get(j)) {
                    model.open_playlist(&p.id, &p.name);
                    view.nav = Nav::Playlist;
                }
            }
            Go::Album(j) => {
                if let Some(a) = opened_album.as_deref().and_then(|l| l.get(j)) {
                    model.open_album(&a.id, &a.name, &a.images, a.year);
                    view.nav = Nav::Album;
                }
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use lune_ui::testing::UiHarness;
    use whipspot_core::provider::Readable;

    use super::*;

    fn track(n: u32) -> Track {
        Track {
            uri: format!("t:{n}"),
            name: format!("t{n}"),
            artist: "a".into(),
            album: "b".into(),
            duration_ms: 61_000,
            images: vec![],
            year: None,
            album_ref: None,
            artists: vec![],
        }
    }

    fn queue(n: u32) -> QueueState {
        QueueState {
            items: Arc::new((0..n).map(track).collect()),
            current: Some(2),
            next: None,
        }
    }

    fn theme() -> Theme {
        list_theme(&SkinTheme::default())
    }

    fn rect() -> Rect {
        Rect::new(0.0, 0.0, 243.0, 58.0)
    }

    /// The y of the middle of row `i`.
    fn row_y(i: usize) -> f32 {
        i as f32 * ROW + ROW / 2.0
    }

    fn click(h: &mut UiHarness, row: usize) {
        h.input.pointer_to(10.0, row_y(row));
        h.input.click(PointerButton::Primary);
    }

    fn queue_frame(
        h: &mut UiHarness,
        ms: u64,
        view: &mut TrackListView,
        q: &QueueState,
    ) -> ListOut {
        h.at_ms(ms);
        let mut out = ListOut::default();
        let th = theme();
        h.frame(|ui| track_list(ui, view, (0, 0), (rect(), 1.0), &th, q, &mut out, None));
        out
    }

    #[test]
    fn a_double_click_on_row_2_plays_the_queue_from_index_2() {
        let mut h = UiHarness::new((300.0, 100.0));
        let mut view = TrackListView::default();
        let q = queue(8);
        assert!(queue_frame(&mut h, 0, &mut view, &q).actions.is_empty());
        click(&mut h, 2);
        assert!(queue_frame(&mut h, 1000, &mut view, &q).actions.is_empty());
        click(&mut h, 2);
        let out = queue_frame(&mut h, 1100, &mut view, &q);
        let [PlayerAction::PlayItem { context, index }] = out.actions.as_slice() else {
            panic!("expected one PlayItem");
        };
        assert_eq!(*index, 2);
        assert_eq!(context.len(), 8);
        assert_eq!(context[2].uri, "t:2");
    }

    #[test]
    fn two_presses_too_far_apart_or_on_two_rows_are_no_double_click() {
        let mut h = UiHarness::new((300.0, 100.0));
        let mut view = TrackListView::default();
        let q = queue(8);
        queue_frame(&mut h, 0, &mut view, &q);
        click(&mut h, 1);
        queue_frame(&mut h, 1000, &mut view, &q);
        click(&mut h, 1);
        assert!(queue_frame(&mut h, 1500, &mut view, &q).actions.is_empty());
        click(&mut h, 3);
        assert!(queue_frame(&mut h, 1600, &mut view, &q).actions.is_empty());
    }

    #[test]
    fn delete_on_the_selection_removes_that_queue_item() {
        let mut h = UiHarness::new((300.0, 100.0));
        let mut view = TrackListView::default();
        let q = queue(8);
        queue_frame(&mut h, 0, &mut view, &q);
        click(&mut h, 1);
        assert!(queue_frame(&mut h, 1000, &mut view, &q).actions.is_empty());
        h.input.key_down(Key::Named(NamedKey::Delete), None);
        let out = queue_frame(&mut h, 2000, &mut view, &q);
        let [PlayerAction::RemoveFromQueue { index }] = out.actions.as_slice() else {
            panic!("expected one RemoveFromQueue");
        };
        assert_eq!(*index, 1);
    }

    #[test]
    fn delete_with_nothing_selected_does_nothing() {
        let mut h = UiHarness::new((300.0, 100.0));
        let mut view = TrackListView::default();
        let q = queue(4);
        queue_frame(&mut h, 0, &mut view, &q);
        h.input.key_down(Key::Named(NamedKey::Delete), None);
        assert!(queue_frame(&mut h, 100, &mut view, &q).actions.is_empty());
    }

    fn ready_model() -> LibraryModel {
        let mut m = LibraryModel::default();
        m.sync(Some("p"));
        m
    }

    fn lib_frame(
        h: &mut UiHarness,
        ms: u64,
        view: &mut LibraryView,
        model: &mut LibraryModel,
    ) -> ListOut {
        lib_frame_as(h, ms, view, model, Access::Ready("p"))
    }

    fn lib_frame_as(
        h: &mut UiHarness,
        ms: u64,
        view: &mut LibraryView,
        model: &mut LibraryModel,
        access: Access<'_>,
    ) -> ListOut {
        h.at_ms(ms);
        let mut out = ListOut::default();
        let th = theme();
        h.frame(|ui| {
            library(
                ui,
                view,
                model,
                access,
                (0, 0),
                (rect(), 1.0),
                &th,
                &mut out,
                None,
            );
        });
        out
    }

    #[test]
    fn the_top_level_opens_a_list_and_the_back_row_returns_without_a_request() {
        let mut h = UiHarness::new((300.0, 100.0));
        let mut view = LibraryView::default();
        let mut model = ready_model();
        model.liked = Slot::fixed((0..4).map(track).collect(), 9);
        lib_frame(&mut h, 0, &mut view, &mut model);
        // The top level: Liked, Playlists, Albums. Row 0 opens Liked.
        click(&mut h, 0);
        lib_frame(&mut h, 100, &mut view, &mut model);
        assert_eq!(view.nav, Nav::Liked);
        lib_frame(&mut h, 200, &mut view, &mut model);
        // Row 0 is now the back row.
        click(&mut h, 0);
        let out = lib_frame(&mut h, 300, &mut view, &mut model);
        assert_eq!(view.nav, Nav::Top);
        assert!(out.actions.is_empty() && out.more.is_empty() && out.retry.is_empty());
    }

    #[test]
    fn the_top_level_rows_follow_liked_playlists_albums() {
        let mut h = UiHarness::new((300.0, 100.0));
        let mut model = ready_model();
        for (row, nav) in [(0, Nav::Liked), (1, Nav::Playlists), (2, Nav::Albums)] {
            let mut view = LibraryView::default();
            lib_frame(&mut h, 0, &mut view, &mut model);
            click(&mut h, row);
            lib_frame(&mut h, 100, &mut view, &mut model);
            assert_eq!(view.nav, nav, "row {row}");
        }
    }

    #[test]
    fn an_opened_list_asks_for_its_first_page_once() {
        let mut h = UiHarness::new((300.0, 100.0));
        let mut view = LibraryView {
            nav: Nav::Liked,
            ..LibraryView::default()
        };
        let mut model = ready_model();
        let out = lib_frame(&mut h, 0, &mut view, &mut model);
        assert_eq!(out.more, [ListKind::Liked]);
        // The caller sends the request; while it is pending the list asks for nothing.
        assert!(model.next_page(ListKind::Liked).is_some());
        assert!(
            lib_frame(&mut h, 100, &mut view, &mut model)
                .more
                .is_empty()
        );
    }

    #[test]
    fn a_double_click_on_a_library_track_plays_the_loaded_tracks() {
        let mut h = UiHarness::new((300.0, 100.0));
        let mut view = LibraryView {
            nav: Nav::Liked,
            ..LibraryView::default()
        };
        let mut model = ready_model();
        model.liked = Slot::fixed((0..5).map(track).collect(), 9);
        lib_frame(&mut h, 0, &mut view, &mut model);
        // Row 0 is the back row, so row 3 is track 2.
        click(&mut h, 3);
        assert!(
            lib_frame(&mut h, 1000, &mut view, &mut model)
                .actions
                .is_empty()
        );
        click(&mut h, 3);
        let out = lib_frame(&mut h, 1100, &mut view, &mut model);
        let [PlayerAction::PlayItem { context, index }] = out.actions.as_slice() else {
            panic!("expected one PlayItem");
        };
        assert_eq!((*index, context.len()), (2, 5));
    }

    #[test]
    fn a_playlist_opens_and_a_logout_puts_the_view_back_at_the_top() {
        let mut h = UiHarness::new((300.0, 100.0));
        let mut view = LibraryView {
            nav: Nav::Playlists,
            ..LibraryView::default()
        };
        let mut model = ready_model();
        model.playlists = Slot::fixed(
            vec![Playlist {
                id: "pl".into(),
                name: "Mix".into(),
                readable: Readable::Readable,
                track_count: None,
                images: vec![],
            }],
            5,
        );
        lib_frame(&mut h, 0, &mut view, &mut model);
        click(&mut h, 1);
        lib_frame(&mut h, 100, &mut view, &mut model);
        assert_eq!(view.nav, Nav::Playlist);
        assert_eq!(
            model.open_playlist.as_ref().map(|o| o.id.as_str()),
            Some("pl")
        );
        lib_frame_as(&mut h, 200, &mut view, &mut model, Access::LoggedOut);
        assert_eq!(view.nav, Nav::Top);
    }

    #[test]
    fn the_list_theme_keeps_the_dark_preset_where_the_skin_is_silent() {
        let t = list_theme(&SkinTheme::default());
        assert_eq!(t.fg_primary, DARK.fg_primary);
        assert_eq!(t.fg_bright, DARK.fg_bright);
        assert_eq!(t.surface_selected, DARK.surface_selected);
        assert_eq!(t.surface_input[3], 255);
        let skin = SkinTheme {
            list_text: Some(Rgb { r: 1, g: 2, b: 3 }),
            list_text_current: Some(Rgb { r: 4, g: 5, b: 6 }),
            list_background: Some(Rgb { r: 7, g: 8, b: 9 }),
            list_selection: Some(Rgb {
                r: 10,
                g: 11,
                b: 12,
            }),
            list_font: Some("Arial".into()),
        };
        let t = list_theme(&skin);
        assert_eq!(t.fg_primary, [1, 2, 3, 255]);
        assert_eq!(t.fg_bright, [4, 5, 6, 255]);
        assert_eq!(t.surface_input, [7, 8, 9, 255]);
        assert_eq!(t.surface_selected, [10, 11, 12, 255]);
    }

    #[test]
    fn a_row_reads_artist_and_title_with_the_duration() {
        let t = track_text(3, &track(1), true);
        assert_eq!(t.left, "3. a - t1");
        assert_eq!(t.right.as_deref(), Some("1:01"));
        assert!(t.current);
    }

    // The rows mode: a 44 px list (4 rows of 11) with an 8 px wide bar beside it and an 18 px
    // thumb, at s = 1.

    fn rows_rect() -> Rect {
        Rect::new(0.0, 0.0, 200.0, 44.0)
    }

    fn rows_geom() -> RowsGeom {
        RowsGeom {
            bar: SkinRect {
                x: 200,
                y: 0,
                w: 8,
                h: 44,
            },
            thumb_h: 18,
            list_h: 44,
        }
    }

    fn rows_frame(
        h: &mut UiHarness,
        ms: u64,
        view: &mut TrackListView,
        scroll: &mut RowScroll,
        q: &QueueState,
    ) -> ListOut {
        h.at_ms(ms);
        let mut out = ListOut::default();
        let th = theme();
        h.frame(|ui| {
            track_list(
                ui,
                view,
                (0, 0),
                (rows_rect(), 1.0),
                &th,
                q,
                &mut out,
                Some((scroll, rows_geom())),
            );
        });
        out
    }

    /// A list of 12 rows with the first row selected and focused, after three frames (the list
    /// learns of its focus a frame after the click).
    fn focused_rows() -> (UiHarness, TrackListView, RowScroll, QueueState) {
        let mut h = UiHarness::new((300.0, 100.0));
        let (mut view, mut scroll, q) = (TrackListView::default(), RowScroll::default(), queue(12));
        rows_frame(&mut h, 0, &mut view, &mut scroll, &q);
        click(&mut h, 0);
        rows_frame(&mut h, 100, &mut view, &mut scroll, &q);
        rows_frame(&mut h, 150, &mut view, &mut scroll, &q);
        (h, view, scroll, q)
    }

    #[test]
    fn the_thumb_is_at_the_top_at_the_bottom_after_end_and_mid_after_a_drag() {
        let (mut h, mut view, mut sc, q) = focused_rows();
        assert_eq!((sc.count, sc.n, sc.first), (12, 4, 0));
        assert_eq!(sc.fraction(), 0.0);
        assert_eq!(thumb_y(0, 44, 18, sc.fraction()), 0);
        h.input.key_down(Key::Named(NamedKey::End), None);
        rows_frame(&mut h, 200, &mut view, &mut sc, &q);
        assert_eq!(sc.first, 8);
        assert_eq!(sc.fraction(), 1.0);
        assert_eq!(thumb_y(0, 44, 18, sc.fraction()), 26);
        // Home goes back.
        h.input.key_down(Key::Named(NamedKey::Home), None);
        rows_frame(&mut h, 300, &mut view, &mut sc, &q);
        assert_eq!(sc.first, 0);
        // A press on the bar below the thumb centres the thumb there: y 22 with an 18 px thumb
        // is the middle of the 26 px of travel.
        sc.op(ScrollOp::Press(22.0));
        assert_eq!(sc.first, 4);
        assert_eq!(sc.fraction(), 0.5);
        // Dragging keeps the grab (9 px from the thumb top) and clamps at both ends.
        sc.op(ScrollOp::Move(60.0));
        assert_eq!(sc.first, 8);
        sc.op(ScrollOp::Move(-30.0));
        assert_eq!(sc.first, 0);
        // Let go: a later move does nothing.
        sc.op(ScrollOp::Release);
        sc.op(ScrollOp::Move(40.0));
        assert_eq!(sc.first, 0);
    }

    #[test]
    fn a_press_on_the_thumb_keeps_where_it_was_grabbed() {
        let (_h, _view, mut sc, _q) = focused_rows();
        sc.first = 4;
        // The thumb is at y 13..31; grabbed at its top edge (y 13): the pointer moving to 20
        // moves the thumb top to 20 (f = 20 / 26 of 8 rows, so row 6).
        sc.op(ScrollOp::Press(13.0));
        assert_eq!(sc.first, 4);
        sc.op(ScrollOp::Move(20.0));
        assert_eq!(sc.first, 6);
    }

    #[test]
    fn the_wheel_scrolls_a_row_per_line_toward_row_zero_for_wheel_up() {
        let (mut h, mut view, mut sc, q) = focused_rows();
        h.input.pointer_to(10.0, 10.0);
        h.input.scroll_lines(0.0, -2.0);
        rows_frame(&mut h, 200, &mut view, &mut sc, &q);
        assert_eq!(sc.first, 2);
        // Over the bar too.
        h.input.pointer_to(204.0, 10.0);
        h.input.scroll_lines(0.0, 1.0);
        rows_frame(&mut h, 300, &mut view, &mut sc, &q);
        assert_eq!(sc.first, 1);
        // Clamped at the end and at the top.
        h.input.pointer_to(10.0, 10.0);
        h.input.scroll_lines(0.0, -50.0);
        rows_frame(&mut h, 400, &mut view, &mut sc, &q);
        assert_eq!(sc.first, 8);
        h.input.scroll_lines(0.0, 50.0);
        rows_frame(&mut h, 500, &mut view, &mut sc, &q);
        assert_eq!(sc.first, 0);
        // Elsewhere nothing scrolls.
        h.input.pointer_to(250.0, 80.0);
        h.input.scroll_lines(0.0, -3.0);
        rows_frame(&mut h, 600, &mut view, &mut sc, &q);
        assert_eq!(sc.first, 0);
    }

    #[test]
    fn touchpad_pixels_add_up_to_a_row_every_row_height() {
        let (mut h, mut view, mut sc, q) = focused_rows();
        h.input.pointer_to(10.0, 10.0);
        // 11 px is a row: 6 + 6 px is one row after the second.
        h.input.scroll_pixels(0.0, -6.0);
        rows_frame(&mut h, 200, &mut view, &mut sc, &q);
        assert_eq!(sc.first, 0);
        h.input.scroll_pixels(0.0, -6.0);
        rows_frame(&mut h, 300, &mut view, &mut sc, &q);
        assert_eq!(sc.first, 1);
    }

    #[test]
    fn a_queue_that_shrinks_puts_the_list_back_and_the_thumb_goes_inert() {
        let (mut h, mut view, mut sc, q) = focused_rows();
        sc.first = 8;
        rows_frame(&mut h, 200, &mut view, &mut sc, &q);
        assert_eq!(sc.first, 8);
        rows_frame(&mut h, 300, &mut view, &mut sc, &queue(3));
        assert_eq!(sc.first, 0);
        assert_eq!(sc.fraction(), 0.0);
        // Everything fits: the thumb stays at the top whatever is done to it.
        sc.op(ScrollOp::Press(30.0));
        sc.op(ScrollOp::Move(40.0));
        assert_eq!((sc.first, sc.fraction()), (0, 0.0));
    }

    #[test]
    fn a_double_click_on_a_visible_row_plays_the_absolute_queue_row() {
        let (mut h, mut view, mut sc, q) = focused_rows();
        sc.first = 5;
        // Visible row 1 is queue row 6.
        click(&mut h, 1);
        assert!(
            rows_frame(&mut h, 1000, &mut view, &mut sc, &q)
                .actions
                .is_empty()
        );
        click(&mut h, 1);
        let out = rows_frame(&mut h, 1100, &mut view, &mut sc, &q);
        let [PlayerAction::PlayItem { context, index }] = out.actions.as_slice() else {
            panic!("expected one PlayItem");
        };
        assert_eq!((*index, context.len()), (6, 12));
        assert_eq!(sc.first, 5, "a click does not scroll");
    }

    #[test]
    fn delete_removes_the_selected_absolute_row() {
        let (mut h, mut view, mut sc, q) = focused_rows();
        sc.first = 5;
        click(&mut h, 2);
        assert!(
            rows_frame(&mut h, 1000, &mut view, &mut sc, &q)
                .actions
                .is_empty()
        );
        h.input.key_down(Key::Named(NamedKey::Delete), None);
        let out = rows_frame(&mut h, 2000, &mut view, &mut sc, &q);
        let [PlayerAction::RemoveFromQueue { index }] = out.actions.as_slice() else {
            panic!("expected one RemoveFromQueue");
        };
        assert_eq!(*index, 7);
    }

    #[test]
    fn the_arrow_keys_scroll_one_row_at_the_edge_and_move_the_selection() {
        let (mut h, mut view, mut sc, q) = focused_rows();
        // Row 3 is the last in view; Down scrolls one row and selects queue row 4.
        click(&mut h, 3);
        rows_frame(&mut h, 200, &mut view, &mut sc, &q);
        h.input.key_down(Key::Named(NamedKey::ArrowDown), None);
        rows_frame(&mut h, 300, &mut view, &mut sc, &q);
        assert_eq!(sc.first, 1);
        // Up inside the view moves the selection and keeps the top.
        h.input.key_down(Key::Named(NamedKey::ArrowUp), None);
        rows_frame(&mut h, 400, &mut view, &mut sc, &q);
        assert_eq!(sc.first, 1);
        h.input.key_down(Key::Named(NamedKey::ArrowUp), None);
        h.input.key_down(Key::Named(NamedKey::ArrowUp), None);
        h.input.key_down(Key::Named(NamedKey::ArrowUp), None);
        rows_frame(&mut h, 500, &mut view, &mut sc, &q);
        // The cursor went 4 -> 3 -> 2 -> 1 -> 0 and the top followed it up.
        assert_eq!(sc.first, 0);
        h.input.key_down(Key::Named(NamedKey::Delete), None);
        let out = rows_frame(&mut h, 600, &mut view, &mut sc, &q);
        let [PlayerAction::RemoveFromQueue { index }] = out.actions.as_slice() else {
            panic!("expected one RemoveFromQueue");
        };
        assert_eq!(*index, 0);
    }

    #[test]
    fn one_arrow_key_inside_the_view_moves_one_row() {
        // Down, Down from row 0 selects row 2; Delete removes it (not row 3).
        let (mut h, mut view, mut sc, q) = focused_rows();
        h.input.key_down(Key::Named(NamedKey::ArrowDown), None);
        rows_frame(&mut h, 200, &mut view, &mut sc, &q);
        h.input.key_down(Key::Named(NamedKey::ArrowDown), None);
        rows_frame(&mut h, 300, &mut view, &mut sc, &q);
        assert_eq!(sc.first, 0);
        h.input.key_down(Key::Named(NamedKey::Delete), None);
        let out = rows_frame(&mut h, 400, &mut view, &mut sc, &q);
        let [PlayerAction::RemoveFromQueue { index }] = out.actions.as_slice() else {
            panic!("expected one RemoveFromQueue");
        };
        assert_eq!(*index, 2);

        // Up, Up from row 3 selects row 1.
        let (mut h, mut view, mut sc, q) = focused_rows();
        click(&mut h, 3);
        rows_frame(&mut h, 200, &mut view, &mut sc, &q);
        h.input.key_down(Key::Named(NamedKey::ArrowUp), None);
        rows_frame(&mut h, 300, &mut view, &mut sc, &q);
        h.input.key_down(Key::Named(NamedKey::ArrowUp), None);
        rows_frame(&mut h, 400, &mut view, &mut sc, &q);
        h.input.key_down(Key::Named(NamedKey::Delete), None);
        let out = rows_frame(&mut h, 500, &mut view, &mut sc, &q);
        let [PlayerAction::RemoveFromQueue { index }] = out.actions.as_slice() else {
            panic!("expected one RemoveFromQueue");
        };
        assert_eq!(*index, 1);
    }

    #[test]
    fn page_keys_move_by_the_rows_in_view() {
        let (mut h, mut view, mut sc, q) = focused_rows();
        h.input.key_down(Key::Named(NamedKey::PageDown), None);
        rows_frame(&mut h, 200, &mut view, &mut sc, &q);
        // The cursor went 0 -> 4, and the top just far enough to show it.
        assert_eq!(sc.first, 1);
        h.input.key_down(Key::Named(NamedKey::PageDown), None);
        rows_frame(&mut h, 300, &mut view, &mut sc, &q);
        assert_eq!(sc.first, 5);
        h.input.key_down(Key::Named(NamedKey::PageUp), None);
        rows_frame(&mut h, 400, &mut view, &mut sc, &q);
        assert_eq!(sc.first, 4);
        h.input.key_down(Key::Named(NamedKey::End), None);
        rows_frame(&mut h, 500, &mut view, &mut sc, &q);
        assert_eq!(sc.first, 8);
        h.input.key_down(Key::Named(NamedKey::PageDown), None);
        rows_frame(&mut h, 600, &mut view, &mut sc, &q);
        assert_eq!(sc.first, 8, "past the last row nothing moves");
    }

    #[test]
    fn a_list_with_a_new_identity_starts_at_the_top() {
        let (mut h, mut view, mut sc, q) = focused_rows();
        sc.first = 6;
        sc.serial = 99;
        rows_frame(&mut h, 200, &mut view, &mut sc, &q);
        assert_eq!(sc.first, 0);
    }

    #[test]
    fn rows_fill_the_list_so_the_toolkit_never_draws_a_bar() {
        // Its content is `shown * row_h` in f64; the rect must be at least that tall.
        for s in [1.0f32, 4.0 / 3.0, 1.5, 2.0, 8.0 / 3.0, 3.0] {
            let row_h = ROW * s;
            for shown in 0..400 {
                let h = rows_height(shown, row_h);
                let total = shown as f64 * f64::from(row_h);
                assert!(f64::from(h) >= total, "{shown} rows at {s}");
                assert!(f64::from(h) - total < 0.01);
            }
        }
    }

    #[test]
    fn the_thumb_rests_on_the_rounded_share_of_the_travel() {
        assert_eq!(thumb_y(10, 100, 20, 0.0), 10);
        assert_eq!(thumb_y(10, 100, 20, 1.0), 90);
        assert_eq!(thumb_y(10, 100, 20, 0.5), 50);
        assert_eq!(thumb_y(10, 100, 20, 0.0625), 15);
        // A thumb as tall as the bar has no travel.
        assert_eq!(thumb_y(10, 20, 20, 1.0), 10);
        assert_eq!(thumb_y(10, 10, 20, 1.0), 10);
    }

    #[test]
    fn a_list_without_a_scroll_bar_keeps_the_toolkit_scrolling() {
        let mut h = UiHarness::new((300.0, 100.0));
        let mut lists = Lists::default();
        let q = queue(30);
        let th = theme();
        for ms in [0, 100] {
            h.at_ms(ms);
            h.frame(|ui| lists.track_list(ui, (0, 0), (rect(), 1.0), &th, &q, None));
        }
        // No scroll position was made for the panel.
        assert!(lists.scrolls.0.is_empty());
        assert_eq!(lists.scroll_fraction(0), 0.0);
        // The old double click still plays the clicked row.
        click(&mut h, 2);
        h.at_ms(1000);
        h.frame(|ui| lists.track_list(ui, (0, 0), (rect(), 1.0), &th, &q, None));
        click(&mut h, 2);
        h.at_ms(1100);
        h.frame(|ui| lists.track_list(ui, (0, 0), (rect(), 1.0), &th, &q, None));
        let [PlayerAction::PlayItem { index, .. }] = lists.out.actions.as_slice() else {
            panic!("expected one PlayItem");
        };
        assert_eq!(*index, 2);
    }
}
