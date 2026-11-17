//! The traits a skin adapter implements and the registry that picks an engine and loads.

use std::sync::Arc;

use super::{MAX_SKIN_BYTES, Skin, SkinError};
use crate::text::display_text;

/// Turns the bytes of one skin file into a [`Skin`]. Engines are pure: no file system, no
/// network, no global state.
pub trait SkinEngine: Send + Sync {
    /// A stable internal id, `^[a-z][a-z0-9-]{0,31}$`; never shown as UI text.
    fn id(&self) -> &'static str;
    /// The lowercase file extensions the engine reads, each `^[a-z0-9]{1,8}$`; not empty.
    fn extensions(&self) -> &'static [&'static str];
    /// A cheap check of whether `bytes` look like this engine's format. Never panics.
    fn detect(&self, bytes: &[u8]) -> bool;
    /// Builds the model. Never panics on any input; `Err(TooLarge)` above `MAX_SKIN_BYTES`.
    /// The registry fills `info` and runs `Skin::validate` on the result.
    fn load(&self, bytes: &[u8]) -> Result<Skin, SkinError>;
}

/// Where skin files come from. Names are file names, never paths.
pub trait SkinSource: Send + Sync {
    /// The names `read` accepts, sorted and capped.
    fn list(&self) -> Vec<String>;
    /// The bytes of one skin; at most `MAX_SKIN_BYTES`.
    fn read(&self, name: &str) -> Result<Vec<u8>, SkinError>;
}

