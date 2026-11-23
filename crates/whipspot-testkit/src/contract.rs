//! Contract suites: what every [`Provider`], [`Remote`](whipspot_core::provider::Remote),
//! [`CredentialStore`], [`AudioOutput`], [`ArtSource`], [`LyricsSource`], [`SkinEngine`],
//! [`SkinSource`] and [`PanelWindows`] must do. Each case panics with its case name in the
//! message. The first suites were kept small (D28); each doc comment now carries a table of
//! method x case (H1), and `tests/seams.rs` fails when a core trait has no suite listed.

use std::future::{Future, poll_fn};
use std::path::Path;
use std::pin::pin;
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::Duration;

use whipspot_core::{
    art::{ArtError, ArtSource, MAX_ART_SIDE},
    credential_store::CredentialStore,
    engine::{
        AudioOutput, DeviceFormat, Engine, EngineConfig, OpenFailed, OutputDevice, OutputError,
        OutputInfo, OutputMode, OutputRequest, Renderer, SampleFormat, SourceInfo, SourceWriter,
        StreamFormat,
    },
    folders::{FolderError, MusicFolders},
    lyrics::{Lyrics, LyricsError, LyricsQuery, LyricsSource, MAX_LYRIC_CHARS, MAX_LYRIC_LINES},
    power::{PowerControl, PowerError, PowerMode},
    provider::{
        Album, AlbumKind, AuthFlow, AuthStepStatus, EntityRef, ImageRef, LibErrorKind, Library,
        LibraryError, LoginInput, MAX_ARTISTS, MAX_IMAGES, MAX_POPULAR, Provider, ProviderInfo,
        ProviderRegistry, Readable, RegisterError, ReleaseGroup, RemoteCommand, Track,
        model::LoginState, registry::is_valid_id,
    },
    secret::Secret,
    skin::{
        BitmapFont, Dock, ImageId, MAX_FONTS, MAX_GLYPHS, MAX_IMAGES as MAX_SKIN_IMAGES,
        MAX_PANEL_SIDE, MAX_PANELS, MAX_POINTS, MAX_POLYGONS, MAX_SKIN_BYTES, MAX_VIS_COLOURS,
        MAX_WIDGETS_PER_PANEL, PanelId, PanelWindows, Point, Rect, Rgb, ScreenPoint, ScreenRect,
        Shape, Size, Skin, SkinEngine, SkinError, SkinSource, SpriteRect, VisPalette,
    },
};

use crate::FakeWindows;
use crate::fake_provider::Sleep;

/// What a sane error text looks like: not empty, one short line, no control character.
fn assert_fixed_text(text: &str, what: &str) {
    assert!(!text.is_empty(), "{what}: the error text is not empty");
    assert!(
        text.chars().count() <= 200 && !text.chars().any(char::is_control),
        "{what}: the error text is short and has no control character ({text:?})"
    );
}

/// Art source cases. No concurrency, no timers; the trait has no cancel (a fetch is a future the
/// caller may drop).
///
/// | method | case |
/// |---|---|
/// | `fetch` | `good` decodes, sides in `1..=MAX_ART_SIDE`, rgba is `w * h * 4` bytes |
/// | `fetch` | the same ref twice gives an equal image |
/// | `fetch` | `refused` is an error, the same error twice, with a fixed text |
/// | `fetch` | a ref with an empty address is an error |
/// | `fetch` | a refusal does not poison the source: `good` still fetches equal after it |
///
/// Every documented error value is covered by [`run_art_error_contract`].
pub async fn run_art_source_contract(source: &dyn ArtSource, good: &ImageRef, refused: &ImageRef) {
    let first = source
        .fetch(good)
        .await
        .unwrap_or_else(|e| panic!("art contract: a good image is fetched: {e}"));
    assert!(
        (1..=MAX_ART_SIDE).contains(&first.width()) && (1..=MAX_ART_SIDE).contains(&first.height()),
        "art contract: both sides are within 1..={MAX_ART_SIDE}"
    );
    assert_eq!(
        first.rgba().len() as u64,
        u64::from(first.width()) * u64::from(first.height()) * 4,
        "art contract: rgba holds width * height * 4 bytes"
    );
    let second = source
        .fetch(good)
        .await
        .unwrap_or_else(|e| panic!("art contract: a good image is fetched twice: {e}"));
    assert_eq!(
        first, second,
        "art contract: the same ref gives an equal image"
    );
    let Err(error) = source.fetch(refused).await else {
        panic!("art contract: a refused ref is an error");
    };
    assert_eq!(
        source.fetch(refused).await.err(),
        Some(error),
        "art contract: a refused ref gives the same error twice"
    );
    assert_fixed_text(error.text(), "art contract: a refused ref");
    let empty = ImageRef {
        url: String::new(),
        width: None,
        height: None,
    };
    assert!(
        source.fetch(&empty).await.is_err(),
        "art contract: an empty address is an error"
    );
    assert_eq!(
        source.fetch(good).await.ok(),
        Some(first),
        "art contract: a refusal does not poison the source"
    );
}

/// Art error cases: each `(ref, error)` must fetch to exactly that error, the same one twice.
/// Callers list the errors their source can produce; the documented values are `Refused`,
/// `Unavailable`, `TooLarge` and `Undecodable`.
///
/// | method | case |
/// |---|---|
/// | `fetch` | every listed ref gives exactly its error |
/// | `fetch` | and gives it again on the second fetch (failures are not cached) |
/// | `fetch` | every error's text is fixed |
pub async fn run_art_error_contract(source: &dyn ArtSource, cases: &[(ImageRef, ArtError)]) {
    assert!(!cases.is_empty(), "art error contract: some cases");
    for (image, expected) in cases {
        for round in 1..=2 {
            assert_eq!(
                source.fetch(image).await.err(),
                Some(*expected),
                "art error contract: {:?} gives {expected:?} (fetch {round})",
                image.url
            );
        }
        assert_fixed_text(expected.text(), "art error contract");
    }
}

/// What a bounded, sane lyrics answer looks like (shared by the lyrics suites).
fn assert_lyrics_bounds(lyrics: &Lyrics, what: &str) {
    let short = |t: &str| t.chars().count() <= MAX_LYRIC_CHARS && !t.chars().any(char::is_control);
    match lyrics {
        Lyrics::Synced(lines) => {
            assert!(
                lines.len() <= MAX_LYRIC_LINES,
                "{what}: at most {MAX_LYRIC_LINES} lines"
            );
            assert!(
                lines.windows(2).all(|w| w[0].at_ms <= w[1].at_ms),
                "{what}: lines are sorted by time"
            );
            assert!(
                lines.iter().all(|l| short(&l.text)),
                "{what}: lines are short and hold no control character"
            );
        }
        Lyrics::Plain(lines) => {
            assert!(
                lines.len() <= MAX_LYRIC_LINES,
                "{what}: at most {MAX_LYRIC_LINES} lines"
            );
            assert!(
                lines.iter().all(|l| short(l)),
                "{what}: lines are short and hold no control character"
            );
        }
        Lyrics::Instrumental | Lyrics::None => {}
    }
}

/// Lyrics source cases. No concurrency, no timers.
///
/// | method | case |
/// |---|---|
/// | `lookup` | `hit` is synced lyrics: at most `MAX_LYRIC_LINES`, sorted, short lines, no control character |
/// | `lookup` | `miss` is `Ok(Lyrics::None)` |
/// | `lookup` | an empty title is `Ok(Lyrics::None)` |
/// | `lookup` | an empty artist is `Ok(Lyrics::None)` |
/// | `lookup` | a blank (spaces only) title is `Ok(Lyrics::None)` |
///
/// The other answers and every error are covered by [`run_lyrics_outcome_contract`].
pub async fn run_lyrics_source_contract(
    source: &dyn LyricsSource,
    hit: &LyricsQuery,
    miss: &LyricsQuery,
) {
    let found = source
        .lookup(hit)
        .await
        .unwrap_or_else(|e| panic!("lyrics contract: a hit is Ok: {e}"));
    let Lyrics::Synced(_) = &found else {
        panic!("lyrics contract: a hit is synced lyrics");
    };
    assert_lyrics_bounds(&found, "lyrics contract: a hit");
    assert_eq!(
        source.lookup(miss).await,
        Ok(Lyrics::None),
        "lyrics contract: a miss is Ok(None)"
    );
    let untitled = LyricsQuery {
        title: String::new(),
        ..hit.clone()
    };
    assert_eq!(
        source.lookup(&untitled).await,
        Ok(Lyrics::None),
        "lyrics contract: an empty title is Ok(None)"
    );
    let anonymous = LyricsQuery {
        artist: String::new(),
        ..hit.clone()
    };
    assert_eq!(
        source.lookup(&anonymous).await,
        Ok(Lyrics::None),
        "lyrics contract: an empty artist is Ok(None)"
    );
    let blank = LyricsQuery {
        title: "   ".into(),
        ..hit.clone()
    };
    assert_eq!(
        source.lookup(&blank).await,
        Ok(Lyrics::None),
        "lyrics contract: a blank title is Ok(None)"
    );
}

/// What a lyrics lookup is expected to give, by kind (the lines are the source's).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LyricsOutcome {
    Synced,
    Plain,
    Instrumental,
    NotFound,
    Error(LyricsError),
}

/// Lyrics outcome cases: each `(query, outcome)` is looked up once on `source` and must give
/// exactly that kind. Synced and plain answers are checked for the caps. A source may pace or
/// back off between lookups, so a caller that lists an error to provoke it uses a fresh source
/// for that case.
///
/// | method | case |
/// |---|---|
/// | `lookup` | `Synced`, `Plain`: within the line and character caps, sorted, no control character |
/// | `lookup` | `Instrumental`, `NotFound` (`Ok(Lyrics::None)`) |
/// | `lookup` | `Error(Unavailable \| RateLimited \| BadResponse)`: exactly that error |
/// | `lookup` | every error's text is fixed |
pub async fn run_lyrics_outcome_contract(
    source: &dyn LyricsSource,
    cases: &[(LyricsQuery, LyricsOutcome)],
) {
    assert!(!cases.is_empty(), "lyrics outcome contract: some cases");
    for (query, outcome) in cases {
        let what = format!("lyrics outcome contract: {:?} ({outcome:?})", query.title);
        let answer = source.lookup(query).await;
        match (outcome, answer) {
            (LyricsOutcome::Synced, Ok(l @ Lyrics::Synced(_)))
            | (LyricsOutcome::Plain, Ok(l @ Lyrics::Plain(_))) => assert_lyrics_bounds(&l, &what),
            (LyricsOutcome::Instrumental, Ok(Lyrics::Instrumental))
            | (LyricsOutcome::NotFound, Ok(Lyrics::None)) => {}
            (LyricsOutcome::Error(want), Err(got)) => {
                assert_eq!(got, *want, "{what}: the error");
                assert_fixed_text(got.text(), &what);
            }
            (_, other) => panic!("{what}: got {other:?}"),
        }
    }
}

/// `f`'s output, or `None` when it has not finished after `limit`. Needs no async runtime (the
/// timer is a helper thread), so it runs under any executor.
async fn within<F: Future>(limit: Duration, f: F) -> Option<F::Output> {
    let mut f = pin!(f);
    let mut timer = pin!(Sleep::new(limit));
    poll_fn(|cx| {
        if let Poll::Ready(v) = f.as_mut().poll(cx) {
            return Poll::Ready(Some(v));
        }
        timer.as_mut().poll(cx).map(|()| None)
    })
    .await
}

/// How long a [`Remote`](whipspot_core::provider::Remote) call with no remote session may take.
const REMOTE_IDLE_LIMIT: Duration = Duration::from_secs(2);

