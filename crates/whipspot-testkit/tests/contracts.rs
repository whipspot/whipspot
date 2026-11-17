//! Contract runs on the testkit fakes. The fakes never wait, so a no-op waker is enough to drive
//! the futures; a pending future is a failure.

use std::{
    future::Future,
    pin::pin,
    sync::Arc,
    task::{Context, Poll, Waker},
};

use whipspot_core::art::{ArtError, ArtRouteError, ArtRouter, ArtSource};
use whipspot_core::credential_store::{CredentialStore, StoreError};
use whipspot_core::engine::{DeviceFormat, OutputMode, OutputRequest, SampleFormat};
use whipspot_core::folders::FolderError;
use whipspot_core::lyrics::{LyricLine, Lyrics, LyricsError, LyricsQuery};
use whipspot_core::player::{EngineSetup, wiring};
use whipspot_core::provider::{
    AuthFailure, AuthFlow, BoxFuture, ImageRef, LibErrorKind, Library, LoginInput, Playback,
    Provider, ProviderAuthStatus, ProviderInfo,
};
use whipspot_core::secret::Secret;
use whipspot_core::skin::{
    MAX_SKIN_BYTES, PanelWindows, ScreenPoint, ScreenRect, Skin, SkinEngine, SkinError, SkinSource,
};
use whipspot_testkit::{
    ClockedOutput, DemoProvider, FakeArt, FakeFolders, FakeLyrics, FakeOutput, FakeProvider,
    FakeSkinEngine, FakeSkinSource, FakeWindows, LyricsOutcome, MemoryStore, demo_images,
    demo_lyrics, run_art_error_contract, run_art_source_contract, run_audio_output_contract,
    run_credential_store_contract, run_fake_windows_contract, run_lyrics_outcome_contract,
    run_lyrics_source_contract, run_music_folders_contract, run_panel_windows_contract,
    run_provider_contract, run_provider_playback_contract, run_provider_session_contract,
    run_remote_contract, run_skin_caps_contract, run_skin_engine_contract,
    run_skin_source_contract, run_skin_source_size_contract,
};

fn block_on<F: Future>(f: F) -> F::Output {
    let mut f = pin!(f);
    let mut cx = Context::from_waker(Waker::noop());
    match f.as_mut().poll(&mut cx) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("contract future is pending, but the fakes never wait"),
    }
}

#[test]
fn provider_contract_on_fake() {
    block_on(run_provider_contract(Arc::new(FakeProvider::new(
        "fake", true,
    ))));
}

#[test]
fn remote_contract_on_fake() {
    block_on(run_remote_contract(Arc::new(
        FakeProvider::new("fake", true).with_remote("fake.remote"),
    )));
}

#[test]
#[should_panic(expected = "remote() is Some")]
fn remote_contract_fails_a_provider_without_a_remote() {
    block_on(run_remote_contract(Arc::new(FakeProvider::new(
        "fake", true,
    ))));
}

#[test]
#[should_panic(expected = "not the provider's id")]
fn remote_contract_fails_a_remote_writing_as_its_provider() {
    block_on(run_remote_contract(Arc::new(
        FakeProvider::new("fake", true).with_remote("fake"),
    )));
}

#[test]
fn credential_store_contract_on_memory_store() {
    run_credential_store_contract(&MemoryStore::default());
}

#[test]
fn art_source_contract_on_fake_art() {
    let good = demo_images("contract").remove(2);
    let refused = ImageRef {
        url: "https://evil.example/x".into(),
        width: None,
        height: None,
    };
    block_on(run_art_source_contract(&FakeArt::new(), &good, &refused));
}

#[test]
fn lyrics_source_contract_on_fake_lyrics() {
    let fake = FakeLyrics::new();
    fake.with("Hit", demo_lyrics());
    let query = |title: &str| LyricsQuery {
        artist: "Artist".into(),
        title: title.into(),
        album: "Album".into(),
        duration_ms: 200_000,
    };
    block_on(run_lyrics_source_contract(
        &fake,
        &query("Hit"),
        &query("Miss"),
    ));
}

