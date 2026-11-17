//! A legible synthetic skin for the demo's panel dumps (D12, hard rule 4).
//!
//! [`demo_skin`] builds an archive in memory: no file is read and no asset is committed. Every
//! sprite rectangle of the geometry tables is drawn in a flat colour with a one pixel border, so a
//! dump can be judged by eye, and the archive goes through the real engine, so a wrong number in
//! `geometry` shows up as a misplaced or missing piece. The art is original: a dark slate and teal
//! scheme, labels in a hand-made 3x5 pixel font, a seven-segment digit sheet, and a slider frame
//! set graded by its index (the fill grows with the frame number).

use whipspot_core::skin::Rect;

use crate::geometry as g;
use whipspot_skin_kit::synth::{bmp_rgb, zip_deflated};

type Colour = [u8; 3];

const PANEL: Colour = [30, 36, 48];
const FRAME: Colour = [42, 52, 74];
const EDGE: Colour = [96, 116, 150];
const EDGE_DIM: Colour = [62, 76, 102];
const DISPLAY: Colour = [12, 16, 24];
const INK: Colour = [120, 232, 200];
const INK_DIM: Colour = [58, 112, 104];
const MARK: Colour = [220, 230, 245];
const BUTTON: Colour = [58, 72, 100];
const BUTTON_PRESSED: Colour = [22, 28, 42];
const ON: Colour = [214, 160, 48];
const ON_PRESSED: Colour = [150, 108, 24];
const ON_INK: Colour = [24, 18, 8];
const TITLE_ACTIVE: Colour = [36, 112, 132];
const TITLE_INACTIVE: Colour = [48, 54, 66];
const SEGMENT: Colour = [90, 240, 160];
const SEGMENT_OFF: Colour = [20, 38, 40];
const CUT: Colour = [0, 0, 0];

const fn r(x: u32, y: u32, w: u32, h: u32) -> Rect {
    Rect { x, y, w, h }
}

/// Blends `a` to `b`: `num` of `den`.
fn lerp(a: Colour, b: Colour, num: u32, den: u32) -> Colour {
    let den = den.max(1);
    let mix = |x: u8, y: u8| {
        let (x, y) = (u32::from(x), u32::from(y));
        ((x * (den - num.min(den)) + y * num.min(den)) / den) as u8
    };
    [mix(a[0], b[0]), mix(a[1], b[1]), mix(a[2], b[2])]
}

// ---------------------------------------------------------------------------------------------
// A pixel canvas
// ---------------------------------------------------------------------------------------------

struct Canvas {
    w: u32,
    h: u32,
    px: Vec<Colour>,
}

impl Canvas {
    fn new(w: u32, h: u32, fill: Colour) -> Canvas {
        Canvas {
            w,
            h,
            px: vec![fill; (w * h) as usize],
        }
    }

    fn set(&mut self, x: u32, y: u32, c: Colour) {
        if x < self.w && y < self.h {
            self.px[(y * self.w + x) as usize] = c;
        }
    }

    fn fill(&mut self, rect: Rect, c: Colour) {
        for y in rect.y..rect.y + rect.h {
            for x in rect.x..rect.x + rect.w {
                self.set(x, y, c);
            }
        }
    }

    /// `rect` in `fill` with a one pixel border.
    fn frame(&mut self, rect: Rect, fill: Colour, edge: Colour) {
        self.fill(rect, edge);
        if rect.w > 2 && rect.h > 2 {
            self.fill(r(rect.x + 1, rect.y + 1, rect.w - 2, rect.h - 2), fill);
        }
    }

    /// Border lines on the chosen sides of `rect`.
    fn edges(&mut self, rect: Rect, sides: [bool; 4], c: Colour) {
        let [top, bottom, left, right] = sides;
        for i in 0..rect.w {
            if top {
                self.set(rect.x + i, rect.y, c);
            }
            if bottom {
                self.set(rect.x + i, rect.y + rect.h - 1, c);
            }
        }
        for i in 0..rect.h {
            if left {
                self.set(rect.x, rect.y + i, c);
            }
            if right {
                self.set(rect.x + rect.w - 1, rect.y + i, c);
            }
        }
    }

