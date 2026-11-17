//! The play queue: items in list order plus a play order (identity, or a shuffle with the
//! current item first). Pure data; the control loop owns the one instance.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::sync::Arc;

use super::state::{QueueState, RepeatMode};
use crate::provider::Track;

/// Small xorshift64 generator for the shuffle order (core takes no `rand` dependency).
struct XorShift(u64);

impl XorShift {
    fn seeded() -> Self {
        let mut h = RandomState::new().build_hasher();
        h.write_u64(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos() as u64),
        );
        XorShift(h.finish() | 1)
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// Uniform enough for a play order: `0..n`, `n > 0`.
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Where play continues after the current item was removed from the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Resume {
    /// The list index of the item that plays next.
    At(usize),
    /// The removed item was last in play order: `next` is the end (or a wrap), and the first
    /// item enqueued afterwards plays next.
    End,
}

pub(crate) struct Queue {
    items: Arc<Vec<Track>>,
    /// A permutation of `0..items.len()`: the play order.
    order: Vec<usize>,
    current: Option<usize>,
    /// Set only while nothing is current because the current item was removed.
    resume: Option<Resume>,
    shuffle: bool,
    rng: XorShift,
}

/// The text before the first `:` of a track uri; the whole uri when it has none.
fn uri_scheme(uri: &str) -> &str {
    uri.split_once(':').map_or(uri, |(scheme, _)| scheme)
}

impl Resume {
    fn map(self, f: impl FnOnce(usize) -> usize) -> Self {
        match self {
            Resume::At(i) => Resume::At(f(i)),
            Resume::End => Resume::End,
        }
    }
}

impl Default for Queue {
    fn default() -> Self {
        Queue {
            items: Arc::default(),
            order: Vec::new(),
            current: None,
            resume: None,
            shuffle: false,
            rng: XorShift::seeded(),
        }
    }
}

impl Queue {
    /// The published view. `next` is what `advance` chooses for `repeat`: under One the
    /// current item again (replay), or nothing in the list when the playing item left it
    /// (`now_playing`: something is playing that is not a list item); else `next(repeat == All)`.
    pub(crate) fn snapshot(&self, repeat: RepeatMode, now_playing: bool) -> QueueState {
        let next = match (repeat, self.current) {
            (RepeatMode::One, Some(c)) => Some(c),
            (RepeatMode::One, None) if now_playing => None,
            _ => self.next(repeat == RepeatMode::All),
        };
        QueueState {
            items: self.items.clone(),
            current: self.current,
            next,
        }
    }

    pub(crate) fn get(&self, index: usize) -> Option<&Track> {
        self.items.get(index)
    }

    pub(crate) fn current(&self) -> Option<usize> {
        self.current
    }

    #[cfg(test)]
    pub(crate) fn order(&self) -> &[usize] {
        &self.order
    }

    /// Replace the whole queue; `index` becomes current (the caller checked it is in range).
    pub(crate) fn replace(&mut self, items: Vec<Track>, index: usize) {
        self.items = Arc::new(items);
        self.current = (index < self.items.len()).then_some(index);
        self.resume = None;
        self.rebuild_order(self.current);
    }

    /// Make the item at `index` current if it is still `uri`; otherwise the first item with
    /// that uri; otherwise nothing in the list is current.
    pub(crate) fn set_current(&mut self, index: usize, uri: &str) {
        self.current = if self.items.get(index).is_some_and(|t| t.uri == uri) {
            Some(index)
        } else {
            self.items.iter().position(|t| t.uri == uri)
        };
        self.resume = None;
    }

    /// Nothing in the list is current any more (a remote plays its own track); the list stays,
    /// and `next` continues with the item after the one that was current, as after `remove`.
    pub(crate) fn detach(&mut self) {
        let Some(c) = self.current.take() else {
            return;
        };
        self.resume = Some(
            self.pos(c)
                .and_then(|p| self.order.get(p + 1).copied())
                .map_or(Resume::End, Resume::At),
        );
    }

