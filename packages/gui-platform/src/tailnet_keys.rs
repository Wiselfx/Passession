//! Encryption keys for the embedded node. No key is persisted in app preferences.
//!
//! All methods block and belong on the runtime's blocking worker, never the UI
//! thread. The runtime owns the exclusive state-directory lock across these calls
//! and node use; this module also serializes native store calls within the process.

use anyhow::{Context, Result, ensure};
use paseo_tailnet::KeyStore;
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};
use zeroize::Zeroizing;

const SERVICE: &str = "sh.paseo.sessions.tailnet.state.v1";
static STORE_LOCK: Mutex<()> = Mutex::new(());

/// OS-backed storage only: Android Keystore, Apple Keychain, Windows Credential
/// Manager, or Linux Secret Service. A locked/unavailable store is an error.
/// Constructing the handle performs no IO and is safe on the UI thread.
pub fn tailnet_key_store() -> Arc<dyn KeyStore> {
    Arc::new(NativeKeyStore)
}

#[derive(Debug)]
struct NativeKeyStore;

impl KeyStore for NativeKeyStore {
    fn load_or_create(&self, namespace: &Path) -> Result<[u8; 32]> {
        let _guard = STORE_LOCK
            .lock()
            .map_err(|_| anyhow::anyhow!("Secure key store lock is poisoned"))?;
        let identity = namespace_identity(namespace)?;
        // Keep this filename in sync with paseo-tailnet's authenticated state
        // store. Missing keys must never silently replace an existing identity.
        let has_state = match std::fs::symlink_metadata(namespace.join("tailscaled.enc")) {
            Ok(_) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => return Err(error).context("Cannot inspect encrypted tailnet state"),
        };
        backend::load_or_create(&identity, has_state)
    }

    fn delete(&self, namespace: &Path) -> Result<()> {
        let _guard = STORE_LOCK
            .lock()
            .map_err(|_| anyhow::anyhow!("Secure key store lock is poisoned"))?;
        backend::delete(&namespace_identity(namespace)?)
    }
}

fn namespace_identity(namespace: &Path) -> Result<String> {
    ensure!(
        namespace.is_absolute(),
        "Tailnet key namespace must be an absolute state directory"
    );
    let canonical = namespace
        .canonicalize()
        .context("Cannot resolve tailnet key namespace")?;
    ensure!(
        canonical.is_dir(),
        "Tailnet key namespace is not a directory"
    );
    // Do not put private filesystem paths into OS credential metadata. Use the
    // native path bytes, not lossy UTF-8, so distinct paths cannot share a key.
    let mut digest = Sha256::new();
    digest.update(SERVICE.as_bytes());
    digest.update([0]);
    digest.update(canonical.as_os_str().as_encoded_bytes());
    Ok(format!("{:x}", digest.finalize()))
}

fn decode_key(bytes: Vec<u8>) -> Result<[u8; 32]> {
    let bytes = Zeroizing::new(bytes);
    ensure!(
        bytes.len() == 32,
        "Stored tailnet encryption key has an invalid length; it was not replaced"
    );
    let mut key = [0; 32];
    key.copy_from_slice(&bytes);
    Ok(key)
}

#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "windows"
))]
mod backend {
    use super::*;
    use keyring_core::{Entry, Error};