    /// Draws `art` (`#` is a pixel) with its top-left at (x, y).
    fn art(&mut self, x: u32, y: u32, art: &[&str], c: Colour) {
        for (dy, row) in art.iter().enumerate() {
            for (dx, ch) in row.bytes().enumerate() {
                if ch == b'#' {
                    self.set(x + dx as u32, y + dy as u32, c);
                }
            }
        }
    }

    /// `text` in the 3x5 font, four pixels per character, top-left at (x, y).
    fn text(&mut self, x: u32, y: u32, text: &str, c: Colour) {
        for (i, ch) in text.chars().enumerate() {
            if let Some(pattern) = glyph(ch) {
                let rows: Vec<&str> = pattern.split('/').collect();
                self.art(x + i as u32 * 4, y, &rows, c);
            }
        }
    }

    fn bmp(&self) -> Vec<u8> {
        let rows: Vec<Vec<Colour>> = self
            .px
            .chunks(self.w as usize)
            .map(<[Colour]>::to_vec)
            .collect();
        bmp_rgb(24, &rows, false)
    }
}

// ---------------------------------------------------------------------------------------------
// The font: 3x5 glyphs, rows separated by '/'
// ---------------------------------------------------------------------------------------------

fn glyph(c: char) -> Option<&'static str> {
    Some(match c.to_ascii_lowercase() {
        'a' => ".#./#.#/###/#.#/#.#",
        'b' => "##./#.#/##./#.#/##.",
        'c' => ".##/#../#../#../.##",
        'd' => "##./#.#/#.#/#.#/##.",
        'e' => "###/#../##./#../###",
        'f' => "###/#../##./#../#..",
        'g' => ".##/#../#.#/#.#/.##",
        'h' => "#.#/#.#/###/#.#/#.#",
        'i' => "###/.#./.#./.#./###",
        'j' => "..#/..#/..#/#.#/.#.",
        'k' => "#.#/#.#/##./#.#/#.#",
        'l' => "#../#../#../#../###",
        'm' => "#.#/###/###/#.#/#.#",
        'n' => "##./#.#/#.#/#.#/#.#",
        'o' => ".#./#.#/#.#/#.#/.#.",
        'p' => "##./#.#/##./#../#..",
        'q' => ".#./#.#/#.#/##./.##",
        'r' => "##./#.#/##./#.#/#.#",
        's' => ".##/#../.#./..#/##.",
        't' => "###/.#./.#./.#./.#.",
        'u' => "#.#/#.#/#.#/#.#/###",
        'v' => "#.#/#.#/#.#/#.#/.#.",
        'w' => "#.#/#.#/###/###/#.#",
        'x' => "#.#/#.#/.#./#.#/#.#",
        'y' => "#.#/#.#/.#./.#./.#.",
        'z' => "###/..#/.#./#../###",
        '0' => "###/#.#/#.#/#.#/###",
        '1' => ".#./##./.#./.#./###",
        '2' => "##./..#/.#./#../###",
        '3' => "##./..#/.#./..#/##.",
        '4' => "#.#/#.#/###/..#/..#",
        '5' => "###/#../##./..#/##.",
        '6' => ".##/#../###/#.#/###",
        '7' => "###/..#/.#./.#./.#.",
        '8' => "###/#.#/###/#.#/###",
        '9' => "###/#.#/###/..#/##.",
        '.' => ".../.../.../.../.#.",
        ':' => ".../.#./.../.#./...",
        '(' => "..#/.#./.#./.#./..#",
        ')' => "#../.#./.#./.#./#..",
        '-' => ".../.../###/.../...",
        '\'' => ".#./.#./.../.../...",
        '!' => ".#./.#./.#./.../.#.",
        '_' => ".../.../.../.../###",
        '+' => ".../.#./###/.#./...",
        '/' => "..#/..#/.#./#../#..",
        '\\' => "#../#../.#./..#/..#",
        '[' => "##./#../#../#../##.",
        ']' => ".##/..#/..#/..#/.##",
        ',' => ".../.../.../.#./#..",
        '=' => ".../###/.../###/...",
        '%' => "#.#/..#/.#./#../#.#",
        '?' => "##./..#/.#./.../.#.",
        '*' => "#.#/.#./###/.#./#.#",
        '#' => "#.#/###/#.#/###/#.#",
        '"' => "#.#/#.#/.../.../...",
        '^' => ".#./#.#/.../.../...",
        _ => return None,
    })
}

