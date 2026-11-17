//! `whipspot-net`: the network adapter for the core's art and lyrics seams (C7, D47).
//!
//! It owns its own reqwest client (https only, no redirects, no cookies, no referer, byte and
//! time caps) and never shares one with another adapter. Only the composition root constructs
//! its types. Every failure is a fixed `ArtError` or `LyricsError` kind; nothing here logs.

mod allow;
mod art;
mod cache;
mod client;
mod decode;
#[cfg(test)]
mod fuzz;
mod lrc;
mod lrclib;
#[doc(hidden)]
pub mod transport;

pub use art::NetArt;
pub use lrclib::Lrclib;