    fn pos(&self, index: usize) -> Option<usize> {
        self.order.iter().position(|&i| i == index)
    }

    /// The list index to play after the current one; `None` at the end (unless `wrap`).
    pub(crate) fn next(&self, wrap: bool) -> Option<usize> {
        if self.items.is_empty() {
            return None;
        }
        let start = match (self.current, self.resume) {
            (Some(c), _) => self.pos(c)? + 1,
            (None, Some(Resume::At(r))) => return Some(r),
            (None, Some(Resume::End)) => self.order.len(),
            (None, None) => 0,
        };
        match self.order.get(start) {
            Some(&i) => Some(i),
            None if wrap => self.order.first().copied(),
            None => None,
        }
    }

    /// The list index before the current one; `None` at the start (unless `wrap`).
    pub(crate) fn previous(&self, wrap: bool) -> Option<usize> {
        let p = match (self.current, self.resume) {
            (Some(c), _) | (None, Some(Resume::At(c))) => self.pos(c)?,
            // Past the end: the item before is the last one in play order.
            (None, Some(Resume::End)) => self.order.len(),
            (None, None) => return None,
        };
        if p > 0 {
            Some(self.order[p - 1])
        } else if wrap {
            self.order.last().copied()
        } else {
            None
        }
    }

    /// Append `tracks`. A queue holds the tracks of one provider (their uri scheme, the routing
    /// key of `ProviderRegistry::resolve_uri`): when the first new track has another scheme than
    /// an item already queued, the queue is cleared first, so the old provider's tracks make way
    /// for the new one's (D11). A playing item keeps playing, as after `clear`.
    pub(crate) fn enqueue(&mut self, tracks: Vec<Track>) {
        let Some(first) = tracks.first() else {
            return;
        };
        let scheme = uri_scheme(&first.uri);
        if self.items.iter().any(|t| uri_scheme(&t.uri) != scheme) {
            self.clear();
        }
        let start = self.items.len();
        Arc::make_mut(&mut self.items).extend(tracks);
        self.order.extend(start..self.items.len());
        if self.resume == Some(Resume::End) {
            self.resume = Some(Resume::At(start));
        }
    }

    /// Remove one item. The current item may be removed: it keeps playing, nothing in the list
    /// is current, and `next` continues with the item after it.
    pub(crate) fn remove(&mut self, index: usize) -> bool {
        if index >= self.items.len() {
            return false;
        }
        // The item after `index` in play order, or past the end when it is last.
        let successor = |q: &Queue| {
            q.pos(index)
                .and_then(|p| q.order.get(p + 1).copied())
                .map_or(Resume::End, Resume::At)
        };
        if self.current == Some(index) {
            self.resume = Some(successor(self));
            self.current = None;
        } else if self.resume == Some(Resume::At(index)) {
            self.resume = Some(successor(self));
        }
        Arc::make_mut(&mut self.items).remove(index);
        let shift = |x: usize| if x > index { x - 1 } else { x };
        self.order.retain(|&i| i != index);
        for i in &mut self.order {
            *i = shift(*i);
        }
        self.current = self.current.map(shift);
        self.resume = self.resume.map(|r| r.map(shift));
        true
    }

    pub(crate) fn move_item(&mut self, from: usize, to: usize) -> bool {
        let len = self.items.len();
        if from >= len || to >= len {
            return false;
        }
        if from == to {
            return true;
        }
        let items = Arc::make_mut(&mut self.items);
        let t = items.remove(from);
        items.insert(to, t);
        let remap = |x: usize| {
            if x == from {
                to
            } else if from < to && x > from && x <= to {
                x - 1
            } else if to < from && x >= to && x < from {
                x + 1
            } else {
                x
            }
        };
        self.current = self.current.map(remap);
        self.resume = self.resume.map(|r| r.map(remap));
        if self.shuffle {
            for i in &mut self.order {
                *i = remap(*i);
            }
        } else {
            self.order = (0..len).collect();
        }
        true
    }

