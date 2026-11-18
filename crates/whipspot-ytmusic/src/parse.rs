//! Reads InnerTube answers into the core's library models.
//!
//! The answers are untrusted JSON of a shape nobody promised: the paths below are the ones the
//! MIT-licensed ytmusicapi reads (its `navigation.py` and parsers), and each one is optional. The
//! rules are:
//!
//! - A field that is missing or has the wrong type skips that item (`skipped` counts them), never
//!   the page. A track with no title, artist, album or duration is still a track: those are empty
//!   or zero. A track with no valid video id is skipped.
//! - At most [`MAX_ITEMS`] items of a page are looked at, every string is cut to [`MAX_TEXT`]
//!   characters and made display-clean, every id is validated, and a thumbnail is kept only on an
//!   allowed image host.
//! - Nothing here panics on any `serde_json::Value`: no indexing, no unwrap, no recursion of its
//!   own (serde_json itself refuses input nested deeper than 128).

use serde_json::Value;
use whipspot_core::provider::{
    Album, AlbumKind, EntityRef, ImageRef, MAX_ARTISTS, MAX_IMAGES, MAX_POPULAR, Playlist,
    Readable, ReleaseGroup, Track,
};
use whipspot_core::text::display_text;

use crate::ids;
use crate::innertube::token_ok;

/// The most items of one page that are read.
pub const MAX_ITEMS: usize = 500;
/// The longest string kept, in characters.
pub const MAX_TEXT: usize = 1024;
/// The hosts a thumbnail may be on (and nothing else is ever fetched for art).
pub const IMAGE_HOSTS: [&str; 3] = ["lh3.googleusercontent.com", "i.ytimg.com", "yt3.ggpht.com"];

/// How the next page of a list is asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Continuation {
    /// The newer kind: a `continuationItemRenderer` ends the list; the token goes in the body.
    Next(String),
    /// The older kind: the token goes in the URL (`ctoken` and `continuation`).
    Legacy(String),
}

/// One page of parsed items.
#[derive(Debug)]
pub struct Page<T> {
    pub items: Vec<T>,
    /// Entries that were looked at and dropped.
    pub skipped: u32,
    /// How many entries were looked at (kept and dropped): the step of the next offset.
    pub raw: usize,
    pub next: Option<Continuation>,
}

impl<T> Page<T> {
    fn empty() -> Page<T> {
        Page {
            items: Vec::new(),
            skipped: 0,
            raw: 0,
            next: None,
        }
    }
}

/// A parsed track and what its `likeStatus` said.
#[derive(Debug)]
pub struct TrackRow {
    pub track: Track,
    pub like: Option<bool>,
}

/// What an album page's header says about every track of it.
#[derive(Debug, Clone, Default)]
pub struct AlbumHeader {
    pub title: String,
    pub artist: String,
    pub year: Option<u16>,
    pub images: Vec<ImageRef>,
    /// The artists as links, at most [`MAX_ARTISTS`].
    pub artists: Vec<EntityRef>,
    pub kind: AlbumKind,
    /// The album's own URI. The page does not name its own browse id: the caller that asked for
    /// it sets this.
    pub uri: String,
    /// The audio playlist a save targets (`OLAK5uy_..`); `None` if the page names none.
    pub audio_playlist_id: Option<String>,
    /// Whether the account has saved the album, when the page says.
    pub saved: Option<bool>,
}

impl AlbumHeader {
    /// The album as a link, once its URI is known.
    pub fn album_ref(&self) -> Option<EntityRef> {
        (!self.uri.is_empty() && !self.title.is_empty()).then(|| EntityRef {
            uri: self.uri.clone(),
            name: self.title.clone(),
        })
    }
}

/// Where the rest of an artist's releases of one group is listed (a "More" link).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct More {
    pub browse_id: String,
    pub params: String,
}

/// The releases an artist page shows in one carousel, and where the full list is.
#[derive(Debug, Clone, Default)]
pub struct ReleaseShelf {
    pub items: Vec<Album>,
    pub more: Option<More>,
}

/// An artist page: the header, the popular songs and the two release carousels.
#[derive(Debug, Clone)]
pub struct ArtistParse {
    pub name: String,
    pub images: Vec<ImageRef>,
    pub popular: Vec<Track>,
    pub albums: ReleaseShelf,
    pub singles: ReleaseShelf,
}

// ---- navigation ---------------------------------------------------------------------------

/// The value at a dotted path: a segment of digits indexes an array, any other names a key.
fn get<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = v;
    for seg in path.split('.') {
        cur = match seg.parse::<usize>() {
            Ok(i) => cur.get(i)?,
            Err(_) => cur.get(seg)?,
        };
    }
    Some(cur)
}

