//! The engine: a skin archive in, the core's skin model out (D3, D5, D10).
//!
//! Loading is a pipeline over untrusted input. The archive's central directory is parsed (every cap
//! in `zip`), the known bitmaps are decoded one by one with the image budget that is left (every cap
//! in `bmp`), the three text files are read, and the four panels are built from the tables in
//! `geometry`. Anything missing or damaged degrades to a missing asset: the main bitmap is the only
//! file the skin cannot do without. Entry names are lookup keys, never paths, and nothing here
//! touches the file system.

use whipspot_core::skin::{
    Align, Anchor, Attach, Axis, BitmapFont, Command, Dock, Edge, Fill, Flag, FontId, FrameRule,
    ImageId, MAX_IMAGE_BYTES_TOTAL, MAX_IMAGES, MAX_SKIN_BYTES, Panel, PanelId, PlayerCommand,
    Rect, Resize, Rgb, Shape, Size, Skin, SkinEngine, SkinError, SkinImage, SkinInfo, SkinTheme,
    SpriteRect, StateRead, TextRead, TimeDigit, Value, VisPalette, Widget, WidgetKind,
    WindowCommand,
};
use whipspot_core::text::display_text;

use crate::geometry as g;
use crate::text;
use whipspot_skin_kit::bmp::{self, BmpError};
use whipspot_skin_kit::zip::{Archive, ZipError};

/// Turns a classic skin archive into a [`Skin`].
#[derive(Debug, Clone, Copy, Default)]
pub struct WinampEngine;

impl SkinEngine for WinampEngine {
    fn id(&self) -> &'static str {
        "classic"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["wsz"]
    }

    fn detect(&self, bytes: &[u8]) -> bool {
        Archive::parse(bytes).is_ok_and(|a| a.has("main.bmp"))
    }

    fn load(&self, bytes: &[u8]) -> Result<Skin, SkinError> {
        if bytes.len() as u64 > MAX_SKIN_BYTES {
            return Err(SkinError::TooLarge);
        }
        let archive = Archive::parse(bytes).map_err(zip_error)?;
        build(&archive)
    }
}

fn zip_error(e: ZipError) -> SkinError {
    match e {
        ZipError::Malformed => SkinError::Malformed,
        ZipError::TooLarge => SkinError::TooLarge,
        ZipError::Unsupported => SkinError::Unsupported,
    }
}

fn bmp_error(e: BmpError) -> SkinError {
    match e {
        BmpError::Malformed => SkinError::Malformed,
        BmpError::TooLarge | BmpError::OverBudget => SkinError::TooLarge,
        BmpError::Unsupported => SkinError::Unsupported,
    }
}

// ---------------------------------------------------------------------------------------------
// Images
// ---------------------------------------------------------------------------------------------

/// A kept image: its id in the skin and its size, so a sprite can be checked against it.
#[derive(Debug, Clone, Copy)]
struct Sheet {
    id: ImageId,
    w: u32,
    h: u32,
}

impl Sheet {
    /// The sprite at `rect`, or `None` when it is empty or sticks out of the sheet (D5).
    fn sprite(self, rect: Rect) -> Option<SpriteRect> {
        let inside = rect.w > 0
            && rect.h > 0
            && rect.x.checked_add(rect.w).is_some_and(|e| e <= self.w)
            && rect.y.checked_add(rect.h).is_some_and(|e| e <= self.h);
        inside.then_some(SpriteRect {
            image: self.id,
            rect,
        })
    }

    fn sprites<const N: usize>(self, rects: [Rect; N]) -> Option<[SpriteRect; N]> {
        let mut out = Vec::with_capacity(N);
        for r in rects {
            out.push(self.sprite(r)?);
        }
        out.try_into().ok()
    }
}

/// The images of one load. Invariant: the RGBA bytes held in `images` plus the one being decoded
/// never exceed `MAX_IMAGE_BYTES_TOTAL`, because each decode gets only what is left and an image
/// that is not kept is dropped before the next decode.
#[derive(Default)]
struct ImageSet {
    images: Vec<SkinImage>,
    used: usize,
}

impl ImageSet {
    fn remaining(&self) -> usize {
        MAX_IMAGE_BYTES_TOTAL.saturating_sub(self.used)
    }

    /// Reads `key`, checks its header for a size of at least `min` (`Malformed` when smaller; no
    /// pixel buffer exists yet), then decodes it within the budget that is left. Keeps nothing.
    fn decode(
        &self,
        archive: &Archive,
        key: &str,
        min: (u32, u32),
    ) -> Result<SkinImage, SkinError> {
        let bytes = archive
            .read(key)
            .ok_or(SkinError::NotFound)?
            .map_err(zip_error)?;
        let (w, h) = bmp::dimensions(&bytes).map_err(bmp_error)?;
        if w < min.0 || h < min.1 {
            return Err(SkinError::Malformed);
        }
        bmp::decode(&bytes, self.remaining()).map_err(bmp_error)
    }

    fn keep(&mut self, image: SkinImage) -> Option<Sheet> {
        if self.images.len() >= MAX_IMAGES {
            return None;
        }
        let sheet = Sheet {
            id: ImageId(u16::try_from(self.images.len()).ok()?),
            w: image.width(),
            h: image.height(),
        };
        self.used = self.used.saturating_add(image.rgba().len());
        self.images.push(image);
        Some(sheet)
    }

    /// A sheet by entry key; any failure is a missing asset.
    fn sheet(&mut self, archive: &Archive, key: &str) -> Option<Sheet> {
        let image = self.decode(archive, key, (1, 1)).ok()?;
        self.keep(image)
    }
}

// ---------------------------------------------------------------------------------------------
// Fonts
// ---------------------------------------------------------------------------------------------

/// The 5x6 text font. Letters are case-folded (both cases map to one cell); the fallback is the
/// space when the sheet has one.
fn text_font(sheet: Sheet) -> Option<BitmapFont> {
    let cell = |row: u32, col: u32| {
        sheet.sprite(Rect {
            x: col * g::TEXT_CELL_W,
            y: row * g::TEXT_CELL_H,
            w: g::TEXT_CELL_W,
            h: g::TEXT_CELL_H,
        })
    };
    let mut glyphs: Vec<(char, SpriteRect)> = Vec::new();
    for &(c, row, col) in g::TEXT_GLYPHS {
        let Some(sprite) = cell(row, col) else {
            continue;
        };
        glyphs.push((c, sprite));
        if c.is_ascii_lowercase() {
            glyphs.push((c.to_ascii_uppercase(), sprite));
        }
    }
    for (lower, upper) in g::TEXT_LOWER_ACCENTS {
        if let Some(sprite) = glyphs.iter().find(|(c, _)| *c == upper).map(|(_, s)| *s) {
            glyphs.push((lower, sprite));
        }
    }
    if glyphs.is_empty() {
        return None;
    }
    let fallback = glyphs.iter().any(|(c, _)| *c == ' ').then_some(' ');
    Some(BitmapFont {
        cell: Size {
            w: g::TEXT_CELL_W,
            h: g::TEXT_CELL_H,
        },
        glyphs,
        fallback,
    })
}

/// The 9x13 numbers font: `0`-`9`, and with the extended sheet a blank and a minus sign. Every
/// digit must fit.
fn numbers_font(sheet: Sheet, extended: bool) -> Option<BitmapFont> {
    let mut glyphs: Vec<(char, SpriteRect)> = Vec::new();
    for d in 0..10u32 {
        let c = char::from_digit(d, 10)?;
        glyphs.push((c, sheet.sprite(g::digit_cell(d))?));
    }
    if extended {
        glyphs.extend(sheet.sprite(g::NUMS_EX_BLANK).map(|s| (' ', s)));
        glyphs.extend(sheet.sprite(g::NUMS_EX_MINUS).map(|s| ('-', s)));
    }
    Some(BitmapFont {
        cell: Size {
            w: g::NUMBERS_CELL_W,
            h: g::NUMBERS_CELL_H,
        },
        glyphs,
        fallback: None,
    })
}

// ---------------------------------------------------------------------------------------------
// Everything the panels are built from
// ---------------------------------------------------------------------------------------------

struct Assets {
    main: Sheet,
    titlebar: Option<Sheet>,
    cbuttons: Option<Sheet>,
    shufrep: Option<Sheet>,
    posbar: Option<Sheet>,
    volume: Option<Sheet>,
    balance: Option<Sheet>,
    monoster: Option<Sheet>,
    playpaus: Option<Sheet>,
    eqmain: Option<Sheet>,
    pledit: Option<Sheet>,
    gen_sheet: Option<Sheet>,
    numbers: Option<FontId>,
    text: Option<FontId>,
}