fn demo_with(logged_in: bool, read_only: bool) -> DemoProvider {
    let fake = FakeOutput::new(
        DeviceFormat {
            rate: 44_100,
            channels: 2,
            sample: SampleFormat::F32,
            valid_bits: 32,
        },
        OutputMode::Shared,
    );
    let setup = EngineSetup::Output {
        output: Box::new(fake),
        request: OutputRequest::default(),
    };
    let (ctx, _wiring) = wiring(setup, 8);
    let d = DemoProvider::new(ctx, logged_in);
    if read_only { d.read_only_likes() } else { d }
}

fn demo(read_only: bool) -> DemoProvider {
    demo_with(true, read_only)
}

#[test]
fn demo_carries_covers_years_and_in_memory_likes() {
    let d = demo(false);
    assert!(d.info().caps.likes);
    let lib = d.library().unwrap();
    assert!(lib.can_change_likes());
    let albums = block_on(lib.albums(0)).unwrap().items;
    for (i, a) in albums.iter().enumerate() {
        assert_eq!(a.images, demo_images(&a.id), "album {i}");
        assert!(
            a.year.is_some_and(|y| (1990..=2024).contains(&y)),
            "album {i}"
        );
    }
    let tracks = block_on(lib.album_tracks(&albums[3].id, 0)).unwrap().items;
    assert!(
        tracks
            .iter()
            .all(|t| t.images == albums[3].images && t.year == albums[3].year)
    );
    let playlists = block_on(lib.playlists(0)).unwrap().items;
    assert_eq!(playlists[0].images, demo_images(&playlists[0].id));

    let liked = block_on(lib.liked_tracks(0)).unwrap().items;
    let uris: Vec<String> = vec![liked[0].uri.clone(), "demo:track:999".into()];
    assert_eq!(block_on(lib.liked(&uris)).unwrap(), vec![true, false]);
    block_on(lib.set_liked(&uris[1], true)).unwrap();
    block_on(lib.set_liked(&uris[0], false)).unwrap();
    assert_eq!(block_on(lib.liked(&uris)).unwrap(), vec![false, true]);
}

#[test]
fn read_only_demo_refuses_like_changes() {
    let d = demo(true);
    let lib = d.library().unwrap();
    assert!(!lib.can_change_likes());
    let e = block_on(lib.set_liked("demo:track:1", true)).unwrap_err();
    assert_eq!(e.kind, LibErrorKind::AccessDenied);
}

#[test]
fn fake_provider_likes_log_and_fail_modes() {
    let p = FakeProvider::new("fake", true).with_likes(&["a"], true);
    assert!(p.info().caps.likes);
    let lib = p.library().unwrap();
    assert!(lib.can_change_likes());
    let uris = vec!["a".to_owned(), "b".to_owned()];
    assert_eq!(block_on(lib.liked(&uris)).unwrap(), vec![true, false]);
    block_on(lib.set_liked("b", true)).unwrap();
    assert_eq!(block_on(lib.liked(&uris)).unwrap(), vec![true, true]);
    assert_eq!(p.calls(), vec!["liked 2", "set_liked b true", "liked 2"]);

    let f = FakeProvider::new("fake", true)
        .with_likes(&[], true)
        .fail_set_liked(LibErrorKind::AccessDenied, false);
    let lib = f.library().unwrap();
    assert!(lib.can_change_likes());
    let e = block_on(lib.set_liked("a", true)).unwrap_err();
    assert_eq!(e.kind, LibErrorKind::AccessDenied);
    assert!(!lib.can_change_likes());

    let off = FakeProvider::new("fake", true);
    assert!(!off.info().caps.likes);
    assert!(!off.library().unwrap().can_change_likes());
}

#[test]
fn skin_engine_contract_on_fake_skin_engine() {
    run_skin_engine_contract(&FakeSkinEngine::new(), &FakeSkinEngine::sample());
}

#[test]
fn skin_source_contract_on_fake_skin_source() {
    let source = FakeSkinSource::new(vec![
        ("b.fakeskin".to_owned(), FakeSkinEngine::sample()),
        ("A.fakeskin".to_owned(), FakeSkinEngine::sample()),
    ]);
    run_skin_source_contract(&source, "nope.fakeskin");
}

#[test]
fn panel_windows_contract_on_fake_windows() {
    let rect = |x, y, w, h| ScreenRect { x, y, w, h };
    let mut windows = FakeWindows::new(
        vec![rect(100, 100, 275, 116), rect(100, 216, 275, 116)],
        vec![rect(0, 0, 1920, 1040)],
    );
    windows.set_pointer(Some(ScreenPoint { x: 5, y: 5 }));
    run_panel_windows_contract(&mut windows);
    assert_eq!(windows.frame(0), Some(rect(100, 100, 275, 116)));
    assert_eq!(windows.frame(1), Some(rect(100, 216, 275, 116)));
    assert!(windows.is_visible(1));
}
// ---- H1 matrices ----

