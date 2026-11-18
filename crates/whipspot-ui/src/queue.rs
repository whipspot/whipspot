//! The queue pane (MD82, MD86): the player's queue as an editable track list. Rows can be removed
//! (a button on the hovered row), moved (the hovered row's grip, or a menu on a secondary click)
//! and the header has Shuffle, Repeat and Clear queue.
//!
//! Every edit names list indices of the snapshot drawn, and the core applies it to whatever the
//! queue is by then. So the pane sends one edit and then waits for a new snapshot (or one second)
//! before it takes another gesture (`edit_blocked`), and it drops a drag or a menu whose snapshot
//! has been replaced. With a filter set the grip, the drag and the menu's Move items are off; a
//! row's remove still names its list index, and a click on a row plays the whole queue from it.

use std::sync::Arc;
use std::time::Duration;

use lune_ui::input::{CursorIcon, Event, Key, NamedKey, PointerButton};
use lune_ui::paint::Rect;
use lune_ui::scene::LayerId;
use lune_ui::ui::{Sense, Ui};
use lune_ui::widgets::menu::{MenuItem, MenuRow};
use whipspot_core::player::{PlayerAction, PlayerHandle, PlayerState, RepeatMode};
use whipspot_core::provider::Track;

use crate::browse::{Feed, Show};
use crate::draw::{disc, led, text, text_c, width};
use crate::grid::{self, Covers};
use crate::history::Route;
use crate::icons::{Icon, icon};
use crate::layout;
use crate::library::Slot;
use crate::likes::LikeBook;
use crate::pages::{self, FilterCache};
use crate::theme::{Glyphs, Indicator, Look};
use crate::tracks::{Edit, Form, TrackTable, play_from, slot_at};

/// After an edit the pane takes no other until the queue's snapshot is new or this long passed.
const EDIT_HOLD: Duration = Duration::from_secs(1);
/// "Clear queue" asks for a second click within this long.
const CLEAR_ARM: Duration = Duration::from_secs(3);
/// A grip press becomes a drag once the pointer is this far (px) from where it landed.
const DRAG_START: f32 = 4.0;
/// The list's scroll position is kept under this key for the pane's life.
const LIST_KEY: u64 = 0;
/// The header buttons: the hit square's half side, their pitch, the gap to Clear queue and the
/// longest Clear queue label (the buttons sit left of it whatever it reads).
const BUTTON_HALF: f32 = 14.0;
const BUTTON_PITCH: f32 = 36.0;
const CLEAR_GAP: f32 = 24.0;
const CLEAR_LABEL: &str = "Clear queue";
const CLEAR_ARMED_LABEL: &str = "Click again to clear";
const SHUFFLED_NOTE: &str = "Plays in shuffled order; the next song is marked.";
const HAIR: f32 = 1.5;

/// A queue drag the demo forces (the dumps only): the row picked up, and the slot it would drop
/// into (the line is drawn above that list index).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueDrag {
    pub from: usize,
    pub to: usize,
}

/// What the pane reports of a frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct QueueOut {
    /// The filter bar had keyboard focus.
    pub(crate) filter_focused: bool,
}

/// The index row `from` takes when it is dropped in `slot` (the line above list index `slot`,
/// `len` for below the last): the slot counts the row itself while it is still in the list.
pub(crate) fn final_index(from: usize, slot: usize) -> usize {
    if slot <= from { slot } else { slot - 1 }
}

/// What the menu's Move items do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Move {
    Up,
    Down,
    Top,
    Bottom,
}

/// The index `item` takes row `from` of `len` to; `None` when it would stay where it is.
pub(crate) fn menu_move(from: usize, item: Move, len: usize) -> Option<usize> {
    let to = match item {
        Move::Up => from.checked_sub(1)?,
        Move::Down => from + 1,
        Move::Top => 0,
        Move::Bottom => len.checked_sub(1)?,
    };
    (to < len && to != from).then_some(to)
}

/// Whether an edit gesture is ignored: the last edit `sent` (the snapshot it acted on and when)
/// has neither been answered by a new snapshot nor outlived [`EDIT_HOLD`].
pub(crate) fn edit_blocked<T>(
    sent: Option<(&Arc<T>, Duration)>,
    items: &Arc<T>,
    now: Duration,
) -> bool {
    sent.is_some_and(|(arc, at)| Arc::ptr_eq(arc, items) && now.saturating_sub(at) < EDIT_HOLD)
}

/// The last edit sent: the snapshot it acted on and when.
type Flight = Option<(Arc<Vec<Track>>, Duration)>;

