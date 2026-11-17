//! The one reqwest client configuration used by every request of this crate.

use std::time::Duration;

/// Connect timeout for every request.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// The builder with every rule of this crate: no redirects, no referer, no default headers, no
/// cookie store (the feature is off), a fixed User-Agent and nothing else identifying.
pub(crate) fn builder(timeout: Duration, https_only: bool) -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .https_only(https_only)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(timeout)
        .connect_timeout(CONNECT_TIMEOUT)
        .referer(false)
        .user_agent(concat!("Whipspot/", env!("CARGO_PKG_VERSION")))
}

/// The client. Production passes `https_only = true`; only test code passes `false`.
pub(crate) fn client(timeout: Duration, https_only: bool) -> Option<reqwest::Client> {
    builder(timeout, https_only).build().ok()
}
