//! The native UI's themes: colours and decorative treatments over one shared layout.
//!
//! A [`Theme`] is pure data (no drawing): the [`Ink`] for text and hairlines, where the accent
//! comes from ([`Accent`]), the glass [`Surface`], the window [`Backdrop`] and the [`Decor`]
//! choices (ring, play button, volume, quality pill, visualiser bars, labels, ...). The layout
//! never reads a theme to decide *where* something goes, only *how it looks*. Each frame resolves
//! the theme against the playing cover into a [`Look`], which is what the painters take.
//!
//! The themes are for the native UI; skin engines are a separate layer.

use lune_ui::color::{Rgba, mix};
use lune_ui::lunegit_dark;
use lune_ui::theme::Theme as LuneTheme;

use crate::palette::CoverAccent;

/// The built-in themes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemeId {
    /// A: straight, one fixed teal accent on smoked glass.
    A,
    /// B: hi-fi hardware, copper and amber, brushed glass, ticks and LEDs.
    B,
    /// C: adaptive, every highlight from the playing cover (teal without art), thin line glass.
    #[default]
    C,
}

impl ThemeId {
    /// Every theme, in cycling order.
    pub const ALL: [ThemeId; 3] = [ThemeId::A, ThemeId::B, ThemeId::C];

    /// The stable key ("a", "b", "c").
    pub fn key(self) -> &'static str {
        match self {
            ThemeId::A => "a",
            ThemeId::B => "b",
            ThemeId::C => "c",
        }
    }

    /// The theme stored under `key`, if it is one.
    pub fn from_key(key: &str) -> Option<ThemeId> {
        ThemeId::ALL.into_iter().find(|t| t.key() == key)
    }
}

/// The definition of theme `id`.
pub fn theme(id: ThemeId) -> &'static Theme {
    match id {
        ThemeId::A => &TEAL,
        ThemeId::B => &HIFI,
        ThemeId::C => &ADAPTIVE,
    }
}

/// Text and hairline colours.
#[derive(Debug, Clone, Copy)]
pub struct Ink {
    pub text: Rgba,
    pub text2: Rgba,
    pub muted: Rgba,
    pub faint: Rgba,
    /// Glass rims and strong separators.
    pub line: Rgba,
    /// Soft separators.
    pub line_soft: Rgba,
    /// A faint fill (hovered rows, plain pills).
    pub wash: Rgba,
    /// The lightest neutral, for knob heads and pointers (white, or warm white).
    pub light: Rgba,
}

/// Where the accent colours come from.
#[derive(Debug, Clone, Copy)]
pub enum Accent {
    /// A fixed ramp, low to high: `ramp[1]` is the accent, `ramp[3]` its pale tip; `on` is text
    /// on a filled accent.
    Fixed { ramp: [Rgba; 4], on: Rgba },
    /// The playing cover's dominant and secondary colours ([`crate::palette::cover_accent`]);
    /// theme A's teal without art.
    Cover,
}

/// Corner radii.
#[derive(Debug, Clone, Copy)]
pub struct Radii {
    pub panel: f32,
    pub sheet: f32,
    pub popup: f32,
    pub art: f32,
    pub thumb: f32,
    pub card: f32,
}

/// The glass sheets.
#[derive(Debug, Clone, Copy)]
pub struct Surface {
    /// The tint over the blurred scene (RGB) and its alpha on the media control, the browse sheet
    /// and popups.
    pub tint: Rgba,
    pub panel_alpha: u8,
    pub sheet_alpha: u8,
    pub popup_alpha: u8,
    pub sigma: f32,
    /// The rim's colour.
    pub rim: Rgba,
    /// The top sheen's peak alpha.
    pub sheen: u8,
    /// Faint horizontal streaks of a brushed faceplate.
    pub brushed: bool,
    /// A lit top edge.
    pub bevel: bool,
    pub radii: Radii,
}

