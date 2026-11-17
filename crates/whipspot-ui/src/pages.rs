//! The track pages' chrome (MD59): where the hero column, the filter bar and the table go, the
//! hero column itself (breadcrumb, cover, facts, play and shuffle-play), the filter bar
//! and the pure helpers (the filter, the meta text). The table is `tracks.rs`.

use lune_ui::input::CursorIcon;
use lune_ui::paint::Rect;
use lune_ui::ui::{Sense, Ui};
use lune_ui::widgets::filter_bar::FilterOptions;
use whipspot_core::provider::Track;

use crate::art::{Art, NoArt, cover};
use crate::draw::{disc, glow, knob, shadow, text, width};
use crate::history::Route;
use crate::icons::{Icon, icon, play_filled};
use crate::theme::{Look, PlayButton};

/// The window width from which the hero column has its full size, and under which it hides.
pub const HERO_FULL_W: f32 = 1180.0;
pub const HERO_HIDDEN_W: f32 = 900.0;
/// The hero art's side at the full size and at the smallest width that shows it.
pub const SIDE_MAX: f32 = 300.0;
pub const SIDE_MIN: f32 = 200.0;
/// The hero column is hidden when the sheet is too short for at least this much art.
const SIDE_FLOOR: f32 = 120.0;
/// What the hero column needs below its art: name, artist, meta and the buttons.
const HERO_BELOW: f32 = 174.0;
/// The hero art's top below the content top, and the gap between art and table.
const ART_TOP: f32 = 44.0;
const GAP_FULL: f32 = 72.0;
const GAP_SMALL: f32 = 48.0;
/// The filter bar's size.
pub const FILTER_W: f32 = 280.0;
pub const FILTER_H: f32 = 32.0;
/// The table header band's top below the content top: next to a hero column, and under a page
/// header with its filter bar.
const BAND_HERO: f32 = 44.0;
const BAND_PLAIN: f32 = 106.0;
/// The filter bar's top below the content top (next to the breadcrumb; under the page header).
const FILTER_HERO_DY: f32 = -4.0;
const FILTER_PLAIN_DY: f32 = 58.0;
/// The icon stroke of the hero's buttons.
const HAIR: f32 = 1.5;

/// Where the parts of a track page go, logical px.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Geo {
    /// The hero art's side, when the page has the hero column.
    pub hero_side: Option<f32>,
    /// The filter bar.
    pub filter: Rect,
    /// The table: its header band at the top, the rows under it down to the content's bottom.
    pub table: Rect,
}

/// The hero art's side in a window `window_w` wide over a content area `content_h` tall: 300 from
/// 1180 px, scaled down to 200 at 900 px, none below that or when the area is too short.
pub fn hero_side(window_w: f32, content_h: f32) -> Option<f32> {
    if window_w < HERO_HIDDEN_W {
        return None;
    }
    let t = ((window_w - HERO_HIDDEN_W) / (HERO_FULL_W - HERO_HIDDEN_W)).clamp(0.0, 1.0);
    let by_width = (SIDE_MIN + (SIDE_MAX - SIDE_MIN) * t).floor();
    let side = by_width.min((content_h - ART_TOP - HERO_BELOW).floor());
    (side >= SIDE_FLOOR).then_some(side)
}

/// The geometry of a page over the content rect `c` of a window `window_w` wide; `hero` asks for
/// the hero column (playlist and album routes), which is left out when it does not fit.
pub fn geometry(window_w: f32, c: Rect, hero: bool) -> Geo {
    let side = hero.then(|| hero_side(window_w, c.h)).flatten();
    let (lx, band_dy, filter_dy) = match side {
        Some(s) => {
            let gap = if s >= SIDE_MAX { GAP_FULL } else { GAP_SMALL };
            (c.x + s + gap, BAND_HERO, FILTER_HERO_DY)
        }
        None => (c.x, BAND_PLAIN, FILTER_PLAIN_DY),
    };
    let w = (c.right() - lx).max(0.0);
    let band_y = c.y + band_dy;
    Geo {
        hero_side: side,
        filter: Rect::new(lx, c.y + filter_dy, FILTER_W.min(w), FILTER_H),
        table: Rect::new(lx, band_y, w, (c.bottom() - band_y).max(0.0)),
    }
}

