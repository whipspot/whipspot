//! In-memory [`CredentialStore`] for tests; it never touches an OS credential store.

use std::{collections::HashMap, sync::Mutex};

use whipspot_core::{
    credential_store::{CredentialStore, StoreError},
    secret::Secret,
};

/// `fail` (see [`MemoryStore::set_failing`]) makes every call return [`StoreError`].
#[derive(Default)]
pub struct MemoryStore {
    items: Mutex<HashMap<String, String>>,
    fail: Mutex<bool>,
}

impl MemoryStore {
    pub fn set_failing(&self, fail: bool) {
        *self.fail.lock().unwrap() = fail;
    }

    fn check(&self) -> Result<(), StoreError> {
        if *self.fail.lock().unwrap() {
            Err(StoreError)
        } else {
            Ok(())
        }
    }

    /// Test helper: the stored value as a plain string, bypassing the failure switch.
    pub fn peek(&self, account: &str) -> Option<String> {
        self.items.lock().unwrap().get(account).cloned()
    }
}

impl CredentialStore for MemoryStore {
    fn get(&self, account: &str) -> Result<Option<Secret>, StoreError> {
        self.check()?;
        Ok(self.peek(account).map(Secret::new))
    }

    fn set(&self, account: &str, value: &Secret) -> Result<(), StoreError> {
        self.check()?;
        if value.is_empty() {
            return Err(StoreError);
        }
        self.items
            .lock()
            .unwrap()
            .insert(account.to_owned(), value.expose().to_owned());
        Ok(())
    }

    fn delete(&self, account: &str) -> Result<bool, StoreError> {
        self.check()?;
        Ok(self.items.lock().unwrap().remove(account).is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_store_roundtrip_and_refuses_empty() {
        let s = MemoryStore::default();
        assert_eq!(s.get("a").unwrap().map(|x| x.expose().to_owned()), None);
        s.set("a", &Secret::new("v")).unwrap();
        assert_eq!(s.get("a").unwrap().unwrap().expose(), "v");
        assert!(s.set("a", &Secret::new("")).is_err());
        assert_eq!(s.get("a").unwrap().unwrap().expose(), "v");
        assert!(s.delete("a").unwrap());
        assert!(!s.delete("a").unwrap());
        s.set_failing(true);
        assert_eq!(s.get("a").unwrap_err(), StoreError);
    }
}