/// Remote cases that need no remote session (no other device, no network), on any provider that
/// has a [`Remote`](whipspot_core::provider::Remote).
///
/// | method | case |
/// |---|---|
/// | `Provider::remote` | `Some` |
/// | `owner` | not empty, not the provider's id, the same twice |
/// | `command` | each `RemoteCommand` (`Seek` and `Volume` at both ends) returns, Ok or an error, within 2 s and without a panic |
/// | `release` | returns within 2 s, twice |
pub async fn run_remote_contract(p: Arc<dyn Provider>) {
    let id = p.info().id;
    let r = p
        .remote()
        .expect("remote contract: remote() is Some for a provider with a remote");
    let owner = r.owner().to_owned();
    assert!(!owner.is_empty(), "remote contract: owner() is not empty");
    assert_ne!(
        owner, id,
        "remote contract: owner() is not the provider's id (the owner switch must separate them)"
    );
    assert_eq!(
        owner,
        r.owner(),
        "remote contract: owner() is the same twice"
    );
    for cmd in [
        RemoteCommand::Play,
        RemoteCommand::Pause,
        RemoteCommand::Next,
        RemoteCommand::Previous,
        RemoteCommand::Seek(0),
        RemoteCommand::Seek(u32::MAX),
        RemoteCommand::Volume(0),
        RemoteCommand::Volume(100),
    ] {
        assert!(
            within(REMOTE_IDLE_LIMIT, r.command(cmd)).await.is_some(),
            "remote contract: command({cmd:?}) returns within 2 s with no remote session"
        );
    }
    for n in 1..=2 {
        assert!(
            within(REMOTE_IDLE_LIMIT, r.release()).await.is_some(),
            "remote contract: release() returns within 2 s with no remote session (call {n})"
        );
    }
}

/// Provider cases that need no login, no playback and no socket (MD7), on any provider.
///
/// | method | case |
/// |---|---|
/// | `info` | valid id; non-empty name; same twice; auth step ids and input names unique; `likes` and `search` imply `library`; `library` matches `library().is_some()` |
/// | `image_hosts` | each host is lowercase, non-empty, no scheme or path |
/// | registry | a fresh registry accepts it; a second registration is `Duplicate` |
/// | `auth.status` | one entry per step, `LoggedOut` for every login step (a switch step may start on); the same twice |
/// | `auth.logout` | every step is Ok and leaves itself and every login step `LoggedOut`; twice is Ok |
/// | `auth.prefill` | an unknown step and input never panics: Ok or a fixed error |
/// | `library.*` | logged out: `playlists`, `liked_tracks`, `playlist_items`, `albums`, `album_tracks`, `search` are errors with a fixed text (the same twice); a provider with no login steps (none, or only switches) is always ready, so each call there is Ok or an error with a fixed text |
/// | `library.check_changes` | returns within 5 s |
/// | `library.liked`, `set_liked`, `can_change_likes` | `caps.likes` off: the first two are errors and the last is false; on: `liked` is an error or one answer per uri |
/// | `caps` | `explore` needs `library`; `save_albums` needs `likes` |
/// | `library.artist`, `album`, `artist_releases` | an empty, a malformed, a foreign-scheme and a wrong-kind uri is an error with a fixed text, never a panic (with `explore` off the default is that error) |
///
/// Playback and the logged-in library are in [`run_provider_playback_contract`] and
/// [`run_provider_session_contract`], run on providers that can do them without a device or a
/// network.
pub async fn run_provider_contract(p: Arc<dyn Provider>) {
    let info = p.info();

    assert!(
        is_valid_id(info.id),
        "provider contract: info().id is a valid id ({:?})",
        info.id
    );
    assert!(
        !info.name.trim().is_empty(),
        "provider contract: info().name is not empty"
    );
    assert_eq!(
        info,
        p.info(),
        "provider contract: info() is the same twice"
    );
    assert_eq!(
        info.caps.library,
        p.library().is_some(),
        "provider contract: caps.library matches library().is_some()"
    );
    assert!(
        !info.caps.likes || info.caps.library,
        "provider contract: caps.likes needs caps.library"
    );
    assert!(
        !info.caps.search || info.caps.library,
        "provider contract: caps.search needs caps.library"
    );
    assert!(
        !info.caps.explore || info.caps.library,
        "provider contract: caps.explore needs caps.library"
    );
    assert!(
        !info.caps.save_albums || info.caps.likes,
        "provider contract: caps.save_albums needs caps.likes"
    );
    let mut step_ids: Vec<&str> = Vec::new();
    for step in &info.auth_steps {
        assert!(
            !step.id.is_empty() && !step_ids.contains(&step.id),
            "provider contract: auth step ids are non-empty and unique ({:?})",
            step.id
        );
        step_ids.push(step.id);
        let mut inputs: Vec<&str> = Vec::new();
        for input in &step.inputs {
            assert!(
                !input.name.is_empty() && input.max_len > 0 && !inputs.contains(&input.name),
                "provider contract: step {:?} inputs are named, bounded and unique",
                step.id
            );
            inputs.push(input.name);
        }
    }
    for host in p.image_hosts() {
        assert!(
            !host.is_empty()
                && *host == host.to_ascii_lowercase()
                && !host.contains(['/', ':', ' ']),
            "provider contract: image host {host:?} is a bare lowercase host"
        );
    }

    let mut registry = ProviderRegistry::new();
    registry
        .register(p.clone())
        .unwrap_or_else(|e| panic!("provider contract: a fresh registry accepts it: {e}"));
    assert_eq!(
        registry.register(p.clone()).unwrap_err(),
        RegisterError::Duplicate,
        "provider contract: a second registration is a duplicate"
    );

    let status = p.auth().status().await.unwrap_or_else(|e| {
        panic!(
            "provider contract: auth status on a fresh provider: {}",
            e.0
        )
    });
    assert_eq!(
        status.steps.len(),
        info.auth_steps.len(),
        "provider contract: one status entry per auth step"
    );
    for step in &info.auth_steps {
        let entries: Vec<_> = status.steps.iter().filter(|s| s.id == step.id).collect();
        assert_eq!(
            entries.len(),
            1,
            "provider contract: one status entry for step {:?}",
            step.id
        );
        // A switch (an optional feature, on or off) may start on; a login starts logged out.
        if step.is_login() {
            assert_eq!(
                entries[0].state,
                LoginState::LoggedOut,
                "provider contract: login step {:?} starts logged out",
                step.id
            );
        }
    }
    assert_eq!(
        p.auth().status().await.ok(),
        Some(status),
        "provider contract: auth status is the same twice"
    );

    for step in &info.auth_steps {
        for round in 1..=2 {
            let after = p.auth().logout(step.id).await.unwrap_or_else(|e| {
                panic!(
                    "provider contract: logout {:?} is Ok (call {round}): {}",
                    step.id, e.0
                )
            });
            for entry in &after.steps {
                // Logging a switch out turns only that switch off; the other switches keep
                // their setting.
                let is_switch = info
                    .auth_steps
                    .iter()
                    .any(|s| s.id == entry.id && !s.is_login());
                if is_switch && entry.id != step.id {
                    continue;
                }
                assert_eq!(
                    entry.state,
                    LoginState::LoggedOut,
                    "provider contract: after logout {:?}, step {:?} is logged out",
                    step.id,
                    entry.id
                );
            }
        }
    }
    // Either answer is fine; a panic or a hang is not.
    if let Err(e) = p.auth().prefill("no-such-step", "no-such-input").await {
        assert_fixed_text(&e.0, "provider contract: prefill of an unknown step");
    }

    if let Some(lib) = p.library() {
        // `check_changes` is a hint: it returns on its own, logged out or not.
        assert!(
            within(Duration::from_secs(5), lib.check_changes())
                .await
                .is_some(),
            "provider contract: check_changes returns within 5 s"
        );
        // A provider with login steps starts logged out: its library refuses. One with none (or
        // only switches) is always ready: its library answers or refuses with a fixed text, the
        // same twice.
        let always_ready = !info.auth_steps.iter().any(|s| s.is_login());
        for round in 1..=2 {
            let errors = [
                ("playlists", lib.playlists(0).await.err()),
                ("liked_tracks", lib.liked_tracks(0).await.err()),
                ("playlist_items", lib.playlist_items("", 0).await.err()),
                ("albums", lib.albums(0).await.err()),
                ("album_tracks", lib.album_tracks("", 0).await.err()),
                ("search", lib.search("", 0).await.err()),
            ];
            for (name, error) in errors {
                match error {
                    Some(error) => assert_fixed_text(
                        &error.message,
                        &format!("provider contract: {name} while logged out"),
                    ),
                    None if always_ready => {}
                    None => panic!(
                        "provider contract: {name} while logged out is an error (call {round})"
                    ),
                }
            }
        }
        // Fixed texts: the same call gives the same kind and message.
        assert_eq!(
            lib.playlists(0).await.err(),
            lib.playlists(0).await.err(),
            "provider contract: a library error is the same twice"
        );
        for round in 1..=2 {
            for bad in bad_entity_uris(info.id) {
                for (name, error) in [
                    ("artist", lib.artist(&bad).await.err()),
                    ("album", lib.album(&bad).await.err()),
                    (
                        "artist_releases",
                        lib.artist_releases(&bad, ReleaseGroup::All, 0).await.err(),
                    ),
                ] {
                    let Some(error) = error else {
                        panic!("provider contract: {name}({bad:?}) is an error (call {round})");
                    };
                    assert_fixed_text(
                        &error.message,
                        &format!("provider contract: {name}({bad:?})"),
                    );
                }
            }
        }
        let uris = vec!["x:track:1".to_owned(), String::new(), "y".to_owned()];
        if info.caps.likes {
            if let Ok(answer) = lib.liked(&uris).await {
                assert_eq!(
                    answer.len(),
                    uris.len(),
                    "provider contract: liked answers once per uri"
                );
            }
        } else {
            assert!(
                lib.liked(&uris).await.is_err(),
                "provider contract: liked without caps.likes is an error"
            );
            assert!(
                lib.set_liked("x:track:1", true).await.is_err(),
                "provider contract: set_liked without caps.likes is an error"
            );
            assert!(
                !lib.can_change_likes(),
                "provider contract: no caps.likes, no can_change_likes"
            );
        }
    }
}

/// Uris no provider may open as an artist or an album: empty, no scheme, another provider's
/// scheme, the provider's scheme with a missing or hostile id, and the wrong kind of entity.
fn bad_entity_uris(id: &str) -> Vec<String> {
    vec![
        String::new(),
        "no-such-id".to_owned(),
        "zz-other:artist:1".to_owned(),
        "zz-other:album:1".to_owned(),
        format!("{id}:artist:"),
        format!("{id}:album:"),
        format!("{id}:artist:no-such-artist"),
        format!("{id}:album:no-such-album"),
        format!("{id}:artist:../../x"),
        format!("{id}:track:1"),
        format!("{id}:artist:{}", "9".repeat(5000)),
    ]
}