    pub(crate) fn clear(&mut self) {
        self.items = Arc::default();
        self.order.clear();
        self.current = None;
        self.resume = None;
    }

    pub(crate) fn set_shuffle(&mut self, on: bool) {
        if self.shuffle == on {
            return;
        }
        self.shuffle = on;
        let first = match (self.current, self.resume) {
            (Some(c), _) | (None, Some(Resume::At(c))) => Some(c),
            _ => None,
        };
        self.rebuild_order(first);
    }

    /// List order, or a shuffle with `first` (when given) at the front.
    fn rebuild_order(&mut self, first: Option<usize>) {
        let len = self.items.len();
        if !self.shuffle {
            self.order = (0..len).collect();
            return;
        }
        let mut rest: Vec<usize> = (0..len).filter(|&i| Some(i) != first).collect();
        for i in (1..rest.len()).rev() {
            let j = self.rng.below(i + 1);
            rest.swap(i, j);
        }
        self.order = first.filter(|&f| f < len).into_iter().chain(rest).collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracks(n: usize) -> Vec<Track> {
        (0..n)
            .map(|i| Track {
                uri: format!("fake:track:{i}"),
                name: format!("t{i}"),
                artist: String::new(),
                album: String::new(),
                duration_ms: 1000,
                images: vec![],
                year: None,
            })
            .collect()
    }

    #[test]
    fn shuffle_is_a_permutation_with_the_current_item_first_and_off_returns_to_list_order() {
        let mut q = Queue::default();
        q.replace(tracks(50), 17);
        q.set_shuffle(true);
        assert_eq!(q.order()[0], 17);
        let mut sorted = q.order().to_vec();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..50).collect::<Vec<_>>());
        q.set_shuffle(false);
        assert_eq!(q.order(), (0..50).collect::<Vec<_>>());
        assert_eq!(q.current(), Some(17));
        assert_eq!(q.next(false), Some(18));
    }

    #[test]
    fn detach_keeps_the_list_and_continues_after_the_detached_item() {
        let mut q = Queue::default();
        q.replace(tracks(3), 1);
        q.detach();
        assert_eq!(q.current(), None);
        assert_eq!(q.snapshot(RepeatMode::Off, true).items.len(), 3);
        assert_eq!(q.next(false), Some(2));
        assert_eq!(q.previous(false), Some(1));
        // The last item detached: the end, or a wrap.
        q.replace(tracks(3), 2);
        q.detach();
        assert_eq!(q.next(false), None);
        assert_eq!(q.next(true), Some(0));
        // Nothing current: nothing changes.
        q.detach();
        assert_eq!(q.next(true), Some(0));
    }

    #[test]
    fn removing_and_moving_keep_current_and_ignore_out_of_range() {
        let mut q = Queue::default();
        q.replace(tracks(4), 2);
        assert!(!q.remove(4));
        assert!(!q.move_item(0, 9));
        assert!(q.remove(0));
        assert_eq!(q.current(), Some(1));
        assert_eq!(q.get(1).unwrap().uri, "fake:track:2");
        // Removing the current item: nothing current, next continues after it.
        assert!(q.remove(1));
        assert_eq!(q.current(), None);
        assert_eq!(q.get(q.next(false).unwrap()).unwrap().uri, "fake:track:3");
        assert_eq!(
            q.get(q.previous(false).unwrap()).unwrap().uri,
            "fake:track:1"
        );
        // Removing the current item when it is last in play order: Off stops, All wraps,
        // previous goes to the item before it, and a later enqueue plays next.
        q.replace(tracks(3), 2);
        assert!(q.remove(2));
        assert_eq!(q.current(), None);
        assert_eq!(q.next(false), None);
        assert_eq!(q.next(true), Some(0));
        assert_eq!(q.previous(false), Some(1));
        assert!(q.move_item(0, 1));
        assert_eq!(q.next(false), None);
        q.enqueue(tracks(5).split_off(3));
        assert_eq!(q.get(q.next(false).unwrap()).unwrap().uri, "fake:track:3");
        // The resume item itself removed while last: past the end again.
        assert!(q.remove(3));
        assert!(q.remove(2));
        assert_eq!(q.next(false), None);
        assert_eq!(q.next(true), Some(0));
        // Shuffle toggled past the end keeps it there.
        q.set_shuffle(true);
        assert_eq!(q.next(false), None);
        q.set_shuffle(false);
        assert_eq!(q.next(false), None);
        q.replace(tracks(3), 0);
        assert!(q.move_item(0, 2));
        assert_eq!(q.current(), Some(2));
        assert_eq!(q.next(false), None);
        assert_eq!(q.next(true), Some(0));
        assert_eq!(q.previous(false), Some(1));
    }

