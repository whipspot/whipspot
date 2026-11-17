//! What a skin element becomes: the fixed tables that map element names, `onclick` text and
//! slider values onto the core's neutral commands (C3, D5). Nothing here runs script: an
//! `onclick` only selects a command from the table, and anything not in the table is inert.

use whipspot_core::engine::BAND_COUNT;
use whipspot_core::skin::{Command, PanelId, PlayerCommand, Rgb, Value, WindowCommand};

/// The id of the composed playlist panel, which `TogglePlaylist();` opens and closes.
pub(crate) fn playlist_id() -> PanelId {
    PanelId::new("playlist").expect("a fixed valid panel id")
}

/// The id of the composed library panel.
pub(crate) fn library_id() -> PanelId {
    PanelId::new("library").expect("a fixed valid panel id")
}

/// `#RRGGBB`, `#RGB`, `black` or `white` (any case). `none` and anything else is `None`.
pub(crate) fn parse_colour(text: &str) -> Option<Rgb> {
    let t = text.trim();
    if t.eq_ignore_ascii_case("black") {
        return Some(Rgb { r: 0, g: 0, b: 0 });
    }
    if t.eq_ignore_ascii_case("white") {
        return Some(Rgb {
            r: 255,
            g: 255,
            b: 255,
        });
    }
    let hex = t.strip_prefix('#')?;
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let digit = |i: usize| u8::from_str_radix(&hex[i..=i], 16).ok();
    let pair = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    match hex.len() {
        6 => Some(Rgb {
            r: pair(0)?,
            g: pair(2)?,
            b: pair(4)?,
        }),
        3 => Some(Rgb {
            r: digit(0)? * 17,
            g: digit(1)? * 17,
            b: digit(2)? * 17,
        }),
        _ => None,
    }
}

/// An `onclick` for table lookup: a `jscript:` prefix is dropped, all ASCII whitespace removed,
/// the rest lowercased and one trailing `;` stripped.
pub(crate) fn normalise_onclick(text: &str) -> String {
    let t = text.trim();
    let body = match t.get(..8) {
        Some(head) if head.eq_ignore_ascii_case("jscript:") => &t[8..],
        _ => t,
    };
    let mut s: String = body
        .chars()
        .filter(|c| !c.is_ascii_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    if s.ends_with(';') {
        s.pop();
    }
    s
}

/// The command an `onclick` selects; `None` for everything not in the table.
pub(crate) fn onclick_command(text: &str) -> Option<Command> {
    Some(match normalise_onclick(text).as_str() {
        "view.minimize()" => Command::Window(WindowCommand::Minimize),
        "view.close()" => Command::Window(WindowCommand::ClosePanel),
        "player.controls.play()" => Command::Player(PlayerCommand::Play),
        "player.controls.pause()" => Command::Player(PlayerCommand::Pause),
        "player.controls.stop()" => Command::Player(PlayerCommand::Stop),
        "player.controls.next()" => Command::Player(PlayerCommand::Next),
        "player.controls.previous()" => Command::Player(PlayerCommand::Previous),
        "toggleplaylist()" => Command::Window(WindowCommand::TogglePanel(playlist_id())),
        _ => return None,
    })
}

/// The command of an element that is a transport control by its name alone.
pub(crate) fn tag_command(tag: &str) -> Option<Command> {
    Some(Command::Player(match tag {
        "playelement" | "playbutton" => PlayerCommand::TogglePlay,
        "pauseelement" | "pausebutton" => PlayerCommand::Pause,
        "stopelement" | "stopbutton" => PlayerCommand::Stop,
        "nextelement" | "nextbutton" => PlayerCommand::Next,
        "prevelement" | "prevbutton" => PlayerCommand::Previous,
        _ => return None,
    }))
}

/// The elements of a `BUTTONGROUP` whose rectangle comes from the group's mapping image.
pub(crate) fn is_group_element(tag: &str) -> bool {
    matches!(
        tag,
        "buttonelement"
            | "playelement"
            | "pauseelement"
            | "stopelement"
            | "nextelement"
            | "prevelement"
    )
}

/// The value a slider element edits, if it edits one the core knows.
pub(crate) fn slider_value(tag: &str, value_attr: Option<&str>) -> Option<Value> {
    match tag {
        "seekslider" => Some(Value::Position),
        "volumeslider" => Some(Value::Volume),
        "balanceslider" => Some(Value::Balance),
        "slider" => match value_attr?.trim().to_ascii_lowercase().as_str() {
            "wmpprop:player.controls.currentposition" => Some(Value::Position),
            "wmpprop:player.settings.volume" => Some(Value::Volume),
            "wmpprop:player.settings.balance" => Some(Value::Balance),
            v => eq_band(v),
        },
        _ => None,
    }
}

/// `wmpprop:<id>.gainlevel<n>` (lowercased) for `n` in `1..=BAND_COUNT`: band `n - 1` of an
/// equalizer element. The id is not checked: the core has one equalizer.
fn eq_band(value: &str) -> Option<Value> {
    let rest = value.strip_prefix("wmpprop:")?;
    let (id, n) = rest.split_once(".gainlevel")?;
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        return None;
    }
    if n.is_empty() || n.len() > 2 || !n.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: usize = n.parse().ok()?;
    (1..=BAND_COUNT)
        .contains(&n)
        .then(|| Value::EqBand(u8::try_from(n - 1).unwrap_or(0)))
}

