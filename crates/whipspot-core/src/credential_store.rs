//! Where login secrets live: the [`CredentialStore`] trait. The OS-backed implementation is in
//! `whipspot-credentials`, the in-memory fake in `whipspot-testkit`.
//!
//! The service name is defined here, in one place. Errors carry no detail on purpose: nothing
//! from the OS credential store is forwarded to a UI or a log.

use std::fmt;

use crate::secret::Secret;

/// Credential store service shared by every item the app stores.
pub const SERVICE: &str = "whipspot";

/// A store read/write/delete failed (locked, denied, unavailable ...). No detail is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoreError;

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("credential store access failed")
    }
}

impl std::error::Error for StoreError {}

pub trait CredentialStore: Send + Sync {
    fn get(&self, account: &str) -> Result<Option<Secret>, StoreError>;
    /// Refuses an empty value.
    fn set(&self, account: &str, value: &Secret) -> Result<(), StoreError>;
    /// Returns true if an item existed and was deleted.
    fn delete(&self, account: &str) -> Result<bool, StoreError>;
}