fn get_str<'a>(v: &'a Value, path: &str) -> Option<&'a str> {
    get(v, path)?.as_str()
}

fn get_array<'a>(v: &'a Value, path: &str) -> Option<&'a [Value]> {
    get(v, path)?.as_array().map(Vec::as_slice)
}

fn clean(s: &str) -> String {
    display_text(s, MAX_TEXT)
}

const PAGE_TYPE: &str = "navigationEndpoint.browseEndpoint.browseEndpointContextSupportedConfigs.browseEndpointContextMusicConfig.pageType";
const TWO_COLUMN: &str = "contents.twoColumnBrowseResultsRenderer";
const SINGLE_COLUMN_TAB: &str = "contents.singleColumnBrowseResultsRenderer.tabs";

// ---- continuations and shelves ----------------------------------------------------------

/// The token of a `continuationItemRenderer` that ends `items`.
fn new_style_token(items: &[Value]) -> Option<String> {
    let last = items.last()?;
    let direct = get_str(
        last,
        "continuationItemRenderer.continuationEndpoint.continuationCommand.token",
    );
    let from_commands = || {
        get_array(
            last,
            "continuationItemRenderer.continuationEndpoint.commandExecutorCommand.commands",
        )?
        .iter()
        .find(|c| {
            get_str(c, "continuationCommand.request") == Some("CONTINUATION_REQUEST_TYPE_BROWSE")
        })
        .and_then(|c| get_str(c, "continuationCommand.token"))
    };
    direct
        .or_else(from_commands)
        .filter(|t| token_ok(t))
        .map(str::to_owned)
}

fn legacy_token(shelf: &Value) -> Option<String> {
    get_str(shelf, "continuations.0.nextContinuationData.continuation")
        .filter(|t| token_ok(t))
        .map(str::to_owned)
}

struct Shelf<'a> {
    items: &'a [Value],
    next: Option<Continuation>,
}

impl<'a> Shelf<'a> {
    /// The items under `key` of a shelf object, and its continuation of either kind.
    fn of(shelf: &'a Value, key: &str) -> Shelf<'a> {
        let items = get_array(shelf, key).unwrap_or(&[]);
        let next = new_style_token(items)
            .map(Continuation::Next)
            .or_else(|| legacy_token(shelf).map(Continuation::Legacy));
        Shelf { items, next }
    }

    /// The items of a continuation response of the newer kind.
    fn appended(items: &'a [Value]) -> Shelf<'a> {
        Shelf {
            items,
            next: new_style_token(items).map(Continuation::Next),
        }
    }
}

/// The shelf of tracks of a playlist, album, liked-songs or continuation response.
fn find_track_shelf(resp: &Value) -> Option<Shelf<'_>> {
    let first_pages = [
        format!(
            "{TWO_COLUMN}.secondaryContents.sectionListRenderer.contents.0.musicPlaylistShelfRenderer"
        ),
        format!("{TWO_COLUMN}.secondaryContents.sectionListRenderer.contents.0.musicShelfRenderer"),
        format!(
            "{SINGLE_COLUMN_TAB}.0.tabRenderer.content.sectionListRenderer.contents.0.musicPlaylistShelfRenderer"
        ),
        format!(
            "{SINGLE_COLUMN_TAB}.0.tabRenderer.content.sectionListRenderer.contents.0.musicShelfRenderer"
        ),
    ];
    for path in &first_pages {
        if let Some(shelf) = get(resp, path) {
            return Some(Shelf::of(shelf, "contents"));
        }
    }
    if let Some(items) = get_array(
        resp,
        "onResponseReceivedActions.0.appendContinuationItemsAction.continuationItems",
    ) {
        return Some(Shelf::appended(items));
    }
    for key in ["musicPlaylistShelfContinuation", "musicShelfContinuation"] {
        if let Some(shelf) = get(resp, &format!("continuationContents.{key}")) {
            return Some(Shelf::of(shelf, "contents"));
        }
    }
    None
}

/// The grid of a library page (playlists, albums) or of a continuation response.
fn find_grid(resp: &Value) -> Option<Shelf<'_>> {
    for tab in 0..3 {
        let base =
            format!("{SINGLE_COLUMN_TAB}.{tab}.tabRenderer.content.sectionListRenderer.contents");
        for section in get_array(resp, &base).unwrap_or(&[]) {
            let grid = section
                .get("gridRenderer")
                .or_else(|| get(section, "itemSectionRenderer.contents.0.gridRenderer"));
            if let Some(grid) = grid {
                return Some(Shelf::of(grid, "items"));
            }
        }
    }
    if let Some(grid) = get(resp, "continuationContents.gridContinuation") {
        return Some(Shelf::of(grid, "items"));
    }
    get_array(
        resp,
        "onResponseReceivedActions.0.appendContinuationItemsAction.continuationItems",
    )
    .map(Shelf::appended)
}