/// text.bmp: 5x6 cells (the glyph sits at 1,0 in its cell) on the text background.
fn text_sheet() -> Canvas {
    let mut c = Canvas::new(155, 18, DISPLAY);
    for &(ch, row, col) in g::TEXT_GLYPHS {
        if let Some(pattern) = glyph(ch) {
            let rows: Vec<&str> = pattern.split('/').collect();
            c.art(col * g::TEXT_CELL_W + 1, row * g::TEXT_CELL_H, &rows, INK);
        }
    }
    c
}

/// nums_ex.bmp: seven-segment digits 0-9 in 9x13 cells, a blank at 90 and a minus at 99.
fn numbers_sheet() -> Canvas {
    // Segments a, b, c, d, e, f, g as rectangles in a 9x13 cell.
    let segments = [
        r(1, 0, 7, 2),
        r(7, 1, 2, 6),
        r(7, 6, 2, 6),
        r(1, 11, 7, 2),
        r(0, 6, 2, 6),
        r(0, 1, 2, 6),
        r(1, 5, 7, 3),
    ];
    let digits: [&str; 10] = [
        "abcdef", "bc", "abdeg", "abcdg", "bcfg", "acdfg", "acdefg", "abc", "abcdefg", "abcdfg",
    ];
    let mut c = Canvas::new(108, 13, DISPLAY);
    let cell = |c: &mut Canvas, x0: u32, lit: &str| {
        for (i, seg) in segments.iter().enumerate() {
            let on = lit.contains(char::from(b'a' + i as u8));
            let at = r(x0 + seg.x, seg.y, seg.w, seg.h);
            c.fill(at, if on { SEGMENT } else { SEGMENT_OFF });
        }
    };
    for (d, lit) in digits.iter().enumerate() {
        cell(&mut c, d as u32 * g::NUMBERS_CELL_W, lit);
    }
    // The blank shows every segment dim; the minus lights the middle one.
    cell(&mut c, 90, "");
    cell(&mut c, 99, "g");
    c
}

// ---------------------------------------------------------------------------------------------
// Buttons
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum Label {
    Text(&'static str),
    Art(&'static [&'static str]),
}

const PLAY: &[&str] = &[
    "#.....", "###...", "#####.", "######", "#####.", "###...", "#.....",
];
const PAUSE: &[&str] = &[
    "##..##", "##..##", "##..##", "##..##", "##..##", "##..##", "##..##",
];
const STOP: &[&str] = &["######", "######", "######", "######", "######", "######"];
const PREVIOUS: &[&str] = &[
    "#....#", "#...##", "#..###", "#.####", "#..###", "#...##", "#....#",
];
const NEXT: &[&str] = &[
    "#....#", "##...#", "###..#", "####.#", "###..#", "##...#", "#....#",
];
const EJECT: &[&str] = &["..##..", ".####.", "######", "......", "######", "######"];
const MINIMISE: &[&str] = &["#####", "#####"];
const CLOSE: &[&str] = &["#...#", ".#.#.", "..#..", ".#.#.", "#...#"];
const STOPPED: &[&str] = &["#####", "#####", "#####", "#####", "#####"];

impl Label {
    fn draw(self, c: &mut Canvas, rect: Rect, nudge: u32, ink: Colour) {
        let (w, h, art) = match self {
            Label::Text(t) => (t.chars().count() as u32 * 4 - 1, 5, None),
            Label::Art(a) => (a[0].len() as u32, a.len() as u32, Some(a)),
        };
        let x = rect.x + rect.w.saturating_sub(w) / 2 + nudge;
        let y = rect.y + rect.h.saturating_sub(h) / 2 + nudge;
        match (self, art) {
            (Label::Text(t), _) => c.text(x, y, t, ink),
            (_, Some(a)) => c.art(x, y, a, ink),
            _ => {}
        }
    }
}