/// Playback cases on a provider that can load tracks without a device or a login (the fake, the
/// demo). With `good: None` only the cases that need no loaded track run (a real provider
/// logged out).
///
/// | method | case |
/// |---|---|
/// | `load` | `bad` is an error with a fixed text, the same twice |
/// | `load` | after a bad load, `good` still loads (paused at the start; playing at 30 s) |
/// | `play`, `pause`, `stop`, `seek` | Ok after a good load; `seek(0)` and `seek(1000)`; `stop` twice |
/// | `preload` | `good` is Ok; `bad` never panics |
/// | `source_format` | none, or a trimmed non-empty text with no control character |
pub async fn run_provider_playback_contract(p: Arc<dyn Provider>, good: Option<&str>, bad: &str) {
    let playback = p.playback();
    let Err(error) = playback.load(bad, true, 0).await else {
        panic!("playback contract: loading {bad:?} is an error");
    };
    assert_fixed_text(&error.0, "playback contract: a bad load");
    assert_eq!(
        playback.load(bad, true, 0).await.err(),
        Some(error),
        "playback contract: a bad load gives the same error twice"
    );
    if let Some(label) = playback.source_format() {
        assert!(
            !label.is_empty() && label == label.trim() && !label.chars().any(char::is_control),
            "playback contract: source_format is a clean label ({label:?})"
        );
    }
    let Some(good) = good else { return };
    for (play, position) in [(false, 0), (true, 30_000)] {
        playback
            .load(good, play, position)
            .await
            .unwrap_or_else(|e| {
                panic!(
                    "playback contract: load good play={play} at {position}: {}",
                    e.0
                )
            });
    }
    playback
        .play()
        .await
        .unwrap_or_else(|e| panic!("playback contract: play: {}", e.0));
    playback
        .pause()
        .await
        .unwrap_or_else(|e| panic!("playback contract: pause: {}", e.0));
    for ms in [0, 1_000] {
        playback
            .seek(ms)
            .await
            .unwrap_or_else(|e| panic!("playback contract: seek {ms}: {}", e.0));
    }
    playback
        .preload(good)
        .await
        .unwrap_or_else(|e| panic!("playback contract: preload good: {}", e.0));
    let _ = playback.preload(bad).await;
    for round in 1..=2 {
        playback
            .stop()
            .await
            .unwrap_or_else(|e| panic!("playback contract: stop (call {round}): {}", e.0));
    }
}

/// Every item of a paged listing, following `next_offset` until it ends.
async fn walk<T, F, Fut>(what: &str, mut fetch: F) -> Vec<T>
where
    F: FnMut(u32) -> Fut,
    Fut: std::future::Future<Output = Result<(Vec<T>, Option<u32>), LibraryError>>,
{
    let (mut all, mut offset) = (Vec::new(), 0u32);
    for _ in 0..50 {
        let (items, next) = fetch(offset)
            .await
            .unwrap_or_else(|e| panic!("session contract: {what} at offset {offset}: {e}"));
        all.extend(items);
        match next {
            Some(n) => {
                assert!(
                    n > offset,
                    "session contract: {what}: next_offset moves forward ({offset} -> {n})"
                );
                offset = n;
            }
            None => return all,
        }
    }
    panic!("session contract: {what}: the listing never ends");
}

/// Logged-in cases on a provider that can log in without a network (the demo). The provider
/// starts logged out. `logins` is every `(step, input)` that logs a step in; `bad_login` a step
/// and input it must refuse.
///
/// | method | case |
/// |---|---|
/// | `auth.login`, `auth.logout` | an unknown step is an error (both) with a fixed text |
/// | `auth.login` | `bad_login` is an error and leaves the login steps logged out (skipped with no auth steps) |
/// | `auth.login` | every `logins` entry is Ok; afterwards `status` has every login step `LoggedIn` and equals the last answer (skipped with no auth steps) |
/// | `library.playlists`, `albums`, `liked_tracks` | the walk ends, ids and uris are unique, images <= `MAX_IMAGES`, `total` (when given) covers what was read |
/// | `library.playlist_items`, `album_tracks` | a readable playlist and an album list named tracks (an unreadable playlist is `NotReadable`); an unknown id is an error |
/// | `library.search` | an existing track name finds it; a nonsense query finds nothing |
/// | `library.artist`, `album`, `artist_releases` | with `caps.explore`: every artist and album ref of the walked tracks and albums (at most `MAX_ARTISTS` per item, at least one of each seen) resolves through the registry to this provider and opens: `artist(uri).artist.uri == uri`, popular <= `MAX_POPULAR`, images <= `MAX_IMAGES`, releases walk unique by uri, each group's kinds match (Albums: album or compilation, Singles: single, AppearsOn: appears-on); `album(uri).album.uri == uri` and lists tracks; a bad uri is an error |
/// | `library.liked`, `set_liked` | with `caps.save_albums`: an album uri answers one bool and, when `can_change_likes`, set then read back, both ways |
/// | `library.liked` | one answer per uri; an unsaved uri is `false` |
/// | `library.set_liked` | when `can_change_likes`: set then read back, both ways; otherwise an error |
/// | `auth.logout` | every step; then `status` is logged out and the library is an error again (skipped with no auth steps) |
///
/// A provider with no auth steps is always ready: the login and logout walk is skipped and the
/// library is walked as it is.
pub async fn run_provider_session_contract(
    p: Arc<dyn Provider>,
    logins: &[(&str, LoginInput)],
    bad_login: (&str, LoginInput),
) {
    let info = p.info();
    let auth: &dyn AuthFlow = p.auth();
    let all_in = |steps: &[AuthStepStatus], state: LoginState| {
        steps.len() == info.auth_steps.len() && steps.iter().all(|s| s.state == state)
    };
    // The same over the login steps only: a switch step keeps its own setting.
    let logins_in = |steps: &[AuthStepStatus], state: LoginState| {
        steps.len() == info.auth_steps.len()
            && steps.iter().all(|s| {
                s.state == state
                    || info
                        .auth_steps
                        .iter()
                        .any(|step| step.id == s.id && !step.is_login())
            })
    };

    for (what, result) in [
        (
            "login",
            auth.login("no-such-step", &LoginInput::default())
                .await
                .err(),
        ),
        ("logout", auth.logout("no-such-step").await.err()),
    ] {
        let Some(e) = result else {
            panic!("session contract: {what} of an unknown step is an error");
        };
        assert_fixed_text(
            &e.0,
            &format!("session contract: {what} of an unknown step"),
        );
    }
    let with_steps = !info.auth_steps.is_empty();
    if with_steps {
        let (step, input) = bad_login;
        assert!(
            auth.login(step, &input).await.is_err(),
            "session contract: the bad login is refused"
        );
        let status = auth.status().await.expect("session contract: status");
        assert!(
            logins_in(&status.steps, LoginState::LoggedOut),
            "session contract: a refused login leaves every login step logged out"
        );

        let mut last = None;
        for (step, input) in logins {
            last = Some(
                auth.login(step, input)
                    .await
                    .unwrap_or_else(|e| panic!("session contract: login {step:?}: {}", e.0)),
            );
        }
        let status = auth.status().await.expect("session contract: status");
        assert!(
            logins_in(&status.steps, LoginState::LoggedIn),
            "session contract: after the logins every login step is logged in"
        );
        assert_eq!(
            last.as_ref(),
            Some(&status),
            "session contract: status equals the last login's answer"
        );
    }

    let lib = p.library();
    if let Some(lib) = lib {
        session_library(&p, &info, lib).await;
    }

    for step in &info.auth_steps {
        auth.logout(step.id)
            .await
            .unwrap_or_else(|e| panic!("session contract: logout {:?}: {}", step.id, e.0));
    }
    let status = auth.status().await.expect("session contract: status");
    assert!(
        all_in(&status.steps, LoginState::LoggedOut),
        "session contract: after the logouts every step is logged out"
    );
    if let (Some(lib), true) = (lib, with_steps) {
        assert!(
            lib.playlists(0).await.is_err() && lib.albums(0).await.is_err(),
            "session contract: logged out again, the library is an error"
        );
    }
}

/// The logged-in library cases of [`run_provider_session_contract`].
async fn session_library(p: &Arc<dyn Provider>, info: &ProviderInfo, lib: &dyn Library) {
    let playlists = walk("playlists", |o| async move {
        lib.playlists(o).await.map(|p| (p.items, p.next_offset))
    })
    .await;
    let albums = walk("albums", |o| async move {
        lib.albums(o).await.map(|p| (p.items, p.next_offset))
    })
    .await;
    let liked_tracks = walk("liked_tracks", |o| async move {
        lib.liked_tracks(o).await.map(|p| (p.items, p.next_offset))
    })
    .await;
    assert!(
        !playlists.is_empty() && !albums.is_empty() && !liked_tracks.is_empty(),
        "session contract: the logged-in library has playlists, albums and liked tracks"
    );
    let unique = |ids: Vec<&str>, what: &str| {
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            ids.len(),
            "session contract: {what} are unique"
        );
    };
    unique(
        playlists.iter().map(|p| p.id.as_str()).collect(),
        "playlist ids",
    );
    unique(albums.iter().map(|a| a.id.as_str()).collect(), "album ids");
    unique(
        liked_tracks.iter().map(|t| t.uri.as_str()).collect(),
        "liked track uris",
    );
    assert!(
        playlists.iter().all(|p| p.images.len() <= MAX_IMAGES)
            && albums.iter().all(|a| a.images.len() <= MAX_IMAGES)
            && liked_tracks.iter().all(|t| t.images.len() <= MAX_IMAGES),
        "session contract: at most MAX_IMAGES images per item"
    );
    let total = lib.playlists(0).await.expect("playlists").total;
    assert!(
        total.is_none_or(|t| t as usize >= playlists.len()),
        "session contract: playlists total covers the walk"
    );

    let readable = playlists
        .iter()
        .find(|p| p.readable != Readable::Unreadable)
        .expect("session contract: a readable playlist");
    let tracks = walk("playlist_items", |o| async move {
        lib.playlist_items(&readable.id, o)
            .await
            .map(|p| (p.items, p.next_offset))
    })
    .await;
    assert!(
        !tracks.is_empty()
            && tracks
                .iter()
                .all(|t| !t.uri.is_empty() && !t.name.is_empty()),
        "session contract: a readable playlist lists named tracks"
    );
    if let Some(locked) = playlists
        .iter()
        .find(|p| p.readable == Readable::Unreadable)
    {
        assert_eq!(
            lib.playlist_items(&locked.id, 0).await.unwrap_err().kind,
            LibErrorKind::NotReadable,
            "session contract: an unreadable playlist is NotReadable"
        );
    }
    let first_album = &albums[0].id;
    let album_tracks = walk("album_tracks", |o| async move {
        lib.album_tracks(first_album, o)
            .await
            .map(|p| (p.items, p.next_offset))
    })
    .await;
    assert!(
        !album_tracks.is_empty(),
        "session contract: an album lists tracks"
    );
    assert!(
        lib.playlist_items("no-such-id", 0).await.is_err()
            && lib.album_tracks("no-such-id", 0).await.is_err(),
        "session contract: an unknown id is an error"
    );

    let wanted = &album_tracks[0];
    let found = walk("search", |o| async move {
        lib.search(&wanted.name, o)
            .await
            .map(|p| (p.items, p.next_offset))
    })
    .await;
    assert!(
        found.iter().any(|t| t.uri == wanted.uri),
        "session contract: search finds a known track by name"
    );
    let nothing = walk("search nothing", |o| async move {
        lib.search("zzzz-no-such-track-zzzz", o)
            .await
            .map(|p| (p.items, p.next_offset))
    })
    .await;
    assert!(
        nothing.is_empty(),
        "session contract: a nonsense search finds nothing"
    );

    if info.caps.explore {
        let seen: Vec<&Track> = liked_tracks
            .iter()
            .chain(tracks.iter())
            .chain(album_tracks.iter())
            .collect();
        session_explore(p, info, lib, &seen, &albums).await;
    }

    if info.caps.likes {
        let known = liked_tracks[0].uri.clone();
        let probe = "session:track:not-saved".to_owned();
        assert_eq!(
            lib.liked(&[known, probe.clone()]).await.ok(),
            Some(vec![true, false]),
            "session contract: liked answers once per uri, an unsaved one is false"
        );
        if lib.can_change_likes() {
            // Toggle a track the walk saw and that is not liked yet, so a provider that keys
            // likes by its own library items (not by any uri) takes part; none left, no toggle.
            let candidates: Vec<String> = album_tracks
                .iter()
                .chain(tracks.iter())
                .map(|t| t.uri.clone())
                .take(50)
                .collect();
            let states = lib
                .liked(&candidates)
                .await
                .expect("session contract: liked answers for the walked tracks");
            let target = candidates
                .iter()
                .zip(states)
                .find(|(_, liked)| !liked)
                .map(|(uri, _)| uri.clone());
            if let Some(target) = target {
                for want in [true, false] {
                    lib.set_liked(&target, want)
                        .await
                        .unwrap_or_else(|e| panic!("session contract: set_liked {want}: {e}"));
                    assert_eq!(
                        lib.liked(std::slice::from_ref(&target)).await.ok(),
                        Some(vec![want]),
                        "session contract: set_liked({want}) reads back"
                    );
                }
            }
        } else {
            assert!(
                lib.set_liked(&probe, true).await.is_err(),
                "session contract: set_liked without can_change_likes is an error"
            );
        }
    }
}

