//! One panel window of the skin front-end as a [`WindowView`]: the host and the dumps draw a
//! skin panel the way they draw any other view.

use lune_ui::image::ImageStore;
use lune_ui::shortcuts::{CommandId, Shortcuts};
use lune_ui::ui::Ui;
use whipspot_core::player::PlayerHandle;
use whipspot_ui::DIAGNOSTICS_KEY;
use whipspot_ui::skin::SkinUi;
use whipspot_ui::title::{WindowTitle, window_title};

use super::WindowView;

/// Panel `panel` of a running skin, drawn by `skin`.
pub struct PanelView<'s> {
    skin: &'s mut SkinUi,
    panel: usize,
}

impl<'s> PanelView<'s> {
    pub fn new(skin: &'s mut SkinUi, panel: usize) -> Self {
        PanelView { skin, panel }
    }
}

impl WindowView for PanelView<'_> {
    fn frame(&mut self, ui: &mut Ui<'_>, player: &PlayerHandle) {
        self.skin.frame_panel(self.panel, ui, player);
    }

    fn images(&mut self, store: &mut dyn ImageStore) {
        self.skin.images(store);
    }

    fn shortcuts(&self) -> Shortcuts {
        self.skin.shortcuts()
    }

    fn fired(&mut self, fired: &[CommandId], text_focused: bool, player: &PlayerHandle) {
        self.skin.fired(fired, text_focused, player);
    }

    fn take_toggles(&mut self) -> Vec<&'static str> {
        if self.skin.take_toggle_diagnostics() {
            vec![DIAGNOSTICS_KEY]
        } else {
            Vec::new()
        }
    }

    /// The first panel is the taskbar button: it names the track like the native window.
    fn title(&self, player: &PlayerHandle) -> Option<WindowTitle> {
        (self.panel == 0).then(|| window_title(&player.state()))
    }
}