/// The main bitmap: the one asset without which the load fails. At least the window size.
fn load_main(archive: &Archive, set: &mut ImageSet) -> Result<Sheet, SkinError> {
    let image = match set.decode(archive, "main.bmp", (g::WINDOW_W, g::WINDOW_H)) {
        Ok(image) => image,
        Err(SkinError::TooLarge) => return Err(SkinError::TooLarge),
        Err(_) => return Err(SkinError::Malformed),
    };
    set.keep(image).ok_or(SkinError::Malformed)
}

fn load_assets(archive: &Archive) -> Result<(Assets, ImageSet, Vec<BitmapFont>), SkinError> {
    let mut set = ImageSet::default();
    let main = load_main(archive, &mut set)?;
    let titlebar = set.sheet(archive, "titlebar.bmp");
    let cbuttons = set.sheet(archive, "cbuttons.bmp");
    let shufrep = set.sheet(archive, "shufrep.bmp");
    let posbar = set.sheet(archive, "posbar.bmp");
    let volume = set.sheet(archive, "volume.bmp");
    let balance = set.sheet(archive, "balance.bmp");
    let monoster = set.sheet(archive, "monoster.bmp");
    let playpaus = set.sheet(archive, "playpaus.bmp");

    let mut fonts: Vec<BitmapFont> = Vec::new();
    let mut add_font = |font: Option<BitmapFont>| {
        font.map(|f| {
            fonts.push(f);
            FontId((fonts.len() - 1) as u16)
        })
    };
    // nums_ex is preferred; a sheet too small for the digits is dropped and numbers is tried.
    let mut numbers = None;
    for (key, extended, min_w) in [("nums_ex.bmp", true, 108), ("numbers.bmp", false, 90)] {
        let Ok(image) = set.decode(archive, key, (min_w, g::NUMBERS_CELL_H)) else {
            continue;
        };
        if let Some(sheet) = set.keep(image) {
            numbers = add_font(numbers_font(sheet, extended));
            break;
        }
    }
    let text = set
        .sheet(archive, "text.bmp")
        .and_then(|s| add_font(text_font(s)));
    let eqmain = set.sheet(archive, "eqmain.bmp");
    let pledit = set.sheet(archive, "pledit.bmp");
    let gen_sheet = set.sheet(archive, "gen.bmp");

    Ok((
        Assets {
            main,
            titlebar,
            cbuttons,
            shufrep,
            posbar,
            volume,
            balance,
            monoster,
            playpaus,
            eqmain,
            pledit,
            gen_sheet,
            numbers,
            text,
        },
        set,
        fonts,
    ))
}

/// A text entry, or nothing when it is missing, damaged or over a cap.
fn read_text(archive: &Archive, key: &str) -> Option<Vec<u8>> {
    archive.read(key)?.ok()
}

fn palette(bytes: &[u8]) -> Option<VisPalette> {
    let v = text::viscolor(bytes)?;
    Some(VisPalette {
        background: v[0],
        grid: v[1],
        bars: v[2..18].to_vec(),
        scope: v[18..23].to_vec(),
        peak: v[23],
    })
}

fn theme(bytes: &[u8]) -> SkinTheme {
    let ini = text::ini(bytes);
    let Some((_, pairs)) = ini.iter().find(|(name, _)| name == "text") else {
        return SkinTheme::default();
    };
    let get = |key: &str| {
        pairs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    };
    let colour = |key: &str| -> Option<Rgb> { text::colour(get(key)?) };
    let font = get("font")
        .map(|f| display_text(f, 64))
        .filter(|f| !f.is_empty());
    SkinTheme {
        list_text: colour("normal"),
        list_text_current: colour("current"),
        list_background: colour("normalbg"),
        list_selection: colour("selectedbg"),
        list_font: font,
    }
}

/// The window shape of a region section, or a rectangle when there is none or it is invalid.
fn shape(regions: &[text::Section], section: &str) -> Shape {
    regions
        .iter()
        .find(|(name, _)| name == section)
        .and_then(|(_, pairs)| text::region(pairs, g::WINDOW_W, g::WINDOW_H))
        .map_or(Shape::Rect, Shape::Polygons)
}

// ---------------------------------------------------------------------------------------------
// Widgets
// ---------------------------------------------------------------------------------------------

const fn anchor(h: Align, v: Align) -> Anchor {
    Anchor { h, v }
}

const FIXED: Anchor = anchor(Align::Start, Align::Start);

fn sprite(sheet: Option<Sheet>, rect: Rect) -> Option<SpriteRect> {
    sheet?.sprite(rect)
}

fn image(sheet: Option<Sheet>, rect: Rect, fill: Fill) -> Option<WidgetKind> {
    Some(WidgetKind::Image {
        sprite: sprite(sheet, rect)?,
        fill,
    })
}

fn button(sheet: Option<Sheet>, command: Command, sprites: [Rect; 2]) -> Option<WidgetKind> {
    let [normal, pressed] = sheet?.sprites(sprites)?;
    Some(WidgetKind::Button {
        command,
        normal,
        pressed,
    })
}

/// `sprites` is `[off normal, off pressed, on normal, on pressed]`.
fn toggle(
    sheet: Option<Sheet>,
    command: Command,
    state: Flag,
    sprites: [Rect; 4],
) -> Option<WidgetKind> {
    let [off_normal, off_pressed, on_normal, on_pressed] = sheet?.sprites(sprites)?;
    Some(WidgetKind::Toggle {
        command,
        state,
        off: [off_normal, off_pressed],
        on: [on_normal, on_pressed],
    })
}

/// `sprites` is `[false, true]`.
fn flag_indicator(sheet: Option<Sheet>, flag: Flag, sprites: [Rect; 2]) -> Option<WidgetKind> {
    Some(WidgetKind::Indicator {
        state: StateRead::Flag(flag),
        sprites: sheet?.sprites(sprites)?.to_vec(),
    })
}

/// A slider whose frames are in the order its `FrameRule` wants (see `geometry`); every frame must
/// fit, a thumb that does not fit is left out.
fn slider(
    sheet: Option<Sheet>,
    value: Value,
    axis: Axis,
    frame_rule: FrameRule,
    frames: &[Rect],
    thumb: [Rect; 2],
) -> Option<WidgetKind> {
    let sheet = sheet?;
    let frames = frames
        .iter()
        .map(|r| sheet.sprite(*r))
        .collect::<Option<Vec<_>>>()?;
    Some(WidgetKind::Slider {
        value,
        axis,
        frames,
        frame_rule,
        thumb: sheet.sprites(thumb),
    })
}

fn text_widget(text: TextRead, font: Option<FontId>, scroll: bool) -> Option<WidgetKind> {
    Some(WidgetKind::Text {
        text,
        font: font?,
        scroll,
    })
}

#[derive(Default)]
struct Widgets(Vec<Widget>);

impl Widgets {
    /// Adds the widget in paint order; a widget whose assets are missing (`None`) is left out.
    fn add(&mut self, rect: Rect, anchor: Anchor, kind: Option<WidgetKind>) {
        if let Some(kind) = kind {
            self.0.push(Widget { rect, anchor, kind });
        }
    }
}

fn full_window() -> Rect {
    Rect {
        x: 0,
        y: 0,
        w: g::WINDOW_W,
        h: g::WINDOW_H,
    }
}

fn player(c: PlayerCommand) -> Command {
    Command::Player(c)
}

fn window(c: WindowCommand) -> Command {
    Command::Window(c)
}

fn panel_id(s: &str) -> Result<PanelId, SkinError> {
    PanelId::new(s).ok_or(SkinError::Malformed)
}

/// Which optional panels exist, so the main window only links to windows it can open.
struct Present {
    equalizer: bool,
    playlist: bool,
    library: bool,
}