/// One button sprite: pressed ones are darker and their label moves a pixel; on ones are amber.
fn face(c: &mut Canvas, rect: Rect, on: bool, pressed: bool, label: Label) {
    let fill = match (on, pressed) {
        (false, false) => BUTTON,
        (false, true) => BUTTON_PRESSED,
        (true, false) => ON,
        (true, true) => ON_PRESSED,
    };
    c.frame(rect, fill, if pressed { EDGE_DIM } else { EDGE });
    let ink = if on { ON_INK } else { MARK };
    label.draw(c, rect, u32::from(pressed), ink);
}

/// `[normal, pressed]`.
fn pair(c: &mut Canvas, rects: [Rect; 2], label: Label) {
    face(c, rects[0], false, false, label);
    face(c, rects[1], false, true, label);
}

/// `[off normal, off pressed, on normal, on pressed]`.
fn quad(c: &mut Canvas, rects: [Rect; 4], label: Label) {
    for (rect, (on, pressed)) in
        rects
            .into_iter()
            .zip([(false, false), (false, true), (true, false), (true, true)])
    {
        face(c, rect, on, pressed, label);
    }
}

/// A title bar sprite with its caption.
fn bar(c: &mut Canvas, rect: Rect, active: bool, caption: &str) {
    let fill = if active { TITLE_ACTIVE } else { TITLE_INACTIVE };
    c.frame(rect, fill, EDGE_DIM);
    c.text(rect.x + 8, rect.y + 4, caption, MARK);
}

// ---------------------------------------------------------------------------------------------
// The sheets
// ---------------------------------------------------------------------------------------------

fn main_sheet() -> Canvas {
    let mut c = Canvas::new(g::WINDOW_W, g::WINDOW_H, PANEL);
    c.edges(r(0, 0, g::WINDOW_W, g::WINDOW_H), [true; 4], EDGE);
    // The elapsed time, a colon between minutes and seconds, the state light.
    c.frame(r(44, 23, 60, 18), DISPLAY, EDGE_DIM);
    c.fill(r(73, 29, 2, 2), SEGMENT);
    c.fill(r(73, 35, 2, 2), SEGMENT);
    c.frame(r(24, 26, 13, 13), DISPLAY, EDGE_DIM);
    // The title line, the bitrate and the sample rate.
    c.frame(r(108, 21, 162, 11), DISPLAY, EDGE_DIM);
    c.frame(r(108, 41, 74, 10), DISPLAY, EDGE_DIM);
    c.text(128, 43, "kbps", INK_DIM);
    c.text(168, 43, "khz", INK_DIM);
    // The visualiser.
    c.frame(r(22, 41, 80, 20), DISPLAY, EDGE_DIM);
    // The stereo and mono lights sit in their own wells.
    c.frame(r(210, 39, 60, 16), DISPLAY, EDGE_DIM);
    // The shaped window loses this corner.
    c.fill(r(0, 104, 12, 12), CUT);
    c
}

fn titlebar_sheet() -> Canvas {
    let mut c = Canvas::new(344, 87, PANEL);
    for (rect, active) in g::TITLEBAR_BAR.into_iter().zip([false, true]) {
        bar(&mut c, rect, active, "demo skin");
    }
    pair(&mut c, g::TITLEBAR_MINIMIZE, Label::Art(MINIMISE));
    pair(&mut c, g::TITLEBAR_CLOSE, Label::Art(CLOSE));
    c
}

fn cbuttons_sheet() -> Canvas {
    let mut c = Canvas::new(136, 36, PANEL);
    pair(&mut c, g::CBUTTONS_PREVIOUS, Label::Art(PREVIOUS));
    pair(&mut c, g::CBUTTONS_PLAY, Label::Art(PLAY));
    pair(&mut c, g::CBUTTONS_PAUSE, Label::Art(PAUSE));
    pair(&mut c, g::CBUTTONS_STOP, Label::Art(STOP));
    pair(&mut c, g::CBUTTONS_NEXT, Label::Art(NEXT));
    // The eject sprites overlap the next button's last column; they are drawn after it.
    pair(&mut c, g::CBUTTONS_EJECT, Label::Art(EJECT));
    c
}

fn shufrep_sheet() -> Canvas {
    let mut c = Canvas::new(92, 85, PANEL);
    quad(&mut c, g::SHUFREP_SHUFFLE, Label::Text("shuffle"));
    quad(&mut c, g::SHUFREP_REPEAT, Label::Text("rep"));
    quad(&mut c, g::SHUFREP_EQ, Label::Text("eq"));
    quad(&mut c, g::SHUFREP_PLAYLIST, Label::Text("pl"));
    c
}