/// Sends `action` unless an edit is still in flight; whether it was sent.
fn send(
    flight: &mut Flight,
    items: &Arc<Vec<Track>>,
    now: Duration,
    player: &PlayerHandle,
    action: PlayerAction,
) -> bool {
    if edit_blocked(flight.as_ref().map(|(a, t)| (a, *t)), items, now) {
        return false;
    }
    player.dispatch(action);
    *flight = Some((Arc::clone(items), now));
    true
}

/// The queue's items as a [`Slot`], rebuilt only when the player's snapshot changes.
#[derive(Debug, Default)]
struct QueueCopy {
    from: Option<Arc<Vec<Track>>>,
    slot: Option<Slot<Track>>,
}

/// A grip press and what became of it.
#[derive(Debug)]
struct Grab {
    from: usize,
    at: (f32, f32),
    /// The pointer has left the press point by [`DRAG_START`].
    moving: bool,
    /// The snapshot `from` names.
    items: Arc<Vec<Track>>,
}

/// The row menu: the list index it is for, where it opened and the snapshot that index names.
#[derive(Debug)]
struct RowMenu {
    index: usize,
    at: (f32, f32),
    items: Arc<Vec<Track>>,
}

/// The pane's retained state.
#[derive(Debug)]
pub(crate) struct QueuePane {
    copy: QueueCopy,
    /// The serial of the copy on show. Queue serials count down from `u64::MAX`, so they never
    /// meet a library request id (the lists' keys and the like feed go by serial). It names the
    /// copy for the filter's cache and the like feed; the list's scroll uses [`LIST_KEY`].
    serial: u64,
    feed: Feed,
    table: TrackTable,
    visible: FilterCache,
    flight: Flight,
    grab: Option<Grab>,
    menu: Option<RowMenu>,
    /// When Clear queue was first clicked.
    clear_armed: Option<Duration>,
}

impl Default for QueuePane {
    fn default() -> Self {
        QueuePane {
            copy: QueueCopy::default(),
            serial: u64::MAX,
            feed: Feed::default(),
            table: TrackTable::default(),
            visible: FilterCache::default(),
            flight: None,
            grab: None,
            menu: None,
            clear_armed: None,
        }
    }
}

/// `lit` when hovered, else `base` (the media control's lift).
fn lift(k: &Look, base: lune_ui::color::Rgba, hovered: bool) -> lune_ui::color::Rgba {
    if hovered { k.ink.text } else { base }
}

