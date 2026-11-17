//! A fake [`LyricsSource`] with canned answers by track title.

use std::collections::HashMap;
use std::sync::Mutex;

use whipspot_core::lyrics::{LyricLine, Lyrics, LyricsError, LyricsQuery, LyricsSource};
use whipspot_core::provider::BoxFuture;

use crate::fake_art::Gate;

#[derive(Default)]
struct Inner {
    answers: HashMap<String, Result<Lyrics, LyricsError>>,
    calls: Vec<LyricsQuery>,
    gate: Option<Gate>,
}

/// Answers `Lyrics::None` for every title it was not told about. An empty title or artist is
/// answered at once without being recorded (a real source would not be asked).
#[derive(Default)]
pub struct FakeLyrics {
    inner: Mutex<Inner>,
}

impl FakeLyrics {
    pub fn new() -> Self {
        Self::default()
    }

    /// Answer `Ok(lyrics)` for tracks titled `title`.
    pub fn with(&self, title: &str, lyrics: Lyrics) -> &Self {
        self.inner
            .lock()
            .unwrap()
            .answers
            .insert(title.to_owned(), Ok(lyrics));
        self
    }

    /// Answer `Err(error)` for tracks titled `title`.
    pub fn fail(&self, title: &str, error: LyricsError) -> &Self {
        self.inner
            .lock()
            .unwrap()
            .answers
            .insert(title.to_owned(), Err(error));
        self
    }

    /// Hold every lookup pending until the returned [`Gate`] opens. Asking again returns the
    /// same gate.
    pub fn gate(&self) -> Gate {
        self.inner
            .lock()
            .unwrap()
            .gate
            .get_or_insert_with(Gate::new)
            .clone()
    }

    /// The queries asked, in the order the lookups started.
    pub fn calls(&self) -> Vec<LyricsQuery> {
        self.inner.lock().unwrap().calls.clone()
    }
}

impl LyricsSource for FakeLyrics {
    fn lookup<'a>(&'a self, query: &'a LyricsQuery) -> BoxFuture<'a, Result<Lyrics, LyricsError>> {
        Box::pin(async move {
            if query.title.is_empty() || query.artist.is_empty() {
                return Ok(Lyrics::None);
            }
            let gate = {
                let mut s = self.inner.lock().unwrap();
                s.calls.push(query.clone());
                s.gate.clone()
            };
            if let Some(g) = gate {
                g.wait().await;
            }
            self.inner
                .lock()
                .unwrap()
                .answers
                .get(&query.title)
                .cloned()
                .unwrap_or(Ok(Lyrics::None))
        })
    }
}

const SUBJECTS: [&str; 8] = [
    "the lantern",
    "a quiet harbour",
    "the amber field",
    "my paper boat",
    "the tidal clock",
    "an orchard light",
    "the velvet signal",
    "a copper bell",
];
const VERBS: [&str; 6] = [
    "drifts past",
    "waits beside",
    "hums over",
    "leans toward",
    "turns with",
    "falls into",
];
const OBJECTS: [&str; 8] = [
    "the slow meridian",
    "a glass horizon",
    "the ember road",
    "an empty canyon",
    "the summer wire",
    "a field of salt",
    "the blue hour",
    "our hollow orbit",
];

/// 48 synced lines, 3 s apart from 0, of invented words (no real lyrics).
pub fn demo_lyrics() -> Lyrics {
    Lyrics::Synced(
        (0..48usize)
            .map(|i| LyricLine {
                at_ms: i as u64 * 3000,
                text: format!(
                    "{} {} {}",
                    SUBJECTS[i % SUBJECTS.len()],
                    VERBS[(i / 2) % VERBS.len()],
                    OBJECTS[(i * 3 + i / 8) % OBJECTS.len()]
                ),
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};

    fn query(title: &str, artist: &str) -> LyricsQuery {
        LyricsQuery {
            artist: artist.into(),
            title: title.into(),
            album: String::new(),
            duration_ms: 1000,
        }
    }

    #[test]
    fn demo_lyrics_are_48_timed_lines() {
        let Lyrics::Synced(lines) = demo_lyrics() else {
            panic!("synced expected")
        };
        assert_eq!(lines.len(), 48);
        assert_eq!(lines[0].at_ms, 0);
        assert_eq!(lines[47].at_ms, 47 * 3000);
        assert!(lines.iter().all(|l| !l.text.is_empty()));
    }

    #[test]
    fn gate_holds_and_empty_fields_are_not_recorded() {
        let fake = FakeLyrics::new();
        fake.with("T", demo_lyrics());
        let gate = fake.gate();
        let q = query("T", "A");
        let mut cx = Context::from_waker(Waker::noop());
        let mut f = pin!(fake.lookup(&q));
        assert!(f.as_mut().poll(&mut cx).is_pending());
        assert_eq!(fake.calls(), vec![q.clone()]);
        gate.open();
        assert_eq!(f.as_mut().poll(&mut cx), Poll::Ready(Ok(demo_lyrics())));
        let empty = query("", "A");
        let mut g = pin!(fake.lookup(&empty));
        assert_eq!(g.as_mut().poll(&mut cx), Poll::Ready(Ok(Lyrics::None)));
        assert_eq!(fake.calls().len(), 1);
    }
}