/// How the cover backdrop is shaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shade {
    /// Darkened towards neutral black.
    Neutral,
    /// Darkened and warmed, with deeper edges (a dim listening room).
    Warm,
    /// Faded into the cover's own deep colour.
    CoverDeep,
}

/// The window background.
#[derive(Debug, Clone, Copy)]
pub struct Backdrop {
    pub shade: Shade,
    /// The ground without art, and its soft lights: (centre, radius, colour, strength).
    pub ground: Rgba,
    pub lights: [((f32, f32), f32, Rgba, f32); 4],
}

/// The progress ring round the play button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ring {
    /// A solid track and a bold arc with a white knob.
    Bold,
    /// A groove inside a ticked dial scale, a glowing head.
    Ticked,
    /// A hairline track, a thin gradient arc.
    Hairline,
}

/// The play/pause button itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayButton {
    /// A solid light disc with a dark glyph.
    Disc,
    /// A machined metal knob.
    Knob,
    /// No body: the glyph inside the ring.
    Bare,
}

/// The volume wedge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Volume {
    /// A solid triangle filled with the accent.
    Solid,
    /// A `\|` wedge under a tick scale, with a needle and a dB readout.
    Scaled,
    /// An outlined triangle, the fill in the accent gradient.
    Outline,
}

/// The quality pill (always borderless).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pill {
    /// Plain glass and medium text.
    Glass,
    /// A dark display window with an LED and tracked caps, lit when hi-res.
    Led,
    /// Plain glass with an accent dot.
    Dot,
}

/// The visualiser's bars.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bars {
    /// Solid bars with rounded tops.
    Solid,
    /// LED-meter segments.
    Segmented,
    /// Soft gradient bars.
    Gradient,
}

/// Small labels ("NOW PLAYING", tooltip keys).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Labels {
    /// Plain medium text, with an equaliser mark before the section label.
    Marked,
    /// Letter-spaced small caps after an LED.
    Tracked,
    /// Plain medium text.
    Plain,
}

/// How a "this is fine" state is shown (tooltip checks, the pill's hi-res state).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Indicator {
    Check,
    Led,
}

/// The active tab of the browse sheet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    /// A soft pill behind it.
    Pill,
    /// A lit LED above it (dim LEDs above the rest).
    Led,
    /// A gradient underline.
    Underline,
}

/// The transport icons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Glyphs {
    /// Filled shapes.
    Solid,
    /// Hairline strokes.
    Line,
}

/// The decorative treatments.
#[derive(Debug, Clone, Copy)]
pub struct Decor {
    pub ring: Ring,
    pub play: PlayButton,
    pub volume: Volume,
    pub pill: Pill,
    pub bars: Bars,
    pub labels: Labels,
    pub indicator: Indicator,
    pub tab: Tab,
    pub glyphs: Glyphs,
    /// The rule under the song info on the idle view: actions, a tick ruler or a gradient line.
    pub rule: Rule,
    /// Numbers (times, levels) in the monospaced face.
    pub mono_numbers: bool,
    /// Glow under the big idle art in the accent.
    pub art_glow: bool,
    /// Display text (the big title, headings) in the medium weight.
    pub display_medium: bool,
}

/// The rule under the song info.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rule {
    Actions,
    Ticks,
    Gradient,
}

/// One theme.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub id: ThemeId,
    pub name: &'static str,
    pub ink: Ink,
    pub accent: Accent,
    pub surface: Surface,
    pub backdrop: Backdrop,
    pub decor: Decor,
}

const COOL_INK: Ink = Ink {
    text: [0xF4, 0xF5, 0xF7, 0xFF],
    text2: [0xBC, 0xC0, 0xC8, 0xFF],
    muted: [0x88, 0x8D, 0x98, 0xFF],
    faint: [0x5E, 0x63, 0x6D, 0xFF],
    line: [0xFF, 0xFF, 0xFF, 0x1C],
    line_soft: [0xFF, 0xFF, 0xFF, 0x10],
    wash: [0xFF, 0xFF, 0xFF, 0x0C],
    light: [0xF6, 0xF7, 0xF9, 0xFF],
};