impl QueuePane {
    /// Draws the page: its header, the filter bar over `filter` and the table, in the sheet
    /// `sheet` with content rect `c`. `hover_row` draws that row as hovered and `drag` a drag
    /// preview, whatever the pointer says (the demo's dumps only).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn frame(
        &mut self,
        ui: &mut Ui<'_>,
        (sheet, c): (Rect, Rect),
        on: &Show<'_>,
        covers: &mut Covers<'_>,
        likes: &mut LikeBook,
        filter: &mut String,
        hover_row: Option<usize>,
        drag: Option<QueueDrag>,
    ) -> QueueOut {
        let Show {
            look,
            state,
            player,
            hearts,
        } = *on;
        let same = self
            .copy
            .from
            .as_ref()
            .is_some_and(|a| Arc::ptr_eq(a, &state.queue.items));
        if !same {
            self.serial -= 1;
            self.copy.from = Some(Arc::clone(&state.queue.items));
            self.copy.slot = Some(Slot::fixed(state.queue.items.to_vec(), self.serial));
        }
        let Some(slot) = self.copy.slot.as_ref() else {
            return QueueOut::default();
        };
        ui.painter().on(LayerId::Popup);

        let items = &state.queue.items;
        let now = ui.time();
        let filtered = !filter.trim().is_empty();
        let n = slot.items().len();
        let sub = (n > 0).then(|| {
            if state.shuffle {
                format!("{n} in queue \u{b7} {SHUFFLED_NOTE}")
            } else {
                format!("{n} in queue")
            }
        });
        grid::header(ui.painter(), look, c, Route::Queue.title(), sub.as_deref());
        if hearts.is_some() {
            self.feed.step(slot, false, likes, player);
        }

        let geo = pages::geometry(sheet.w + 2.0 * layout::SHEET_X, c, false);
        let shown = n > 0;
        let filter_focused = shown && pages::filter_bar(ui, geo.filter, filter);
        controls(
            ui,
            look,
            state,
            player,
            Rect::new(geo.filter.x, geo.filter.y, geo.table.w, geo.filter.h),
            (shown, &mut self.clear_armed),
            (&mut self.flight, items),
        );

        // A drag in progress, from the rows of the last frame (the table has none yet).
        advance_grab(
            (&mut self.grab, &mut self.flight),
            &self.table,
            ui,
            items,
            player,
            filtered,
        );
        let (dragging, drop) = match (drag, self.grab.as_ref().filter(|g| g.moving)) {
            (Some(d), _) => (Some(d.from), Some(d.to)),
            (None, Some(g)) => {
                let over = ui
                    .pointer_pos()
                    .filter(|p| self.table.list_rect().contains(*p));
                (
                    Some(g.from),
                    over.and_then(|p| slot_at(self.table.rows(), p.1)),
                )
            }
            (None, None) => (None, None),
        };
        if dragging.is_some() {
            ui.set_cursor(CursorIcon::Grabbing);
        }
        self.table.set_edit(Edit {
            key: LIST_KEY,
            current: state.queue.current,
            next: state.queue.next,
            grip: !filtered,
            hover: hover_row,
            dragging,
            drop,
        });

        let visible = self
            .visible
            .get(&Route::Queue, filter, slot.serial(), slot.items());
        let result = self.table.frame(
            ui,
            geo.table,
            look,
            Form {
                album_page: false,
                ..Form::default()
            },
            slot,
            visible,
            "The queue is empty.",
            None,
            covers,
            likes,
            player,
            hearts,
        );
        if let Some(pos) = result.play {
            // The context is the whole queue, filtered or not.
            play_from(player, slot, visible[pos]);
        }

        let out = self.table.edit_out();
        if let Some(index) = out.remove {
            send(
                &mut self.flight,
                items,
                now,
                player,
                PlayerAction::RemoveFromQueue { index },
            );
        }
        if let Some((from, at)) = out.grip
            && self.grab.is_none()
            && !released_after_press(&ui.input().events)
            && !edit_blocked(self.flight.as_ref().map(|(a, t)| (a, *t)), items, now)
        {
            self.grab = Some(Grab {
                from,
                at,
                moving: false,
                items: Arc::clone(items),
            });
        }
        if let Some(index) = out.menu
            && self.menu.is_none()
        {
            let at = ui.pointer_pos().unwrap_or((geo.table.x, geo.table.y));
            self.menu = Some(RowMenu {
                index,
                at,
                items: Arc::clone(items),
            });
        }
        self.row_menu(ui, items, player, now, (n, filtered));
        QueueOut { filter_focused }
    }

    /// The row menu, while one is open.
    fn row_menu(
        &mut self,
        ui: &mut Ui<'_>,
        items: &Arc<Vec<Track>>,
        player: &PlayerHandle,
        now: Duration,
        (len, filtered): (usize, bool),
    ) {
        let Some(m) = self.menu.as_ref() else {
            return;
        };
        if !Arc::ptr_eq(&m.items, items) {
            self.menu = None;
            return;
        }
        let (from, at) = (m.index, m.at);
        let target = |mv| (!filtered).then(|| menu_move(from, mv, len)).flatten();
        let moves = [Move::Up, Move::Down, Move::Top, Move::Bottom];
        let names = ["Move up", "Move down", "Move to top", "Move to bottom"];
        let mut rows: Vec<MenuRow<'_>> = moves
            .iter()
            .zip(names)
            .map(|(&mv, text)| {
                MenuRow::Entry(MenuItem {
                    text,
                    disabled: target(mv).is_none(),
                    ..MenuItem::default()
                })
            })
            .collect();
        rows.push(MenuRow::Separator);
        rows.push(MenuRow::Entry(MenuItem {
            text: "Remove from queue",
            danger: true,
            ..MenuItem::default()
        }));
        let r = ui.menu("queue-row-menu", &rows, at);
        if let Some(i) = r.picked {
            let action = match i {
                0..=3 => target(moves[i]).map(|to| PlayerAction::MoveInQueue { from, to }),
                _ => Some(PlayerAction::RemoveFromQueue { index: from }),
            };
            if let Some(action) = action {
                send(&mut self.flight, items, now, player, action);
            }
            self.menu = None;
        } else if r.dismissed {
            self.menu = None;
        }
    }
}

