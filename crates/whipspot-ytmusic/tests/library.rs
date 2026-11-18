//! The library: playlists, albums, liked songs, tracks, search and likes over a fake InnerTube
//! (`FakeYt`), the provider contracts, paging by continuation, and the parser against bad input.
//! Nothing is fetched and nothing is run; every JSON value is made up in `fixtures`.

mod fixtures;

use std::sync::Arc;

use serde_json::{Value, json};
use whipspot_core::provider::model::LoginState;
use whipspot_core::provider::{
    Album, AlbumKind, EntityRef, ImageRef, LibErrorKind, Library, MAX_ARTISTS, MAX_IMAGES,
    MAX_POPULAR, Playlist, Provider, ReleaseGroup,
};
use whipspot_testkit::rng::{Case, SplitMix64, fuzz, mutate};
use whipspot_testkit::{
    run_provider_contract, run_provider_playback_contract, run_provider_session_contract,
};
use whipspot_ytmusic::auth::STEP_COOKIES;
use whipspot_ytmusic::http::{Cap, Http, HttpError};
use whipspot_ytmusic::ids;
use whipspot_ytmusic::library::{MAX_REWALK_PAGES, MSG_IMPORT_COOKIES};
use whipspot_ytmusic::parse::{
    self, AlbumHeader, IMAGE_HOSTS, MAX_ITEMS, MAX_TEXT, Page, TrackRow,
};

use fixtures::*;

fn lib(rig: &Rig) -> &dyn Library {
    rig.provider.library().expect("a library")
}

async fn logged_in() -> Rig {
    let rig = Rig::new();
    rig.login_cookies().await;
    rig
}

fn names(items: &[whipspot_core::provider::Track]) -> Vec<&str> {
    items.iter().map(|t| t.name.as_str()).collect()
}

// ---- contract 3: the provider contracts ---------------------------------------------------------

#[tokio::test]
async fn the_provider_contract_passes_with_fake_yt() {
    let rig = Rig::new();
    let info = rig.provider.info();
    assert!(info.caps.library && info.caps.search && info.caps.likes);
    assert!(info.caps.pcm_tap && info.caps.eq);
    run_provider_contract(rig.provider.clone()).await;
    run_provider_playback_contract(rig.provider.clone(), None, "ytmusic:track:dQw4w9WgXcQ").await;
}

#[tokio::test]
async fn the_session_contract_passes_with_fake_yt() {
    for page_size in [1, 2, 3, 100] {
        let rig = Rig::new();
        rig.yt.set_page_size(page_size);
        let logins = rig.logins();
        run_provider_session_contract(rig.provider.clone(), &logins, rig.bad_login()).await;
    }
}

// ---- contract 4: logged out ----------------------------------------------------------------------

#[tokio::test]
async fn every_library_call_while_logged_out_is_the_fixed_error() {
    let rig = Rig::new();
    let lib = lib(&rig);
    let uris = vec![track_uri("trackAAAA01")];
    let errors = [
        lib.playlists(0).await.unwrap_err(),
        lib.albums(0).await.unwrap_err(),
        lib.liked_tracks(0).await.unwrap_err(),
        lib.playlist_items(&playlist_uri(PLAYLIST_ONE), 0)
            .await
            .unwrap_err(),
        lib.album_tracks(&album_uri(ALBUM), 0).await.unwrap_err(),
        lib.search("night", 0).await.unwrap_err(),
        lib.search("", 0).await.unwrap_err(),
        lib.liked(&uris).await.unwrap_err(),
        lib.set_liked(&uris[0], true).await.unwrap_err(),
        lib.artist(&artist_uri(ARTISTS[0].1)).await.unwrap_err(),
        lib.album(&album_uri(ALBUM)).await.unwrap_err(),
        lib.artist_releases(&artist_uri(ARTISTS[0].1), ReleaseGroup::All, 0)
            .await
            .unwrap_err(),
        lib.set_liked(&album_uri(ALBUM), true).await.unwrap_err(),
    ];
    for e in &errors {
        assert_eq!(e.message, "Import your YouTube cookies first");
        assert_eq!(e.kind, LibErrorKind::WebNotLoggedIn);
    }
    assert_eq!(MSG_IMPORT_COOKIES, "Import your YouTube cookies first");
    assert!(!lib.can_change_likes());
    assert!(rig.yt.posts().is_empty(), "nothing is sent without cookies");
}

// ---- what the library lists ------------------------------------------------------------------------

async fn all_playlists(lib: &dyn Library) -> (Vec<Playlist>, u32) {
    let (mut all, mut skipped, mut offset) = (Vec::new(), 0, 0);
    loop {
        let page = lib.playlists(offset).await.unwrap();
        skipped += page.skipped;
        all.extend(page.items);
        match page.next_offset {
            Some(n) => offset = n,
            None => return (all, skipped),
        }
    }
}

async fn all_albums(lib: &dyn Library) -> Vec<Album> {
    let (mut all, mut offset) = (Vec::new(), 0);
    loop {
        let page = lib.albums(offset).await.unwrap();
        all.extend(page.items);
        match page.next_offset {
            Some(n) => offset = n,
            None => return all,
        }
    }
}

#[tokio::test]
async fn playlists_and_albums_are_listed_with_prefixed_ids_and_clean_images() {
    for size in [1, 2, 50] {
        let rig = logged_in().await;
        rig.yt.set_page_size(size);
        let (playlists, skipped) = all_playlists(lib(&rig)).await;
        // The "New playlist" button has no playlist behind it: it is skipped, not an error.
        assert_eq!(skipped, 1, "page size {size}");
        assert_eq!(
            playlists
                .iter()
                .map(|p| (p.id.as_str(), p.name.as_str(), p.track_count))
                .collect::<Vec<_>>(),
            vec![
                (playlist_uri(PLAYLIST_ONE).as_str(), "Road Trip", Some(3)),
                (playlist_uri(PLAYLIST_TWO).as_str(), "Evening", Some(3)),
            ],
            "page size {size}"
        );
        let albums = all_albums(lib(&rig)).await;
        assert_eq!(albums.len(), 1);
        let a = &albums[0];
        assert_eq!(
            (a.id.as_str(), a.name.as_str(), a.artist.as_str(), a.year),
            (
                album_uri(ALBUM).as_str(),
                "Fake Album",
                "Album Artist",
                Some(2019)
            )
        );
        for images in playlists.iter().map(|p| &p.images).chain([&a.images]) {
            assert_images_clean(images);
        }
    }
}

fn assert_images_clean(images: &[ImageRef]) {
    assert!(!images.is_empty() && images.len() <= MAX_IMAGES);
    for i in images {
        let url = reqwest_host(&i.url);
        assert!(i.url.starts_with("https://"), "{}", i.url);
        assert!(
            IMAGE_HOSTS
                .iter()
                .any(|h| url == *h || url.ends_with(&format!(".{h}"))),
            "{}",
            i.url
        );
    }
    // The widest first: the 640 px ytimg one before the 226 and 60 px lh3 ones; the 1200 px ones
    // on a foreign host or plain http are not there.
    let widths: Vec<u32> = images.iter().filter_map(|i| i.width).collect();
    let mut sorted = widths.clone();
    sorted.sort_unstable_by(|a, b| b.cmp(a));
    assert_eq!(widths, sorted);
    assert_eq!(widths.first(), Some(&640));
    assert!(!widths.contains(&1200));
}

