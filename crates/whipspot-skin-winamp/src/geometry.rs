//! Where everything sits in a classic skin: sheet rectangles, window positions and the order of the
//! frames in the slider sheets (D9, MD102).
//!
//! Every number here is a fact about the file format, transcribed by hand from the public Webamp
//! reference (github.com/captbaritone/webamp, MIT licence): `packages/webamp/js/skinSprites.ts`
//! (sprite rectangles per sheet and the font table `FONT_LOOKUP`), `packages/webamp/css/`
//! `main-window.css`, `equalizer-window.css`, `playlist-window.css` and `gen-window.css` (window
//! positions), and the components `EqualizerWindow/Band.tsx`, `MainWindow/MainVolume.tsx` and
//! `MainWindow/MainBalance.tsx` (frame order and stride). No code, text or asset is copied. The
//! geometry cannot be checked by eye until M9 renders it; a wrong number is fixed here, the model
//! does not change.
//!
//! Sprite rects are relative to their sheet's top-left, widget rects to their panel's top-left, both
//! at 1x. The engine drops a sprite that does not fit its sheet, so a short sheet loses widgets
//! instead of failing the skin (D5).

use whipspot_core::skin::Rect;

const fn r(x: u32, y: u32, w: u32, h: u32) -> Rect {
    Rect { x, y, w, h }
}

/// The size of the main, equalizer, playlist and library windows at the start.
pub(crate) const WINDOW_W: u32 = 275;
pub(crate) const WINDOW_H: u32 = 116;

/// The playlist and library windows grow in these steps (Webamp's playlist: 25 px per column and
/// 29 px per row).
pub(crate) const RESIZE_STEP_W: u32 = 25;
pub(crate) const RESIZE_STEP_H: u32 = 29;

// ---------------------------------------------------------------------------------------------
// Main window: widget rects (panel pixels)
// ---------------------------------------------------------------------------------------------

pub(crate) const MAIN_TITLE: Rect = r(0, 0, 275, 14);
pub(crate) const MAIN_MINIMIZE: Rect = r(244, 3, 9, 9);
pub(crate) const MAIN_CLOSE: Rect = r(264, 3, 9, 9);
pub(crate) const MAIN_PREVIOUS: Rect = r(16, 88, 23, 18);
pub(crate) const MAIN_PLAY: Rect = r(39, 88, 23, 18);
pub(crate) const MAIN_PAUSE: Rect = r(62, 88, 23, 18);
pub(crate) const MAIN_STOP: Rect = r(85, 88, 23, 18);
/// The next button is 22 wide (its pressed sprite is 22 wide as well).
pub(crate) const MAIN_NEXT: Rect = r(108, 88, 22, 18);
pub(crate) const MAIN_EJECT: Rect = r(136, 89, 22, 16);
pub(crate) const MAIN_POSITION: Rect = r(16, 72, 248, 10);
pub(crate) const MAIN_VOLUME: Rect = r(107, 57, 68, 13);
pub(crate) const MAIN_BALANCE: Rect = r(177, 57, 38, 13);
pub(crate) const MAIN_SHUFFLE: Rect = r(164, 89, 47, 15);
pub(crate) const MAIN_REPEAT: Rect = r(210, 89, 28, 15);
pub(crate) const MAIN_EQ_BUTTON: Rect = r(219, 58, 23, 12);
pub(crate) const MAIN_PLAYLIST_BUTTON: Rect = r(242, 58, 23, 12);
pub(crate) const MAIN_MONO: Rect = r(212, 41, 27, 12);
pub(crate) const MAIN_STEREO: Rect = r(239, 41, 29, 12);
pub(crate) const MAIN_PLAY_STATE: Rect = r(26, 28, 9, 9);
/// The four elapsed-time digits: minutes tens and units, seconds tens and units (the time block
/// sits at 39,26; its digits at 9, 21, 39 and 51 from it).
pub(crate) const MAIN_TIME_DIGITS: [Rect; 4] = [
    r(48, 26, 9, 13),
    r(60, 26, 9, 13),
    r(78, 26, 9, 13),
    r(90, 26, 9, 13),
];
pub(crate) const MAIN_KBPS: Rect = r(111, 43, 15, 6);
pub(crate) const MAIN_KHZ: Rect = r(156, 43, 10, 6);
/// The scrolling track title ("marquee").
pub(crate) const MAIN_TITLE_TEXT: Rect = r(111, 24, 154, 6);
pub(crate) const MAIN_VISUALISER: Rect = r(24, 43, 76, 16);