fn art_ref(url: &str) -> ImageRef {
    ImageRef {
        url: url.into(),
        width: None,
        height: None,
    }
}

fn lyrics_query(title: &str) -> LyricsQuery {
    LyricsQuery {
        artist: "Artist".into(),
        title: title.into(),
        album: "Album".into(),
        duration_ms: 200_000,
    }
}

#[test]
fn art_errors_on_fake_art() {
    let fake = FakeArt::new();
    let unavailable = demo_images("down").remove(2);
    fake.fail(&unavailable.url);
    // The fake renders every sane cover, so it can give only these two of the four errors.
    block_on(run_art_error_contract(
        &fake,
        &[
            (art_ref("https://evil.example/x"), ArtError::Refused),
            (
                art_ref("https://covers.demo.invalid/a/0"),
                ArtError::Refused,
            ),
            (
                art_ref("https://covers.demo.invalid/a/2049"),
                ArtError::Refused,
            ),
            (unavailable, ArtError::Unavailable),
        ],
    ));
}

#[test]
fn lyrics_outcomes_on_fake_lyrics() {
    let fake = FakeLyrics::new();
    fake.with("Synced", demo_lyrics())
        .with(
            "Plain",
            Lyrics::Plain(vec!["one".into(), "two".into(), "three".into()]),
        )
        .with("Instrumental", Lyrics::Instrumental)
        .fail("Down", LyricsError::Unavailable)
        .fail("Slow", LyricsError::RateLimited)
        .fail("Junk", LyricsError::BadResponse);
    let case = |t: &str, o| (lyrics_query(t), o);
    block_on(run_lyrics_outcome_contract(
        &fake,
        &[
            case("Synced", LyricsOutcome::Synced),
            case("Plain", LyricsOutcome::Plain),
            case("Instrumental", LyricsOutcome::Instrumental),
            case("Nothing", LyricsOutcome::NotFound),
            case("Down", LyricsOutcome::Error(LyricsError::Unavailable)),
            case("Slow", LyricsOutcome::Error(LyricsError::RateLimited)),
            case("Junk", LyricsOutcome::Error(LyricsError::BadResponse)),
        ],
    ));
}

#[test]
fn lyrics_outcome_contract_fails_on_the_wrong_kind_or_an_overlong_line() {
    let fake = FakeLyrics::new();
    fake.with(
        "Loud",
        Lyrics::Synced(vec![LyricLine {
            at_ms: 0,
            text: "x".repeat(400),
        }]),
    );
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        block_on(run_lyrics_outcome_contract(
            &fake,
            &[(lyrics_query("Loud"), LyricsOutcome::Synced)],
        ));
    }));
    assert!(r.is_err(), "an over-long line must fail the caps check");
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        block_on(run_lyrics_outcome_contract(
            &fake,
            &[(lyrics_query("Loud"), LyricsOutcome::NotFound)],
        ));
    }));
    assert!(r.is_err(), "a wrong kind must fail");
}

#[test]
fn provider_contract_on_fake_with_likes_and_without_library() {
    block_on(run_provider_contract(Arc::new(
        FakeProvider::new("fake", true).with_likes(&["a"], true),
    )));
    block_on(run_provider_contract(Arc::new(FakeProvider::new(
        "fake", false,
    ))));
}

#[test]
fn provider_contract_on_logged_out_demo() {
    block_on(run_provider_contract(Arc::new(demo_with(false, false))));
    block_on(run_provider_contract(Arc::new(demo_with(false, true))));
}

#[test]
fn playback_contract_on_fake_and_demo() {
    let fake = Arc::new(FakeProvider::new("fake", true));
    fake.fail_load("bad", true);
    block_on(run_provider_playback_contract(
        fake.clone(),
        Some("fake:track:1"),
        "bad",
    ));
    assert_eq!(fake.calls()[0], "load bad true 0");
    let labelled = Arc::new(FakeProvider::new("fake", true).with_source_format("PCM"));
    labelled.fail_load("bad", true);
    block_on(run_provider_playback_contract(
        labelled,
        Some("fake:track:1"),
        "bad",
    ));
    block_on(run_provider_playback_contract(
        Arc::new(demo(false)),
        Some("demo:track:1"),
        "not-a-demo-track",
    ));
}