    // Construct the concrete platform store, never keyring-core's process-global
    // default store (which can be replaced with an in-memory/mock store). Each
    // store addresses the same OS item keyring 3.6's
    // `new_with_target(None, SERVICE, identity)` did, so existing keys stay readable.
    fn credential(identity: &str) -> Result<Entry> {
        // Secret Service items whose service/username/target attributes are
        // SERVICE/identity/"default", created in the default collection; the
        // store always opens a DH-encrypted session.
        #[cfg(target_os = "linux")]
        let entry = {
            use keyring_core::api::CredentialStoreApi;
            let modifiers = std::collections::HashMap::from([("target", "default")]);
            zbus_secret_service_keyring_store::Store::new()
                .and_then(|store| store.build(SERVICE, identity, Some(&modifiers)))
        };
        // Generic password service/account in the user's legacy login keychain.
        #[cfg(target_os = "macos")]
        let entry = {
            use apple_native_keyring_store::keychain::{Cred, MacKeychainDomain};
            Cred::build(MacKeychainDomain::User, SERVICE, identity)
        };
        // Generic password service/account in the app's default access group,
        // device-local and accessible when unlocked (the iOS item defaults).
        #[cfg(target_os = "ios")]
        let entry = {
            use apple_native_keyring_store::protected::{AccessPolicy, Cred};
            Cred::build(SERVICE, identity, AccessPolicy::WhenUnlocked, None, false)
        };
        // Generic credential `{identity}.{SERVICE}` with enterprise persistence.
        #[cfg(target_os = "windows")]
        let entry = {
            use keyring_core::api::CredentialStoreApi;
            windows_native_keyring_store::Store::new()
                .and_then(|store| store.build(SERVICE, identity, None))
        };
        entry.map_err(storage_error)
    }

    pub(super) fn load_or_create(identity: &str, has_state: bool) -> Result<[u8; 32]> {
        load_credential(&credential(identity)?, has_state)
    }

    fn load_credential(entry: &Entry, has_state: bool) -> Result<[u8; 32]> {
        match entry.get_secret() {
            Ok(bytes) => decode_key(bytes),
            Err(Error::NoEntry) => {
                ensure!(
                    !has_state,
                    "The secure key for existing tailnet state is missing; restore the original OS key store or explicitly reset the node"
                );
                let mut key = Zeroizing::new([0; 32]);
                getrandom::fill(&mut key[..]).map_err(|_| {
                    anyhow::anyhow!("OS randomness is unavailable for the tailnet encryption key")
                })?;
                entry.set_secret(&key[..]).map_err(storage_error)?;
                // Verify durability before allowing the runtime to write state.
                let saved = Zeroizing::new(entry.get_secret().map_err(storage_error)?);
                ensure!(
                    saved.as_slice() == &key[..],
                    "The OS key store did not retain the tailnet encryption key"
                );
                Ok(*key)
            }
            // Only NoEntry permits creation. Denied, locked, unavailable,
            // ambiguous and malformed credentials are never overwritten.
            Err(error) => Err(storage_error(error)),
        }
    }

    pub(super) fn delete(identity: &str) -> Result<()> {
        delete_credential(&credential(identity)?)
    }

    fn delete_credential(entry: &Entry) -> Result<()> {
        match entry.delete_credential() {
            Ok(()) => {}
            Err(Error::NoEntry) => return Ok(()),
            Err(error) => return Err(storage_error(error)),
        }
        // Read back so logout cleanup never reports success while the key
        // survives a delete call the platform acknowledged.
        match entry.get_secret() {
            Err(Error::NoEntry) => Ok(()),
            Ok(bytes) => {
                let _bytes = Zeroizing::new(bytes);
                anyhow::bail!("The OS key store did not delete the tailnet encryption key")
            }
            Err(error) => Err(storage_error(error)),
        }
    }