// ---------------------------------------------------------------------------------------------
// Main window: sprites (sheet pixels)
// ---------------------------------------------------------------------------------------------

/// main.bmp: the whole background.
pub(crate) const MAIN_BACKGROUND: Rect = r(0, 0, 275, 116);

/// titlebar.bmp: [inactive, active] bars and the [normal, pressed] window buttons.
pub(crate) const TITLEBAR_BAR: [Rect; 2] = [r(27, 15, 275, 14), r(27, 0, 275, 14)];
pub(crate) const TITLEBAR_MINIMIZE: [Rect; 2] = [r(9, 0, 9, 9), r(9, 9, 9, 9)];
pub(crate) const TITLEBAR_CLOSE: [Rect; 2] = [r(18, 0, 9, 9), r(18, 9, 9, 9)];

/// cbuttons.bmp: [normal, pressed] per button (row 0 normal, row 1 pressed).
pub(crate) const CBUTTONS_PREVIOUS: [Rect; 2] = [r(0, 0, 23, 18), r(0, 18, 23, 18)];
pub(crate) const CBUTTONS_PLAY: [Rect; 2] = [r(23, 0, 23, 18), r(23, 18, 23, 18)];
pub(crate) const CBUTTONS_PAUSE: [Rect; 2] = [r(46, 0, 23, 18), r(46, 18, 23, 18)];
pub(crate) const CBUTTONS_STOP: [Rect; 2] = [r(69, 0, 23, 18), r(69, 18, 23, 18)];
pub(crate) const CBUTTONS_NEXT: [Rect; 2] = [r(92, 0, 23, 18), r(92, 18, 22, 18)];
pub(crate) const CBUTTONS_EJECT: [Rect; 2] = [r(114, 0, 22, 16), r(114, 16, 22, 16)];

/// shufrep.bmp: toggles as `[off normal, off pressed, on normal, on pressed]`; Webamp names the
/// rows "button", "depressed", "selected" and "selected depressed" (selected = on).
pub(crate) const SHUFREP_SHUFFLE: [Rect; 4] = [
    r(28, 0, 47, 15),
    r(28, 15, 47, 15),
    r(28, 30, 47, 15),
    r(28, 45, 47, 15),
];
pub(crate) const SHUFREP_REPEAT: [Rect; 4] = [
    r(0, 0, 28, 15),
    r(0, 15, 28, 15),
    r(0, 30, 28, 15),
    r(0, 45, 28, 15),
];
/// The equalizer and playlist buttons: `[off normal, off pressed, on normal, on pressed]` (Webamp's
/// button, depressed, selected and depressed-selected sprites).
pub(crate) const SHUFREP_EQ: [Rect; 4] = [
    r(0, 61, 23, 12),
    r(46, 61, 23, 12),
    r(0, 73, 23, 12),
    r(46, 73, 23, 12),
];
pub(crate) const SHUFREP_PLAYLIST: [Rect; 4] = [
    r(23, 61, 23, 12),
    r(69, 61, 23, 12),
    r(23, 73, 23, 12),
    r(69, 73, 23, 12),
];

/// posbar.bmp: the background and the [normal, pressed] thumbs.
pub(crate) const POSBAR_BACKGROUND: Rect = r(0, 0, 248, 10);
pub(crate) const POSBAR_THUMB: [Rect; 2] = [r(248, 0, 29, 10), r(278, 0, 29, 10)];

/// monoster.bmp: `[off, on]` for the stereo and mono lights.
pub(crate) const MONOSTER_STEREO: [Rect; 2] = [r(0, 12, 29, 12), r(0, 0, 29, 12)];
pub(crate) const MONOSTER_MONO: [Rect; 2] = [r(29, 12, 27, 12), r(29, 0, 27, 12)];

/// playpaus.bmp: `[stopped, playing, paused]` (the sheet holds them as playing, paused, stopped).
pub(crate) const PLAYPAUS_STATES: [Rect; 3] = [r(18, 0, 9, 9), r(0, 0, 9, 9), r(9, 0, 9, 9)];

// ---------------------------------------------------------------------------------------------
// Slider sheets: frame order read from the Webamp components
// ---------------------------------------------------------------------------------------------

/// The number of frames in the volume, balance and equalizer-band sheets.
pub(crate) const SLIDER_FRAMES: u32 = 28;

