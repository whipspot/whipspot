//! Engine errors. Every `Display` text is fixed (never a library or OS message).

use std::fmt;

use super::render::Renderer;

/// Why a [`super::SourceWriter`] call failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceError {
    /// No output is open.
    NoOutput,
    /// The open output failed; nothing plays until it is reopened.
    OutputFailed,
    /// The output stopped consuming for longer than the stall timeout.
    Stalled,
    /// `write` before `start_track`.
    NoTrack,
    /// The stream format is not supported.
    Unsupported,
}

impl fmt::Display for SourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            SourceError::NoOutput => "no audio output is open",
            SourceError::OutputFailed => "the audio output failed",
            SourceError::Stalled => "the audio output stopped consuming",
            SourceError::NoTrack => "no track was started",
            SourceError::Unsupported => "unsupported stream format",
        })
    }
}

impl std::error::Error for SourceError {}

/// Why an output could not be listed or opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputError {
    /// The engine has no output adapter.
    NoAdapter,
    /// The requested device does not exist.
    NoDevice,
    /// The device cannot run the requested format or mode.
    Unsupported,
    /// The device or the engine's output is already in use.
    Busy,
    /// Anything else.
    Failed,
}

impl fmt::Display for OutputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            OutputError::NoAdapter => "no audio output adapter",
            OutputError::NoDevice => "audio device not found",
            OutputError::Unsupported => "audio format not supported by the device",
            OutputError::Busy => "audio output is busy",
            OutputError::Failed => "audio output failed",
        })
    }
}

impl std::error::Error for OutputError {}

/// A failure of a running output stream, raised through a [`super::FaultSignal`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputFault {
    DeviceLost,
    Failed,
}

impl fmt::Display for OutputFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            OutputFault::DeviceLost => "the audio device was lost",
            OutputFault::Failed => "the audio stream failed",
        })
    }
}

impl std::error::Error for OutputFault {}

/// A failed [`super::AudioOutput::open`]: the error and the renderer handed back.
#[derive(Debug)]
pub struct OpenFailed {
    pub error: OutputError,
    pub renderer: Renderer,
}

impl fmt::Display for OpenFailed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.error, f)
    }
}

impl std::error::Error for OpenFailed {}
