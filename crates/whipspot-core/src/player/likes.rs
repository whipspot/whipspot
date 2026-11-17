//! Likes scheduling (MD69, MD70). Owned by the control loop: `SetLiked` calls are serialised per
//! provider (one in flight; a waiting entry for the same uri takes the newer target); reads are
//! grouped per provider. Tasks report through `Internal`; only the loop changes the state.
//!
//! A read never overwrites a newer local intent: a read leaves out the uris that have a change
//! running or waiting when it starts, and a change queued while a read runs strikes its uri
//! from that read, so only answers read after every change of a uri are posted.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::provider::ProviderRegistry;

/// The most uris one `RequestLiked` asks about; the rest are ignored.
pub(crate) const MAX_LIKE_URIS: usize = 200;
/// The most `SetLiked` calls waiting per provider (each a different uri); more are refused.
pub(crate) const MAX_LIKE_QUEUE: usize = 200;

#[derive(Default)]
struct Queue {
    /// The uri of the change in flight.
    running: Option<String>,
    waiting: VecDeque<(String, bool)>,
}

impl Queue {
    fn has(&self, uri: &str) -> bool {
        self.running.as_deref() == Some(uri) || self.waiting.iter().any(|(u, _)| u == uri)
    }
}

/// Per-provider queues of like changes, and the reads in flight.
#[derive(Default)]
pub(crate) struct LikeQueues {
    queues: HashMap<&'static str, Queue>,
    /// Per read in flight: the uris whose answer may still be posted.
    reads: HashMap<u64, HashSet<String>>,
    next_read: u64,
}

impl LikeQueues {
    /// Queue `(uri, liked)` for `provider`. Returns the call to start now, if the provider has
    /// none running; `Err` when the queue is full.
    pub(crate) fn push(
        &mut self,
        provider: &'static str,
        uri: String,
        liked: bool,
    ) -> Result<Option<(String, bool)>, ()> {
        // Every read in flight may have seen the state before this change.
        for keep in self.reads.values_mut() {
            keep.remove(&uri);
        }
        let q = self.queues.entry(provider).or_default();
        if let Some(w) = q.waiting.iter_mut().find(|(u, _)| *u == uri) {
            w.1 = liked;
        } else if q.waiting.len() >= MAX_LIKE_QUEUE {
            return Err(());
        } else {
            q.waiting.push_back((uri, liked));
        }
        Ok(Self::start(q))
    }

    /// The running call of `provider` ended; returns the next one to start.
    pub(crate) fn done(&mut self, provider: &'static str) -> Option<(String, bool)> {
        let q = self.queues.get_mut(provider)?;
        q.running = None;
        Self::start(q)
    }

    /// A read of `uris` from `provider` starts; returns its id for [`Self::end_read`]. Uris with
    /// a change running or waiting are left out of the answer.
    pub(crate) fn begin_read(&mut self, provider: &'static str, uris: &[String]) -> u64 {
        let q = self.queues.get(provider);
        let keep = uris
            .iter()
            .filter(|u| !q.is_some_and(|q| q.has(u)))
            .cloned()
            .collect();
        let id = self.next_read;
        self.next_read += 1;
        self.reads.insert(id, keep);
        id
    }