/// volume.bmp holds 28 frames, 15 px apart, each 68x13; `MainVolume.tsx` picks the frame for
/// volume 0 at the top and for full volume at index 27 (`(round(volume / 100 * 28) - 1) * 15`).
/// So `frames[0]` is volume 0, as `FrameRule::Linear` wants.
pub(crate) fn volume_frame(i: u32) -> Rect {
    r(0, i * 15, 68, 13)
}

/// balance.bmp holds 28 frames, 15 px apart; `MainBalance.tsx` picks the frame by
/// `floor(abs(balance) / 100 * 27)`, so index 0 is the centre and 27 either end, as
/// `FrameRule::FromCentre` wants. The background is 38 wide and starts at x = 9.
pub(crate) fn balance_frame(i: u32) -> Rect {
    r(9, i * 15, 38, 13)
}

/// The thumbs of the volume and balance sliders: `[normal, pressed]`, 14x11 at the foot of the
/// sheet (the pressed one at x = 0, the normal one at x = 15).
pub(crate) const VOLUME_THUMB: [Rect; 2] = [r(15, 422, 14, 11), r(0, 422, 14, 11)];
pub(crate) const BALANCE_THUMB: [Rect; 2] = [r(15, 422, 14, 11), r(0, 422, 14, 11)];

/// eqmain.bmp's slider background is a 14 x 2 grid of 14x63 frames, 15 px apart across and 65 px
/// apart down, starting at (13, 164). `Band.tsx` picks sprite `round(value / 100 * 27)` where value
/// 100 is +12 dB (the top of the slider), and places sprite `n` at column `n % 14`, row `n / 14`.
/// So `frames[0]` is -12 dB and `frames[27]` is +12 dB, as `FrameRule::Linear` wants.
pub(crate) fn eq_band_frame(i: u32) -> Rect {
    r(13 + (i % 14) * 15, 164 + (i / 14) * 65, 14, 63)
}

/// The equalizer slider thumb: `[normal, pressed]`, 11x11.
pub(crate) const EQ_THUMB: [Rect; 2] = [r(0, 164, 11, 11), r(0, 176, 11, 11)];

// ---------------------------------------------------------------------------------------------
// Equalizer window
// ---------------------------------------------------------------------------------------------

pub(crate) const EQ_BACKGROUND: Rect = r(0, 0, 275, 116);
/// eqmain.bmp: `[inactive, active]` title bars.
pub(crate) const EQ_TITLE_BAR: [Rect; 2] = [r(0, 149, 275, 14), r(0, 134, 275, 14)];
pub(crate) const EQ_TITLE: Rect = r(0, 0, 275, 14);
pub(crate) const EQ_CLOSE: Rect = r(264, 3, 9, 9);
/// `[normal, pressed]` close button in eqmain.bmp.
pub(crate) const EQ_CLOSE_SPRITES: [Rect; 2] = [r(0, 116, 9, 9), r(0, 125, 9, 9)];
pub(crate) const EQ_ON: Rect = r(14, 18, 26, 12);
/// `[off normal, off pressed, on normal, on pressed]`.
pub(crate) const EQ_ON_SPRITES: [Rect; 4] = [
    r(10, 119, 26, 12),
    r(128, 119, 26, 12),
    r(69, 119, 26, 12),
    r(187, 119, 26, 12),
];
pub(crate) const EQ_GRAPH: Rect = r(86, 17, 113, 19);
pub(crate) const EQ_GRAPH_BACKGROUND: Rect = r(0, 294, 113, 19);
/// The 1-px-wide strip whose pixel at a row colours the curve at that row.
pub(crate) const EQ_GRAPH_COLOURS: Rect = r(115, 294, 1, 19);
pub(crate) const EQ_PREAMP: Rect = r(21, 38, 14, 63);
/// The ten band sliders, 18 px apart, left to right from 60 Hz to 16 kHz.
pub(crate) fn eq_band_rect(band: u32) -> Rect {
    r(78 + band * 18, 38, 14, 63)
}

// ---------------------------------------------------------------------------------------------
// Playlist window (pledit.bmp): the focused ("selected") pieces
// ---------------------------------------------------------------------------------------------