    fn storage_error(error: Error) -> anyhow::Error {
        // Do not chain or Debug-format backend errors: some error variants can
        // contain raw credential bytes or arbitrary third-party metadata.
        let message = match error {
            Error::NoEntry => "Tailnet encryption key was not found in the OS key store",
            Error::NoStorageAccess(_) => {
                "OS secure storage is locked or access was denied; unlock it and allow Passession access, then retry"
            }
            Error::Ambiguous(_) => {
                "Multiple OS credentials match this tailnet state; resolve duplicate credentials before retrying"
            }
            Error::BadEncoding(_) => {
                "The OS tailnet credential is malformed; it was not replaced"
            }
            _ => {
                "OS secure storage is unavailable or access failed; unlock the Keychain/Credential Manager/Secret Service and retry (Linux requires a running Secret Service and session bus)"
            }
        };
        anyhow::anyhow!(message)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use keyring_core::api::CredentialStoreApi;
        use keyring_core::mock;

        fn mock_entry() -> Entry {
            mock::Store::new()
                .unwrap()
                .build(SERVICE, "identity", None)
                .unwrap()
        }

        fn fail_next(entry: &Entry, error: Error) {
            let cred: &mock::Cred = entry.as_any().downcast_ref().unwrap();
            cred.set_error(error);
        }

        #[test]
        fn missing_key_is_created_once_and_deleted_idempotently() {
            let entry = mock_entry();
            let first = load_credential(&entry, false).unwrap();
            let second = load_credential(&entry, true).unwrap();
            assert!(first == second);
            delete_credential(&entry).unwrap();
            delete_credential(&entry).unwrap();
            assert!(matches!(entry.get_secret(), Err(Error::NoEntry)));
        }

        #[test]
        fn missing_key_for_existing_state_is_not_replaced() {
            let entry = mock_entry();
            assert!(load_credential(&entry, true).is_err());
            assert!(matches!(entry.get_secret(), Err(Error::NoEntry)));
        }

        #[test]
        fn denied_or_unavailable_store_never_creates_a_key() {
            for error in [
                Error::NoStorageAccess(Box::new(std::io::Error::from(
                    std::io::ErrorKind::PermissionDenied,
                ))),
                Error::PlatformFailure(Box::new(std::io::Error::from(
                    std::io::ErrorKind::NotConnected,
                ))),
            ] {
                let entry = mock_entry();
                fail_next(&entry, error);
                assert!(load_credential(&entry, false).is_err());
                assert!(matches!(entry.get_secret(), Err(Error::NoEntry)));
            }
        }

        #[test]
        fn malformed_key_is_not_replaced() {
            let entry = mock_entry();
            entry.set_secret(&[7; 31]).unwrap();
            assert!(load_credential(&entry, false).is_err());
            assert!(entry.get_secret().unwrap() == [7; 31]);
        }

        #[test]
        fn delete_denied_preserves_the_key_and_reports_failure() {
            let entry = mock_entry();
            entry.set_secret(&[7; 32]).unwrap();
            fail_next(
                &entry,
                Error::NoStorageAccess(Box::new(std::io::Error::from(
                    std::io::ErrorKind::PermissionDenied,
                ))),
            );
            assert!(delete_credential(&entry).is_err());
            assert!(entry.get_secret().unwrap() == [7; 32]);
        }

        #[test]
        fn errors_do_not_disclose_credential_bytes_or_backend_messages() {
            let error = storage_error(Error::BadEncoding(b"private-key-material".to_vec()));
            assert!(!format!("{error:?}").contains("private-key-material"));
            let error = storage_error(Error::PlatformFailure(Box::new(std::io::Error::other(
                "private-key-material",
            ))));
            assert!(!format!("{error:?}").contains("private-key-material"));
        }
    }
}

#[cfg(target_os = "android")]
#[path = "tailnet_keys_android.rs"]
mod backend;

#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "windows",
    target_os = "android"
)))]
mod backend {
    use super::*;
    pub(super) fn load_or_create(_: &str, _: bool) -> Result<[u8; 32]> {
        anyhow::bail!("This platform has no supported secure tailnet key store")
    }
    pub(super) fn delete(_: &str) -> Result<()> {
        anyhow::bail!("This platform has no supported secure tailnet key store")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_directories_have_isolated_stable_identities() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let identity = namespace_identity(first.path()).unwrap();
        assert_eq!(identity.len(), 64);
        assert!(identity.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(
            identity,
            namespace_identity(&first.path().join(".")).unwrap()
        );
        assert_ne!(identity, namespace_identity(second.path()).unwrap());
        assert!(namespace_identity(Path::new("relative-state")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_namespaces_share_the_real_directory_identity() {
        let root = tempfile::tempdir().unwrap();
        let actual = root.path().join("actual");
        let alias = root.path().join("alias");
        std::fs::create_dir(&actual).unwrap();
        std::os::unix::fs::symlink(&actual, &alias).unwrap();
        assert_eq!(
            namespace_identity(&actual).unwrap(),
            namespace_identity(&alias).unwrap()
        );
    }
}
