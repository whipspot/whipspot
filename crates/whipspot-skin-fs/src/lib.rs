//! whipspot-skin-fs: the skins folder as a `SkinSource` (the core's skin seam, C5).

mod dir;
#[cfg(test)]
mod fuzz;

pub use dir::{DirSkinSource, MAX_LISTED, MAX_STEM_CHARS};