    fn with_scheme(scheme: &str, n: usize) -> Vec<Track> {
        tracks(n)
            .into_iter()
            .map(|mut t| {
                t.uri = t.uri.replacen("fake", scheme, 1);
                t
            })
            .collect()
    }

    #[test]
    fn enqueueing_another_providers_track_clears_the_queue_first() {
        let mut q = Queue::default();
        q.replace(with_scheme("spotify", 3), 1);
        // The same scheme appends as before.
        q.enqueue(with_scheme("spotify", 2));
        assert_eq!(q.snapshot(RepeatMode::Off, true).items.len(), 5);
        assert_eq!(q.current(), Some(1));
        // Another scheme: only the new tracks are left; the old current item is gone from the list.
        q.enqueue(with_scheme("local", 2));
        let items = q.snapshot(RepeatMode::Off, true).items;
        assert_eq!(items.len(), 2);
        assert!(items.iter().all(|t| t.uri.starts_with("local:")));
        assert_eq!(q.current(), None);
        assert_eq!(q.order(), [0, 1]);
        assert_eq!(q.get(q.next(false).unwrap()).unwrap().uri, "local:track:0");
        // An empty enqueue changes nothing, whatever its scheme; a uri with no colon is its own scheme.
        q.enqueue(vec![]);
        assert_eq!(q.snapshot(RepeatMode::Off, true).items.len(), 2);
        let mut bare = tracks(1);
        bare[0].uri = "other".into();
        q.enqueue(bare);
        assert_eq!(q.snapshot(RepeatMode::Off, true).items.len(), 1);
        // An empty queue takes any scheme.
        q.clear();
        q.enqueue(with_scheme("spotify", 1));
        assert_eq!(q.snapshot(RepeatMode::Off, true).items.len(), 1);
    }

    // ---- property test (H13, D3): random op sequences against a plain reference model ----

