//! The YouTube Music provider (adapter). It owns every YouTube-specific thing: the InnerTube
//! client for the library and likes, the stream-helper tools, the stream resolution and the
//! decode of the audio it fetches. The core sees only the provider traits.

pub mod auth;
pub mod cookies;
pub mod http;
pub mod ids;
pub mod innertube;
pub mod library;
pub mod parse;
pub mod playback;
pub mod provider;
pub mod resolve;
pub mod run;
pub mod source;
pub mod tools;

pub use provider::{ID as PROVIDER_ID, StartupUpdate, YtDeps, YtMusicProvider};
