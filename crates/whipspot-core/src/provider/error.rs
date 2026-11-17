//! Provider-neutral errors. All texts are fixed, whitelisted and safe to show in a UI.

use std::fmt;

use serde::Serialize;

/// Machine-readable library failure class; the strings are the existing wire values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LibErrorKind {
    RateLimited,
    WebNotLoggedIn,
    NotReadable,
    AccessDenied,
    InvalidArgument,
    UnexpectedShape,
    Http,
    Network,
    CredentialStore,
    AuthFailed,
}

impl LibErrorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            LibErrorKind::RateLimited => "rateLimited",
            LibErrorKind::WebNotLoggedIn => "webNotLoggedIn",
            LibErrorKind::NotReadable => "notReadable",
            LibErrorKind::AccessDenied => "accessDenied",
            LibErrorKind::InvalidArgument => "invalidArgument",
            LibErrorKind::UnexpectedShape => "unexpectedShape",
            LibErrorKind::Http => "http",
            LibErrorKind::Network => "network",
            LibErrorKind::CredentialStore => "credentialStore",
            LibErrorKind::AuthFailed => "authFailed",
        }
    }
}

/// Serialises as `{kind, message, retryAfterSecs}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryError {
    pub kind: LibErrorKind,
    pub message: String,
    pub retry_after_secs: Option<u64>,
}

impl LibraryError {
    pub fn new(kind: LibErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            retry_after_secs: None,
        }
    }
}

impl fmt::Display for LibraryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for LibraryError {}

impl Serialize for LibraryError {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut st = s.serialize_struct("LibraryError", 3)?;
        st.serialize_field("kind", self.kind.as_str())?;
        st.serialize_field("message", &self.message)?;
        st.serialize_field("retryAfterSecs", &self.retry_after_secs)?;
        st.end()
    }
}

/// A playback command failed. Carries fixed text only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaybackError(pub String);

impl fmt::Display for PlaybackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PlaybackError {}

/// A login/logout failed. Carries fixed text only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthFailure(pub String);

impl fmt::Display for AuthFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for AuthFailure {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn library_error_json_shape() {
        let e = LibraryError {
            kind: LibErrorKind::RateLimited,
            message: "slow down".into(),
            retry_after_secs: Some(30),
        };
        assert_eq!(
            serde_json::to_string(&e).unwrap(),
            r#"{"kind":"rateLimited","message":"slow down","retryAfterSecs":30}"#
        );
        let e = LibraryError::new(LibErrorKind::WebNotLoggedIn, "m");
        assert_eq!(
            serde_json::to_string(&e).unwrap(),
            r#"{"kind":"webNotLoggedIn","message":"m","retryAfterSecs":null}"#
        );
    }
}