/// The explore cases of [`run_provider_session_contract`] (`caps.explore`).
async fn session_explore(
    p: &Arc<dyn Provider>,
    info: &ProviderInfo,
    lib: &dyn Library,
    tracks: &[&Track],
    albums: &[Album],
) {
    let mut registry = ProviderRegistry::new();
    registry
        .register(p.clone())
        .unwrap_or_else(|e| panic!("session contract: explore: registers: {e}"));
    let resolves = |uri: &str, what: &str| {
        assert_eq!(
            registry.resolve_uri(uri).map(|q| q.info().id),
            Some(info.id),
            "session contract: {what} {uri:?} resolves to the provider that made it"
        );
    };

    // The refs seen, unique by uri.
    let mut artists: Vec<EntityRef> = Vec::new();
    let mut album_refs: Vec<EntityRef> = Vec::new();
    let add = |into: &mut Vec<EntityRef>, r: &EntityRef| {
        if !r.uri.is_empty() && !into.iter().any(|x| x.uri == r.uri) {
            into.push(r.clone());
        }
    };
    for t in tracks {
        assert!(
            t.artists.len() <= MAX_ARTISTS,
            "session contract: a track names at most MAX_ARTISTS artists"
        );
        t.artists.iter().for_each(|r| add(&mut artists, r));
        if let Some(r) = &t.album_ref {
            add(&mut album_refs, r);
        }
    }
    for a in albums {
        assert!(
            a.artists.len() <= MAX_ARTISTS,
            "session contract: an album names at most MAX_ARTISTS artists"
        );
        a.artists.iter().for_each(|r| add(&mut artists, r));
        add(
            &mut album_refs,
            &EntityRef {
                uri: a.uri.clone(),
                name: a.name.clone(),
            },
        );
    }
    assert!(
        !artists.is_empty() && !album_refs.is_empty(),
        "session contract: with caps.explore the walked items carry artist and album refs"
    );
    artists.truncate(12);
    album_refs.truncate(12);

    let unique_albums = |items: &[Album], what: &str| {
        let mut uris: Vec<&str> = items.iter().map(|a| a.uri.as_str()).collect();
        assert!(
            uris.iter().all(|u| !u.is_empty()),
            "session contract: {what}: every release has a uri"
        );
        uris.sort_unstable();
        let n = uris.len();
        uris.dedup();
        assert_eq!(uris.len(), n, "session contract: {what} are unique by uri");
        assert!(
            items.iter().all(|a| a.images.len() <= MAX_IMAGES),
            "session contract: {what}: at most MAX_IMAGES images"
        );
    };

    for r in &artists {
        resolves(&r.uri, "an artist ref");
        let page = lib
            .artist(&r.uri)
            .await
            .unwrap_or_else(|e| panic!("session contract: artist({:?}): {e}", r.uri));
        assert_eq!(
            page.artist.uri, r.uri,
            "session contract: artist(uri).artist.uri is the uri"
        );
        assert!(
            !page.artist.name.is_empty() && page.artist.images.len() <= MAX_IMAGES,
            "session contract: an artist has a name and at most MAX_IMAGES images"
        );
        assert!(
            page.popular.len() <= MAX_POPULAR,
            "session contract: popular tracks are at most MAX_POPULAR"
        );
        unique_albums(&page.releases.items, "the first page of releases");

        let all = walk("artist_releases all", |o| async move {
            lib.artist_releases(&r.uri, ReleaseGroup::All, o)
                .await
                .map(|p| (p.items, p.next_offset))
        })
        .await;
        unique_albums(&all, "all releases");
        for (group, ok) in [
            (
                ReleaseGroup::Albums,
                &[AlbumKind::Album, AlbumKind::Compilation][..],
            ),
            (ReleaseGroup::Singles, &[AlbumKind::Single][..]),
            (ReleaseGroup::AppearsOn, &[AlbumKind::AppearsOn][..]),
        ] {
            let items = walk("artist_releases group", |o| async move {
                lib.artist_releases(&r.uri, group, o)
                    .await
                    .map(|p| (p.items, p.next_offset))
            })
            .await;
            unique_albums(&items, "a release group");
            assert!(
                items.iter().all(|a| ok.contains(&a.kind)),
                "session contract: the releases of {group:?} have a matching kind"
            );
        }
    }

    for r in &album_refs {
        resolves(&r.uri, "an album ref");
        let detail = lib
            .album(&r.uri)
            .await
            .unwrap_or_else(|e| panic!("session contract: album({:?}): {e}", r.uri));
        assert_eq!(
            detail.album.uri, r.uri,
            "session contract: album(uri).album.uri is the uri"
        );
        assert!(
            detail.album.images.len() <= MAX_IMAGES && !detail.tracks.items.is_empty(),
            "session contract: an opened album has at most MAX_IMAGES images and lists tracks"
        );
    }

    for bad in bad_entity_uris(info.id) {
        assert!(
            lib.artist(&bad).await.is_err()
                && lib.album(&bad).await.is_err()
                && lib
                    .artist_releases(&bad, ReleaseGroup::All, 0)
                    .await
                    .is_err(),
            "session contract: a bad uri ({bad:?}) is an error"
        );
    }
    // An artist uri is no album and an album uri no artist.
    assert!(
        lib.album(&artists[0].uri).await.is_err() && lib.artist(&album_refs[0].uri).await.is_err(),
        "session contract: an artist uri is not an album, an album uri is not an artist"
    );

    if info.caps.save_albums {
        let uri = album_refs[0].uri.clone();
        let one = lib.liked(std::slice::from_ref(&uri)).await;
        assert!(
            one.as_ref().is_ok_and(|v| v.len() == 1),
            "session contract: liked answers once for an album uri"
        );
        if lib.can_change_likes() {
            let was = one.unwrap()[0];
            for want in [!was, was] {
                lib.set_liked(&uri, want)
                    .await
                    .unwrap_or_else(|e| panic!("session contract: save album {want}: {e}"));
                assert_eq!(
                    lib.liked(std::slice::from_ref(&uri)).await.ok(),
                    Some(vec![want]),
                    "session contract: saving an album ({want}) reads back"
                );
            }
        }
    }
}

/// Store cases. The accounts used here (`contract-account-a`, `-b` and the empty name) must not
/// exist in `store` when the suite starts; it leaves none behind.
///
/// | method | case |
/// |---|---|
/// | `get` | a missing account is `Ok(None)` |
/// | `set`, `get` | set then get returns the same value |
/// | `set` | overwrite replaces; an empty secret is an error and keeps the old value |
/// | `set`, `get` | a 1000-character value and a unicode value round-trip (the Windows blob holds 1280 UTF-16 units) |
/// | `get`, `set`, `delete` | two accounts are independent: changing or deleting one leaves the other |
/// | `delete` | existing is `Ok(true)`, then `get` is `Ok(None)`; a second delete is `Ok(false)` |
/// | the empty account name | rejected (`set` is an error), or, when accepted, it round-trips and deletes like any other; never a panic |
pub fn run_credential_store_contract(store: &dyn CredentialStore) {
    const A: &str = "contract-account-a";
    const B: &str = "contract-account-b";

    fn read(store: &dyn CredentialStore, account: &str, case: &str) -> Option<String> {
        match store.get(account) {
            Ok(v) => v.map(|s| s.expose().to_owned()),
            Err(_) => panic!("store contract: {case}: get failed"),
        }
    }

    assert_eq!(
        read(store, A, "get of a missing account"),
        None,
        "store contract: get of a missing account is Ok(None)"
    );

    assert!(
        store.set(A, &Secret::new("one")).is_ok(),
        "store contract: set succeeds"
    );
    assert_eq!(
        read(store, A, "set then get"),
        Some("one".to_owned()),
        "store contract: set then get returns the same value"
    );

    assert!(
        store.set(A, &Secret::new("two")).is_ok(),
        "store contract: overwrite succeeds"
    );
    assert_eq!(
        read(store, A, "set overwrites"),
        Some("two".to_owned()),
        "store contract: set overwrites"
    );

    assert!(
        store.set(A, &Secret::new("")).is_err(),
        "store contract: set of an empty secret is an error"
    );
    assert_eq!(
        read(store, A, "empty set keeps the value"),
        Some("two".to_owned()),
        "store contract: a refused empty set keeps the old value"
    );

    for (case, value) in [
        ("a long value", "x".repeat(1000)),
        ("a unicode value", "pässwörd-日本語-\u{1F3B5}".to_owned()),
    ] {
        assert!(
            store.set(A, &Secret::new(value.clone())).is_ok(),
            "store contract: set of {case} succeeds"
        );
        assert_eq!(
            read(store, A, case),
            Some(value),
            "store contract: {case} round-trips"
        );
    }

    assert!(
        store.set(A, &Secret::new("a-value")).is_ok()
            && store.set(B, &Secret::new("b-value")).is_ok(),
        "store contract: two accounts are set"
    );
    assert!(
        store.set(A, &Secret::new("a-changed")).is_ok(),
        "store contract: one of two accounts is changed"
    );
    assert_eq!(
        read(store, B, "independent accounts"),
        Some("b-value".to_owned()),
        "store contract: changing one account leaves the other"
    );
    assert_eq!(
        store.delete(A),
        Ok(true),
        "store contract: delete of an existing account is Ok(true)"
    );
    assert_eq!(
        read(store, A, "get after delete"),
        None,
        "store contract: get after delete is Ok(None)"
    );
    assert_eq!(
        read(store, B, "independent delete"),
        Some("b-value".to_owned()),
        "store contract: deleting one account leaves the other"
    );

    assert_eq!(
        store.delete(A),
        Ok(false),
        "store contract: delete of a missing account is Ok(false)"
    );
    assert_eq!(
        store.delete(B),
        Ok(true),
        "store contract: the second account deletes"
    );
    assert_eq!(
        store.delete(B),
        Ok(false),
        "store contract: delete twice, the second is Ok(false)"
    );

    // The empty account name: rejected, or a normal account.
    if store.set("", &Secret::new("empty-name")).is_ok() {
        assert_eq!(
            read(store, "", "empty name"),
            Some("empty-name".to_owned()),
            "store contract: an accepted empty account name round-trips"
        );
        assert_eq!(
            store.delete(""),
            Ok(true),
            "store contract: an accepted empty account name deletes"
        );
    }
    let _ = store.get("");
}

/// An output adapter that only keeps the renderer it is given: the way a suite gets a real
/// [`Renderer`] (its constructor is crate-private) wired to an engine whose source it can write to.
struct Lender(Arc<Mutex<Option<Renderer>>>);

impl AudioOutput for Lender {
    fn devices(&mut self) -> Result<Vec<OutputDevice>, OutputError> {
        Ok(Vec::new())
    }

    fn open(
        &mut self,
        _request: &OutputRequest,
        renderer: Renderer,
    ) -> Result<OutputInfo, OpenFailed> {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(renderer);
        Ok(OutputInfo {
            host_api: "lender".into(),
            device: "lender".into(),
            mode: OutputMode::Shared,
            format: DeviceFormat {
                rate: LENDER_RATE,
                channels: 2,
                sample: SampleFormat::F32,
                valid_bits: 32,
            },
            buffer_frames: None,
            latency_ms: None,
        })
    }

