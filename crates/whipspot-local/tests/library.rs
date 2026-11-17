//! The local provider's library over the testkit's music tree: albums, untagged files, playlists,
//! search, favourites, ids, the index cache and malformed input.

use std::io::Cursor;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::broadcast;
use whipspot_core::engine::{Engine, EngineConfig};
use whipspot_core::events::UiEvent;
use whipspot_core::folders::MusicFolders;
use whipspot_core::provider::{Library, Provider, ProviderContext, Track};
use whipspot_local::{
    FAVOURITES_FILE, INDEX_FILE, LocalConfig, LocalProvider, UNKNOWN_ALBUM, UNKNOWN_ARTIST,
    is_track_uri, read_tags_from_bytes,
};
use whipspot_testkit::audio_files::{
    ALBUM_TITLE, ALBUM_TRACKS, FlacPicture, TempMusicDir, flac, id3_bomb_mp3, png_rgb, signal,
};
use whipspot_testkit::rng::{fuzz, mutate};
use whipspot_testkit::run_provider_contract;

static NEXT: AtomicU32 = AtomicU32::new(0);

/// A fresh directory under the OS temp dir (checked absent first), removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(what: &str) -> Scratch {
        let p = std::env::temp_dir().join(format!(
            "whipspot-local-{what}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        assert!(!p.exists());
        std::fs::create_dir_all(&p).unwrap();
        Scratch(p)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Rig {
    music: TempMusicDir,
    cache: Scratch,
    data: Scratch,
}

impl Rig {
    fn new() -> Rig {
        Rig {
            music: TempMusicDir::new(),
            cache: Scratch::new("cache"),
            data: Scratch::new("data"),
        }
    }

    fn config(&self) -> LocalConfig {
        LocalConfig {
            folders: vec![self.music.path().to_str().unwrap().to_owned()],
            cache_dir: self.cache.0.clone(),
            data_dir: self.data.0.clone(),
        }
    }

    /// A provider whose first scan has finished.
    fn scanned(&self) -> LocalProvider {
        let p = LocalProvider::new(self.config(), ctx());
        assert!(p.wait_idle(Duration::from_secs(20)), "the scan ends");
        p
    }
}

fn ctx() -> ProviderContext {
    let (events, _rx) = broadcast::channel(16);
    let source = Engine::new(EngineConfig::default(), None).source;
    ProviderContext { source, events }
}

/// Whether `rx` holds a `LibraryChanged` for the local provider (drains it).
fn library_changed(rx: &mut broadcast::Receiver<UiEvent>) -> bool {
    let mut seen = false;
    while let Ok(e) = rx.try_recv() {
        seen |= e
            == UiEvent::LibraryChanged {
                provider: "local".into(),
            };
    }
    seen
}

#[test]
fn every_finished_scan_says_the_library_changed() {
    let rig = Rig::new();
    let c = ctx();
    let mut rx = c.events.subscribe();
    let p = LocalProvider::new(rig.config(), c);
    assert!(p.wait_idle(Duration::from_secs(20)), "the scan ends");
    assert!(library_changed(&mut rx), "the first scan");
    // A rescan from the folders card, and a removed folder, say so again.
    let folders = p.folders();
    folders.rescan();
    assert!(p.wait_idle(Duration::from_secs(20)));
    assert!(library_changed(&mut rx), "a rescan");
    folders.remove(rig.music.path().to_str().unwrap());
    assert!(p.wait_idle(Duration::from_secs(20)));
    assert!(library_changed(&mut rx), "a removed folder");
    assert_eq!(p.scan_status().tracks, 0);
}

async fn all_tracks(lib: &dyn Library) -> Vec<Track> {
    let mut out = Vec::new();
    for a in lib.albums(0).await.unwrap().items {
        out.extend(lib.album_tracks(&a.id, 0).await.unwrap().items);
    }
    out
}

fn names(tracks: &[Track]) -> Vec<&str> {
    tracks.iter().map(|t| t.name.as_str()).collect()
}

#[tokio::test]
async fn the_music_tree_reads_as_a_library() {
    let rig = Rig::new();
    let p = rig.scanned();
    let lib = p.library().unwrap();

    let albums = lib.albums(0).await.unwrap();
    let album = albums
        .items
        .iter()
        .find(|a| a.name == ALBUM_TITLE)
        .expect("the fixture album");
    assert_eq!(album.track_count, Some(3));
    assert_eq!(album.year, Some(1999));
    assert!(album.images[0].url.starts_with("local-art:local:track:"));
    let tracks = lib.album_tracks(&album.id, 0).await.unwrap().items;
    assert_eq!(
        names(&tracks),
        ALBUM_TRACKS.map(|t| t.title),
        "the album lists its tracks in track order"
    );
    assert!(tracks[0].images.len() == 1 && tracks[1].images.is_empty());
    assert!((990..=1010).contains(&tracks[0].duration_ms));

    let all = all_tracks(lib).await;
    assert_eq!(
        all.len(),
        5,
        "three album tracks and two WAVs: {:?}",
        names(&all)
    );
    let loose = all
        .iter()
        .find(|t| t.name == "loose")
        .expect("the untagged WAV");
    assert_eq!(loose.artist, UNKNOWN_ARTIST);
    assert_eq!(loose.album, UNKNOWN_ALBUM);
    assert!(
        all.iter().all(|t| !t.name.contains("ignored")),
        ".opus is not indexed"
    );
    assert!(all.iter().all(|t| is_track_uri(&t.uri)));
    assert_eq!(p.scan_status().tracks, 5);
    assert!(!p.scan_status().scanning);

    let playlists = lib.playlists(0).await.unwrap().items;
    assert_eq!(playlists.len(), 1);
    assert_eq!(playlists[0].name, "mix");
    let items = lib.playlist_items(&playlists[0].id, 0).await.unwrap().items;
    assert_eq!(
        names(&items),
        [ALBUM_TRACKS[0].title, "loose"],
        "only the entries inside the tree"
    );

    let found = lib.search("sECOND SONG", 0).await.unwrap().items;
    assert_eq!(names(&found), [ALBUM_TRACKS[1].title]);
    assert!(lib.search("Fixture", 0).await.unwrap().items.len() >= 3);
    assert!(lib.search("zz-nothing", 0).await.unwrap().items.is_empty());
    assert!(lib.search("", 0).await.unwrap().items.is_empty());

    assert!(lib.album_tracks("album:0000000000000000", 0).await.is_err());
    assert!(lib.playlist_items("../mix.m3u", 0).await.is_err());
}

#[tokio::test]
async fn favourites_persist_across_providers() {
    let rig = Rig::new();
    let uri = {
        let p = rig.scanned();
        let lib = p.library().unwrap();
        assert!(lib.liked_tracks(0).await.unwrap().items.is_empty());
        let uri = all_tracks(lib).await[1].uri.clone();
        assert_eq!(
            lib.liked(std::slice::from_ref(&uri)).await.unwrap(),
            [false]
        );
        lib.set_liked(&uri, true).await.unwrap();
        let liked = lib.liked_tracks(0).await.unwrap().items;
        assert_eq!(liked.len(), 1);
        assert_eq!(liked[0].uri, uri);
        assert_eq!(
            lib.liked(&[uri.clone(), "local:track:0000000000000000".into()])
                .await
                .unwrap(),
            [true, false]
        );
        assert!(
            lib.set_liked("local:track:0000000000000000", true)
                .await
                .is_err()
        );
        assert!(lib.can_change_likes());
        uri
    };
    let p = rig.scanned();
    let lib = p.library().unwrap();
    let liked = lib.liked_tracks(0).await.unwrap().items;
    assert_eq!(
        liked.len(),
        1,
        "a new provider over the same app data lists it"
    );
    assert_eq!(liked[0].uri, uri);
    lib.set_liked(&uri, false).await.unwrap();
    assert!(lib.liked_tracks(0).await.unwrap().items.is_empty());
}

#[tokio::test]
async fn ids_are_stable_across_scans_and_a_restart() {
    let rig = Rig::new();
    let p = rig.scanned();
    let first: Vec<String> = all_tracks(p.library().unwrap())
        .await
        .into_iter()
        .map(|t| t.uri)
        .collect();
    p.rescan();
    assert!(p.wait_idle(Duration::from_secs(20)));
    let second: Vec<String> = all_tracks(p.library().unwrap())
        .await
        .into_iter()
        .map(|t| t.uri)
        .collect();
    assert_eq!(first, second, "the same ids after a second scan");
    drop(p);
    assert!(
        rig.cache.0.join(INDEX_FILE).is_file(),
        "the scan wrote the index"
    );

    // A restart that only reads the index (no scan) has the same ids, albums and playlists.
    let restarted = LocalProvider::from_index(rig.config(), ctx());
    let lib = restarted.library().unwrap();
    let from_index: Vec<String> = all_tracks(lib).await.into_iter().map(|t| t.uri).collect();
    assert_eq!(first, from_index);
    assert_eq!(lib.playlists(0).await.unwrap().items.len(), 1);
    assert!(first.iter().all(|u| is_track_uri(u)));
}

#[tokio::test]
async fn a_bad_index_is_a_full_rescan() {
    let rig = Rig::new();
    let index = rig.cache.0.join(INDEX_FILE);
    let good = {
        rig.scanned();
        std::fs::read(&index).unwrap()
    };
    let other_version =
        String::from_utf8(good.clone())
            .unwrap()
            .replacen("\"version\":1", "\"version\":2", 1);
    let random: Vec<u8> = (0..4096u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 11) as u8)
        .collect();
    for (what, bytes) in [
        ("another version", other_version.into_bytes()),
        ("truncated JSON", good[..good.len() / 2].to_vec()),
        ("random bytes", random),
        ("empty", Vec::new()),
    ] {
        std::fs::write(&index, &bytes).unwrap();
        let p = LocalProvider::from_index(rig.config(), ctx());
        let lib = p.library().unwrap();
        assert!(
            lib.albums(0).await.unwrap().items.is_empty(),
            "{what}: no index, no error"
        );
        p.rescan();
        assert!(p.wait_idle(Duration::from_secs(20)));
        assert_eq!(
            all_tracks(lib).await.len(),
            5,
            "{what}: the rescan rebuilt it"
        );
        assert_eq!(
            std::fs::read(&index).unwrap(),
            good,
            "{what}: index rewritten"
        );
    }
}

#[tokio::test]
async fn malformed_files_are_skipped() {
    let rig = Rig::new();
    let root = rig.music.path();
    let fixture = std::fs::read(&rig.music.tracks[0]).unwrap();
    let noise: Vec<u8> = (0..10_000u32)
        .map(|i| (i.wrapping_mul(2_246_822_519) >> 9) as u8)
        .collect();
    std::fs::write(root.join("noise.flac"), noise).unwrap();
    std::fs::write(root.join("truncated.flac"), &fixture[..fixture.len() / 3]).unwrap();
    std::fs::write(root.join("short.flac"), &fixture[..40]).unwrap();
    std::fs::write(root.join("empty.flac"), b"").unwrap();
    std::fs::write(root.join("text.mp3"), b"just some text, not audio").unwrap();
    std::fs::write(root.join("fake.caf"), b"not a caf").unwrap();
    let p = rig.scanned();
    let all = all_tracks(p.library().unwrap()).await;
    let got = names(&all);
    for bad in ["noise", "short", "empty", "text", "fake"] {
        assert!(!got.contains(&bad), "{bad} is skipped: {got:?}");
    }
    // A truncated FLAC keeps its header and tags; listing it or not are both fine, but the
    // scan finished and every good file is there.
    for good in ALBUM_TRACKS
        .map(|t| t.title)
        .iter()
        .chain(&["loose", "long"])
    {
        assert!(got.contains(good), "{good} is listed: {got:?}");
    }
}

#[test]
fn a_favourites_file_of_another_version_is_left_alone() {
    let rig = Rig::new();
    let file = rig.data.0.join(FAVOURITES_FILE);
    let foreign = br#"{"version":7,"paths":["/somewhere/else.flac"],"extra":true}"#;
    std::fs::write(&file, foreign).unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let p = rig.scanned();
    let lib = p.library().unwrap();
    rt.block_on(async {
        assert!(lib.liked_tracks(0).await.unwrap().items.is_empty());
        let uris: Vec<String> = all_tracks(lib).await.into_iter().map(|t| t.uri).collect();
        assert_eq!(lib.liked(&uris).await.unwrap(), vec![false; uris.len()]);
    });
    drop(p);
    let p = rig.scanned();
    assert_eq!(
        std::fs::read(&file).unwrap(),
        foreign,
        "unchanged until a favourite changes"
    );
    let lib = p.library().unwrap();
    rt.block_on(async {
        let uri = all_tracks(lib).await[0].uri.clone();
        lib.set_liked(&uri, true).await.unwrap();
    });
    let now: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(now["version"], 1);
    assert_eq!(now["paths"].as_array().map(Vec::len), Some(1));
}

#[test]
fn mutated_flac_never_panics_the_tag_reader() {
    let cover = png_rgb(4, 4, [1, 2, 3]);
    let picture = FlacPicture {
        mime: "image/png",
        width: 4,
        height: 4,
        data: &cover,
    };
    let seed = flac(
        44_100,
        16,
        2,
        &signal(64, 2, 16),
        &[
            ("TITLE", "T"),
            ("ARTIST", "A"),
            ("TRACKNUMBER", "1"),
            ("REPLAYGAIN_TRACK_GAIN", "-6.50 dB"),
        ],
        Some(&picture),
    );
    let tags = read_tags_from_bytes(&seed).expect("the seed reads");
    assert_eq!(tags.title.as_deref(), Some("T"));
    assert_eq!(tags.track_no, Some(1));
    assert_eq!(
        tags.rg_track_db,
        Some(-6.5),
        "ReplayGain from a Vorbis comment"
    );
    assert!(tags.has_picture);
    let dict: &[&[u8]] = &[
        b"fLaC",
        b"TITLE=",
        b"REPLAYGAIN_TRACK_GAIN=",
        b"image/png",
        &[0xff, 0xff, 0xff, 0x7f],
        &[0, 0, 0, 0],
        &[0x84, 0xff, 0xff, 0xff],
    ];
    let mut still_read = 0u32;
    fuzz("local tag reader", 0x10ca1, 1_500, |case| {
        let mut bytes = seed.clone();
        mutate(&mut bytes, &mut case.rng, dict);
        case.set_input_len(bytes.len());
        if read_tags_from_bytes(&bytes).is_some() {
            still_read += 1;
        }
    });
    assert!(
        still_read > 0,
        "some mutations still parse, so the parser is exercised"
    );
}

#[tokio::test]
async fn provider_contract_on_local() {
    let rig = Rig::new();
    let p: Arc<dyn Provider> = Arc::new(rig.scanned());
    run_provider_contract(p.clone()).await;
    // Always ready: login and logout of any step are refused with a fixed text.
    assert!(
        p.auth()
            .login("no-such-step", &Default::default())
            .await
            .is_err()
    );
    assert!(p.auth().logout("no-such-step").await.is_err());
    // An empty provider (no folders) passes too.
    let empty = LocalProvider::new(
        LocalConfig {
            folders: vec![],
            cache_dir: rig.cache.0.join("empty"),
            data_dir: rig.data.0.join("empty"),
        },
        ctx(),
    );
    assert!(empty.wait_idle(Duration::from_secs(20)));
    run_provider_contract(Arc::new(empty)).await;
}

/// S1: a compressed ID3v2 frame (a zlib bomb of 64 MiB of zeros in a file of about 420 KB) is
/// skipped by the vendored lofty, never inflated, and the rest of the tag still reads.
#[tokio::test]
async fn a_compressed_id3_frame_is_skipped_never_inflated() {
    use lofty::config::ParseOptions;
    use lofty::file::AudioFile;
    use lofty::mpeg::MpegFile;

    let bomb = id3_bomb_mp3("Bomb title", 64);
    assert!(bomb.len() < 600_000, "{}", bomb.len());

    // lofty itself: no GEOB frame was built (an inflated one would hold the 64 MiB), the TIT2
    // after it was read.
    let t = Instant::now();
    let options = ParseOptions::new().read_cover_art(true);
    let file = MpegFile::read_from(&mut Cursor::new(&bomb), options).expect("the file reads");
    let tag = file.id3v2().expect("the ID3v2 tag reads");
    let ids: Vec<&str> = tag.into_iter().map(|f| f.id_str()).collect();
    assert_eq!(ids, ["TIT2"]);
    let tags = read_tags_from_bytes(&bomb).expect("the scanner's reader takes it");
    assert_eq!(tags.title.as_deref(), Some("Bomb title"));
    assert!(!tags.has_picture);
    // Liveness bound, loose for loaded hosts.
    assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());

    // The scan indexes it and finishes.
    let rig = Rig::new();
    std::fs::write(rig.music.path().join("bomb.mp3"), &bomb).unwrap();
    let p = rig.scanned();
    let lib = p.library().unwrap();
    let found = lib.search("bomb title", 0).await.unwrap().items;
    assert_eq!(names(&found), ["Bomb title"]);
}