/// The function an `onclick` calls when it is one bare call without arguments, `name()` (after
/// [`normalise_onclick`]), and not a command of the table: a skin's own script function. The
/// script is never read; the name only links the buttons that call the same function.
pub(crate) fn bare_call(text: &str) -> Option<String> {
    if onclick_command(text).is_some() {
        return None;
    }
    let s = normalise_onclick(text);
    let name = s.strip_suffix("()")?;
    let mut bytes = name.bytes();
    let first = bytes.next()?;
    let ok = (first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
        && name.len() <= 64;
    ok.then(|| s.clone())
}

/// Elements whose picture is the `backgroundImage` attribute rather than `image`.
pub(crate) fn uses_background_image(tag: &str) -> bool {
    matches!(
        tag,
        "view"
            | "subview"
            | "slider"
            | "seekslider"
            | "volumeslider"
            | "balanceslider"
            | "customslider"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours() {
        assert_eq!(
            parse_colour("#FF00FF"),
            Some(Rgb {
                r: 255,
                g: 0,
                b: 255
            })
        );
        assert_eq!(
            parse_colour(" #0f8 "),
            Some(Rgb {
                r: 0,
                g: 255,
                b: 136
            })
        );
        assert_eq!(parse_colour("White"), parse_colour("#ffffff"));
        assert_eq!(parse_colour("BLACK"), parse_colour("#000"));
        for bad in [
            "none",
            "",
            "#",
            "#12",
            "#1234",
            "#GG0000",
            "red",
            "#ff00ff00",
            "ÿ",
        ] {
            assert_eq!(parse_colour(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn onclick_table_is_exact_after_normalising() {
        let win = |c| Some(Command::Window(c));
        assert_eq!(
            onclick_command("view.minimize();"),
            win(WindowCommand::Minimize)
        );
        assert_eq!(
            onclick_command("  JScript: View . Close ( ) ; "),
            win(WindowCommand::ClosePanel)
        );
        assert_eq!(
            onclick_command("TogglePlaylist();"),
            win(WindowCommand::TogglePanel(playlist_id()))
        );
        assert_eq!(
            onclick_command("player.controls.next()"),
            Some(Command::Player(PlayerCommand::Next))
        );
        for inert in [
            "OpenMedia();",
            "vis.next();",
            "eq.reset();",
            "view.close();view.minimize();",
            "view.close(); x",
            "",
            "player.controls.play();;",
            "ToggleEqualizer();",
        ] {
            assert_eq!(onclick_command(inert), None, "{inert:?}");
        }
    }

    #[test]
    fn bare_calls_are_script_functions_only() {
        assert_eq!(
            bare_call("ToggleEqView();"),
            Some("toggleeqview()".to_owned())
        );
        assert_eq!(
            bare_call(" jscript: Toggle_Pl2 ( ) "),
            Some("toggle_pl2()".to_owned())
        );
        for no in [
            "view.close();",
            "TogglePlaylist();",
            "eq.reset();",
            "Toggle(1);",
            "a();b();",
            "2go()",
            "()",
            "",
        ] {
            assert_eq!(bare_call(no), None, "{no:?}");
        }
        assert_eq!(bare_call(&format!("{}()", "a".repeat(65))), None);
    }

    #[test]
    fn slider_values() {
        assert_eq!(slider_value("seekslider", None), Some(Value::Position));
        assert_eq!(
            slider_value("slider", Some("wmpprop:player.settings.volume")),
            Some(Value::Volume)
        );
        assert_eq!(
            slider_value("slider", Some("wmpprop:player.settings.balance")),
            Some(Value::Balance)
        );
        assert_eq!(
            slider_value("slider", Some("wmpprop:eq.gainLevel1")),
            Some(Value::EqBand(0))
        );
        assert_eq!(
            slider_value("slider", Some(" WMPPROP:myEq.GainLevel10 ")),
            Some(Value::EqBand(9))
        );
        for no in [
            "wmpprop:eq.gainLevel0",
            "wmpprop:eq.gainLevel11",
            "wmpprop:eq.gainLevel",
            "wmpprop:.gainLevel1",
            "wmpprop:e q.gainLevel1",
            "wmpprop:eq.gainLevel1x",
            "wmpprop:eq.gainLevel001",
            "eq.gainLevel1",
        ] {
            assert_eq!(slider_value("slider", Some(no)), None, "{no:?}");
        }
        assert_eq!(slider_value("slider", None), None);
        assert_eq!(slider_value("customslider", None), None);
    }
}
