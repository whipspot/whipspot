//! A fake [`ArtSource`] that renders synthetic covers (see [`crate::cover`]) for demo URLs, plus
//! the runtime-free [`Gate`] the fakes use to hold an answer pending in a test.

use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use whipspot_core::art::{ArtError, ArtImage, ArtSource, MAX_ART_SIDE};
use whipspot_core::provider::{BoxFuture, ImageRef};

use crate::cover;

/// The only host [`FakeArt`] accepts (`.invalid` never resolves).
const PREFIX: &str = "https://covers.demo.invalid/";

/// The sizes [`demo_images`] offers, largest first.
const DEMO_SIZES: [u32; 3] = [640, 300, 64];

/// The image refs of a demo item: `https://covers.demo.invalid/<key>/<size>` at 640, 300 and 64.
pub fn demo_images(key: &str) -> Vec<ImageRef> {
    DEMO_SIZES
        .iter()
        .map(|&s| ImageRef {
            url: format!("{PREFIX}{key}/{s}"),
            width: Some(s),
            height: Some(s),
        })
        .collect()
}

#[derive(Default)]
struct GateState {
    open: bool,
    wakers: Vec<Waker>,
}

/// A latch shared by clones: [`Gate::wait`] is pending until [`Gate::open`]. A future with no
/// runtime: it stores the wakers and wakes them on open.
#[derive(Clone, Default)]
pub struct Gate {
    state: Arc<Mutex<GateState>>,
}

impl Gate {
    pub fn new() -> Self {
        Self::default()
    }

    /// Release every waiter, now and later.
    pub fn open(&self) {
        let wakers = {
            let mut s = self.state.lock().unwrap();
            s.open = true;
            std::mem::take(&mut s.wakers)
        };
        for w in wakers {
            w.wake();
        }
    }

    pub fn is_open(&self) -> bool {
        self.state.lock().unwrap().open
    }

    /// Pending until the gate opens.
    pub fn wait(&self) -> GateWait {
        GateWait {
            state: self.state.clone(),
        }
    }
}

/// The future of [`Gate::wait`].
pub struct GateWait {
    state: Arc<Mutex<GateState>>,
}

impl Future for GateWait {
    type Output = ();
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let mut s = self.state.lock().unwrap();
        if s.open {
            return Poll::Ready(());
        }
        if !s.wakers.iter().any(|w| w.will_wake(cx.waker())) {
            s.wakers.push(cx.waker().clone());
        }
        Poll::Pending
    }
}

#[derive(Default)]
struct Inner {
    calls: Vec<String>,
    failing: HashSet<String>,
    gate: Option<Gate>,
    in_flight: usize,
    max_in_flight: usize,
}

/// Renders `https://covers.demo.invalid/<key>/<size>` (size `1..=2048`) as
/// `cover::render(key, size)`; every other URL is `Refused`. Records the URLs it is asked for.
#[derive(Default)]
pub struct FakeArt {
    inner: Mutex<Inner>,
    /// Every cover is the near-white paper one ([`cover::render_paper`]).
    paper: bool,
}

impl FakeArt {
    pub fn new() -> Self {
        Self::default()
    }

    /// A source whose covers are all near-white (the demo's light-cover legibility shot).
    pub fn paper() -> Self {
        Self {
            paper: true,
            ..Self::default()
        }
    }

    /// Make the fetch of `url` fail with `Unavailable`.
    pub fn fail(&self, url: &str) {
        self.inner.lock().unwrap().failing.insert(url.to_owned());
    }

    /// Hold every fetch pending until the returned [`Gate`] opens. Asking again returns the
    /// same gate.
    pub fn gate(&self) -> Gate {
        self.inner
            .lock()
            .unwrap()
            .gate
            .get_or_insert_with(Gate::new)
            .clone()
    }

    /// The URLs asked for, in the order the fetches started.
    pub fn calls(&self) -> Vec<String> {
        self.inner.lock().unwrap().calls.clone()
    }

