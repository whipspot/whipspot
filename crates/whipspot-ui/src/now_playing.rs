//! The idle now-playing view: song info over the visualiser while the browse sheet is away (M5
//! step 7, M6 step 10; C6). With a cover (or lyrics) it is the round-3 layout, the whole group centred in the window: the
//! art square, or the synced lyrics in its place, and an info column (label, title, artist,
//! "album . year", the decor's rule, "next up"); without either, the centred block of round 3 with
//! no empty box (MD75). Add and more have no function (MD53, MD75): only theme A's action row
//! carries the like heart.

use lune_ui::color::Rgba;
use lune_ui::paint::Rect;
use lune_ui::scene::LayerId;
use lune_ui::text::{Spec, Style};
use lune_ui::ui::Ui;
use whipspot_core::lyrics::Lyrics;
use whipspot_core::player::{ArtUrgency, LyricsState, PlayState, PlayerHandle, PlayerState};
use whipspot_core::provider::{ImageRef, Track};

use crate::art::{Art, ArtCache, NoArt, Variant, cover};
use crate::draw::{
    P, alpha, caps_shadowed as caps, caps_width, glow, hgradient, led, outline, shadow,
    text_shadowed as text, width,
};
use crate::likes::{self, LikeBook};
use crate::lyrics as lyric_view;
use crate::theme::{Decor, Labels, Look, Rule};

/// The block sits this much higher than the rect's centre: room for the visualiser's crest.
const CREST_ROOM: f32 = 100.0;
/// Space kept above and below the block in a small window.
const EDGE: f32 = 16.0;
/// Side margin of the text (title, artist, album): the limit is the rect's width minus twice it.
const SIDE: f32 = 120.0;
/// Title size, the distance between the label and the title's top, and between two title lines.
const TITLE_SIZE: f32 = 64.0;
/// How far the idle view's quieter text colours move toward the full text colour, and the least
/// alpha they keep.
const IDLE_INK_LIFT: f32 = 0.4;
const IDLE_INK_MIN_ALPHA: u8 = 0xE6;
const TITLE_DY: f32 = 28.0;
const TITLE_PITCH: f32 = 76.0;
/// The art layout's design size (the idle area at a 1440 x 900 window) and its parts: the art's
/// side and left edge, the gap before the info column, the column's right margin and least width.
const DESIGN_W: f32 = 1440.0;
const DESIGN_H: f32 = 752.0;
const ART_SIDE: f32 = 440.0;
const ART_X: f32 = 150.0;
const COL_GAP: f32 = 96.0;
const COL_RIGHT: f32 = 120.0;
const MIN_COLUMN: f32 = 120.0;
/// The info column's widest size at the design scale (a wider window wraps the title here; the
/// group is then centred by its measured width, see [`centre_dx`]).
const COLUMN_MAX: f32 = 680.0;
/// The art layout's full info block (label to next-up card) at one title line.
const FULL_HEIGHT: f32 = 330.0;
/// The art layout's title size and the pitch of its two lines.
const SIDE_TITLE_SIZE: f32 = 62.0;
const SIDE_TITLE_PITCH: f32 = 72.0;
/// The next-up card's text limit.
const NEXT_TEXT_MAX: f32 = 420.0;
/// The capsule card's height.
const CAPSULE_H: f32 = 64.0;

/// The track that plays after the current one, if the queue names one.
pub fn next_up(state: &PlayerState) -> Option<&Track> {
    state.queue.next.and_then(|i| state.queue.items.get(i))
}

/// Draws the idle view in `rect` (the window less a caption's height top and bottom; the media control is hidden then).
pub fn idle(
    ui: &mut Ui<'_>,
    rect: Rect,
    look: &Look,
    state: &PlayerState,
    art: &mut ArtCache,
    likes: &mut LikeBook,
    player: &PlayerHandle,
) {
    let Some(now) = &state.current else {
        return;
    };
    let track = &now.track;
    let shown = lyrics_for(state, &track.uri);
    let scale = ui.painter().s;
    let m = Side::new(rect);
    // The cover is asked for only while it is to be drawn (MD62: what is drawn is what is asked).
    let cover_art = if shown.is_none() {
        let phys = (m.art.w * scale).ceil() as u32;
        ImageRef::pick(&track.images, phys)
            .and_then(|i| art.get(player, i, Variant::Plain, ArtUrgency::Current, phys))
    } else {
        None
    };
    match Body::choose(shown.is_some(), cover_art.is_some()) {
        Body::Centred => centred(ui, rect, look, state, track, art, player),
        body => side(
            ui,
            rect,
            look,
            state,
            m,
            body,
            shown,
            cover_art,
            (art, likes, player),
        ),
    }
}

/// What the idle view shows where the art goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Body {
    /// M5's centred block, no art square and no lyrics region.
    Centred,
    /// The art square and the info column beside it.
    Art,
    /// The lyrics in the art's place.
    Lyrics,
}

impl Body {
    /// Lyrics win; else a ready cover; else the centred block.
    fn choose(lyrics: bool, cover_ready: bool) -> Body {
        if lyrics {
            Body::Lyrics
        } else if cover_ready {
            Body::Art
        } else {
            Body::Centred
        }
    }
}