/// Looks at the first [`MAX_ITEMS`] of `items`: `f` turns one into a value or drops it. An item
/// for which `ignored` holds (a continuation marker) is neither kept nor counted.
fn collect<T>(
    items: &[Value],
    ignored: impl Fn(&Value) -> bool,
    mut f: impl FnMut(&Value) -> Option<T>,
) -> (Vec<T>, u32, usize) {
    let (mut out, mut skipped, mut raw) = (Vec::new(), 0u32, 0usize);
    for item in items.iter().take(MAX_ITEMS) {
        if ignored(item) {
            continue;
        }
        raw += 1;
        match f(item) {
            Some(v) => out.push(v),
            None => skipped = skipped.saturating_add(1),
        }
    }
    (out, skipped, raw)
}

fn is_continuation_marker(item: &Value) -> bool {
    item.get("continuationItemRenderer").is_some()
}

fn finish<T>(shelf: Shelf<'_>, items: Vec<T>, skipped: u32, raw: usize) -> Page<T> {
    // A page that read nothing does not point at a next one: no loop on an empty answer.
    let next = if raw == 0 { None } else { shelf.next };
    Page {
        items,
        skipped,
        raw,
        next,
    }
}

// ---- text, runs, images -------------------------------------------------------------------

/// `m:ss` or `h:mm:ss` in milliseconds.
pub fn parse_duration_ms(s: &str) -> Option<u64> {
    let parts: Vec<&str> = s.trim().split(':').collect();
    if !(2..=3).contains(&parts.len()) {
        return None;
    }
    let mut secs: u64 = 0;
    for p in &parts {
        if p.is_empty() || p.len() > 3 || !p.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        secs = secs * 60 + p.parse::<u64>().ok()?;
    }
    Some(secs * 1000)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RunKind {
    Artist,
    Album,
    Other,
}

/// What a run with a link is: an artist, an album or something else. `None` for plain text.
fn linked_run_kind(run: &Value) -> Option<RunKind> {
    run.get("navigationEndpoint")?;
    let page_type = get_str(run, PAGE_TYPE);
    let browse_id = get_str(run, "navigationEndpoint.browseEndpoint.browseId").unwrap_or("");
    Some(match page_type {
        Some("MUSIC_PAGE_TYPE_ALBUM" | "MUSIC_PAGE_TYPE_AUDIOBOOK") => RunKind::Album,
        Some(
            "MUSIC_PAGE_TYPE_ARTIST" | "MUSIC_PAGE_TYPE_UNKNOWN" | "MUSIC_PAGE_TYPE_USER_CHANNEL",
        ) => RunKind::Artist,
        _ if browse_id.starts_with("MPRE") => RunKind::Album,
        _ if browse_id.starts_with("UC") => RunKind::Artist,
        _ => RunKind::Other,
    })
}

fn is_separator(text: &str) -> bool {
    let t = text.trim();
    t.is_empty()
        || t.chars()
            .all(|c| matches!(c, '•' | '&' | ',' | '-' | '·' | '|' | '/'))
}

fn is_type_label(text: &str) -> bool {
    matches!(
        text.trim(),
        "Song"
            | "Video"
            | "Single"
            | "EP"
            | "Album"
            | "Playlist"
            | "Artist"
            | "Episode"
            | "Podcast"
    )
}

/// The link of an artist run: only a `UC` id on an artist page (or on no stated page type).
fn artist_ref(run: &Value, name: &str) -> Option<EntityRef> {
    if !matches!(
        get_str(run, PAGE_TYPE),
        None | Some("MUSIC_PAGE_TYPE_ARTIST")
    ) || name.is_empty()
    {
        return None;
    }
    let uri = ids::artist_uri(get_str(run, "navigationEndpoint.browseEndpoint.browseId")?)?;
    Some(EntityRef {
        uri,
        name: name.to_owned(),
    })
}

/// The link of an album run: only an `MPRE` id on an album page (or on no stated page type).
fn album_ref(run: &Value, name: &str) -> Option<EntityRef> {
    if !matches!(
        get_str(run, PAGE_TYPE),
        None | Some("MUSIC_PAGE_TYPE_ALBUM")
    ) || name.is_empty()
    {
        return None;
    }
    let uri = ids::album_uri(get_str(run, "navigationEndpoint.browseEndpoint.browseId")?)?;
    Some(EntityRef {
        uri,
        name: name.to_owned(),
    })
}

/// Adds `r` unless it is `None`, a repeat, or past [`MAX_ARTISTS`].
fn push_ref(into: &mut Vec<EntityRef>, r: Option<EntityRef>) {
    if let Some(r) = r
        && into.len() < MAX_ARTISTS
        && !into.iter().any(|x| x.uri == r.uri)
    {
        into.push(r);
    }
}

fn kind_of_label(text: &str) -> AlbumKind {
    match text.trim() {
        "Album" => AlbumKind::Album,
        "Single" | "EP" => AlbumKind::Single,
        "Compilation" => AlbumKind::Compilation,
        _ => AlbumKind::Unknown,
    }
}

fn plain_year(text: &str) -> Option<u16> {
    let t = text.trim();
    (t.len() == 4 && t.bytes().all(|b| b.is_ascii_digit()))
        .then(|| t.parse::<u16>().ok())
        .flatten()
        .filter(|y| (1000..=2999).contains(y))
}

fn image_host_ok(url: &reqwest::Url) -> bool {
    if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    if url.port().is_some() {
        return false;
    }
    let Some(domain) = url.domain() else {
        return false;
    };
    IMAGE_HOSTS.iter().any(|h| {
        domain == *h
            || domain
                .strip_suffix(h)
                .is_some_and(|prefix| prefix.ends_with('.'))
    })
}

/// The thumbnails at `path` that are on an allowed host: at most [`MAX_IMAGES`], the widest first.
fn images_at(v: &Value, path: &str) -> Vec<ImageRef> {
    let mut out: Vec<ImageRef> = Vec::new();
    for t in get_array(v, path).unwrap_or(&[]).iter().take(32) {
        let Some(text) = t.get("url").and_then(Value::as_str) else {
            continue;
        };
        if text.len() > 2048 {
            continue;
        }
        let Ok(url) = reqwest::Url::parse(text) else {
            continue;
        };
        if !image_host_ok(&url) {
            continue;
        }
        let side = |k: &str| {
            t.get(k)
                .and_then(Value::as_u64)
                .and_then(|n| u32::try_from(n).ok())
        };
        out.push(ImageRef {
            url: url.into(),
            width: side("width"),
            height: side("height"),
        });
    }
    out.sort_by_key(|i| std::cmp::Reverse(i.width.unwrap_or(0)));
    out.truncate(MAX_IMAGES);
    out
}

fn dedup_join(names: Vec<String>) -> String {
    let mut seen: Vec<String> = Vec::new();
    for n in names {
        if !n.is_empty() && !seen.contains(&n) {
            seen.push(n);
        }
    }
    // Many long names joined: the whole is cut to the same limit as any other string.
    clean(&seen.join(", "))
}

// ---- tracks -------------------------------------------------------------------------------

/// What the flex columns after the title say.
#[derive(Default)]
struct Details {
    artists: Vec<String>,
    artist_refs: Vec<EntityRef>,
    album: Option<String>,
    album_ref: Option<EntityRef>,
    duration_ms: Option<u64>,
}

fn row_details(row: &Value) -> Details {
    let mut d = Details::default();
    let mut loose: Vec<String> = Vec::new();
    let columns = get_array(row, "flexColumns").unwrap_or(&[]);
    for (index, col) in columns.iter().enumerate().skip(1).take(8) {
        let runs =
            get_array(col, "musicResponsiveListItemFlexColumnRenderer.text.runs").unwrap_or(&[]);
        for run in runs.iter().take(32) {
            let Some(text) = run.get("text").and_then(Value::as_str) else {
                continue;
            };
            match linked_run_kind(run) {
                Some(RunKind::Artist) => {
                    let name = clean(text);
                    push_ref(&mut d.artist_refs, artist_ref(run, &name));
                    d.artists.push(name);
                }
                Some(RunKind::Album) => {
                    if d.album.is_none() {
                        let name = clean(text);
                        d.album_ref = album_ref(run, &name);
                        d.album = Some(name);
                    }
                }
                Some(RunKind::Other) => {}
                None => {
                    if let Some(ms) = parse_duration_ms(text) {
                        d.duration_ms.get_or_insert(ms);
                    } else if index == 1 && !is_separator(text) && !is_type_label(text) {
                        loose.push(clean(text));
                    }
                }
            }
        }
    }
    if d.artists.iter().all(String::is_empty) {
        d.artists = loose;
    }
    d
}

fn fixed_duration_ms(row: &Value) -> Option<u64> {
    let text = "fixedColumns.0.musicResponsiveListItemFixedColumnRenderer.text";
    let s = get_str(row, &format!("{text}.runs.0.text"))
        .or_else(|| get_str(row, &format!("{text}.simpleText")))?;
    parse_duration_ms(s)
}

/// The video id of a row: its play button's, else its `playlistItemData`.
fn row_video_id(row: &Value) -> Option<&str> {
    get_str(
        row,
        "overlay.musicItemThumbnailOverlayRenderer.content.musicPlayButtonRenderer.playNavigationEndpoint.watchEndpoint.videoId",
    )
    .or_else(|| get_str(row, "playlistItemData.videoId"))
    .filter(|id| ids::valid_id(id))
}

fn row_like(row: &Value) -> Option<bool> {
    get_str(
        row,
        "menu.menuRenderer.topLevelButtons.0.likeButtonRenderer.likeStatus",
    )
    .map(|s| s == "LIKE")
}

/// One `musicResponsiveListItemRenderer` row as a track. `album` is the header of the album page
/// the row is on: it names the album, the year and the cover that the rows themselves leave out.
fn parse_track_row(item: &Value, album: Option<&AlbumHeader>) -> Option<TrackRow> {
    let row = item.get("musicResponsiveListItemRenderer")?;
    let video_id = row_video_id(row)?;
    let uri = ids::track_uri(video_id)?;

    let title = get_str(
        row,
        "flexColumns.0.musicResponsiveListItemFlexColumnRenderer.text.runs.0.text",
    )
    .map(clean)
    .unwrap_or_default();
    let details = row_details(row);
    let duration_ms = fixed_duration_ms(row).or(details.duration_ms).unwrap_or(0);

    let mut artist = dedup_join(details.artists);
    let mut artists = details.artist_refs;
    let mut album_ref = details.album_ref;
    let mut images = images_at(row, "thumbnail.musicThumbnailRenderer.thumbnail.thumbnails");
    let mut album_name = details.album.unwrap_or_default();
    let mut year = None;
    if let Some(h) = album {
        album_name = h.title.clone();
        album_ref = h.album_ref();
        year = h.year;
        if artist.is_empty() {
            artist = h.artist.clone();
            artists = h.artists.clone();
        }
        if images.is_empty() {
            images = h.images.clone();
        }
    }
    Some(TrackRow {
        track: Track {
            uri,
            name: title,
            artist,
            album: album_name,
            duration_ms,
            images,
            year,
            album_ref,
            artists,
        },
        like: row_like(row),
    })
}

fn is_row(item: &Value) -> bool {
    item.get("musicResponsiveListItemRenderer").is_some()
}

/// A page of tracks from a playlist, album, liked-songs or continuation response. `None` when the
/// answer has no track shelf at all.
pub fn track_page(resp: &Value, album: Option<&AlbumHeader>) -> Option<Page<TrackRow>> {
    let shelf = find_track_shelf(resp)?;
    let (items, skipped, raw) = collect(shelf.items, is_continuation_marker, |i| {
        parse_track_row(i, album)
    });
    Some(finish(shelf, items, skipped, raw))
}

/// The header of an album page.
pub fn album_header(resp: &Value) -> Option<AlbumHeader> {
    let new = format!(
        "{TWO_COLUMN}.tabs.0.tabRenderer.content.sectionListRenderer.contents.0.musicResponsiveHeaderRenderer"
    );
    let header = get(resp, &new).or_else(|| get(resp, "header.musicDetailHeaderRenderer"))?;
    let title = get_str(header, "title.runs.0.text")
        .map(clean)
        .unwrap_or_default();

    let mut artists: Vec<String> = Vec::new();
    let mut refs: Vec<EntityRef> = Vec::new();
    let mut loose: Vec<String> = Vec::new();
    for run in get_array(header, "straplineTextOne.runs")
        .unwrap_or(&[])
        .iter()
        .take(16)
    {
        let Some(text) = run.get("text").and_then(Value::as_str) else {
            continue;
        };
        match linked_run_kind(run) {
            Some(RunKind::Artist) => {
                let name = clean(text);
                push_ref(&mut refs, artist_ref(run, &name));
                artists.push(name);
            }
            Some(_) => {}
            None if !is_separator(text) => loose.push(clean(text)),
            None => {}
        }
    }
    let subtitle = get_array(header, "subtitle.runs").unwrap_or(&[]);
    let year = subtitle
        .iter()
        .filter_map(|r| r.get("text").and_then(Value::as_str))
        .find_map(plain_year);
    // The older header has the artist in its subtitle, after the type and a separator.
    if artists.is_empty() && loose.is_empty() {
        for run in subtitle.iter().skip(2).take(16) {
            let Some(text) = run.get("text").and_then(Value::as_str) else {
                continue;
            };
            if linked_run_kind(run) == Some(RunKind::Artist) {
                let name = clean(text);
                push_ref(&mut refs, artist_ref(run, &name));
                artists.push(name);
            }
        }
    }
    if artists.is_empty() {
        artists = loose;
    }
    let images = images_at(
        header,
        "thumbnail.musicThumbnailRenderer.thumbnail.thumbnails",
    );
    let kind = subtitle
        .first()
        .and_then(|r| r.get("text").and_then(Value::as_str))
        .map_or(AlbumKind::Unknown, kind_of_label);
    let (audio_playlist_id, saved) = header_actions(header);
    Some(AlbumHeader {
        title,
        artist: dedup_join(artists),
        year,
        images,
        artists: refs,
        kind,
        uri: String::new(),
        audio_playlist_id,
        saved,
    })
}

/// The audio playlist a save targets and whether the album is saved, from the header's buttons:
/// the save toggle's own target first, else the play button's playlist. A playlist id that is
/// not an album's audio playlist (`OLAK5uy_..`) is never taken.
fn header_actions(header: &Value) -> (Option<String>, Option<bool>) {
    let (mut from_toggle, mut from_play, mut saved) = (None, None, None);
    for path in [
        "buttons",
        "menu.menuRenderer.topLevelButtons",
        "menu.menuRenderer.items",
    ] {
        for b in get_array(header, path).unwrap_or(&[]).iter().take(16) {
            let play = ["watchEndpoint", "watchPlaylistEndpoint"]
                .iter()
                .find_map(|k| {
                    get_str(
                        b,
                        &format!("musicPlayButtonRenderer.playNavigationEndpoint.{k}.playlistId"),
                    )
                });
            if let Some(id) = play.and_then(ids::audio_playlist_id) {
                from_play.get_or_insert_with(|| id.to_owned());
            }
            if let Some(like) = get(
                b,
                "toggleButtonRenderer.defaultServiceEndpoint.likeEndpoint",
            ) {
                if let Some(id) =
                    get_str(like, "target.playlistId").and_then(ids::audio_playlist_id)
                {
                    from_toggle.get_or_insert_with(|| id.to_owned());
                }
                // The endpoint names what a press does: LIKE saves, INDIFFERENT removes.
                match get_str(like, "status") {
                    Some("LIKE") => _ = saved.get_or_insert(false),
                    Some("INDIFFERENT") => _ = saved.get_or_insert(true),
                    _ => {}
                }
            }
        }
    }
    (from_toggle.or(from_play), saved)
}

/// A page of search results (songs). An answer with no results is an empty page.
pub fn search_page(resp: &Value) -> Option<Page<TrackRow>> {
    if !resp.is_object() {
        return None;
    }
    let sections = get_array(
        resp,
        "contents.tabbedSearchResultsRenderer.tabs.0.tabRenderer.content.sectionListRenderer.contents",
    );
    let mut rows: Vec<&Value> = Vec::new();
    let mut next = None;
    if let Some(sections) = sections {
        for section in sections.iter().take(32) {
            if let Some(shelf) = section.get("musicShelfRenderer") {
                let shelf = Shelf::of(shelf, "contents");
                rows.extend(shelf.items.iter().filter(|i| is_row(i)).take(MAX_ITEMS));
                next = shelf.next;
            }
        }
    } else if let Some(shelf) = find_track_shelf(resp) {
        rows.extend(shelf.items.iter().filter(|i| is_row(i)).take(MAX_ITEMS));
        next = shelf.next;
    } else {
        return Some(Page::empty());
    }
    rows.truncate(MAX_ITEMS);
    let mut items = Vec::new();
    let mut skipped = 0u32;
    for row in &rows {
        match parse_track_row(row, None) {
            Some(t) => items.push(t),
            None => skipped = skipped.saturating_add(1),
        }
    }
    let raw = rows.len();
    Some(Page {
        items,
        skipped,
        raw,
        next: if raw == 0 { None } else { next },
    })
}

// ---- playlists and albums (the library grids) ------------------------------------------------

fn two_row(item: &Value) -> Option<&Value> {
    item.get("musicTwoRowItemRenderer")
}

fn two_row_browse_id(d: &Value) -> Option<&str> {
    get_str(d, "title.runs.0.navigationEndpoint.browseEndpoint.browseId")
        .or_else(|| get_str(d, "navigationEndpoint.browseEndpoint.browseId"))
}

/// "12 songs" or "1 song" among the subtitle's runs.
fn subtitle_track_count(d: &Value) -> Option<u32> {
    get_array(d, "subtitle.runs")?
        .iter()
        .filter_map(|r| r.get("text").and_then(Value::as_str))
        .find_map(|t| {
            let (n, rest) = t.trim().split_once(' ')?;
            matches!(rest, "song" | "songs" | "track" | "tracks")
                .then(|| n.replace(',', "").parse::<u32>().ok())
                .flatten()
        })
}

fn parse_playlist(item: &Value) -> Option<Playlist> {
    let d = two_row(item)?;
    let browse_id = two_row_browse_id(d)?;
    if !browse_id.starts_with("VL") {
        return None;
    }
    let id = ids::playlist_uri(ids::playlist_id_of_browse_id(browse_id)?)?;
    let name = get_str(d, "title.runs.0.text")
        .map(clean)
        .filter(|n| !n.is_empty())?;
    Some(Playlist {
        id,
        name,
        readable: Readable::Readable,
        track_count: subtitle_track_count(d),
        images: images_at(
            d,
            "thumbnailRenderer.musicThumbnailRenderer.thumbnail.thumbnails",
        ),
    })
}

fn parse_album(item: &Value) -> Option<Album> {
    let d = two_row(item)?;
    let browse_id = two_row_browse_id(d)?;
    if !browse_id.starts_with("MPRE") {
        return None;
    }
    let id = ids::album_uri(browse_id)?;
    let name = get_str(d, "title.runs.0.text")
        .map(clean)
        .filter(|n| !n.is_empty())?;

    let mut artists: Vec<String> = Vec::new();
    let mut refs: Vec<EntityRef> = Vec::new();
    let mut loose: Vec<String> = Vec::new();
    let mut year = None;
    let subtitle = get_array(d, "subtitle.runs").unwrap_or(&[]);
    let kind = subtitle
        .first()
        .and_then(|r| r.get("text").and_then(Value::as_str))
        .map_or(AlbumKind::Unknown, kind_of_label);
    // The first run is the type ("Album", "Single", "EP"), then separated artists and the year.
    for run in subtitle.iter().skip(1).take(16) {
        let Some(text) = run.get("text").and_then(Value::as_str) else {
            continue;
        };
        match linked_run_kind(run) {
            Some(RunKind::Artist) => {
                let name = clean(text);
                push_ref(&mut refs, artist_ref(run, &name));
                artists.push(name);
            }
            Some(_) => {}
            None => {
                if let Some(y) = plain_year(text) {
                    year = Some(y);
                } else if !is_separator(text) && !is_type_label(text) {
                    loose.push(clean(text));
                }
            }
        }
    }
    if artists.is_empty() {
        artists = loose;
    }
    Some(Album {
        uri: id.clone(),
        id,
        name,
        artist: dedup_join(artists),
        track_count: None,
        images: images_at(
            d,
            "thumbnailRenderer.musicThumbnailRenderer.thumbnail.thumbnails",
        ),
        year,
        artists: refs,
        kind,
    })
}

/// One release of an artist page's carousel or "More" grid. The kind is the group's (an album
/// of the Albums group may say "Compilation"); an item that names no artist is the page's own.
fn parse_release(item: &Value, group: ReleaseGroup, owner: &EntityRef) -> Option<Album> {
    let mut a = parse_album(item)?;
    a.kind = match group {
        ReleaseGroup::Singles => AlbumKind::Single,
        _ if a.kind == AlbumKind::Compilation => AlbumKind::Compilation,
        _ => AlbumKind::Album,
    };
    if a.artists.is_empty() {
        a.artists = vec![owner.clone()];
        a.artist = owner.name.clone();
    }
    Some(a)
}

/// Where a carousel's "More" link leads: an `MPAD` browse id and its params, nothing else.
fn more_endpoint(carousel: &Value) -> Option<More> {
    let head = "header.musicCarouselShelfBasicHeaderRenderer";
    let ep = get(
        carousel,
        &format!("{head}.title.runs.0.navigationEndpoint.browseEndpoint"),
    )
    .or_else(|| {
        get(
            carousel,
            &format!("{head}.moreContentButton.buttonRenderer.navigationEndpoint.browseEndpoint"),
        )
    })?;
    let browse_id =
        get_str(ep, "browseId").filter(|b| b.starts_with("MPAD") && ids::valid_id(b))?;
    let params = get_str(ep, "params").filter(|p| token_ok(p))?;
    Some(More {
        browse_id: browse_id.to_owned(),
        params: params.to_owned(),
    })
}

/// An artist page: `owner` is the artist asked for (its URI), whose name the header gives.
/// `None` when the answer has no header with a name.
pub fn artist_page(resp: &Value, uri: &str) -> Option<ArtistParse> {
    let header = get(resp, "header.musicImmersiveHeaderRenderer")
        .or_else(|| get(resp, "header.musicVisualHeaderRenderer"))?;
    let name = get_str(header, "title.runs.0.text")
        .map(clean)
        .filter(|n| !n.is_empty())?;
    let mut images = images_at(
        header,
        "thumbnail.musicThumbnailRenderer.thumbnail.thumbnails",
    );
    if images.is_empty() {
        images = images_at(
            header,
            "foregroundThumbnail.musicThumbnailRenderer.thumbnail.thumbnails",
        );
    }
    let owner = EntityRef {
        uri: uri.to_owned(),
        name: name.clone(),
    };
    let mut out = ArtistParse {
        name,
        images,
        popular: Vec::new(),
        albums: ReleaseShelf::default(),
        singles: ReleaseShelf::default(),
    };
    let sections = get_array(
        resp,
        &format!("{SINGLE_COLUMN_TAB}.0.tabRenderer.content.sectionListRenderer.contents"),
    )
    .unwrap_or(&[]);
    let (mut songs_seen, mut albums_seen, mut singles_seen) = (false, false, false);
    for section in sections.iter().take(32) {
        if let Some(shelf) = section.get("musicShelfRenderer") {
            if songs_seen {
                continue;
            }
            songs_seen = true;
            for item in get_array(shelf, "contents")
                .unwrap_or(&[])
                .iter()
                .take(MAX_ITEMS)
            {
                if out.popular.len() >= MAX_POPULAR {
                    break;
                }
                let Some(mut t) = parse_track_row(item, None).map(|r| r.track) else {
                    continue;
                };
                // A song of the artist's own page may leave the artist out of its columns.
                if t.artists.is_empty() {
                    t.artists = vec![owner.clone()];
                    if t.artist.is_empty() {
                        t.artist = owner.name.clone();
                    }
                }
                out.popular.push(t);
            }
        } else if let Some(carousel) = section.get("musicCarouselShelfRenderer") {
            let title = get_str(
                carousel,
                "header.musicCarouselShelfBasicHeaderRenderer.title.runs.0.text",
            )
            .map(|t| t.trim().to_lowercase());
            let (group, seen, target) = match title.as_deref() {
                Some("albums") => (ReleaseGroup::Albums, &mut albums_seen, &mut out.albums),
                Some("singles & eps" | "singles and eps" | "singles") => {
                    (ReleaseGroup::Singles, &mut singles_seen, &mut out.singles)
                }
                _ => continue,
            };
            if *seen {
                continue;
            }
            *seen = true;
            let items = get_array(carousel, "contents").unwrap_or(&[]);
            let (albums, _, _) = collect(items, |_| false, |i| parse_release(i, group, &owner));
            *target = ReleaseShelf {
                items: albums,
                more: more_endpoint(carousel),
            };
        }
    }
    Some(out)
}

/// A grid page of an artist's releases of `group` (the "More" page or its continuation).
pub fn releases_page(resp: &Value, group: ReleaseGroup, owner: &EntityRef) -> Option<Page<Album>> {
    grid_page(resp, |i| parse_release(i, group, owner))
}

/// A grid page of the library. An answer with a `contents` but no grid is an empty library.
fn grid_page<T>(resp: &Value, parse: impl Fn(&Value) -> Option<T>) -> Option<Page<T>> {
    let Some(shelf) = find_grid(resp) else {
        return resp.get("contents").map(|_| Page::empty());
    };
    let (items, skipped, raw) = collect(shelf.items, is_continuation_marker, |i| parse(i));
    Some(finish(shelf, items, skipped, raw))
}

/// A page of the user's playlists.
pub fn playlists_page(resp: &Value) -> Option<Page<Playlist>> {
    grid_page(resp, parse_playlist)
}

/// A page of the user's saved albums.
pub fn albums_page(resp: &Value) -> Option<Page<Album>> {
    grid_page(resp, parse_album)
}