/// Follows a grip press: it becomes a drag when the pointer leaves the press point, and a
/// release over the list drops the row (one `MoveInQueue`). A new snapshot or Escape ends it.
fn advance_grab(
    (grab, flight): (&mut Option<Grab>, &mut Flight),
    table: &TrackTable,
    ui: &Ui<'_>,
    items: &Arc<Vec<Track>>,
    player: &PlayerHandle,
    filtered: bool,
) {
    let Some(g) = grab.as_mut() else {
        return;
    };
    let mut release = None;
    let mut cancel = filtered || !Arc::ptr_eq(&g.items, items);
    for event in &ui.input().events {
        match event {
            Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed: false,
                ..
            } => release = Some(*pos),
            Event::Key {
                key: Key::Named(NamedKey::Escape),
                pressed: true,
                ..
            } => cancel = true,
            _ => {}
        }
    }
    if cancel {
        *grab = None;
        return;
    }
    if let Some(p) = release.or(ui.pointer_pos())
        && (p.0 - g.at.0).hypot(p.1 - g.at.1) >= DRAG_START
    {
        g.moving = true;
    }
    let Some(rel) = release else {
        return;
    };
    let (from, moving) = (g.from, g.moving);
    *grab = None;
    if moving
        && table.list_rect().contains(rel)
        && let Some(slot) = slot_at(table.rows(), rel.1)
    {
        let to = final_index(from, slot);
        if to != from {
            send(
                flight,
                items,
                ui.time(),
                player,
                PlayerAction::MoveInQueue { from, to },
            );
        }
    }
}

/// A primary press followed by its release inside one batch of events (a touchpad tap, or a
/// slow frame): the grip was clicked, not dragged, and no later frame will see the release.
fn released_after_press(events: &[Event]) -> bool {
    let mut pressed = false;
    for event in events {
        if let Event::PointerButton {
            button: PointerButton::Primary,
            pressed: down,
            ..
        } = event
        {
            if *down {
                pressed = true;
            } else if pressed {
                return true;
            }
        }
    }
    false
}