fn posbar_sheet() -> Canvas {
    let mut c = Canvas::new(307, 10, PANEL);
    c.frame(g::POSBAR_BACKGROUND, DISPLAY, EDGE_DIM);
    c.fill(r(2, 4, 244, 2), EDGE_DIM);
    for (rect, fill) in g::POSBAR_THUMB.into_iter().zip([ON, ON_PRESSED]) {
        c.frame(rect, fill, EDGE);
        c.fill(r(rect.x + 13, rect.y + 2, 3, 6), ON_INK);
    }
    c
}

/// The thumb sprites of the volume and balance sheets, `[normal, pressed]`.
fn thumbs(c: &mut Canvas, rects: [Rect; 2]) {
    for (rect, fill) in rects.into_iter().zip([BUTTON, BUTTON_PRESSED]) {
        c.frame(rect, fill, EDGE);
        c.fill(r(rect.x + 6, rect.y + 2, 2, rect.h - 4), MARK);
    }
}

fn volume_sheet() -> Canvas {
    let mut c = Canvas::new(68, 433, PANEL);
    for i in 0..g::SLIDER_FRAMES {
        let rect = g::volume_frame(i);
        c.frame(rect, DISPLAY, EDGE_DIM);
        // Frame 0 is silence, frame 27 is full: the fill grows with the index.
        let fill = (rect.w - 2) * i / (g::SLIDER_FRAMES - 1);
        c.fill(
            r(rect.x + 1, rect.y + 1, fill, rect.h - 2),
            lerp([40, 90, 200], [60, 220, 120], i, g::SLIDER_FRAMES - 1),
        );
    }
    thumbs(&mut c, g::VOLUME_THUMB);
    c
}

fn balance_sheet() -> Canvas {
    let mut c = Canvas::new(68, 433, PANEL);
    for i in 0..g::SLIDER_FRAMES {
        let rect = g::balance_frame(i);
        c.frame(rect, DISPLAY, EDGE_DIM);
        // Frame 0 is the centre; the fill grows outwards on both sides.
        let half = (rect.w - 2) / 2;
        let reach = half * i / (g::SLIDER_FRAMES - 1);
        let mid = rect.x + rect.w / 2;
        c.fill(
            r(mid - reach, rect.y + 1, reach * 2, rect.h - 2),
            lerp([200, 150, 40], [230, 90, 60], i, g::SLIDER_FRAMES - 1),
        );
        c.fill(r(mid - 1, rect.y + 1, 2, rect.h - 2), MARK);
    }
    thumbs(&mut c, g::BALANCE_THUMB);
    c
}

fn monoster_sheet() -> Canvas {
    let mut c = Canvas::new(56, 24, PANEL);
    for (rects, label) in [(g::MONOSTER_STEREO, "stereo"), (g::MONOSTER_MONO, "mono")] {
        for (rect, on) in rects.into_iter().zip([false, true]) {
            c.frame(rect, DISPLAY, EDGE_DIM);
            c.text(
                rect.x + 2,
                rect.y + 3,
                label,
                if on { SEGMENT } else { SEGMENT_OFF },
            );
        }
    }
    c
}

fn playpaus_sheet() -> Canvas {
    let mut c = Canvas::new(42, 9, PANEL);
    // `PLAYPAUS_STATES` is [stopped, playing, paused].
    let states: [&[&str]; 3] = [STOPPED, PLAY, PAUSE];
    for (rect, art) in g::PLAYPAUS_STATES.into_iter().zip(states) {
        c.frame(rect, DISPLAY, DISPLAY);
        let x = rect.x + (rect.w - art[0].len() as u32) / 2;
        let y = rect.y + (rect.h - art.len() as u32) / 2;
        c.art(x, y, art, SEGMENT);
    }
    c
}