/// A: straight teal.
pub const TEAL: Theme = Theme {
    id: ThemeId::A,
    name: "Teal",
    ink: COOL_INK,
    accent: Accent::Fixed {
        ramp: [
            [0x16, 0x7A, 0x6A, 0xFF],
            [0x2E, 0xD3, 0xB0, 0xFF],
            [0x5C, 0xE4, 0xC4, 0xFF],
            [0x8A, 0xF2, 0xDB, 0xFF],
        ],
        on: [0x02, 0x1C, 0x16, 0xFF],
    },
    surface: Surface {
        tint: [0x10, 0x11, 0x15, 0xFF],
        panel_alpha: 0xB4,
        sheet_alpha: 0x90,
        popup_alpha: 0xD0,
        sigma: 10.0,
        rim: [0xFF, 0xFF, 0xFF, 0x1C],
        sheen: 0x00,
        brushed: false,
        bevel: false,
        radii: Radii {
            panel: 24.0,
            sheet: 24.0,
            popup: 16.0,
            art: 14.0,
            thumb: 8.0,
            card: 12.0,
        },
    },
    backdrop: Backdrop {
        shade: Shade::Neutral,
        ground: [0x14, 0x16, 0x1A, 0xFF],
        lights: [
            ((260.0, 160.0), 640.0, [0x40, 0x4C, 0x5A, 0xFF], 1.3),
            ((1240.0, 280.0), 620.0, [0x26, 0x5A, 0x54, 0xFF], 1.2),
            ((720.0, 960.0), 700.0, [0x1E, 0x66, 0x58, 0xFF], 1.0),
            ((820.0, 140.0), 380.0, [0x55, 0x5C, 0x66, 0xFF], 0.35),
        ],
    },
    decor: Decor {
        ring: Ring::Bold,
        play: PlayButton::Disc,
        volume: Volume::Solid,
        pill: Pill::Glass,
        bars: Bars::Solid,
        labels: Labels::Marked,
        indicator: Indicator::Check,
        tab: Tab::Pill,
        glyphs: Glyphs::Solid,
        rule: Rule::Actions,
        mono_numbers: false,
        art_glow: false,
        display_medium: true,
    },
};

/// B: hi-fi hardware.
pub const HIFI: Theme = Theme {
    id: ThemeId::B,
    name: "Hi-fi copper",
    ink: Ink {
        text: [0xF5, 0xEE, 0xE4, 0xFF],
        text2: [0xCF, 0xC4, 0xB6, 0xFF],
        muted: [0x98, 0x8D, 0x80, 0xFF],
        faint: [0x66, 0x5E, 0x55, 0xFF],
        line: [0xFF, 0xEE, 0xDC, 0x22],
        line_soft: [0xFF, 0xEE, 0xDC, 0x12],
        wash: [0xFF, 0xF0, 0xE0, 0x0C],
        light: [0xFF, 0xF4, 0xE6, 0xFF],
    },
    accent: Accent::Fixed {
        ramp: [
            [0x9A, 0x4E, 0x24, 0xFF],
            [0xE0, 0x92, 0x52, 0xFF],
            [0xF4, 0xB2, 0x62, 0xFF],
            [0xFF, 0xD9, 0x9E, 0xFF],
        ],
        on: [0x24, 0x14, 0x08, 0xFF],
    },
    surface: Surface {
        tint: [0x1E, 0x19, 0x15, 0xFF],
        panel_alpha: 0xC2,
        sheet_alpha: 0x88,
        popup_alpha: 0xD8,
        sigma: 10.0,
        rim: [0xFF, 0xEC, 0xD6, 0x1A],
        sheen: 0x0C,
        brushed: true,
        bevel: true,
        radii: Radii {
            panel: 16.0,
            sheet: 18.0,
            popup: 14.0,
            art: 4.0,
            thumb: 4.0,
            card: 8.0,
        },
    },
    backdrop: Backdrop {
        shade: Shade::Warm,
        ground: [0x14, 0x10, 0x0D, 0xFF],
        lights: [
            ((240.0, 160.0), 720.0, [0xB8, 0x6A, 0x36, 0xFF], 0.9),
            ((1240.0, 760.0), 760.0, [0x7A, 0x34, 0x3C, 0xFF], 0.85),
            ((900.0, 260.0), 520.0, [0xD8, 0xA0, 0x60, 0xFF], 0.45),
            ((120.0, 860.0), 520.0, [0x3A, 0x2A, 0x50, 0xFF], 0.6),
        ],
    },
    decor: Decor {
        ring: Ring::Ticked,
        play: PlayButton::Knob,
        volume: Volume::Scaled,
        pill: Pill::Led,
        bars: Bars::Segmented,
        labels: Labels::Tracked,
        indicator: Indicator::Led,
        tab: Tab::Led,
        glyphs: Glyphs::Solid,
        rule: Rule::Ticks,
        mono_numbers: true,
        art_glow: false,
        display_medium: false,
    },
};

