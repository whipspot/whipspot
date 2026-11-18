//! What the host asks AppKit itself (macOS only): the mouse buttons the OS holds down now. Safe
//! calls only; no `unsafe` here.

use objc2_app_kit::NSEvent;

/// The mouse buttons down now as AppKit's bit mask (bit 0 left, 1 right, 2 middle, then the
/// other buttons by number), whatever window has them. `None` is never returned on macOS; the
/// `Option` matches the other systems, which cannot say.
pub(crate) fn mouse_buttons_down() -> Option<u64> {
    Some(NSEvent::pressedMouseButtons() as u64)
}