/// The lyrics to show for `uri`: lyrics on, found for exactly this uri, and synced with at least
/// one line. Plain (unsynced), instrumental, none, searching, failed and off show the art.
fn lyrics_for<'a>(state: &'a PlayerState, uri: &str) -> Option<&'a Lyrics> {
    if !state.lyrics_enabled {
        return None;
    }
    match &state.lyrics {
        LyricsState::Found { uri: u, lyrics } if u == uri => {
            let lyrics: &Lyrics = lyrics;
            matches!(lyrics, Lyrics::Synced(v) if !v.is_empty()).then_some(lyrics)
        }
        _ => None,
    }
}

/// The idle view's art layout at the design size (1440 x 752 of idle area), scaled down with the
/// window: the art square, the lyrics region that replaces it and the info column.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Side {
    cy: f32,
    art: Rect,
    lyrics: Rect,
    /// The info column's left edge and width.
    rx: f32,
    rw: f32,
}

impl Side {
    fn new(rect: Rect) -> Side {
        let k = (rect.w / DESIGN_W).min(rect.h / DESIGN_H).clamp(0.4, 1.0);
        let cy = ((rect.y + rect.bottom()) / 2.0).round();
        let side = (ART_SIDE * k).round();
        let gap = (COL_GAP * k).round();
        let left = (ART_X * k).round();
        // The column fills the design's right margin up to its widest size.
        let avail = rect.w - left - (COL_RIGHT * k).round() - side - gap;
        let rw = avail.min((COLUMN_MAX * k).round()).max(MIN_COLUMN);
        let ax = rect.x + left;
        let art = Rect::new(ax, cy - side / 2.0, side, side);
        // Clear of the visualiser's tallest bars.
        let lyrics = Rect::new(ax, rect.y, side + 60.0 * k, (rect.h - 200.0 * k).max(0.0));
        Side {
            cy,
            art,
            lyrics,
            rx: art.right() + gap,
            rw,
        }
    }
}

impl Side {
    /// The same layout moved `dx` px right.
    fn shifted(mut self, dx: f32) -> Side {
        self.art.x += dx;
        self.lyrics.x += dx;
        self.rx += dx;
        self
    }
}

/// How far right the group (the art or lyrics block, the gap and an info column `content_w`
/// wide) moves to sit in the middle of `rect`; never left of where the design puts it.
fn centre_dx(rect: Rect, m: &Side, content_w: f32) -> f32 {
    let content_w = content_w.clamp(MIN_COLUMN.min(m.rw), m.rw);
    let group = (m.rx - m.art.x) + content_w;
    (((rect.w - group) / 2.0).round() - (m.art.x - rect.x)).max(0.0)
}

/// Where each element of the info column sits, as offsets from its top; `None` for what is not
/// drawn.
#[derive(Debug, Clone, Copy, PartialEq)]
struct SideOffsets {
    title: f32,
    artist: f32,
    album: Option<f32>,
    rule: Option<f32>,
    next_label: Option<f32>,
    height: f32,
}

impl SideOffsets {
    fn new(d: &Decor, lines: usize, album: bool, rule: bool, next: bool) -> SideOffsets {
        let title = 26.0;
        let artist = title + 86.0 + SIDE_TITLE_PITCH * (lines.max(1) - 1) as f32;
        let album_y = album.then_some(artist + 38.0);
        let line_end = album_y.unwrap_or(artist);
        let rule_h = if d.rule == Rule::Actions { 44.0 } else { 30.0 };
        let rule_y = rule.then_some(line_end + 44.0);
        let next_label = (rule && next).then_some(line_end + 44.0 + rule_h + 14.0);
        let height = match (rule_y, next_label) {
            // The full block is as tall as round 3's (330, whatever the rule row), centred on cy.
            (_, Some(_)) => FULL_HEIGHT + SIDE_TITLE_PITCH * (lines.max(1) - 1) as f32,
            (Some(r), None) => r + rule_h + 6.0,
            (None, None) => line_end + 30.0,
        };
        SideOffsets {
            title,
            artist,
            album: album_y,
            rule: rule_y,
            next_label,
            height,
        }
    }

    /// The fullest block that fits `avail` px: drops next up, then the rule, then the title's
    /// second line.
    fn fit(d: &Decor, avail: f32, lines: usize, album: bool, next: bool) -> SideOffsets {
        let tries = [
            (lines, true, next),
            (lines, true, false),
            (lines, false, false),
            (1, false, false),
        ];
        let mut o = SideOffsets::new(d, 1, album, false, false);
        for (l, r, n) in tries {
            o = SideOffsets::new(d, l, album, r, n);
            if o.height <= avail {
                break;
            }
        }
        o
    }
}

/// The column's top: the block centred on `cy`, inside the rect.
fn side_top(rect: Rect, cy: f32, height: f32) -> f32 {
    let lo = rect.y + EDGE;
    let hi = (rect.bottom() - EDGE - height).max(lo);
    (cy - height / 2.0).round().clamp(lo, hi)
}