pub(crate) const PLEDIT_TOP_TILE: Rect = r(127, 0, 25, 20);
pub(crate) const PLEDIT_TOP_LEFT: Rect = r(0, 0, 25, 20);
pub(crate) const PLEDIT_TITLE: Rect = r(26, 0, 100, 20);
pub(crate) const PLEDIT_TOP_RIGHT: Rect = r(153, 0, 25, 20);
pub(crate) const PLEDIT_LEFT_TILE: Rect = r(0, 42, 12, 29);
pub(crate) const PLEDIT_RIGHT_TILE: Rect = r(31, 42, 20, 29);
pub(crate) const PLEDIT_BOTTOM_TILE: Rect = r(179, 0, 25, 38);
pub(crate) const PLEDIT_BOTTOM_LEFT: Rect = r(0, 72, 125, 38);
pub(crate) const PLEDIT_BOTTOM_RIGHT: Rect = r(126, 72, 150, 38);
/// The close button's pressed sprite; its normal look is the part of the top-right corner it
/// covers (the corner is 25 wide, the button sits 14 px in and 3 px down).
pub(crate) const PLEDIT_CLOSE_PRESSED: Rect = r(52, 42, 9, 9);
pub(crate) const PLEDIT_CLOSE_NORMAL: Rect = r(153 + 14, 3, 9, 9);
/// The playlist's scroll thumb: `[normal, pressed]`, 8x18 (Webamp's `PLAYLIST_SCROLL_HANDLE` and
/// `_SELECTED`).
pub(crate) const PLEDIT_SCROLL_THUMB: [Rect; 2] = [r(52, 53, 8, 18), r(61, 53, 8, 18)];

// Playlist window: widget rects at the base size 275x116.
pub(crate) const PL_TOP_TILE: Rect = r(25, 0, 225, 20);
pub(crate) const PL_TOP_LEFT: Rect = r(0, 0, 25, 20);
pub(crate) const PL_TOP_RIGHT: Rect = r(250, 0, 25, 20);
/// The title bar piece, 100 wide, centred.
pub(crate) const PL_TITLE: Rect = r(87, 0, 100, 20);
pub(crate) const PL_LEFT_TILE: Rect = r(0, 20, 12, 58);
pub(crate) const PL_RIGHT_TILE: Rect = r(255, 20, 20, 58);
pub(crate) const PL_BOTTOM_TILE: Rect = r(0, 78, 275, 38);
pub(crate) const PL_BOTTOM_LEFT: Rect = r(0, 78, 125, 38);
pub(crate) const PL_BOTTOM_RIGHT: Rect = r(125, 78, 150, 38);
pub(crate) const PL_LIST: Rect = r(12, 20, 243, 58);
/// The scroll track: 5 px into the 20-px right tile, as tall as the list (58).
pub(crate) const PL_SCROLL: Rect = r(260, 20, 8, 58);
/// The resize grip: the bottom-right 20 x 20 corner.
pub(crate) const PL_RESIZE: Rect = r(255, 96, 20, 20);
pub(crate) const PL_DRAG: Rect = r(0, 0, 275, 20);
pub(crate) const PL_CLOSE: Rect = r(264, 3, 9, 9);

// ---------------------------------------------------------------------------------------------
// Library window (gen.bmp): the focused pieces
// ---------------------------------------------------------------------------------------------

pub(crate) const GEN_TOP_LEFT: Rect = r(0, 0, 25, 20);
pub(crate) const GEN_TOP_FILL: Rect = r(104, 0, 25, 20);
pub(crate) const GEN_TOP_RIGHT: Rect = r(130, 0, 25, 20);
pub(crate) const GEN_MIDDLE_LEFT: Rect = r(127, 42, 11, 29);
pub(crate) const GEN_MIDDLE_RIGHT: Rect = r(139, 42, 8, 29);
pub(crate) const GEN_BOTTOM_FILL: Rect = r(127, 72, 25, 14);
pub(crate) const GEN_BOTTOM_LEFT: Rect = r(0, 42, 125, 14);
pub(crate) const GEN_BOTTOM_RIGHT: Rect = r(0, 57, 125, 14);
pub(crate) const GEN_CLOSE_PRESSED: Rect = r(148, 42, 9, 9);
/// The part of the top-right piece the close button covers (14 px in, 3 px down).
pub(crate) const GEN_CLOSE_NORMAL: Rect = r(130 + 14, 3, 9, 9);

// Library window: widget rects at the base size 275x116.
pub(crate) const LIB_TOP_FILL: Rect = r(25, 0, 225, 20);
pub(crate) const LIB_TOP_LEFT: Rect = r(0, 0, 25, 20);
pub(crate) const LIB_TOP_RIGHT: Rect = r(250, 0, 25, 20);
pub(crate) const LIB_MIDDLE_LEFT: Rect = r(0, 20, 11, 82);
pub(crate) const LIB_MIDDLE_RIGHT: Rect = r(267, 20, 8, 82);
pub(crate) const LIB_BOTTOM_FILL: Rect = r(0, 102, 275, 14);
pub(crate) const LIB_BOTTOM_LEFT: Rect = r(0, 102, 125, 14);
pub(crate) const LIB_BOTTOM_RIGHT: Rect = r(150, 102, 125, 14);
pub(crate) const LIB_VIEW: Rect = r(11, 20, 256, 82);
pub(crate) const LIB_DRAG: Rect = r(0, 0, 275, 20);
pub(crate) const LIB_CLOSE: Rect = r(264, 3, 9, 9);