fn reqwest_host(url: &str) -> String {
    url.trim_start_matches("https://")
        .split('/')
        .next()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn playlist_items_have_their_fields_filled() {
    let rig = logged_in().await;
    rig.yt.set_page_size(10);
    let page = lib(&rig)
        .playlist_items(&playlist_uri(PLAYLIST_ONE), 0)
        .await
        .unwrap();
    assert_eq!(page.next_offset, None);
    assert_eq!(page.skipped, 0);
    assert_eq!(
        names(&page.items),
        ["Moonlit Drive", "Paper Boats", "Quiet Rooms"]
    );
    let t = &page.items[1];
    assert_eq!(t.uri, "ytmusic:track:trackAAAA02");
    assert_eq!(t.artist, "Artist A, Artist B");
    assert_eq!(t.album, "First Record");
    assert_eq!(t.duration_ms, (4 * 60 + 5) * 1000);
    assert_eq!(t.year, None);
    assert_images_clean(&t.images);
    // Another row: an hour-long duration.
    let page = lib(&rig)
        .playlist_items(&playlist_uri(PLAYLIST_TWO), 0)
        .await
        .unwrap();
    assert_eq!(page.items[0].duration_ms, (3600 + 60 + 5) * 1000);
}

#[tokio::test]
async fn album_tracks_take_the_album_the_year_and_the_cover_from_the_header() {
    let rig = logged_in().await;
    rig.yt.set_page_size(2);
    let lib = lib(&rig);
    let first = lib.album_tracks(&album_uri(ALBUM), 0).await.unwrap();
    // The second page has no header: the one kept from the first is used.
    let second = lib
        .album_tracks(&album_uri(ALBUM), first.next_offset.unwrap())
        .await
        .unwrap();
    let all: Vec<_> = first.items.iter().chain(&second.items).collect();
    assert_eq!(
        all.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
        ["Album Opener", "Album Middle Night", "Album Closer"]
    );
    for t in all {
        assert_eq!(
            (t.album.as_str(), t.artist.as_str(), t.year),
            ("Fake Album", "Album Artist", Some(2019))
        );
        assert_images_clean(&t.images);
    }
    assert_eq!(second.next_offset, None);
}

#[tokio::test]
async fn liked_tracks_search_and_the_like_state() {
    let rig = logged_in().await;
    rig.yt.set_page_size(100);
    let lib = lib(&rig);
    let liked = lib.liked_tracks(0).await.unwrap();
    assert_eq!(
        names(&liked.items),
        ["Liked Only", "Moonlit Drive", "Album Opener"]
    );
    let uris: Vec<String> = liked.items.iter().map(|t| t.uri.clone()).collect();
    assert_eq!(lib.liked(&uris).await.unwrap(), vec![true, true, true]);

    // "night" matches two titles, whatever the case.
    let found = lib.search("  NIGHT ", 0).await.unwrap();
    assert_eq!(names(&found.items), ["Night Bus", "Album Middle Night"]);
    assert_eq!(found.items[0].artist, "Artist A");
    assert_eq!(found.items[0].album, "Second Record");
    assert_eq!(found.items[0].duration_ms, (3600 + 60 + 5) * 1000);
    // A track seen in search is liked or not as the answer said.
    let probe = [
        found.items[0].uri.clone(),
        uris[0].clone(),
        "x".into(),
        String::new(),
    ];
    assert_eq!(
        lib.liked(&probe).await.unwrap(),
        vec![false, true, false, false]
    );
    assert!(
        lib.search("zzzz-no-such-track", 0)
            .await
            .unwrap()
            .items
            .is_empty()
    );
    // An empty query finds nothing and sends nothing.
    rig.yt.clear_posts();
    assert!(lib.search("   ", 0).await.unwrap().items.is_empty());
    assert!(rig.yt.posts().is_empty());
}

#[tokio::test]
async fn set_liked_posts_the_video_id_and_changes_what_the_next_reads_say() {
    let rig = logged_in().await;
    rig.yt.set_page_size(100);
    let lib = lib(&rig);
    assert!(lib.can_change_likes());
    let target = track_uri("trackAAAA05");
    assert_eq!(
        lib.liked(std::slice::from_ref(&target)).await.unwrap(),
        vec![false]
    );

    lib.set_liked(&target, true).await.unwrap();
    let post = rig
        .yt
        .posts_to("like/like")
        .pop()
        .expect("a like was posted");
    assert_eq!(post.body["target"], json!({"videoId": "trackAAAA05"}));
    assert_eq!(post.body["context"]["client"]["clientName"], "WEB_REMIX");
    assert_eq!(
        lib.liked(std::slice::from_ref(&target)).await.unwrap(),
        vec![true]
    );
    // The service has it too: it is first in the next liked list.
    let liked = lib.liked_tracks(0).await.unwrap();
    assert_eq!(liked.items[0].uri, target);

    lib.set_liked(&target, false).await.unwrap();
    assert_eq!(rig.yt.posts_to("like/removelike").len(), 1);
    assert_eq!(
        lib.liked(std::slice::from_ref(&target)).await.unwrap(),
        vec![false]
    );
    assert!(!names(&lib.liked_tracks(0).await.unwrap().items).contains(&"Glass Harbour"));

    // A uri that is not a track uri never reaches the service.
    rig.yt.clear_posts();
    for bad in [
        "",
        "ytmusic:track:",
        "ytmusic:track:a b",
        "ytmusic:playlist:abc",
        "spotify:track:x",
        &track_uri(&"a".repeat(65)),
    ] {
        assert_eq!(
            lib.set_liked(bad, true).await.unwrap_err().kind,
            LibErrorKind::InvalidArgument,
            "{bad:?}"
        );
    }
    assert!(rig.yt.posts().is_empty());
}

#[tokio::test]
async fn what_was_cached_for_one_login_is_not_used_for_the_next() {
    let rig = logged_in().await;
    rig.yt.set_page_size(100);
    let lib = lib(&rig);
    let target = track_uri("trackAAAA10");
    lib.liked_tracks(0).await.unwrap();
    assert_eq!(
        lib.liked(std::slice::from_ref(&target)).await.unwrap(),
        vec![true]
    );
    rig.provider.auth().logout(STEP_COOKIES).await.unwrap();
    assert!(lib.liked(std::slice::from_ref(&target)).await.is_err());
    rig.login_cookies().await;
    // The new login has seen no list yet: the answer falls back to false, it does not reuse the old.
    assert_eq!(
        lib.liked(std::slice::from_ref(&target)).await.unwrap(),
        vec![false]
    );
    lib.liked_tracks(0).await.unwrap();
    assert_eq!(
        lib.liked(std::slice::from_ref(&target)).await.unwrap(),
        vec![true]
    );
}

// ---- paging by continuation ------------------------------------------------------------------------------

#[tokio::test]
async fn the_next_page_by_its_offset_costs_one_request_with_its_token() {
    let rig = logged_in().await;
    rig.yt.set_page_size(2);
    let id = playlist_uri(PLAYLIST_ONE);
    rig.yt.clear_posts();
    let first = lib(&rig).playlist_items(&id, 0).await.unwrap();
    assert_eq!((first.items.len(), first.next_offset), (2, Some(2)));
    assert_eq!(rig.yt.posts().len(), 1);
    let second = lib(&rig).playlist_items(&id, 2).await.unwrap();
    assert_eq!((second.items.len(), second.next_offset), (1, None));
    let posts = rig.yt.posts();
    assert_eq!(posts.len(), 2, "one request per page");
    // The newer kind: the token is in the body.
    assert!(posts[1].body["continuation"].as_str().is_some());
    assert_eq!(names(&second.items), ["Quiet Rooms"]);
}

#[tokio::test]
async fn the_older_kind_of_continuation_puts_the_token_in_the_url() {
    let rig = logged_in().await;
    rig.yt.set_page_size(1);
    // Playlists (a grid) and search both use it.
    let p0 = lib(&rig).playlists(0).await.unwrap();
    assert_eq!(
        (p0.items.len(), p0.skipped, p0.next_offset),
        (0, 1, Some(1))
    );
    let p1 = lib(&rig).playlists(1).await.unwrap();
    assert_eq!(p1.items[0].name, "Road Trip");
    let s0 = lib(&rig).search("night", 0).await.unwrap();
    let s1 = lib(&rig)
        .search("night", s0.next_offset.unwrap())
        .await
        .unwrap();
    assert_eq!(
        (names(&s0.items), names(&s1.items), s1.next_offset),
        (vec!["Night Bus"], vec!["Album Middle Night"], None)
    );
    let browse = rig.yt.posts_to("browse");
    assert!(
        browse[1]
            .url
            .contains("&ctoken=pls.x.1&continuation=pls.x.1"),
        "{}",
        browse[1].url
    );
    assert_eq!(browse[1].body["browseId"], "FEmusic_liked_playlists");
    let search = rig.yt.posts_to("search");
    assert!(search[1].url.contains("ctoken=s."), "{}", search[1].url);
    assert_eq!(search[1].body["query"], "night");
    assert_eq!(search[0].body["params"], "EgWKAQIIAWoMEA4QChADEAQQCRAF");
}

#[tokio::test]
async fn an_offset_without_a_kept_token_is_found_by_walking_the_list_again() {
    let rig = logged_in().await;
    rig.yt.set_page_size(1);
    rig.yt.add_long_playlist("PLlong000000000001", 30);
    let id = playlist_uri("PLlong000000000001");
    let lib = lib(&rig);
    // Logging in again drops every kept token.
    rig.login_cookies().await;
    rig.yt.clear_posts();
    let page = lib.playlist_items(&id, 15).await.unwrap();
    assert_eq!(page.items[0].name, "Long longtrack16");
    // 15 pages to find the token, one more for the page itself.
    assert_eq!(rig.yt.posts().len(), 16);

    // Too far to find: the walk stops at 20 pages.
    rig.login_cookies().await;
    rig.yt.clear_posts();
    let err = lib.playlist_items(&id, 25).await.unwrap_err();
    assert_eq!(err.kind, LibErrorKind::InvalidArgument);
    assert_eq!(err.message, "That position is not in the list");
    assert_eq!(rig.yt.posts().len(), MAX_REWALK_PAGES);

    // An offset that is not where any page starts.
    let rig = logged_in().await;
    rig.yt.set_page_size(2);
    rig.provider
        .auth()
        .login(STEP_COOKIES, &rig.cookies_input())
        .await
        .unwrap();
    let err = rig
        .provider
        .library()
        .unwrap()
        .playlist_items(&playlist_uri(PLAYLIST_ONE), 1)
        .await
        .unwrap_err();
    assert_eq!(err.kind, LibErrorKind::InvalidArgument);
}

// ---- ids ----------------------------------------------------------------------------------------------------

#[tokio::test]
async fn ids_are_validated_and_a_bad_one_never_reaches_the_service() {
    let rig = logged_in().await;
    rig.yt.clear_posts();
    let bad = [
        "",
        "no-such-id",
        PLAYLIST_ONE,
        "ytmusic:playlist:",
        "ytmusic:playlist:../x",
        "ytmusic:playlist:a b",
        "ytmusic:playlist:a\nb",
        "ytmusic:album:abc",
        "ytmusic:track:abc",
        &playlist_uri(&"a".repeat(65)),
    ];
    for id in bad {
        let e = lib(&rig).playlist_items(id, 0).await.unwrap_err();
        assert_eq!(e.kind, LibErrorKind::InvalidArgument, "{id:?}");
    }
    for id in [
        "",
        "ytmusic:playlist:abc",
        ALBUM,
        "ytmusic:album:",
        "ytmusic:album:a/b",
    ] {
        let e = lib(&rig).album_tracks(id, 0).await.unwrap_err();
        assert_eq!(e.kind, LibErrorKind::InvalidArgument, "{id:?}");
    }
    assert!(rig.yt.posts().is_empty());
    // A valid id the service does not know is the service's error, not a panic.
    let e = lib(&rig)
        .playlist_items(&playlist_uri("PLunknown0000000000"), 0)
        .await
        .unwrap_err();
    assert_eq!(e.kind, LibErrorKind::Http);

    assert_eq!(
        ids::track_uri("abc-_XYZ019"),
        Some("ytmusic:track:abc-_XYZ019".to_owned())
    );
    for bad in ["", "a b", "a/b", "é", &"a".repeat(65)] {
        assert!(
            ids::track_uri(bad).is_none()
                && ids::playlist_uri(bad).is_none()
                && ids::album_uri(bad).is_none()
        );
    }
    assert_eq!(ids::playlist_id_of_browse_id("VLPLabc"), Some("PLabc"));
    assert_eq!(ids::playlist_id_of_browse_id("VLLM"), Some("LM"));
    assert_eq!(ids::parse_track_uri("ytmusic:track:x_-"), Some("x_-"));
    assert_eq!(ids::parse_track_uri("ytmusic:playlist:x"), None);
}

#[tokio::test]
async fn the_provider_names_the_image_hosts_and_the_liked_list() {
    let rig = Rig::new();
    assert_eq!(
        rig.provider.image_hosts(),
        ["lh3.googleusercontent.com", "i.ytimg.com", "yt3.ggpht.com"]
    );
    rig.login_cookies().await;
    rig.yt.clear_posts();
    lib(&rig).liked_tracks(0).await.unwrap();
    assert_eq!(rig.yt.posts()[0].body["browseId"], "VLLM");
    lib(&rig).album_tracks(&album_uri(ALBUM), 0).await.unwrap();
    assert_eq!(rig.yt.posts()[1].body["browseId"], ALBUM);
    lib(&rig)
        .playlist_items(&playlist_uri(PLAYLIST_TWO), 0)
        .await
        .unwrap();
    assert_eq!(
        rig.yt.posts()[2].body["browseId"],
        format!("VL{PLAYLIST_TWO}")
    );
}

// ---- errors ----------------------------------------------------------------------------------------------------

#[tokio::test]
async fn service_and_network_failures_are_fixed_errors_of_the_right_kind() {
    let rig = logged_in().await;
    let lib = lib(&rig);
    let cases: Vec<(Script, LibErrorKind, &str)> = vec![
        (
            Script::Status(429),
            LibErrorKind::RateLimited,
            "YouTube Music asked to slow down",
        ),
        (
            Script::Status(500),
            LibErrorKind::Http,
            "YouTube Music answered with an error",
        ),
        (
            Script::Status(404),
            LibErrorKind::Http,
            "YouTube Music answered with an error",
        ),
        (
            Script::Error(HttpError::Network),
            LibErrorKind::Network,
            "Could not reach YouTube Music",
        ),
        (
            Script::Error(HttpError::Timeout),
            LibErrorKind::Network,
            "Could not reach YouTube Music",
        ),
        (
            Script::Error(HttpError::TooLarge),
            LibErrorKind::Network,
            "Could not reach YouTube Music",
        ),
        (
            Script::Raw(200, b"<html>not json</html>".to_vec()),
            LibErrorKind::UnexpectedShape,
            "YouTube Music's answer was not readable",
        ),
        (
            Script::Json(json!({"unexpected": true})),
            LibErrorKind::UnexpectedShape,
            "YouTube Music's answer was not readable",
        ),
        (
            Script::Json(json!(null)),
            LibErrorKind::UnexpectedShape,
            "YouTube Music's answer was not readable",
        ),
    ];
    for (script, kind, message) in cases {
        rig.yt.script(script);
        let e = lib
            .playlist_items(&playlist_uri(PLAYLIST_ONE), 0)
            .await
            .unwrap_err();
        assert_eq!((e.kind, e.message.as_str()), (kind, message));
    }
    // Still working afterwards.
    assert!(
        lib.playlist_items(&playlist_uri(PLAYLIST_ONE), 0)
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn a_refused_login_asks_for_the_cookies_again_and_says_so_in_the_step() {
    let rig = logged_in().await;
    for status in [401, 403] {
        rig.yt.script(Script::Status(status));
        let e = lib(&rig).liked_tracks(0).await.unwrap_err();
        assert_eq!(e.kind, LibErrorKind::WebNotLoggedIn);
        assert_eq!(e.message, "Cookies expired: import them again");
        let status = rig.provider.auth().status().await.unwrap();
        assert_eq!(
            status.steps[1].state,
            LoginState::Error {
                message: "Cookies expired: import them again".into()
            }
        );
        // The next request that goes through clears it.
        lib(&rig).liked_tracks(0).await.unwrap();
        assert_eq!(
            rig.provider.auth().status().await.unwrap().steps[1].state,
            LoginState::LoggedIn
        );
    }
}

#[tokio::test]
async fn an_empty_library_is_an_empty_page_not_an_error() {
    let rig = logged_in().await;
    let empty_grid = json!({"contents": {"singleColumnBrowseResultsRenderer": {"tabs": [{"tabRenderer": {
        "content": {"sectionListRenderer": {"contents": [{"itemSectionRenderer": {"contents": [
            {"messageRenderer": {}}]}}]}}}}]}}});
    rig.yt.script(Script::Json(empty_grid.clone()));
    let p = lib(&rig).playlists(0).await.unwrap();
    assert!(p.items.is_empty() && p.next_offset.is_none());
    rig.yt.script(Script::Json(empty_grid));
    assert!(lib(&rig).albums(0).await.unwrap().items.is_empty());
    // A playlist with an empty shelf.
    rig.yt.script(Script::Json(
        json!({"contents": {"twoColumnBrowseResultsRenderer": {"secondaryContents": {
        "sectionListRenderer": {"contents": [{"musicPlaylistShelfRenderer": {}}]}}}}}),
    ));
    assert!(
        lib(&rig)
            .playlist_items(&playlist_uri(PLAYLIST_ONE), 0)
            .await
            .unwrap()
            .items
            .is_empty()
    );
    // Search with no results at all.
    rig.yt.script(Script::Json(json!({"header": {}})));
    assert!(lib(&rig).search("x", 0).await.unwrap().items.is_empty());
}

// ---- contract 5: fields filled when present, empty when absent ---------------------------------------------------

fn playlist_response(rows: Vec<Value>) -> Value {
    json!({"contents": {"twoColumnBrowseResultsRenderer": {"secondaryContents": {"sectionListRenderer": {
        "contents": [{"musicPlaylistShelfRenderer": {"contents": rows}}]}}}}})
}

fn remove(v: &mut Value, pointer: &str) {
    let (parent, key) = pointer.rsplit_once('/').unwrap();
    let parent = v.pointer_mut(parent).unwrap();
    match parent {
        Value::Object(m) => {
            m.remove(key);
        }
        Value::Array(a) => {
            a.remove(key.parse::<usize>().unwrap());
        }
        _ => panic!("no parent"),
    }
}

#[test]
fn a_track_is_filled_when_its_fields_are_there_and_empty_when_they_are_not() {
    let full = row(&SONGS[1], Layout::Playlist, true);
    let r = "/musicResponsiveListItemRenderer";
    let parse_one = |v: Value| -> Option<whipspot_core::provider::Track> {
        let page = parse::track_page(&playlist_response(vec![v]), None).unwrap();
        assert_eq!(page.raw, 1);
        page.items.into_iter().next().map(|r| r.track)
    };
    let t = parse_one(full.clone()).unwrap();
    assert_eq!(
        (
            t.name.as_str(),
            t.artist.as_str(),
            t.album.as_str(),
            t.duration_ms
        ),
        ("Paper Boats", "Artist A, Artist B", "First Record", 245_000)
    );

    // No duration column: 0. No artist column or no album column: empty.
    let mut v = full.clone();
    remove(&mut v, &format!("{r}/fixedColumns"));
    assert_eq!(parse_one(v).unwrap().duration_ms, 0);
    let mut v = full.clone();
    remove(&mut v, &format!("{r}/flexColumns/2"));
    let t = parse_one(v).unwrap();
    assert_eq!(
        (t.album.as_str(), t.artist.as_str()),
        ("", "Artist A, Artist B")
    );
    let mut v = full.clone();
    remove(&mut v, &format!("{r}/flexColumns/2"));
    remove(&mut v, &format!("{r}/flexColumns/1"));
    let t = parse_one(v).unwrap();
    assert_eq!(
        (t.artist.as_str(), t.album.as_str(), t.name.as_str()),
        ("", "", "Paper Boats")
    );
    // No title text: the track is kept with an empty name.
    let mut v = full.clone();
    remove(
        &mut v,
        &format!("{r}/flexColumns/0/musicResponsiveListItemFlexColumnRenderer/text/runs"),
    );
    let t = parse_one(v).unwrap();
    assert_eq!(
        (t.name.as_str(), t.artist.as_str()),
        ("", "Artist A, Artist B")
    );
    // No flex columns at all, and no thumbnail.
    let mut v = full.clone();
    remove(&mut v, &format!("{r}/flexColumns"));
    remove(&mut v, &format!("{r}/thumbnail"));
    let t = parse_one(v).unwrap();
    assert_eq!(
        (t.name.as_str(), t.artist.as_str(), t.album.as_str()),
        ("", "", "")
    );
    assert!(t.images.is_empty());
    // A duration that is not a time is 0.
    let mut v = full.clone();
    *v.pointer_mut(&format!(
        "{r}/fixedColumns/0/musicResponsiveListItemFixedColumnRenderer/text/runs/0/text"
    ))
    .unwrap() = json!("soon");
    assert_eq!(parse_one(v).unwrap().duration_ms, 0);

    // No video id: the row is skipped (and counted), not the page.
    let mut v = full.clone();
    remove(&mut v, &format!("{r}/overlay"));
    let page = parse::track_page(&playlist_response(vec![v, full.clone()]), None).unwrap();
    assert_eq!((page.items.len(), page.skipped, page.raw), (1, 1, 2));
    // A video id that is not an id is skipped too.
    let mut v = full.clone();
    *v.pointer_mut(&format!("{r}/overlay/musicItemThumbnailOverlayRenderer/content/musicPlayButtonRenderer/playNavigationEndpoint/watchEndpoint/videoId")).unwrap() = json!("a b/../c");
    let page = parse::track_page(&playlist_response(vec![v]), None).unwrap();
    assert_eq!((page.items.len(), page.skipped), (0, 1));
}

#[test]
fn the_id_of_a_row_without_a_play_button_comes_from_its_playlist_item_data() {
    let mut v = row(&SONGS[0], Layout::Playlist, false);
    let r = "/musicResponsiveListItemRenderer";
    remove(&mut v, &format!("{r}/overlay"));
    v.pointer_mut(r).unwrap()["playlistItemData"] = json!({"videoId": "trackAAAA01"});
    let page = parse::track_page(&playlist_response(vec![v]), None).unwrap();
    assert_eq!(page.items[0].track.uri, "ytmusic:track:trackAAAA01");
}

#[test]
fn text_is_cut_and_made_clean_and_a_row_count_is_capped() {
    let mut v = row(&SONGS[0], Layout::Playlist, false);
    let r = "/musicResponsiveListItemRenderer/flexColumns/0/musicResponsiveListItemFlexColumnRenderer/text/runs/0/text";
    *v.pointer_mut(r).unwrap() = json!(format!("A\u{202e}B\tC\n{}", "x".repeat(5000)));
    let page = parse::track_page(&playlist_response(vec![v]), None).unwrap();
    let name = &page.items[0].track.name;
    assert_eq!(name.chars().count(), MAX_TEXT);
    assert!(name.starts_with("AB C "), "{name:?}");
    assert!(!name.chars().any(|c| c.is_control() || c == '\u{202e}'));

    // Many artists: the joined text is cut too (the fuzz test found this).
    let mut v = row(&SONGS[0], Layout::Playlist, false);
    let runs: Vec<Value> = (0..40)
        .flat_map(|i| {
            [
                json!({"text": " & "}),
                json!({"text": format!("{i:02}{}", "n".repeat(100)),
                    "navigationEndpoint": {"browseEndpoint": {"browseId": format!("UCx{i}")}}}),
            ]
        })
        .collect();
    v["musicResponsiveListItemRenderer"]["flexColumns"][1]["musicResponsiveListItemFlexColumnRenderer"]
        ["text"]["runs"] = Value::Array(runs);
    let page = parse::track_page(&playlist_response(vec![v]), None).unwrap();
    assert_eq!(page.items[0].track.artist.chars().count(), MAX_TEXT);

    // 10,000 rows: at most 500 are read.
    let one = row(&SONGS[0], Layout::Playlist, false);
    let page = parse::track_page(&playlist_response(vec![one; 10_000]), None).unwrap();
    assert_eq!(
        (page.items.len(), page.raw, page.skipped),
        (MAX_ITEMS, MAX_ITEMS, 0)
    );
    // 10,000 entries that are not rows: skipped, 500 of them counted.
    let page = parse::track_page(&playlist_response(vec![json!({"x": 1}); 10_000]), None).unwrap();
    assert_eq!(
        (page.items.len(), page.raw, page.skipped),
        (0, MAX_ITEMS, MAX_ITEMS as u32)
    );
    // And a page that read nothing does not point at a next one.
    let mut items = vec![json!({"x": 1})];
    items.push(json!({"continuationItemRenderer": {"continuationEndpoint": {"continuationCommand": {"token": "abc"}}}}));
    assert!(
        parse::track_page(&playlist_response(items), None)
            .unwrap()
            .next
            .is_some()
    );
    let only_marker = vec![
        json!({"continuationItemRenderer": {"continuationEndpoint": {"continuationCommand": {"token": "abc"}}}}),
    ];
    assert!(
        parse::track_page(&playlist_response(only_marker), None)
            .unwrap()
            .next
            .is_none()
    );
}

#[tokio::test]
async fn a_ten_thousand_row_answer_gives_a_page_through_the_whole_stack() {
    let rig = logged_in().await;
    let one = row(&SONGS[0], Layout::Playlist, false);
    rig.yt
        .script(Script::Json(playlist_response(vec![one; 10_000])));
    let page = lib(&rig)
        .playlist_items(&playlist_uri(PLAYLIST_ONE), 0)
        .await
        .unwrap();
    assert_eq!(page.items.len(), MAX_ITEMS);
    // 100-deep nesting that a JSON reader accepts: a page; one it refuses: a fixed error.
    let mut v = row(&SONGS[0], Layout::Playlist, false);
    let mut deep = json!("x");
    for _ in 0..100 {
        deep = json!([deep]);
    }
    v["musicResponsiveListItemRenderer"]["flexColumns"][0] = deep;
    rig.yt.script(Script::Json(playlist_response(vec![v])));
    let page = lib(&rig)
        .playlist_items(&playlist_uri(PLAYLIST_ONE), 0)
        .await
        .unwrap();
    assert_eq!(page.items[0].name, "");
    let nested = format!("{}{}", "[".repeat(300), "]".repeat(300));
    rig.yt.script(Script::Raw(200, nested.into_bytes()));
    let e = lib(&rig)
        .playlist_items(&playlist_uri(PLAYLIST_ONE), 0)
        .await
        .unwrap_err();
    assert_eq!(e.kind, LibErrorKind::UnexpectedShape);
}

// ---- exploring: refs, artists, albums, saving ----------------------------------------------------------------

fn refs(items: &[EntityRef]) -> Vec<(&str, &str)> {
    items
        .iter()
        .map(|r| (r.uri.as_str(), r.name.as_str()))
        .collect()
}

#[tokio::test]
async fn the_provider_says_it_can_explore_and_save_albums() {
    let rig = Rig::new();
    let caps = rig.provider.info().caps;
    assert!(caps.explore && caps.save_albums && caps.likes);
}

#[tokio::test]
async fn tracks_name_their_artists_and_album_as_links() {
    let rig = logged_in().await;
    rig.yt.set_page_size(10);
    let lib = lib(&rig);
    // A playlist row: two linked artists and a linked album, in the order of the display text.
    let page = lib
        .playlist_items(&playlist_uri(PLAYLIST_ONE), 0)
        .await
        .unwrap();
    let t = &page.items[1];
    assert_eq!(t.artist, "Artist A, Artist B");
    assert_eq!(
        refs(&t.artists),
        [
            ("ytmusic:artist:UCartistA0000000000000001", "Artist A"),
            ("ytmusic:artist:UCartistB0000000000000002", "Artist B"),
        ]
    );
    assert_eq!(
        t.album_ref
            .as_ref()
            .map(|r| (r.uri.as_str(), r.name.as_str())),
        Some(("ytmusic:album:MPREb_other000001", "First Record"))
    );
    // A search row (one column of runs) too.
    let found = lib.search("night", 0).await.unwrap();
    assert_eq!(found.items[0].artists.len(), 1);
    assert_eq!(
        found.items[0].album_ref.as_ref().unwrap().uri,
        "ytmusic:album:MPREb_other000002"
    );
    // An album page's rows take both from the header.
    let tracks = lib.album_tracks(&album_uri(ALBUM), 0).await.unwrap();
    for t in &tracks.items {
        assert_eq!(
            refs(&t.artists),
            [("ytmusic:artist:UCalbumArtist00000000003", "Album Artist")]
        );
        assert_eq!(t.album_ref.as_ref().unwrap().uri, album_uri(ALBUM));
    }
    // The saved albums name theirs.
    let albums = all_albums(lib).await;
    assert_eq!(albums[0].uri, album_uri(ALBUM));
    assert_eq!(
        refs(&albums[0].artists),
        [("ytmusic:artist:UCalbumArtist00000000003", "Album Artist")]
    );
    assert_eq!(albums[0].kind, AlbumKind::Album);
}

#[tokio::test]
async fn an_artist_page_has_a_header_popular_songs_and_the_first_releases() {
    for size in [1, 2, 100] {
        let rig = logged_in().await;
        rig.yt.set_page_size(size);
        let lib = lib(&rig);
        let uri = artist_uri(ARTISTS[1].1);
        let page = lib.artist(&uri).await.unwrap();
        assert_eq!(
            (page.artist.uri.as_str(), page.artist.name.as_str()),
            (uri.as_str(), "Artist B")
        );
        assert_images_clean(&page.artist.images);
        assert!(!page.popular.is_empty() && page.popular.len() <= MAX_POPULAR);
        assert!(
            page.popular
                .iter()
                .all(|t| t.artists.iter().any(|a| a.uri == uri))
        );
        // The albums carousel shows two of the three; the "More" grid has all of them. Then the
        // singles.
        let names: Vec<&str> = page
            .releases
            .items
            .iter()
            .map(|a| a.name.as_str())
            .collect();
        assert_eq!(
            names,
            [
                "First Record",
                "Second Record",
                "Third Record",
                "Fourth Record"
            ],
            "page size {size}"
        );
        let kinds: Vec<AlbumKind> = page.releases.items.iter().map(|a| a.kind).collect();
        assert_eq!(
            kinds,
            [
                AlbumKind::Album,
                AlbumKind::Album,
                AlbumKind::Album,
                AlbumKind::Single
            ]
        );
        // Releases that name no artist belong to the page's own artist.
        for a in &page.releases.items {
            assert_eq!(refs(&a.artists), [(uri.as_str(), "Artist B")]);
            assert_eq!(a.artist, "Artist B");
            assert!(a.uri.starts_with("ytmusic:album:MPRE") && a.id == a.uri);
        }
        assert_eq!(page.releases.next_offset, None);
        assert_eq!(page.releases.total, Some(4));
    }
}

#[tokio::test]
async fn an_artists_releases_come_by_group_and_page() {
    let rig = logged_in().await;
    rig.yt.set_page_size(1);
    let lib = lib(&rig);
    let uri = artist_uri(ARTISTS[1].1);
    let name_of = |p: &whipspot_core::provider::AlbumPage| {
        p.items.iter().map(|a| a.name.clone()).collect::<Vec<_>>()
    };
    let albums = lib
        .artist_releases(&uri, ReleaseGroup::Albums, 0)
        .await
        .unwrap();
    assert_eq!(
        name_of(&albums),
        ["First Record", "Second Record", "Third Record"]
    );
    assert!(albums.items.iter().all(|a| a.kind == AlbumKind::Album));
    let singles = lib
        .artist_releases(&uri, ReleaseGroup::Singles, 0)
        .await
        .unwrap();
    assert_eq!(name_of(&singles), ["Fourth Record"]);
    assert!(singles.items.iter().all(|a| a.kind == AlbumKind::Single));
    // The YouTube Music page lists no guest releases.
    let guest = lib
        .artist_releases(&uri, ReleaseGroup::AppearsOn, 0)
        .await
        .unwrap();
    assert!(guest.items.is_empty() && guest.next_offset.is_none());
    // An offset past the end is an empty last page.
    let past = lib
        .artist_releases(&uri, ReleaseGroup::All, 10_000)
        .await
        .unwrap();
    assert!(past.items.is_empty() && past.next_offset.is_none());
    // The page is read once: the lists are kept.
    rig.yt.clear_posts();
    lib.artist(&uri).await.unwrap();
    lib.artist_releases(&uri, ReleaseGroup::All, 0)
        .await
        .unwrap();
    assert!(rig.yt.posts().is_empty(), "{:?}", rig.yt.posts());
}

#[tokio::test]
async fn the_more_grid_is_followed_by_its_continuations_and_asked_for_by_params() {
    let rig = logged_in().await;
    rig.yt.set_page_size(1);
    let lib = lib(&rig);
    rig.yt.clear_posts();
    lib.artist_releases(&artist_uri(ARTISTS[1].1), ReleaseGroup::Albums, 0)
        .await
        .unwrap();
    let browse = rig.yt.posts_to("browse");
    // The page, then the grid (3 albums at one a page: 3 requests).
    assert_eq!(
        browse.len(),
        4,
        "{:?}",
        browse.iter().map(|p| &p.url).collect::<Vec<_>>()
    );
    assert_eq!(browse[0].body["browseId"], ARTISTS[1].1);
    assert_eq!(browse[1].body["browseId"], format!("MPAD{}", ARTISTS[1].1));
    assert_eq!(browse[1].body["params"], "pAlbums");
    assert!(browse[2].url.contains("ctoken=rel."), "{}", browse[2].url);
}

#[tokio::test]
async fn a_more_grid_that_fails_leaves_the_carousel_and_is_asked_again_next_time() {
    let rig = logged_in().await;
    rig.yt.set_page_size(100);
    let lib = lib(&rig);
    let uri = artist_uri(ARTISTS[1].1);
    // The artist page answers, the grid does not.
    rig.yt.script_after(1, Script::Status(500));
    let page = lib
        .artist_releases(&uri, ReleaseGroup::Albums, 0)
        .await
        .unwrap();
    assert_eq!(
        page.items
            .iter()
            .map(|a| a.name.as_str())
            .collect::<Vec<_>>(),
        ["First Record", "Second Record"]
    );
    // Not kept: the next call reads the grid and has all three.
    let page = lib
        .artist_releases(&uri, ReleaseGroup::Albums, 0)
        .await
        .unwrap();
    assert_eq!(page.items.len(), 3);
}

#[tokio::test]
async fn an_album_page_names_its_artists_year_kind_and_tracks() {
    let rig = logged_in().await;
    rig.yt.set_page_size(2);
    let lib = lib(&rig);
    let d = lib.album(&album_uri("MPREb_other000004")).await.unwrap();
    assert_eq!(d.album.uri, "ytmusic:album:MPREb_other000004");
    assert_eq!(d.album.id, d.album.uri);
    assert_eq!(
        (
            d.album.name.as_str(),
            d.album.artist.as_str(),
            d.album.year,
            d.album.kind
        ),
        ("Fourth Record", "Artist B", Some(2023), AlbumKind::Single)
    );
    assert_eq!(
        refs(&d.album.artists),
        [("ytmusic:artist:UCartistB0000000000000002", "Artist B")]
    );
    assert_images_clean(&d.album.images);
    assert_eq!(names(&d.tracks.items), ["Liked Only"]);
    assert_eq!(
        d.tracks.items[0].album_ref.as_ref().unwrap().uri,
        d.album.uri
    );
    // Its tracks continue by offset as before.
    let big = lib.album(&album_uri(ALBUM)).await.unwrap();
    assert_eq!(big.tracks.items.len(), 2);
    let rest = lib
        .album_tracks(&album_uri(ALBUM), big.tracks.next_offset.unwrap())
        .await
        .unwrap();
    assert_eq!(names(&rest.items), ["Album Closer"]);
    assert_eq!(big.album.year, Some(2019));
}

#[tokio::test]
async fn a_bad_artist_or_album_uri_never_reaches_the_service() {
    let rig = logged_in().await;
    let lib = lib(&rig);
    rig.yt.clear_posts();
    let long = format!("ytmusic:artist:UC{}", "9".repeat(5000));
    for bad in [
        "",
        "no-such-id",
        "spotify:artist:abc",
        "ytmusic:artist:",
        "ytmusic:artist:abc",
        "ytmusic:artist:UC a",
        "ytmusic:artist:UC../x",
        "ytmusic:album:",
        "ytmusic:album:abc",
        "ytmusic:album:MPRE/x",
        "ytmusic:track:trackAAAA01",
        "ytmusic:artist:MPREb_other000001",
        "ytmusic:album:UCartistA0000000000000001",
        long.as_str(),
    ] {
        for e in [
            lib.artist(bad).await.unwrap_err(),
            lib.album(bad).await.unwrap_err(),
            lib.artist_releases(bad, ReleaseGroup::All, 0)
                .await
                .unwrap_err(),
        ] {
            assert_eq!(e.kind, LibErrorKind::InvalidArgument, "{bad:?}");
        }
    }
    assert!(rig.yt.posts().is_empty());
    // A valid id the service does not know is the service's error.
    let e = lib
        .artist(&artist_uri("UCunknown00000000000000"))
        .await
        .unwrap_err();
    assert_eq!(e.kind, LibErrorKind::Http);
}

#[tokio::test]
async fn an_album_is_saved_through_its_audio_playlist_and_reads_back() {
    let rig = logged_in().await;
    rig.yt.set_page_size(100);
    let lib = lib(&rig);
    let uri = album_uri("MPREb_other000002");
    // Unseen: read from the album's own page.
    rig.yt.clear_posts();
    assert_eq!(
        lib.liked(std::slice::from_ref(&uri)).await.unwrap(),
        vec![false]
    );
    assert_eq!(rig.yt.posts_to("browse").len(), 1);
    // Seen: kept, no request.
    lib.liked(std::slice::from_ref(&uri)).await.unwrap();
    assert_eq!(rig.yt.posts_to("browse").len(), 1);

    lib.set_liked(&uri, true).await.unwrap();
    let post = rig.yt.posts_to("like/like").pop().unwrap();
    assert_eq!(
        post.body["target"],
        json!({"playlistId": "OLAK5uy_other000002"})
    );
    assert_eq!(rig.yt.saved_albums(), ["OLAK5uy_other000002"]);
    assert_eq!(
        lib.liked(std::slice::from_ref(&uri)).await.unwrap(),
        vec![true]
    );
    // The service says so too, when the page is read again.
    let again = rig.yt.posts_to("browse").len();
    lib.album(&uri).await.unwrap();
    assert_eq!(rig.yt.posts_to("browse").len(), again + 1);
    assert_eq!(
        lib.liked(std::slice::from_ref(&uri)).await.unwrap(),
        vec![true]
    );

    lib.set_liked(&uri, false).await.unwrap();
    assert_eq!(rig.yt.posts_to("like/removelike").len(), 1);
    assert!(rig.yt.saved_albums().is_empty());
    assert_eq!(
        lib.liked(std::slice::from_ref(&uri)).await.unwrap(),
        vec![false]
    );
    // A track and an album in one call: one answer each, in order.
    let track = track_uri("trackAAAA10");
    lib.liked_tracks(0).await.unwrap();
    assert_eq!(
        lib.liked(&[track, uri.clone(), "x".into()]).await.unwrap(),
        vec![true, false, false]
    );
}

#[tokio::test]
async fn an_album_whose_page_names_no_audio_playlist_cannot_be_saved() {
    let rig = logged_in().await;
    let lib = lib(&rig);
    let uri = album_uri("MPREb_other000003");
    rig.yt.clear_posts();
    let e = lib.set_liked(&uri, true).await.unwrap_err();
    assert_eq!(e.kind, LibErrorKind::InvalidArgument);
    assert_eq!(e.message, "This album cannot be saved");
    assert!(rig.yt.posts_to("like/like").is_empty());
    assert!(rig.yt.saved_albums().is_empty());
    // It reads as not saved, and still opens.
    assert_eq!(
        lib.liked(std::slice::from_ref(&uri)).await.unwrap(),
        vec![false]
    );
    lib.album(&uri).await.unwrap();
    // Only an album's own audio playlist is a target: a page that names another kind is not used.
    let mut page = json!({
        "title": {"runs": [{"text": "T"}]},
        "buttons": [{"musicPlayButtonRenderer": {"playNavigationEndpoint": {
            "watchEndpoint": {"playlistId": "RDAMPLxyz"}}}}],
    });
    let wrap = |h: &Value| {
        json!({"contents": {"twoColumnBrowseResultsRenderer": {"tabs": [{
        "tabRenderer": {"content": {"sectionListRenderer": {"contents": [
            {"musicResponsiveHeaderRenderer": h}]}}}}]}}})
    };
    assert_eq!(
        parse::album_header(&wrap(&page)).unwrap().audio_playlist_id,
        None
    );
    page["buttons"][0]["musicPlayButtonRenderer"]["playNavigationEndpoint"]["watchEndpoint"]["playlistId"] =
        json!("OLAK5uy_ok");
    assert_eq!(
        parse::album_header(&wrap(&page)).unwrap().audio_playlist_id,
        Some("OLAK5uy_ok".to_owned())
    );
}

#[tokio::test]
async fn the_albums_a_liked_call_probes_are_bounded() {
    let rig = logged_in().await;
    let lib = lib(&rig);
    let uris: Vec<String> = (0..30)
        .map(|i| album_uri(&format!("MPREb_nothere{i:02}")))
        .collect();
    // Each unseen album is one page; the service refuses these, so the first is the error.
    rig.yt.clear_posts();
    assert!(lib.liked(&uris).await.is_err());
    assert_eq!(rig.yt.posts().len(), 1);
}

#[test]
fn only_uc_ids_are_artists_and_only_mpre_ids_are_albums() {
    assert_eq!(
        ids::artist_uri("UCabc"),
        Some("ytmusic:artist:UCabc".to_owned())
    );
    assert_eq!(ids::parse_artist_uri("ytmusic:artist:UCabc"), Some("UCabc"));
    for bad in [
        "",
        "abc",
        "MPREb_x",
        "UC a",
        "UC/x",
        &format!("UC{}", "a".repeat(64)),
    ] {
        assert!(ids::artist_uri(bad).is_none(), "{bad:?}");
    }
    for bad in [
        "ytmusic:artist:",
        "ytmusic:artist:abc",
        "ytmusic:album:UCabc",
        "x",
    ] {
        assert!(ids::parse_artist_uri(bad).is_none(), "{bad:?}");
    }
    assert_eq!(
        ids::parse_album_uri("ytmusic:album:MPREb_x"),
        Some("MPREb_x")
    );
    for bad in [
        "ytmusic:album:abc",
        "ytmusic:album:UCabc",
        "ytmusic:album:MPRE/x",
    ] {
        assert!(ids::parse_album_uri(bad).is_none(), "{bad:?}");
    }
    assert!(ids::album_uri("abc").is_none());
    assert_eq!(ids::audio_playlist_id("OLAK5uy_abc"), Some("OLAK5uy_abc"));
    for bad in ["", "PLabc", "RDAMPL1", "OLAK5uy_a b", "LM"] {
        assert!(ids::audio_playlist_id(bad).is_none(), "{bad:?}");
    }
}

fn linked(text: &str, browse_id: &str, page_type: Option<&str>) -> Value {
    let mut ep = json!({"browseId": browse_id});
    if let Some(t) = page_type {
        ep["browseEndpointContextSupportedConfigs"] =
            json!({"browseEndpointContextMusicConfig": {"pageType": t}});
    }
    json!({"text": text, "navigationEndpoint": {"browseEndpoint": ep}})
}

fn row_with(runs: Vec<Value>) -> Value {
    let mut v = row(&SONGS[0], Layout::Playlist, false);
    v["musicResponsiveListItemRenderer"]["flexColumns"][1]["musicResponsiveListItemFlexColumnRenderer"]
        ["text"]["runs"] = Value::Array(runs);
    v
}

#[test]
fn a_run_is_a_link_only_for_the_kind_of_id_it_names() {
    let parse_one = |v: Value| {
        parse::track_page(&playlist_response(vec![v]), None)
            .unwrap()
            .items
            .remove(0)
            .track
    };
    let t = parse_one(row_with(vec![
        linked("Good", "UCgood", Some("MUSIC_PAGE_TYPE_ARTIST")),
        json!({"text": " & "}),
        // A channel of a user, a video's uploader, a made-up page, a bad id, an album id: shown
        // in the text, never a link.
        linked("Chan", "UCchan", Some("MUSIC_PAGE_TYPE_USER_CHANNEL")),
        linked("Odd", "UCodd", Some("MUSIC_PAGE_TYPE_PLAYLIST")),
        linked("Bad id", "UC a/b", Some("MUSIC_PAGE_TYPE_ARTIST")),
        linked("Album id", "MPREb_x", Some("MUSIC_PAGE_TYPE_ARTIST")),
        linked("", "UCnoname", Some("MUSIC_PAGE_TYPE_ARTIST")),
        // No page type stated: a UC id is an artist.
        linked("Plain", "UCplain", None),
        // The same artist twice is one link.
        linked("Good", "UCgood", Some("MUSIC_PAGE_TYPE_ARTIST")),
    ]));
    assert_eq!(
        refs(&t.artists),
        [
            ("ytmusic:artist:UCgood", "Good"),
            ("ytmusic:artist:UCplain", "Plain")
        ]
    );
    // At most MAX_ARTISTS links; the display text keeps the rest.
    let runs: Vec<Value> = (0..20)
        .map(|i| linked(&format!("A{i}"), &format!("UC{i}"), None))
        .collect();
    let t = parse_one(row_with(runs));
    assert_eq!(t.artists.len(), MAX_ARTISTS);
    assert!(t.artist.contains("A19"));
    // An album run: only an MPRE id on an album page.
    let mut v = row(&SONGS[0], Layout::Playlist, false);
    v["musicResponsiveListItemRenderer"]["flexColumns"][2]["musicResponsiveListItemFlexColumnRenderer"]
        ["text"]["runs"] = json!([linked("Not an album", "UCx", Some("MUSIC_PAGE_TYPE_ALBUM"))]);
    assert!(parse_one(v).album_ref.is_none());
}

fn artist_response(sections: Vec<Value>) -> Value {
    json!({
        "header": {"musicImmersiveHeaderRenderer": {"title": {"runs": [{"text": "Name"}]}}},
        "contents": {"singleColumnBrowseResultsRenderer": {"tabs": [{"tabRenderer": {
            "content": {"sectionListRenderer": {"contents": sections}}}}]}},
    })
}

fn carousel(title: &str, more: Option<(&str, &str)>, items: Vec<Value>) -> Value {
    let mut run = json!({"text": title});
    if let Some((id, params)) = more {
        run["navigationEndpoint"] = json!({"browseEndpoint": {"browseId": id, "params": params}});
    }
    json!({"musicCarouselShelfRenderer": {
        "header": {"musicCarouselShelfBasicHeaderRenderer": {"title": {"runs": [run]}}},
        "contents": items}})
}

#[test]
fn an_artist_page_is_read_by_what_it_has_and_not_by_what_it_lacks() {
    let uri = "ytmusic:artist:UCme";
    // No header with a name: not an artist page.
    assert!(parse::artist_page(&json!({}), uri).is_none());
    assert!(
        parse::artist_page(
            &json!({"header": {"musicImmersiveHeaderRenderer": {}}}),
            uri
        )
        .is_none()
    );
    // A header alone is an artist with nothing else.
    let p = parse::artist_page(&artist_response(vec![]), uri).unwrap();
    assert_eq!(p.name, "Name");
    assert!(p.popular.is_empty() && p.albums.items.is_empty() && p.singles.items.is_empty());
    // The older header shape works too, and the foreground picture is its portrait.
    let visual = json!({"header": {"musicVisualHeaderRenderer": {
        "title": {"runs": [{"text": "V"}]},
        "foregroundThumbnail": {"musicThumbnailRenderer": {"thumbnail": {"thumbnails": [
            {"url": "https://lh3.googleusercontent.com/x=w226", "width": 226, "height": 226}]}}}}}});
    let p = parse::artist_page(&visual, uri).unwrap();
    assert_eq!((p.name.as_str(), p.images.len()), ("V", 1));

    // The "More" endpoint is an MPAD id with params, and nothing else.
    let item = row_release("Rel", "MPREb_r");
    let with = |more| {
        parse::artist_page(
            &artist_response(vec![carousel("Albums", more, vec![item.clone()])]),
            uri,
        )
        .unwrap()
        .albums
    };
    let shelf = with(Some(("MPADUCme", "ggMI%3D")));
    assert_eq!(shelf.items.len(), 1);
    assert_eq!(
        shelf.more,
        Some(parse::More {
            browse_id: "MPADUCme".into(),
            params: "ggMI%3D".into()
        })
    );
    for bad in [
        ("FEmusic_home", "ggMI"),
        ("MPAD a", "ggMI"),
        ("MPADUCme", "has space"),
        ("MPADUCme", "a&b"),
        ("MPADUCme", ""),
    ] {
        assert!(with(Some(bad)).more.is_none(), "{bad:?}");
    }
    assert!(with(None).more.is_none());
    // A release that is not an MPRE album is dropped, an unknown carousel is ignored, and only the
    // first carousel of a title counts.
    let p = parse::artist_page(
        &artist_response(vec![
            carousel(
                "Albums",
                None,
                vec![
                    row_release("Real", "MPREb_a"),
                    row_release("Fake", "UCx"),
                    json!(5),
                ],
            ),
            carousel("Albums", None, vec![row_release("Second", "MPREb_b")]),
            carousel("Videos", None, vec![row_release("Video", "MPREb_v")]),
            carousel("Singles & EPs", None, vec![row_release("One", "MPREb_s")]),
        ]),
        uri,
    )
    .unwrap();
    assert_eq!(
        p.albums
            .items
            .iter()
            .map(|a| a.name.as_str())
            .collect::<Vec<_>>(),
        ["Real"]
    );
    assert_eq!(p.singles.items[0].kind, AlbumKind::Single);
    assert_eq!(p.albums.items[0].artists[0].uri, uri);
    // Popular songs: at most MAX_POPULAR, and a song that names no artist gets the page's.
    let many: Vec<Value> = (0..30)
        .map(|_| row(&SONGS[0], Layout::Album, false))
        .collect();
    let p = parse::artist_page(
        &artist_response(vec![json!({"musicShelfRenderer": {"contents": many}})]),
        uri,
    )
    .unwrap();
    assert_eq!(p.popular.len(), MAX_POPULAR);
    assert_eq!(p.popular[0].artist, "Name");
    assert_eq!(refs(&p.popular[0].artists), [(uri, "Name")]);
}

fn row_release(title: &str, id: &str) -> Value {
    json!({"musicTwoRowItemRenderer": {
        "title": {"runs": [{"text": title, "navigationEndpoint": {"browseEndpoint": {"browseId": id}}}]},
        "subtitle": {"runs": [{"text": "Album"}, {"text": " • "}, {"text": "2020"}]},
    }})
}

// ---- contract 2: fuzz ---------------------------------------------------------------------------------------------------

fn junk(rng: &mut SplitMix64) -> Value {
    match rng.below(12) {
        0 => Value::Null,
        1 => json!(true),
        2 => json!(-1),
        3 => json!(1e30),
        4 => json!(u64::MAX),
        5 => json!(""),
        6 => json!("x".repeat(3000)),
        7 => json!("\u{202e}odd\n\t\u{0}"),
        8 => {
            let mut v = json!(1);
            for _ in 0..100 {
                v = json!([v]);
            }
            v
        }
        9 => json!({"text": 5, "runs": "no"}),
        10 => json!([]),
        _ => json!({}),
    }
}

fn mutate_once(v: &mut Value, rng: &mut SplitMix64, depth: usize) {
    let descend = depth < 40 && !rng.one_in(4);
    match v {
        Value::Object(m) if !m.is_empty() => {
            let keys: Vec<String> = m.keys().cloned().collect();
            let k = rng.pick(&keys).clone();
            if descend {
                if let Some(child) = m.get_mut(&k) {
                    mutate_once(child, rng, depth + 1);
                }
            } else {
                match rng.below(3) {
                    0 => {
                        m.remove(&k);
                    }
                    1 => {
                        m.insert(k, junk(rng));
                    }
                    _ => {
                        m.insert(format!("{k}x"), junk(rng));
                    }
                }
            }
        }
        Value::Array(a) if !a.is_empty() => {
            let i = rng.below(a.len());
            if descend {
                mutate_once(&mut a[i], rng, depth + 1);
            } else {
                match rng.below(4) {
                    0 => {
                        a.remove(i);
                    }
                    1 => a[i] = junk(rng),
                    2 => {
                        let c = a[i].clone();
                        a.push(c);
                    }
                    _ => a.clear(),
                }
            }
        }
        _ => *v = junk(rng),
    }
}

fn check_tracks(page: &Page<TrackRow>) {
    assert!(page.items.len() <= MAX_ITEMS && page.raw <= MAX_ITEMS);
    assert!(page.items.len() + page.skipped as usize <= page.raw);
    for r in &page.items {
        let t = &r.track;
        assert!(ids::parse_track_uri(&t.uri).is_some(), "{}", t.uri);
        for s in [&t.name, &t.artist, &t.album] {
            assert!(s.chars().count() <= MAX_TEXT && !s.chars().any(char::is_control));
        }
        assert!(t.images.len() <= MAX_IMAGES);
        assert!(t.artists.len() <= MAX_ARTISTS);
        for a in &t.artists {
            assert!(ids::parse_artist_uri(&a.uri).is_some());
            assert!(!a.name.is_empty() && a.name.chars().count() <= MAX_TEXT);
        }
        if let Some(a) = &t.album_ref {
            assert!(ids::parse_album_uri(&a.uri).is_some() && !a.name.is_empty());
        }
        for i in &t.images {
            assert!(i.url.starts_with("https://"));
            assert!(
                IMAGE_HOSTS
                    .iter()
                    .any(|h| reqwest_host(&i.url).ends_with(h))
            );
        }
    }
}

fn run_parsers(v: &Value) {
    let header = AlbumHeader::default();
    for page in [
        parse::track_page(v, None),
        parse::track_page(v, Some(&header)),
        parse::search_page(v),
    ]
    .into_iter()
    .flatten()
    {
        check_tracks(&page);
    }
    if let Some(p) = parse::playlists_page(v) {
        assert!(p.items.len() <= MAX_ITEMS);
        for i in &p.items {
            assert!(ids::parse_playlist_uri(&i.id).is_some());
            assert!(i.name.chars().count() <= MAX_TEXT && i.images.len() <= MAX_IMAGES);
        }
    }
    if let Some(p) = parse::albums_page(v) {
        assert!(p.items.len() <= MAX_ITEMS);
        for i in &p.items {
            assert!(ids::parse_album_uri(&i.id).is_some());
            assert!(i.name.chars().count() <= MAX_TEXT && i.artist.chars().count() <= MAX_TEXT);
        }
        check_albums(&p.items);
    }
    if let Some(h) = parse::album_header(v) {
        assert!(h.title.chars().count() <= MAX_TEXT && h.images.len() <= MAX_IMAGES);
        assert!(h.artists.len() <= MAX_ARTISTS);
        for a in &h.artists {
            assert!(ids::parse_artist_uri(&a.uri).is_some());
        }
        for id in h.audio_playlist_id.iter() {
            assert!(ids::audio_playlist_id(id).is_some());
        }
    }
    let owner = EntityRef {
        uri: "ytmusic:artist:UCme".into(),
        name: "Me".into(),
    };
    if let Some(a) = parse::artist_page(v, &owner.uri) {
        assert!(a.name.chars().count() <= MAX_TEXT && a.images.len() <= MAX_IMAGES);
        assert!(a.popular.len() <= MAX_POPULAR);
        for t in &a.popular {
            assert!(t.artists.len() <= MAX_ARTISTS);
        }
        for shelf in [&a.albums, &a.singles] {
            assert!(shelf.items.len() <= MAX_ITEMS);
            check_albums(&shelf.items);
            if let Some(m) = &shelf.more {
                assert!(m.browse_id.starts_with("MPAD") && ids::valid_id(&m.browse_id));
            }
        }
    }
    for group in [ReleaseGroup::Albums, ReleaseGroup::Singles] {
        if let Some(p) = parse::releases_page(v, group, &owner) {
            assert!(p.items.len() <= MAX_ITEMS);
            check_albums(&p.items);
        }
    }
}

fn check_albums(items: &[Album]) {
    for a in items {
        assert!(ids::parse_album_uri(&a.uri).is_some() && a.id == a.uri);
        assert!(a.name.chars().count() <= MAX_TEXT && a.images.len() <= MAX_IMAGES);
        assert!(a.artists.len() <= MAX_ARTISTS);
        for r in &a.artists {
            assert!(ids::parse_artist_uri(&r.uri).is_some());
            assert!(!r.name.is_empty() && r.name.chars().count() <= MAX_TEXT);
        }
    }
}

fn bases() -> Vec<Value> {
    let yt = FakeYt::new();
    yt.set_page_size(2);
    let headers = [
        ("Authorization", "SAPISIDHASH 1_x"),
        ("Cookie", "SAPISID=x"),
    ];
    let url = "https://music.youtube.com/youtubei/v1/";
    let ask = |endpoint: &str, body: Value, query: &str| -> Value {
        let r = yt
            .post_json(
                &format!("{url}{endpoint}?alt=json{query}"),
                &headers,
                &serde_json::to_vec(&body).unwrap(),
                Cap::INNERTUBE,
            )
            .unwrap();
        serde_json::from_slice(&r.body).unwrap()
    };
    vec![
        ask("browse", json!({"browseId": "FEmusic_liked_playlists"}), ""),
        ask(
            "browse",
            json!({"browseId": "FEmusic_liked_playlists"}),
            "&ctoken=pls.x.1&continuation=pls.x.1",
        ),
        ask("browse", json!({"browseId": "FEmusic_liked_albums"}), ""),
        ask("browse", json!({"browseId": "VLLM"}), ""),
        ask("browse", json!({"continuation": "liked.x.2"}), ""),
        ask(
            "browse",
            json!({"browseId": format!("VL{PLAYLIST_ONE}")}),
            "",
        ),
        ask("browse", json!({"browseId": ALBUM}), ""),
        ask("browse", json!({"browseId": ARTISTS[1].1}), ""),
        ask("browse", json!({"browseId": ARTISTS[2].1}), ""),
        ask(
            "browse",
            json!({"browseId": format!("MPAD{}", ARTISTS[1].1), "params": "pAlbums"}),
            "",
        ),
        ask("search", json!({"query": "night"}), ""),
        ask(
            "search",
            json!({"query": "night"}),
            "&ctoken=s.6e69676874.1",
        ),
    ]
}

#[test]
fn the_parsers_survive_every_mutated_answer() {
    let bases = bases();
    for b in &bases {
        run_parsers(b);
    }
    let dict: [&[u8]; 6] = [
        b"null",
        b"\"",
        b"[",
        b"{",
        b"musicResponsiveListItemRenderer",
        b"runs",
    ];
    fuzz("ytmusic parsers", 0x594D_4C42, 6_000, |case: &mut Case| {
        let mut v = case.rng.pick(&bases).clone();
        for _ in 0..=case.rng.below(3) {
            mutate_once(&mut v, &mut case.rng, 0);
        }
        let mut bytes = serde_json::to_vec(&v).unwrap();
        case.set_input_len(bytes.len());
        run_parsers(&v);
        // And the bytes of the answer cut or flipped: if a JSON reader still takes them, so do
        // the parsers.
        mutate(&mut bytes, &mut case.rng, &dict);
        if let Ok(v) = serde_json::from_slice::<Value>(&bytes) {
            run_parsers(&v);
        }
    });
}

// ---- no secret in a message -----------------------------------------------------------------------------------------------

#[tokio::test]
async fn no_library_error_carries_a_cookie_or_a_url() {
    let rig = logged_in().await;
    let lib = lib(&rig);
    let mut errors = Vec::new();
    for script in [
        Script::Status(401),
        Script::Status(500),
        Script::Status(429),
        Script::Error(HttpError::Network),
        Script::Raw(
            200,
            format!("{SENTINEL_COOKIE} {SENTINEL_SAPISID}").into_bytes(),
        ),
        Script::Json(json!({"error": SENTINEL_COOKIE})),
    ] {
        rig.yt.script(script);
        errors.push(lib.playlists(0).await.unwrap_err());
    }
    errors.push(lib.playlist_items(SENTINEL_COOKIE, 0).await.unwrap_err());
    errors.push(lib.set_liked(SENTINEL_SAPISID, true).await.unwrap_err());
    for e in errors {
        let text = format!("{e} {e:?} {}", serde_json::to_string(&e).unwrap());
        for bad in [
            SENTINEL_COOKIE,
            SENTINEL_SAPISID,
            "SAPISIDHASH",
            "://",
            "music.youtube.com",
        ] {
            assert!(!text.contains(bad), "{bad} in {text}");
        }
    }
    let _: Arc<dyn Provider> = rig.provider.clone();
}