/// The header's buttons on the filter bar's row, right-aligned to `row`'s right edge less the
/// time column's inset: Shuffle and Repeat as the media control draws them, then Clear queue.
/// `clear` is `(shown, armed since)`: Clear queue shows only for a queue with songs.
fn controls(
    ui: &mut Ui<'_>,
    k: &Look,
    state: &PlayerState,
    player: &PlayerHandle,
    row: Rect,
    (shown, armed): (bool, &mut Option<Duration>),
    (flight, items): (&mut Flight, &Arc<Vec<Track>>),
) {
    let now = ui.time();
    let cy = row.y + row.h / 2.0;
    let right = row.right() - 8.0;
    let size = ui.theme().font_base;
    let wide = width(ui.painter(), CLEAR_ARMED_LABEL, size, false);
    let repeat_x = right - wide - CLEAR_GAP - BUTTON_HALF;
    let shuffle_x = repeat_x - BUTTON_PITCH;

    let press = |ui: &mut Ui<'_>, key: &str, x: f32| {
        let id = ui.id(key);
        let rect = Rect::new(
            x - BUTTON_HALF,
            cy - BUTTON_HALF,
            2.0 * BUTTON_HALF,
            2.0 * BUTTON_HALF,
        );
        let r = ui.interact(id, rect, Sense::CLICK);
        if r.hovered {
            ui.set_cursor(CursorIcon::Pointer);
        }
        r
    };
    let shuffle = press(ui, "queue-shuffle", shuffle_x);
    let repeat = press(ui, "queue-repeat", repeat_x);
    ui.tooltip(&shuffle, "Shuffle");
    ui.tooltip(&repeat, "Repeat");
    if shuffle.clicked {
        player.dispatch(PlayerAction::SetShuffle(!state.shuffle));
    }
    if repeat.clicked {
        let next = match state.repeat {
            RepeatMode::Off => RepeatMode::All,
            RepeatMode::All => RepeatMode::One,
            RepeatMode::One => RepeatMode::Off,
        };
        player.dispatch(PlayerAction::SetRepeat(next));
    }
    {
        let ink = k.ink;
        let sw = if k.decor().glyphs == Glyphs::Solid {
            2.0
        } else {
            HAIR
        };
        let sh_col = if state.shuffle {
            k.a
        } else {
            lift(k, ink.muted, shuffle.hovered)
        };
        let on_repeat = state.repeat != RepeatMode::Off;
        let rp_col = if on_repeat {
            k.a
        } else {
            lift(k, ink.muted, repeat.hovered)
        };
        let p = ui.painter();
        icon(p, Icon::Shuffle, (shuffle_x, cy), 18.0, sh_col, sw);
        icon(p, Icon::Repeat, (repeat_x, cy), 18.0, rp_col, sw);
        if state.repeat == RepeatMode::One {
            text_c(p, "1", 7.5, rp_col, true, repeat_x, cy - 3.6);
        }
        match k.decor().indicator {
            Indicator::Led => {
                led(p, (shuffle_x, cy + 16.0), 3.0, k.a, k.hi, state.shuffle);
                led(p, (repeat_x, cy + 16.0), 3.0, k.a, k.hi, on_repeat);
            }
            Indicator::Check => {
                if state.shuffle {
                    disc(p, (shuffle_x, cy + 15.0), 3.5, k.a);
                }
                if on_repeat {
                    disc(p, (repeat_x, cy + 15.0), 3.5, k.a);
                }
            }
        }
    }

    if !shown {
        *armed = None;
        return;
    }
    if armed.is_some_and(|at| now.saturating_sub(at) >= CLEAR_ARM) {
        *armed = None;
    }
    let label = if armed.is_some() {
        CLEAR_ARMED_LABEL
    } else {
        CLEAR_LABEL
    };
    let w = width(ui.painter(), label, size, false);
    let x = right - w;
    let id = ui.id("queue-clear");
    let hit = Rect::new(x - 6.0, cy - BUTTON_HALF, w + 12.0, 2.0 * BUTTON_HALF);
    let r = ui.interact(id, hit, Sense::CLICK);
    if r.hovered {
        ui.set_cursor(CursorIcon::Pointer);
    }
    let color = if armed.is_some() {
        k.a
    } else {
        lift(k, k.ink.text2, r.hovered)
    };
    text(
        ui.painter(),
        label,
        size,
        color,
        false,
        x,
        cy - size * 0.62,
        0.0,
    );
    if r.clicked {
        match *armed {
            Some(_) => {
                if send(flight, items, now, player, PlayerAction::ClearQueue) {
                    *armed = None;
                }
            }
            None => {
                *armed = Some(now);
                ui.request_repaint_at(now + CLEAR_ARM);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn q1_final_index_of_a_drop_slot() {
        assert_eq!(final_index(2, 0), 0);
        assert_eq!(final_index(2, 2), 2, "above itself: no move");
        assert_eq!(final_index(2, 3), 2, "below itself: no move");
        assert_eq!(final_index(2, 5), 4);
        assert_eq!(final_index(0, 6), 5, "below the last of six");
    }

    #[test]
    fn q2_menu_moves() {
        assert_eq!(menu_move(0, Move::Up, 5), None);
        assert_eq!(menu_move(4, Move::Down, 5), None);
        assert_eq!(menu_move(3, Move::Top, 5), Some(0));
        assert_eq!(menu_move(0, Move::Bottom, 5), Some(4));
        assert_eq!(menu_move(3, Move::Up, 5), Some(2));
        assert_eq!(menu_move(3, Move::Down, 5), Some(4));
        assert_eq!(menu_move(0, Move::Top, 5), None, "already on top");
        assert_eq!(menu_move(4, Move::Bottom, 5), None, "already last");
        assert_eq!(menu_move(0, Move::Bottom, 0), None, "an empty queue");
    }

    fn button(pressed: bool, button: PointerButton) -> Event {
        Event::PointerButton {
            pos: (1.0, 2.0),
            button,
            pressed,
            modifiers: Default::default(),
        }
    }

    #[test]
    fn q4_a_press_and_release_in_one_frame_is_no_grab() {
        use PointerButton::{Primary, Secondary};
        assert!(!released_after_press(&[]));
        assert!(!released_after_press(&[button(true, Primary)]), "held");
        assert!(released_after_press(&[
            button(true, Primary),
            button(false, Primary)
        ]));
        assert!(
            !released_after_press(&[button(false, Primary), button(true, Primary)]),
            "an earlier release is not this press's"
        );
        assert!(
            !released_after_press(&[button(true, Primary), button(false, Secondary)]),
            "another button"
        );
    }

    #[test]
    fn q3_one_edit_in_flight() {
        let ms = Duration::from_millis;
        let old = Arc::new(vec![1u8]);
        let new = Arc::new(vec![2u8]);
        assert!(!edit_blocked(None, &old, ms(0)), "nothing sent");
        assert!(edit_blocked(Some((&old, ms(100))), &old, ms(500)));
        assert!(
            !edit_blocked(Some((&old, ms(100))), &new, ms(500)),
            "a new snapshot releases"
        );
        assert!(edit_blocked(Some((&old, ms(100))), &old, ms(1099)));
        assert!(
            !edit_blocked(Some((&old, ms(100))), &old, ms(1100)),
            "a second releases"
        );
    }
}