    fn close(&mut self) -> Option<Renderer> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

const LENDER_RATE: u32 = 44_100;

/// An engine on a [`Lender`] that is "open" (so a source can write), and the renderer it lent.
struct Lent {
    engine: Engine,
    source: SourceWriter,
    renderer: Option<Renderer>,
}

fn lend() -> Lent {
    let slot = Arc::new(Mutex::new(None));
    let parts = Engine::new(
        EngineConfig::default(),
        Some(Box::new(Lender(slot.clone()))),
    );
    let mut engine = parts.engine;
    engine
        .open(&OutputRequest::default())
        .unwrap_or_else(|e| panic!("audio output contract: the lender opens: {e}"));
    let renderer = slot.lock().unwrap_or_else(|e| e.into_inner()).take();
    Lent {
        engine,
        source: parts.source,
        renderer,
    }
}

/// Whether `format` is one an engine renderer accepts (`Renderer::prepare` refuses the rest).
fn is_valid_format(format: &DeviceFormat) -> bool {
    (8_000..=384_000).contains(&format.rate)
        && format.channels >= 1
        && matches!(
            (format.sample, format.valid_bits),
            (SampleFormat::F32, 32) | (SampleFormat::I16, 16) | (SampleFormat::I32, 24 | 32)
        )
}

/// Audio output cases. `pump` runs the output's callback for a short while from the test
/// thread (a fake renders a few blocks; a clocked output sleeps while its own thread renders);
/// `refused` is a request the output must not open (an unknown device), `None` when it has none.
/// A real device is the user's to run (hard rule 2).
///
/// | method | case |
/// |---|---|
/// | `modes` | contains `Shared`, no duplicate |
/// | `devices` | Ok; ids unique and non-empty; at most one default |
/// | `close` | with nothing open: `None`, twice |
/// | `open` | the default request: `Ok(info)`; format valid (rate, channels, bits agree with the sample type); host and device named; mode is one of `modes()` |
/// | `open` | a `Shared` request opens in `Shared` |
/// | `open` + callback | the renderer was prepared before it renders: after the pump the engine counts callbacks and frames |
/// | `open` while open | `OpenFailed(Busy)` with the renderer back; the first stream keeps running |
/// | `open` refused | `OpenFailed` with the renderer back, and that renderer opens afterwards |
/// | `close` | returns the renderer; the callback stops; a second close is `None` |
/// | `open` after `close` | the returned renderer opens again |
pub fn run_audio_output_contract(
    output: &mut dyn AudioOutput,
    pump: &mut dyn FnMut(),
    refused: Option<&OutputRequest>,
) {
    let modes = output.modes();
    assert!(
        modes.contains(&OutputMode::Shared),
        "audio output contract: modes contain Shared"
    );
    for (i, m) in modes.iter().enumerate() {
        assert!(
            !modes[..i].contains(m),
            "audio output contract: modes have no duplicate"
        );
    }
    let devices = output
        .devices()
        .unwrap_or_else(|e| panic!("audio output contract: devices is Ok: {e}"));
    for (i, d) in devices.iter().enumerate() {
        assert!(
            !d.id.is_empty() && !devices[..i].iter().any(|o| o.id == d.id),
            "audio output contract: device ids are non-empty and unique"
        );
    }
    assert!(
        devices.iter().filter(|d| d.is_default).count() <= 1,
        "audio output contract: at most one default device"
    );
    for round in 1..=2 {
        assert!(
            output.close().is_none(),
            "audio output contract: close with nothing open is None (call {round})"
        );
    }

    let mut first = lend();
    let renderer = first.renderer.take().expect("lent renderer");
    let info = output
        .open(&OutputRequest::default(), renderer)
        .unwrap_or_else(|f| {
            panic!(
                "audio output contract: the default request opens: {}",
                f.error
            )
        });
    assert!(
        is_valid_format(&info.format),
        "audio output contract: the opened format is valid ({:?})",
        info.format
    );
    assert!(
        !info.host_api.is_empty() && !info.device.is_empty(),
        "audio output contract: the opened stream names its host and device"
    );
    assert!(
        output.modes().contains(&info.mode),
        "audio output contract: the opened mode is one of modes()"
    );

    // The callback runs only on a prepared renderer: a status with callbacks proves it.
    first
        .source
        .start_track(StreamFormat {
            rate: LENDER_RATE,
            channels: 2,
            source: SourceInfo {
                codec: "pcm",
                bits: Some(16),
                bitrate_kbps: None,
                lossless: true,
                replay_gain: None,
            },
        })
        .unwrap_or_else(|e| panic!("audio output contract: a track starts: {e}"));
    first
        .source
        .write(&vec![0.25; 2 * 4096])
        .unwrap_or_else(|e| panic!("audio output contract: frames are written: {e}"));
    pump();
    let running = first.engine.status();
    assert!(
        running.callbacks > 0 && running.frames > 0,
        "audio output contract: the renderer rendered after open (prepare came first)"
    );

    // A second open while open fails with Busy and hands its renderer back.
    let mut second = lend();
    let spare = second.renderer.take().expect("lent renderer");
    match output.open(&OutputRequest::default(), spare) {
        Err(OpenFailed { error, renderer }) => {
            assert_eq!(
                error,
                OutputError::Busy,
                "audio output contract: open while open is Busy"
            );
            second.renderer = Some(renderer);
        }
        Ok(_) => panic!("audio output contract: open while open is an error"),
    }
    let before = first.engine.status().callbacks;
    pump();
    assert!(
        first.engine.status().callbacks > before,
        "audio output contract: a refused second open leaves the first stream running"
    );

    // Close gives the renderer back and stops the callback.
    let Some(back) = output.close() else {
        panic!("audio output contract: close returns the renderer");
    };
    let stopped = first.engine.status().callbacks;
    pump();
    assert_eq!(
        first.engine.status().callbacks,
        stopped,
        "audio output contract: no callback after close"
    );
    assert!(
        output.close().is_none(),
        "audio output contract: a second close is None"
    );

    // The renderer that came back opens again, in Shared when asked.
    let shared = OutputRequest {
        mode: OutputMode::Shared,
        ..OutputRequest::default()
    };
    let info = output.open(&shared, back).unwrap_or_else(|f| {
        panic!(
            "audio output contract: the returned renderer reopens: {}",
            f.error
        )
    });
    assert_eq!(
        info.mode,
        OutputMode::Shared,
        "audio output contract: a Shared request opens in Shared"
    );
    let Some(back) = output.close() else {
        panic!("audio output contract: close after the reopen returns the renderer");
    };

    if let Some(refused) = refused {
        match output.open(refused, back) {
            Err(OpenFailed { renderer, .. }) => {
                let info = output
                    .open(&OutputRequest::default(), renderer)
                    .unwrap_or_else(|f| {
                        panic!(
                            "audio output contract: after a refusal the renderer opens: {}",
                            f.error
                        )
                    });
                assert!(is_valid_format(&info.format));
                assert!(
                    output.close().is_some(),
                    "audio output contract: close after the refusal returns the renderer"
                );
            }
            Ok(_) => panic!("audio output contract: the refused request {refused:?} opened"),
        }
    }
    drop(second);
}
/// The errors an engine's `load` may return: file-level failures. The name, folder and read
/// errors belong to a [`SkinSource`], never to a pure engine.
fn is_engine_error(e: SkinError) -> bool {
    matches!(
        e,
        SkinError::TooLarge
            | SkinError::UnknownFormat
            | SkinError::Malformed
            | SkinError::Unsupported
    )
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

/// Skin engine cases: `good` is a valid sample file of the engine. Sync: the skin seams are sync.
/// Synthetic input only (hard rule 4); nothing touches a file system.
///
/// | method | case |
/// |---|---|
/// | `id`, `extensions` | `id` is `^[a-z][a-z0-9-]{0,31}$`; `extensions` is not empty and each is `^[a-z0-9]{1,8}$` |
/// | `detect` | decides on the bytes alone: true for `good`, false for empty input, zeros and noise; the same answer twice; never a panic on any corpus entry |
/// | `load` | `good` loads and validates; twice gives an equal skin; panel ids are unique and the first panel is visible at start |
/// | `load` (caps) | the loaded skin holds the core caps (panels, widgets per panel, images, fonts, glyphs, panel sides); `run_skin_caps_contract` runs each cap at the limit and one over |
/// | `load` (determinism) | no state between calls: `good`, then garbage, then `good` again gives the same skin |
/// | `load` (malformed) | truncations, byte flips (three masks), zero runs, noise and tiny inputs give `Err` or a valid skin, never a panic |
/// | `load` (errors) | every `Err` is `TooLarge`, `UnknownFormat`, `Malformed` or `Unsupported` (name, folder and read errors are a source's), with a fixed text |
/// | `load` (size) | `MAX_SKIN_BYTES` of zeros is not `TooLarge`; one byte more is `TooLarge`, also when it starts like `good` |
pub fn run_skin_engine_contract(engine: &dyn SkinEngine, good: &[u8]) {
    assert!(
        is_valid_engine_id(engine.id()),
        "skin engine contract: id is a valid engine id"
    );
    let extensions = engine.extensions();
    assert!(
        !extensions.is_empty()
            && extensions.iter().all(|e| {
                (1..=8).contains(&e.len())
                    && e.bytes()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
            }),
        "skin engine contract: extensions are not empty and each is 1 to 8 lowercase a-z0-9"
    );
    assert!(
        !good.is_empty(),
        "skin engine contract: the good sample is not empty"
    );
    assert!(
        engine.detect(good),
        "skin engine contract: detect accepts a good skin"
    );
    assert_eq!(
        engine.detect(good),
        engine.detect(good),
        "skin engine contract: detect gives the same answer twice"
    );
    assert!(
        !engine.detect(&[]),
        "skin engine contract: detect refuses empty input"
    );
    assert!(
        !engine.detect(&[0; 4096]) && !engine.detect(&lcg_bytes(4096, 7)),
        "skin engine contract: detect refuses zeros and noise"
    );

    let first = engine
        .load(good)
        .unwrap_or_else(|e| panic!("skin engine contract: a good skin loads: {e}"));
    first
        .validate()
        .unwrap_or_else(|e| panic!("skin engine contract: a loaded skin validates: {e}"));
    let second = engine
        .load(good)
        .unwrap_or_else(|e| panic!("skin engine contract: a good skin loads twice: {e}"));
    assert_eq!(
        first, second,
        "skin engine contract: the same bytes give an equal skin"
    );

    let mut seen: Vec<&PanelId> = Vec::new();
    for panel in &first.panels {
        assert!(
            !seen.contains(&&panel.id),
            "skin engine contract: panel ids are unique"
        );
        seen.push(&panel.id);
    }
    assert!(
        first.panels[0].visible_at_start,
        "skin engine contract: the first panel is visible at start"
    );
    assert_skin_within_caps(&first);

    let len = good.len();
    let mut corpus: Vec<Vec<u8>> = vec![Vec::new(), vec![0], vec![0; 64 * 1024]];
    corpus.push(lcg_bytes(4096, 1));
    corpus.push(lcg_bytes(4096, 2));
    for n in 1..=8 {
        corpus.push(good[..n.min(len)].to_vec());
    }
    for i in 0..64 {
        corpus.push(good[..len * i / 64].to_vec());
    }
    for k in len.saturating_sub(16)..len {
        corpus.push(good[..k].to_vec());
    }
    for i in 0..64 {
        for mask in [0xFFu8, 0x01, 0x80] {
            let mut flipped = good.to_vec();
            flipped[len * i / 64] ^= mask;
            corpus.push(flipped);
        }
    }
    // A zero run of 32 bytes at 16 places, and the sample followed by noise.
    for i in 0..16 {
        let mut zeroed = good.to_vec();
        let at = len * i / 16;
        let end = (at + 32).min(len);
        zeroed[at..end].fill(0);
        corpus.push(zeroed);
    }
    let mut tailed = good.to_vec();
    tailed.extend(lcg_bytes(512, 3));
    corpus.push(tailed);

    for (i, bytes) in corpus.iter().enumerate() {
        let _ = engine.detect(bytes);
        match engine.load(bytes) {
            Ok(skin) => {
                assert!(
                    skin.validate().is_ok(),
                    "skin engine contract: malformed input {i} gives Err or a valid skin"
                );
            }
            Err(e) => {
                assert!(
                    is_engine_error(e),
                    "skin engine contract: malformed input {i} gives a file-level error, not {e:?}"
                );
                assert_fixed_text(&e.to_string(), "skin engine contract");
            }
        }
    }

    // The engine keeps no state between calls.
    let _ = engine.load(&lcg_bytes(2048, 5));
    let third = engine
        .load(good)
        .unwrap_or_else(|e| panic!("skin engine contract: a good skin loads after garbage: {e}"));
    assert_eq!(
        first, third,
        "skin engine contract: garbage in between changes nothing"
    );

    let at_cap = vec![0u8; MAX_SKIN_BYTES as usize];
    assert!(
        !matches!(engine.load(&at_cap), Err(SkinError::TooLarge)),
        "skin engine contract: MAX_SKIN_BYTES of input is not TooLarge"
    );
    drop(at_cap);
    let big = vec![0u8; MAX_SKIN_BYTES as usize + 1];
    assert!(
        matches!(engine.load(&big), Err(SkinError::TooLarge)),
        "skin engine contract: a file over MAX_SKIN_BYTES is TooLarge"
    );
    drop(big);
    let mut padded = good.to_vec();
    padded.resize(MAX_SKIN_BYTES as usize + 1, 0);
    assert!(
        matches!(engine.load(&padded), Err(SkinError::TooLarge)),
        "skin engine contract: a good skin padded over MAX_SKIN_BYTES is TooLarge"
    );
}

/// A loaded skin holds the caps of core `Skin::validate`, counted here without calling it.
fn assert_skin_within_caps(skin: &Skin) {
    assert!(
        (1..=MAX_PANELS).contains(&skin.panels.len()),
        "skin engine contract: 1 to MAX_PANELS panels"
    );
    assert!(
        skin.images.len() <= MAX_SKIN_IMAGES,
        "skin engine contract: at most skin MAX_IMAGES images"
    );
    assert!(
        skin.fonts.len() <= MAX_FONTS && skin.fonts.iter().all(|f| f.glyphs.len() <= MAX_GLYPHS),
        "skin engine contract: at most MAX_FONTS fonts and MAX_GLYPHS glyphs each"
    );
    for panel in &skin.panels {
        assert!(
            panel.widgets.len() <= MAX_WIDGETS_PER_PANEL,
            "skin engine contract: at most MAX_WIDGETS_PER_PANEL widgets"
        );
        let side = 1..=MAX_PANEL_SIDE;
        assert!(
            side.contains(&panel.size.w) && side.contains(&panel.size.h),
            "skin engine contract: panel sides are 1 to MAX_PANEL_SIDE"
        );
    }
}

/// `Skin::validate` caps, each at its limit (valid) and one over (`Malformed`), on skins built
/// from `template` (any valid skin of an engine, with an image and a widget in its first panel).
/// The core's own tests cover the rules; this runs them through the model an engine really
/// produced, so a skin carrying a field the caps do not meet is found.
///
/// | cap | at the limit | one over |
/// |---|---|---|
/// | panels (`MAX_PANELS`) | valid | `Malformed` |
/// | widgets per panel (`MAX_WIDGETS_PER_PANEL`) | valid | `Malformed` |
/// | images (`skin::MAX_IMAGES`) | valid | `Malformed` |
/// | fonts (`MAX_FONTS`) | valid | `Malformed` |
/// | glyphs per font (`MAX_GLYPHS`) | valid | `Malformed` |
/// | panel side (`MAX_PANEL_SIDE`) | valid | `Malformed`; a side of 0 too |
/// | polygons (`MAX_POLYGONS`) and points (`MAX_POINTS`) | valid | `Malformed` |
/// | palette colours (`MAX_VIS_COLOURS`) | valid | `Malformed`; an empty list too |
/// | a duplicate panel id; a first panel hidden at start | | `Malformed` |
pub fn run_skin_caps_contract(template: &Skin) {
    template
        .validate()
        .unwrap_or_else(|e| panic!("skin caps contract: the template validates: {e}"));
    assert!(
        !template.images.is_empty() && !template.panels[0].widgets.is_empty(),
        "skin caps contract: the template has an image and a widget in its first panel"
    );

    fn check(template: &Skin, what: &str, at_limit: impl Fn(&mut Skin), over: impl Fn(&mut Skin)) {
        let mut ok = template.clone();
        at_limit(&mut ok);
        assert_eq!(
            ok.validate(),
            Ok(()),
            "skin caps contract: {what} at the limit is valid"
        );
        let mut bad = template.clone();
        over(&mut bad);
        assert_eq!(
            bad.validate(),
            Err(SkinError::Malformed),
            "skin caps contract: {what} one over is Malformed"
        );
    }

    // `total` panels (at least the template's own): copies of the first, hidden, after the rest.
    let panels = |s: &mut Skin, total: usize| {
        let proto = s.panels[0].clone();
        for i in s.panels.len()..total {
            let mut p = proto.clone();
            p.id = PanelId::new(&format!("cap{i}")).expect("a valid panel id");
            p.dock = Dock::default();
            p.visible_at_start = false;
            s.panels.push(p);
        }
    };
    check(
        template,
        "the panel count",
        |s| panels(s, MAX_PANELS),
        |s| panels(s, MAX_PANELS + 1),
    );
    let widgets = |s: &mut Skin, n: usize| {
        let w = s.panels[0].widgets[0].clone();
        s.panels[0].widgets = vec![w; n];
    };
    check(
        template,
        "the widgets of a panel",
        |s| widgets(s, MAX_WIDGETS_PER_PANEL),
        |s| widgets(s, MAX_WIDGETS_PER_PANEL + 1),
    );
    let images = |s: &mut Skin, n: usize| {
        let img = s.images[0].clone();
        s.images.resize(n, img);
    };
    check(
        template,
        "the image count",
        |s| images(s, MAX_SKIN_IMAGES),
        |s| images(s, MAX_SKIN_IMAGES + 1),
    );
    let one = Size { w: 1, h: 1 };
    let sprite = SpriteRect {
        image: ImageId(0),
        rect: Rect {
            x: 0,
            y: 0,
            w: 1,
            h: 1,
        },
    };
    let blank = BitmapFont {
        cell: one,
        glyphs: Vec::new(),
        fallback: None,
    };
    let fonts = |s: &mut Skin, n: usize| s.fonts.resize(n, blank.clone());
    check(
        template,
        "the font count",
        |s| fonts(s, MAX_FONTS),
        |s| fonts(s, MAX_FONTS + 1),
    );
    let glyphs = |s: &mut Skin, n: usize| {
        fonts(s, s.fonts.len().max(1));
        s.fonts[0].glyphs = vec![('a', sprite); n];
    };
    check(
        template,
        "the glyphs of a font",
        |s| glyphs(s, MAX_GLYPHS),
        |s| glyphs(s, MAX_GLYPHS + 1),
    );
    let side = |s: &mut Skin, w: u32, h: u32| {
        s.panels[0].resize = None;
        s.panels[0].size = Size { w, h };
    };
    check(
        template,
        "the panel side",
        |s| side(s, MAX_PANEL_SIDE, MAX_PANEL_SIDE),
        |s| side(s, MAX_PANEL_SIDE + 1, 16),
    );
    let mut zero = template.clone();
    side(&mut zero, 0, 16);
    zero.panels[0].widgets.clear();
    assert_eq!(
        zero.validate(),
        Err(SkinError::Malformed),
        "skin caps contract: a panel side of 0 is Malformed"
    );
    // `n` polygons of `m` points each, inside a 64 by 64 panel.
    let polys = |s: &mut Skin, n: usize, m: usize| {
        side(s, 64, 64);
        s.panels[0].widgets.clear();
        let poly = vec![Point { x: 1, y: 1 }; m];
        s.panels[0].shape = Shape::Polygons(vec![poly; n]);
    };
    check(
        template,
        "the polygon count",
        |s| polys(s, MAX_POLYGONS, 3),
        |s| polys(s, MAX_POLYGONS + 1, 3),
    );
    check(
        template,
        "the point count",
        |s| polys(s, MAX_POLYGONS, MAX_POINTS / MAX_POLYGONS),
        |s| polys(s, MAX_POLYGONS, MAX_POINTS / MAX_POLYGONS + 1),
    );
    let palette = |s: &mut Skin, bars: usize, scope: usize| {
        let c = Rgb { r: 1, g: 2, b: 3 };
        s.visualiser = Some(VisPalette {
            background: c,
            grid: c,
            bars: vec![c; bars],
            scope: vec![c; scope],
            peak: c,
        });
    };
    check(
        template,
        "the bar colours",
        |s| palette(s, MAX_VIS_COLOURS, 1),
        |s| palette(s, MAX_VIS_COLOURS + 1, 1),
    );
    check(
        template,
        "the scope colours",
        |s| palette(s, 1, MAX_VIS_COLOURS),
        |s| palette(s, 1, MAX_VIS_COLOURS + 1),
    );
    let mut empty_bars = template.clone();
    palette(&mut empty_bars, 0, 1);
    assert_eq!(
        empty_bars.validate(),
        Err(SkinError::Malformed),
        "skin caps contract: an empty bar palette is Malformed"
    );

    let mut duplicate = template.clone();
    let mut twin = duplicate.panels[0].clone();
    twin.dock = Dock::default();
    duplicate.panels.push(twin);
    assert_eq!(
        duplicate.validate(),
        Err(SkinError::Malformed),
        "skin caps contract: a duplicate panel id is Malformed"
    );
    let mut hidden = template.clone();
    hidden.panels[0].visible_at_start = false;
    assert_eq!(
        hidden.validate(),
        Err(SkinError::Malformed),
        "skin caps contract: a hidden first panel is Malformed"
    );
}
/// Panel windows cases for any implementation, the real host included (needs `count() >= 2` and
/// at least one work area; restores what it changes). Sync, no timers. Cases that need to read
/// back a flag or move a hidden window are in [`run_fake_windows_contract`], which runs on
/// `FakeWindows` only.
///
/// | method | case |
/// |---|---|
/// | `count` | at least two windows |
/// | `frame` | every panel's frame is `Some` and not empty; `count` and `usize::MAX` are `None`; a hidden panel still has its frame |
/// | `is_visible` | out of range (`count`, `usize::MAX`) is `false` |
/// | `work_areas` | at least one, each with a positive size |
/// | `pointer` | callable twice; no value is required |
/// | `units_per_pixel` | finite, positive, the same twice |
/// | `move_panels` | one move sets the origin and keeps the size; a batch moves both and keeps both sizes; an empty batch is `Ok` and changes nothing |
/// | `move_panels` (refused) | an out-of-range index anywhere in the batch (last, first, alone, `usize::MAX`) is `Err` and nothing moves |
/// | `resize_panel` | panel 1 to twice its frame size is `Ok`, its frame has that size and the same origin; resized back it has the original frame |
/// | `resize_panel` (refused) | an out-of-range index (`count`, `usize::MAX`), a width of 0 and a height of -1 are `Err` and every frame stays |
/// | `set_visible` | hiding a member does not hide the leader; hide and show are idempotent; out of range does nothing |
/// | `set_docked`, `set_always_on_top` | on and off, twice, and out of range, do not panic and leave the frames alone |
pub fn run_panel_windows_contract(windows: &mut dyn PanelWindows) {
    let count = windows.count();
    assert!(
        count >= 2,
        "panel windows contract: needs at least two panel windows"
    );
    let areas = windows.work_areas();
    assert!(
        !areas.is_empty() && areas.iter().all(|a| a.w > 0 && a.h > 0),
        "panel windows contract: at least one work area, each with a positive size"
    );
    let area = areas[0];
    let at = |dx: i32, dy: i32| ScreenPoint {
        x: area.x + dx,
        y: area.y + dy,
    };

    let mut original = Vec::new();
    for i in 0..count {
        let f = windows
            .frame(i)
            .unwrap_or_else(|| panic!("panel windows contract: frame {i} is Some"));
        assert!(
            f.w > 0 && f.h > 0,
            "panel windows contract: frame {i} is not empty"
        );
        original.push(f);
    }
    assert!(
        windows.frame(count).is_none() && windows.frame(usize::MAX).is_none(),
        "panel windows contract: frame out of range is None"
    );
    assert!(
        !windows.is_visible(count) && !windows.is_visible(usize::MAX),
        "panel windows contract: an out-of-range panel is not visible"
    );
    let _ = (windows.pointer(), windows.pointer());

    let p = at(40, 40);
    windows
        .move_panels(&[(0, p)])
        .expect("panel windows contract: a move of panel 0 succeeds");
    let f0 = windows.frame(0).expect("panel windows contract: frame 0");
    assert_eq!(
        (f0.x, f0.y, f0.w, f0.h),
        (p.x, p.y, original[0].w, original[0].h),
        "panel windows contract: a move sets the origin and keeps the size"
    );

    let (a, b) = (at(60, 70), at(90, 100));
    windows
        .move_panels(&[(0, a), (1, b)])
        .expect("panel windows contract: a batch move succeeds");
    let f0 = windows.frame(0).expect("panel windows contract: frame 0");
    let f1 = windows.frame(1).expect("panel windows contract: frame 1");
    assert_eq!(
        ((f0.x, f0.y), (f1.x, f1.y)),
        ((a.x, a.y), (b.x, b.y)),
        "panel windows contract: one call moves both panels"
    );
    assert_eq!(
        ((f0.w, f0.h), (f1.w, f1.h)),
        (
            (original[0].w, original[0].h),
            (original[1].w, original[1].h)
        ),
        "panel windows contract: a batch move keeps both sizes"
    );

    let settled: Vec<Option<ScreenRect>> = (0..count).map(|i| windows.frame(i)).collect();
    assert!(
        windows.move_panels(&[]).is_ok(),
        "panel windows contract: an empty batch succeeds"
    );
    let frames = |w: &dyn PanelWindows| (0..count).map(|i| w.frame(i)).collect::<Vec<_>>();
    assert_eq!(
        frames(windows),
        settled,
        "panel windows contract: an empty batch changes nothing"
    );
    let q = at(20, 20);
    for (what, batch) in [
        ("last", vec![(0, q), (count, q)]),
        ("first", vec![(count, q), (0, q)]),
        ("alone", vec![(count, q)]),
        ("usize::MAX", vec![(0, q), (usize::MAX, q)]),
    ] {
        assert!(
            windows.move_panels(&batch).is_err(),
            "panel windows contract: an out-of-range index ({what}) is an error"
        );
        assert_eq!(
            frames(windows),
            settled,
            "panel windows contract: a refused move ({what}) changes nothing"
        );
    }

    let before = windows
        .frame(1)
        .expect("panel windows contract: frame 1 before a resize");
    windows
        .resize_panel(1, before.w * 2, before.h * 2)
        .expect("panel windows contract: a resize of panel 1 succeeds");
    assert_eq!(
        windows.frame(1),
        Some(ScreenRect {
            w: before.w * 2,
            h: before.h * 2,
            ..before
        }),
        "panel windows contract: a resize sets the size and keeps the origin"
    );
    windows
        .resize_panel(1, before.w, before.h)
        .expect("panel windows contract: a resize back succeeds");
    assert_eq!(
        windows.frame(1),
        Some(before),
        "panel windows contract: resized back, the frame is the original"
    );
    let settled = frames(windows);
    for (what, panel, w, h) in [
        ("count", count, before.w, before.h),
        ("usize::MAX", usize::MAX, before.w, before.h),
        ("width 0", 1, 0, before.h),
        ("height -1", 1, before.w, -1),
    ] {
        assert!(
            windows.resize_panel(panel, w, h).is_err(),
            "panel windows contract: a refused resize ({what}) is an error"
        );
        assert_eq!(
            frames(windows),
            settled,
            "panel windows contract: a refused resize ({what}) changes nothing"
        );
    }

    let (was0, was1) = (windows.is_visible(0), windows.is_visible(1));
    windows.set_visible(1, false);
    windows.set_visible(1, false);
    assert!(
        !windows.is_visible(1),
        "panel windows contract: a hidden panel is not visible (twice)"
    );
    assert_eq!(
        windows.is_visible(0),
        was0,
        "panel windows contract: hiding a member never hides the leader"
    );
    let hidden = windows
        .frame(1)
        .expect("panel windows contract: a hidden panel still has a frame");
    assert!(
        hidden.w > 0 && hidden.h > 0,
        "panel windows contract: a hidden panel's frame is not empty"
    );
    windows.set_visible(1, true);
    windows.set_visible(1, true);
    assert!(
        windows.is_visible(1),
        "panel windows contract: a shown panel is visible (twice)"
    );
    windows.set_visible(1, was1);

    let upp = windows.units_per_pixel();
    assert!(
        upp.is_finite() && upp > 0.0,
        "panel windows contract: units_per_pixel is finite and positive"
    );
    assert_eq!(
        upp,
        windows.units_per_pixel(),
        "panel windows contract: units_per_pixel is the same twice"
    );

    let before_flags = frames(windows);
    for _ in 0..2 {
        windows.set_docked(1, true);
        windows.set_docked(1, false);
        windows.set_always_on_top(true);
        windows.set_always_on_top(false);
    }
    windows.set_visible(count, true);
    windows.set_visible(usize::MAX, false);
    windows.set_docked(count, false);
    windows.set_docked(usize::MAX, true);
    assert_eq!(
        frames(windows),
        before_flags,
        "panel windows contract: the flag setters and out-of-range calls leave the frames alone"
    );

    let restore: Vec<(usize, ScreenPoint)> = original
        .iter()
        .enumerate()
        .map(|(i, f)| (i, ScreenPoint { x: f.x, y: f.y }))
        .collect();
    windows
        .move_panels(&restore)
        .expect("panel windows contract: the frames can be put back");
}

/// Cases only the fake can show, because they read back what the real host keeps in the OS or
/// would disturb the user's desktop (a minimised window, a moved hidden one). Needs
/// `count() >= 2`; the fake starts every panel visible, undocked, not on top, not minimised.
///
/// | method | case |
/// |---|---|
/// | start | all visible, none docked, not on top, not minimised |
/// | `set_docked` | the flag reads back, per panel; out of range changes none |
/// | `set_always_on_top` | on and off read back |
/// | `minimise` | reads back (and is idempotent) |
/// | `set_visible` | hides only that panel; out of range hides none |
/// | `move_panels` | a hidden panel moves and keeps its size; a repeated index: the last move wins; an empty batch is `Ok` |
/// | `resize_panel` | a hidden panel resizes to exactly the given size and keeps its origin; a refused call changes nothing |
/// | `pointer` | `None` by default, then what was set, then `None` again |
pub fn run_fake_windows_contract(windows: &mut FakeWindows) {
    let count = windows.count();
    assert!(
        count >= 2,
        "fake windows contract: needs at least two panel windows"
    );
    for i in 0..count {
        assert!(
            windows.is_visible(i) && !windows.docked(i),
            "fake windows contract: panel {i} starts visible and undocked"
        );
    }
    assert!(
        !windows.on_top() && !windows.minimised(),
        "fake windows contract: starts not on top and not minimised"
    );
    assert_eq!(
        windows.pointer(),
        None,
        "fake windows contract: no pointer by default"
    );

    windows.set_docked(1, true);
    assert!(
        windows.docked(1) && !windows.docked(0),
        "fake windows contract: docked reads back for that panel only"
    );
    windows.set_docked(count, true);
    windows.set_docked(usize::MAX, true);
    assert!(
        (0..count).filter(|&i| windows.docked(i)).count() == 1 && !windows.docked(count),
        "fake windows contract: an out-of-range docked call changes nothing"
    );
    windows.set_docked(1, false);
    assert!(!windows.docked(1), "fake windows contract: docked clears");

    windows.set_always_on_top(true);
    assert!(windows.on_top(), "fake windows contract: on top reads back");
    windows.set_always_on_top(false);
    assert!(!windows.on_top(), "fake windows contract: on top clears");
    windows.minimise();
    windows.minimise();
    assert!(
        windows.minimised(),
        "fake windows contract: minimise reads back"
    );

    windows.set_visible(count, false);
    windows.set_visible(usize::MAX, false);
    assert!(
        (0..count).all(|i| windows.is_visible(i)),
        "fake windows contract: an out-of-range hide hides none"
    );
    windows.set_visible(1, false);
    assert!(
        !windows.is_visible(1) && windows.is_visible(0),
        "fake windows contract: a hide hides only that panel"
    );
    let size = windows.frame(1).expect("fake windows contract: frame 1");
    let to = ScreenPoint { x: 11, y: 22 };
    windows
        .move_panels(&[(1, to)])
        .expect("fake windows contract: a hidden panel moves");
    assert_eq!(
        windows.frame(1),
        Some(ScreenRect {
            x: 11,
            y: 22,
            ..size
        }),
        "fake windows contract: a hidden panel moves and keeps its size"
    );
    windows
        .resize_panel(1, 123, 45)
        .expect("fake windows contract: a hidden panel resizes");
    assert_eq!(
        windows.frame(1),
        Some(ScreenRect {
            x: 11,
            y: 22,
            w: 123,
            h: 45
        }),
        "fake windows contract: a hidden panel resizes exactly and keeps its origin"
    );
    assert!(
        windows.resize_panel(1, 0, 45).is_err() && windows.resize_panel(count, 9, 9).is_err(),
        "fake windows contract: a refused resize is an error"
    );
    assert_eq!(
        windows.frame(1).map(|f| (f.w, f.h)),
        Some((123, 45)),
        "fake windows contract: a refused resize changes nothing"
    );
    windows.set_visible(1, true);
    let (first, last) = (ScreenPoint { x: 1, y: 2 }, ScreenPoint { x: 3, y: 4 });
    windows
        .move_panels(&[(0, first), (0, last)])
        .expect("fake windows contract: a repeated index moves");
    let f0 = windows.frame(0).expect("fake windows contract: frame 0");
    assert_eq!(
        (f0.x, f0.y),
        (last.x, last.y),
        "fake windows contract: the last move of a repeated index wins"
    );
    assert!(
        windows.move_panels(&[]).is_ok(),
        "fake windows contract: an empty batch is Ok"
    );

    windows.set_pointer(Some(ScreenPoint { x: 7, y: 8 }));
    assert_eq!(
        windows.pointer(),
        Some(ScreenPoint { x: 7, y: 8 }),
        "fake windows contract: the pointer reads back"
    );
    windows.set_pointer(None);
    assert_eq!(
        windows.pointer(),
        None,
        "fake windows contract: the pointer clears"
    );
}

/// `len` bytes of a u32 linear congruential generator (inline; no RNG crate).
fn lcg_bytes(len: usize, seed: u32) -> Vec<u8> {
    let mut state = seed;
    (0..len)
        .map(|_| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (state >> 24) as u8
        })
        .collect()
}
/// Skin source cases. `missing` is a well-formed name the source does not hold.
///
/// | method | case |
/// |---|---|
/// | `list` | every name is a plain file name (not empty, no `/`, `\` or `..`); sorted by `(lowercase, exact)`; no duplicates; the same twice |
/// | `read` | every listed name reads within `MAX_SKIN_BYTES`, the same bytes twice; `missing` is `NotFound`, twice |
/// | `read` (names) | empty, `..`, `../x`, `a/b`, `a\b`, `a/../b`, `/abs`, `\abs` and `C:\x` are `InvalidName`; a NUL, a 10 000-character name and non-ASCII names are `InvalidName` or `NotFound`, never `Ok` |
/// | errors | every error text is fixed, one short line |
///
/// The caps and the extension filter are the implementation's own tests (`MAX_LISTED`, the
/// extension list); [`run_skin_source_size_contract`] covers the size cap at read time. Sync.
pub fn run_skin_source_contract(source: &dyn SkinSource, missing: &str) {
    let names = source.list();
    assert_eq!(
        names,
        source.list(),
        "skin source contract: list gives the same names twice"
    );
    for name in &names {
        assert!(
            !name.is_empty() && !name.contains(['/', '\\']) && !name.contains(".."),
            "skin source contract: a listed name is a plain file name: {name:?}"
        );
        let bytes = source
            .read(name)
            .unwrap_or_else(|e| panic!("skin source contract: a listed name reads: {e}"));
        assert!(
            bytes.len() as u64 <= MAX_SKIN_BYTES,
            "skin source contract: a listed name reads within MAX_SKIN_BYTES"
        );
        assert_eq!(
            Ok(bytes),
            source.read(name),
            "skin source contract: a name reads the same bytes twice"
        );
    }
    let mut sorted = names.clone();
    sorted.sort_by(|a, b| {
        a.to_ascii_lowercase()
            .cmp(&b.to_ascii_lowercase())
            .then_with(|| a.cmp(b))
    });
    assert_eq!(
        names, sorted,
        "skin source contract: list is sorted by (lowercase, exact)"
    );
    assert!(
        names.windows(2).all(|w| w[0] != w[1]),
        "skin source contract: list has no duplicates"
    );
    for _ in 0..2 {
        assert!(
            matches!(source.read(missing), Err(SkinError::NotFound)),
            "skin source contract: a missing name is NotFound"
        );
    }
    for bad in [
        "", "..", "../x", "a/b", "a\\b", "a/../b", "/abs", "\\abs", "C:\\x",
    ] {
        assert!(
            matches!(source.read(bad), Err(SkinError::InvalidName)),
            "skin source contract: {bad:?} is InvalidName"
        );
    }
    let long = "a".repeat(10_000);
    for odd in ["x\0y", long.as_str(), "\u{e9}.skn", "\u{202e}x", "a b\n"] {
        let shown: String = odd.chars().take(8).collect();
        assert!(
            matches!(
                source.read(odd),
                Err(SkinError::InvalidName | SkinError::NotFound)
            ),
            "skin source contract: the odd name {shown:?} is InvalidName or NotFound"
        );
    }
    for e in [
        SkinError::InvalidName,
        SkinError::NotFound,
        SkinError::NotAllowed,
        SkinError::TooLarge,
        SkinError::Io,
    ] {
        assert_fixed_text(&e.to_string(), "skin source contract");
    }
}

/// Skin source size cap at read time. `at_cap` is a name the source holds with exactly
/// `MAX_SKIN_BYTES` bytes, `over` one with `MAX_SKIN_BYTES + 1`.
///
/// | method | case |
/// |---|---|
/// | `read` | `at_cap` reads all `MAX_SKIN_BYTES` bytes; `over` is `Err(TooLarge)`, never a truncated read |
/// | `list` | whatever it offers reads within the cap (an over-cap file may be left out) |
pub fn run_skin_source_size_contract(source: &dyn SkinSource, at_cap: &str, over: &str) {
    match source.read(at_cap) {
        Ok(bytes) => assert_eq!(
            bytes.len() as u64,
            MAX_SKIN_BYTES,
            "skin source size contract: a file at the cap reads whole"
        ),
        Err(e) => panic!("skin source size contract: a file at the cap reads: {e}"),
    }
    assert!(
        matches!(source.read(over), Err(SkinError::TooLarge)),
        "skin source size contract: a file one byte over the cap is TooLarge"
    );
    for name in source.list() {
        match source.read(&name) {
            Ok(bytes) => assert!(
                bytes.len() as u64 <= MAX_SKIN_BYTES,
                "skin source size contract: a listed name reads within the cap"
            ),
            Err(SkinError::TooLarge) => {}
            Err(e) => panic!("skin source size contract: a listed name reads: {e}"),
        }
    }
}

/// Whether two folder paths name the same folder as far as the suite can tell: equal after
/// dropping trailing separators, and on Windows after folding case and slash direction.
fn same_folder(a: &str, b: &str) -> bool {
    let key = |p: &str| {
        let p = if cfg!(windows) {
            p.replace('/', "\\").to_lowercase()
        } else {
            p.to_owned()
        };
        p.trim_end_matches(std::path::is_separator).to_owned()
    };
    key(a) == key(b)
}

/// Music folders cases. `dirs` are two existing, different directories and `file` an existing
/// file (a path that is not a folder); `folders` starts empty. The suite adds and removes only
/// those paths and never touches the disk.
///
/// | method | case |
/// |---|---|
/// | `add` | an empty path, a missing path and a file are `NotAFolder` (a fixed text) and leave the list as it was |
/// | `add` | a directory is Ok and `folders` lists it |
/// | `add` | the same directory again, with a trailing separator, and (Windows) in upper case is `AlreadyAdded`; the list is unchanged |
/// | `folders` | lists folders in the order they were added |
/// | `remove` | an unknown folder is a no-op; a listed one leaves the list and the others keep their order; twice is a no-op; it can be added again |
/// | `rescan` | returns at once (well under 2 s) with folders listed and with none |
/// | `status` | `scanning` goes false within 10 s of a rescan; with no folders left, `tracks` is 0 once idle |
pub fn run_music_folders_contract(folders: &dyn MusicFolders, dirs: [&Path; 2], file: &Path) {
    let text = |p: &Path| p.to_str().expect("a UTF-8 test path").to_owned();
    let (a, b) = (text(dirs[0]), text(dirs[1]));
    assert!(
        !same_folder(&a, &b),
        "music folders contract: the two directories differ"
    );
    assert!(
        folders.folders().is_empty(),
        "music folders contract: the list starts empty"
    );

    let missing = text(&dirs[0].join("no-such-folder-whipspot"));
    for bad in [String::new(), missing, text(file)] {
        let Err(e) = folders.add(bad.clone()) else {
            panic!("music folders contract: adding {bad:?} is an error");
        };
        assert_eq!(
            e,
            FolderError::NotAFolder,
            "music folders contract: {bad:?} is not a folder"
        );
        assert_fixed_text(e.text(), "music folders contract: a refused folder");
    }
    assert!(
        folders.folders().is_empty(),
        "music folders contract: a refused folder is not listed"
    );

    folders
        .add(a.clone())
        .unwrap_or_else(|e| panic!("music folders contract: a directory is added: {e}"));
    let listed = folders.folders();
    assert!(
        listed.len() == 1 && same_folder(&listed[0], &a),
        "music folders contract: the added folder is listed ({listed:?})"
    );

    let mut again = vec![a.clone(), format!("{a}{}", std::path::MAIN_SEPARATOR)];
    if cfg!(windows) {
        again.push(a.to_uppercase());
    }
    for dup in again {
        assert_eq!(
            folders.add(dup.clone()),
            Err(FolderError::AlreadyAdded),
            "music folders contract: {dup:?} is a duplicate"
        );
    }
    assert_eq!(
        folders.folders(),
        listed,
        "music folders contract: a duplicate leaves the list as it was"
    );

    folders
        .add(b.clone())
        .unwrap_or_else(|e| panic!("music folders contract: a second directory is added: {e}"));
    let listed = folders.folders();
    assert!(
        listed.len() == 2 && same_folder(&listed[0], &a) && same_folder(&listed[1], &b),
        "music folders contract: folders are listed in the order they were added ({listed:?})"
    );

    folders.remove(&text(&dirs[0].join("no-such-folder-whipspot")));
    assert_eq!(
        folders.folders(),
        listed,
        "music folders contract: removing an unknown folder is a no-op"
    );
    folders.remove(&listed[0]);
    let after = folders.folders();
    assert!(
        after.len() == 1 && same_folder(&after[0], &b),
        "music folders contract: remove drops only that folder ({after:?})"
    );
    folders.remove(&listed[0]);
    assert_eq!(
        folders.folders(),
        after,
        "music folders contract: removing twice is a no-op"
    );
    folders
        .add(a.clone())
        .unwrap_or_else(|e| panic!("music folders contract: a removed folder is added again: {e}"));
    let listed = folders.folders();
    assert!(
        listed.len() == 2 && same_folder(&listed[0], &b) && same_folder(&listed[1], &a),
        "music folders contract: a re-added folder goes last ({listed:?})"
    );

    let rescan_and_wait = |what: &str| {
        let start = std::time::Instant::now();
        folders.rescan();
        assert!(
            start.elapsed() < std::time::Duration::from_secs(2),
            "music folders contract: rescan returns at once ({what})"
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while folders.status().scanning {
            assert!(
                std::time::Instant::now() < deadline,
                "music folders contract: the scan ends within 10 s ({what})"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    };
    rescan_and_wait("with folders");
    for f in folders.folders() {
        folders.remove(&f);
    }
    assert!(
        folders.folders().is_empty(),
        "music folders contract: every folder can be removed"
    );
    rescan_and_wait("with no folders");
    assert_eq!(
        folders.status().tracks,
        0,
        "music folders contract: no folders, no tracks once idle"
    );
}

/// Power control cases. `p` is any implementation: a fake, or the adapter on this machine, which
/// then changes the test process's own power mode and must leave it at Normal.
///
/// | method | case |
/// |---|---|
/// | `name` | not empty, at most 48 characters, no control character |
/// | `set` | Normal, Eco, EcoIdle, EcoIdle, Eco, Normal each give Ok, or each gives Unsupported: never a mix, and never Refused on Normal |
/// | `set` | 4 threads × 50 mixed calls do not panic |
/// | `set` | the suite ends at Normal, with the answer it gave before |
pub fn run_power_control_contract(p: &dyn PowerControl) {
    let name = p.name();
    assert!(
        !name.is_empty() && name.chars().count() <= 48 && !name.chars().any(char::is_control),
        "power control contract: the name is short, printable and not empty ({name:?})"
    );

    let sequence = [
        PowerMode::Normal,
        PowerMode::Eco,
        PowerMode::EcoIdle,
        PowerMode::EcoIdle,
        PowerMode::Eco,
        PowerMode::Normal,
    ];
    let results: Vec<Result<(), PowerError>> = sequence.iter().map(|&m| p.set(m)).collect();
    let first = results[0];
    assert!(
        matches!(first, Ok(()) | Err(PowerError::Unsupported)),
        "power control contract: set(Normal) is Ok or Unsupported ({first:?})"
    );
    for (mode, result) in sequence.iter().zip(&results) {
        assert_eq!(
            *result, first,
            "power control contract: set({mode:?}) answers as set(Normal) does"
        );
    }

    let all_joined = std::thread::scope(|s| {
        let handles: Vec<_> = (0..4usize)
            .map(|t| {
                s.spawn(move || {
                    for i in 0..50usize {
                        let _ = p.set(sequence[(t + i) % sequence.len()]);
                    }
                })
            })
            .collect();
        handles.into_iter().all(|h| h.join().is_ok())
    });
    assert!(
        all_joined,
        "power control contract: concurrent set calls do not panic"
    );

    assert_eq!(
        p.set(PowerMode::Normal),
        first,
        "power control contract: the suite ends at Normal, with the answer it gave before"
    );
}