fn eqmain_sheet() -> Canvas {
    let mut c = Canvas::new(275, 315, PANEL);
    // The window background: wells behind the preamp and the ten bands, the graph and the labels.
    c.edges(g::EQ_BACKGROUND, [true; 4], EDGE);
    c.frame(r(84, 15, 117, 21), DISPLAY, EDGE_DIM);
    c.frame(r(19, 36, 18, 67), DISPLAY, EDGE_DIM);
    c.text(21, 105, "pre", INK_DIM);
    for b in 0..10u32 {
        let rect = g::eq_band_rect(b);
        c.frame(
            r(rect.x - 2, rect.y - 2, rect.w + 4, rect.h + 4),
            DISPLAY,
            EDGE_DIM,
        );
        c.text(rect.x + 2, 105, &(b + 1).to_string(), INK_DIM);
    }
    bar(&mut c, g::EQ_TITLE_BAR[0], false, "equalizer");
    bar(&mut c, g::EQ_TITLE_BAR[1], true, "equalizer");
    pair(&mut c, g::EQ_CLOSE_SPRITES, Label::Art(CLOSE));
    quad(&mut c, g::EQ_ON_SPRITES, Label::Text("on"));
    c.frame(g::EQ_GRAPH_BACKGROUND, DISPLAY, DISPLAY);
    let mid = g::EQ_GRAPH_BACKGROUND.y + g::EQ_GRAPH_BACKGROUND.h / 2;
    c.fill(
        r(g::EQ_GRAPH_BACKGROUND.x, mid, g::EQ_GRAPH_BACKGROUND.w, 1),
        EDGE_DIM,
    );
    // The curve's colour strip: warm at the top (boost), cool at the bottom (cut).
    for y in 0..g::EQ_GRAPH_COLOURS.h {
        c.set(
            g::EQ_GRAPH_COLOURS.x,
            g::EQ_GRAPH_COLOURS.y + y,
            lerp([250, 200, 60], [60, 200, 160], y, g::EQ_GRAPH_COLOURS.h - 1),
        );
    }
    // 28 vertical frames: the fill rises with the index (frame 0 is -12 dB, 27 is +12 dB).
    for i in 0..g::SLIDER_FRAMES {
        let rect = g::eq_band_frame(i);
        c.frame(rect, DISPLAY, EDGE_DIM);
        let rise = (rect.h - 2) * i / (g::SLIDER_FRAMES - 1);
        c.fill(
            r(rect.x + 4, rect.y + rect.h - 1 - rise, rect.w - 8, rise),
            lerp([60, 200, 160], [250, 200, 60], i, g::SLIDER_FRAMES - 1),
        );
        c.fill(r(rect.x + 1, rect.y + rect.h / 2, rect.w - 2, 1), EDGE);
    }
    for (rect, fill) in g::EQ_THUMB.into_iter().zip([BUTTON, BUTTON_PRESSED]) {
        c.frame(rect, fill, EDGE);
        c.fill(r(rect.x + 2, rect.y + 5, rect.w - 4, 1), MARK);
    }
    c
}

/// A strip along the top or bottom of a frame: tiles sideways (edges above and below).
fn tile_h(c: &mut Canvas, rect: Rect) {
    c.fill(rect, FRAME);
    c.edges(rect, [true, true, false, false], EDGE);
}

/// A strip down a side: tiles vertically (edges left and right).
fn tile_v(c: &mut Canvas, rect: Rect, left: bool) {
    c.fill(rect, FRAME);
    c.edges(rect, [false, false, left, !left], EDGE);
}

/// A corner piece: every edge but the one that joins its neighbour.
fn corner(c: &mut Canvas, rect: Rect, left: bool) {
    c.fill(rect, FRAME);
    c.edges(rect, [true, true, left, !left], EDGE);
}

fn pledit_sheet() -> Canvas {
    let mut c = Canvas::new(280, 186, PANEL);
    tile_h(&mut c, g::PLEDIT_TOP_TILE);
    corner(&mut c, g::PLEDIT_TOP_LEFT, true);
    bar(&mut c, g::PLEDIT_TITLE, true, "playlist");
    corner(&mut c, g::PLEDIT_TOP_RIGHT, false);
    // The close button's normal look is the part of the corner it covers.
    face(
        &mut c,
        g::PLEDIT_CLOSE_NORMAL,
        false,
        false,
        Label::Art(CLOSE),
    );
    tile_v(&mut c, g::PLEDIT_LEFT_TILE, true);
    tile_v(&mut c, g::PLEDIT_RIGHT_TILE, false);
    tile_h(&mut c, g::PLEDIT_BOTTOM_TILE);
    corner(&mut c, g::PLEDIT_BOTTOM_LEFT, true);
    corner(&mut c, g::PLEDIT_BOTTOM_RIGHT, false);
    face(
        &mut c,
        g::PLEDIT_CLOSE_PRESSED,
        false,
        true,
        Label::Art(CLOSE),
    );
    thumbs(&mut c, g::PLEDIT_SCROLL_THUMB);
    c
}

