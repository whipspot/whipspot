//! Playback of the YouTube Music provider (step 7): the ranged reader against a fake server, and
//! the provider's decode thread through the core engine on a fake output.
//!
//! Nothing here touches the network or a real device: `FakeYt` serves the synthetic 3 s fragmented
//! AAC fixture by range on a `googlevideo.com` URL, a fake runner answers yt-dlp's `-J` with that
//! URL, and the engine renders into the testkit's `FakeOutput`.
//!
//! The fixture is 47 774 bytes and the fake yt-dlp asks for a 16 KiB chunk, so a track is three
//! blocks and a fault at the n-th request lands inside the stream.

mod fixtures;

use std::ffi::OsString;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::json;
use symphonia::core::io::MediaSource;
use tokio::sync::broadcast;
use whipspot_core::engine::{
    DeviceFormat, Engine, EngineConfig, OutputMode, OutputRequest, SampleFormat,
};
use whipspot_core::events::UiEvent;
use whipspot_core::provider::{LoginInput, Provider};
use whipspot_testkit::{FakeOutput, MemoryStore, run_provider_playback_contract};
use whipspot_ytmusic::auth::{STEP_COOKIES, STEP_HELPER};
use whipspot_ytmusic::http::Http;
use whipspot_ytmusic::resolve::{RangeForm, Stream};
use whipspot_ytmusic::run::{Output, RunError, Runner};
use whipspot_ytmusic::source::{BLOCK, Cancel, MIN_BLOCK, RangeSource, SLOTS};
use whipspot_ytmusic::{YtDeps, YtMusicProvider};

use fixtures::{FakeYt, MediaFault, cookies_txt, release_env};

const FIXTURE: &[u8] = include_bytes!("data/tone-frag.m4a");
const MEDIA_URL: &str = "https://rr1---sn-test.googlevideo.com/videoplayback";
const GOOD: &str = "ytmusic:track:dQw4w9WgXcQ";
const BAD: &str = "ytmusic:track:bad";
/// What a failed open says, and what a stream that dies mid-track says (fixed texts).
const CANNOT_PLAY: &str = "this track could not be played";
const STREAM_FAILED: &str = "this track's stream failed";

fn stream(len: Option<u64>, chunk: Option<u64>) -> Stream {
    Stream {
        url: format!("{MEDIA_URL}?expire=4102444800&mime=audio%2Fmp4&itag=140"),
        headers: vec![("User-Agent".to_owned(), "Mozilla/5.0 (test)".to_owned())],
        itag: 140,
        abr_kbps: Some(128),
        filesize: len,
        expires_at: None,
        range_form: RangeForm::Header,
        chunk_size: chunk,
    }
}

/// 100 000 bytes of a pattern nothing else makes: seven blocks of 16 KiB.
fn pattern() -> Vec<u8> {
    (0..100_000u32).map(|i| (i * 7 + i / 251) as u8).collect()
}

/// A refresh that counts its calls and gives back `stream`.
fn counting_refresh(
    calls: &Arc<AtomicUsize>,
    stream: Option<Stream>,
) -> Box<dyn FnMut() -> Option<Stream> + Send> {
    let calls = calls.clone();
    Box::new(move || {
        calls.fetch_add(1, Ordering::SeqCst);
        stream.clone()
    })
}

fn open(
    yt: &Arc<FakeYt>,
    s: Stream,
    refresh: Box<dyn FnMut() -> Option<Stream> + Send>,
) -> Result<RangeSource, whipspot_ytmusic::source::StreamError> {
    RangeSource::open(yt.clone() as Arc<dyn Http>, s, refresh, Cancel::never())
}

fn read_all(src: &mut RangeSource) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match src.read(&mut chunk)? {
            0 => return Ok(out),
            n => out.extend_from_slice(&chunk[..n]),
        }
    }
}

// ---- the ranged reader ------------------------------------------------------------------------