fn main_panel(a: &Assets, present: &Present, shape: Shape) -> Result<Panel, SkinError> {
    let eq = panel_id("equalizer")?;
    let playlist = panel_id("playlist")?;
    let library = panel_id("library")?;
    let toggle_panel = |id: &PanelId| window(WindowCommand::TogglePanel(id.clone()));

    let mut w = Widgets::default();
    w.add(
        full_window(),
        FIXED,
        image(Some(a.main), g::MAIN_BACKGROUND, Fill::Once),
    );
    w.add(
        g::MAIN_TITLE,
        FIXED,
        flag_indicator(a.titlebar, Flag::PanelFocused, g::TITLEBAR_BAR),
    );
    w.add(g::MAIN_TITLE, FIXED, Some(WidgetKind::DragArea));
    w.add(
        g::MAIN_MINIMIZE,
        FIXED,
        button(
            a.titlebar,
            window(WindowCommand::Minimize),
            g::TITLEBAR_MINIMIZE,
        ),
    );
    w.add(
        g::MAIN_CLOSE,
        FIXED,
        button(
            a.titlebar,
            window(WindowCommand::ClosePanel),
            g::TITLEBAR_CLOSE,
        ),
    );
    let transport = [
        (
            g::MAIN_PREVIOUS,
            PlayerCommand::Previous,
            g::CBUTTONS_PREVIOUS,
        ),
        (g::MAIN_PLAY, PlayerCommand::Play, g::CBUTTONS_PLAY),
        (g::MAIN_PAUSE, PlayerCommand::Pause, g::CBUTTONS_PAUSE),
        (g::MAIN_STOP, PlayerCommand::Stop, g::CBUTTONS_STOP),
        (g::MAIN_NEXT, PlayerCommand::Next, g::CBUTTONS_NEXT),
    ];
    for (rect, command, sprites) in transport {
        w.add(rect, FIXED, button(a.cbuttons, player(command), sprites));
    }
    if present.library {
        w.add(
            g::MAIN_EJECT,
            FIXED,
            button(a.cbuttons, toggle_panel(&library), g::CBUTTONS_EJECT),
        );
    }
    w.add(
        g::MAIN_POSITION,
        FIXED,
        slider(
            a.posbar,
            Value::Position,
            Axis::Horizontal,
            FrameRule::Linear,
            &[g::POSBAR_BACKGROUND],
            g::POSBAR_THUMB,
        ),
    );
    let volume: Vec<Rect> = (0..g::SLIDER_FRAMES).map(g::volume_frame).collect();
    w.add(
        g::MAIN_VOLUME,
        FIXED,
        slider(
            a.volume,
            Value::Volume,
            Axis::Horizontal,
            FrameRule::Linear,
            &volume,
            g::VOLUME_THUMB,
        ),
    );
    let balance: Vec<Rect> = (0..g::SLIDER_FRAMES).map(g::balance_frame).collect();
    w.add(
        g::MAIN_BALANCE,
        FIXED,
        slider(
            a.balance,
            Value::Balance,
            Axis::Horizontal,
            FrameRule::FromCentre,
            &balance,
            g::BALANCE_THUMB,
        ),
    );
    w.add(
        g::MAIN_SHUFFLE,
        FIXED,
        toggle(
            a.shufrep,
            player(PlayerCommand::ToggleShuffle),
            Flag::Shuffle,
            g::SHUFREP_SHUFFLE,
        ),
    );
    w.add(
        g::MAIN_REPEAT,
        FIXED,
        toggle(
            a.shufrep,
            player(PlayerCommand::CycleRepeat),
            Flag::Repeat,
            g::SHUFREP_REPEAT,
        ),
    );
    if present.equalizer {
        w.add(
            g::MAIN_EQ_BUTTON,
            FIXED,
            toggle(
                a.shufrep,
                toggle_panel(&eq),
                Flag::PanelVisible(eq.clone()),
                g::SHUFREP_EQ,
            ),
        );
    }
    if present.playlist {
        w.add(
            g::MAIN_PLAYLIST_BUTTON,
            FIXED,
            toggle(
                a.shufrep,
                toggle_panel(&playlist),
                Flag::PanelVisible(playlist.clone()),
                g::SHUFREP_PLAYLIST,
            ),
        );
    }
    w.add(
        g::MAIN_STEREO,
        FIXED,
        flag_indicator(a.monoster, Flag::Stereo, g::MONOSTER_STEREO),
    );
    w.add(
        g::MAIN_MONO,
        FIXED,
        flag_indicator(a.monoster, Flag::Mono, g::MONOSTER_MONO),
    );
    w.add(
        g::MAIN_PLAY_STATE,
        FIXED,
        a.playpaus
            .and_then(|s| s.sprites(g::PLAYPAUS_STATES))
            .map(|s| WidgetKind::Indicator {
                state: StateRead::PlayState,
                sprites: s.to_vec(),
            }),
    );
    let digits = [
        TimeDigit::MinutesTens,
        TimeDigit::MinutesUnits,
        TimeDigit::SecondsTens,
        TimeDigit::SecondsUnits,
    ];
    for (rect, digit) in g::MAIN_TIME_DIGITS.into_iter().zip(digits) {
        w.add(
            rect,
            FIXED,
            text_widget(TextRead::Time(digit), a.numbers, false),
        );
    }
    w.add(
        g::MAIN_KBPS,
        FIXED,
        text_widget(TextRead::BitrateKbps, a.text, false),
    );
    w.add(
        g::MAIN_KHZ,
        FIXED,
        text_widget(TextRead::SampleRateKhz, a.text, false),
    );
    w.add(
        g::MAIN_TITLE_TEXT,
        FIXED,
        text_widget(TextRead::TrackTitle, a.text, true),
    );
    w.add(g::MAIN_VISUALISER, FIXED, Some(WidgetKind::Visualiser));

    Ok(Panel {
        id: panel_id("main")?,
        title: "Player".to_owned(),
        size: Size {
            w: g::WINDOW_W,
            h: g::WINDOW_H,
        },
        resize: None,
        shape,
        dock: dock(None),
        visible_at_start: true,
        widgets: w.0,
    })
}

fn equalizer_panel(sheet: Sheet, shape: Shape) -> Result<Panel, SkinError> {
    let s = Some(sheet);
    let mut w = Widgets::default();
    w.add(full_window(), FIXED, image(s, g::EQ_BACKGROUND, Fill::Once));
    w.add(
        g::EQ_TITLE,
        FIXED,
        flag_indicator(s, Flag::PanelFocused, g::EQ_TITLE_BAR),
    );
    w.add(g::EQ_TITLE, FIXED, Some(WidgetKind::DragArea));
    w.add(
        g::EQ_CLOSE,
        FIXED,
        button(s, window(WindowCommand::ClosePanel), g::EQ_CLOSE_SPRITES),
    );
    w.add(
        g::EQ_ON,
        FIXED,
        toggle(
            s,
            player(PlayerCommand::ToggleEq),
            Flag::EqEnabled,
            g::EQ_ON_SPRITES,
        ),
    );
    w.add(
        g::EQ_GRAPH,
        FIXED,
        image(s, g::EQ_GRAPH_BACKGROUND, Fill::Once),
    );
    w.add(
        g::EQ_GRAPH,
        FIXED,
        Some(WidgetKind::EqCurve {
            colours: sprite(s, g::EQ_GRAPH_COLOURS),
        }),
    );
    let frames: Vec<Rect> = (0..g::SLIDER_FRAMES).map(g::eq_band_frame).collect();
    let band = |value: Value| {
        slider(
            s,
            value,
            Axis::Vertical,
            FrameRule::Linear,
            &frames,
            g::EQ_THUMB,
        )
    };
    w.add(g::EQ_PREAMP, FIXED, band(Value::EqPreamp));
    for b in 0..10u8 {
        w.add(g::eq_band_rect(u32::from(b)), FIXED, band(Value::EqBand(b)));
    }
    Ok(Panel {
        id: panel_id("equalizer")?,
        title: "Equalizer".to_owned(),
        size: Size {
            w: g::WINDOW_W,
            h: g::WINDOW_H,
        },
        resize: None,
        shape,
        dock: dock(Some(Attach {
            to: panel_id("main")?,
            edge: Edge::Bottom,
        })),
        visible_at_start: true,
        widgets: w.0,
    })
}

fn resizable() -> Option<Resize> {
    Some(Resize {
        min: Size {
            w: g::WINDOW_W,
            h: g::WINDOW_H,
        },
        step: Size {
            w: g::RESIZE_STEP_W,
            h: g::RESIZE_STEP_H,
        },
    })
}