/// "album . year" (the separator is a middle dot); either may be missing.
fn album_line(t: &Track) -> Option<String> {
    let album = t.album.trim();
    match (album.is_empty(), t.year) {
        (false, Some(y)) => Some(format!("{album}  \u{b7}  {y}")),
        (false, None) => Some(album.to_owned()),
        (true, Some(y)) => Some(y.to_string()),
        (true, None) => None,
    }
}

/// The next-up card's cover: asked for (as `Visible`) only when the card is drawn.
fn next_thumb(art: &mut ArtCache, player: &PlayerHandle, next: &Track, scale: f32) -> Option<Art> {
    let phys = (52.0 * scale).ceil() as u32;
    ImageRef::pick(&next.images, phys)
        .and_then(|i| art.get(player, i, Variant::Plain, ArtUrgency::Visible, phys))
}

/// The art layout: the cover or the lyrics on the left, the info column on the right.
#[allow(clippy::too_many_arguments)]
fn side(
    ui: &mut Ui<'_>,
    rect: Rect,
    look: &Look,
    state: &PlayerState,
    m: Side,
    body: Body,
    shown: Option<&Lyrics>,
    cover_art: Option<Art>,
    (art, likes, player): (&mut ArtCache, &mut LikeBook, &PlayerHandle),
) {
    let Some(now) = &state.current else {
        return;
    };
    let track = &now.track;
    let scale = ui.painter().s;
    let decor = look.decor();
    let rad = look.radii();
    let p = ui.painter();
    p.on(LayerId::Popup);

    let medium = decor.display_medium;
    let mut title = wrap_title(&track.name, m.rw, |s| width(p, s, SIDE_TITLE_SIZE, medium));
    title.truncate(2);
    let album = album_line(track);
    let next = next_up(state);
    let o = SideOffsets::fit(
        decor,
        (rect.h - 2.0 * EDGE).max(0.0),
        title.len(),
        album.is_some(),
        next.is_some(),
    );
    // The group is centred by what the column really holds (v1 feedback: it sat left in a wide
    // window).
    let mut content_w = width(p, &track.artist, 24.0, false);
    for line in &title {
        content_w = content_w.max(width(p, line, SIDE_TITLE_SIZE, medium));
    }
    if let Some(line) = &album {
        content_w = content_w.max(width(p, line, 15.0, false));
    }
    if let (Some(_), Some(next)) = (o.next_label, next) {
        content_w = content_w.max(next_card_width(p, look, next));
    }
    let m = m.shifted(centre_dx(rect, &m, content_w));

    if let (Body::Art, Some(a)) = (body, cover_art) {
        if decor.art_glow {
            glow(p, m.art, rad.art, 70.0, look.a, 0.55);
        }
        shadow(p, m.art, rad.art, 30.0, 0.9);
        cover(p, look, m.art, rad.art, Some(a), NoArt::Thumb);
        if look.surface().bevel {
            p.ring(m.art, rad.art, 1.0, alpha(look.ink.light, 0x18), false);
        }
    }
    let y0 = side_top(rect, m.cy, o.height);
    let rx = m.rx;

    label_at(p, look, "NOW PLAYING", rx, y0, true);
    for (i, line) in title.iter().enumerate() {
        let y = y0 + o.title + SIDE_TITLE_PITCH * i as f32;
        let (c, w) = (look.ink.text, m.rw);
        text(p, line, SIDE_TITLE_SIZE, c, medium, rx - 3.0, y, w);
    }
    let (hi, w) = (look.hi_or_text(), m.rw);
    text(p, &track.artist, 24.0, hi, false, rx, y0 + o.artist, w);
    if let (Some(y), Some(line)) = (o.album, &album) {
        text(
            p,
            line,
            15.0,
            idle_ink(look.ink.muted, look.ink.text),
            false,
            rx,
            y0 + y,
            m.rw,
        );
    }
    if let Some(r) = o.rule {
        rule_at(p, look, rx, y0 + r);
    }
    if let (Some(nl), Some(next)) = (o.next_label, next) {
        label_at(p, look, "NEXT UP", rx, y0 + nl, false);
        let thumb = next_thumb(art, player, next, scale);
        next_card(ui.painter(), look, rx, y0 + nl + 22.0, next, thumb);
    }

    // Theme A's action row: the heart alone (MD75).
    // Drawn only when the provider has likes at all (as the media control's and the rows').
    if let (Some(r), Rule::Actions) = (o.rule, decor.rule)
        && let Some(entry) = state
            .providers
            .iter()
            .find(|e| e.info.id == now.provider && e.info.caps.likes)
    {
        let c = (rx + 12.0, y0 + r + 12.0);
        let hit = Rect::new(c.0 - 14.0, c.1 - 14.0, 28.0, 28.0);
        let writable = entry.likes_writable;
        let liked = likes.state(&track.uri);
        if likes::heart(ui, "idle-heart", hit, look, liked, writable, true) {
            likes.click(&track.uri, writable, player);
        }
    }

    if let (Body::Lyrics, Some(lyrics)) = (body, shown) {
        let position = u64::from(state.position_now());
        lyric_view::lyrics(
            ui,
            m.lyrics,
            m.cy,
            look,
            lyrics,
            position,
            track.duration_ms,
        );
        if state.play_state == PlayState::Playing
            && let Some(after) = lyric_view::repaint_in(lyrics, position)
        {
            let at = ui.time() + after;
            ui.request_repaint_at(at);
        }
    }
}