#[test]
fn reads_the_whole_stream_byte_for_byte_in_block_sized_requests() {
    let yt = FakeYt::new();
    let data = pattern();
    yt.serve_media(MEDIA_URL, &data);
    let calls = Arc::new(AtomicUsize::new(0));
    let mut src = open(
        &yt,
        stream(Some(data.len() as u64), Some(MIN_BLOCK)),
        counting_refresh(&calls, None),
    )
    .unwrap();
    assert_eq!(src.len(), data.len() as u64);
    assert_eq!(src.byte_len(), Some(data.len() as u64));
    assert!(src.is_seekable());
    assert_eq!(read_all(&mut src).unwrap(), data);
    let requests = yt.media_requests();
    // Seven blocks, each asked once, none larger than a block, in order.
    assert_eq!(requests.len(), 7, "{requests:?}");
    for (i, (_, (a, b))) in requests.iter().enumerate() {
        assert_eq!(*a, i as u64 * MIN_BLOCK);
        assert!(b - a < MIN_BLOCK);
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "no re-resolve without a 403"
    );
}

#[test]
fn the_block_buffers_are_reused_and_never_grow() {
    let yt = FakeYt::new();
    let data = pattern();
    yt.serve_media(MEDIA_URL, &data);
    let mut src = open(
        &yt,
        stream(Some(data.len() as u64), Some(MIN_BLOCK)),
        Box::new(|| None),
    )
    .unwrap();
    let first = src.buffer_capacities();
    assert_eq!(first.len(), 1, "one block held after the open");
    // Read in a scrambled order: every block is fetched more than once.
    let mut got = vec![0u8; 100];
    for round in 0..4u64 {
        for block in [6u64, 0, 3, 5, 1, 4, 2] {
            let at = block * MIN_BLOCK + round * 10;
            src.seek(SeekFrom::Start(at)).unwrap();
            src.read_exact(&mut got).unwrap();
            assert_eq!(got, data[at as usize..at as usize + 100]);
        }
    }
    let caps = src.buffer_capacities();
    assert_eq!(caps.len(), SLOTS, "three buffers, no more");
    for c in caps {
        assert!(c >= MIN_BLOCK as usize, "each holds a block");
        assert!(c <= BLOCK as usize, "none larger than a block");
        assert!(
            c < 2 * MIN_BLOCK as usize,
            "none grew past its first allocation"
        );
    }
}

#[test]
fn a_held_block_is_not_fetched_again_and_the_oldest_goes_first() {
    let yt = FakeYt::new();
    let data = pattern();
    yt.serve_media(MEDIA_URL, &data);
    let mut src = open(
        &yt,
        stream(Some(data.len() as u64), Some(MIN_BLOCK)),
        Box::new(|| None),
    )
    .unwrap();
    let read_at = |src: &mut RangeSource, block: u64| {
        let mut b = [0u8; 1];
        src.seek(SeekFrom::Start(block * MIN_BLOCK)).unwrap();
        src.read_exact(&mut b).unwrap();
    };
    // Held: 0 (from the open). Fetch 1 and 2: three held.
    read_at(&mut src, 1);
    read_at(&mut src, 2);
    assert_eq!(yt.media_requests().len(), 3);
    // Going back through the held blocks costs nothing, however often.
    for _ in 0..3 {
        for held in [0, 1, 2] {
            read_at(&mut src, held);
        }
    }
    assert_eq!(yt.media_requests().len(), 3);
    // Block 3 pushes out the least recently used (block 0 was touched first of the last pass).
    read_at(&mut src, 3);
    assert_eq!(yt.media_requests().len(), 4);
    // 1, 2 and 3 are held; asking for 0 fetches it again.
    for held in [1, 2, 3] {
        read_at(&mut src, held);
    }
    assert_eq!(yt.media_requests().len(), 4);
    read_at(&mut src, 0);
    assert_eq!(yt.media_requests().len(), 5);
}

#[test]
fn seek_moves_the_position_without_a_request() {
    let yt = FakeYt::new();
    let data = pattern();
    yt.serve_media(MEDIA_URL, &data);
    let mut src = open(
        &yt,
        stream(Some(data.len() as u64), Some(MIN_BLOCK)),
        Box::new(|| None),
    )
    .unwrap();
    let before = yt.media_requests().len();
    let len = data.len() as u64;
    assert_eq!(src.seek(SeekFrom::End(-10)).unwrap(), len - 10);
    assert_eq!(src.seek(SeekFrom::Current(-5)).unwrap(), len - 15);
    assert_eq!(
        src.seek(SeekFrom::Start(len + 500)).unwrap(),
        len,
        "clamped"
    );
    assert!(src.seek(SeekFrom::End(-(len as i64) - 1)).is_err());
    assert_eq!(yt.media_requests().len(), before);
    // At the end a read gives 0 bytes: the clean end of file.
    assert_eq!(src.read(&mut [0u8; 8]).unwrap(), 0);
    assert_eq!(yt.media_requests().len(), before);
}

