//! The output seam: what an output adapter (cpal, a test fake) implements.

use super::error::{OpenFailed, OutputError};
use super::format::{OutputDevice, OutputInfo, OutputMode, OutputRequest};
use super::render::Renderer;

/// An output adapter. The engine moves its [`Renderer`] in at `open` and gets it back at
/// `close` (or inside [`OpenFailed`]).
///
/// An adapter calls [`Renderer::prepare`] with the format it opened before its stream starts,
/// then calls [`Renderer::render`] from its audio callback.
pub trait AudioOutput: Send {
    fn devices(&mut self) -> Result<Vec<OutputDevice>, OutputError>;
    // `OpenFailed` carries the renderer back by value; the error path is cold.
    #[allow(clippy::result_large_err)]
    fn open(
        &mut self,
        request: &OutputRequest,
        renderer: Renderer,
    ) -> Result<OutputInfo, OpenFailed>;
    /// Stop the stream and give the renderer back; `None` when nothing is open (or the
    /// renderer was lost).
    fn close(&mut self) -> Option<Renderer>;
    /// The modes this adapter can open.
    fn modes(&self) -> Vec<OutputMode> {
        vec![OutputMode::Shared]
    }
}

/// A device buffer to fill: interleaved, the device's channel count.
#[derive(Debug)]
pub enum OutBuf<'a> {
    F32(&'a mut [f32]),
    I16(&'a mut [i16]),
    I32(&'a mut [i32]),
}

impl OutBuf<'_> {
    /// Number of samples (not frames).
    pub fn len(&self) -> usize {
        match self {
            OutBuf::F32(b) => b.len(),
            OutBuf::I16(b) => b.len(),
            OutBuf::I32(b) => b.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub(crate) fn zero(&mut self) {
        match self {
            OutBuf::F32(b) => b.fill(0.0),
            OutBuf::I16(b) => b.fill(0),
            OutBuf::I32(b) => b.fill(0),
        }
    }
}

/// What the adapter knows about the callback's timing.
#[derive(Clone, Copy, Debug, Default)]
pub struct RenderTiming {
    /// Estimated time from this callback to the first frame reaching the DAC.
    pub latency_ns: Option<u64>,
}