/// C: adaptive minimal.
pub const ADAPTIVE: Theme = Theme {
    id: ThemeId::C,
    name: "Adaptive",
    ink: Ink {
        text: [0xF6, 0xF6, 0xF8, 0xFF],
        text2: [0xC9, 0xCB, 0xD2, 0xFF],
        muted: [0x93, 0x97, 0xA2, 0xFF],
        faint: [0x66, 0x6A, 0x75, 0xFF],
        line: [0xFF, 0xFF, 0xFF, 0x24],
        line_soft: [0xFF, 0xFF, 0xFF, 0x12],
        wash: [0xFF, 0xFF, 0xFF, 0x0C],
        light: [0xF6, 0xF6, 0xF8, 0xFF],
    },
    accent: Accent::Cover,
    surface: Surface {
        tint: [0x0A, 0x0B, 0x10, 0xFF],
        panel_alpha: 0x66,
        sheet_alpha: 0x72,
        popup_alpha: 0x80,
        sigma: 10.0,
        rim: [0xFF, 0xFF, 0xFF, 0x24],
        sheen: 0x0A,
        brushed: false,
        bevel: false,
        radii: Radii {
            panel: 26.0,
            sheet: 30.0,
            popup: 20.0,
            art: 20.0,
            thumb: 10.0,
            card: 16.0,
        },
    },
    backdrop: Backdrop {
        shade: Shade::CoverDeep,
        ground: [0x0D, 0x0F, 0x15, 0xFF],
        lights: [
            ((240.0, 180.0), 620.0, [0x4C, 0x66, 0xC8, 0xFF], 1.3),
            ((1240.0, 300.0), 640.0, [0x86, 0x58, 0xC8, 0xFF], 1.15),
            ((720.0, 940.0), 700.0, [0x2C, 0x86, 0xA8, 0xFF], 1.0),
            ((800.0, 160.0), 360.0, [0xC0, 0x96, 0xE8, 0xFF], 0.45),
        ],
    },
    decor: Decor {
        ring: Ring::Hairline,
        play: PlayButton::Bare,
        volume: Volume::Outline,
        pill: Pill::Dot,
        bars: Bars::Gradient,
        labels: Labels::Plain,
        indicator: Indicator::Check,
        tab: Tab::Underline,
        glyphs: Glyphs::Line,
        rule: Rule::Gradient,
        mono_numbers: false,
        art_glow: true,
        display_medium: false,
    },
};