#[test]
fn the_length_comes_from_content_range_then_the_file_size_then_the_body() {
    let yt = FakeYt::new();
    let data = pattern();
    yt.serve_media(MEDIA_URL, &data);
    let len = data.len() as u64;

    // The server's total beats a file size that is wrong.
    let src = open(
        &yt,
        stream(Some(len + 5), Some(MIN_BLOCK)),
        Box::new(|| None),
    )
    .unwrap();
    assert_eq!(src.len(), len);

    // No size from yt-dlp: the total.
    let src = open(&yt, stream(None, Some(MIN_BLOCK)), Box::new(|| None)).unwrap();
    assert_eq!(src.len(), len);

    // No `Content-Range`: the file size.
    yt.script_media(&[Some(MediaFault::NoRangeHeader)]);
    let src = open(&yt, stream(Some(len), Some(MIN_BLOCK)), Box::new(|| None)).unwrap();
    assert_eq!(src.len(), len);

    // No `Content-Range` and no size: a body shorter than a block is the whole stream ...
    let small = FakeYt::new();
    small.serve_media(MEDIA_URL, FIXTURE);
    small.script_media(&[Some(MediaFault::NoRangeHeader)]);
    let src = open(&small, stream(None, None), Box::new(|| None)).unwrap();
    assert_eq!(src.len(), FIXTURE.len() as u64);

    // ... and a full block with nothing to say where it ends is refused.
    yt.script_media(&[Some(MediaFault::NoRangeHeader)]);
    assert!(open(&yt, stream(None, Some(MIN_BLOCK)), Box::new(|| None)).is_err());
}

#[test]
fn a_403_resolves_once_and_goes_on_at_the_same_offset() {
    let yt = FakeYt::new();
    let data = pattern();
    yt.serve_media(MEDIA_URL, &data);
    let calls = Arc::new(AtomicUsize::new(0));
    let mut fresh = stream(Some(data.len() as u64), Some(MIN_BLOCK));
    fresh.url = format!("{MEDIA_URL}?expire=4102444800&mime=audio%2Fmp4&itag=140&fresh=1");
    let mut src = open(
        &yt,
        stream(Some(data.len() as u64), Some(MIN_BLOCK)),
        counting_refresh(&calls, Some(fresh)),
    )
    .unwrap();
    // The open was request 1; the second (block 1) is refused, the third is its retry.
    yt.script_media(&[Some(MediaFault::Forbidden)]);
    assert_eq!(read_all(&mut src).unwrap(), data);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let requests = yt.media_requests();
    assert_eq!(requests.len(), 8, "seven blocks and one refused try");
    assert_eq!(
        requests[1].1, requests[2].1,
        "the retry asks for the same range"
    );
    assert!(!requests[1].0.contains("fresh=1"));
    assert!(
        requests[2].0.contains("fresh=1"),
        "the retry uses the new link"
    );
    assert!(requests[3..].iter().all(|(u, _)| u.contains("fresh=1")));
}