/// The playlist window. The frame pieces are drawn in their focused look; the unfocused look and
/// the buttons along the bottom are left to later (D8).
fn playlist_panel(sheet: Sheet, attach_to: &str) -> Result<Panel, SkinError> {
    let s = Some(sheet);
    let stretch_h = anchor(Align::Stretch, Align::Start);
    let end_h = anchor(Align::End, Align::Start);
    let stretch_v = anchor(Align::Start, Align::Stretch);
    let end_h_stretch_v = anchor(Align::End, Align::Stretch);
    let bottom = anchor(Align::Start, Align::End);
    let bottom_stretch_h = anchor(Align::Stretch, Align::End);
    let bottom_end = anchor(Align::End, Align::End);

    let mut w = Widgets::default();
    w.add(
        g::PL_TOP_TILE,
        stretch_h,
        image(s, g::PLEDIT_TOP_TILE, Fill::Tile),
    );
    w.add(
        g::PL_TOP_LEFT,
        FIXED,
        image(s, g::PLEDIT_TOP_LEFT, Fill::Once),
    );
    w.add(
        g::PL_TOP_RIGHT,
        end_h,
        image(s, g::PLEDIT_TOP_RIGHT, Fill::Once),
    );
    w.add(
        g::PL_TITLE,
        anchor(Align::Center, Align::Start),
        image(s, g::PLEDIT_TITLE, Fill::Once),
    );
    w.add(
        g::PL_LEFT_TILE,
        stretch_v,
        image(s, g::PLEDIT_LEFT_TILE, Fill::Tile),
    );
    w.add(
        g::PL_RIGHT_TILE,
        end_h_stretch_v,
        image(s, g::PLEDIT_RIGHT_TILE, Fill::Tile),
    );
    w.add(
        g::PL_BOTTOM_TILE,
        bottom_stretch_h,
        image(s, g::PLEDIT_BOTTOM_TILE, Fill::Tile),
    );
    w.add(
        g::PL_BOTTOM_LEFT,
        bottom,
        image(s, g::PLEDIT_BOTTOM_LEFT, Fill::Once),
    );
    w.add(
        g::PL_BOTTOM_RIGHT,
        bottom_end,
        image(s, g::PLEDIT_BOTTOM_RIGHT, Fill::Once),
    );
    w.add(g::PL_RESIZE, bottom_end, Some(WidgetKind::ResizeArea));
    w.add(
        g::PL_LIST,
        anchor(Align::Stretch, Align::Stretch),
        Some(WidgetKind::TrackList),
    );
    // A sheet too small for the thumb leaves the scroll bar out (the grip has no sprite and stays).
    w.add(
        g::PL_SCROLL,
        end_h_stretch_v,
        sheet
            .sprites(g::PLEDIT_SCROLL_THUMB)
            .map(|thumb| WidgetKind::ListScroll { thumb }),
    );
    w.add(g::PL_DRAG, stretch_h, Some(WidgetKind::DragArea));
    w.add(
        g::PL_CLOSE,
        end_h,
        close_button(sheet, g::PLEDIT_CLOSE_NORMAL, g::PLEDIT_CLOSE_PRESSED),
    );
    Ok(Panel {
        id: panel_id("playlist")?,
        title: "Playlist".to_owned(),
        size: Size {
            w: g::WINDOW_W,
            h: g::WINDOW_H,
        },
        resize: resizable(),
        shape: Shape::Rect,
        dock: dock(Some(Attach {
            to: panel_id(attach_to)?,
            edge: Edge::Bottom,
        })),
        visible_at_start: true,
        widgets: w.0,
    })
}

/// A close button whose normal look is the piece of the frame under it.
fn close_button(sheet: Sheet, normal: Rect, pressed: Rect) -> Option<WidgetKind> {
    button(
        Some(sheet),
        window(WindowCommand::ClosePanel),
        [normal, pressed],
    )
}

/// The library window, drawn like the playlist window from its own sheet and starting hidden.
fn library_panel(sheet: Sheet) -> Result<Panel, SkinError> {
    let s = Some(sheet);
    let stretch_h = anchor(Align::Stretch, Align::Start);
    let end_h = anchor(Align::End, Align::Start);
    let mut w = Widgets::default();
    w.add(
        g::LIB_TOP_FILL,
        stretch_h,
        image(s, g::GEN_TOP_FILL, Fill::Tile),
    );
    w.add(
        g::LIB_TOP_LEFT,
        FIXED,
        image(s, g::GEN_TOP_LEFT, Fill::Once),
    );
    w.add(
        g::LIB_TOP_RIGHT,
        end_h,
        image(s, g::GEN_TOP_RIGHT, Fill::Once),
    );
    w.add(
        g::LIB_MIDDLE_LEFT,
        anchor(Align::Start, Align::Stretch),
        image(s, g::GEN_MIDDLE_LEFT, Fill::Tile),
    );
    w.add(
        g::LIB_MIDDLE_RIGHT,
        anchor(Align::End, Align::Stretch),
        image(s, g::GEN_MIDDLE_RIGHT, Fill::Tile),
    );
    w.add(
        g::LIB_BOTTOM_FILL,
        anchor(Align::Stretch, Align::End),
        image(s, g::GEN_BOTTOM_FILL, Fill::Tile),
    );
    w.add(
        g::LIB_BOTTOM_LEFT,
        anchor(Align::Start, Align::End),
        image(s, g::GEN_BOTTOM_LEFT, Fill::Once),
    );
    w.add(
        g::LIB_BOTTOM_RIGHT,
        anchor(Align::End, Align::End),
        image(s, g::GEN_BOTTOM_RIGHT, Fill::Once),
    );
    w.add(
        g::LIB_VIEW,
        anchor(Align::Stretch, Align::Stretch),
        Some(WidgetKind::Library),
    );
    w.add(g::LIB_DRAG, stretch_h, Some(WidgetKind::DragArea));
    w.add(
        g::LIB_CLOSE,
        end_h,
        close_button(sheet, g::GEN_CLOSE_NORMAL, g::GEN_CLOSE_PRESSED),
    );
    Ok(Panel {
        id: panel_id("library")?,
        title: "Library".to_owned(),
        size: Size {
            w: g::WINDOW_W,
            h: g::WINDOW_H,
        },
        resize: resizable(),
        shape: Shape::Rect,
        dock: dock(Some(Attach {
            to: panel_id("main")?,
            edge: Edge::Right,
        })),
        visible_at_start: false,
        widgets: w.0,
    })
}

fn dock(attach: Option<Attach>) -> Dock {
    Dock {
        group: Some("main".to_owned()),
        attach,
        snap: true,
    }
}

// ---------------------------------------------------------------------------------------------
// The load
// ---------------------------------------------------------------------------------------------

fn build(archive: &Archive) -> Result<Skin, SkinError> {
    let (assets, set, fonts) = load_assets(archive)?;
    let regions = read_text(archive, "region.txt")
        .map(|b| text::ini(&b))
        .unwrap_or_default();
    let present = Present {
        equalizer: assets.eqmain.is_some(),
        playlist: assets.pledit.is_some(),
        library: assets.gen_sheet.is_some(),
    };

    let mut panels = vec![main_panel(&assets, &present, shape(&regions, "normal"))?];
    if let Some(sheet) = assets.eqmain {
        panels.push(equalizer_panel(sheet, shape(&regions, "equalizer"))?);
    }
    if let Some(sheet) = assets.pledit {
        let below = if present.equalizer {
            "equalizer"
        } else {
            "main"
        };
        panels.push(playlist_panel(sheet, below)?);
    }
    if let Some(sheet) = assets.gen_sheet {
        panels.push(library_panel(sheet)?);
    }

    Ok(Skin {
        info: SkinInfo {
            engine: "classic".to_owned(),
            name: String::new(),
        },
        panels,
        images: set.images,
        fonts,
        visualiser: read_text(archive, "viscolor.txt").and_then(|b| palette(&b)),
        theme: read_text(archive, "pledit.txt")
            .map(|b| theme(&b))
            .unwrap_or_default(),
    })
}
#[cfg(test)]
mod tests {
    use whipspot_core::skin::{MAX_GLYPHS, Point, SkinRegistry, SkinSource};
    use whipspot_testkit::{run_skin_caps_contract, run_skin_engine_contract};

    use super::*;
    use crate::synth::{self, Parts, index_at, palette_at, parts, px, sheet1, sheet8, sheet24};
    use whipspot_skin_kit::synth::{BmpSpec, ZEntry, ZipSpec};

    const MIB: usize = 1024 * 1024;

    fn rect(x: u32, y: u32, w: u32, h: u32) -> Rect {
        Rect { x, y, w, h }
    }

    fn load(bytes: &[u8]) -> Skin {
        let skin = WinampEngine.load(bytes).expect("the skin loads");
        skin.validate().expect("the skin validates");
        skin
    }