/// The positions in `tracks` whose title, artist or album contains `query`, ignoring case; an
/// empty query keeps them all.
pub fn filter(tracks: &[Track], query: &str) -> Vec<usize> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return (0..tracks.len()).collect();
    }
    let has = |s: &str| s.to_lowercase().contains(&q);
    tracks
        .iter()
        .enumerate()
        .filter(|(_, t)| has(&t.name) || has(&t.artist) || has(&t.album))
        .map(|(i, _)| i)
        .collect()
}

/// The result of [`filter`] kept between frames (M5-F6): the filter lowercases and allocates per
/// row, so a page asks the cache each frame and the filter runs again only when the route, the
/// text, the list (its serial) or the number of loaded rows changed.
#[derive(Debug, Default)]
pub struct FilterCache {
    key: Option<(Route, String, u64, usize)>,
    visible: Vec<usize>,
    /// How many times the filter ran (the test counts them).
    #[cfg(test)]
    runs: u32,
}

impl FilterCache {
    /// The positions of `tracks` that `query` keeps (see [`filter`]) on `route`, for the list
    /// named by `serial`.
    pub fn get(&mut self, route: &Route, query: &str, serial: u64, tracks: &[Track]) -> &[usize] {
        let same = self.key.as_ref().is_some_and(|(r, q, s, n)| {
            r == route && q == query && *s == serial && *n == tracks.len()
        });
        if !same {
            self.visible = filter(tracks, query);
            self.key = Some((route.clone(), query.to_owned(), serial, tracks.len()));
            #[cfg(test)]
            {
                self.runs += 1;
            }
        }
        &self.visible
    }
}

/// The hero's facts line: "Album · 1993 · N songs · M min" or "Playlist · N songs". `year` is the
/// album's release year when known (albums only); `songs` is the list's size when known; the
/// minutes (albums) sum the loaded durations and wait until every page is `complete`.
pub fn meta_text(
    album: bool,
    year: Option<u16>,
    songs: Option<u32>,
    loaded: &[Track],
    complete: bool,
) -> String {
    let mut parts = vec![(if album { "Album" } else { "Playlist" }).to_owned()];
    if let Some(y) = year.filter(|_| album) {
        parts.push(y.to_string());
    }
    if let Some(n) = songs {
        parts.push(if n == 1 {
            "1 song".to_owned()
        } else {
            format!("{n} songs")
        });
    }
    if album && complete {
        let ms: u64 = loaded.iter().map(|t| t.duration_ms).sum();
        parts.push(format!("{} min", ms / 60_000));
    }
    parts.join(" \u{b7} ")
}

/// What the hero column shows.
#[derive(Debug, Clone, Copy)]
pub struct Hero<'a> {
    /// "Albums" or "Playlists": the breadcrumb's first part, which opens that tab.
    pub crumb: &'a str,
    pub name: &'a str,
    /// The artist (album) or owner (playlist), when known.
    pub by: Option<&'a str>,
    pub meta: &'a str,
    pub side: f32,
    /// The registered cover, when it is there; the placeholder otherwise.
    pub art: Option<Art>,
}

/// What the hero column reported in a frame.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct HeroOut {
    pub crumb: bool,
    pub play: bool,
    pub shuffle: bool,
}

