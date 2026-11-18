//! What differs per OS in the host: the window chrome and the fallback font files. This is the
//! only `cfg(target_os)` code of the host; `whipspot-ui` gets the chrome as data.

use whipspot_ui::Chrome;

/// The chrome of this OS: Windows draws the caption buttons (a borderless window), macOS keeps its
/// traffic lights (the band is drawn under them), anything else keeps native decorations.
pub fn chrome() -> Chrome {
    #[cfg(target_os = "windows")]
    {
        Chrome {
            drawn_caption: true,
            // Three 46 px buttons and a margin.
            title_inset: 3.0 * 46.0 + 8.0,
        }
    }
    #[cfg(target_os = "macos")]
    {
        Chrome {
            drawn_caption: false,
            title_inset: 78.0,
        }
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        Chrome {
            drawn_caption: false,
            title_inset: 78.0,
        }
    }
}

/// Asks the user for a folder with the OS dialog and returns it as text; `None` when the dialog
/// was cancelled or the path is not valid text. Blocks until the dialog closes, so it runs on the
/// UI thread (macOS requires the main thread, D12). No path is logged.
pub fn pick_folder() -> Option<String> {
    rfd::FileDialog::new()
        .set_title("Choose a music folder")
        .pick_folder()
        .and_then(|p| p.into_os_string().into_string().ok())
}

/// What the tray icon opens: the menu under the bar on macOS, over the bar elsewhere (a click of
/// either button).
pub(super) fn tray_surface() -> lune_ui::tray::TraySurface {
    #[cfg(target_os = "macos")]
    {
        lune_ui::tray::TraySurface::MenuUnderBar
    }
    #[cfg(not(target_os = "macos"))]
    {
        lune_ui::tray::TraySurface::MenuOverBar
    }
}

/// Whether the taskbar shows a scrolling window title: Windows does (MD52); macOS hides the title
/// of its glass windows and only the Dock and Window menu read it, so it stays static there.
pub(super) const MARQUEE: bool = cfg!(target_os = "windows");

/// Fallback faces (emoji, CJK) added by path after the bundled ones; a missing file is skipped.
#[cfg(target_os = "windows")]
pub const FALLBACK_FONTS: &[&str] = &[
    r"C:\Windows\Fonts\seguiemj.ttf",
    r"C:\Windows\Fonts\msyh.ttc",
    r"C:\Windows\Fonts\YuGothR.ttc",
    r"C:\Windows\Fonts\malgun.ttf",
];

#[cfg(target_os = "macos")]
pub const FALLBACK_FONTS: &[&str] = &[
    "/System/Library/Fonts/Apple Color Emoji.ttc",
    "/System/Library/Fonts/Hiragino Sans GB.ttc",
    "/System/Library/Fonts/AppleSDGothicNeo.ttc",
];

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub const FALLBACK_FONTS: &[&str] = &[];

/// Whether the OS can say which mouse buttons are down ([`mouse_buttons_down`]): macOS. The host
/// then checks a held button of a skin panel against it on the follow tick; elsewhere it never
/// ticks for that.
pub(super) const OS_BUTTONS: bool = cfg!(target_os = "macos");

/// The mouse buttons the OS holds down now as a bit mask (bit 0 left, 1 right, 2 middle, then by
/// number); `None` where the OS cannot say, which changes nothing.
pub(super) fn mouse_buttons_down() -> Option<u64> {
    #[cfg(target_os = "macos")]
    {
        crate::platform_macos::mouse_buttons_down()
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}
