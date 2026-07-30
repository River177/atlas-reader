use atlas_domain::AtlasError;
use atlas_provider_settings::{Secret, SecretStore};
use keyring::{Entry, Error as KeyringError};

/// Keychain service name. Changing it orphans every stored credential, so it is
/// deliberately independent of the bundle identifier and the version number.
const SERVICE: &str = "com.atlasreader.providers";

/// Stores provider credentials in the macOS keychain, so they never reach the
/// Atlas database, backups of it, or log output.
#[derive(Clone, Debug, Default)]
pub struct MacOsKeychainAdapter;

impl MacOsKeychainAdapter {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    fn entry(account: &str) -> Result<Entry, AtlasError> {
        Entry::new(SERVICE, account).map_err(map_keyring)
    }
}

impl SecretStore for MacOsKeychainAdapter {
    fn set(&self, account: &str, secret: &Secret) -> Result<(), AtlasError> {
        Self::entry(account)?
            .set_password(secret.expose())
            .map_err(map_keyring)
    }

    fn get(&self, account: &str) -> Result<Option<Secret>, AtlasError> {
        match Self::entry(account)?.get_password() {
            Ok(password) => Ok(Some(Secret::new(password))),
            Err(KeyringError::NoEntry) => Ok(None),
            Err(error) => Err(map_keyring(error)),
        }
    }

    fn delete(&self, account: &str) -> Result<(), AtlasError> {
        match Self::entry(account)?.delete_credential() {
            Ok(()) | Err(KeyringError::NoEntry) => Ok(()),
            Err(error) => Err(map_keyring(error)),
        }
    }
}

/// Keychain failures are reported without the credential and without the
/// underlying platform error code, which can embed the account contents.
fn map_keyring(error: KeyringError) -> AtlasError {
    match error {
        KeyringError::NoStorageAccess(_) => AtlasError::storage(
            "Atlas could not reach the macOS keychain. Unlock the login keychain and try again.",
        ),
        KeyringError::Invalid(argument, reason) => {
            AtlasError::invalid_input(format!("The keychain rejected the {argument}: {reason}"))
        }
        KeyringError::Ambiguous(_) => AtlasError::storage(
            "The macOS keychain holds more than one Atlas entry for this provider. Remove the duplicates in Keychain Access and try again.",
        ),
        _ => AtlasError::storage("The macOS keychain could not store the provider credential."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atlas_domain::ProviderKind;
    use atlas_provider_settings::secret_account;

    // The keychain itself is not exercised here: touching it from an automated
    // run prompts for authorization and fails in CI.

    #[test]
    fn every_provider_maps_to_its_own_keychain_entry() {
        let mineru = secret_account(ProviderKind::Mineru);
        let translation = secret_account(ProviderKind::Translation);

        assert_ne!(mineru, translation);
        assert!(MacOsKeychainAdapter::entry(&mineru).is_ok());
        assert!(MacOsKeychainAdapter::entry(&translation).is_ok());
    }

    #[test]
    fn keychain_failures_never_leak_platform_detail() {
        let rendered = format!(
            "{}",
            map_keyring(KeyringError::NoStorageAccess(Box::new(
                std::io::Error::other("account=atlas.cloud_mineru secret=super-secret-value")
            )))
        );

        assert!(!rendered.contains("super-secret-value"));
        assert!(rendered.contains("keychain"));
    }
}
