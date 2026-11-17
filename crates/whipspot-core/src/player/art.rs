//! The art pull of C1 (MD62, MD63): the UI asks per frame for what it draws; a bounded worker
//! fetches through the [`ArtSource`]; each image is handed out once.
//!
//! Concurrency: [`ArtBook`] is pure (every method takes `now`) and lives in [`ArtHub`] under a
//! std `Mutex` next to a `Notify`. [`PlayerHandle::art`] and the worker are its only users; each
//! holds the lock only for map work (no await, I/O, decode or wake under it). The worker owns
//! the fetch tasks (at most [`MAX_IN_FLIGHT`]), stores each result in the book and then calls
//! the wake with no lock held.

use std::collections::HashMap;
use std::collections::hash_map::Entry as MapEntry;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::Notify;
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinSet;

use super::control::Internal;
use super::{PlayerHandle, Shared, lock};
use crate::art::{ArtError, ArtImage, ArtSource};
use crate::provider::ImageRef;

/// Fetches running at once.
pub(crate) const MAX_IN_FLIGHT: usize = 4;
/// A want not renewed by an ask for this long is dropped.
pub(crate) const WANT_TTL: Duration = Duration::from_secs(10);
/// A ready image not taken for this long is dropped.
pub(crate) const READY_TTL: Duration = Duration::from_secs(30);
/// Untaken ready images are kept under this many bytes (w·h·4 each); the oldest go first.
pub(crate) const READY_BUDGET: usize = 64 * 1024 * 1024;
/// A failed URL is answered `Failed` for this long before it may be fetched again.
pub(crate) const RETRY_AFTER: Duration = Duration::from_secs(300);
/// The most entries the book keeps.
pub(crate) const MAX_BOOK: usize = 1024;

/// How soon the UI wants an image: `Current` is the playing item's cover, `Visible` anything else
/// on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtUrgency {
    Current,
    Visible,
}

/// The answer to [`PlayerHandle::art`].
#[derive(Debug, Clone, PartialEq)]
pub enum ArtStatus {
    /// Being fetched or waiting; ask again.
    Pending,
    /// Handed out once: the core no longer holds it.
    Ready(Arc<ArtImage>),
    /// Not available (refused, failed or no art source); do not ask again for a while.
    Failed,
}

enum Entry {
    Wanted {
        urgency: ArtUrgency,
        asked: Instant,
        image: ImageRef,
    },
    InFlight,
    Ready {
        image: Arc<ArtImage>,
        at: Instant,
    },
    Failed {
        at: Instant,
    },
}

fn bytes(image: &ArtImage) -> usize {
    image.rgba().len()
}

/// Every URL the UI asked for, by state. Pure: time comes in as `now`.
#[derive(Default)]
pub(crate) struct ArtBook {
    map: HashMap<String, Entry>,
    /// The bytes of every `Ready` entry.
    ready_bytes: usize,
}

impl ArtBook {
    /// One ask of the UI. Returns the answer and whether the worker should look for work.
    pub(crate) fn ask(
        &mut self,
        image: &ImageRef,
        urgency: ArtUrgency,
        now: Instant,
    ) -> (ArtStatus, bool) {
        match self.map.entry(image.url.clone()) {
            MapEntry::Vacant(v) => {
                v.insert(Entry::Wanted {
                    urgency,
                    asked: now,
                    image: image.clone(),
                });
                self.bound(Some(&image.url));
                (ArtStatus::Pending, true)
            }
            MapEntry::Occupied(mut o) => match o.get_mut() {
                Entry::Wanted {
                    urgency: u, asked, ..
                } => {
                    *asked = now;
                    if urgency == ArtUrgency::Current {
                        *u = ArtUrgency::Current;
                    }
                    (ArtStatus::Pending, false)
                }
                Entry::InFlight => (ArtStatus::Pending, false),
                Entry::Ready { .. } => {
                    let Entry::Ready { image, .. } = o.remove() else {
                        unreachable!("matched Ready above")
                    };
                    self.ready_bytes -= bytes(&image);
                    (ArtStatus::Ready(image), false)
                }
                Entry::Failed { at } if now.saturating_duration_since(*at) < RETRY_AFTER => {
                    (ArtStatus::Failed, false)
                }
                e @ Entry::Failed { .. } => {
                    *e = Entry::Wanted {
                        urgency,
                        asked: now,
                        image: image.clone(),
                    };
                    (ArtStatus::Pending, true)
                }
            },
        }
    }