// ---------------------------------------------------------------------------------------------
// Fonts
// ---------------------------------------------------------------------------------------------

/// text.bmp: 5x6 cells. `FONT_LOOKUP` in skinSprites.ts gives each character's `[row, column]`;
/// the cell is at `(column * 5, row * 6)`. Letters are listed in lower case and the engine adds the
/// upper-case twins; `<` and `>` share the cells of `[` and `]` as in the table.
pub(crate) const TEXT_CELL_W: u32 = 5;
pub(crate) const TEXT_CELL_H: u32 = 6;

/// `(character, row, column)`, from `FONT_LOOKUP`.
pub(crate) const TEXT_GLYPHS: &[(char, u32, u32)] = &[
    ('a', 0, 0),
    ('b', 0, 1),
    ('c', 0, 2),
    ('d', 0, 3),
    ('e', 0, 4),
    ('f', 0, 5),
    ('g', 0, 6),
    ('h', 0, 7),
    ('i', 0, 8),
    ('j', 0, 9),
    ('k', 0, 10),
    ('l', 0, 11),
    ('m', 0, 12),
    ('n', 0, 13),
    ('o', 0, 14),
    ('p', 0, 15),
    ('q', 0, 16),
    ('r', 0, 17),
    ('s', 0, 18),
    ('t', 0, 19),
    ('u', 0, 20),
    ('v', 0, 21),
    ('w', 0, 22),
    ('x', 0, 23),
    ('y', 0, 24),
    ('z', 0, 25),
    ('"', 0, 26),
    ('@', 0, 27),
    (' ', 0, 30),
    ('0', 1, 0),
    ('1', 1, 1),
    ('2', 1, 2),
    ('3', 1, 3),
    ('4', 1, 4),
    ('5', 1, 5),
    ('6', 1, 6),
    ('7', 1, 7),
    ('8', 1, 8),
    ('9', 1, 9),
    ('\u{2026}', 1, 10),
    ('.', 1, 11),
    (':', 1, 12),
    ('(', 1, 13),
    (')', 1, 14),
    ('-', 1, 15),
    ('\'', 1, 16),
    ('!', 1, 17),
    ('_', 1, 18),
    ('+', 1, 19),
    ('\\', 1, 20),
    ('/', 1, 21),
    ('[', 1, 22),
    (']', 1, 23),
    ('^', 1, 24),
    ('&', 1, 25),
    ('%', 1, 26),
    (',', 1, 27),
    ('=', 1, 28),
    ('$', 1, 29),
    ('#', 1, 30),
    ('\u{c5}', 2, 0),
    ('\u{d6}', 2, 1),
    ('\u{c4}', 2, 2),
    ('?', 2, 3),
    ('*', 2, 4),
    ('<', 1, 22),
    ('>', 1, 23),
    ('{', 1, 22),
    ('}', 1, 23),
];

/// The three accented letters' lower-case twins, which the table lists in upper case only.
pub(crate) const TEXT_LOWER_ACCENTS: [(char, char); 3] = [
    ('\u{e5}', '\u{c5}'),
    ('\u{f6}', '\u{d6}'),
    ('\u{e4}', '\u{c4}'),
];

/// The numbers fonts: 9x13 cells, digit `n` at x = 9n. nums_ex.bmp adds a blank cell at 90 and a
/// minus sign at 99; numbers.bmp has neither (its minus sprites are 5x1 slivers, not cells).
pub(crate) const NUMBERS_CELL_W: u32 = 9;
pub(crate) const NUMBERS_CELL_H: u32 = 13;
pub(crate) const NUMS_EX_BLANK: Rect = r(90, 0, 9, 13);
pub(crate) const NUMS_EX_MINUS: Rect = r(99, 0, 9, 13);

/// The cell of digit `d` (0..=9) in numbers.bmp and nums_ex.bmp.
pub(crate) fn digit_cell(d: u32) -> Rect {
    r(d * NUMBERS_CELL_W, 0, NUMBERS_CELL_W, NUMBERS_CELL_H)
}
