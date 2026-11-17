//! credential-store accounts (one per stored item) for the Spotify provider. The service name stays in
//! [`whipspot_core::credential_store::SERVICE`]. Exactly these three exist; values are frozen (changing
//! one silently logs existing users out).

/// Playback login: refresh token for the librespot client id.
pub const PLAYBACK_REFRESH: &str = "spotify-refresh-token";
/// Web API login: refresh token for the user's own client id.
pub const WEB_REFRESH: &str = "spotify-web-refresh-token";
/// Web API login: the user's own client id (public, stored for convenience).
pub const WEB_CLIENT_ID: &str = "spotify-web-client-id";

pub const ALL: [&str; 3] = [PLAYBACK_REFRESH, WEB_REFRESH, WEB_CLIENT_ID];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{auth::spotify_consts, web_auth};
    use whipspot_core::credential_store::SERVICE;

    #[test]
    fn account_names_are_distinct_and_fixed() {
        let set: std::collections::HashSet<_> = ALL.iter().collect();
        assert_eq!(set.len(), 3);
        assert_eq!(
            ALL,
            [
                "spotify-refresh-token",
                "spotify-web-refresh-token",
                "spotify-web-client-id"
            ]
        );
        assert!(ALL.iter().all(|a| a.starts_with("spotify-")));
    }

    #[test]
    fn store_names_are_frozen() {
        assert_eq!(PLAYBACK_REFRESH, "spotify-refresh-token");
        assert_eq!(WEB_REFRESH, "spotify-web-refresh-token");
        assert_eq!(WEB_CLIENT_ID, "spotify-web-client-id");
        assert_eq!(SERVICE, "whipspot");
    }

    #[test]
    fn legacy_constants_point_here() {
        // The spike-era constants in auth/web_auth must not drift from the single definition.
        assert_eq!(spotify_consts::STORE_ACCOUNT, PLAYBACK_REFRESH);
        assert_eq!(spotify_consts::STORE_SERVICE, SERVICE);
        assert_eq!(web_auth::consts::STORE_REFRESH_ACCOUNT, WEB_REFRESH);
        assert_eq!(web_auth::consts::STORE_CLIENT_ID_ACCOUNT, WEB_CLIENT_ID);
    }
}
