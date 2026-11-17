//! whipspot-skin-winamp: the engine that turns a classic skin archive into the core's skin model
//! (the core's skin seam, C5).
//!
//! A skin archive is untrusted input (hard rule 5): the parsers here cap every size, never use an
//! entry name as a path and never panic on malformed bytes. [`WinampEngine`] is the one public
//! item; `text` is the parser and `geometry` the transcribed coordinate tables. The zip and BMP
//! readers are in `whipspot-skin-kit`.

#[cfg(feature = "demo")]
pub mod demo;
mod engine;
#[cfg(test)]
mod fuzz;
mod geometry;
#[cfg(any(test, feature = "demo"))]
#[cfg_attr(not(test), allow(dead_code))]
mod synth;
mod text;

pub use engine::WinampEngine;