#[test]
fn a_second_403_fails_the_stream_with_one_resolve_only() {
    let yt = FakeYt::new();
    let data = pattern();
    yt.serve_media(MEDIA_URL, &data);
    let calls = Arc::new(AtomicUsize::new(0));
    let fresh = stream(Some(data.len() as u64), Some(MIN_BLOCK));
    let mut src = open(
        &yt,
        stream(Some(data.len() as u64), Some(MIN_BLOCK)),
        counting_refresh(&calls, Some(fresh)),
    )
    .unwrap();
    yt.script_media(&[Some(MediaFault::Forbidden), Some(MediaFault::Forbidden)]);
    let err = read_all(&mut src).unwrap_err();
    assert_eq!(err.to_string(), "the stream link was refused");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    // The track is over: a later 403 does not buy another resolve.
    yt.script_media(&[Some(MediaFault::Forbidden)]);
    src.seek(SeekFrom::Start(0)).unwrap();
    src.seek(SeekFrom::Start(5 * MIN_BLOCK)).unwrap();
    assert!(src.read(&mut [0u8; 8]).is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn a_refresh_that_gives_nothing_or_another_file_fails_the_stream() {
    let yt = FakeYt::new();
    let data = pattern();
    yt.serve_media(MEDIA_URL, &data);
    let len = data.len() as u64;
    for fresh in [None, Some(stream(Some(len + 1), Some(MIN_BLOCK)))] {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut src = open(
            &yt,
            stream(Some(len), Some(MIN_BLOCK)),
            counting_refresh(&calls, fresh),
        )
        .unwrap();
        yt.script_media(&[Some(MediaFault::Forbidden)]);
        assert!(read_all(&mut src).is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn a_bad_answer_is_a_fixed_error_never_a_short_read() {
    let data = pattern();
    let len = data.len() as u64;
    for fault in [
        MediaFault::Truncate,
        MediaFault::LieStart,
        MediaFault::LieTotal,
        MediaFault::Whole,
    ] {
        // At the open and at each block after it.
        for at in 0..7usize {
            let yt = FakeYt::new();
            yt.serve_media(MEDIA_URL, &data);
            let mut script = vec![None; at];
            script.push(Some(fault));
            yt.script_media(&script);
            let outcome = open(&yt, stream(Some(len), Some(MIN_BLOCK)), Box::new(|| None))
                .map(|mut s| read_all(&mut s));
            let text = match outcome {
                Err(e) => e.to_string(),
                Ok(Err(e)) => e.to_string(),
                Ok(Ok(bytes)) => panic!("{fault:?} at {at}: read {} bytes cleanly", bytes.len()),
            };
            assert_eq!(
                text, "the stream's answer was not usable",
                "{fault:?} at {at}"
            );
        }
    }
}

#[test]
fn a_connection_that_fails_or_a_cancelled_reader_gives_a_fixed_error() {
    let yt = FakeYt::new();
    let data = pattern();
    yt.serve_media(MEDIA_URL, &data);
    let counter = Arc::new(AtomicU64::new(0));
    let mut src = RangeSource::open(
        yt.clone() as Arc<dyn Http>,
        stream(Some(data.len() as u64), Some(MIN_BLOCK)),
        Box::new(|| None),
        Cancel::new(counter.clone()),
    )
    .unwrap();
    let before = yt.media_requests().len();
    src.seek(SeekFrom::Start(3 * MIN_BLOCK)).unwrap();
    counter.fetch_add(1, Ordering::SeqCst);
    let err = src.read(&mut [0u8; 8]).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::ConnectionAborted);
    assert_eq!(
        yt.media_requests().len(),
        before,
        "a cancelled read asks for nothing"
    );
    // A block that is already held still reads (it costs no request).
    src.seek(SeekFrom::Start(0)).unwrap();
    assert!(src.read(&mut [0u8; 8]).is_ok());

    // A URL that is not on the allowed list: the fake has no such body, so the call fails.
    let mut other = stream(Some(data.len() as u64), Some(MIN_BLOCK));
    other.url = "https://rr2.googlevideo.com/other".to_owned();
    assert!(open(&yt, other, Box::new(|| None)).is_err());
}

#[test]
fn only_the_header_range_form_is_read() {
    let yt = FakeYt::new();
    yt.serve_media(MEDIA_URL, &pattern());
    let mut s = stream(Some(100_000), Some(MIN_BLOCK));
    s.range_form = RangeForm::Query;
    assert!(open(&yt, s, Box::new(|| None)).is_err());
    assert!(yt.media_requests().is_empty());
}

// ---- the provider through the engine ----------------------------------------------------------

/// A yt-dlp that answers `-J` with the fixture's stream and `--version` with a version.
struct StreamRunner {
    solves: AtomicUsize,
    cookie_runs: AtomicUsize,
    /// The `n=` of the URL of the last answer.
    last_url: Mutex<String>,
}

impl StreamRunner {
    fn new() -> Arc<StreamRunner> {
        Arc::new(StreamRunner {
            solves: AtomicUsize::new(0),
            cookie_runs: AtomicUsize::new(0),
            last_url: Mutex::new(String::new()),
        })
    }

    fn solves(&self) -> usize {
        self.solves.load(Ordering::SeqCst)
    }
}

impl Runner for StreamRunner {
    fn run(
        &self,
        _program: &Path,
        args: &[OsString],
        _timeout: Duration,
    ) -> Result<Output, RunError> {
        let args: Vec<String> = args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        if !args.iter().any(|a| a == "-J") {
            return Ok(Output {
                code: Some(0),
                stdout: b"2026.10.26\n".to_vec(),
                stderr: Vec::new(),
            });
        }
        let n = self.solves.fetch_add(1, Ordering::SeqCst) + 1;
        if args.iter().any(|a| a == "--cookies") {
            self.cookie_runs.fetch_add(1, Ordering::SeqCst);
        }
        let url = format!("{MEDIA_URL}?expire=4102444800&mime=audio%2Fmp4&itag=140&n={n}");
        *self.last_url.lock().unwrap() = url.clone();
        let info = json!({
            "id": "dQw4w9WgXcQ",
            "format_id": "140",
            "url": url,
            "ext": "m4a",
            "acodec": "mp4a.40.2",
            "vcodec": "none",
            "abr": 129.5,
            "filesize": FIXTURE.len(),
            "http_headers": {"User-Agent": "Mozilla/5.0 (test)"},
            "downloader_options": {"http_chunk_size": MIN_BLOCK},
        });
        Ok(Output {
            code: Some(0),
            stdout: serde_json::to_vec(&info).unwrap(),
            stderr: Vec::new(),
        })
    }
}

/// Renders the fake output continuously on a thread (a stand-in for a device).
struct Pump {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Pump {
    fn new(fake: FakeOutput) -> Pump {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                fake.render_f32(256);
                std::thread::sleep(Duration::from_millis(1));
            }
        });
        Pump {
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for Pump {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

struct Rig {
    engine: Engine,
    fake: FakeOutput,
    yt: Arc<FakeYt>,
    runner: Arc<StreamRunner>,
    provider: Arc<YtMusicProvider>,
    events: broadcast::Receiver<UiEvent>,
    seen: Vec<UiEvent>,
    _dir: tempfile::TempDir,
}

fn device(rate: u32) -> DeviceFormat {
    DeviceFormat {
        rate,
        channels: 2,
        sample: SampleFormat::F32,
        valid_bits: 32,
    }
}

impl Rig {
    /// A provider on an engine with a fake 44.1 kHz output, the helper logged in (a fake install)
    /// and the fixture served for any track.
    async fn new() -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let fake = FakeOutput::new(device(44_100), OutputMode::Exclusive);
        let parts = Engine::new(EngineConfig::default(), Some(Box::new(fake.clone())));
        let mut engine = parts.engine;
        engine.open(&OutputRequest::default()).unwrap();
        let yt = FakeYt::new();
        yt.serve_media(MEDIA_URL, FIXTURE);
        let env = release_env(&yt, dir.path().join("tools"));
        let runner = StreamRunner::new();
        let (tx, events) = broadcast::channel(4096);
        let provider = Arc::new(YtMusicProvider::new(YtDeps {
            http: yt.clone(),
            runner: runner.clone(),
            env,
            store: Arc::new(MemoryStore::default()),
            source: parts.source,
            events: tx,
        }));
        provider
            .auth()
            .login(STEP_HELPER, &LoginInput::default())
            .await
            .expect("helper login");
        Rig {
            engine,
            fake,
            yt,
            runner,
            provider,
            events,
            seen: Vec::new(),
            _dir: dir,
        }
    }

    async fn load(&self, play: bool, position_ms: u32) -> Result<(), String> {
        self.provider
            .playback()
            .load(GOOD, play, position_ms)
            .await
            .map_err(|e| e.0)
    }

    /// Waits for an event `matching` (the ones before it are kept in `seen`).
    fn wait(&mut self, what: &str, matching: impl Fn(&UiEvent) -> bool) -> UiEvent {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            match self.events.try_recv() {
                Ok(e) => {
                    self.seen.push(e.clone());
                    if matching(&e) {
                        return e;
                    }
                }
                Err(broadcast::error::TryRecvError::Empty) => {
                    assert!(Instant::now() < deadline, "no {what}; saw {:?}", self.seen);
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(e) => panic!("events: {e}"),
            }
        }
    }

    fn wait_end(&mut self) -> UiEvent {
        self.wait("end of track", |e| matches!(e, UiEvent::EndOfTrack { .. }))
    }

    fn errors(&self) -> Vec<String> {
        self.seen
            .iter()
            .filter_map(|e| match e {
                UiEvent::Error { message } => Some(message.clone()),
                _ => None,
            })
            .collect()
    }
}

/// Plays the fixture to its end and returns the frames the engine took in.
async fn frames_of_a_full_play(r: &mut Rig) -> u64 {
    let before = r.engine.written_frames();
    r.load(true, 0).await.unwrap();
    r.wait_end();
    r.engine.written_frames() - before
}

#[tokio::test(flavor = "multi_thread")]
async fn provider_playback_contract_passes_on_the_assembled_provider() {
    let r = Rig::new().await;
    let _pump = Pump::new(r.fake.clone());
    run_provider_playback_contract(r.provider.clone(), Some(GOOD), BAD).await;
    // The helper and the cookies are the provider's own logins; playback needs only the first.
    r.provider
        .auth()
        .login(STEP_COOKIES, &{
            let mut input = LoginInput::default();
            let path = r._dir.path().join("cookies.txt");
            std::fs::write(&path, cookies_txt()).unwrap();
            input
                .0
                .insert("path".to_owned(), path.to_string_lossy().into_owned());
            input
        })
        .await
        .unwrap();
    run_provider_playback_contract(r.provider.clone(), Some(GOOD), BAD).await;
    // A track not resolved before: its run carries the cookies.
    let before = r.runner.cookie_runs.load(Ordering::SeqCst);
    r.provider
        .playback()
        .load("ytmusic:track:aaaaaaaaaaa", false, 0)
        .await
        .unwrap();
    assert_eq!(
        r.runner.cookie_runs.load(Ordering::SeqCst),
        before + 1,
        "a logged-in resolve hands yt-dlp the cookies"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn plays_the_whole_track_and_says_what_it_is() {
    let mut r = Rig::new().await;
    let _pump = Pump::new(r.fake.clone());
    assert_eq!(r.provider.playback().source_format(), None);
    r.load(false, 0).await.unwrap();
    // The itag and yt-dlp's figure agree: 128 kbps. The fixture is 44.1 kHz.
    assert_eq!(
        r.provider.playback().source_format().as_deref(),
        Some("AAC 128 kbps 44.1 kHz")
    );
    let frames = frames_of_a_full_play(&mut r).await;
    assert!(frames >= 2 * 44_100 + 44_100 * 9 / 10, "{frames} frames");
    assert!(frames <= 3 * 44_100 + 44_100 / 4, "{frames} frames");
    assert!(r.errors().is_empty(), "{:?}", r.errors());
    // The order the service relies on: loading, the track, playing, then its end.
    let kinds: Vec<&str> = r
        .seen
        .iter()
        .map(|e| match e {
            UiEvent::Loading { .. } => "loading",
            UiEvent::TrackChanged { .. } => "track",
            UiEvent::Playing { .. } => "playing",
            UiEvent::Paused { .. } => "paused",
            UiEvent::EndOfTrack { .. } => "end",
            _ => "other",
        })
        .collect();
    let first = |k: &str| kinds.iter().position(|x| *x == k).unwrap();
    assert!(
        first("loading") < first("track") && first("track") < first("end"),
        "{kinds:?}"
    );
    // The audio came from ranged requests of 16 KiB blocks: the first load only opened the
    // stream (its first block), the play read all three.
    assert_eq!(r.yt.media_requests().len(), 1 + 3);
    assert_eq!(r.runner.solves(), 1, "the second play used the cache");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_seek_to_two_seconds_lands_within_50_ms() {
    let mut r = Rig::new().await;
    let _pump = Pump::new(r.fake.clone());
    let total = frames_of_a_full_play(&mut r).await;
    let tolerance = 44_100 / 20;
    let expect = total.saturating_sub(2 * 44_100);

    // Seek while loaded and paused.
    r.load(false, 0).await.unwrap();
    r.provider.playback().seek(2_000).await.unwrap();
    let seeked = r.wait("seeked", |e| matches!(e, UiEvent::Seeked { .. }));
    let UiEvent::Seeked { position_ms, .. } = seeked else {
        unreachable!()
    };
    assert!(position_ms.abs_diff(2_000) <= 50, "{position_ms} ms");
    let before = r.engine.written_frames();
    r.provider.playback().play().await.unwrap();
    r.wait_end();
    let after_seek = r.engine.written_frames() - before;
    assert!(
        after_seek.abs_diff(expect) <= tolerance,
        "{after_seek} frames after a seek to 2.0 s, expected {expect} (+-{tolerance})"
    );

    // Load at a position: the same.
    let before = r.engine.written_frames();
    r.load(true, 2_000).await.unwrap();
    r.wait_end();
    let from_load = r.engine.written_frames() - before;
    assert!(
        from_load.abs_diff(expect) <= tolerance,
        "{from_load} frames after a load at 2.0 s, expected {expect}"
    );
    assert!(r.errors().is_empty(), "{:?}", r.errors());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_seek_past_the_end_ends_the_track_cleanly() {
    let mut r = Rig::new().await;
    let _pump = Pump::new(r.fake.clone());
    r.load(true, 0).await.unwrap();
    r.provider.playback().seek(60_000).await.unwrap();
    r.wait_end();
    assert!(r.errors().is_empty(), "{:?}", r.errors());
    // Nothing is stuck: the next track loads and plays.
    let frames = frames_of_a_full_play(&mut r).await;
    assert!(frames > 2 * 44_100, "{frames}");
    // A load past the end ends it just as cleanly.
    r.load(true, 60_000).await.unwrap();
    r.wait_end();
    assert!(r.errors().is_empty(), "{:?}", r.errors());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_403_in_the_middle_resolves_once_and_the_track_carries_on() {
    for at in 1..3usize {
        let mut r = Rig::new().await;
        let _pump = Pump::new(r.fake.clone());
        let mut script = vec![None; at];
        script.push(Some(MediaFault::Forbidden));
        r.yt.script_media(&script);
        let frames = frames_of_a_full_play(&mut r).await;
        assert!(
            frames > 2 * 44_100,
            "refused at request {at}: {frames} frames"
        );
        assert!(r.errors().is_empty(), "{:?}", r.errors());
        assert_eq!(
            r.runner.solves(),
            2,
            "the first resolve and exactly one more"
        );
        let urls: Vec<String> = r.yt.media_requests().into_iter().map(|(u, _)| u).collect();
        assert!(urls.last().unwrap().contains("n=2"), "{urls:?}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_403_fails_the_track_with_a_fixed_text() {
    let mut r = Rig::new().await;
    let _pump = Pump::new(r.fake.clone());
    // Request 1 is the open; the second and its retry are refused.
    r.yt.script_media(&[
        None,
        Some(MediaFault::Forbidden),
        Some(MediaFault::Forbidden),
    ]);
    let outcome = r.load(true, 0).await;
    match outcome {
        // Refused while the stream opened.
        Err(text) => assert_eq!(text, CANNOT_PLAY),
        // Refused while it played.
        Ok(()) => {
            r.wait_end();
            assert_eq!(r.errors(), vec![STREAM_FAILED.to_owned()]);
        }
    }
    assert_eq!(r.runner.solves(), 2, "one re-resolve, not two");
    // The provider is not left broken.
    r.yt.script_media(&[]);
    r.load(true, 0).await.unwrap();
    r.wait_end();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_truncated_body_a_lying_range_or_a_non_mp4_body_fails_the_track() {
    for fault in [
        MediaFault::Truncate,
        MediaFault::LieStart,
        MediaFault::LieTotal,
        MediaFault::Whole,
    ] {
        for at in 0..3usize {
            let mut r = Rig::new().await;
            let _pump = Pump::new(r.fake.clone());
            let mut script = vec![None; at];
            script.push(Some(fault));
            r.yt.script_media(&script);
            match r.load(true, 0).await {
                Err(text) => assert_eq!(text, CANNOT_PLAY, "{fault:?} at {at}"),
                Ok(()) => {
                    r.wait_end();
                    assert_eq!(
                        r.errors(),
                        vec![STREAM_FAILED.to_owned()],
                        "{fault:?} at {at}"
                    );
                }
            }
            assert_eq!(
                r.runner.solves(),
                1,
                "{fault:?} at {at}: no re-resolve for a bad answer"
            );
        }
    }

    // A body that is no MP4 at all.
    let mut r = Rig::new().await;
    r.yt.serve_media(MEDIA_URL, &vec![0xABu8; 40_000]);
    let text = r.load(true, 0).await.unwrap_err();
    assert_eq!(text, CANNOT_PLAY);
    r.wait("unavailable", |e| matches!(e, UiEvent::Unavailable { .. }));
    // And an MP4 cut off after its first block.
    r.yt.serve_media(MEDIA_URL, &FIXTURE[..MIN_BLOCK as usize]);
    let outcome = r.load(true, 0).await;
    if outcome.is_ok() {
        let _pump = Pump::new(r.fake.clone());
        r.wait_end();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_content_range_still_plays_by_the_file_size() {
    let mut r = Rig::new().await;
    let _pump = Pump::new(r.fake.clone());
    r.yt.script_media(&[Some(MediaFault::NoRangeHeader); 3]);
    let frames = frames_of_a_full_play(&mut r).await;
    assert!(frames > 2 * 44_100, "{frames}");
    assert!(r.errors().is_empty(), "{:?}", r.errors());
}

#[tokio::test(flavor = "multi_thread")]
async fn stop_is_heard_at_once_and_a_dropped_provider_joins_its_thread_within_a_second() {
    let mut r = Rig::new().await;
    // Nothing renders: the thread is in `write` with the ring full.
    r.load(true, 0).await.unwrap();
    std::thread::sleep(Duration::from_millis(200));
    let start = Instant::now();
    r.provider.playback().stop().await.unwrap();
    r.wait("stopped", |e| matches!(e, UiEvent::Stopped { .. }));
    assert!(
        start.elapsed() < Duration::from_millis(900),
        "stop took {:?}",
        start.elapsed()
    );
    assert!(r.errors().is_empty(), "{:?}", r.errors());
    // Stop twice is fine, and so is a stop with nothing loaded.
    r.provider.playback().stop().await.unwrap();
    assert_eq!(r.provider.playback().source_format(), None);

    // Dropping with a track blocked in `write`.
    r.load(true, 0).await.unwrap();
    std::thread::sleep(Duration::from_millis(200));
    let start = Instant::now();
    let Rig { provider, .. } = r;
    drop(provider);
    assert!(
        start.elapsed() < Duration::from_millis(900),
        "drop took {:?}",
        start.elapsed()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_load_replaces_the_track_that_is_playing() {
    let mut r = Rig::new().await;
    r.load(true, 0).await.unwrap();
    std::thread::sleep(Duration::from_millis(100));
    // Nothing renders, so the first track is blocked in `write`: the load must interrupt it.
    let start = Instant::now();
    r.load(false, 0).await.unwrap();
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "{:?}",
        start.elapsed()
    );
    assert!(r.errors().is_empty(), "{:?}", r.errors());
    let _pump = Pump::new(r.fake.clone());
    r.provider.playback().play().await.unwrap();
    r.wait_end();
    assert!(r.errors().is_empty(), "{:?}", r.errors());
}

#[tokio::test(flavor = "multi_thread")]
async fn preload_resolves_the_next_track_and_fetches_no_audio() {
    let r = Rig::new().await;
    let before = r.runner.solves();
    r.provider.playback().preload(GOOD).await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while r.runner.solves() == before {
        assert!(Instant::now() < deadline, "no resolve");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(r.yt.media_requests().is_empty(), "no audio was fetched");
    // The load then finds the resolved stream in the cache.
    let solves = r.runner.solves();
    r.load(false, 0).await.unwrap();
    assert_eq!(r.runner.solves(), solves);
    // Another provider's uri is an error and runs nothing.
    let solves = r.runner.solves();
    assert!(
        r.provider
            .playback()
            .preload("local:track:x")
            .await
            .is_err()
    );
    assert_eq!(r.runner.solves(), solves);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_load_with_the_helper_off_or_a_bad_uri_says_so_and_runs_nothing() {
    let r = Rig::new().await;
    for uri in [
        "local:track:abc",
        "ytmusic:track:",
        "ytmusic:playlist:PLabc",
        "ytmusic:track:a b",
    ] {
        let e = r.provider.playback().load(uri, true, 0).await.unwrap_err();
        assert_eq!(e.0, "this is not a YouTube Music track", "{uri}");
    }
    assert_eq!(r.runner.solves(), 0);
    r.provider.auth().logout(STEP_HELPER).await.unwrap();
    let e = r.provider.playback().load(GOOD, true, 0).await.unwrap_err();
    assert_eq!(e.0, "Turn on the stream helper in Accounts first");
    assert_eq!(r.runner.solves(), 0);
}
