//! Playback login: interactive PKCE via `librespot-oauth` (librespot's public client id, scope
//! `streaming`). Token storage and refresh live in `auth_manager`/`credential_store`.
//!
//! Nothing here formats a token with `{:?}`/`{}`; tokens live in [`Secret`], whose Debug and
//! Display are redacted. NOTE: `librespot-oauth` (and librespot's token provider) log full token
//! responses at `trace!`; never enable `trace` logging for librespot crates.

use std::{
    fmt,
    io::Write,
    net::{Ipv4Addr, SocketAddr, TcpStream},
    sync::mpsc,
    time::{Duration, Instant},
};

pub use whipspot_core::secret::Secret;

use anyhow::{Result, anyhow, bail};
use librespot_oauth::{OAuthClient, OAuthClientBuilder, OAuthError, OAuthToken};

/// Constants for the Spotify OAuth flow.
pub mod spotify_consts {
    /// Public librespot/Spotify client id, not a secret. Source: librespot-core 0.8.0
    /// `src/config.rs:6` (`KEYMASTER_CLIENT_ID`) and librespot-oauth 0.8.0 `examples/oauth_async.rs`.
    pub const CLIENT_ID: &str = "65b708073fc0480ea92a077233ca87bd";
    /// Loopback redirect registered for this client id (librespot-oauth example).
    pub const REDIRECT_URI: &str = "http://127.0.0.1:8898/login";
    /// Loopback listener address derived from `REDIRECT_URI`.
    pub const LISTEN_PORT: u16 = 8898;
    /// Playback login requests `streaming` only (least privilege; the Web API uses a separate
    /// login with the user's own client id, see `web_auth`).
    pub const SCOPES_STREAMING: &[&str] = &["streaming"];
    /// Credential store location of the refresh token (defined once in `credential_store`).
    pub const STORE_SERVICE: &str = whipspot_core::credential_store::SERVICE;
    pub const STORE_ACCOUNT: &str = crate::accounts::PLAYBACK_REFRESH;
    /// Login gives up after this long.
    pub const LOGIN_TIMEOUT_SECS: u64 = 120;
}

use spotify_consts as consts;

/// Why an interactive login did not complete. Attached to the `anyhow` error returned by the
/// blocking login functions so callers can classify without parsing text
/// (`err.downcast_ref::<LoginError>()`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginError {
    Timeout,
    PortInUse,
    /// The user (or Spotify) ended the login with an error callback, e.g. `access_denied`.
    Denied,
}

impl fmt::Display for LoginError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            LoginError::Timeout => "login timed out",
            LoginError::PortInUse => "the login port is already in use",
            LoginError::Denied => "login was not completed",
        })
    }
}

impl std::error::Error for LoginError {}

/// An OAuth token pair with redacting formatting (replaces direct use of `OAuthToken`).
#[derive(Debug, Clone)]
pub struct Tokens {
    pub access: Secret,
    /// Empty when Spotify did not return one (no rotation).
    pub refresh: Secret,
    pub expires_at: Instant,
    pub scopes: Vec<String>,
}

impl From<OAuthToken> for Tokens {
    fn from(t: OAuthToken) -> Self {
        Tokens {
            access: Secret::new(t.access_token),
            refresh: Secret::new(t.refresh_token),
            expires_at: t.expires_at,
            scopes: t.scopes,
        }
    }
}

/// A refreshed refresh token overwrites the stored one only when non-empty.
pub fn should_store_refresh(new: &str) -> bool {
    !new.is_empty()
}

fn build_client(scopes: &[&str]) -> Result<OAuthClient> {
    OAuthClientBuilder::new(consts::CLIENT_ID, consts::REDIRECT_URI, scopes.to_vec())
        .open_in_browser()
        .with_custom_message("whipspot: login complete, you can close this tab.")
        .build()
        .map_err(|e| anyhow!("building OAuth client: {e}"))
}

/// Interactive PKCE login for one scope set.
///
/// `librespot-oauth` runs a blocking `accept()` with no timeout, so this runs on a plain thread
/// (not a tokio worker; its sync flow also uses `reqwest::blocking`). After `timeout` a throwaway
/// connection to the listener unblocks the accept so the thread ends.
pub fn login_blocking(scopes: &[&str], timeout: Duration) -> Result<Tokens> {
    let client = build_client(scopes)?;
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        let _ = tx.send(client.get_access_token());
    });
    let result = match rx.recv_timeout(timeout) {
        Ok(r) => r,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, consts::LISTEN_PORT));
            if let Ok(mut s) = TcpStream::connect_timeout(&addr, Duration::from_secs(2)) {
                let _ = s.flush();
            }
            // The listener thread now errors out (empty request) and reports on the channel.
            let _ = rx.recv_timeout(Duration::from_secs(5));
            let _ = handle.join();
            return Err(anyhow::Error::new(LoginError::Timeout)
                .context(format!("no callback within {}s", timeout.as_secs())));
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => bail!("login thread ended unexpectedly"),
    };
    let _ = handle.join();
    // OAuthError's Display never includes tokens.
    result.map(Tokens::from).map_err(|e| match &e {
        OAuthError::AuthCodeListenerBind { e: io, .. }
            if io.kind() == std::io::ErrorKind::AddrInUse =>
        {
            anyhow::Error::new(LoginError::PortInUse)
        }
        _ => anyhow!("login failed: {e}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_never_formats_value() {
        let s = Secret::new("AQDsupersecretvalue");
        assert!(!format!("{s:?}").contains("supersecret"));
        assert!(!format!("{s}").contains("supersecret"));
        let t = Tokens {
            access: s.clone(),
            refresh: s,
            expires_at: Instant::now(),
            scopes: vec![],
        };
        assert!(!format!("{t:?}").contains("supersecret"));
    }

    #[test]
    fn empty_refresh_is_not_stored() {
        assert!(!should_store_refresh(""));
        assert!(should_store_refresh("x"));
    }
}
