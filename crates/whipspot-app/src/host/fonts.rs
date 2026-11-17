//! Fonts (D43, MD16): three bundled faces, plus per-OS fallback files for emoji and CJK on request.
//! Dumps use the bundled faces only, so they look the same on every machine.

use lune_ui::text::TextCache;

use super::platform::FALLBACK_FONTS;

/// The UI family (bundled RznSans, OFL 1.1).
pub const UI_FONT: &str = "RznSans";
/// The mono family (bundled RobotoMono Nerd Font Mono, Apache 2.0): Codicon glyphs draw with it.
pub const MONO_FONT: &str = "RobotoMono Nerd Font Mono";

const MONO_REGULAR: &[u8] = include_bytes!("../../assets/fonts/RobotoMonoNerdFontMono-Regular.ttf");
const UI_REGULAR: &[u8] = include_bytes!("../../assets/fonts/RznSans-Regular.ttf");
const UI_MEDIUM: &[u8] = include_bytes!("../../assets/fonts/RznSans-Medium.ttf");

/// The bundled faces in the order the cache loads them.
fn bundled() -> [&'static [u8]; 3] {
    [MONO_REGULAR, UI_REGULAR, UI_MEDIUM]
}

/// A `TextCache` with the bundled faces only.
pub fn bundled_text_cache() -> TextCache {
    TextCache::new(&bundled(), UI_FONT, MONO_FONT)
}

/// The bundled faces, plus the OS fallback files that exist when `system_fonts` is set.
pub fn text_cache(system_fonts: bool) -> TextCache {
    let mut text = bundled_text_cache();
    if system_fonts {
        for path in FALLBACK_FONTS {
            // A missing or unreadable file is skipped: the fallback is best effort.
            let _ = text.fonts.db_mut().load_font_file(path);
        }
    }
    text
}