    /// The most fetches that were running at once.
    pub fn max_in_flight(&self) -> usize {
        self.inner.lock().unwrap().max_in_flight
    }
}

/// `key` and `size` of a demo cover URL, `None` for anything else.
fn parse(url: &str) -> Option<(&str, u32)> {
    let (key, size) = url.strip_prefix(PREFIX)?.split_once('/')?;
    if key.is_empty() || size.is_empty() || !size.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let size: u32 = size.parse().ok()?;
    (1..=MAX_ART_SIDE).contains(&size).then_some((key, size))
}

/// Counts a running fetch; dropping the future (cancellation) counts it out.
struct Running<'a>(&'a Mutex<Inner>);

impl Drop for Running<'_> {
    fn drop(&mut self) {
        self.0.lock().unwrap().in_flight -= 1;
    }
}

impl ArtSource for FakeArt {
    fn fetch<'a>(&'a self, image: &'a ImageRef) -> BoxFuture<'a, Result<ArtImage, ArtError>> {
        Box::pin(async move {
            let gate = {
                let mut s = self.inner.lock().unwrap();
                s.calls.push(image.url.clone());
                s.in_flight += 1;
                s.max_in_flight = s.max_in_flight.max(s.in_flight);
                s.gate.clone()
            };
            let _running = Running(&self.inner);
            if let Some(g) = gate {
                g.wait().await;
            }
            let (key, size) = parse(&image.url).ok_or(ArtError::Refused)?;
            if self.inner.lock().unwrap().failing.contains(&image.url) {
                return Err(ArtError::Unavailable);
            }
            let rgba = if self.paper {
                cover::render_paper(key, size)
            } else {
                cover::render(key, size)
            };
            ArtImage::new(size, size, rgba).ok_or(ArtError::Undecodable)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::pin::pin;

    fn poll_once<F: Future>(f: &mut Pin<&mut F>) -> Poll<F::Output> {
        f.as_mut().poll(&mut Context::from_waker(Waker::noop()))
    }

    fn img(url: &str) -> ImageRef {
        ImageRef {
            url: url.into(),
            width: None,
            height: None,
        }
    }

    #[test]
    fn parse_accepts_only_demo_urls() {
        assert_eq!(parse("https://covers.demo.invalid/a/64"), Some(("a", 64)));
        assert_eq!(
            parse("https://covers.demo.invalid/a/2048"),
            Some(("a", 2048))
        );
        for bad in [
            "https://covers.demo.invalid/a/0",
            "https://covers.demo.invalid/a/2049",
            "https://covers.demo.invalid/a/+5",
            "https://covers.demo.invalid//5",
            "https://covers.demo.invalid/a",
            "http://covers.demo.invalid/a/5",
            "https://evil.example/a/5",
            "https://covers.demo.invalid.evil.example/a/5",
        ] {
            assert_eq!(parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn gate_holds_a_fetch_until_opened_and_counts_it() {
        let art = FakeArt::new();
        let gate = art.gate();
        let image = demo_images("k").pop().unwrap();
        let mut fut = pin!(art.fetch(&image));
        assert!(poll_once(&mut fut).is_pending());
        assert_eq!(art.max_in_flight(), 1);
        assert_eq!(art.calls(), vec![image.url.clone()]);
        gate.open();
        let Poll::Ready(Ok(done)) = poll_once(&mut fut) else {
            panic!("the opened gate lets the fetch finish")
        };
        assert_eq!((done.width(), done.height()), (64, 64));
        assert_eq!(done.rgba(), cover::render("k", 64).as_slice());
    }

    #[test]
    fn refused_and_failed_urls() {
        let art = FakeArt::new();
        let bad = img("https://evil.example/x");
        let mut a = pin!(art.fetch(&bad));
        assert_eq!(poll_once(&mut a), Poll::Ready(Err(ArtError::Refused)));
        let good = demo_images("k").pop().unwrap();
        art.fail(&good.url);
        let mut b = pin!(art.fetch(&good));
        assert_eq!(poll_once(&mut b), Poll::Ready(Err(ArtError::Unavailable)));
    }
}
