//! The album art seam (C7): what an art adapter returns and the trait it implements. The core
//! schedules every fetch (see `player::ArtStatus`); an adapter only answers one request.

use std::fmt;
use std::sync::Arc;

use crate::provider::BoxFuture;
use crate::provider::model::ImageRef;

/// The longest side an [`ArtImage`] may have, in pixels.
pub const MAX_ART_SIDE: u32 = 2048;

/// A decoded image: RGBA8, straight alpha, `width * height * 4` bytes.
#[derive(Clone, PartialEq, Eq)]
pub struct ArtImage {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

impl ArtImage {
    /// `None` unless both sides are `1..=MAX_ART_SIDE` and `rgba` holds exactly `w * h * 4` bytes.
    pub fn new(width: u32, height: u32, rgba: Vec<u8>) -> Option<ArtImage> {
        let ok = (1..=MAX_ART_SIDE).contains(&width)
            && (1..=MAX_ART_SIDE).contains(&height)
            && rgba.len() as u64 == u64::from(width) * u64::from(height) * 4;
        ok.then_some(ArtImage {
            width,
            height,
            rgba,
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }
}

impl fmt::Debug for ArtImage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ArtImage({}x{})", self.width, self.height)
    }
}

/// Why an art fetch failed. The texts are fixed and name no provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtError {
    /// The URL or its host is not allowed (the adapter's checks refused it).
    Refused,
    /// The host did not answer (network, status, timeout, redirect).
    Unavailable,
    /// The answer was over a size or dimension cap.
    TooLarge,
    /// The bytes are not an image the decoder accepts.
    Undecodable,
}

impl ArtError {
    pub const fn text(self) -> &'static str {
        match self {
            ArtError::Refused => "this image address is not allowed",
            ArtError::Unavailable => "the image could not be fetched",
            ArtError::TooLarge => "the image is too large",
            ArtError::Undecodable => "the image could not be decoded",
        }
    }
}

impl fmt::Display for ArtError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.text())
    }
}

impl std::error::Error for ArtError {}

/// Fetches and decodes cover art. The [`ImageRef`] is provider data: an implementation checks it
/// itself (host, scheme, caps) before fetching anything.
///
/// Never panics; every failure is an [`ArtError`].
pub trait ArtSource: Send + Sync {
    fn fetch<'a>(&'a self, image: &'a ImageRef) -> BoxFuture<'a, Result<ArtImage, ArtError>>;
}

/// Why [`ArtRouter::register`] refused a scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtRouteError {
    /// Not `^[a-z][a-z0-9-]{0,31}$`.
    BadScheme,
    /// The scheme already has a source.
    Duplicate,
}

impl ArtRouteError {
    pub const fn text(self) -> &'static str {
        match self {
            ArtRouteError::BadScheme => "this image scheme is not valid",
            ArtRouteError::Duplicate => "this image scheme is already registered",
        }
    }
}

impl fmt::Display for ArtRouteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.text())
    }
}

impl std::error::Error for ArtRouteError {}

