//! At-rest protection for the secrets this bridge writes to disk.
//!
//! Three values are stored here, and all three are worth stealing: the CA
//! private key (which can sign for any host the CA covers), the outbound-proxy
//! password, and each model's provider credential — the last being the user's
//! relay key, which spends their quota.
//!
//! On Windows the protection is DPAPI (`CryptProtectData`), which ties the
//! ciphertext to the current user account: another user on the same machine
//! cannot read it even with the file. This matters because the alternative —
//! ACLs — is what the unix mode bits in the CA writer look like, and those are
//! simply ignored on Windows.
//!
//! On other platforms this is a no-op and the `0o600`/`0o700` permissions those
//! writers already set are the whole defence. That is a real limitation rather
//! than a claim of equivalence; it is stated here so nobody reads "protected" as
//! "protected everywhere".
//!
//! ## Compatibility
//!
//! Values carry a prefix, so unprotection is self-describing: anything without
//! it is plaintext and is returned unchanged. That is what lets an existing
//! install keep working across the upgrade — the old rows read fine and are
//! re-written in protected form the next time they are saved. No migration step
//! is needed, and none can half-fail.

use base64::{engine::general_purpose::STANDARD, Engine};

use crate::{Error, Result};

/// Marks a value produced by [`protect`].
///
/// Versioned (`:1:`) so a future scheme can be recognised rather than guessed at
/// from the bytes, and textual so the protected form is still valid UTF-8 —
/// these values are stored in JSON columns and PEM-shaped files, both of which
/// would otherwise have to become binary.
const WRAP_PREFIX: &str = "coderelay-protected:1:";

/// Wraps `plaintext` for storage.
pub fn protect(plaintext: &[u8]) -> Result<Vec<u8>> {
    let Some(sealed) = platform::seal(plaintext)? else {
        // No OS-level sealing on this platform; the file permissions applied by
        // the caller are the defence. Storing the value unwrapped (rather than,
        // say, base64) keeps it readable by the same code path that reads legacy
        // values.
        return Ok(plaintext.to_vec());
    };
    Ok(format!("{WRAP_PREFIX}{}", STANDARD.encode(sealed)).into_bytes())
}

/// Reverses [`protect`], passing legacy plaintext through unchanged.
pub fn unprotect(stored: &[u8]) -> Result<Vec<u8>> {
    // A non-UTF-8 value cannot be one of ours, so it is legacy plaintext.
    let Ok(text) = std::str::from_utf8(stored) else {
        return Ok(stored.to_vec());
    };
    let Some(encoded) = text.strip_prefix(WRAP_PREFIX) else {
        return Ok(stored.to_vec());
    };
    let sealed = STANDARD
        .decode(encoded)
        .map_err(|error| Error::Config(format!("decode protected secret: {error}")))?;
    platform::unseal(&sealed)
}

/// [`protect`] for the string-valued secrets (proxy password, provider key).
pub fn protect_string(value: &str) -> Result<String> {
    let bytes = protect(value.as_bytes())?;
    String::from_utf8(bytes)
        .map_err(|error| Error::Config(format!("encode protected secret: {error}")))
}

/// [`unprotect`] for the string-valued secrets.
pub fn unprotect_string(stored: &str) -> Result<String> {
    let bytes = unprotect(stored.as_bytes())?;
    String::from_utf8(bytes)
        .map_err(|error| Error::Config(format!("decode protected secret: {error}")))
}

/// Whether this build actually seals secrets, so callers can be honest in logs
/// and diagnostics instead of implying protection that is not there.
pub const fn is_available() -> bool {
    platform::AVAILABLE
}

