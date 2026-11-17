//! The Spotify provider: librespot playback, PKCE logins, credential-store accounts and the Web API
//! library. May import whipspot-core only (never another adapter).

pub mod accounts;
pub mod auth;
pub mod auth_flow;
pub mod auth_manager;
pub mod connect;
pub mod core_sink;
pub mod events;
pub mod library;
pub mod playback;
pub mod player_host;
pub mod provider;
pub mod session;
pub mod trace;
pub mod uri;
pub(crate) mod watch;
pub mod web_auth;
pub mod webapi;

pub use provider::{Quality, SpotifyOutput, SpotifyProvider};