    /// The read `id` of `uris` ended with `saved` (one bool per uri, or None on a failure): the
    /// pairs still allowed, in request order; nothing for a short answer.
    pub(crate) fn end_read(
        &mut self,
        id: u64,
        uris: Vec<String>,
        saved: Option<Vec<bool>>,
    ) -> Vec<(String, bool)> {
        let keep = self.reads.remove(&id).unwrap_or_default();
        match saved {
            Some(saved) if saved.len() == uris.len() => uris
                .into_iter()
                .zip(saved)
                .filter(|(u, _)| keep.contains(u))
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Take every waiting change of `provider` (the running one is left alone).
    pub(crate) fn drain(&mut self, provider: &'static str) -> Vec<(String, bool)> {
        self.queues
            .get_mut(provider)
            .map(|q| q.waiting.drain(..).collect())
            .unwrap_or_default()
    }

    fn start(q: &mut Queue) -> Option<(String, bool)> {
        if q.running.is_some() {
            return None;
        }
        let next = q.waiting.pop_front()?;
        q.running = Some(next.0.clone());
        Some(next)
    }
}

/// The first [`MAX_LIKE_URIS`] of `uris`, grouped by the provider that owns them (only providers
/// with the likes capability and a library), each group in request order; groups in order of
/// their first uri.
pub(crate) fn group(
    registry: &ProviderRegistry,
    uris: Vec<String>,
) -> Vec<(&'static str, Vec<String>)> {
    let mut groups: Vec<(&'static str, Vec<String>)> = Vec::new();
    for uri in uris.into_iter().take(MAX_LIKE_URIS) {
        let Some(p) = registry.resolve_uri(&uri) else {
            continue;
        };
        let info = p.info();
        if !info.caps.likes || p.library().is_none() {
            continue;
        }
        match groups.iter_mut().find(|(id, _)| *id == info.id) {
            Some((_, g)) => g.push(uri),
            None => groups.push((info.id, vec![uri])),
        }
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calls_run_one_at_a_time_and_a_waiting_entry_takes_the_newer_target() {
        let mut q = LikeQueues::default();
        assert_eq!(q.push("p", "a".into(), true), Ok(Some(("a".into(), true))));
        assert_eq!(q.push("p", "b".into(), true), Ok(None));
        assert_eq!(q.push("p", "a".into(), false), Ok(None));
        assert_eq!(q.push("p", "b".into(), false), Ok(None));
        // Another provider runs independently.
        assert_eq!(q.push("o", "x".into(), true), Ok(Some(("x".into(), true))));
        assert_eq!(q.done("p"), Some(("b".into(), false)));
        assert_eq!(q.done("p"), Some(("a".into(), false)));
        assert_eq!(q.done("p"), None);
        assert_eq!(q.push("p", "c".into(), true), Ok(Some(("c".into(), true))));
    }

    #[test]
    fn a_read_never_answers_for_a_uri_changed_before_its_answer() {
        let mut q = LikeQueues::default();
        let u = |s: &str| s.to_owned();
        // A change runs for a: a read started now leaves a out.
        q.push("p", u("a"), true).unwrap();
        let r1 = q.begin_read("p", &[u("a"), u("b")]);
        // A change for b is queued while the reads run: b is struck from both.
        let r2 = q.begin_read("p", &[u("b"), u("c")]);
        q.push("p", u("b"), true).unwrap();
        assert_eq!(
            q.end_read(r1, vec![u("a"), u("b")], Some(vec![false, false])),
            []
        );
        assert_eq!(
            q.end_read(r2, vec![u("b"), u("c")], Some(vec![false, true])),
            [(u("c"), true)]
        );
        // After both changes ended, a new read answers for a again.
        q.done("p");
        q.done("p");
        let r3 = q.begin_read("p", &[u("a")]);
        assert_eq!(
            q.end_read(r3, vec![u("a")], Some(vec![true])),
            [(u("a"), true)]
        );
        // A short answer or a failure posts nothing and forgets the read.
        let r4 = q.begin_read("p", &[u("a"), u("b")]);
        assert_eq!(q.end_read(r4, vec![u("a"), u("b")], Some(vec![true])), []);
        let r5 = q.begin_read("p", &[u("a")]);
        assert_eq!(q.end_read(r5, vec![u("a")], None), []);
        assert!(q.reads.is_empty());
    }

    #[test]
    fn a_full_queue_refuses() {
        let mut q = LikeQueues::default();
        q.push("p", "first".into(), true).unwrap();
        for n in 0..MAX_LIKE_QUEUE {
            assert_eq!(q.push("p", format!("u{n}"), true), Ok(None));
        }
        assert_eq!(q.push("p", "one more".into(), true), Err(()));
        assert_eq!(q.push("p", "u0".into(), false), Ok(None));
    }
}