/// M5's centred block: no art square, no lyrics region.
fn centred(
    ui: &mut Ui<'_>,
    rect: Rect,
    look: &Look,
    state: &PlayerState,
    track: &Track,
    art: &mut ArtCache,
    player: &PlayerHandle,
) {
    let scale = ui.painter().s;
    let p = ui.painter();
    p.on(LayerId::Popup);
    let decor = look.decor();
    let cx = rect.x + rect.w / 2.0;
    let limit = (rect.w - 2.0 * SIDE).max(160.0);
    let next = next_up(state);
    let medium = decor.display_medium;

    let title = wrap_title(&track.name, limit, |s| width(p, s, TITLE_SIZE, medium));
    let album = !track.album.trim().is_empty();
    let plan = Plan::new(rect, decor, title.len(), album, next.is_some());
    let y0 = plan.y0;
    let o = plan.offsets;

    label(p, look, "NOW PLAYING", cx, y0, true);
    for (i, line) in title.iter().enumerate() {
        let y = y0 + o.title + TITLE_PITCH * i as f32;
        text_c(p, line, TITLE_SIZE, look.ink.text, medium, cx, y, limit);
    }
    let artist = y0 + o.artist;
    text_c(
        p,
        &track.artist,
        24.0,
        look.hi_or_text(),
        false,
        cx,
        artist,
        limit,
    );
    if album {
        let y = y0 + o.album;
        text_c(
            p,
            &track.album,
            15.0,
            idle_ink(look.ink.muted, look.ink.text),
            false,
            cx,
            y,
            limit,
        );
    }
    if let Some(r) = o.rule {
        rule(p, look, cx, y0 + r);
    }
    if let (Some(nl), Some(next)) = (o.next_label, next) {
        label(p, look, "NEXT UP", cx, y0 + nl, false);
        let thumb = next_thumb(art, player, next, scale);
        let w = next_card_width(p, look, next);
        let x = (cx - w / 2.0).round();
        next_card(p, look, x, y0 + nl + 24.0, next, thumb);
    }
}

/// Where each element of the block sits, as offsets from the block's top; `None` for what is not
/// drawn. `height` is the bottom of the last drawn element.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Offsets {
    title: f32,
    artist: f32,
    album: f32,
    rule: Option<f32>,
    next_label: Option<f32>,
    height: f32,
}

impl Offsets {
    fn new(d: &Decor, lines: usize, album: bool, rule: bool, next: bool) -> Offsets {
        let title = TITLE_DY;
        let artist = title + TITLE_PITCH * lines as f32 + 14.0;
        let album_y = artist + 38.0;
        // Without an album line everything below measures from the artist line.
        let line_end = if album { album_y } else { artist };
        let rule_y = (rule && d.rule != Rule::Actions).then_some(line_end + 46.0);
        let mut height = match rule_y {
            Some(r) => r + 30.0,
            None => line_end + if album { 18.0 } else { 29.0 },
        };
        let next_label = next.then(|| rule_y.map_or(line_end + 56.0, |r| r + 44.0));
        if let Some(nl) = next_label {
            let card = if d.labels == Labels::Tracked {
                6.0 + 52.0
            } else {
                CAPSULE_H
            };
            height = nl + 24.0 + card;
        }
        Offsets {
            title,
            artist,
            album: album_y,
            rule: rule_y,
            next_label,
            height,
        }
    }
}

/// The block's placement in `rect`.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Plan {
    y0: f32,
    offsets: Offsets,
}

impl Plan {
    /// Centres the block, `CREST_ROOM` above the rect's middle; in a window too small for it
    /// drops the next-up block, then the rule, then the title's second line, until it fits.
    fn new(rect: Rect, d: &Decor, lines: usize, album: bool, next: bool) -> Plan {
        let avail = (rect.h - 2.0 * EDGE).max(0.0);
        let tries = [
            (lines, true, next),
            (lines, true, false),
            (lines, false, false),
            (1, false, false),
        ];
        let mut o = Offsets::new(d, 1, album, false, false);
        for (l, r, n) in tries {
            o = Offsets::new(d, l, album, r, n);
            if o.height <= avail {
                break;
            }
        }
        let mid = (rect.y + (rect.h - CREST_ROOM) / 2.0).round();
        let lo = rect.y + EDGE;
        let hi = (rect.bottom() - EDGE - o.height).max(lo);
        Plan {
            y0: (mid - o.height / 2.0).clamp(lo, hi),
            offsets: o,
        }
    }
}

/// `title` in at most two lines of at most `limit` px (`measure` gives a string's width): one
/// line when it fits, else broken at a word boundary; the second line may still be too wide and
/// is elided when drawn. A first word wider than the limit leaves one line (elided when drawn).
fn wrap_title(title: &str, limit: f32, mut measure: impl FnMut(&str) -> f32) -> Vec<String> {
    let words: Vec<&str> = title.split_whitespace().collect();
    let whole = words.join(" ");
    if words.len() < 2 || measure(&whole) <= limit {
        return vec![whole];
    }
    let mut n = 0;
    let mut line = String::new();
    for (i, w) in words.iter().enumerate() {
        let tried = if line.is_empty() {
            (*w).to_string()
        } else {
            format!("{line} {w}")
        };
        if measure(&tried) > limit {
            break;
        }
        line = tried;
        n = i + 1;
    }
    if n == 0 || n == words.len() {
        return vec![whole];
    }
    vec![line, words[n..].join(" ")]
}

