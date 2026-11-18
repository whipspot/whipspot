//! Registry of providers, routing by provider id or by track-URI scheme.

use std::fmt;
use std::sync::Arc;

use super::Provider;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegisterError {
    InvalidId,
    Duplicate,
    /// `caps.library` disagrees with `library().is_some()`, or `caps.likes` or `caps.explore` is
    /// set without one, or `caps.save_albums` without `likes`.
    LibraryMismatch,
}

impl fmt::Display for RegisterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            RegisterError::InvalidId => "invalid provider id",
            RegisterError::Duplicate => "duplicate provider id",
            RegisterError::LibraryMismatch => "library capability does not match the provider",
        })
    }
}

impl std::error::Error for RegisterError {}

/// `^[a-z][a-z0-9]{0,31}$`.
pub fn is_valid_id(id: &str) -> bool {
    let b = id.as_bytes();
    (1..=32).contains(&b.len())
        && b[0].is_ascii_lowercase()
        && b[1..]
            .iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

/// Providers in registration order; the first one is the default (see [`Self::resolve_uri`]).
#[derive(Default)]
pub struct ProviderRegistry {
    by_id: Vec<Arc<dyn Provider>>,
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, p: Arc<dyn Provider>) -> Result<(), RegisterError> {
        let info = p.info();
        if !is_valid_id(info.id) {
            return Err(RegisterError::InvalidId);
        }
        if self.by_id.iter().any(|q| q.info().id == info.id) {
            return Err(RegisterError::Duplicate);
        }
        let c = info.caps;
        if c.library != p.library().is_some()
            || ((c.likes || c.explore || c.save_albums) && !c.library)
            || (c.save_albums && !c.likes)
        {
            return Err(RegisterError::LibraryMismatch);
        }
        self.by_id.push(p);
        Ok(())
    }

    /// Exact byte match: no trimming, no case folding.
    pub fn get(&self, id: &str) -> Option<&Arc<dyn Provider>> {
        self.by_id.iter().find(|p| p.info().id == id)
    }

    pub fn default_provider(&self) -> Option<&Arc<dyn Provider>> {
        self.by_id.first()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Arc<dyn Provider>> {
        self.by_id.iter()
    }

    /// Every provider's image hosts in registration order, duplicates removed.
    pub fn image_hosts(&self) -> Vec<&'static str> {
        let mut out: Vec<&'static str> = Vec::new();
        for p in &self.by_id {
            for h in p.image_hosts() {
                if !out.contains(h) {
                    out.push(h);
                }
            }
        }
        out
    }

    /// Route a track URI by its scheme (text before the first `:`). A value without a colon
    /// goes to the default provider, whose own validator stays authoritative. An unknown or
    /// malformed scheme fails closed.
    pub fn resolve_uri(&self, uri: &str) -> Option<&Arc<dyn Provider>> {
        match uri.split_once(':') {
            None => self.default_provider(),
            Some((scheme, _)) => self.get(scheme),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::fake::FakeProvider;

    fn reg(ids: &[&'static str]) -> ProviderRegistry {
        let mut r = ProviderRegistry::new();
        for id in ids {
            r.register(Arc::new(FakeProvider::new(id, true))).unwrap();
        }
        r
    }

    #[test]
    fn get_is_exact() {
        let r = reg(&["spotify"]);
        assert!(r.get("spotify").is_some());
        for bad in ["", " spotify", "SPOTIFY", "spotify\n", "spotify ", "spot"] {
            assert!(r.get(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn register_rejects_bad_ids_and_duplicates() {
        let mut r = ProviderRegistry::new();
        for bad in ["", "Spotify", "1abc", "a-b", "a_b", "a b", &"a".repeat(33)] {
            // Leak is fine in a test: ids are &'static str.
            let id: &'static str = Box::leak(bad.to_owned().into_boxed_str());
            assert_eq!(
                r.register(Arc::new(FakeProvider::new(id, true)))
                    .unwrap_err(),
                RegisterError::InvalidId,
                "{bad:?}"
            );
        }
        r.register(Arc::new(FakeProvider::new("a", true))).unwrap();
        r.register(Arc::new(FakeProvider::new("a".repeat(32).leak(), true)))
            .unwrap();
        assert_eq!(
            r.register(Arc::new(FakeProvider::new("a", false)))
                .unwrap_err(),
            RegisterError::Duplicate
        );
    }

    #[test]
    fn register_checks_library_capability() {
        let mut r = ProviderRegistry::new();
        let p = FakeProvider::new("x", true).with_caps_library(false);
        assert_eq!(
            r.register(Arc::new(p)).unwrap_err(),
            RegisterError::LibraryMismatch
        );
    }

    #[test]
    fn register_refuses_likes_without_a_library() {
        let mut r = ProviderRegistry::new();
        let p = FakeProvider::new("x", false).with_caps_likes(true);
        assert_eq!(
            r.register(Arc::new(p)).unwrap_err(),
            RegisterError::LibraryMismatch
        );
        let ok = FakeProvider::new("y", true).with_caps_likes(true);
        r.register(Arc::new(ok)).unwrap();
    }

    #[test]
    fn register_refuses_explore_and_save_albums_without_their_base() {
        let mut r = ProviderRegistry::new();
        let no_lib = FakeProvider::new("a", false).with_caps_explore(true);
        assert_eq!(
            r.register(Arc::new(no_lib)).unwrap_err(),
            RegisterError::LibraryMismatch
        );
        let no_lib = FakeProvider::new("b", false).with_caps_save_albums(true);
        assert_eq!(
            r.register(Arc::new(no_lib)).unwrap_err(),
            RegisterError::LibraryMismatch
        );
        // save_albums needs likes.
        let no_likes = FakeProvider::new("c", true).with_caps_save_albums(true);
        assert_eq!(
            r.register(Arc::new(no_likes)).unwrap_err(),
            RegisterError::LibraryMismatch
        );
        let ok = FakeProvider::new("d", true)
            .with_caps_likes(true)
            .with_caps_save_albums(true)
            .with_caps_explore(true);
        r.register(Arc::new(ok)).unwrap();
    }

    #[test]
    fn image_hosts_keep_the_first_of_a_duplicate() {
        let mut r = ProviderRegistry::new();
        assert!(r.image_hosts().is_empty());
        r.register(Arc::new(
            FakeProvider::new("a", true).with_image_hosts(&["one.test", "two.test"]),
        ))
        .unwrap();
        r.register(Arc::new(
            FakeProvider::new("b", true).with_image_hosts(&["two.test", "three.test"]),
        ))
        .unwrap();
        assert_eq!(r.image_hosts(), vec!["one.test", "two.test", "three.test"]);
    }

    #[test]
    fn resolve_uri_table() {
        let r = reg(&["spotify", "tidal"]);
        let id = |u: &str| r.resolve_uri(u).map(|p| p.info().id);
        assert_eq!(id("spotify:track:abc"), Some("spotify"));
        assert_eq!(id("tidal:track:1"), Some("tidal"));
        assert_eq!(id("0123456789012345678901"), Some("spotify")); // scheme-less -> default
        assert_eq!(id(""), Some("spotify"));
        for bad in [
            "nope:track:x",
            "Spotify:track:x",
            " spotify:track:x",
            ":track:x",
            "spotify :x",
        ] {
            assert_eq!(id(bad), None, "{bad:?}");
        }
        assert!(
            ProviderRegistry::new()
                .resolve_uri("spotify:track:x")
                .is_none()
        );
        assert!(ProviderRegistry::new().resolve_uri("x").is_none());
    }

    #[test]
    fn two_providers_resolve_independently() {
        let r = reg(&["one", "two"]);
        assert_eq!(r.get("one").unwrap().info().id, "one");
        assert_eq!(r.get("two").unwrap().info().id, "two");
        assert_eq!(r.iter().count(), 2);
    }

    #[test]
    fn provider_without_library_has_none() {
        let p = FakeProvider::new("nolib", false);
        assert!(p.library().is_none());
        let mut r = ProviderRegistry::new();
        r.register(Arc::new(p)).unwrap();
    }
}
