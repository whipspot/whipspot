//! The skin seam (C5): a format-neutral model of a skin (panels, widgets, images, fonts, theme),
//! the engine and source traits an adapter implements, and the registry that loads and validates.
//!
//! A skin is untrusted input for every engine. [`SkinRegistry::load`] always runs
//! [`Skin::validate`] on what an engine returns, so an engine bug fails closed here and a
//! front-end only ever sees a model that passed the caps and reference checks. Widgets bind to
//! the player through the neutral enums ([`Command`], [`Value`], [`Flag`],
//! [`StateRead`], [`TextRead`]); the front-end maps them onto the player actions and state.
//! Nothing here names a skin format or a front-end type.

use std::fmt;

mod model;
mod seam;
mod validate;
mod windows;

pub use crate::art::ArtImage as SkinImage;
pub use model::{
    Align, Anchor, Attach, Axis, BitmapFont, Command, Dock, Edge, Fill, Flag, FontId, FrameRule,
    ImageId, Panel, PanelId, PlayerCommand, Point, Rect, Resize, Rgb, Shape, Size, Skin, SkinInfo,
    SkinTheme, SpriteRect, StateRead, TextRead, TimeDigit, Value, VisPalette, Widget, WidgetKind,
    WindowCommand,
};
pub use seam::{SkinEngine, SkinRegistry, SkinSource};
pub use windows::{PanelWindows, PanelWindowsError, ScreenPoint, ScreenRect};

/// The largest skin file an engine or source accepts, in bytes (hard rule 5).
pub const MAX_SKIN_BYTES: u64 = 20 * 1024 * 1024;
/// The most panels a skin may declare.
pub const MAX_PANELS: usize = 16;
/// The most widgets one panel may hold.
pub const MAX_WIDGETS_PER_PANEL: usize = 512;
/// The most images a skin may hold.
pub const MAX_IMAGES: usize = 64;
/// The most RGBA bytes all images of a skin may hold together.
pub const MAX_IMAGE_BYTES_TOTAL: usize = 64 * 1024 * 1024;
/// The most bitmap fonts a skin may hold.
pub const MAX_FONTS: usize = 8;
/// The most glyphs one bitmap font may hold.
pub const MAX_GLYPHS: usize = 256;
/// The most polygons one panel shape may hold.
pub const MAX_POLYGONS: usize = 128;
/// The most points all polygons of one panel shape may hold together.
pub const MAX_POINTS: usize = 4096;
/// The most colours the visualiser palette may hold in each of `bars` and `scope`.
pub const MAX_VIS_COLOURS: usize = 256;
/// The longest side a panel may have, in pixels.
pub const MAX_PANEL_SIDE: u32 = 4096;

/// Why a skin could not be listed, read or loaded. The texts are fixed and carry no input text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkinError {
    /// The name is not an acceptable skin name (or an engine id or extension is not).
    InvalidName,
    /// No skin of that name exists.
    NotFound,
    /// The name resolved to something a source must not hand out (outside the folder, not a file).
    NotAllowed,
    /// The file is over [`MAX_SKIN_BYTES`] or an inner cap.
    TooLarge,
    /// The file could not be read.
    Io,
    /// No registered engine claims the file.
    UnknownFormat,
    /// The file is damaged, or the model an engine built broke a [`Skin::validate`] rule.
    Malformed,
    /// The file uses a feature this build does not support.
    Unsupported,
}

impl fmt::Display for SkinError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            SkinError::InvalidName => "not a valid skin file name",
            SkinError::NotFound => "skin not found",
            SkinError::NotAllowed => "skin file is not allowed",
            SkinError::TooLarge => "skin file is too large",
            SkinError::Io => "could not read the skin file",
            SkinError::UnknownFormat => "not a skin this app can read",
            SkinError::Malformed => "the skin file is damaged",
            SkinError::Unsupported => "the skin uses a feature this app does not support",
        })
    }
}

impl std::error::Error for SkinError {}
