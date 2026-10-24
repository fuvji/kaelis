//! Small wrappers for optional integration secrets in the OS credential store.
//! Values are never serialized, logged, or written into instance metadata.

use kaelis_core::brand::KEYRING_SERVICE;

pub const CURSEFORGE_KEY_ACCOUNT: &str = "curseforge-api-key";

fn entry(account: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(KEYRING_SERVICE, account).map_err(|error| error.to_string())
}

pub fn load_secret(account: &str) -> Result<Option<String>, String> {
    match entry(account)?.get_password() {
        Ok(value) => Ok(Some(value)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

pub fn store_secret(account: &str, value: &str) -> Result<(), String> {
    entry(account)?
        .set_password(value)
        .map_err(|error| error.to_string())
}

/// Delete a CurseForge key an older build stored in Settings.
///
/// The key field is gone and content resolves without one, so the entry is
/// dead weight; removing it keeps the promise that the launcher stores no
/// third-party credential. A missing entry is not an error.
pub fn forget_legacy_curseforge_key() {
    if matches!(load_secret(CURSEFORGE_KEY_ACCOUNT), Ok(Some(_))) {
        let _ = delete_secret(CURSEFORGE_KEY_ACCOUNT);
    }
}

pub fn delete_secret(account: &str) -> Result<(), String> {
    match entry(account)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::CURSEFORGE_KEY_ACCOUNT;

    #[test]
    fn secret_account_name_is_stable() {
        assert_eq!(CURSEFORGE_KEY_ACCOUNT, "curseforge-api-key");
    }
}
