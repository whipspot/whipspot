//! N1: `NetArt` over a fake transport. Nothing here touches the network.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};
use whipspot_core::art::{ArtError, ArtSource};
use whipspot_core::provider::{BoxFuture, ImageRef};
use whipspot_net::NetArt;
use whipspot_net::transport::{FetchError, Fetched, Transport};
use whipspot_testkit::{run_art_error_contract, run_art_source_contract};

const GOOD: &str = "https://i.scdn.co/good.png";

/// Canned answers by URL text; every call is recorded.
#[derive(Default)]
struct Fake {
    answers: Mutex<HashMap<String, Result<Fetched, FetchError>>>,
    calls: Mutex<Vec<String>>,
}

impl Fake {
    fn set(&self, url: &str, answer: Result<Fetched, FetchError>) {
        self.answers.lock().unwrap().insert(url.to_owned(), answer);
    }

    fn status(&self, url: &str, status: u16, body: &[u8]) {
        self.set(
            url,
            Ok(Fetched {
                status,
                retry_after: None,
                body: body.to_vec(),
            }),
        );
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

impl Transport for Fake {
    fn get<'a>(
        &'a self,
        url: &'a reqwest::Url,
        _cap: usize,
    ) -> BoxFuture<'a, Result<Fetched, FetchError>> {
        self.calls.lock().unwrap().push(url.as_str().to_owned());
        let answer = self
            .answers
            .lock()
            .unwrap()
            .get(url.as_str())
            .cloned()
            .unwrap_or(Err(FetchError::Network));
        Box::pin(async move { answer })
    }
}

fn png(w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().unwrap();
        writer
            .write_image_data(&vec![77u8; (w * h * 3) as usize])
            .unwrap();
    }
    out
}

fn image(url: &str) -> ImageRef {
    ImageRef {
        url: url.to_owned(),
        width: None,
        height: None,
    }
}

