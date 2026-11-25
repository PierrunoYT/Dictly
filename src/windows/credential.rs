//! Optional API-key storage in Windows Credential Manager (per user, this PC).
//! The key never touches the configuration file or the process environment.

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

const TARGET: &str = "Dictly/API key";

pub fn load() -> Result<Option<String>> {
    load_from(TARGET)
}

pub fn save(key: &str) -> Result<()> {
    save_to(TARGET, key)
}

pub fn delete() -> Result<()> {
    delete_from(TARGET)
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