    fn load_parts(p: &Parts) -> Skin {
        load(&p.zip())
    }

    fn panel<'a>(skin: &'a Skin, id: &str) -> &'a Panel {
        skin.panels
            .iter()
            .find(|p| p.id.as_str() == id)
            .unwrap_or_else(|| panic!("panel {id}"))
    }

    fn ids(skin: &Skin) -> Vec<&str> {
        skin.panels.iter().map(|p| p.id.as_str()).collect()
    }

    fn pid(s: &str) -> PanelId {
        PanelId::new(s).unwrap()
    }

    fn find_slider(p: &Panel, value: Value) -> &WidgetKind {
        p.widgets
            .iter()
            .map(|w| &w.kind)
            .find(|k| matches!(k, WidgetKind::Slider { value: v, .. } if *v == value))
            .unwrap_or_else(|| panic!("slider {value:?}"))
    }

    fn buttons(p: &Panel) -> Vec<Command> {
        p.widgets
            .iter()
            .filter_map(|w| match &w.kind {
                WidgetKind::Button { command, .. } => Some(command.clone()),
                _ => None,
            })
            .collect()
    }

    fn toggles(p: &Panel) -> Vec<(Command, Flag)> {
        p.widgets
            .iter()
            .filter_map(|w| match &w.kind {
                WidgetKind::Toggle { command, state, .. } => Some((command.clone(), state.clone())),
                _ => None,
            })
            .collect()
    }

    fn indicators(p: &Panel) -> Vec<StateRead> {
        p.widgets
            .iter()
            .filter_map(|w| match &w.kind {
                WidgetKind::Indicator { state, .. } => Some(state.clone()),
                _ => None,
            })
            .collect()
    }

    fn slider_values(p: &Panel) -> Vec<Value> {
        p.widgets
            .iter()
            .filter_map(|w| match &w.kind {
                WidgetKind::Slider { value, .. } => Some(*value),
                _ => None,
            })
            .collect()
    }

    fn texts(p: &Panel) -> Vec<(TextRead, bool)> {
        p.widgets
            .iter()
            .filter_map(|w| match &w.kind {
                WidgetKind::Text { text, scroll, .. } => Some((*text, *scroll)),
                _ => None,
            })
            .collect()
    }

    fn count(p: &Panel, f: impl Fn(&WidgetKind) -> bool) -> usize {
        p.widgets.iter().filter(|w| f(&w.kind)).count()
    }

    fn glyph(f: &BitmapFont, c: char) -> Option<Rect> {
        f.glyphs.iter().find(|(g, _)| *g == c).map(|(_, s)| s.rect)
    }

    fn pcmd(c: PlayerCommand) -> Command {
        Command::Player(c)
    }

    fn wcmd(c: WindowCommand) -> Command {
        Command::Window(c)
    }

    /// A 24-bit sheet of zeros: a big inflated size for a few archive bytes.
    fn zero24(w: usize, h: usize) -> Vec<u8> {
        let mut s = BmpSpec::new(w as i32, h as i32, 24);
        s.data = vec![0; (w * 3).div_ceil(4) * 4 * h];
        s.bytes()
    }

    #[test]
    fn engine_contract_suite() {
        run_skin_engine_contract(&WinampEngine, &synth::skin());
    }

    #[test]
    fn caps_contract_on_the_synthetic_skin() {
        let skin = WinampEngine.load(&synth::skin()).unwrap();
        run_skin_caps_contract(&skin);
    }

    #[test]
    fn identity_detect_and_registry() {
        assert_eq!(WinampEngine.id(), "classic");
        assert_eq!(WinampEngine.extensions(), &["wsz"]);
        let good = synth::skin();
        assert!(WinampEngine.detect(&good));
        assert!(!WinampEngine.detect(&[]));
        assert!(!WinampEngine.detect(&[0; 4096]));
        assert!(!WinampEngine.detect(&parts().remove("main.bmp").zip()));

        struct One(Vec<u8>);
        impl SkinSource for One {
            fn list(&self) -> Vec<String> {
                vec!["a.wsz".to_owned()]
            }
            fn read(&self, _: &str) -> Result<Vec<u8>, SkinError> {
                Ok(self.0.clone())
            }
        }
        let mut reg = SkinRegistry::new();
        reg.register(std::sync::Arc::new(WinampEngine)).unwrap();
        assert_eq!(reg.extensions(), vec!["wsz".to_owned()]);
        let skin = reg.load(&One(good), "a.wsz").unwrap();
        assert_eq!(skin.info.engine, "classic");
        assert_eq!(skin.info.name, "a");
    }

    #[test]
    fn four_panels_with_their_shapes_docks_and_counts() {
        let skin = load(&synth::skin());
        assert_eq!(ids(&skin), ["main", "equalizer", "playlist", "library"]);
        let titles: Vec<&str> = skin.panels.iter().map(|p| p.title.as_str()).collect();
        assert_eq!(titles, ["Player", "Equalizer", "Playlist", "Library"]);
        for p in &skin.panels {
            assert_eq!(p.size, Size { w: 275, h: 116 }, "{}", p.id.as_str());
            assert_eq!(p.dock.group.as_deref(), Some("main"));
            assert!(p.dock.snap);
        }
        let [main, eq, pl, lib] = &skin.panels[..] else {
            panic!("four panels");
        };
        let visible = [
            main.visible_at_start,
            eq.visible_at_start,
            pl.visible_at_start,
            lib.visible_at_start,
        ];
        assert_eq!(visible, [true, true, true, false]);
        assert_eq!(main.dock.attach, None);
        let attach = |to: &str, edge| Some(Attach { to: pid(to), edge });
        assert_eq!(eq.dock.attach, attach("main", Edge::Bottom));
        assert_eq!(pl.dock.attach, attach("equalizer", Edge::Bottom));
        assert_eq!(lib.dock.attach, attach("main", Edge::Right));
        let rule = Some(Resize {
            min: Size { w: 275, h: 116 },
            step: Size { w: 25, h: 29 },
        });
        assert_eq!(
            (main.resize, eq.resize, pl.resize, lib.resize),
            (None, None, rule, rule)
        );
        assert_eq!(pl.shape, Shape::Rect);
        assert_eq!(lib.shape, Shape::Rect);
        let counts = [
            main.widgets.len(),
            eq.widgets.len(),
            pl.widgets.len(),
            lib.widgets.len(),
        ];
        assert_eq!(counts, [29, 18, 14, 11]);
        // 14 sheets are kept (nums_ex wins over numbers) and two fonts are built.
        assert_eq!(skin.images.len(), 14);
        assert_eq!(skin.fonts.len(), 2);
        // The first widget of the main panel is its background.
        assert!(matches!(
            main.widgets[0].kind,
            WidgetKind::Image {
                fill: Fill::Once,
                ..
            }
        ));
    }

    #[test]
    fn main_window_bindings() {
        let skin = load(&synth::skin());
        let main = panel(&skin, "main");
        assert_eq!(
            buttons(main),
            vec![
                wcmd(WindowCommand::Minimize),
                wcmd(WindowCommand::ClosePanel),
                pcmd(PlayerCommand::Previous),
                pcmd(PlayerCommand::Play),
                pcmd(PlayerCommand::Pause),
                pcmd(PlayerCommand::Stop),
                pcmd(PlayerCommand::Next),
                wcmd(WindowCommand::TogglePanel(pid("library"))),
            ]
        );
        assert_eq!(
            toggles(main),
            vec![
                (pcmd(PlayerCommand::ToggleShuffle), Flag::Shuffle),
                (pcmd(PlayerCommand::CycleRepeat), Flag::Repeat),
                (
                    wcmd(WindowCommand::TogglePanel(pid("equalizer"))),
                    Flag::PanelVisible(pid("equalizer"))
                ),
                (
                    wcmd(WindowCommand::TogglePanel(pid("playlist"))),
                    Flag::PanelVisible(pid("playlist"))
                ),
            ]
        );
        assert_eq!(
            indicators(main),
            vec![
                StateRead::Flag(Flag::PanelFocused),
                StateRead::Flag(Flag::Stereo),
                StateRead::Flag(Flag::Mono),
                StateRead::PlayState,
            ]
        );
        assert_eq!(
            slider_values(main),
            vec![Value::Position, Value::Volume, Value::Balance]
        );
        assert_eq!(
            texts(main),
            vec![
                (TextRead::Time(TimeDigit::MinutesTens), false),
                (TextRead::Time(TimeDigit::MinutesUnits), false),
                (TextRead::Time(TimeDigit::SecondsTens), false),
                (TextRead::Time(TimeDigit::SecondsUnits), false),
                (TextRead::BitrateKbps, false),
                (TextRead::SampleRateKhz, false),
                (TextRead::TrackTitle, true),
            ]
        );
        assert_eq!(count(main, |k| matches!(k, WidgetKind::DragArea)), 1);
        assert_eq!(count(main, |k| matches!(k, WidgetKind::Visualiser)), 1);
        // Every indicator has the exact sprite count the core checks.
        for w in &main.widgets {
            if let WidgetKind::Indicator { state, sprites } = &w.kind {
                let want = if *state == StateRead::PlayState { 3 } else { 2 };
                assert_eq!(sprites.len(), want);
            }
        }
        // The title bar sprites are [inactive, active]: the active bar is the upper one.
        let WidgetKind::Indicator { sprites, .. } = &main.widgets[1].kind else {
            panic!("title bar indicator");
        };
        assert_eq!(sprites[0].rect, rect(27, 15, 275, 14));
        assert_eq!(sprites[1].rect, rect(27, 0, 275, 14));
        // The play-state sprites are [stopped, playing, paused].
        let play_state = main
            .widgets
            .iter()
            .find_map(|w| match &w.kind {
                WidgetKind::Indicator {
                    state: StateRead::PlayState,
                    sprites,
                } => Some(sprites),
                _ => None,
            })
            .unwrap();
        let xs: Vec<u32> = play_state.iter().map(|s| s.rect.x).collect();
        assert_eq!(xs, [18, 0, 9]);
    }

    #[test]
    fn equalizer_playlist_and_library_widgets() {
        let skin = load(&synth::skin());
        let eq = panel(&skin, "equalizer");
        assert_eq!(buttons(eq), vec![wcmd(WindowCommand::ClosePanel)]);
        assert_eq!(
            toggles(eq),
            vec![(pcmd(PlayerCommand::ToggleEq), Flag::EqEnabled)]
        );
        let mut want = vec![Value::EqPreamp];
        want.extend((0..10).map(Value::EqBand));
        assert_eq!(slider_values(eq), want);
        let curve = eq
            .widgets
            .iter()
            .find_map(|w| match &w.kind {
                WidgetKind::EqCurve { colours } => Some(*colours),
                _ => None,
            })
            .expect("curve");
        assert_eq!(curve.map(|s| s.rect), Some(rect(115, 294, 1, 19)));

        let tiles = |p: &Panel| {
            count(p, |k| {
                matches!(
                    k,
                    WidgetKind::Image {
                        fill: Fill::Tile,
                        ..
                    }
                )
            })
        };
        let pl = panel(&skin, "playlist");
        assert_eq!(count(pl, |k| matches!(k, WidgetKind::TrackList)), 1);
        assert_eq!(count(pl, |k| matches!(k, WidgetKind::DragArea)), 1);
        assert_eq!(buttons(pl), vec![wcmd(WindowCommand::ClosePanel)]);
        assert_eq!(tiles(pl), 4);
        let lib = panel(&skin, "library");
        assert_eq!(count(lib, |k| matches!(k, WidgetKind::Library)), 1);
        assert_eq!(count(lib, |k| matches!(k, WidgetKind::DragArea)), 1);
        assert_eq!(buttons(lib), vec![wcmd(WindowCommand::ClosePanel)]);
        assert_eq!(tiles(lib), 4);
        // Frame pieces follow a resize: a stretching top tile, a right-hand tile that sticks to
        // the right edge, and a list that grows both ways.
        let tile = &pl.widgets[0];
        assert_eq!(
            (tile.anchor.h, tile.anchor.v),
            (Align::Stretch, Align::Start)
        );
        let right = &pl.widgets[5];
        assert_eq!(
            (right.anchor.h, right.anchor.v),
            (Align::End, Align::Stretch)
        );
        let list = pl
            .widgets
            .iter()
            .find(|w| matches!(w.kind, WidgetKind::TrackList))
            .unwrap();
        assert_eq!(
            (list.anchor.h, list.anchor.v),
            (Align::Stretch, Align::Stretch)
        );
        // The scroll bar: a track in the right tile that sticks to the right edge and stretches
        // with the height, its thumb `[normal, pressed]` 8x18 as cut from the sheet.
        let scroll: Vec<_> = pl
            .widgets
            .iter()
            .filter(|w| matches!(w.kind, WidgetKind::ListScroll { .. }))
            .collect();
        assert_eq!(scroll.len(), 1);
        assert_eq!(scroll[0].rect, rect(260, 20, 8, 58));
        assert_eq!(
            (scroll[0].anchor.h, scroll[0].anchor.v),
            (Align::End, Align::Stretch)
        );
        let WidgetKind::ListScroll { thumb } = &scroll[0].kind else {
            unreachable!()
        };
        assert_eq!(thumb[0].rect, rect(52, 53, 8, 18), "normal");
        assert_eq!(thumb[1].rect, rect(61, 53, 8, 18), "pressed");
        // The resize grip: the bottom-right corner, pinned to both edges, one per playlist.
        let grip: Vec<_> = pl
            .widgets
            .iter()
            .filter(|w| matches!(w.kind, WidgetKind::ResizeArea))
            .collect();
        assert_eq!(grip.len(), 1);
        assert_eq!(grip[0].rect, rect(255, 96, 20, 20));
        assert_eq!(
            (grip[0].anchor.h, grip[0].anchor.v),
            (Align::End, Align::End)
        );
        // Only the playlist has either.
        for id in ["main", "equalizer", "library"] {
            let p = panel(&skin, id);
            assert_eq!(
                count(p, |k| matches!(
                    k,
                    WidgetKind::ListScroll { .. } | WidgetKind::ResizeArea
                )),
                0,
                "{id}"
            );
        }
    }

    #[test]
    fn a_pledit_sheet_too_small_for_the_thumb_keeps_the_grip_only() {
        let skin = load_parts(&parts().set("pledit.bmp", sheet8(280, 60, 14)));
        let pl = panel(&skin, "playlist");
        assert_eq!(count(pl, |k| matches!(k, WidgetKind::ListScroll { .. })), 0);
        assert_eq!(count(pl, |k| matches!(k, WidgetKind::ResizeArea)), 1);
        assert_eq!(count(pl, |k| matches!(k, WidgetKind::TrackList)), 1);
    }

    #[test]
    fn volume_balance_and_band_frames_follow_the_frame_rule() {
        let skin = load(&synth::skin());
        let main = panel(&skin, "main");

        let WidgetKind::Slider {
            axis,
            frames,
            frame_rule,
            thumb,
            ..
        } = find_slider(main, Value::Volume)
        else {
            unreachable!()
        };
        assert_eq!(
            (*axis, *frame_rule, frames.len()),
            (Axis::Horizontal, FrameRule::Linear, 28)
        );
        assert_eq!(frames[0].rect, rect(0, 0, 68, 13), "volume 0");
        assert_eq!(frames[27].rect, rect(0, 405, 68, 13), "full volume");
        let img = &skin.images[usize::from(frames[0].image.0)];
        assert_eq!((img.width(), img.height()), (68, 433));
        let thumb = thumb.expect("volume thumb");
        assert_eq!(thumb[0].rect, rect(15, 422, 14, 11));
        assert_eq!(thumb[1].rect, rect(0, 422, 14, 11));

        let WidgetKind::Slider {
            frames, frame_rule, ..
        } = find_slider(main, Value::Balance)
        else {
            unreachable!()
        };
        assert_eq!((*frame_rule, frames.len()), (FrameRule::FromCentre, 28));
        assert_eq!(frames[0].rect, rect(9, 0, 38, 13), "balance centre");
        assert_eq!(frames[27].rect, rect(9, 405, 38, 13), "balance end");

        let WidgetKind::Slider {
            axis,
            frames,
            frame_rule,
            ..
        } = find_slider(main, Value::Position)
        else {
            unreachable!()
        };
        assert_eq!(
            (*axis, *frame_rule, frames.len()),
            (Axis::Horizontal, FrameRule::Linear, 1)
        );

        let eq = panel(&skin, "equalizer");
        let WidgetKind::Slider {
            axis,
            frames,
            frame_rule,
            ..
        } = find_slider(eq, Value::EqBand(0))
        else {
            unreachable!()
        };
        assert_eq!(
            (*axis, *frame_rule, frames.len()),
            (Axis::Vertical, FrameRule::Linear, 28)
        );
        assert_eq!(frames[0].rect, rect(13, 164, 14, 63), "-12 dB");
        assert_eq!(frames[27].rect, rect(208, 229, 14, 63), "+12 dB");
        let img = &skin.images[usize::from(frames[0].image.0)];
        assert_eq!((img.width(), img.height()), (275, 315));
        // Each band sits 18 px right of the one before it; the preamp is left of all of them.
        let band_x = |v: Value| {
            eq.widgets
                .iter()
                .find(|w| matches!(&w.kind, WidgetKind::Slider { value, .. } if *value == v))
                .map(|w| w.rect.x)
                .unwrap()
        };
        assert_eq!(band_x(Value::EqPreamp), 21);
        assert_eq!(band_x(Value::EqBand(0)), 78);
        assert_eq!(band_x(Value::EqBand(9)), 78 + 9 * 18);
    }

    #[test]
    fn images_are_decoded_from_their_sheets() {
        let skin = load(&synth::skin());
        let at = |img: &SkinImage, x: usize, y: usize| -> [u8; 4] {
            let o = (y * img.width() as usize + x) * 4;
            img.rgba()[o..o + 4].try_into().unwrap()
        };
        // main.bmp is the first image: 24-bit.
        let main = &skin.images[0];
        assert_eq!((main.width(), main.height()), (275, 116));
        let [r, g, b] = px(3, 2, 1);
        assert_eq!(at(main, 3, 2), [r, g, b, 255]);
        // titlebar.bmp is 8-bit.
        let titlebar = &skin.images[1];
        assert_eq!((titlebar.width(), titlebar.height()), (344, 87));
        let [r, g, b] = palette_at(usize::from(index_at(5, 2, 2)), 2);
        assert_eq!(at(titlebar, 5, 2), [r, g, b, 255]);
        // posbar.bmp is RLE8 (the fifth image: main, titlebar, cbuttons, shufrep, posbar).
        let posbar = &skin.images[4];
        assert_eq!((posbar.width(), posbar.height()), (307, 10));
        let [r, g, b] = palette_at(usize::from(index_at(9, 3, 5)), 5);
        assert_eq!(at(posbar, 9, 3), [r, g, b, 255]);
    }

    #[test]
    fn fonts_text_and_numbers() {
        let skin = load(&synth::skin());
        let numbers = &skin.fonts[0];
        assert_eq!(numbers.cell, Size { w: 9, h: 13 });
        for d in 0..10u32 {
            let c = char::from_digit(d, 10).unwrap();
            assert_eq!(glyph(numbers, c), Some(rect(d * 9, 0, 9, 13)));
        }
        // nums_ex is preferred: blank and minus exist.
        assert_eq!(glyph(numbers, ' '), Some(rect(90, 0, 9, 13)));
        assert_eq!(glyph(numbers, '-'), Some(rect(99, 0, 9, 13)));
        assert_eq!(numbers.fallback, None);

        let text = &skin.fonts[1];
        assert_eq!(text.cell, Size { w: 5, h: 6 });
        assert_eq!(glyph(text, 'a'), Some(rect(0, 0, 5, 6)));
        assert_eq!(glyph(text, 'A'), Some(rect(0, 0, 5, 6)), "case-folded");
        assert_eq!(glyph(text, 'z'), Some(rect(125, 0, 5, 6)));
        assert_eq!(glyph(text, '0'), Some(rect(0, 6, 5, 6)));
        assert_eq!(glyph(text, '<'), glyph(text, '['));
        assert_eq!(glyph(text, ' '), Some(rect(150, 0, 5, 6)));
        assert_eq!(glyph(text, '\u{e4}'), glyph(text, '\u{c4}'));
        assert_eq!(text.fallback, Some(' '));
        assert!(text.glyphs.len() <= MAX_GLYPHS);

        // Without nums_ex the plain numbers sheet serves, with digits only.
        let skin = load_parts(&parts().remove("nums_ex.bmp"));
        let numbers = &skin.fonts[0];
        assert_eq!(numbers.glyphs.len(), 10);
        assert_eq!(glyph(numbers, '-'), None);
        // Neither sheet: no numbers font, so the four time digits are left out.
        let skin = load_parts(&parts().remove("nums_ex.bmp").remove("numbers.bmp"));
        assert_eq!(skin.fonts.len(), 1);
        assert!(
            texts(panel(&skin, "main"))
                .iter()
                .all(|(t, _)| !matches!(t, TextRead::Time(_)))
        );
        // A numbers sheet too small for the digits is skipped and the plain one serves.
        let skin = load_parts(&parts().set("nums_ex.bmp", sheet24(50, 13, 11)));
        assert_eq!(skin.fonts[0].glyphs.len(), 10);
        // A text sheet too small for most glyphs keeps the ones that fit.
        let skin = load_parts(&parts().set("text.bmp", sheet24(30, 6, 12)));
        let text = skin.fonts.last().unwrap();
        assert_eq!(glyph(text, 'f'), Some(rect(25, 0, 5, 6)));
        assert_eq!(glyph(text, 'g'), None);
        assert_eq!(text.fallback, None);
    }

    #[test]
    fn nested_folder_and_upper_case_names_load_the_same_skin() {
        let plain = load(&synth::skin());
        let nested = parts().zip_named(|n| format!("Skin/Sub\\{}", n.to_uppercase()));
        assert_eq!(load(&nested), plain);
        let dirs = parts().zip_named(|n| format!("../{n}"));
        assert_eq!(load(&dirs), plain);
    }

    #[test]
    fn no_equalizer_bitmap_means_no_equalizer_panel_or_toggle() {
        let skin = load_parts(&parts().remove("eqmain.bmp"));
        assert_eq!(ids(&skin), ["main", "playlist", "library"]);
        let main = panel(&skin, "main");
        assert!(
            !toggles(main)
                .iter()
                .any(|(_, f)| *f == Flag::PanelVisible(pid("equalizer")))
        );
        assert_eq!(toggles(main).len(), 3);
        assert_eq!(
            panel(&skin, "playlist").dock.attach,
            Some(Attach {
                to: pid("main"),
                edge: Edge::Bottom
            })
        );
        // The same for the playlist and the library: no panel, no toggle, no eject.
        let skin = load_parts(&parts().remove("pledit.bmp").remove("gen.bmp"));
        assert_eq!(ids(&skin), ["main", "equalizer"]);
        let main = panel(&skin, "main");
        assert_eq!(toggles(main).len(), 3);
        assert!(!buttons(main).contains(&wcmd(WindowCommand::TogglePanel(pid("library")))));
    }

    #[test]
    fn a_short_transport_sheet_keeps_only_the_buttons_that_fit() {
        let skin = load_parts(&parts().set("cbuttons.bmp", sheet24(46, 36, 3)));
        assert_eq!(
            buttons(panel(&skin, "main")),
            vec![
                wcmd(WindowCommand::Minimize),
                wcmd(WindowCommand::ClosePanel),
                pcmd(PlayerCommand::Previous),
                pcmd(PlayerCommand::Play),
            ]
        );
    }

    #[test]
    fn an_unsupported_sheet_is_a_missing_asset() {
        let mut s = BmpSpec::new(136, 36, 4);
        s.compression = 2; // RLE4
        s.palette = vec![[0, 0, 0]; 16];
        s.data = vec![0; 16];
        let skin = load_parts(&parts().set("cbuttons.bmp", s.bytes()));
        assert_eq!(
            buttons(panel(&skin, "main")),
            vec![
                wcmd(WindowCommand::Minimize),
                wcmd(WindowCommand::ClosePanel)
            ]
        );
        assert_eq!(skin.images.len(), 13);
    }

    #[test]
    fn regions_give_shapes_and_bad_ones_give_rectangles() {
        let skin = load(&synth::skin());
        let p = |x, y| Point { x, y };
        assert_eq!(
            panel(&skin, "main").shape,
            Shape::Polygons(vec![
                vec![p(0, 0), p(275, 0), p(275, 116), p(0, 116)],
                vec![p(10, 10), p(50, 10), p(30, 50)],
            ])
        );
        assert_eq!(
            panel(&skin, "equalizer").shape,
            Shape::Polygons(vec![vec![p(0, 0), p(275, 0), p(275, 116), p(0, 116)]])
        );
        let bad = b"[Normal]\r\nNumPoints=4\r\nPointList=0,0,276,0,275,116,0,116\r\n".to_vec();
        let skin = load_parts(&parts().set("region.txt", bad));
        assert_eq!(panel(&skin, "main").shape, Shape::Rect);
        assert_eq!(panel(&skin, "equalizer").shape, Shape::Rect);
        let skin = load_parts(&parts().remove("region.txt"));
        assert_eq!(panel(&skin, "main").shape, Shape::Rect);
    }

    #[test]
    fn visualiser_palette_needs_all_24_lines() {
        let skin = load(&synth::skin());
        let pal = skin.visualiser.expect("palette");
        let rgb = |r, g, b| Rgb { r, g, b };
        assert_eq!(pal.background, rgb(0, 255, 0));
        assert_eq!(pal.grid, rgb(10, 245, 1));
        assert_eq!(pal.bars.len(), 16);
        assert_eq!(pal.bars[0], rgb(20, 235, 2));
        assert_eq!(pal.bars[15], rgb(170, 85, 17));
        assert_eq!(pal.scope.len(), 5);
        assert_eq!(pal.scope[0], rgb(180, 75, 18));
        assert_eq!(pal.peak, rgb(230, 25, 23));
        let p = parts();
        let viscolor = String::from_utf8(p.get("viscolor.txt").to_vec()).unwrap();
        let short: String = viscolor
            .lines()
            .take(23)
            .map(|l| format!("{l}\r\n"))
            .collect();
        let skin = load_parts(&p.set("viscolor.txt", short.into_bytes()));
        assert_eq!(skin.visualiser, None);
        let skin = load_parts(&parts().remove("viscolor.txt"));
        assert_eq!(skin.visualiser, None);
    }

    #[test]
    fn theme_comes_from_the_text_section() {
        let skin = load(&synth::skin());
        let rgb = |r, g, b| Some(Rgb { r, g, b });
        assert_eq!(skin.theme.list_text, rgb(0, 255, 0));
        assert_eq!(skin.theme.list_text_current, rgb(255, 255, 255));
        assert_eq!(skin.theme.list_background, rgb(0, 0, 0));
        assert_eq!(skin.theme.list_selection, rgb(0, 0, 255));
        assert_eq!(skin.theme.list_font.as_deref(), Some("Arial"));

        let bad = b"[Text]\r\nNormal=#GG0000\r\nCurrent=#FFFFFF\r\nFont=\r\n".to_vec();
        let skin = load_parts(&parts().set("pledit.txt", bad));
        assert_eq!(skin.theme.list_text, None);
        assert_eq!(skin.theme.list_text_current, rgb(255, 255, 255));
        assert_eq!(skin.theme.list_background, None);
        assert_eq!(skin.theme.list_font, None);

        let long = format!("[Text]\r\nFont={}\r\n", "x".repeat(200)).into_bytes();
        let skin = load_parts(&parts().set("pledit.txt", long));
        assert_eq!(skin.theme.list_font.map(|f| f.chars().count()), Some(64));

        let skin = load_parts(&parts().remove("pledit.txt"));
        assert_eq!(skin.theme, SkinTheme::default());
    }

    #[test]
    fn the_main_bitmap_is_required() {
        let err = |p: Parts| WinampEngine.load(&p.zip()).unwrap_err();
        assert_eq!(err(parts().remove("main.bmp")), SkinError::Malformed);
        assert_eq!(
            err(parts().set("main.bmp", sheet24(100, 50, 1))),
            SkinError::Malformed
        );
        assert_eq!(
            err(parts().set("main.bmp", sheet24(275, 115, 1))),
            SkinError::Malformed
        );
        assert_eq!(
            err(parts().set("main.bmp", vec![1, 2, 3])),
            SkinError::Malformed
        );
        // A main bitmap with a side past the cap.
        assert_eq!(
            err(parts().set("main.bmp", sheet1(2049, 116))),
            SkinError::TooLarge
        );
        // Not an archive at all.
        assert_eq!(
            WinampEngine.load(&[0x42; 1000]).unwrap_err(),
            SkinError::Malformed
        );
        // The smallest main bitmap passes.
        assert!(
            WinampEngine
                .load(&parts().set("main.bmp", sheet24(275, 116, 1)).zip())
                .is_ok()
        );
    }

    #[test]
    fn a_deflate_bomb_as_the_main_bitmap_is_an_error() {
        let zeros = vec![0u8; 8 * MIB];
        let mut bomb = ZEntry::deflated("main.bmp", &zeros);
        bomb.size = 64 * 1024;
        let bytes = ZipSpec {
            entries: vec![bomb],
            ..ZipSpec::default()
        }
        .bytes();
        assert!(bytes.len() < MIB);
        assert_eq!(WinampEngine.load(&bytes).unwrap_err(), SkinError::Malformed);

        // As any other bitmap it is only a missing asset.
        let mut bomb = ZEntry::deflated("cbuttons.bmp", &zeros);
        bomb.size = 64 * 1024;
        let mut entries: Vec<ZEntry> = parts()
            .remove("cbuttons.bmp")
            .files
            .iter()
            .map(|(n, d)| ZEntry::deflated(n, d))
            .collect();
        entries.push(bomb);
        let bytes = ZipSpec {
            entries,
            ..ZipSpec::default()
        }
        .bytes();
        assert_eq!(load(&bytes).images.len(), 13);
    }

    #[test]
    fn archive_level_errors_pass_through_and_entry_errors_do_not() {
        let entries = |flag_titlebar: bool| -> Vec<ZEntry> {
            parts()
                .files
                .iter()
                .map(|(n, d)| {
                    let mut e = ZEntry::deflated(n, d);
                    if flag_titlebar && n == "titlebar.bmp" {
                        e.flags = 1; // encrypted
                    }
                    e
                })
                .collect()
        };
        let multi_disk = ZipSpec {
            entries: entries(false),
            disk: 1,
            ..ZipSpec::default()
        }
        .bytes();
        assert_eq!(
            WinampEngine.load(&multi_disk).unwrap_err(),
            SkinError::Unsupported
        );
        // An encrypted entry is only a missing asset.
        let bytes = ZipSpec {
            entries: entries(true),
            ..ZipSpec::default()
        }
        .bytes();
        let skin = load(&bytes);
        assert_eq!(skin.images.len(), 13);
        assert_eq!(indicators(panel(&skin, "main")).len(), 3);
    }

    #[test]
    fn the_image_budget_is_never_exceeded() {
        // Five one-bit 2048 x 2048 sheets are 16 MiB of RGBA each; three fit next to main and the
        // small sheets, so the fourth (and what follows it) is a missing asset, not a failure.
        let big = sheet1(2048, 2048);
        let p = parts()
            .set("titlebar.bmp", big.clone())
            .set("cbuttons.bmp", big.clone())
            .set("eqmain.bmp", big.clone())
            .set("pledit.bmp", big.clone())
            .set("gen.bmp", big);
        let skin = load_parts(&p);
        let total: usize = skin.images.iter().map(|i| i.rgba().len()).sum();
        assert!(total <= MAX_IMAGE_BYTES_TOTAL, "{total}");
        assert!(
            total > 48 * MIB,
            "the sheets decoded before the budget ran out are kept"
        );
        // Decode order: main, titlebar, cbuttons, ..., eqmain, pledit, gen.
        assert_eq!(ids(&skin), ["main", "equalizer"]);
        assert_eq!(skin.images.iter().filter(|i| i.width() == 2048).count(), 3);
    }

    #[test]
    fn the_per_archive_inflate_cap_makes_late_sheets_missing() {
        // Every sheet is a 3 MB zero bitmap: 11 of them fit the 32 MiB per-archive inflate cap, so
        // the twelfth (the playlist sheet) and the thirteenth are missing; the skin still loads.
        let sheet = zero24(1000, 1000);
        let mut p = parts();
        for name in [
            "titlebar.bmp",
            "cbuttons.bmp",
            "shufrep.bmp",
            "posbar.bmp",
            "volume.bmp",
            "balance.bmp",
            "monoster.bmp",
            "playpaus.bmp",
            "nums_ex.bmp",
            "text.bmp",
            "eqmain.bmp",
            "pledit.bmp",
            "gen.bmp",
        ] {
            p = p.set(name, sheet.clone());
        }
        let skin = load_parts(&p);
        assert_eq!(ids(&skin), ["main", "equalizer"]);
        assert_eq!(skin.images.len(), 12);
    }

    #[test]
    fn load_is_deterministic() {
        assert_eq!(load(&synth::skin()), load(&synth::skin()));
    }
}
