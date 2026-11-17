//! whipspot-skin-wmp: the engine that turns a Windows Media Player skin archive (`.wmz`) into the
//! core's skin model (the core's skin seam, C5).
//!
//! The archive and the `.wms` description inside it are untrusted input (hard rule 5): every
//! reader caps its sizes, no archive entry name is used as a path, and no script in the skin is
//! read, inflated or run. [`WmpEngine`] is the one public item; `wms` is the lenient description
//! reader, `geometry` the bounded arithmetic that resolves element positions, `compose` the static
//! picture of the first view, `widgets` the fixed tables from elements to commands and `engine`
//! the assembly of the skin.

mod compose;
mod engine;
mod geometry;
#[cfg(test)]
mod synth;
#[cfg(test)]
mod tests;
mod widgets;
mod wms;

pub use engine::WmpEngine;