/// `^[a-z][a-z0-9-]{0,31}$`.
fn is_valid_scheme(scheme: &str) -> bool {
    let mut chars = scheme.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && scheme.len() <= 32
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// An [`ArtSource`] that sends each request to the source registered for the scheme of its
/// [`ImageRef::url`] (the text before the first `:`, compared without case), or to the default
/// source when no scheme matches. With no default, an unmatched ref is [`ArtError::Refused`].
/// The router checks nothing else: the chosen source checks the ref itself.
#[derive(Default)]
pub struct ArtRouter {
    routes: Vec<(String, Arc<dyn ArtSource>)>,
    default: Option<Arc<dyn ArtSource>>,
}

impl ArtRouter {
    pub fn new() -> ArtRouter {
        ArtRouter::default()
    }

    /// The source for refs whose scheme has no registered source (the network adapter).
    pub fn with_default(mut self, source: Arc<dyn ArtSource>) -> ArtRouter {
        self.default = Some(source);
        self
    }

    /// Route `scheme:...` refs to `source`. Refused: a scheme that is not
    /// `^[a-z][a-z0-9-]{0,31}$`, and one already registered.
    pub fn register(
        &mut self,
        scheme: &str,
        source: Arc<dyn ArtSource>,
    ) -> Result<(), ArtRouteError> {
        if !is_valid_scheme(scheme) {
            return Err(ArtRouteError::BadScheme);
        }
        if self.routes.iter().any(|(s, _)| s == scheme) {
            return Err(ArtRouteError::Duplicate);
        }
        self.routes.push((scheme.to_owned(), source));
        Ok(())
    }

    /// The source that serves `url`: the one registered for its scheme, else the default.
    pub fn route(&self, url: &str) -> Option<&Arc<dyn ArtSource>> {
        let scheme = url.split_once(':').map(|(scheme, _)| scheme);
        scheme
            .and_then(|scheme| {
                self.routes
                    .iter()
                    .find(|(s, _)| s.eq_ignore_ascii_case(scheme))
                    .map(|(_, source)| source)
            })
            .or(self.default.as_ref())
    }
}

impl ArtSource for ArtRouter {
    fn fetch<'a>(&'a self, image: &'a ImageRef) -> BoxFuture<'a, Result<ArtImage, ArtError>> {
        match self.route(&image.url) {
            Some(source) => source.fetch(image),
            None => Box::pin(std::future::ready(Err(ArtError::Refused))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn art_image_new_checks_sides_and_length() {
        assert!(ArtImage::new(1, 1, vec![0; 4]).is_some());
        assert!(ArtImage::new(MAX_ART_SIDE, 1, vec![0; MAX_ART_SIDE as usize * 4]).is_some());
        assert!(ArtImage::new(0, 1, vec![]).is_none());
        assert!(ArtImage::new(1, 0, vec![]).is_none());
        assert!(
            ArtImage::new(
                MAX_ART_SIDE + 1,
                1,
                vec![0; (MAX_ART_SIDE as usize + 1) * 4]
            )
            .is_none()
        );
        assert!(ArtImage::new(2, 2, vec![0; 15]).is_none());
        assert!(ArtImage::new(2, 2, vec![0; 17]).is_none());
        // Huge sides are refused before any size arithmetic can overflow.
        assert!(ArtImage::new(u32::MAX, u32::MAX, vec![]).is_none());
        assert!(ArtImage::new(u32::MAX, 1, vec![0; 4]).is_none());
    }

    #[test]
    fn art_image_debug_prints_only_the_size() {
        let i = ArtImage::new(2, 1, vec![9; 8]).unwrap();
        assert_eq!(format!("{i:?}"), "ArtImage(2x1)");
    }

    #[test]
    fn art_error_texts_are_fixed() {
        for e in [
            ArtError::Refused,
            ArtError::Unavailable,
            ArtError::TooLarge,
            ArtError::Undecodable,
        ] {
            assert_eq!(e.to_string(), e.text());
            assert!(!e.text().is_empty());
        }
    }

    /// Answers every fetch with a 1x1 image whose red byte is `tag`, and counts the calls.
    struct Tagged {
        tag: u8,
        calls: std::sync::atomic::AtomicUsize,
    }

    impl Tagged {
        fn new(tag: u8) -> Arc<Tagged> {
            Arc::new(Tagged {
                tag,
                calls: std::sync::atomic::AtomicUsize::new(0),
            })
        }

        fn calls(&self) -> usize {
            self.calls.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    impl ArtSource for Tagged {
        fn fetch<'a>(&'a self, _image: &'a ImageRef) -> BoxFuture<'a, Result<ArtImage, ArtError>> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let image = ArtImage::new(1, 1, vec![self.tag, 0, 0, 255]).ok_or(ArtError::Undecodable);
            Box::pin(std::future::ready(image))
        }
    }

    fn image_ref(url: &str) -> ImageRef {
        ImageRef {
            url: url.to_owned(),
            width: None,
            height: None,
        }
    }

    async fn tag_of(router: &ArtRouter, url: &str) -> Result<u8, ArtError> {
        router.fetch(&image_ref(url)).await.map(|i| i.rgba()[0])
    }

    #[tokio::test]
    async fn router_sends_a_scheme_to_its_source_and_the_rest_to_the_default() {
        let (local, net) = (Tagged::new(1), Tagged::new(2));
        let mut router = ArtRouter::new().with_default(net.clone());
        router.register("local-art", local.clone()).unwrap();
        assert_eq!(tag_of(&router, "local-art:x").await, Ok(1));
        assert_eq!(tag_of(&router, "LOCAL-ART:x").await, Ok(1));
        assert_eq!((local.calls(), net.calls()), (2, 0));
        assert_eq!(
            tag_of(&router, "https://example.invalid/a.jpg").await,
            Ok(2)
        );
        assert_eq!(tag_of(&router, "no-colon-at-all").await, Ok(2));
        assert_eq!(tag_of(&router, "").await, Ok(2));
        assert_eq!((local.calls(), net.calls()), (2, 3));
    }

    #[tokio::test]
    async fn router_without_a_default_refuses_an_unknown_scheme() {
        let local = Tagged::new(1);
        let mut router = ArtRouter::new();
        router.register("local-art", local.clone()).unwrap();
        assert!(router.route("https://example.invalid/a.jpg").is_none());
        assert_eq!(
            tag_of(&router, "https://example.invalid/a.jpg").await,
            Err(ArtError::Refused)
        );
        assert_eq!(tag_of(&router, "local-art:x").await, Ok(1));
        assert_eq!(local.calls(), 1);
        assert_eq!(
            tag_of(&ArtRouter::new(), "local-art:x").await,
            Err(ArtError::Refused)
        );
    }

    #[test]
    fn router_checks_the_scheme_and_refuses_a_duplicate() {
        let source = Tagged::new(1);
        let mut router = ArtRouter::new();
        let long_ok = format!("a{}", "b".repeat(31));
        let too_long = format!("a{}", "b".repeat(32));
        for good in ["a", "local-art", "x9", "a-", long_ok.as_str()] {
            assert_eq!(router.register(good, source.clone()), Ok(()), "{good}");
        }
        for bad in [
            "",
            "Local",
            "1a",
            "-a",
            "a_b",
            "a b",
            "a:b",
            "a/",
            "é",
            too_long.as_str(),
        ] {
            assert_eq!(
                router.register(bad, source.clone()),
                Err(ArtRouteError::BadScheme),
                "{bad:?}"
            );
        }
        assert_eq!(
            router.register("local-art", source.clone()),
            Err(ArtRouteError::Duplicate)
        );
        assert!(!ArtRouteError::BadScheme.text().is_empty());
        assert_eq!(
            ArtRouteError::Duplicate.to_string(),
            ArtRouteError::Duplicate.text()
        );
    }
}