    /// The next URL to fetch (marked in flight): Current wants first, then the most recently
    /// asked. Wants not renewed for [`WANT_TTL`] are dropped first.
    pub(crate) fn next(&mut self, now: Instant) -> Option<ImageRef> {
        self.sweep(now);
        let url = self
            .map
            .iter()
            .filter_map(|(url, e)| match e {
                Entry::Wanted { urgency, asked, .. } => {
                    Some(((*urgency == ArtUrgency::Current, *asked), url))
                }
                _ => None,
            })
            .max_by_key(|(key, _)| *key)
            .map(|(_, url)| url.clone())?;
        let entry = self.map.get_mut(&url)?;
        match std::mem::replace(entry, Entry::InFlight) {
            Entry::Wanted { image, .. } => Some(image),
            _ => None,
        }
    }

    /// The result of a fetch of `url`; then the book is trimmed to its bounds.
    pub(crate) fn finish(&mut self, url: String, result: Result<ArtImage, ArtError>, now: Instant) {
        let entry = match result {
            Ok(image) => {
                let image = Arc::new(image);
                self.ready_bytes += bytes(&image);
                Entry::Ready { image, at: now }
            }
            Err(_) => Entry::Failed { at: now },
        };
        if let Some(Entry::Ready { image, .. }) = self.map.insert(url, entry) {
            // Not reachable (a URL in flight is not ready), but keep the count right.
            self.ready_bytes -= bytes(&image);
        }
        self.sweep(now);
        while self.ready_bytes > READY_BUDGET {
            if !self.drop_oldest(|e| match e {
                Entry::Ready { at, .. } => Some(*at),
                _ => None,
            }) {
                break;
            }
        }
        self.bound(None);
    }

    /// Drop wants older than [`WANT_TTL`] and ready images older than [`READY_TTL`].
    fn sweep(&mut self, now: Instant) {
        let mut freed = 0;
        self.map.retain(|_, e| match e {
            Entry::Wanted { asked, .. } => now.saturating_duration_since(*asked) < WANT_TTL,
            Entry::Ready { image, at } => {
                let keep = now.saturating_duration_since(*at) < READY_TTL;
                if !keep {
                    freed += bytes(image);
                }
                keep
            }
            _ => true,
        });
        self.ready_bytes -= freed;
    }

    /// Keep at most [`MAX_BOOK`] entries: drop the oldest failed, then the oldest wanted (never
    /// `keep`), then the oldest ready; a URL in flight is never dropped.
    fn bound(&mut self, keep: Option<&str>) {
        while self.map.len() > MAX_BOOK {
            let dropped = self.drop_oldest(|e| match e {
                Entry::Failed { at } => Some(*at),
                _ => None,
            }) || self.drop_oldest_wanted(keep)
                || self.drop_oldest(|e| match e {
                    Entry::Ready { at, .. } => Some(*at),
                    _ => None,
                });
            if !dropped {
                break;
            }
        }
    }

    fn drop_oldest_wanted(&mut self, keep: Option<&str>) -> bool {
        let oldest = self
            .map
            .iter()
            .filter(|(url, _)| Some(url.as_str()) != keep)
            .filter_map(|(url, e)| match e {
                Entry::Wanted { asked, .. } => Some((*asked, url)),
                _ => None,
            })
            .min_by_key(|(at, _)| *at)
            .map(|(_, url)| url.clone());
        match oldest {
            Some(url) => {
                self.map.remove(&url);
                true
            }
            None => false,
        }
    }

    /// Remove the entry with the smallest time `key` gives; false when none has one.
    fn drop_oldest(&mut self, key: impl Fn(&Entry) -> Option<Instant>) -> bool {
        let oldest = self
            .map
            .iter()
            .filter_map(|(url, e)| key(e).map(|at| (at, url)))
            .min_by_key(|(at, _)| *at)
            .map(|(_, url)| url.clone());
        let Some(url) = oldest else {
            return false;
        };
        if let Some(Entry::Ready { image, .. }) = self.map.remove(&url) {
            self.ready_bytes -= bytes(&image);
        }
        true
    }
}

/// The book and the worker's doorbell, shared by the handles and the worker.
pub(crate) struct ArtHub {
    book: Mutex<ArtBook>,
    notify: Notify,
}

impl ArtHub {
    pub(crate) fn new() -> Self {
        ArtHub {
            book: Mutex::new(ArtBook::default()),
            notify: Notify::new(),
        }
    }
}