/// A theme resolved for one frame (against the playing cover): what the painters read.
#[derive(Debug, Clone, Copy)]
pub struct Look {
    pub theme: &'static Theme,
    pub ink: Ink,
    /// The accent.
    pub a: Rgba,
    /// The secondary accent (the far end of the ramp).
    pub b: Rgba,
    /// A pale accent, for text on dark.
    pub hi: Rgba,
    /// A very dark accent, for shading the background.
    pub deep: Rgba,
    /// Text and glyphs on a filled accent.
    pub on: Rgba,
    ramp: [Rgba; 4],
}

impl Look {
    /// `theme` for a track whose cover gave `cover`; a cover-accent theme without one uses theme
    /// A's teal ramp and `on` exactly (the theme's own ink, surface and decor stay).
    pub fn new(theme: &'static Theme, cover: Option<CoverAccent>) -> Look {
        let black = [0x04, 0x04, 0x07, 0xFF];
        let (ramp, a, hi, on) = match (theme.accent, cover) {
            (Accent::Fixed { ramp, on }, _) => (ramp, ramp[1], ramp[3], on),
            (Accent::Cover, None) => match TEAL.accent {
                Accent::Fixed { ramp, on } => (ramp, ramp[1], ramp[3], on),
                Accent::Cover => unreachable!("theme A has a fixed accent"),
            },
            (Accent::Cover, Some(c)) => {
                let ramp = [c.a, mix(c.a, c.b, 0.33), mix(c.a, c.b, 0.66), c.b];
                (ramp, c.a, mix(c.a, [0xFF; 4], 0.55), mix(c.a, black, 0.8))
            }
        };
        Look {
            theme,
            ink: theme.ink,
            a,
            b: ramp[3],
            hi,
            deep: mix(a, black, 0.86),
            on,
            ramp,
        }
    }

    /// The three stops of a link's hover gradient (a, b, c; the loop returns to a): A and B have
    /// their own fixed set (the approved mockup), C takes a and b from the cover and c as a turn
    /// of a by 55 degrees of hue away from b (see `third_stop`).
    pub fn link_stops(&self) -> [Rgba; 3] {
        match self.theme.id {
            ThemeId::A => [
                [0x2F, 0xD3, 0xC0, 0xFF],
                [0x8A, 0xF0, 0xE0, 0xFF],
                [0x4A, 0xA8, 0xFF, 0xFF],
            ],
            ThemeId::B => [
                [0xD9, 0x8A, 0x4E, 0xFF],
                [0xF4, 0xC9, 0x8E, 0xFF],
                [0xE0, 0x60, 0x3A, 0xFF],
            ],
            ThemeId::C => [self.a, self.b, self.third_stop()],
        }
    }

    /// Theme C's third link stop: `a` turned 55 degrees of hue, away from `b` so the three stops
    /// differ (the mockup's own samples turn pink/orange to violet and green/yellow to teal).
    fn third_stop(&self) -> Rgba {
        let toward_b = (crate::palette::hue_of(self.b) - crate::palette::hue_of(self.a) + 540.0)
            .rem_euclid(360.0)
            - 180.0;
        let turn = if toward_b > 0.0 { -55.0 } else { 55.0 };
        crate::palette::turn_hue(self.a, turn)
    }

    /// The accent ramp at `t` (0..1).
    pub fn at(&self, t: f32) -> Rgba {
        let t = t.clamp(0.0, 1.0) * 3.0;
        let i = (t.floor() as usize).min(2);
        mix(self.ramp[i], self.ramp[i + 1], t - i as f32)
    }

    /// The secondary-emphasis colour (an artist under a title): the pale accent where the accent
    /// comes from the cover, otherwise light neutral text.
    pub fn hi_or_text(&self) -> Rgba {
        match self.theme.accent {
            Accent::Cover => self.hi,
            Accent::Fixed { .. } => self.ink.text2,
        }
    }

    pub fn decor(&self) -> &'static Decor {
        &self.theme.decor
    }

    pub fn surface(&self) -> &'static Surface {
        &self.theme.surface
    }

    pub fn radii(&self) -> Radii {
        self.theme.surface.radii
    }
}