    /// SplitMix64, as in `whipspot-testkit`'s `rng` (core cannot dev-depend on the testkit).
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }

        fn below(&mut self, n: usize) -> usize {
            if n == 0 {
                0
            } else {
                (self.next() % n as u64) as usize
            }
        }
    }

    #[derive(Clone, Copy, Debug)]
    enum Op {
        Replace {
            count: usize,
            index: usize,
        },
        Enqueue {
            count: usize,
        },
        Remove(usize),
        Move {
            from: usize,
            to: usize,
        },
        Clear,
        SetShuffle(bool),
        /// `pick`: an existing item (by list index modulo the length) or an unknown uri.
        SetCurrent {
            index: usize,
            pick: Option<usize>,
        },
        /// What the control loop does for next/previous: query, then make that item current.
        Advance {
            back: bool,
            wrap: bool,
        },
    }

    /// An item is identified by its number (its uri is `fake:track:<id>`), so the model needs no
    /// index arithmetic. `order` is the play order in ids.
    #[derive(Clone, Copy, Debug, PartialEq)]
    enum ModelResume {
        At(u32),
        End,
    }

    #[derive(Default)]
    struct Model {
        items: Vec<u32>,
        order: Vec<u32>,
        current: Option<u32>,
        resume: Option<ModelResume>,
        shuffle: bool,
    }

    impl Model {
        fn pos(&self, id: u32) -> Option<usize> {
            self.order.iter().position(|&i| i == id)
        }

        fn next(&self, wrap: bool) -> Option<u32> {
            if self.items.is_empty() {
                return None;
            }
            let start = match (self.current, self.resume) {
                (Some(c), _) => self.pos(c)? + 1,
                (None, Some(ModelResume::At(r))) => return Some(r),
                (None, Some(ModelResume::End)) => self.order.len(),
                (None, None) => 0,
            };
            match self.order.get(start) {
                Some(&i) => Some(i),
                None if wrap => self.order.first().copied(),
                None => None,
            }
        }

        fn previous(&self, wrap: bool) -> Option<u32> {
            let p = match (self.current, self.resume) {
                (Some(c), _) | (None, Some(ModelResume::At(c))) => self.pos(c)?,
                (None, Some(ModelResume::End)) => self.order.len(),
                (None, None) => return None,
            };
            if p > 0 {
                Some(self.order[p - 1])
            } else if wrap {
                self.order.last().copied()
            } else {
                None
            }
        }

        /// The item a shuffle starts from: the current one, else the resume item.
        fn anchor(&self) -> Option<u32> {
            match (self.current, self.resume) {
                (Some(c), _) | (None, Some(ModelResume::At(c))) => Some(c),
                _ => None,
            }
        }
    }

    fn uri_id(t: &Track) -> u32 {
        t.uri.strip_prefix("fake:track:").unwrap().parse().unwrap()
    }

    fn id_tracks(ids: &[u32]) -> Vec<Track> {
        ids.iter()
            .map(|&i| Track {
                uri: format!("fake:track:{i}"),
                name: format!("t{i}"),
                artist: String::new(),
                album: String::new(),
                duration_ms: 1000,
                images: vec![],
                year: None,
            })
            .collect()
    }

    fn queue_ids(q: &Queue) -> Vec<u32> {
        q.snapshot(RepeatMode::Off, false)
            .items
            .iter()
            .map(uri_id)
            .collect()
    }

    fn queue_order(q: &Queue) -> Vec<u32> {
        q.order()
            .iter()
            .map(|&i| uri_id(q.get(i).expect("order holds in-range indexes")))
            .collect()
    }

    fn id_at(q: &Queue, index: Option<usize>) -> Option<u32> {
        index.map(|i| uri_id(q.get(i).expect("an index the queue returned is in range")))
    }

    /// The model takes the queue's (random) shuffle order after checking it is a permutation of
    /// the items with `first` at the front when there is one.
    fn adopt_order(q: &Queue, m: &mut Model, first: Option<u32>) -> Result<(), String> {
        let got = queue_order(q);
        let mut a = got.clone();
        let mut b = queue_ids(q);
        a.sort_unstable();
        b.sort_unstable();
        if a != b {
            return Err(format!(
                "shuffle order {got:?} is not a permutation of the items"
            ));
        }
        if let Some(f) = first
            && got.first() != Some(&f)
        {
            return Err(format!("shuffle order {got:?} does not start with {f}"));
        }
        m.order = got;
        Ok(())
    }

    fn random_ops(rng: &mut Rng) -> Vec<Op> {
        let n = 5 + rng.below(36);
        (0..n)
            .map(|_| match rng.below(100) {
                0..=7 => Op::Replace {
                    count: rng.below(9),
                    index: rng.below(10),
                },
                8..=27 => Op::Enqueue {
                    count: rng.below(5),
                },
                28..=45 => Op::Remove(rng.below(12)),
                46..=60 => Op::Move {
                    from: rng.below(12),
                    to: rng.below(12),
                },
                61..=63 => Op::Clear,
                64..=73 => Op::SetShuffle(rng.below(2) == 0),
                74..=81 => Op::SetCurrent {
                    index: rng.below(12),
                    pick: (rng.below(5) != 0).then(|| rng.below(1000)),
                },
                _ => Op::Advance {
                    back: rng.below(2) == 0,
                    wrap: rng.below(2) == 0,
                },
            })
            .collect()
    }

    fn fresh_ids(fresh: &mut u32, count: usize) -> Vec<u32> {
        let ids: Vec<u32> = (*fresh..*fresh + count as u32).collect();
        *fresh += count as u32;
        ids
    }

    fn apply(op: Op, q: &mut Queue, m: &mut Model, fresh: &mut u32) -> Result<(), String> {
        match op {
            Op::Replace { count, index } => {
                let ids = fresh_ids(fresh, count);
                q.replace(id_tracks(&ids), index);
                m.current = ids.get(index).copied();
                m.items = ids;
                m.resume = None;
                if m.shuffle {
                    let first = m.current;
                    adopt_order(q, m, first)?;
                } else {
                    m.order = m.items.clone();
                }
            }
            Op::Enqueue { count } => {
                let ids = fresh_ids(fresh, count);
                q.enqueue(id_tracks(&ids));
                m.items.extend(&ids);
                m.order.extend(&ids);
                if m.resume == Some(ModelResume::End) && !ids.is_empty() {
                    m.resume = Some(ModelResume::At(ids[0]));
                }
            }
            Op::Remove(index) => {
                let got = q.remove(index);
                if got != (index < m.items.len()) {
                    return Err(format!("remove({index}) returned {got}"));
                }
                if got {
                    let id = m.items[index];
                    let succ = m
                        .pos(id)
                        .and_then(|p| m.order.get(p + 1).copied())
                        .map_or(ModelResume::End, ModelResume::At);
                    if m.current == Some(id) {
                        m.resume = Some(succ);
                        m.current = None;
                    } else if m.resume == Some(ModelResume::At(id)) {
                        m.resume = Some(succ);
                    }
                    m.items.remove(index);
                    m.order.retain(|&i| i != id);
                }
            }
            Op::Move { from, to } => {
                let got = q.move_item(from, to);
                let ok = from < m.items.len() && to < m.items.len();
                if got != ok {
                    return Err(format!("move_item({from}, {to}) returned {got}"));
                }
                if ok {
                    let id = m.items.remove(from);
                    m.items.insert(to, id);
                    if !m.shuffle {
                        m.order = m.items.clone();
                    }
                }
            }
            Op::Clear => {
                q.clear();
                *m = Model {
                    shuffle: m.shuffle,
                    ..Model::default()
                };
            }
            Op::SetShuffle(on) => {
                let first = m.anchor();
                q.set_shuffle(on);
                if on != m.shuffle {
                    m.shuffle = on;
                    if on {
                        adopt_order(q, m, first)?;
                    } else {
                        m.order = m.items.clone();
                    }
                }
            }
            Op::SetCurrent { index, pick } => {
                let id = match pick {
                    Some(p) if !m.items.is_empty() => m.items[p % m.items.len()],
                    _ => u32::MAX,
                };
                q.set_current(index, &format!("fake:track:{id}"));
                m.current = m.items.contains(&id).then_some(id);
                m.resume = None;
            }
            Op::Advance { back, wrap } => {
                let want = if back { m.previous(wrap) } else { m.next(wrap) };
                let got = if back { q.previous(wrap) } else { q.next(wrap) };
                if id_at(q, got) != want {
                    return Err(format!(
                        "{}({wrap}) gave {:?}, the model {want:?}",
                        if back { "previous" } else { "next" },
                        id_at(q, got)
                    ));
                }
                if let (Some(id), Some(index)) = (want, got) {
                    q.set_current(index, &format!("fake:track:{id}"));
                    m.current = Some(id);
                    m.resume = None;
                }
            }
        }
        Ok(())
    }

    fn compare(q: &Queue, m: &Model) -> Result<(), String> {
        let items = queue_ids(q);
        if items != m.items {
            return Err(format!("items {items:?}, the model {:?}", m.items));
        }
        let order = queue_order(q);
        if order != m.order {
            return Err(format!("order {order:?}, the model {:?}", m.order));
        }
        if !m.shuffle && order != items {
            return Err("shuffle is off but the order is not the list order".into());
        }
        if id_at(q, q.current()) != m.current {
            return Err(format!(
                "current {:?}, the model {:?}",
                id_at(q, q.current()),
                m.current
            ));
        }
        for wrap in [false, true] {
            if id_at(q, q.next(wrap)) != m.next(wrap) {
                return Err(format!(
                    "next({wrap}) {:?}, the model {:?}",
                    id_at(q, q.next(wrap)),
                    m.next(wrap)
                ));
            }
            if id_at(q, q.previous(wrap)) != m.previous(wrap) {
                return Err(format!(
                    "previous({wrap}) {:?}, the model {:?}",
                    id_at(q, q.previous(wrap)),
                    m.previous(wrap)
                ));
            }
        }
        for repeat in [RepeatMode::Off, RepeatMode::All, RepeatMode::One] {
            for now_playing in [false, true] {
                let snap = q.snapshot(repeat, now_playing);
                let want_next = match (repeat, m.current) {
                    (RepeatMode::One, Some(c)) => Some(c),
                    (RepeatMode::One, None) if now_playing => None,
                    _ => m.next(repeat == RepeatMode::All),
                };
                if id_at(q, snap.current) != m.current || id_at(q, snap.next) != want_next {
                    return Err(format!(
                        "snapshot({repeat:?}, {now_playing}): current {:?} next {:?}, the model \
                         {:?} / {want_next:?}",
                        id_at(q, snap.current),
                        id_at(q, snap.next),
                        m.current
                    ));
                }
            }
        }
        Ok(())
    }

    fn run_case(ops: &[Op]) -> Result<(), (usize, String)> {
        let mut q = Queue::default();
        let mut m = Model::default();
        let mut fresh = 0u32;
        for (n, &op) in ops.iter().enumerate() {
            apply(op, &mut q, &mut m, &mut fresh)
                .and_then(|()| compare(&q, &m))
                .map_err(|e| (n, e))?;
        }
        Ok(())
    }

    fn env_u64(name: &str, default: u64) -> u64 {
        std::env::var(name)
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(default)
    }

    #[test]
    fn random_op_sequences_match_a_plain_reference_model() {
        let seed = 0x0051_EED0 ^ env_u64("WHIPSPOT_FUZZ_SEED", 0);
        let cases = env_u64("WHIPSPOT_FUZZ_ITERS", 500);
        for case in 0..cases {
            let mut rng = Rng(seed ^ case.wrapping_mul(0xD6E8_FEB8_6659_FD93));
            let ops = random_ops(&mut rng);
            if let Err((n, msg)) = run_case(&ops) {
                panic!(
                    "queue property failed: seed={seed} case={case} at op {n} ({:?}): {msg}\nops: {ops:?}",
                    ops[n]
                );
            }
        }
    }

    #[test]
    fn the_model_comparison_reports_a_skewed_model() {
        // A known sequence passes; then a model with its resume item dropped must be reported,
        // so a green property run is not vacuous.
        let ops = [
            Op::Replace { count: 4, index: 2 },
            Op::Remove(2),
            Op::Enqueue { count: 2 },
        ];
        run_case(&ops).unwrap();
        let mut q = Queue::default();
        let mut m = Model::default();
        let mut fresh = 0;
        for op in ops {
            apply(op, &mut q, &mut m, &mut fresh).unwrap();
        }
        compare(&q, &m).unwrap();
        m.resume = None;
        assert!(compare(&q, &m).is_err());
    }
}