fn hash(url: &str) -> String {
    Sha256::digest(url.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn setup(cache: Option<&std::path::Path>) -> (Arc<Fake>, NetArt) {
    let fake = Arc::new(Fake::default());
    let art = NetArt::with_transport(
        fake.clone(),
        cache.map(|p| p.to_path_buf()),
        &["scdn.co", "spotifycdn.com"],
    );
    (fake, art)
}

fn run<F: Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(f)
}

#[test]
fn refused_urls_never_reach_the_transport() {
    run(async {
        let (fake, art) = setup(None);
        for u in [
            "https://evil.example/x",
            "http://i.scdn.co/x",
            "https://i.scdn.co@evil.example/x",
            "https://127.0.0.1/x",
            "not a url",
        ] {
            assert_eq!(art.fetch(&image(u)).await, Err(ArtError::Refused), "{u}");
        }
        let long = format!("https://i.scdn.co/{}", "a".repeat(2100));
        assert_eq!(art.fetch(&image(&long)).await, Err(ArtError::Refused));
        assert!(fake.calls().is_empty());
    });
}

#[test]
fn statuses_and_junk_map_to_fixed_errors() {
    run(async {
        let dir = tempfile::tempdir().unwrap();
        let (fake, art) = setup(Some(dir.path()));
        fake.status(GOOD, 404, b"");
        assert_eq!(art.fetch(&image(GOOD)).await, Err(ArtError::Unavailable));
        fake.status(GOOD, 302, b"");
        assert_eq!(art.fetch(&image(GOOD)).await, Err(ArtError::Unavailable));
        fake.set(GOOD, Err(FetchError::Network));
        assert_eq!(art.fetch(&image(GOOD)).await, Err(ArtError::Unavailable));
        fake.set(GOOD, Err(FetchError::TooLarge));
        assert_eq!(art.fetch(&image(GOOD)).await, Err(ArtError::TooLarge));
        fake.status(GOOD, 200, b"<html>not an image</html>");
        assert_eq!(art.fetch(&image(GOOD)).await, Err(ArtError::Undecodable));
        // No cache file for any failure.
        let art_dir = dir.path().join("art");
        let count = std::fs::read_dir(&art_dir).map(|d| d.count()).unwrap_or(0);
        assert_eq!(count, 0, "a failure left a cache file");
    });
}

#[test]
fn a_valid_png_is_cached_by_hash_and_served_from_the_cache() {
    run(async {
        let dir = tempfile::tempdir().unwrap();
        let (fake, art) = setup(Some(dir.path()));
        fake.status(GOOD, 200, &png(4, 3));
        let first = art.fetch(&image(GOOD)).await.unwrap();
        assert_eq!((first.width(), first.height()), (4, 3));
        assert_eq!(fake.calls(), vec![GOOD.to_owned()], "same as_str()");
        let cached = dir.path().join("art").join(hash(GOOD));
        assert!(cached.is_file(), "no cache file at {cached:?}");
        // The second fetch is served from the cache: no transport call.
        let second = art.fetch(&image(GOOD)).await.unwrap();
        assert_eq!(first, second);
        assert_eq!(fake.calls().len(), 1);
    });
}

#[test]
fn a_corrupted_cache_file_is_replaced() {
    run(async {
        let dir = tempfile::tempdir().unwrap();
        let (fake, art) = setup(Some(dir.path()));
        let art_dir = dir.path().join("art");
        std::fs::create_dir_all(&art_dir).unwrap();
        let file = art_dir.join(hash(GOOD));
        std::fs::write(&file, b"corrupt").unwrap();
        fake.status(GOOD, 200, &png(2, 2));
        let got = art.fetch(&image(GOOD)).await.unwrap();
        assert_eq!((got.width(), got.height()), (2, 2));
        assert_eq!(fake.calls().len(), 1, "the corrupt hit went to the network");
        assert_ne!(std::fs::read(&file).unwrap(), b"corrupt");
        // And the replacement is now a hit.
        art.fetch(&image(GOOD)).await.unwrap();
        assert_eq!(fake.calls().len(), 1);
    });
}

#[test]
fn an_oversize_side_is_too_large_and_not_cached() {
    run(async {
        let dir = tempfile::tempdir().unwrap();
        let (fake, art) = setup(Some(dir.path()));
        fake.status(GOOD, 200, &png(2049, 1));
        assert_eq!(art.fetch(&image(GOOD)).await, Err(ArtError::TooLarge));
        assert!(!dir.path().join("art").join(hash(GOOD)).exists());
    });
}

#[test]
fn the_art_source_contract_holds() {
    run(async {
        let (fake, art) = setup(None);
        fake.status(GOOD, 200, &png(8, 8));
        run_art_source_contract(&art, &image(GOOD), &image("https://evil.example/x")).await;
    });
}

#[test]
fn the_art_error_matrix_holds() {
    run(async {
        let (fake, art) = setup(None);
        let (down, gone, huge, wide, junk, page) = (
            "https://i.scdn.co/down.png",
            "https://i.scdn.co/gone.png",
            "https://i.scdn.co/huge.png",
            "https://i.scdn.co/wide.png",
            "https://i.scdn.co/junk.png",
            "https://i.scdn.co/page.png",
        );
        fake.set(down, Err(FetchError::Network));
        fake.status(gone, 404, b"");
        fake.set(huge, Err(FetchError::TooLarge));
        fake.status(wide, 200, &png(2049, 1));
        fake.status(junk, 200, b"<html>not an image</html>");
        fake.status(page, 302, b"");
        run_art_error_contract(
            &art,
            &[
                (image("https://evil.example/x"), ArtError::Refused),
                (image("http://i.scdn.co/x"), ArtError::Refused),
                (image(""), ArtError::Refused),
                (image(down), ArtError::Unavailable),
                (image(gone), ArtError::Unavailable),
                (image(page), ArtError::Unavailable),
                (image(huge), ArtError::TooLarge),
                (image(wide), ArtError::TooLarge),
                (image(junk), ArtError::Undecodable),
            ],
        )
        .await;
    });
}
