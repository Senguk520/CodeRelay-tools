//! Implements Windows-specific certificate authority integration.
//! Native Windows system root-store access without external command-line tools.

use std::{ffi::c_void, io, ptr, slice};

use windows_sys::Win32::Security::Cryptography::{
    CertCloseStore, CertEnumCertificatesInStore, CertOpenStore, CERT_STORE_OPEN_EXISTING_FLAG,
    CERT_STORE_PROV_SYSTEM_W, CERT_STORE_READONLY_FLAG, CERT_SYSTEM_STORE_CURRENT_USER,
    CERT_SYSTEM_STORE_LOCAL_MACHINE,
};

use crate::{Error, Result};

const ROOT_STORE: [u16; 5] = [b'R' as u16, b'O' as u16, b'O' as u16, b'T' as u16, 0];

/// Whether the certificate is trusted in **any** store the bridge can install to.
///
/// Both scopes are checked because the install command targets CurrentUser while
/// an older build targeted LocalMachine, and a root already trusted
/// machine-wide is still trust: reporting `Untrusted` would ask the user to
/// install something they already have, and re-adding it would leave two copies
/// behind. Checking CurrentUser is also what makes the new `-user` install
/// detectable at all — without it the state would read `Untrusted` forever after
/// a successful install, and `enable()` (which requires `Ready`) could never
/// proceed.
pub(super) fn is_installed(cert: &str) -> Result<bool> {
    let der = certificate_der(cert)?;
    for scope in [
        CERT_SYSTEM_STORE_CURRENT_USER,
        CERT_SYSTEM_STORE_LOCAL_MACHINE,
    ] {
        if store_contains(scope, &der)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Scans one system root store for an exact DER match.
///
/// DER equality rather than a name or fingerprint comparison: anything that can
/// write to the store can duplicate a name, so matching on one would let an
/// unrelated certificate make this report `Ready` and let the takeover proceed
/// under a CA that cannot actually sign for the traffic.
fn store_contains(scope: u32, der: &[u8]) -> Result<bool> {
    let store = open_root_store(scope)?;
    let mut context = ptr::null();
    let mut found = false;
    loop {
        context = unsafe { CertEnumCertificatesInStore(store, context) };
        if context.is_null() {
            break;
        }
        let encoded = unsafe {
            slice::from_raw_parts((*context).pbCertEncoded, (*context).cbCertEncoded as usize)
        };
        if encoded == der {
            found = true;
            break;
        }
    }
    if !context.is_null() {
        unsafe { windows_sys::Win32::Security::Cryptography::CertFreeCertificateContext(context) };
    }
    close_store(store)?;
    Ok(found)
}

fn certificate_der(cert: &str) -> Result<Vec<u8>> {
    pem::parse(cert)
        .map(|pem| pem.into_contents())
        .map_err(|error| Error::Config(format!("parse CA PEM: {error}")))
}

fn open_root_store(scope: u32) -> Result<*mut c_void> {
    let flags = scope | CERT_STORE_OPEN_EXISTING_FLAG | CERT_STORE_READONLY_FLAG;
    let store = unsafe {
        CertOpenStore(
            CERT_STORE_PROV_SYSTEM_W,
            0,
            0,
            flags,
            ROOT_STORE.as_ptr().cast(),
        )
    };
    if store.is_null() {
        return Err(Error::Config(format!(
            "open Windows Root store (flags {flags:#x}): {}",
            io::Error::last_os_error()
        )));
    }
    Ok(store)
}

fn close_store(store: *mut c_void) -> Result<()> {
    if unsafe { CertCloseStore(store, 0) } == 0 {
        return Err(Error::Config(format!(
            "close Windows certificate store: {}",
            io::Error::last_os_error()
        )));
    }
    Ok(())
}