#[cfg(windows)]
mod platform {
    use std::ptr;

    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN,
    };

    use crate::{Error, Result};

    pub(super) const AVAILABLE: bool = true;

    /// Seals with DPAPI, scoped to the current user.
    ///
    /// `CRYPTPROTECT_UI_FORBIDDEN` is not optional here: without it DPAPI may
    /// put up its own prompt when it cannot work silently, and this runs inside
    /// a windowless sidecar with no way to answer one — the call would hang
    /// instead of failing.
    pub(super) fn seal(plaintext: &[u8]) -> Result<Option<Vec<u8>>> {
        let mut input = blob(plaintext)?;
        let mut output = CRYPT_INTEGER_BLOB::default();
        let ok = unsafe {
            CryptProtectData(
                &input,
                ptr::null(),
                ptr::null(),
                ptr::null(),
                ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        };
        let _ = &mut input;
        if ok == 0 {
            return Err(Error::Config(format!(
                "protect secret with DPAPI: {}",
                std::io::Error::last_os_error()
            )));
        }
        Ok(Some(take_blob(output)))
    }

    /// Reverses [`seal`].
    ///
    /// Unlike sealing there is no plaintext fallback on failure: a value that
    /// claims to be protected but cannot be opened means the user profile
    /// changed (or the row was copied from another account), and quietly
    /// returning the ciphertext would surface later as an unexplained
    /// authentication failure. Failing here names the real cause.
    pub(super) fn unseal(sealed: &[u8]) -> Result<Vec<u8>> {
        let mut input = blob(sealed)?;
        let mut output = CRYPT_INTEGER_BLOB::default();
        let ok = unsafe {
            CryptUnprotectData(
                &input,
                ptr::null_mut(),
                ptr::null(),
                ptr::null(),
                ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        };
        let _ = &mut input;
        if ok == 0 {
            return Err(Error::Config(format!(
                "unprotect stored secret with DPAPI (was it written by another Windows user?): {}",
                std::io::Error::last_os_error()
            )));
        }
        Ok(take_blob(output))
    }

    fn blob(data: &[u8]) -> Result<CRYPT_INTEGER_BLOB> {
        Ok(CRYPT_INTEGER_BLOB {
            cbData: u32::try_from(data.len())
                .map_err(|_| Error::Config("secret is too large to protect".into()))?,
            // The API takes `*const` for the input, but the field is declared
            // mutable; it is never written through.
            pbData: data.as_ptr() as *mut u8,
        })
    }

    /// Copies a DPAPI-allocated blob into an owned `Vec` and frees the original.
    ///
    /// The copy is unavoidable: DPAPI hands back a `LocalAlloc` block this crate
    /// does not own, and leaking it on every read would accumulate for the life
    /// of the process.
    fn take_blob(output: CRYPT_INTEGER_BLOB) -> Vec<u8> {
        let bytes =
            unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec();
        if !output.pbData.is_null() {
            unsafe { LocalFree(output.pbData.cast()) };
        }
        bytes
    }
}

#[cfg(not(windows))]
mod platform {
    use crate::Result;

    /// No OS-level sealing here; permissions are the defence.
    pub(super) const AVAILABLE: bool = false;

    pub(super) fn seal(_plaintext: &[u8]) -> Result<Option<Vec<u8>>> {
        Ok(None)
    }

    pub(super) fn unseal(_sealed: &[u8]) -> Result<Vec<u8>> {
        Err(crate::Error::Config(
            "this secret was protected on Windows and cannot be opened on this platform".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plaintext_without_the_prefix_is_returned_unchanged() {
        // Legacy rows and values written on a platform without sealing must keep
        // working, or the upgrade would corrupt every existing credential.
        assert_eq!(unprotect(b"plain-api-key").unwrap(), b"plain-api-key");
        assert_eq!(unprotect_string("plain-password").unwrap(), "plain-password");
        // Non-UTF-8 cannot be one of ours either, so it is passed through rather
        // than turned into a decode error.
        assert_eq!(unprotect(&[0xff, 0xfe]).unwrap(), vec![0xff, 0xfe]);
    }

    #[test]
    fn a_round_trip_preserves_the_value() {
        let secret = "sk-verify-round-trip-2f81c4";
        let stored = protect_string(secret).unwrap();
        assert_eq!(unprotect_string(&stored).unwrap(), secret);
    }

    #[test]
    fn the_stored_form_does_not_contain_the_plaintext() {
        // The point of the exercise: a database dump must not leak the key. On a
        // platform without sealing this assertion is vacuous by construction,
        // which is why `is_available` exists.
        let secret = "sk-verify-canary-plaintext";
        let stored = protect_string(secret).unwrap();
        if is_available() {
            assert!(!stored.contains(secret), "protected form echoed the secret");
            assert!(stored.starts_with(WRAP_PREFIX));
        } else {
            assert_eq!(stored, secret);
        }
    }

    #[test]
    fn an_empty_value_round_trips() {
        // "no password" is a legitimate state and must not become an error.
        assert_eq!(unprotect_string(&protect_string("").unwrap()).unwrap(), "");
    }
}