/// `^[a-z][a-z0-9-]{0,31}$`.
fn is_valid_engine_id(id: &str) -> bool {
    let b = id.as_bytes();
    (1..=32).contains(&b.len())
        && b[0].is_ascii_lowercase()
        && b[1..]
            .iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

/// `^[a-z0-9]{1,8}$`.
fn is_valid_extension(ext: &str) -> bool {
    (1..=8).contains(&ext.len())
        && ext
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

/// Engines in registration order. Among the engines registered for a file's extension, the
/// first whose `detect` is true loads it.
#[derive(Default)]
pub struct SkinRegistry {
    engines: Vec<Arc<dyn SkinEngine>>,
}

impl SkinRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// `InvalidName` for a bad or duplicate engine id, or an empty or bad extension list.
    pub fn register(&mut self, engine: Arc<dyn SkinEngine>) -> Result<(), SkinError> {
        let id = engine.id();
        let exts = engine.extensions();
        if !is_valid_engine_id(id)
            || exts.is_empty()
            || !exts.iter().all(|e| is_valid_extension(e))
            || self.engines.iter().any(|e| e.id() == id)
        {
            return Err(SkinError::InvalidName);
        }
        self.engines.push(engine);
        Ok(())
    }

    /// Every registered extension, lowercase, in registration order, duplicates removed.
    pub fn extensions(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for ext in self.engines.iter().flat_map(|e| e.extensions()) {
            if !out.iter().any(|o| o == ext) {
                out.push((*ext).to_owned());
            }
        }
        out
    }

    /// The first engine registered for `name`'s extension (the part after the last `.`, matched
    /// case-insensitively) whose `detect` accepts `bytes`.
    pub fn engine_for(&self, name: &str, bytes: &[u8]) -> Option<&dyn SkinEngine> {
        let (_, ext) = name.rsplit_once('.')?;
        self.engines
            .iter()
            .find(|e| e.extensions().iter().any(|x| x.eq_ignore_ascii_case(ext)) && e.detect(bytes))
            .map(|e| e.as_ref())
    }

    /// Reads `name`, picks the engine, loads, fills `info` and validates. Fails closed: an engine
    /// that returns a model breaking a `validate` rule gives `Malformed`.
    pub fn load(&self, source: &dyn SkinSource, name: &str) -> Result<Skin, SkinError> {
        let bytes = source.read(name)?;
        if bytes.len() as u64 > MAX_SKIN_BYTES {
            return Err(SkinError::TooLarge);
        }
        let engine = self
            .engine_for(name, &bytes)
            .ok_or(SkinError::UnknownFormat)?;
        let mut skin = engine.load(&bytes)?;
        skin.info.engine = engine.id().to_owned();
        let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
        skin.info.name = display_text(stem, 100);
        skin.validate()?;
        Ok(skin)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::skin::validate::minimal_skin;

    struct TestEngine {
        id: &'static str,
        exts: &'static [&'static str],
        detects: bool,
        result: Mutex<Option<Result<Skin, SkinError>>>,
    }

    impl TestEngine {
        fn new(id: &'static str, exts: &'static [&'static str], detects: bool) -> TestEngine {
            TestEngine {
                id,
                exts,
                detects,
                result: Mutex::new(None),
            }
        }

        fn returning(self, r: Result<Skin, SkinError>) -> TestEngine {
            *self.result.lock().unwrap() = Some(r);
            self
        }
    }

    impl SkinEngine for TestEngine {
        fn id(&self) -> &'static str {
            self.id
        }
        fn extensions(&self) -> &'static [&'static str] {
            self.exts
        }
        fn detect(&self, _bytes: &[u8]) -> bool {
            self.detects
        }
        fn load(&self, _bytes: &[u8]) -> Result<Skin, SkinError> {
            self.result
                .lock()
                .unwrap()
                .clone()
                .unwrap_or_else(|| Ok(minimal_skin()))
        }
    }

    struct TestSource(Vec<u8>);

    impl SkinSource for TestSource {
        fn list(&self) -> Vec<String> {
            vec!["a.skn".to_owned()]
        }
        fn read(&self, _name: &str) -> Result<Vec<u8>, SkinError> {
            Ok(self.0.clone())
        }
    }

    fn registry(e: TestEngine) -> SkinRegistry {
        let mut r = SkinRegistry::new();
        r.register(Arc::new(e)).unwrap();
        r
    }

    #[test]
    fn register_rejects_bad_ids_extensions_and_duplicates() {
        let mut r = SkinRegistry::new();
        r.register(Arc::new(TestEngine::new("one", &["skn"], true)))
            .unwrap();
        let bad = [
            TestEngine::new("one", &["abc"], true),
            TestEngine::new("", &["abc"], true),
            TestEngine::new("One", &["abc"], true),
            TestEngine::new("1one", &["abc"], true),
            TestEngine::new("a_b", &["abc"], true),
            TestEngine::new("a-b", &["SKN"], true),
            TestEngine::new("a-b", &[""], true),
            TestEngine::new("a-b", &["abcdefghi"], true),
            TestEngine::new("a-b", &["a.b"], true),
            TestEngine::new("a-b", &[], true),
        ];
        for e in bad {
            let id = e.id;
            assert_eq!(r.register(Arc::new(e)), Err(SkinError::InvalidName), "{id}");
        }
        assert_eq!(r.extensions(), vec!["skn"]);
    }

    #[test]
    fn extensions_keep_registration_order_without_duplicates() {
        let mut r = SkinRegistry::new();
        r.register(Arc::new(TestEngine::new("a", &["zzz", "skn"], true)))
            .unwrap();
        r.register(Arc::new(TestEngine::new("b", &["skn", "abc"], true)))
            .unwrap();
        assert_eq!(r.extensions(), vec!["zzz", "skn", "abc"]);
    }

    #[test]
    fn engine_for_matches_extension_case_insensitively_then_detect() {
        let mut r = SkinRegistry::new();
        r.register(Arc::new(TestEngine::new("no", &["skn"], false)))
            .unwrap();
        r.register(Arc::new(TestEngine::new("yes", &["skn", "alt"], true)))
            .unwrap();
        assert_eq!(r.engine_for("x.SKN", b"").map(|e| e.id()), Some("yes"));
        assert_eq!(r.engine_for("x.y.Alt", b"").map(|e| e.id()), Some("yes"));
        assert!(r.engine_for("x.zip", b"").is_none());
        assert!(r.engine_for("skn", b"").is_none());
        assert!(r.engine_for("x.", b"").is_none());
    }

    #[test]
    fn load_fills_info_and_validates() {
        let r = registry(TestEngine::new("test-eng", &["skn"], true));
        let skin = r.load(&TestSource(vec![1, 2, 3]), "My Skin.SKN").unwrap();
        assert_eq!(skin.info.engine, "test-eng");
        assert_eq!(skin.info.name, "My Skin");
        let named = r.load(&TestSource(vec![0]), "a.b.skn").unwrap();
        assert_eq!(named.info.name, "a.b");
    }

    #[test]
    fn load_unknown_format_when_detect_is_false_or_extension_unknown() {
        let r = registry(TestEngine::new("test-eng", &["skn"], false));
        assert_eq!(
            r.load(&TestSource(vec![1]), "a.skn").unwrap_err(),
            SkinError::UnknownFormat
        );
        let r = registry(TestEngine::new("test-eng", &["skn"], true));
        assert_eq!(
            r.load(&TestSource(vec![1]), "a.other").unwrap_err(),
            SkinError::UnknownFormat
        );
    }

    #[test]
    fn load_too_large_from_the_source() {
        let r = registry(TestEngine::new("test-eng", &["skn"], true));
        let big = vec![0u8; MAX_SKIN_BYTES as usize + 1];
        assert_eq!(
            r.load(&TestSource(big), "a.skn").unwrap_err(),
            SkinError::TooLarge
        );
        let at_cap = vec![0u8; MAX_SKIN_BYTES as usize];
        assert!(r.load(&TestSource(at_cap), "a.skn").is_ok());
    }

    #[test]
    fn load_fails_closed_on_an_invalid_model() {
        let mut bad = minimal_skin();
        bad.panels.clear();
        let r = registry(TestEngine::new("test-eng", &["skn"], true).returning(Ok(bad)));
        assert_eq!(
            r.load(&TestSource(vec![1]), "a.skn").unwrap_err(),
            SkinError::Malformed
        );
    }

    #[test]
    fn load_passes_engine_errors_through() {
        let r = registry(
            TestEngine::new("test-eng", &["skn"], true).returning(Err(SkinError::Unsupported)),
        );
        assert_eq!(
            r.load(&TestSource(vec![1]), "a.skn").unwrap_err(),
            SkinError::Unsupported
        );
    }

    #[test]
    fn error_texts_are_fixed_and_distinct() {
        let all = [
            SkinError::InvalidName,
            SkinError::NotFound,
            SkinError::NotAllowed,
            SkinError::TooLarge,
            SkinError::Io,
            SkinError::UnknownFormat,
            SkinError::Malformed,
            SkinError::Unsupported,
        ];
        let texts: Vec<String> = all.iter().map(|e| e.to_string()).collect();
        for (i, a) in texts.iter().enumerate() {
            assert!(!a.is_empty());
            for b in &texts[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }
}