fn gen_sheet() -> Canvas {
    let mut c = Canvas::new(194, 109, PANEL);
    corner(&mut c, g::GEN_TOP_LEFT, true);
    c.text(4, 7, "lib", MARK);
    tile_h(&mut c, g::GEN_TOP_FILL);
    corner(&mut c, g::GEN_TOP_RIGHT, false);
    face(&mut c, g::GEN_CLOSE_NORMAL, false, false, Label::Art(CLOSE));
    tile_v(&mut c, g::GEN_MIDDLE_LEFT, true);
    tile_v(&mut c, g::GEN_MIDDLE_RIGHT, false);
    tile_h(&mut c, g::GEN_BOTTOM_FILL);
    corner(&mut c, g::GEN_BOTTOM_LEFT, true);
    corner(&mut c, g::GEN_BOTTOM_RIGHT, false);
    face(&mut c, g::GEN_CLOSE_PRESSED, false, true, Label::Art(CLOSE));
    c
}

// ---------------------------------------------------------------------------------------------
// The text files
// ---------------------------------------------------------------------------------------------

/// 24 lines: background, grid, 16 bars (top colour first), 5 scope, peak.
fn viscolor() -> String {
    let mut lines: Vec<(Colour, &str)> = vec![([10, 14, 22], "background"), ([28, 36, 52], "grid")];
    for i in 0..16u32 {
        // Red at the top of a bar, through amber, to green at the foot.
        let c = if i < 8 {
            lerp([250, 84, 64], [250, 200, 60], i, 7)
        } else {
            lerp([250, 200, 60], [44, 200, 120], i - 8, 7)
        };
        lines.push((c, "bar"));
    }
    for i in 0..5u32 {
        lines.push((lerp([60, 200, 230], [200, 240, 250], i, 4), "scope"));
    }
    lines.push(([235, 240, 250], "peak"));
    lines
        .iter()
        .map(|(c, label)| format!("{},{},{} // {label}\r\n", c[0], c[1], c[2]))
        .collect()
}

const PLEDIT_TXT: &str = "[Text]\r\nNormal=#78E8C8\r\nCurrent=#FFFFFF\r\nNormalBG=#0C1018\r\n\
                          SelectedBG=#2A4A78\r\nFont=Arial\r\n";

/// The main window loses a 12x12 notch in its bottom-left corner.
const REGION_TXT: &str = "[Normal]\r\nNumPoints=6\r\n\
                          PointList=0,0,275,0,275,116,12,116,12,104,0,104\r\n";

/// A synthetic skin archive: the bitmaps the engine reads for its four panels, the three text
/// files, all generated. Pass it to `WinampEngine::load`.
pub fn demo_skin() -> Vec<u8> {
    let viscolor = viscolor();
    let files: Vec<(&str, Vec<u8>)> = vec![
        ("main.bmp", main_sheet().bmp()),
        ("titlebar.bmp", titlebar_sheet().bmp()),
        ("cbuttons.bmp", cbuttons_sheet().bmp()),
        ("shufrep.bmp", shufrep_sheet().bmp()),
        ("posbar.bmp", posbar_sheet().bmp()),
        ("volume.bmp", volume_sheet().bmp()),
        ("balance.bmp", balance_sheet().bmp()),
        ("monoster.bmp", monoster_sheet().bmp()),
        ("playpaus.bmp", playpaus_sheet().bmp()),
        ("nums_ex.bmp", numbers_sheet().bmp()),
        ("text.bmp", text_sheet().bmp()),
        ("eqmain.bmp", eqmain_sheet().bmp()),
        ("pledit.bmp", pledit_sheet().bmp()),
        ("gen.bmp", gen_sheet().bmp()),
        ("pledit.txt", PLEDIT_TXT.as_bytes().to_vec()),
        ("viscolor.txt", viscolor.into_bytes()),
        ("region.txt", REGION_TXT.as_bytes().to_vec()),
    ];
    let entries: Vec<(&str, &[u8])> = files.iter().map(|(n, d)| (*n, d.as_slice())).collect();
    zip_deflated(&entries)
}

