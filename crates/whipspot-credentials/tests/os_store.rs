//! H22: the credential-store contract on the real OS store. User-run only: it writes to the
//! Windows Credential Manager or the macOS Keychain (hard rules 2 and 3), so it is ignored in
//! every automatic run.
//!
//! `cargo test -p whipspot-credentials --test os_store -- --ignored`
//!
//! It touches only the items `contract-account-a` and `contract-account-b` under the service
//! `whipspot` (and the empty account name, if the platform accepts it, which the suite
//! deletes again), removes any left from an earlier aborted run before it starts and again when
//! it ends, and prints no value.

use whipspot_core::credential_store::CredentialStore;
use whipspot_credentials::OsCredentialStore;
use whipspot_testkit::run_credential_store_contract;

const ACCOUNTS: [&str; 3] = ["contract-account-a", "contract-account-b", ""];

/// Deletes the contract accounts when created and when dropped (also on a failing assertion).
struct Clean(OsCredentialStore);

impl Clean {
    fn new() -> Self {
        let clean = Clean(OsCredentialStore);
        clean.sweep();
        clean
    }

    fn sweep(&self) {
        for account in ACCOUNTS {
            // A store that refuses even this fails the suite itself with a clearer message.
            let _ = self.0.delete(account);
        }
    }
}

impl Drop for Clean {
    fn drop(&mut self) {
        self.sweep();
    }
}

#[test]
#[ignore = "user-run: touches the OS credential store"]
fn credential_store_contract_on_the_os_store() {
    let guard = Clean::new();
    run_credential_store_contract(&guard.0);
}
