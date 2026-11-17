//! whipspot-skin-kit: the format-neutral readers and helpers every skin engine shares: a bounded
//! zip reader and a BMP decoder, PNG and JPEG wrappers (cargo features `png` and `jpeg`), the
//! window-shape tracer, a built-in pixel font and the composed list panels.
//!
//! A skin archive is untrusted input (hard rule 5): every reader here caps its sizes, never
//! uses an entry name as a path and never panics on malformed bytes. The kit names no skin
//! format and implements no core trait.

pub mod bmp;
pub mod font;
pub mod image;
pub mod panels;
pub mod shape;
#[cfg(any(test, feature = "synth"))]
pub mod synth;
pub mod zip;
