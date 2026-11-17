//! Output adapters for the whipspot audio engine. [`OutputRouter`] is the engine's adapter
//! (MD43): shared mode through the cpal output (Windows WASAPI, macOS CoreAudio), exclusive
//! mode through the WASAPI exclusive output (Windows only).

mod cpal_out;
pub mod exclusive;
mod router;
#[cfg(windows)]
mod wasapi_out;

pub use cpal_out::CpalOutput;
pub use router::OutputRouter;