/// `s` centred on `cx` with its top at `y`, elided to `max_w`; returns its width.
#[allow(clippy::too_many_arguments)]
fn text_c(
    p: &mut P<'_>,
    s: &str,
    size: f32,
    color: Rgba,
    medium: bool,
    cx: f32,
    y: f32,
    max_w: f32,
) -> f32 {
    let w = fitted_width(p, s, size, medium, max_w);
    text(p, s, size, color, medium, cx - w / 2.0, y, max_w);
    w
}

/// The width `s` takes when drawn elided to `max_w`.
fn fitted_width(p: &mut P<'_>, s: &str, size: f32, medium: bool, max_w: f32) -> f32 {
    let mut style = Style::ui(size, [0xFF; 4]);
    if medium {
        style = style.medium();
    }
    let spec = p.elide(&Spec::plain(s, style), max_w);
    p.measure(&spec).0
}

/// The width of a section label in the look's treatment; the lead label of the marked style
/// carries the equaliser mark before its text.
fn label_width(p: &mut P<'_>, k: &Look, s: &str, lead: bool) -> f32 {
    match k.decor().labels {
        Labels::Marked if lead => 20.0 + width(p, s, 11.5, true),
        Labels::Marked | Labels::Plain => width(p, s, 11.5, true),
        Labels::Tracked => 16.0 + caps_width(p, s, 10.0, 2.4),
    }
}

/// A small section label centred on `cx` with its top at `y`; `lead` lights it (the first label
/// of a block), otherwise it is quiet.
fn label(p: &mut P<'_>, k: &Look, s: &str, cx: f32, y: f32, lead: bool) {
    let x = (cx - label_width(p, k, s, lead) / 2.0).round();
    label_at(p, k, s, x, y, lead);
}

/// A section label with its left edge at `x` and its top at `y`.
fn label_at(p: &mut P<'_>, k: &Look, s: &str, x: f32, y: f32, lead: bool) {
    match k.decor().labels {
        Labels::Marked => {
            if lead {
                for (i, h) in [7.0, 11.0, 5.0, 9.0].iter().enumerate() {
                    let bx = x + i as f32 * 3.6;
                    p.fill(Rect::new(bx, y + 13.0 - h, 2.2, *h), 1.0, k.a);
                }
                text(p, s, 11.5, k.a, true, x + 20.0, y, 0.0);
            } else {
                text(
                    p,
                    s,
                    11.5,
                    idle_ink(k.ink.faint, k.ink.text),
                    true,
                    x,
                    y,
                    0.0,
                );
            }
        }
        Labels::Tracked => {
            let c = if lead {
                k.hi
            } else {
                idle_ink(k.ink.muted, k.ink.text)
            };
            led(p, (x + 4.0, y + 7.0), 5.0, k.a, k.hi, lead);
            caps(p, s, 10.0, c, x + 16.0, y, 2.4);
        }
        Labels::Plain => {
            let c = if lead {
                k.hi
            } else {
                idle_ink(k.ink.faint, k.ink.text)
            };
            text(p, s, 11.5, c, true, x, y, 0.0);
        }
    }
}

/// The decor's rule under the song info, centred on `cx`, its box's top at `y`.
fn rule(p: &mut P<'_>, k: &Look, cx: f32, y: f32) {
    let w = match k.decor().rule {
        Rule::Actions => return,
        Rule::Ticks => 320.0,
        Rule::Gradient => 64.0,
    };
    rule_at(p, k, cx - w / 2.0, y);
}

/// The decor's rule with its left edge at `x0`: the hardware theme's tick ruler and the adaptive
/// theme's gradient line. The straight theme's rule is its action row (the heart, drawn by the
/// art layout), so there is none here.
fn rule_at(p: &mut P<'_>, k: &Look, x0: f32, y: f32) {
    match k.decor().rule {
        Rule::Actions => {}
        Rule::Ticks => {
            let w = 320.0;
            p.fill(
                Rect::new(x0, y + 8.0, w, 1.0),
                0.0,
                alpha(k.ink.light, 0x22),
            );
            for i in 0..=32 {
                let tx = x0 + w * i as f32 / 32.0;
                let th = if i % 4 == 0 { 6.0 } else { 3.0 };
                p.fill(
                    Rect::new(tx, y + 8.0 - th, 1.0, th),
                    0.0,
                    alpha(k.ink.light, 0x30),
                );
            }
        }
        Rule::Gradient => hgradient(p, Rect::new(x0, y + 8.0, 64.0, 2.0), k.a, k.b),
    }
}