#[test]
fn session_contract_on_demo_writable_and_read_only() {
    let key = LoginInput(
        [("key".to_owned(), "abcd".to_owned())]
            .into_iter()
            .collect(),
    );
    let logins = [("play", LoginInput::default()), ("library", key)];
    for read_only in [false, true] {
        block_on(run_provider_session_contract(
            Arc::new(demo_with(false, read_only)),
            &logins,
            ("library", LoginInput::default()),
        ));
    }
}

#[test]
fn credential_store_failures_are_the_fixed_error() {
    let s = MemoryStore::default();
    s.set_failing(true);
    assert_eq!(s.get("a").unwrap_err(), StoreError);
    assert_eq!(s.set("a", &Secret::new("v")).unwrap_err(), StoreError);
    assert_eq!(s.delete("a").unwrap_err(), StoreError);
    assert_eq!(StoreError.to_string(), "credential store access failed");
    // Nothing leaked into the store by a failing set.
    s.set_failing(false);
    assert_eq!(s.peek("a"), None);
}

#[test]
fn audio_output_contract_on_fake_output() {
    let mut fake = FakeOutput::new(
        DeviceFormat {
            rate: 44_100,
            channels: 2,
            sample: SampleFormat::F32,
            valid_bits: 32,
        },
        OutputMode::Shared,
    );
    fake.fail_device("missing");
    let probe = fake.clone();
    let refused = OutputRequest {
        device: Some("missing".into()),
        ..OutputRequest::default()
    };
    run_audio_output_contract(
        &mut fake,
        &mut || {
            probe.render_f32(1024);
            probe.render_f32(1024);
        },
        Some(&refused),
    );
    assert!(!probe.is_open(), "the suite leaves the output closed");
}

#[test]
fn audio_output_contract_on_clocked_output() {
    let mut clocked = ClockedOutput::new(44_100);
    run_audio_output_contract(
        &mut clocked,
        &mut || std::thread::sleep(std::time::Duration::from_millis(80)),
        None,
    );
}

/// An output that breaks one rule of the contract, to show the suite notices.
struct Broken {
    inner: FakeOutput,
    /// Never prepares the renderer (the stream looks open but renders nothing).
    keeps_renderer_unprepared: bool,
    stash: Option<whipspot_core::engine::Renderer>,
    /// `close` loses the renderer.
    loses_renderer: bool,
}

impl whipspot_core::engine::AudioOutput for Broken {
    fn devices(
        &mut self,
    ) -> Result<Vec<whipspot_core::engine::OutputDevice>, whipspot_core::engine::OutputError> {
        self.inner.devices()
    }

    fn open(
        &mut self,
        request: &OutputRequest,
        renderer: whipspot_core::engine::Renderer,
    ) -> Result<whipspot_core::engine::OutputInfo, whipspot_core::engine::OpenFailed> {
        if self.keeps_renderer_unprepared {
            self.stash = Some(renderer);
            return Ok(whipspot_core::engine::OutputInfo {
                host_api: "broken".into(),
                device: "broken".into(),
                mode: OutputMode::Shared,
                format: DeviceFormat {
                    rate: 44_100,
                    channels: 2,
                    sample: SampleFormat::F32,
                    valid_bits: 32,
                },
                buffer_frames: None,
                latency_ms: None,
            });
        }
        self.inner.open(request, renderer)
    }

    fn close(&mut self) -> Option<whipspot_core::engine::Renderer> {
        let back = self.stash.take().or_else(|| self.inner.close());
        if self.loses_renderer { None } else { back }
    }
}

fn broken_output(unprepared: bool, loses: bool) -> Broken {
    Broken {
        inner: FakeOutput::new(
            DeviceFormat {
                rate: 44_100,
                channels: 2,
                sample: SampleFormat::F32,
                valid_bits: 32,
            },
            OutputMode::Shared,
        ),
        keeps_renderer_unprepared: unprepared,
        stash: None,
        loses_renderer: loses,
    }
}

