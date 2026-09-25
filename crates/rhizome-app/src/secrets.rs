//! Remembering a password, if the person asks.
//!
//! By default nothing secret is stored: the password is asked for when
//! connecting and lives only in memory. "Remember my password" is opt-in, and
//! when it is chosen the password goes to the operating system's credential
//! store (Windows Credential Manager, the macOS Keychain, the Secret Service on
//! Linux), never to a file of ours. A program that can read our profile file
//! cannot read the password from there without the person's login session.
//!
//! The store is a trait so the rest of the application, and its tests, do not
//! depend on a real credential store being present.

use std::collections::HashMap;
use std::sync::Mutex;

/// Somewhere a secret can be kept between runs.
pub trait SecretStore: Send + Sync {
    /// The secret stored under `key`, or `None` if there is none.
    fn get(&self, key: &str) -> Result<Option<String>, String>;
    fn set(&self, key: &str, value: &str) -> Result<(), String>;
    /// Removes a secret. Removing one that is not there is not an error.
    fn delete(&self, key: &str) -> Result<(), String>;
}

/// Keeps secrets in memory only. Used by tests, and as the fallback where the
/// platform has no credential store.
#[derive(Default)]
pub struct MemorySecrets {
    map: Mutex<HashMap<String, String>>,
}

impl std::fmt::Debug for MemorySecrets {
    /// Says how many secrets are held and nothing about them, so that a stray
    /// `{:?}` in a log line or a panic message cannot reveal one.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let held = self.map.lock().map_or(0, |m| m.len());
        write!(f, "MemorySecrets({held} held)")
    }
}

impl SecretStore for MemorySecrets {
    fn get(&self, key: &str) -> Result<Option<String>, String> {
        Ok(self
            .map
            .lock()
            .map_err(|e| e.to_string())?
            .get(key)
            .cloned())
    }
    fn set(&self, key: &str, value: &str) -> Result<(), String> {
        self.map
            .lock()
            .map_err(|e| e.to_string())?
            .insert(key.to_owned(), value.to_owned());
        Ok(())
    }
    fn delete(&self, key: &str) -> Result<(), String> {
        self.map.lock().map_err(|e| e.to_string())?.remove(key);
        Ok(())
    }
}

/// The operating system's credential store.
#[derive(Debug)]
pub struct SystemSecrets {
    #[cfg_attr(
        not(any(windows, target_os = "macos", target_os = "linux")),
        allow(dead_code)
    )]
    service: String,
}

impl SystemSecrets {
    /// `service` names the application in the credential store; each secret
    /// is then filed under its own key beneath it.
    pub fn new(service: impl Into<String>) -> SystemSecrets {
        SystemSecrets {
            service: service.into(),
        }
    }
}

#[cfg(any(windows, target_os = "macos", target_os = "linux"))]
impl SystemSecrets {
    fn entry(&self, key: &str) -> Result<keyring::Entry, String> {
        keyring::Entry::new(&self.service, key)
            .map_err(|e| format!("the system credential store is not available: {e}"))
    }
}

#[cfg(any(windows, target_os = "macos", target_os = "linux"))]
impl SecretStore for SystemSecrets {
    fn get(&self, key: &str) -> Result<Option<String>, String> {
        match self.entry(key)?.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(format!("could not read the saved password: {e}")),
        }
    }

    fn set(&self, key: &str, value: &str) -> Result<(), String> {
        self.entry(key)?
            .set_password(value)
            .map_err(|e| format!("could not save the password: {e}"))
    }

    fn delete(&self, key: &str) -> Result<(), String> {
        match self.entry(key)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(format!("could not remove the saved password: {e}")),
        }
    }
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
impl SecretStore for SystemSecrets {
    fn get(&self, _key: &str) -> Result<Option<String>, String> {
        Ok(None)
    }
    fn set(&self, _key: &str, _value: &str) -> Result<(), String> {
        Err("this platform has no credential store Rhizome can use yet".into())
    }
    fn delete(&self, _key: &str) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exercise(store: &dyn SecretStore) {
        assert_eq!(
            store.get("libera").unwrap(),
            None,
            "nothing is saved at first"
        );
        store.set("libera", "hunter2").unwrap();
        assert_eq!(store.get("libera").unwrap().as_deref(), Some("hunter2"));

        // Saving again replaces.
        store.set("libera", "correct horse").unwrap();
        assert_eq!(
            store.get("libera").unwrap().as_deref(),
            Some("correct horse")
        );

        // Secrets are kept apart by key.
        store.set("oftc", "another").unwrap();
        assert_eq!(
            store.get("libera").unwrap().as_deref(),
            Some("correct horse")
        );

        // Non-ASCII passwords are bytes, and must come back exactly.
        store.set("tr", "şifre-ÇöğüİI-🔑").unwrap();
        assert_eq!(store.get("tr").unwrap().as_deref(), Some("şifre-ÇöğüİI-🔑"));

        store.delete("libera").unwrap();
        assert_eq!(store.get("libera").unwrap(), None);
        assert_eq!(store.get("oftc").unwrap().as_deref(), Some("another"));
        // Deleting what is not there is fine.
        store.delete("libera").unwrap();
        store.delete("never-existed").unwrap();

        store.delete("oftc").unwrap();
        store.delete("tr").unwrap();
    }

    #[test]
    fn debug_output_never_shows_a_secret() {
        let m = MemorySecrets::default();
        m.set("libera", "hunter2-very-secret").unwrap();
        let shown = format!("{m:?}");
        assert!(
            !shown.contains("hunter2") && !shown.contains("libera"),
            "{shown}"
        );
        assert!(shown.contains('1'));
    }

    #[test]
    fn the_in_memory_store_behaves_like_a_secret_store() {
        exercise(&MemorySecrets::default());
    }

    /// Talks to the real Windows Credential Manager, under a service name of its
    /// own, and cleans up after itself.
    #[cfg(windows)]
    #[test]
    fn the_windows_credential_store_round_trips_secrets() {
        let store = SystemSecrets::new(format!("Rhizome-test-{}", std::process::id()));
        exercise(&store);
    }
}