impl PlayerHandle {
    /// The cover for `image`, without waiting. Every ask while the answer is `Pending` renews the
    /// core's want; a `Ready` image is handed out once.
    pub fn art(&self, image: &ImageRef, urgency: ArtUrgency) -> ArtStatus {
        let Some(hub) = &self.inner.shared.art else {
            return ArtStatus::Failed;
        };
        let (status, notify) = lock(&hub.book).ask(image, urgency, Instant::now());
        if notify {
            // Stores a permit when the worker is not waiting, so no wake-up is lost.
            hub.notify.notify_one();
        }
        status
    }
}

/// The fetch worker; spawned by `Player::start` when an art source is set. Ends when the control
/// loop has ended (`internal` closed).
pub(crate) async fn worker(
    shared: Arc<Shared>,
    source: Arc<dyn ArtSource>,
    internal: UnboundedSender<Internal>,
) {
    let Some(hub) = shared.art.as_ref() else {
        return;
    };
    let mut running: JoinSet<Result<ArtImage, ArtError>> = JoinSet::new();
    let mut urls: HashMap<tokio::task::Id, String> = HashMap::new();
    loop {
        while running.len() < MAX_IN_FLIGHT {
            let next = lock(&hub.book).next(Instant::now());
            let Some(image) = next else {
                break;
            };
            let source = source.clone();
            let url = image.url.clone();
            let handle = running.spawn(async move { source.fetch(&image).await });
            urls.insert(handle.id(), url);
        }
        tokio::select! {
            _ = internal.closed() => break,
            _ = hub.notify.notified() => {}
            Some(joined) = running.join_next_with_id(), if !running.is_empty() => {
                // A panicking fetch fails only its own URL.
                let (url, result) = match joined {
                    Ok((id, result)) => (urls.remove(&id), result),
                    Err(e) => (urls.remove(&e.id()), Err(ArtError::Unavailable)),
                };
                if let Some(url) = url {
                    lock(&hub.book).finish(url, result, Instant::now());
                }
                shared.wake();
            }
        }
    }
    running.abort_all();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(n: usize) -> ImageRef {
        ImageRef {
            url: format!("https://covers.test/{n}"),
            width: None,
            height: None,
        }
    }

    fn picture(side: u32) -> ArtImage {
        ArtImage::new(side, side, vec![0; (side * side * 4) as usize]).unwrap()
    }

    const S: Duration = Duration::from_secs(1);

    #[test]
    fn b1_pick_order_current_first_then_newest_asked() {
        let t = Instant::now();
        let mut b = ArtBook::default();
        assert!(b.ask(&img(1), ArtUrgency::Visible, t).1);
        assert!(b.ask(&img(2), ArtUrgency::Visible, t + S).1);
        assert!(b.ask(&img(3), ArtUrgency::Current, t).1);
        assert!(b.ask(&img(4), ArtUrgency::Visible, t + 2 * S).1);
        // 1 asked again as Current: upgraded and renewed, no new notify.
        assert_eq!(
            b.ask(&img(1), ArtUrgency::Current, t + 3 * S),
            (ArtStatus::Pending, false)
        );
        let now = t + 3 * S;
        let order: Vec<_> = std::iter::from_fn(|| b.next(now)).map(|i| i.url).collect();
        assert_eq!(order, [img(1).url, img(3).url, img(4).url, img(2).url]);
        // Everything is in flight now: asks stay Pending without a notify.
        assert_eq!(
            b.ask(&img(2), ArtUrgency::Visible, now),
            (ArtStatus::Pending, false)
        );
    }

    #[test]
    fn b2_a_want_not_renewed_for_ten_seconds_is_dropped() {
        let t = Instant::now();
        let mut b = ArtBook::default();
        b.ask(&img(1), ArtUrgency::Visible, t);
        b.ask(&img(2), ArtUrgency::Visible, t);
        b.ask(&img(2), ArtUrgency::Visible, t + 5 * S);
        assert_eq!(b.next(t + 10 * S).map(|i| i.url), Some(img(2).url));
        assert_eq!(b.next(t + 10 * S), None);
        assert!(!b.map.contains_key(&img(1).url));
    }

    #[test]
    fn b3_a_ready_image_is_handed_out_once() {
        let t = Instant::now();
        let mut b = ArtBook::default();
        b.ask(&img(1), ArtUrgency::Current, t);
        let i = b.next(t).unwrap();
        b.finish(i.url, Ok(picture(4)), t);
        assert_eq!(b.ready_bytes, 64);
        assert_eq!(
            b.ask(&img(1), ArtUrgency::Current, t),
            (ArtStatus::Ready(Arc::new(picture(4))), false)
        );
        assert_eq!(b.ready_bytes, 0);
        assert_eq!(
            b.ask(&img(1), ArtUrgency::Current, t),
            (ArtStatus::Pending, true)
        );
    }

    #[test]
    fn b4_ready_ttl_and_budget() {
        let t = Instant::now();
        let mut b = ArtBook::default();
        // 1024² · 4 = 4 MiB each; 18 of them = 72 MiB.
        for n in 0..18 {
            b.ask(&img(n), ArtUrgency::Visible, t);
        }
        for n in 0..18u32 {
            let i = b.next(t).unwrap();
            b.finish(
                i.url,
                Ok(picture(1024)),
                t + Duration::from_millis(n.into()),
            );
        }
        assert!(b.ready_bytes <= READY_BUDGET, "{}", b.ready_bytes);
        assert_eq!(b.ready_bytes, 16 * 4 * 1024 * 1024);
        let ready = |b: &ArtBook| {
            b.map
                .values()
                .filter(|e| matches!(e, Entry::Ready { .. }))
                .count()
        };
        assert_eq!(ready(&b), 16);
        // The two oldest finished went first; asking for them fetches again.
        let finished_first = |b: &ArtBook, n: usize| b.map.contains_key(&img(n).url);
        let first_two_gone = (0..18)
            .filter(|&n| !finished_first(&b, n))
            .collect::<Vec<_>>();
        assert_eq!(first_two_gone.len(), 2);
        // READY_TTL: one more result 30 s later drops every untaken image before it.
        b.ask(&img(100), ArtUrgency::Visible, t + READY_TTL);
        let i = b.next(t + READY_TTL).unwrap();
        b.finish(
            i.url,
            Ok(picture(2)),
            t + READY_TTL + Duration::from_millis(17),
        );
        assert_eq!(ready(&b), 1);
        assert_eq!(b.ready_bytes, 16);
    }

    #[test]
    fn b5_a_failed_url_is_failed_for_five_minutes() {
        let t = Instant::now();
        let mut b = ArtBook::default();
        b.ask(&img(1), ArtUrgency::Visible, t);
        let i = b.next(t).unwrap();
        b.finish(i.url, Err(ArtError::Unavailable), t);
        assert_eq!(
            b.ask(&img(1), ArtUrgency::Visible, t + RETRY_AFTER - S),
            (ArtStatus::Failed, false)
        );
        assert_eq!(
            b.ask(&img(1), ArtUrgency::Visible, t + RETRY_AFTER),
            (ArtStatus::Pending, true)
        );
        assert_eq!(b.next(t + RETRY_AFTER).map(|i| i.url), Some(img(1).url));
    }

    #[test]
    fn b6_the_book_keeps_at_most_max_book_entries() {
        let t = Instant::now();
        let mut b = ArtBook::default();
        // Four in flight, ten failed, the rest wanted.
        for n in 0..14 {
            b.ask(
                &img(n),
                ArtUrgency::Visible,
                t + Duration::from_millis(n as u64),
            );
        }
        let mut flying = vec![];
        for _ in 0..14 {
            flying.push(b.next(t + S).unwrap());
        }
        for i in flying.drain(4..) {
            b.finish(i.url, Err(ArtError::Refused), t + S);
        }
        for n in 14..(MAX_BOOK + 40) {
            let at = t + 2 * S + Duration::from_millis(n as u64);
            b.ask(&img(n), ArtUrgency::Visible, at);
            assert!(b.map.len() <= MAX_BOOK);
        }
        // The failed went first, then the oldest wanted; the four in flight stayed.
        for i in &flying {
            assert!(matches!(b.map.get(&i.url), Some(Entry::InFlight)));
        }
        assert!(!b.map.values().any(|e| matches!(e, Entry::Failed { .. })));
        assert!(b.map.contains_key(&img(MAX_BOOK + 39).url));
        assert!(!b.map.contains_key(&img(14).url));
        assert_eq!(b.map.len(), MAX_BOOK);
    }

    #[test]
    fn b7_a_want_dropped_by_ttl_is_recorded_again_by_the_next_ask() {
        let t = Instant::now();
        let mut b = ArtBook::default();
        assert!(b.ask(&img(1), ArtUrgency::Visible, t).1);
        assert_eq!(b.next(t + WANT_TTL), None);
        assert_eq!(
            b.ask(&img(1), ArtUrgency::Visible, t + WANT_TTL),
            (ArtStatus::Pending, true)
        );
        assert_eq!(b.next(t + WANT_TTL).map(|i| i.url), Some(img(1).url));
    }
}
