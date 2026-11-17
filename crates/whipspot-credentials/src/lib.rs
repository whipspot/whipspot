//! The OS credential store behind [`whipspot_core::credential_store::CredentialStore`]: a
//! `keyring` entry per account under the service [`SERVICE`] (the macOS Keychain, the Windows
//! Credential Manager or the Secret Service, whichever the platform has).
//!
//! No keyring error is ever formatted, logged or kept: `keyring::Error::BadEncoding` and
//! `BadDataFormat` carry the raw secret bytes.

use whipspot_core::{
    credential_store::{CredentialStore, SERVICE, StoreError},
    secret::Secret,
};

/// The platform credential store, service [`SERVICE`].
#[derive(Debug, Default, Clone, Copy)]
pub struct OsCredentialStore;

impl OsCredentialStore {
    fn entry(account: &str) -> Result<keyring::Entry, StoreError> {
        keyring::Entry::new(SERVICE, account).map_err(|_| StoreError)
    }
}

impl CredentialStore for OsCredentialStore {
    fn get(&self, account: &str) -> Result<Option<Secret>, StoreError> {
        match Self::entry(account)?.get_password() {
            Ok(p) => Ok(Some(Secret::new(p))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(_) => Err(StoreError),
        }
    }

    fn set(&self, account: &str, value: &Secret) -> Result<(), StoreError> {
        if value.is_empty() {
            return Err(StoreError);
        }
        Self::entry(account)?
            .set_password(value.expose())
            .map_err(|_| StoreError)
    }

    fn delete(&self, account: &str) -> Result<bool, StoreError> {
        match Self::entry(account)?.delete_credential() {
            Ok(()) => Ok(true),
            Err(keyring::Error::NoEntry) => Ok(false),
            Err(_) => Err(StoreError),
        }
    }
}
