//! Local art (embedded and folder images, caps, downscaling, junk) and the music folders seam.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, SystemTime};

use tokio::sync::broadcast;
use whipspot_core::art::{ArtError, ArtSource, MAX_ART_SIDE};
use whipspot_core::engine::{Engine, EngineConfig};
use whipspot_core::folders::MusicFolders;
use whipspot_core::provider::{ImageRef, Provider, ProviderContext, Track};
use whipspot_local::{LocalConfig, LocalProvider, MAX_IMAGE_BYTES, decode_art};
use whipspot_testkit::audio_files::{ALBUM_TRACKS, TempMusicDir, flac, png_rgb, signal};
use whipspot_testkit::rng::{fuzz, mutate};
use whipspot_testkit::run_music_folders_contract;

static NEXT: AtomicU32 = AtomicU32::new(0);

struct Scratch(PathBuf);

impl Scratch {
    fn new(what: &str) -> Scratch {
        let p = std::env::temp_dir().join(format!(
            "whipspot-artf-{what}-{}-{}",
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

fn ctx() -> ProviderContext {
    let (events, _rx) = broadcast::channel(16);
    let source = Engine::new(EngineConfig::default(), None).source;
    ProviderContext { source, events }
}

fn provider(folders: Vec<String>, cache: &Scratch, data: &Scratch) -> LocalProvider {
    let p = LocalProvider::new(
        LocalConfig {
            folders,
            cache_dir: cache.0.clone(),
            data_dir: data.0.clone(),
        },
        ctx(),
    );
    assert!(p.wait_idle(Duration::from_secs(20)));
    p
}

async fn tracks(p: &LocalProvider) -> Vec<Track> {
    let lib = p.library().unwrap();
    let mut out = Vec::new();
    for a in lib.albums(0).await.unwrap().items {
        out.extend(lib.album_tracks(&a.id, 0).await.unwrap().items);
    }
    out
}

fn jpeg(w: u16, h: u16) -> Vec<u8> {
    let px: Vec<u8> = (0..usize::from(w) * usize::from(h))
        .flat_map(|_| [30u8, 160, 90])
        .collect();
    let mut out = Vec::new();
    jpeg_encoder::Encoder::new(&mut out, 90)
        .encode(&px, w, h, jpeg_encoder::ColorType::Rgb)
        .unwrap();
    out
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

#[tokio::test(flavor = "multi_thread")]
async fn embedded_and_folder_art_decode() {
    let music = TempMusicDir::new();
    // A second album whose track has no picture, with a folder image in an odd case.
    let other = music.path().join("Other");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(
        other.join("x.flac"),
        flac(
            44_100,
            16,
            2,
            &signal(100, 2, 16),
            &[("TITLE", "Plain"), ("ALBUM", "Other")],
            None,
        ),
    )
    .unwrap();
    std::fs::write(other.join("Cover.JPG"), jpeg(6, 5)).unwrap();
    let (cache, data) = (Scratch::new("cache"), Scratch::new("data"));
    let p = provider(
        vec![music.path().to_str().unwrap().to_owned()],
        &cache,
        &data,
    );
    let art = p.art();
    let all = tracks(&p).await;
    let first = all
        .iter()
        .find(|t| t.name == ALBUM_TRACKS[0].title)
        .unwrap();
    let img = art.fetch(&first.images[0]).await.unwrap();
    assert_eq!(
        (img.width(), img.height()),
        (4, 4),
        "the embedded 4x4 cover"
    );
    assert_eq!(&img.rgba()[..4], &[200, 30, 60, 255]);

    let plain = all.iter().find(|t| t.name == "Plain").unwrap();
    assert_eq!(plain.images.len(), 1, "a folder image gives the track art");
    let img = art.fetch(&plain.images[0]).await.unwrap();
    assert_eq!(
        (img.width(), img.height()),
        (6, 5),
        "Cover.JPG, found in any case"
    );

    let refused = |url: &str| ImageRef {
        url: url.to_owned(),
        width: None,
        height: None,
    };
    for url in [
        "local-art:local:track:0000000000000000",
        "local-art:../../etc/passwd",
        "https://example.invalid/a.png",
        "",
    ] {
        assert!(art.fetch(&refused(url)).await.is_err(), "{url:?}");
    }
}

#[test]
fn large_images_are_downscaled_and_oversize_refused() {
    let img = decode_art(&png_rgb(4096, 1024, [10, 20, 30])).unwrap();
    assert_eq!((img.width(), img.height()), (MAX_ART_SIDE, 512));
    assert_eq!(&img.rgba()[..4], &[10, 20, 30, 255]);
    let img = decode_art(&png_rgb(2049, 2049, [1, 2, 3])).unwrap();
    assert!(img.width() <= MAX_ART_SIDE && img.height() <= MAX_ART_SIDE);
    let img = decode_art(&jpeg(3000, 100)).unwrap();
    assert_eq!(img.width(), MAX_ART_SIDE);

    // A 5000 x 5000 header (with a valid CRC) is refused before anything is allocated.
    let mut p = png_rgb(8, 8, [0, 0, 0]);
    p[16..20].copy_from_slice(&5_000u32.to_be_bytes());
    p[20..24].copy_from_slice(&5_000u32.to_be_bytes());
    let crc = crc32(&p[12..29]);
    p[29..33].copy_from_slice(&crc.to_be_bytes());
    assert_eq!(decode_art(&p).err(), Some(ArtError::TooLarge));
    // Over 20 MiB of bytes.
    let mut big = png_rgb(8, 8, [0, 0, 0]);
    big.resize(MAX_IMAGE_BYTES as usize + 1, 0);
    assert_eq!(decode_art(&big).err(), Some(ArtError::TooLarge));
    assert!(decode_art(b"").is_err());
    assert!(decode_art(b"GIF89a").is_err());
}

#[test]
fn random_and_mutated_image_bytes_never_panic() {
    let seeds = [png_rgb(8, 6, [9, 8, 7]), jpeg(8, 6)];
    let dict: &[&[u8]] = &[
        b"IHDR",
        b"IDAT",
        &[0xff, 0xd8, 0xff],
        &[0xff, 0xc0],
        &[0xff; 4],
    ];
    fuzz("local art decode", 0xa27, 1_500, |case| {
        let mut bytes = case.rng.pick(&seeds).clone();
        mutate(&mut bytes, &mut case.rng, dict);
        case.set_input_len(bytes.len());
        let _ = decode_art(&bytes);
        let mut junk = vec![0u8; case.rng.below(512)];
        case.rng.fill(&mut junk);
        let _ = decode_art(&junk);
    });
}

/// Every file under `root` with its length and modification time.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, (u64, SystemTime)> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).unwrap().flatten() {
            let m = e.metadata().unwrap();
            if m.is_dir() {
                stack.push(e.path());
            } else {
                out.insert(e.path(), (m.len(), m.modified().unwrap()));
            }
        }
    }
    out
}

#[test]
fn music_folders_contract_on_local() {
    let (a, b, cache, data) = (
        Scratch::new("a"),
        Scratch::new("b"),
        Scratch::new("cache"),
        Scratch::new("data"),
    );
    let file = a.0.join("not-a-folder.txt");
    std::fs::write(&file, b"x").unwrap();
    let p = provider(vec![], &cache, &data);
    run_music_folders_contract(&*p.folders(), [&a.0, &b.0], &file);
}

#[test]
fn a_rescan_never_touches_the_files() {
    let music = TempMusicDir::new();
    let (cache, data) = (Scratch::new("cache"), Scratch::new("data"));
    let before = snapshot(music.path());
    let p = provider(vec![], &cache, &data);
    let folders = p.folders();
    folders
        .add(music.path().to_str().unwrap().to_owned())
        .unwrap();
    assert!(p.wait_idle(Duration::from_secs(20)));
    assert_eq!(folders.status().tracks, 5);
    folders.rescan();
    assert!(p.wait_idle(Duration::from_secs(20)));
    folders.remove(music.path().to_str().unwrap());
    assert!(p.wait_idle(Duration::from_secs(20)));
    assert_eq!(folders.status().tracks, 0, "removing forgets the tracks");
    assert_eq!(
        snapshot(music.path()),
        before,
        "no file was written, moved or removed"
    );
}