/// The message of a panic caught by `catch_unwind` (empty when it was not a string).
fn panic_text(r: std::thread::Result<()>) -> String {
    let e = r.expect_err("the suite must fail");
    e.downcast_ref::<String>()
        .cloned()
        .or_else(|| e.downcast_ref::<&str>().map(|s| (*s).to_owned()))
        .unwrap_or_default()
}

#[test]
fn audio_output_contract_notices_a_renderer_that_never_renders_or_is_lost() {
    for (unprepared, loses, expect) in [
        (true, false, "the renderer rendered after open"),
        (false, true, "close returns the renderer"),
    ] {
        let mut out = broken_output(unprepared, loses);
        let probe = out.inner.clone();
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_audio_output_contract(&mut out, &mut || drop(probe.render_f32(512)), None);
        }));
        let text = panic_text(r);
        assert!(text.contains(expect), "{text}");
    }
}

#[test]
fn provider_contract_notices_a_library_cap_that_disagrees() {
    let p = FakeProvider::new("fake", true).with_caps_library(false);
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        block_on(run_provider_contract(Arc::new(p)));
    }));
    let text = panic_text(r);
    assert!(text.contains("caps.library matches"), "{text}");
}
// ---- H1 matrices: the skin seams ----

fn fake_skin() -> Skin {
    FakeSkinEngine::new()
        .load(&FakeSkinEngine::sample())
        .expect("the fake sample loads")
}

#[test]
fn skin_caps_contract_on_the_fake_skin() {
    run_skin_caps_contract(&fake_skin());
}

#[test]
fn skin_caps_contract_notices_a_template_that_is_not_valid() {
    let mut bad = fake_skin();
    bad.panels[0].visible_at_start = false;
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_skin_caps_contract(&bad);
    }));
    assert!(panic_text(r).contains("the template validates"));
}

/// An engine wrapping the fake whose `load` and `detect` can be bent to break one rule.
struct BentEngine {
    inner: FakeSkinEngine,
    /// What `load` answers for input that is not the good sample.
    answer: Option<SkinError>,
    detect_all: bool,
    /// Once a garbage input was loaded, the good sample loads as a different skin.
    keeps_state: bool,
    garbage_seen: std::sync::atomic::AtomicBool,
}

impl BentEngine {
    fn new() -> Self {
        Self {
            inner: FakeSkinEngine::new(),
            answer: None,
            detect_all: false,
            keeps_state: false,
            garbage_seen: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

impl SkinEngine for BentEngine {
    fn id(&self) -> &'static str {
        self.inner.id()
    }
    fn extensions(&self) -> &'static [&'static str] {
        self.inner.extensions()
    }
    fn detect(&self, bytes: &[u8]) -> bool {
        self.detect_all || self.inner.detect(bytes)
    }
    fn load(&self, bytes: &[u8]) -> Result<Skin, SkinError> {
        use std::sync::atomic::Ordering::Relaxed;
        if bytes == FakeSkinEngine::sample() {
            let mut skin = self.inner.load(bytes)?;
            if self.keeps_state && self.garbage_seen.load(Relaxed) {
                skin.info.name = "changed".to_owned();
            }
            return Ok(skin);
        }
        self.garbage_seen.store(true, Relaxed);
        match self.answer {
            Some(e) if bytes.len() as u64 <= MAX_SKIN_BYTES => Err(e),
            _ => self.inner.load(bytes),
        }
    }
}

fn engine_contract_panics(engine: &BentEngine) -> String {
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_skin_engine_contract(engine, &FakeSkinEngine::sample());
    }));
    panic_text(r)
}

#[test]
fn skin_engine_contract_passes_the_unbent_engine() {
    run_skin_engine_contract(&BentEngine::new(), &FakeSkinEngine::sample());
}

#[test]
fn skin_engine_contract_notices_a_source_error_from_load() {
    for e in [SkinError::NotFound, SkinError::Io, SkinError::InvalidName] {
        let mut engine = BentEngine::new();
        engine.answer = Some(e);
        let text = engine_contract_panics(&engine);
        assert!(text.contains("file-level error"), "{e:?}: {text}");
    }
}

#[test]
fn skin_engine_contract_notices_a_detect_that_accepts_noise() {
    let mut engine = BentEngine::new();
    engine.detect_all = true;
    assert!(engine_contract_panics(&engine).contains("detect refuses"));
}

