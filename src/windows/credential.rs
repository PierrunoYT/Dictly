//! Optional API-key storage in Windows Credential Manager (per user, this PC).
//! The key never touches the configuration file or the process environment.
//! Each key is stored under its provider's origin and only offered back for
//! that origin, so a configuration pointing elsewhere never receives it.

use super::wide;
use anyhow::{Result, bail, ensure};
use std::ptr::null_mut;
use windows_sys::Win32::{
    Foundation::{ERROR_NOT_FOUND, GetLastError},
    Security::Credentials::{
        CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredFree,
        CredReadW, CredWriteW,
    },
};

/// Unbound entry written by earlier builds; removed when a key is saved or
/// removed so it cannot linger without a provider.
const LEGACY_TARGET: &str = "Dictly/API key";

pub use crate::config::provider_origin as origin;

pub const STORE: &str = "Windows Credential Manager";

/// Credential Manager target name for the key of one provider origin.
fn target(origin: &str) -> String {
    format!("Dictly/API key/{origin}")
}

pub fn load(origin: &str) -> Result<Option<String>> {
    load_from(&target(origin))
}

pub fn save(origin: &str, key: &str) -> Result<()> {
    save_to(&target(origin), key)?;
    delete_from(LEGACY_TARGET)
}

pub fn delete(origin: &str) -> Result<()> {
    delete_from(&target(origin))?;
    delete_from(LEGACY_TARGET)
}

fn load_from(target: &str) -> Result<Option<String>> {
    let target = wide(target);
    let mut credential: *mut CREDENTIALW = null_mut();
    // SAFETY: target is NUL-terminated; the returned buffer is freed with CredFree.
    unsafe {
        if CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut credential) == 0 {
            if GetLastError() == ERROR_NOT_FOUND {
                return Ok(None);
            }
            bail!("Cannot read the saved API key from Windows Credential Manager");
        }
        let entry = &*credential;
        // An empty secret has a null blob, which from_raw_parts must never receive.
        let blob = if entry.CredentialBlob.is_null() || entry.CredentialBlobSize == 0 {
            Vec::new()
        } else {
            std::slice::from_raw_parts(entry.CredentialBlob, entry.CredentialBlobSize as usize)
                .to_vec()
        };
        CredFree(credential.cast());
        match String::from_utf8(blob) {
            Ok(key) if !key.trim().is_empty() => Ok(Some(key)),
            _ => bail!("The saved API key is unreadable; enter it again"),
        }
    }
}

fn save_to(target: &str, key: &str) -> Result<()> {
    ensure!(!key.trim().is_empty(), "API key must not be empty");
    let mut target = wide(target);
    let mut user = wide("Dictly");
    // SAFETY: every pointer in the credential outlives the CredWriteW call.
    let saved = unsafe {
        let mut credential: CREDENTIALW = std::mem::zeroed();
        credential.Type = CRED_TYPE_GENERIC;
        credential.TargetName = target.as_mut_ptr();
        credential.UserName = user.as_mut_ptr();
        credential.CredentialBlobSize = key.len() as u32;
        credential.CredentialBlob = key.as_ptr().cast_mut();
        credential.Persist = CRED_PERSIST_LOCAL_MACHINE;
        CredWriteW(&credential, 0)
    };
    ensure!(
        saved != 0,
        "Cannot save the API key to Windows Credential Manager"
    );
    Ok(())
}

fn delete_from(target: &str) -> Result<()> {
    let target = wide(target);
    // SAFETY: target is NUL-terminated.
    unsafe {
        if CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) == 0
            && GetLastError() != ERROR_NOT_FOUND
        {
            bail!("Cannot remove the saved API key from Windows Credential Manager");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_bound_to_the_provider_origin() {
        let openai = origin("https://api.openai.com/v1").unwrap();
        assert_eq!(openai, "https://api.openai.com");
        assert_eq!(target(&openai), "Dictly/API key/https://api.openai.com");
        for same in [
            "https://API.OpenAI.com/v1/",
            "https://api.openai.com:443/other",
        ] {
            assert_eq!(origin(same).as_deref(), Some("https://api.openai.com"));
        }
        for other in [
            "https://attacker.example/v1",
            "http://api.openai.com/v1",
            "https://api.openai.com:8443/v1",
            "https://api.openai.com.attacker.example/v1",
        ] {
            assert_ne!(origin(other).as_deref(), Some("https://api.openai.com"));
        }
        assert_eq!(
            origin("http://127.0.0.1:8080/v1").as_deref(),
            Some("http://127.0.0.1:8080")
        );
        assert_eq!(origin("not a url"), None);
        assert_eq!(origin("file:///C:/key"), None);
    }

    #[test]
    fn credentials_round_trip_and_delete() {
        // A throwaway name so a real saved Dictly key is never touched.
        let target = format!("Dictly/test credential {}", std::process::id());
        assert!(load_from(&target).unwrap().is_none());
        save_to(&target, "sk-test-Grüße").unwrap();
        assert_eq!(
            load_from(&target).unwrap().as_deref(),
            Some("sk-test-Grüße")
        );
        save_to(&target, "replaced").unwrap();
        assert_eq!(load_from(&target).unwrap().as_deref(), Some("replaced"));
        delete_from(&target).unwrap();
        assert!(load_from(&target).unwrap().is_none());
        delete_from(&target).unwrap();
        assert!(save_to(&target, " ").is_err());
    }

    #[test]
    fn an_empty_saved_credential_is_reported_as_unreadable() {
        let target = format!("Dictly/test empty credential {}", std::process::id());
        let mut name = wide(&target);
        let mut user = wide("Dictly");
        // SAFETY: every pointer outlives the call; a null blob of size 0 is how
        // Credential Manager stores an emptied password.
        let saved = unsafe {
            let mut credential: CREDENTIALW = std::mem::zeroed();
            credential.Type = CRED_TYPE_GENERIC;
            credential.TargetName = name.as_mut_ptr();
            credential.UserName = user.as_mut_ptr();
            credential.Persist = CRED_PERSIST_LOCAL_MACHINE;
            CredWriteW(&credential, 0)
        };
        assert_ne!(saved, 0);
        let loaded = load_from(&target);
        delete_from(&target).unwrap();
        assert!(loaded.is_err());
    }
}