/// `c` at alpha `a`.
fn with_alpha(c: Rgba, a: u8) -> Rgba {
    [c[0], c[1], c[2], a]
}

/// `c` with its alpha scaled by `k`.
fn scale_alpha(c: Rgba, k: f32) -> Rgba {
    with_alpha(c, (f32::from(c[3]) * k).round().clamp(0.0, 255.0) as u8)
}

/// The toolkit theme the look's widgets (text fields, tables, scrollbars, tooltips) read: the
/// dark preset with accent, ink, hover and selection fills and scrollbar thumbs from `look`.
pub fn lune_theme(look: &Look) -> LuneTheme {
    let ink = &look.ink;
    LuneTheme {
        fg_primary: ink.text,
        fg_secondary: ink.text2,
        fg_tertiary: ink.muted,
        fg_quaternary: ink.faint,
        surface_input: ink.wash,
        surface_hover: scale_alpha(ink.wash, 1.6),
        surface_selected: ink.line,
        surface_active: scale_alpha(ink.line, 1.5),
        border: ink.line,
        border_subtle: ink.line_soft,
        accent: look.a,
        on_accent: look.on,
        text_selection: with_alpha(look.a, 0x59),
        scroll_thumb: with_alpha(look.a, 0x8C),
        scroll_thumb_hover: with_alpha(look.a, 0xE6),
        ..lunegit_dark::THEME
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cover_theme_without_art_uses_teal_with_its_own_treatments() {
        let a = Look::new(theme(ThemeId::A), None);
        let c = Look::new(theme(ThemeId::C), None);
        assert_eq!((c.a, c.b, c.on), (a.a, a.b, a.on));
        assert_eq!(c.decor().ring, Ring::Hairline);
        assert_eq!(c.decor().bars, Bars::Gradient);
        assert_eq!(c.ink.text, ADAPTIVE.ink.text);
        assert_eq!(c.surface().panel_alpha, ADAPTIVE.surface.panel_alpha);
        assert_ne!(c.ink.text, a.ink.text);
    }

    #[test]
    fn cover_accent_drives_theme_c_only() {
        let cover = CoverAccent {
            a: [0xD0, 0x40, 0x40, 0xFF],
            b: [0x40, 0x40, 0xD0, 0xFF],
        };
        let c = Look::new(theme(ThemeId::C), Some(cover));
        assert_eq!((c.a, c.b), (cover.a, cover.b));
        let a = Look::new(theme(ThemeId::A), Some(cover));
        assert_eq!(a.a, [0x2E, 0xD3, 0xB0, 0xFF]);
    }

    #[test]
    fn keys_round_trip_and_default_is_c() {
        for id in ThemeId::ALL {
            assert_eq!(ThemeId::from_key(id.key()), Some(id));
            assert_eq!(theme(id).id, id);
        }
        assert_eq!(ThemeId::from_key("x"), None);
        assert_eq!(ThemeId::default(), ThemeId::C);
        let names: Vec<_> = ThemeId::ALL.iter().map(|&i| theme(i).name).collect();
        assert_eq!(names, ["Teal", "Hi-fi copper", "Adaptive"]);
    }

    #[test]
    fn ramp_is_clamped_and_ends_at_the_accent_ends() {
        let k = Look::new(theme(ThemeId::A), None);
        assert_eq!(k.at(-1.0), k.ramp[0]);
        assert_eq!(k.at(2.0), k.ramp[3]);
    }

    #[test]
    fn lune_theme_carries_the_look() {
        let k = Look::new(theme(ThemeId::B), None);
        let t = lune_theme(&k);
        assert_eq!(t.accent, k.a);
        assert_eq!(t.on_accent, k.on);
        assert_eq!(t.fg_primary, k.ink.text);
        assert_eq!(t.text_selection[3], 0x59);
        assert_eq!(t.scroll_thumb[..3], k.a[..3]);
        assert_eq!(t.font_base, lunegit_dark::THEME.font_base);
    }
}