#[test]
fn skin_engine_contract_notices_state_kept_between_loads() {
    let mut engine = BentEngine::new();
    engine.keeps_state = true;
    let text = engine_contract_panics(&engine);
    assert!(
        text.contains("same bytes give an equal skin") || text.contains("garbage in between"),
        "{text}"
    );
}

#[test]
fn skin_source_contract_on_a_source_with_case_pairs_and_odd_names() {
    let source = FakeSkinSource::new(
        [
            "Zed.fakeskin",
            "alpha.fakeskin",
            "Alpha.fakeskin",
            "b.fakeskin",
        ]
        .map(|n| (n.to_owned(), FakeSkinEngine::sample()))
        .to_vec(),
    );
    assert_eq!(
        source.list(),
        [
            "Alpha.fakeskin",
            "alpha.fakeskin",
            "b.fakeskin",
            "Zed.fakeskin"
        ]
    );
    run_skin_source_contract(&source, "nope.fakeskin");
    let empty = FakeSkinSource::new(Vec::new());
    assert!(empty.list().is_empty());
    run_skin_source_contract(&empty, "nope.fakeskin");
}

fn size_entries() -> Vec<(String, Vec<u8>)> {
    vec![
        ("cap.fakeskin".to_owned(), vec![0; MAX_SKIN_BYTES as usize]),
        (
            "over.fakeskin".to_owned(),
            vec![0; MAX_SKIN_BYTES as usize + 1],
        ),
    ]
}

#[test]
fn skin_source_size_contract_on_the_fake_source() {
    let source = FakeSkinSource::new(size_entries());
    run_skin_source_size_contract(&source, "cap.fakeskin", "over.fakeskin");
}

/// A source that breaks one rule of the source contract.
struct BentSource {
    inner: FakeSkinSource,
    unsorted: bool,
    accepts_traversal: bool,
}

impl SkinSource for BentSource {
    fn list(&self) -> Vec<String> {
        let mut names = self.inner.list();
        if self.unsorted {
            names.reverse();
        }
        names
    }
    fn read(&self, name: &str) -> Result<Vec<u8>, SkinError> {
        if self.accepts_traversal && name == "../x" {
            return Ok(Vec::new());
        }
        self.inner.read(name)
    }
}

#[test]
fn skin_source_contract_notices_an_unsorted_list_and_an_accepted_traversal() {
    let entries = || {
        ["a.fakeskin", "b.fakeskin"]
            .map(|n| (n.to_owned(), FakeSkinEngine::sample()))
            .to_vec()
    };
    for (unsorted, accepts_traversal, expect) in [
        (true, false, "sorted by (lowercase, exact)"),
        (false, true, "is InvalidName"),
    ] {
        let source = BentSource {
            inner: FakeSkinSource::new(entries()),
            unsorted,
            accepts_traversal,
        };
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_skin_source_contract(&source, "nope.fakeskin");
        }));
        assert!(panic_text(r).contains(expect), "{expect}");
    }
}

#[test]
fn skin_source_size_contract_notices_a_truncated_oversize_read() {
    // A source that cuts an oversized file to the cap instead of refusing it.
    struct Cuts(FakeSkinSource);
    impl SkinSource for Cuts {
        fn list(&self) -> Vec<String> {
            self.0.list()
        }
        fn read(&self, name: &str) -> Result<Vec<u8>, SkinError> {
            match self.0.read(name) {
                Err(SkinError::TooLarge) => Ok(vec![0; MAX_SKIN_BYTES as usize]),
                other => other,
            }
        }
    }
    let source = Cuts(FakeSkinSource::new(size_entries()));
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_skin_source_size_contract(&source, "cap.fakeskin", "over.fakeskin");
    }));
    assert!(panic_text(r).contains("one byte over the cap is TooLarge"));
}

fn three_windows_two_monitors() -> FakeWindows {
    let rect = |x, y, w, h| ScreenRect { x, y, w, h };
    FakeWindows::new(
        vec![
            rect(100, 100, 275, 116),
            rect(100, 216, 275, 116),
            rect(375, 100, 275, 232),
        ],
        vec![rect(0, 0, 1920, 1040), rect(1920, 0, 1280, 1000)],
    )
}

#[test]
fn panel_windows_contract_on_three_windows_and_two_monitors() {
    let mut windows = three_windows_two_monitors();
    run_panel_windows_contract(&mut windows);
    assert_eq!(windows.work_areas().len(), 2, "both monitors stay listed");
    assert!(windows.is_visible(0) && windows.is_visible(1) && windows.is_visible(2));
}