/// Draws the hero column of content rect `c`: the breadcrumb, the cover, the name, the
/// artist, the meta line, and the play and shuffle-play buttons.
pub fn hero(ui: &mut Ui<'_>, c: Rect, k: &Look, h: &Hero<'_>) -> HeroOut {
    let ink = k.ink;
    let rad = k.radii();
    let side = h.side;
    let art = Rect::new(c.x, c.y + ART_TOP, side, side);
    let mut y = art.bottom() + 24.0;
    let name_y = y;
    y += 44.0;
    let by_y = y;
    if h.by.is_some() {
        y += 28.0;
    }
    let meta_y = y;
    let btn_y = meta_y + 52.0;
    let play_c = (c.x + 26.0, btn_y);
    let shuffle_c = (c.x + 86.0, btn_y);

    // The breadcrumb's first part is a link.
    let bw = {
        let p = ui.painter();
        width(p, h.crumb, 13.5, false)
    };
    let crumb_r = Rect::new(c.x - 4.0, c.y - 4.0, bw + 8.0, 26.0);
    let crumb = ui.interact(ui.id(("crumb",)), crumb_r, Sense::CLICK_FOCUS);
    let play_r = Rect::new(play_c.0 - 26.0, play_c.1 - 26.0, 52.0, 52.0);
    let play = ui.interact(ui.id(("hero-play",)), play_r, Sense::CLICK_FOCUS);
    let shuffle_r = Rect::new(shuffle_c.0 - 18.0, shuffle_c.1 - 18.0, 36.0, 36.0);
    let shuffle = ui.interact(ui.id(("hero-shuffle",)), shuffle_r, Sense::CLICK_FOCUS);
    if crumb.hovered || play.hovered || shuffle.hovered {
        ui.set_cursor(CursorIcon::Pointer);
    }

    let p = ui.painter();
    let crumb_c = if crumb.hovered { ink.text2 } else { ink.muted };
    text(p, h.crumb, 13.5, crumb_c, false, c.x, c.y, 0.0);
    icon(
        p,
        Icon::ChevR,
        (c.x + bw + 14.0, c.y + 9.0),
        10.0,
        ink.faint,
        1.2,
    );
    let name_w = (side + 56.0 - bw - 28.0).max(40.0);
    text(
        p,
        h.name,
        13.5,
        ink.text2,
        false,
        c.x + bw + 28.0,
        c.y,
        name_w,
    );

    // The cover with the mockup's glow (theme C) or drop shadow, or a quiet placeholder.
    if h.art.is_some() {
        if k.decor().art_glow {
            glow(p, art, rad.art.min(18.0), 50.0, k.a, 0.45);
        } else {
            shadow(p, art, rad.art.min(18.0), 22.0, 0.9);
        }
    } else if rad.art < 6.0 {
        shadow(p, art, rad.art.min(18.0), 10.0, 0.6);
    }
    cover(p, k, art, rad.art.min(18.0), h.art, NoArt::Card);

    let name_size = (30.0 * side / SIDE_MAX).clamp(24.0, 30.0);
    text(
        p,
        h.name,
        name_size,
        ink.text,
        k.decor().display_medium,
        c.x - 1.0,
        name_y,
        side,
    );
    if let Some(by) = h.by {
        text(p, by, 17.0, k.hi_or_text(), false, c.x, by_y, side);
    }
    text(p, h.meta, 13.0, ink.muted, false, c.x, meta_y, side);

    if k.decor().play == PlayButton::Knob {
        knob(p, play_c, 52.0);
        play_filled(p, (play_c.0 + 2.0, play_c.1), 17.0, k.a);
    } else {
        disc(p, play_c, 52.0, k.a);
        play_filled(p, (play_c.0 + 2.0, play_c.1), 17.0, k.on);
    }
    icon(p, Icon::Shuffle, shuffle_c, 19.0, ink.text2, HAIR);
    for (r, resp, radius) in [
        (play_r, &play, 26.0),
        (shuffle_r, &shuffle, 18.0),
        (crumb_r, &crumb, 6.0),
    ] {
        if resp.focus_visible {
            p.ring(r, radius, 1.5, k.a, true);
        }
    }
    HeroOut {
        crumb: crumb.clicked,
        play: play.clicked,
        shuffle: shuffle.clicked,
    }
}