/// The next-up card's width.
fn next_card_width(p: &mut P<'_>, k: &Look, next: &Track) -> f32 {
    let tw = fitted_width(p, &next.name, 14.5, true, NEXT_TEXT_MAX);
    let aw = fitted_width(p, &next.artist, 13.0, false, NEXT_TEXT_MAX);
    if k.decor().labels == Labels::Tracked {
        52.0 + 16.0 + tw.max(aw)
    } else {
        64.0 + tw.max(aw) + 28.0
    }
}

/// The next-up card with its left edge at `x` and its top at `y`: a capsule (a bare sleeve and
/// its lines in the hardware theme) with the cover (the no-art placeholder without it), the title
/// and the artist.
fn next_card(p: &mut P<'_>, k: &Look, x: f32, y: f32, next: &Track, thumb_art: Option<Art>) {
    let rad = k.radii();
    if k.decor().labels == Labels::Tracked {
        let sleeve = Rect::new(x, y + 6.0, 52.0, 52.0);
        shadow(p, sleeve, rad.thumb, 6.0, 0.8);
        cover(p, k, sleeve, rad.thumb, thumb_art, NoArt::Thumb);
        let tx = sleeve.right() + 16.0;
        let (ty, ay) = (sleeve.y + 7.0, sleeve.y + 28.0);
        text(p, &next.name, 14.5, k.ink.text, true, tx, ty, NEXT_TEXT_MAX);
        text(
            p,
            &next.artist,
            13.0,
            idle_ink(k.ink.muted, k.ink.text),
            false,
            tx,
            ay,
            NEXT_TEXT_MAX,
        );
    } else {
        let w = next_card_width(p, k, next);
        let cap = Rect::new(x, y, w, CAPSULE_H);
        let (fill, rim) = match k.decor().labels {
            Labels::Marked => ([0x00, 0x00, 0x00, 0x48], k.ink.line),
            _ => ([0xFF, 0xFF, 0xFF, 0x07], k.ink.line_soft),
        };
        outline(p, cap, (rad.card + 2.0).min(18.0), fill, rim);
        let thumb = Rect::new(cap.x + 10.0, cap.y + 10.0, 44.0, 44.0);
        cover(p, k, thumb, rad.thumb.min(10.0), thumb_art, NoArt::Thumb);
        let (tx, ty, ay) = (cap.x + 66.0, cap.y + 12.0, cap.y + 33.0);
        text(
            p,
            &next.name,
            14.5,
            idle_ink(k.ink.text2, k.ink.text),
            true,
            tx,
            ty,
            NEXT_TEXT_MAX,
        );
        text(
            p,
            &next.artist,
            13.0,
            idle_ink(k.ink.muted, k.ink.text),
            false,
            tx,
            ay,
            NEXT_TEXT_MAX,
        );
    }
}

/// An idle-view text colour `c` lifted toward the full text colour `full` and made nearly opaque,
/// so the quieter lines stay readable over any cover (v1 round 2).
pub(crate) fn idle_ink(c: Rgba, full: Rgba) -> Rgba {
    let t = IDLE_INK_LIFT;
    let m = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8;
    [
        m(c[0], full[0]),
        m(c[1], full[1]),
        m(c[2], full[2]),
        c[3].max(IDLE_INK_MIN_ALPHA),
    ]
}