#[test]
fn fake_windows_contract_runs_on_the_fake() {
    run_fake_windows_contract(&mut three_windows_two_monitors());
}

#[test]
fn fake_windows_with_no_windows_answers_without_panicking() {
    let mut none = FakeWindows::new(Vec::new(), Vec::new());
    assert_eq!(none.count(), 0);
    assert_eq!(none.frame(0), None);
    assert!(!none.is_visible(0));
    assert!(none.work_areas().is_empty());
    assert!(none.move_panels(&[]).is_ok());
    assert!(
        none.move_panels(&[(0, ScreenPoint { x: 0, y: 0 })])
            .is_err()
    );
    none.set_visible(0, true);
    none.set_docked(0, true);
    none.minimise();
    assert!(none.minimised() && !none.docked(0));
}

#[test]
fn panel_windows_contract_notices_a_hide_that_takes_the_leader_along() {
    /// Hiding any panel also hides the leader.
    struct HidesLeader(FakeWindows);
    impl PanelWindows for HidesLeader {
        fn count(&self) -> usize {
            self.0.count()
        }
        fn frame(&self, panel: usize) -> Option<ScreenRect> {
            self.0.frame(panel)
        }
        fn is_visible(&self, panel: usize) -> bool {
            self.0.is_visible(panel)
        }
        fn work_areas(&self) -> Vec<ScreenRect> {
            self.0.work_areas()
        }
        fn pointer(&self) -> Option<ScreenPoint> {
            self.0.pointer()
        }
        fn units_per_pixel(&self) -> f32 {
            self.0.units_per_pixel()
        }
        fn move_panels(
            &mut self,
            moves: &[(usize, ScreenPoint)],
        ) -> Result<(), whipspot_core::skin::PanelWindowsError> {
            self.0.move_panels(moves)
        }
        fn resize_panel(
            &mut self,
            panel: usize,
            w: i32,
            h: i32,
        ) -> Result<(), whipspot_core::skin::PanelWindowsError> {
            self.0.resize_panel(panel, w, h)
        }
        fn set_visible(&mut self, panel: usize, visible: bool) {
            self.0.set_visible(panel, visible);
            if !visible {
                self.0.set_visible(0, false);
            }
        }
        fn set_docked(&mut self, panel: usize, docked: bool) {
            self.0.set_docked(panel, docked);
        }
        fn set_always_on_top(&mut self, on: bool) {
            self.0.set_always_on_top(on);
        }
        fn minimise(&mut self) {
            self.0.minimise();
        }
    }
    let mut windows = HidesLeader(three_windows_two_monitors());
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_panel_windows_contract(&mut windows);
    }));
    assert!(panic_text(r).contains("never hides the leader"));
}

// ---- the art router, the music folders and a provider with no auth steps (step 2) ----

#[test]
fn art_source_contract_on_a_router_with_fake_art_as_the_default() {
    let good = demo_images("contract").remove(2);
    let refused = ImageRef {
        url: "https://evil.example/x".into(),
        width: None,
        height: None,
    };
    let router = ArtRouter::new().with_default(Arc::new(FakeArt::new()));
    block_on(run_art_source_contract(&router, &good, &refused));
    // A scheme of its own goes to its source, not the default.
    let mut router = router;
    router
        .register("local-art", Arc::new(FakeArt::new()))
        .unwrap();
    assert_eq!(
        router.register("local-art", Arc::new(FakeArt::new())),
        Err(ArtRouteError::Duplicate)
    );
    assert_eq!(
        block_on(router.fetch(&art_ref("local-art:1"))).unwrap_err(),
        ArtError::Refused,
        "FakeArt, behind the scheme, refuses what it does not know"
    );
}

/// A scratch directory this test creates and removes (never an existing one).
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("whipspot-{name}-{}", std::process::id()));
        assert!(!dir.exists(), "{dir:?} is not there yet");
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn music_folders_contract_on_fake_folders() {
    let scratch = Scratch::new("folders-contract");
    let (a, b, file) = (
        scratch.0.join("a"),
        scratch.0.join("b"),
        scratch.0.join("f.txt"),
    );
    std::fs::create_dir(&a).unwrap();
    std::fs::create_dir(&b).unwrap();
    std::fs::write(&file, b"x").unwrap();
    run_music_folders_contract(&FakeFolders::new(), [&a, &b], &file);
}

