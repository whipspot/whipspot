//! Headless frame dump: one view drawn offscreen with the bundled fonts and written as a PNG, for
//! agents and the director to look at (D40). No window, no OS fonts, no wake.
//!
//! [`Dumper`] runs frames one at a time over injected input, so a script can open a list, type a
//! query and wait for the answers before the capture; [`dump`] is the two-frame shortcut.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, anyhow};
use lune_ui::dump;
use lune_ui::frame::FrameRenderer;
use lune_ui::gpu::{self, Gpu};
use lune_ui::input::{Event, RawInput};
use lune_ui::text::TextCache;
use whipspot_core::player::PlayerHandle;

use super::WindowView;
use super::fonts;
use super::frame::{UiState, params};

/// The format of the offscreen target.
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8Unorm;

/// One headless window of `size` logical pixels at `scale`: its own device, target and UI state.
pub struct Dumper {
    gpu: Gpu,
    text: TextCache,
    frame: FrameRenderer,
    texture: wgpu::Texture,
    target: wgpu::TextureView,
    ui: UiState,
    size: (u32, u32),
    scale: f32,
    physical: (u32, u32),
    time: Duration,
}

impl Dumper {
    /// A dumper for `view`; the PNG is `size * scale` physical pixels.
    pub fn new(view: &mut dyn WindowView, size: (u32, u32), scale: f32) -> anyhow::Result<Self> {
        let mut gpu = pollster::block_on(Gpu::new(gpu::instance(None), None))
            .map_err(|e| anyhow!("gpu: {e}"))?;
        let text = fonts::bundled_text_cache();
        let frame = FrameRenderer::new(&mut gpu, FORMAT);
        let physical = (
            (size.0 as f32 * scale).round() as u32,
            (size.1 as f32 * scale).round() as u32,
        );
        let texture = dump::target(&gpu.device, FORMAT, physical);
        let target = texture.create_view(&wgpu::TextureViewDescriptor::default());
        view.set_maximized(false);
        Ok(Self {
            gpu,
            text,
            frame,
            texture,
            target,
            ui: UiState::new(view.shortcuts()),
            size,
            scale,
            physical,
            time: Duration::ZERO,
        })
    }

    /// Draws one frame with `events` as its input, `advance` after the previous frame's app
    /// time (zero: the same instant, so no animation moves).
    pub fn step(
        &mut self,
        view: &mut dyn WindowView,
        player: &PlayerHandle,
        events: Vec<Event>,
        advance: Duration,
    ) -> anyhow::Result<()> {
        self.time += advance;
        let input = RawInput {
            size: (self.size.0 as f32, self.size.1 as f32),
            scale: self.scale,
            time: self.time,
            focused: true,
            events,
        };
        view.images(&mut self.gpu);
        let (scene, _out) = self.ui.run(&input, &mut self.text, view, player);
        self.frame
            .draw(
                &mut self.gpu,
                &mut self.text,
                &scene,
                &self.target,
                params(self.physical, self.scale),
            )
            .map_err(|e| anyhow!("draw: {e}"))?;
        self.gpu.end_frame(&mut self.text);
        Ok(())
    }

    /// Writes the last frame to `path` (its directory is made if needed).
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        self.save_masked(path, |_, _| true)
    }

    /// [`Dumper::save`] with every pixel `keep(x, y)` refuses made fully transparent: a window
    /// whose OS shape cuts a corner off shows the desktop there, so its dump does too.
    pub fn save_masked(&self, path: &Path, keep: impl Fn(u32, u32) -> bool) -> anyhow::Result<()> {
        let mut rgba = dump::read_rgba(&self.gpu.device, &self.gpu.queue, &self.texture)
            .map_err(|e| anyhow!("{e}"))?;
        let width = self.physical.0 as usize;
        for (i, px) in rgba.chunks_exact_mut(4).enumerate() {
            if !keep((i % width) as u32, (i / width) as u32) {
                px.fill(0);
            }
        }
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        }
        dump::write_png(path, self.physical, &rgba).map_err(|e| anyhow!("{e}"))
    }
}

/// Draws `view` at `size` logical pixels and `scale`: two frames a second apart, then the
/// second one is captured to `path`.
pub fn dump(
    view: &mut dyn WindowView,
    player: &PlayerHandle,
    size: (u32, u32),
    scale: f32,
    path: &Path,
) -> anyhow::Result<()> {
    let mut dumper = Dumper::new(view, size, scale)?;
    dumper.step(view, player, Vec::new(), Duration::ZERO)?;
    dumper.step(view, player, Vec::new(), Duration::from_secs(1))?;
    dumper.save(path)
}