/// `m:ss`, or `h:mm:ss` from one hour, of `ms` milliseconds.
pub fn format_time(ms: u64) -> String {
    let secs = ms / 1000;
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Instant;

    use super::*;
    use crate::theme::{ADAPTIVE, HIFI, TEAL, Theme};
    use whipspot_core::engine::BAND_COUNT;
    use whipspot_core::player::{
        EqState, LyricsState, OutputStatus, PlayState, QueueState, RepeatMode,
    };

    fn track(n: &str) -> Track {
        Track {
            uri: format!("t:{n}"),
            name: n.into(),
            artist: "Artist".into(),
            album: "Album".into(),
            duration_ms: 1000,
            images: vec![],
            year: None,
        }
    }

    fn state(next: Option<usize>) -> PlayerState {
        PlayerState {
            revision: 0,
            play_state: PlayState::Playing,
            current: None,
            position_ms: 0,
            position_at: Instant::now(),
            volume: 100,
            balance: 0,
            replay_gain: Default::default(),
            eq: EqState {
                enabled: false,
                preamp_db: 0.0,
                bands_db: [0.0; BAND_COUNT],
            },
            shuffle: false,
            repeat: RepeatMode::Off,
            queue: QueueState {
                items: Arc::new(vec![track("a"), track("b"), track("c")]),
                current: Some(0),
                next,
            },
            providers: Vec::new(),
            output_summary: String::new(),
            output: OutputStatus::default(),
            lyrics: LyricsState::Off,
            lyrics_enabled: false,
            remote: None,
        }
    }

    #[test]
    fn u1_next_up_is_the_queue_item_the_state_names() {
        assert_eq!(next_up(&state(Some(1))).map(|t| t.name.as_str()), Some("b"));
        assert_eq!(next_up(&state(None)), None);
        assert_eq!(next_up(&state(Some(3))), None);
    }

    const BIG: Rect = Rect {
        x: 0.0,
        y: 40.0,
        w: 1440.0,
        h: 752.0,
    };

    fn plan(t: &Theme, rect: Rect, lines: usize, album: bool, next: bool) -> Plan {
        Plan::new(rect, &t.decor, lines, album, next)
    }

    #[test]
    fn u2_block_is_centred_above_the_crest_per_theme() {
        for (t, h, y0) in [
            (&TEAL, 300.0, 216.0),
            (&HIFI, 328.0, 202.0),
            (&ADAPTIVE, 334.0, 199.0),
        ] {
            let p = plan(t, BIG, 1, true, true);
            assert_eq!(p.offsets.height, h, "{}", t.name);
            assert_eq!(p.y0, y0, "{}", t.name);
            // The block's centre is the rect's middle lifted by the crest room (366).
            let centre = p.y0 + p.offsets.height / 2.0;
            assert!((centre - 366.0).abs() <= 1.0, "{}", t.name);
            assert_eq!(p.offsets.title, 28.0);
            assert_eq!(p.offsets.artist, 118.0);
            assert_eq!(p.offsets.album, 156.0);
        }
    }

    #[test]
    fn u3_rule_follows_the_decor_and_a_has_none() {
        let o = |t| plan(t, BIG, 1, true, true).offsets;
        assert_eq!(o(&TEAL).rule, None);
        assert_eq!(o(&HIFI).rule, Some(202.0));
        assert_eq!(o(&ADAPTIVE).rule, Some(202.0));
        // A: NEXT UP follows the album at +56; B and C follow the rule at +44.
        assert_eq!(o(&TEAL).next_label, Some(212.0));
        assert_eq!(o(&HIFI).next_label, Some(246.0));
    }

    #[test]
    fn u4_no_next_ends_at_the_rule_or_the_album() {
        let c = plan(&ADAPTIVE, BIG, 1, true, false).offsets;
        assert_eq!((c.next_label, c.height), (None, 232.0));
        let a = plan(&TEAL, BIG, 1, true, false).offsets;
        assert_eq!((a.next_label, a.height), (None, 174.0));
    }

    #[test]
    fn u5_empty_album_closes_the_rhythm_up() {
        let c = plan(&ADAPTIVE, BIG, 1, false, true).offsets;
        assert_eq!(c.rule, Some(118.0 + 46.0));
        assert_eq!(c.next_label, Some(118.0 + 46.0 + 44.0));
    }

    #[test]
    fn u6_small_window_drops_next_then_rule_then_second_line() {
        let small = Rect::new(0.0, 40.0, 760.0, 332.0);
        // 332 - 32 = 300 available: A fits whole, B and C lose the next-up block.
        let a = plan(&TEAL, small, 1, true, true);
        assert!(a.offsets.next_label.is_some());
        for t in [&HIFI, &ADAPTIVE] {
            let p = plan(t, small, 1, true, true);
            assert_eq!(p.offsets.next_label, None, "{}", t.name);
            assert!(p.offsets.rule.is_some(), "{}", t.name);
        }
        let tiny = Rect::new(0.0, 40.0, 760.0, 200.0);
        let c = plan(&ADAPTIVE, tiny, 1, true, true);
        assert_eq!((c.offsets.next_label, c.offsets.rule), (None, None));
        // Two title lines go to one when even that does not fit with the rest.
        let c2 = plan(&ADAPTIVE, tiny, 2, true, true);
        assert_eq!(c2.offsets.artist, 28.0 + TITLE_PITCH + 14.0);
        for p in [a, c, c2] {
            assert!(p.y0 >= 40.0 + EDGE);
        }
    }

    fn with_lyrics(lyrics: LyricsState, enabled: bool) -> PlayerState {
        let mut s = state(None);
        s.lyrics = lyrics;
        s.lyrics_enabled = enabled;
        s
    }

    fn found(uri: &str, l: Lyrics) -> LyricsState {
        LyricsState::Found {
            uri: uri.into(),
            lyrics: Arc::new(l),
        }
    }

    /// The body the idle view picks for `state` (the current uri is "t:a") and a cover ready or not.
    fn body_of(state: &PlayerState, cover: bool) -> Body {
        Body::choose(lyrics_for(state, "t:a").is_some(), cover)
    }

    #[test]
    fn ly4_layout_choice_per_lyrics_state_with_and_without_a_cover() {
        use whipspot_core::lyrics::LyricLine;
        let synced = Lyrics::Synced(vec![LyricLine {
            at_ms: 0,
            text: "la".into(),
        }]);
        let plain = Lyrics::Plain(vec!["la".into()]);
        // Shown: synced lyrics found for this uri, lyrics on; with or without a cover.
        let s = with_lyrics(found("t:a", synced.clone()), true);
        assert_eq!(body_of(&s, false), Body::Lyrics);
        assert_eq!(body_of(&s, true), Body::Lyrics);
        // Not shown: the art when there is a cover, else the centred block.
        let not_shown = [
            with_lyrics(LyricsState::Off, true),
            with_lyrics(LyricsState::Idle, true),
            with_lyrics(LyricsState::Searching { uri: "t:a".into() }, true),
            with_lyrics(LyricsState::Failed { uri: "t:a".into() }, true),
            with_lyrics(found("t:a", Lyrics::Instrumental), true),
            with_lyrics(found("t:a", Lyrics::None), true),
            with_lyrics(found("t:a", Lyrics::Synced(vec![])), true),
            with_lyrics(found("t:a", Lyrics::Plain(vec![])), true),
            // Plain (unsynced) lyrics are not shown.
            with_lyrics(found("t:a", plain.clone()), true),
            // Lyrics of another track, and lyrics switched off.
            with_lyrics(found("t:other", plain.clone()), true),
            with_lyrics(found("t:a", plain), false),
            with_lyrics(found("t:a", synced), false),
        ];
        for s in &not_shown {
            assert_eq!(body_of(s, true), Body::Art, "{:?}", s.lyrics);
            assert_eq!(body_of(s, false), Body::Centred, "{:?}", s.lyrics);
        }
    }

    #[test]
    fn lyrics_for_shows_synced_lines_only() {
        use whipspot_core::lyrics::LyricLine;
        let plain = with_lyrics(found("t:a", Lyrics::Plain(vec!["a".into()])), true);
        assert!(lyrics_for(&plain, "t:a").is_none());
        let synced = with_lyrics(
            found(
                "t:a",
                Lyrics::Synced(vec![LyricLine {
                    at_ms: 0,
                    text: "a".into(),
                }]),
            ),
            true,
        );
        assert!(lyrics_for(&synced, "t:a").is_some());
    }

    #[test]
    fn side_layout_at_the_design_size_matches_round_3() {
        let m = Side::new(BIG);
        assert_eq!(m.cy, 416.0);
        assert_eq!(m.art, Rect::new(150.0, 196.0, 440.0, 440.0));
        assert_eq!(m.rx, 686.0);
        assert_eq!(m.rw, 1440.0 - 120.0 - 686.0);
        assert_eq!(m.lyrics, Rect::new(150.0, 40.0, 500.0, 552.0));
        // Full block: 330 tall, label at cy - 165.
        for t in [&TEAL, &HIFI, &ADAPTIVE] {
            let o = SideOffsets::new(&t.decor, 1, true, true, true);
            assert_eq!(o.height, 330.0, "{}", t.name);
            assert_eq!(side_top(BIG, m.cy, o.height), 251.0, "{}", t.name);
        }
        let o = SideOffsets::new(&ADAPTIVE.decor, 1, true, true, true);
        assert_eq!((o.title, o.artist, o.album), (26.0, 112.0, Some(150.0)));
        // A smaller window scales the art down and keeps the column on screen.
        let small = Side::new(Rect::new(0.0, 40.0, 760.0, 332.0));
        assert!(small.art.w < 440.0 && small.art.right() < small.rx);
        assert!(small.rx + small.rw <= 760.0 + MIN_COLUMN);
    }

    #[test]
    fn side_layout_centres_the_group_in_a_wide_window() {
        for w in [1920.0, 2000.0, 2560.0] {
            let rect = Rect::new(0.0, 40.0, w, 1040.0);
            let m = Side::new(rect);
            assert_eq!(m.rw, COLUMN_MAX, "{w}");
            assert_eq!(m.art.x, 150.0, "{w}");
        }
        // The shifted group has the same margin on both sides, whatever the column holds.
        let rect = Rect::new(0.0, 40.0, 2000.0, 1040.0);
        let m = Side::new(rect);
        for content in [300.0, 450.0, COLUMN_MAX] {
            let s = m.shifted(centre_dx(rect, &m, content));
            let (left, right) = (s.art.x, 2000.0 - (s.rx + content));
            assert!((left - right).abs() <= 1.0, "{content}: {left} vs {right}");
            assert!(s.lyrics.x == s.art.x && s.art.right() < s.rx);
        }
        // A column as wide as the room, or no room at all, never moves the group left.
        assert_eq!(centre_dx(BIG, &Side::new(BIG), 10_000.0), 0.0);
        assert!(centre_dx(BIG, &Side::new(BIG), 300.0) >= 0.0);
    }

    #[test]
    fn album_line_omits_a_missing_year() {
        let mut t = track("x");
        assert_eq!(album_line(&t).as_deref(), Some("Album"));
        t.year = Some(2019);
        assert_eq!(album_line(&t).as_deref(), Some("Album  \u{b7}  2019"));
        t.album = " ".into();
        assert_eq!(album_line(&t).as_deref(), Some("2019"));
        t.year = None;
        assert_eq!(album_line(&t), None);
    }

    // A measure of 10 px per character.
    fn m(s: &str) -> f32 {
        s.chars().count() as f32 * 10.0
    }

    #[test]
    fn u7_title_wraps_at_a_word_boundary_into_two_lines() {
        assert_eq!(wrap_title("Short one", 200.0, m), ["Short one"]);
        assert_eq!(
            wrap_title("alpha beta gamma delta", 120.0, m),
            ["alpha beta", "gamma delta"]
        );
        // The second line may stay too wide: the caller elides it.
        assert_eq!(
            wrap_title("aa bb cc dd ee ff gg hh", 60.0, m),
            ["aa bb", "cc dd ee ff gg hh"]
        );
        // A first word wider than the limit leaves one line.
        assert_eq!(
            wrap_title("enormousword x y", 50.0, m),
            ["enormousword x y"]
        );
        assert_eq!(wrap_title("", 50.0, m), [""]);
    }
}
