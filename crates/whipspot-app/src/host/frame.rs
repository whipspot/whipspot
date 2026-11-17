//! One UI frame, shared by the window host and the headless dump so both draw the same way.

use lune_ui::input::{FrameOutput, RawInput, keymap};
use lune_ui::lunegit_dark::THEME;
use lune_ui::motion::{Cadence, MotionTier};
use lune_ui::scene::{FrameParams, Scene};
use lune_ui::shortcuts::Shortcuts;
use lune_ui::strings::{DisplayFormat, StringTable};
use lune_ui::text::TextCache;
use lune_ui::ui::{FrameConfig, Ui, UiStore};
use whipspot_core::player::PlayerHandle;

use super::WindowView;

/// The backdrop blur of the glass layers (luneterm's medium), logical px.
const BLUR_SIGMA: f32 = 8.0;
/// The backdrop saturation boost.
const BACKDROP_SATURATION: f32 = 1.8;

/// What one window keeps between frames: the retained widget state, the strings and the shortcut
/// table.
pub(super) struct UiState {
    store: UiStore,
    strings: StringTable,
    shortcuts: Shortcuts,
}

impl UiState {
    /// Fresh state for a window whose view's shortcut table is `shortcuts`.
    pub(super) fn new(shortcuts: Shortcuts) -> Self {
        Self {
            store: UiStore::default(),
            strings: StringTable::default(),
            shortcuts,
        }
    }

    /// Runs `view` over `input` and returns the scene and what the frame asks of the window; the
    /// view's own window commands are appended to the frame's.
    pub(super) fn run(
        &mut self,
        input: &RawInput,
        text: &mut TextCache,
        view: &mut dyn WindowView,
        player: &PlayerHandle,
    ) -> (Scene, FrameOutput) {
        let config = FrameConfig {
            theme: view.theme().unwrap_or(THEME),
            tier: MotionTier::Full,
            busy_cadence: Cadence::BUSY,
            strings: &self.strings,
            arg_format: &DisplayFormat,
            keymap: keymap(),
            shortcuts: &self.shortcuts,
        };
        let mut ui = Ui::begin(&mut self.store, input, config, text);
        view.frame(&mut ui, player);
        let (scene, mut out) = ui.finish();
        // `ime_area` is set while a text field has focus.
        view.fired(&out.fired, out.ime_area.is_some(), player);
        out.commands.extend(view.take_commands());
        (scene, out)
    }
}

/// The renderer's parameters for a target of `size` physical pixels at `scale`.
pub(super) fn params(size: (u32, u32), scale: f32) -> FrameParams {
    FrameParams {
        size,
        scale,
        blur_sigma: BLUR_SIGMA * scale,
        saturation: BACKDROP_SATURATION,
        ..FrameParams::default()
    }
}