#[cfg(test)]
mod tests {
    use whipspot_core::skin::{
        Command, Flag, PanelId, PlayerCommand, Shape, SkinEngine, TextRead, TimeDigit, Value,
        WidgetKind, WindowCommand,
    };

    use super::*;
    use crate::WinampEngine;

    #[test]
    fn the_demo_skin_loads_through_the_engine_and_validates() {
        let skin = WinampEngine.load(&demo_skin()).expect("loads");
        skin.validate().expect("validates");
        let ids: Vec<&str> = skin.panels.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["main", "equalizer", "playlist", "library"]);
        let main = &skin.panels[0];
        assert!(matches!(&main.shape, Shape::Polygons(p) if p.len() == 1));
        assert_eq!(skin.visualiser.as_ref().map(|v| v.bars.len()), Some(16));
        assert_eq!(skin.visualiser.as_ref().map(|v| v.scope.len()), Some(5));
        // Every sheet is read and every sprite of every panel fits (the engine drops one that does
        // not): the widget counts are those of the full reference skin.
        let counts: Vec<usize> = skin.panels.iter().map(|p| p.widgets.len()).collect();
        assert_eq!(counts, [29, 18, 14, 11]);
        assert_eq!(skin.images.len(), 14);
        assert_eq!(skin.fonts.len(), 2);
        assert!(skin.theme.list_text.is_some() && skin.theme.list_selection.is_some());

        let buttons: Vec<Command> = main
            .widgets
            .iter()
            .filter_map(|w| match &w.kind {
                WidgetKind::Button { command, .. } => Some(command.clone()),
                _ => None,
            })
            .collect();
        let library = PanelId::new("library").unwrap();
        for want in [
            Command::Window(WindowCommand::Minimize),
            Command::Window(WindowCommand::ClosePanel),
            Command::Player(PlayerCommand::Previous),
            Command::Player(PlayerCommand::Play),
            Command::Player(PlayerCommand::Pause),
            Command::Player(PlayerCommand::Stop),
            Command::Player(PlayerCommand::Next),
            Command::Window(WindowCommand::TogglePanel(library)),
        ] {
            assert!(buttons.contains(&want), "button {want:?}");
        }
        let toggles: Vec<Flag> = main
            .widgets
            .iter()
            .filter_map(|w| match &w.kind {
                WidgetKind::Toggle { state, .. } => Some(state.clone()),
                _ => None,
            })
            .collect();
        assert!(toggles.contains(&Flag::Shuffle) && toggles.contains(&Flag::Repeat));
        let sliders: Vec<Value> = main
            .widgets
            .iter()
            .filter_map(|w| match &w.kind {
                WidgetKind::Slider { value, .. } => Some(*value),
                _ => None,
            })
            .collect();
        for want in [Value::Volume, Value::Balance, Value::Position] {
            assert!(sliders.contains(&want), "slider {want:?}");
        }
        let texts: Vec<TextRead> = main
            .widgets
            .iter()
            .filter_map(|w| match &w.kind {
                WidgetKind::Text { text, .. } => Some(*text),
                _ => None,
            })
            .collect();
        for want in [
            TextRead::Time(TimeDigit::MinutesTens),
            TextRead::Time(TimeDigit::MinutesUnits),
            TextRead::Time(TimeDigit::SecondsTens),
            TextRead::Time(TimeDigit::SecondsUnits),
            TextRead::TrackTitle,
            TextRead::BitrateKbps,
            TextRead::SampleRateKhz,
        ] {
            assert!(texts.contains(&want), "text {want:?}");
        }
        assert_eq!(
            main.widgets
                .iter()
                .filter(|w| matches!(w.kind, WidgetKind::Visualiser))
                .count(),
            1
        );
    }

    #[test]
    fn the_demo_skin_is_deterministic_and_small() {
        let a = demo_skin();
        assert_eq!(a, demo_skin());
        assert!(a.len() < 512 * 1024, "{} bytes", a.len());
    }
}
