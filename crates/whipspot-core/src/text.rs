//! Making text from outside (provider names, lyrics) safe to draw.

/// Characters removed outright: bidi controls and marks, zero-width space, word joiner and the
/// byte-order mark. U+200C and U+200D are kept (Persian and Indic shaping, emoji sequences).
const fn is_removed(c: char) -> bool {
    matches!(
        c,
        '\u{061C}'
            | '\u{200E}'
            | '\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2066}'..='\u{2069}'
            | '\u{200B}'
            | '\u{2060}'
            | '\u{FEFF}'
    )
}

/// `s` made safe to draw: C0 and C1 control characters (tab, CR and LF included) become spaces,
/// bidi controls and marks, U+200B, U+2060 and U+FEFF are removed, the result is trimmed and cut
/// to `max_chars` characters on a char boundary. U+200C and U+200D stay.
pub fn display_text(s: &str, max_chars: usize) -> String {
    let cleaned: String = s
        .chars()
        .filter(|c| !is_removed(*c))
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let cut: String = cleaned.trim().chars().take(max_chars).collect();
    cut.trim_end().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controls_become_spaces() {
        assert_eq!(display_text("a\tb\r\nc", 100), "a b  c");
        assert_eq!(display_text("a\u{85}b", 100), "a b");
        assert_eq!(display_text("a\u{7f}b\u{0}c", 100), "a b c");
    }

    #[test]
    fn bidi_and_zero_width_are_removed_joiners_stay() {
        assert_eq!(display_text("a\u{202E}b\u{200B}c", 100), "abc");
        for c in [
            '\u{061C}', '\u{200E}', '\u{200F}', '\u{202A}', '\u{2066}', '\u{2069}', '\u{2060}',
            '\u{FEFF}',
        ] {
            assert_eq!(display_text(&format!("x{c}y"), 100), "xy", "{c:?}");
        }
        assert_eq!(display_text("a\u{200D}b", 100), "a\u{200D}b");
        assert_eq!(display_text("a\u{200C}b", 100), "a\u{200C}b");
    }

    #[test]
    fn trims_and_cuts_on_a_char_boundary() {
        assert_eq!(display_text("  hi  ", 100), "hi");
        assert_eq!(display_text("ééééé", 3), "ééé");
        assert_eq!(display_text(&"x".repeat(1000), 300).chars().count(), 300);
        assert_eq!(display_text("", 10), "");
        assert_eq!(display_text("abc", 0), "");
    }
}
