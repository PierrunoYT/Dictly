//! Optional API-key storage in the macOS Keychain or, on Linux, a Secret
//! Service keyring such as GNOME Keyring or KWallet. The key never touches the
//! configuration file or the process environment. Each key is stored under
//! its provider's origin and only offered back for that origin.

use anyhow::{Result, anyhow, bail};
use keyring::{Entry, Error};

pub use crate::config::provider_origin as origin;

#[cfg(target_os = "macos")]
pub const STORE: &str = "macOS Keychain";
#[cfg(target_os = "linux")]
pub const STORE: &str = "system keyring";

fn entry(origin: &str) -> Result<Entry> {
    Entry::new("Dictly", &format!("API key/{origin}"))
        .map_err(|error| anyhow!("Cannot open the {STORE}: {error}"))
}

pub fn load(origin: &str) -> Result<Option<String>> {
    match entry(origin)?.get_password() {
        Ok(key) if key.trim().is_empty() => {
            bail!("The API key saved in the {STORE} is empty; enter it again")
        }
        Ok(key) => Ok(Some(key)),
        // Many Linux sessions run no Secret Service. A store that cannot be
        // reached holds no key for us; saving one still reports the error.
        Err(Error::NoEntry | Error::NoStorageAccess(_) | Error::PlatformFailure(_)) => Ok(None),
        Err(error) => Err(anyhow!("Cannot read the API key from the {STORE}: {error}")),
    }
}

pub fn save(origin: &str, key: &str) -> Result<()> {
    entry(origin)?.set_password(key).map_err(|error| {
        let error = error.to_string();
        anyhow!(
            "Cannot save the API key in the {STORE}: {}. It is used for this session only.",
            error.trim_end_matches('.')
        )
    })
}

pub fn delete(origin: &str) -> Result<()> {
    match entry(origin)?.delete_credential() {
        Ok(()) | Err(Error::NoEntry) => Ok(()),
        Err(error) => Err(anyhow!(
            "Cannot remove the API key from the {STORE}: {error}"
        )),
    }
}