#[test]
fn music_folders_contract_notices_a_list_that_forgets_the_order_or_accepts_a_file() {
    use whipspot_core::folders::{MusicFolders, ScanStatus};

    /// Lists folders newest first and takes anything.
    #[derive(Default)]
    struct Sloppy(std::sync::Mutex<Vec<String>>);

    impl MusicFolders for Sloppy {
        fn folders(&self) -> Vec<String> {
            self.0.lock().unwrap().clone()
        }
        fn add(&self, path: String) -> Result<(), FolderError> {
            self.0.lock().unwrap().insert(0, path);
            Ok(())
        }
        fn remove(&self, _path: &str) {}
        fn rescan(&self) {}
        fn status(&self) -> ScanStatus {
            ScanStatus::default()
        }
    }

    let scratch = Scratch::new("folders-sloppy");
    let (a, b, file) = (
        scratch.0.join("a"),
        scratch.0.join("b"),
        scratch.0.join("f.txt"),
    );
    std::fs::create_dir(&a).unwrap();
    std::fs::create_dir(&b).unwrap();
    std::fs::write(&file, b"x").unwrap();
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_music_folders_contract(&Sloppy::default(), [&a, &b], &file);
    }));
    let text = panic_text(r);
    assert!(text.contains("music folders contract"), "{text}");
}

/// A logged-in demo provider that declares no auth steps: always ready, like a local-file one.
struct AlwaysReady(DemoProvider);

impl Provider for AlwaysReady {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            auth_steps: vec![],
            ..self.0.info()
        }
    }
    fn auth(&self) -> &dyn AuthFlow {
        self
    }
    fn playback(&self) -> &dyn Playback {
        self.0.playback()
    }
    fn library(&self) -> Option<&dyn Library> {
        self.0.library()
    }
}

impl AuthFlow for AlwaysReady {
    fn status(&self) -> BoxFuture<'_, Result<ProviderAuthStatus, AuthFailure>> {
        Box::pin(async { Ok(ProviderAuthStatus { steps: vec![] }) })
    }
    fn login<'a>(
        &'a self,
        _step: &'a str,
        _input: &'a LoginInput,
    ) -> BoxFuture<'a, Result<ProviderAuthStatus, AuthFailure>> {
        Box::pin(async { Err(AuthFailure("there is nothing to log in to".into())) })
    }
    fn logout<'a>(
        &'a self,
        _step: &'a str,
    ) -> BoxFuture<'a, Result<ProviderAuthStatus, AuthFailure>> {
        Box::pin(async { Err(AuthFailure("there is nothing to log out of".into())) })
    }
}

#[test]
fn provider_contracts_on_a_provider_with_no_auth_steps() {
    // Logged-in library calls succeed; the logged-out error checks do not apply.
    block_on(run_provider_contract(Arc::new(AlwaysReady(demo(false)))));
    block_on(run_provider_contract(Arc::new(AlwaysReady(demo(true)))));
    // The session walk skips the login part and walks the library as it is.
    for read_only in [false, true] {
        block_on(run_provider_session_contract(
            Arc::new(AlwaysReady(demo(read_only))),
            &[],
            ("library", LoginInput::default()),
        ));
    }
}

/// Auth steps that read logged out, but a library that answers anyway.
struct LeakyLibrary {
    logged_out: DemoProvider,
    logged_in: DemoProvider,
}

impl Provider for LeakyLibrary {
    fn info(&self) -> ProviderInfo {
        self.logged_out.info()
    }
    fn auth(&self) -> &dyn AuthFlow {
        self.logged_out.auth()
    }
    fn playback(&self) -> &dyn Playback {
        self.logged_out.playback()
    }
    fn library(&self) -> Option<&dyn Library> {
        self.logged_in.library()
    }
}

#[test]
fn provider_contract_on_a_provider_with_auth_steps_still_demands_logged_out_errors() {
    let p = LeakyLibrary {
        logged_out: demo_with(false, false),
        logged_in: demo(false),
    };
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        block_on(run_provider_contract(Arc::new(p)));
    }));
    let text = panic_text(r);
    assert!(text.contains("while logged out is an error"), "{text}");
}