/// The filter bar in `rect` editing `query`; whether it has keyboard focus.
pub fn filter_bar(ui: &mut Ui<'_>, rect: Rect, query: &mut String) -> bool {
    let opts = FilterOptions {
        placeholder: "Filter songs",
        label: Some("Filter songs"),
        clear_unfocusable: false,
    };
    ui.filter_bar("track-filter", rect, query, &opts)
        .response
        .focused
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(name: &str, artist: &str, album: &str, ms: u64) -> Track {
        Track {
            uri: format!("demo:{name}"),
            name: name.into(),
            artist: artist.into(),
            album: album.into(),
            duration_ms: ms,
            images: vec![],
            year: None,
        }
    }

    fn sample() -> Vec<Track> {
        vec![
            track("Night Drive", "Velmora", "Low Tide Sessions", 200_000),
            track("Paper Moons", "Sun Dial Youth", "Atlas", 250_000),
            track("Atlas", "Velmora", "Atlas", 180_000),
        ]
    }

    #[test]
    fn f1_filter_is_case_insensitive_over_title_artist_and_album() {
        let t = sample();
        assert_eq!(filter(&t, ""), vec![0, 1, 2], "an empty query keeps all");
        assert_eq!(filter(&t, "   "), vec![0, 1, 2], "a blank query keeps all");
        assert_eq!(filter(&t, "NIGHT"), vec![0], "title, any case");
        assert_eq!(filter(&t, "velmora"), vec![0, 2], "artist");
        assert_eq!(filter(&t, "tide ses"), vec![0], "album");
        assert_eq!(filter(&t, "atlas"), vec![1, 2], "title or album");
        assert!(filter(&t, "zzz").is_empty(), "no hits");
        assert!(filter(&[], "a").is_empty());
    }

    #[test]
    fn n1_filter_cache_recomputes_only_on_a_change() {
        let mut t = sample();
        let mut cache = FilterCache::default();
        let route = Route::Liked;
        assert_eq!(cache.get(&route, "", 1, &t), [0, 1, 2]);
        assert_eq!(cache.runs, 1);
        // The same frame again: the cached answer.
        assert_eq!(cache.get(&route, "", 1, &t), [0, 1, 2]);
        assert_eq!(cache.runs, 1);
        // A filter change recomputes, once.
        assert_eq!(cache.get(&route, "velmora", 1, &t), [0, 2]);
        assert_eq!(cache.get(&route, "velmora", 1, &t), [0, 2]);
        assert_eq!(cache.runs, 2);
        // So do another route, another list and a page that grew.
        cache.get(&Route::Search, "velmora", 1, &t);
        assert_eq!(cache.runs, 3);
        cache.get(&Route::Search, "velmora", 2, &t);
        assert_eq!(cache.runs, 4);
        t.push(track("Velmora Theme", "Velmora", "Atlas", 100_000));
        assert_eq!(cache.get(&Route::Search, "velmora", 2, &t), [0, 2, 3]);
        assert_eq!(cache.runs, 5);
        cache.get(&Route::Search, "velmora", 2, &t);
        assert_eq!(cache.runs, 5);
    }

    #[test]
    fn f2_meta_text_with_complete_and_partial_pages() {
        let t = sample();
        assert_eq!(
            meta_text(true, None, Some(3), &t, true),
            "Album \u{b7} 3 songs \u{b7} 10 min"
        );
        // Partial: the total is known, the minutes wait for every page.
        assert_eq!(
            meta_text(true, None, Some(12), &t, false),
            "Album \u{b7} 12 songs"
        );
        assert_eq!(meta_text(true, None, None, &t, false), "Album");
        // The year follows "Album"; a playlist ignores it.
        assert!(
            meta_text(true, Some(1993), Some(3), &t, true)
                .starts_with("Album \u{b7} 1993 \u{b7} 3 songs")
        );
        assert!(
            meta_text(false, Some(1993), Some(3), &t, true).starts_with("Playlist \u{b7} 3 songs")
        );
        assert_eq!(
            meta_text(false, None, Some(3), &t, true),
            "Playlist \u{b7} 3 songs"
        );
        assert_eq!(
            meta_text(false, None, Some(1), &t[..1], true),
            "Playlist \u{b7} 1 song"
        );
    }

    #[test]
    fn hero_side_scales_with_the_window_and_hides_below_900() {
        assert_eq!(hero_side(1440.0, 600.0), Some(300.0));
        assert_eq!(hero_side(1180.0, 600.0), Some(300.0));
        assert_eq!(hero_side(1000.0, 600.0), Some(235.0));
        assert_eq!(hero_side(900.0, 600.0), Some(200.0));
        assert_eq!(hero_side(899.0, 600.0), None);
        assert_eq!(hero_side(1440.0, 300.0), None, "a short sheet hides it");
    }

    #[test]
    fn geometry_at_1440_by_900() {
        let c = Rect::new(88.0, 148.0, 1264.0, 600.0);
        let g = geometry(1440.0, c, true);
        assert_eq!(g.hero_side, Some(300.0));
        assert_eq!(g.table, Rect::new(460.0, 192.0, 892.0, 556.0));
        assert_eq!(g.filter, Rect::new(460.0, 144.0, 280.0, 32.0));
        let g = geometry(1440.0, c, false);
        assert_eq!(g.hero_side, None);
        assert_eq!(g.table, Rect::new(88.0, 254.0, 1264.0, 494.0));
        assert_eq!(g.filter, Rect::new(88.0, 206.0, 280.0, 32.0));
        // A hero page without room for the hero falls back to the plain page.
        let narrow = Rect::new(88.0, 148.0, 699.0 - 176.0, 600.0);
        assert_eq!(geometry(699.0, narrow, true).hero_side, None);
    }
}
