//! Lyrics lookups for the current item (MD67, MD68). Owned by the control loop: at most one
//! lookup in flight, the newest waiting query replacing older ones, a session cache, and an
//! answer published only while its item is current. The loop spawns the lookups decided here.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use super::state::LyricsState;
use crate::lyrics::{Lyrics, LyricsError, LyricsQuery, LyricsSource};

/// Answers kept for the session; the oldest is dropped first.
pub(crate) const LYRICS_CACHE: usize = 256;

/// One lookup: `uri` was current when it was asked.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Lookup {
    pub(crate) uri: String,
    pub(crate) query: LyricsQuery,
}

/// What the loop does next: publish a state, start a lookup, or both.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Step {
    pub(crate) publish: Option<LyricsState>,
    pub(crate) start: Option<Lookup>,
}

pub(crate) struct LyricsLookups {
    source: Option<Arc<dyn LyricsSource>>,
    enabled: bool,
    cache: HashMap<LyricsQuery, Arc<Lyrics>>,
    /// Cache keys, oldest first.
    order: VecDeque<LyricsQuery>,
    in_flight: bool,
    /// The newest lookup waiting for the running one to end.
    pending: Option<Lookup>,
}

impl LyricsLookups {
    /// `enabled` counts only with a source.
    pub(crate) fn new(source: Option<Arc<dyn LyricsSource>>, enabled: bool) -> Self {
        LyricsLookups {
            enabled: enabled && source.is_some(),
            source,
            cache: HashMap::new(),
            order: VecDeque::new(),
            in_flight: false,
            pending: None,
        }
    }

    pub(crate) fn source(&self) -> Option<&Arc<dyn LyricsSource>> {
        self.source.as_ref()
    }

    pub(crate) fn enabled(&self) -> bool {
        self.enabled
    }

    /// The state while nothing is current.
    pub(crate) fn idle(&self) -> LyricsState {
        if self.enabled {
            LyricsState::Idle
        } else {
            LyricsState::Off
        }
    }

    /// Switch lookups on or off (on only with a source). Off drops the waiting lookup; an
    /// answer in flight is still cached, not shown.
    pub(crate) fn set_enabled(&mut self, on: bool) {
        self.enabled = on && self.source.is_some();
        if !self.enabled {
            self.pending = None;
        }
    }

    /// `lookup.uri` became current: the cache answers at once, else it is looked up now or
    /// waits (replacing any waiting one) while another lookup runs.
    pub(crate) fn current(&mut self, lookup: Lookup) -> Step {
        if !self.enabled {
            return Step::default();
        }
        if let Some(lyrics) = self.cache.get(&lookup.query) {
            self.pending = None;
            return Step {
                publish: Some(LyricsState::Found {
                    uri: lookup.uri,
                    lyrics: lyrics.clone(),
                }),
                start: None,
            };
        }
        let publish = Some(LyricsState::Searching {
            uri: lookup.uri.clone(),
        });
        if self.in_flight {
            self.pending = Some(lookup);
            return Step {
                publish,
                start: None,
            };
        }
        self.in_flight = true;
        Step {
            publish,
            start: Some(lookup),
        }
    }

    /// The running lookup ended. Its answer is sanitised and cached when Ok (errors are not),
    /// shown only while lookups are on and `done.uri` is `current`; then the waiting lookup, if
    /// its item is still current, runs.
    pub(crate) fn answered(
        &mut self,
        done: Lookup,
        result: Result<Lyrics, LyricsError>,
        current: Option<&str>,
    ) -> Step {
        self.in_flight = false;
        let result = result.map(|l| Arc::new(l.sanitized()));
        if let Ok(l) = &result {
            self.remember(done.query.clone(), l.clone());
        }
        if self.enabled && current == Some(done.uri.as_str()) {
            // The waiting lookup, if any, was for this same item (only one item is current).
            self.pending = None;
            return Step {
                publish: Some(match result {
                    Ok(lyrics) => LyricsState::Found {
                        uri: done.uri,
                        lyrics,
                    },
                    Err(_) => LyricsState::Failed { uri: done.uri },
                }),
                start: None,
            };
        }
        match self.pending.take() {
            Some(p) if current == Some(p.uri.as_str()) => self.current(p),
            _ => Step::default(),
        }
    }

    fn remember(&mut self, query: LyricsQuery, lyrics: Arc<Lyrics>) {
        if self.cache.insert(query.clone(), lyrics).is_none() {
            self.order.push_back(query);
            while self.order.len() > LYRICS_CACHE {
                if let Some(old) = self.order.pop_front() {
                    self.cache.remove(&old);
                }
            }
        }
    }
}
