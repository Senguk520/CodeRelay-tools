//! Installs and manages the local certificate authority.
use std::{fs, path::PathBuf};

#[cfg(target_os = "macos")]
use std::process::Command;

#[cfg(target_os = "windows")]
mod windows;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, GeneralSubtree, IsCa, Issuer,
    KeyPair, KeyUsagePurpose, NameConstraints, RsaKeySize, PKCS_RSA_SHA256,
};
#[cfg(target_os = "macos")]
use sha1::{Digest, Sha1};
use time::{Duration, OffsetDateTime};
use x509_parser::prelude::FromDer;

use crate::{config::managed_ca_dir, secret, Error, Result};

use super::CaState;

/// The DNS subtree the CA is allowed to sign for.
///
/// The proxy already restricts interception to `*.cursor.sh` at runtime
/// (`local_app::proxy::is_cursor_host`), but that is a *code* check: it stops
/// being a limit the moment someone adds an upstream host or the matching logic
/// regresses, and it does nothing at all if the private key leaks. Encoding the
/// same boundary in the certificate turns "this CA happens to only be used for
/// Cursor" into "this CA *cannot* vouch for anything else", so a leaked key buys
/// an attacker no trusted certificate for any other domain.
///
/// Both spellings are listed: `cursor.sh` covers the bare apex, and
/// `.cursor.sh` the subdomains (`api2.cursor.sh`, `api3.cursor.sh`). TLS stacks
/// treat a leading dot as "subdomains only", so the apex needs its own entry.
fn permitted_subtrees() -> Vec<GeneralSubtree> {
    vec![
        GeneralSubtree::DnsName("cursor.sh".into()),
        GeneralSubtree::DnsName(".cursor.sh".into()),
    ]
}

/// Subject common name of the generated CA.
///
/// Shared with the uninstall command, which removes the root *by name*: a
/// literal repeated in two places is exactly how "uninstall" quietly stops
/// matching the certificate it is supposed to remove.
const CA_COMMON_NAME: &str = "CodeRelay Cursor Bridge CA";

#[derive(Clone)]
pub struct CaManager {
    dir: PathBuf,
}

pub struct LoadedCa {
    pub issuer: Issuer<'static, KeyPair>,
}

impl CaManager {
    pub fn managed() -> Result<Self> {
        let dir = managed_ca_dir()?;
        migrate_legacy_location(&dir)?;
        Ok(Self { dir })
    }

    fn cert_path(&self) -> PathBuf {
        self.dir.join("ca.crt")
    }
    fn key_path(&self) -> PathBuf {
        self.dir.join("ca.key")
    }

    pub fn state(&self) -> Result<CaState> {
        let cert = fs::read_to_string(self.cert_path());
        let key = self.read_key();
        match (cert, key) {
            (Err(cert_error), Err(key_error))
                if cert_error.kind() == std::io::ErrorKind::NotFound
                    && key_error.kind() == std::io::ErrorKind::NotFound =>
            {
                Ok(CaState::Missing)
            }
            (Ok(cert), Ok(key)) => {
                if parse_issuer(&cert, &key).is_err() {
                    return Ok(CaState::Invalid);
                }
                Ok(if is_installed(&cert)? {
                    CaState::Ready
                } else {
                    CaState::Untrusted
                })
            }
            _ => Ok(CaState::Invalid),
        }
    }

    /// Reads the private key, unsealing it if it was stored protected.
    ///
    /// A failure to unseal becomes `NotFound` rather than being propagated, so
    /// `state()` reports `Invalid` — the same outcome as any other unreadable
    /// key. That keeps the recovery path ("regenerate the CA") reachable instead
    /// of wedging the settings page on an error the user cannot act on.
    fn read_key(&self) -> std::io::Result<String> {
        let stored = fs::read(self.key_path())?;
        secret::unprotect(&stored)
            .and_then(|bytes| {
                String::from_utf8(bytes)
                    .map_err(|error| Error::Config(format!("decode CA key: {error}")))
            })
            .map_err(|error| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
            })
    }

    pub fn load(&self) -> Result<LoadedCa> {
        let cert = fs::read_to_string(self.cert_path())?;
        let key = self.read_key()?;
        Ok(LoadedCa {
            issuer: parse_issuer(&cert, &key)?,
        })
    }

    pub fn install_command(&self) -> Option<String> {
        let path = self.cert_path().to_string_lossy().replace('\'', "'\\''");
        match std::env::consts::OS {
            "macos" => dirs::home_dir().map(|_| {
                format!(
                    "sudo security add-trusted-cert -d -r trustRoot -p ssl -k /Library/Keychains/System.keychain '{}'",
                    path
                )
            }),
            // `-user` targets the CurrentUser store rather than LocalMachine.
            // That is what makes this runnable **without administrator
            // privileges** — CodeRelay deliberately never elevates — and it
            // narrows the trust to the account actually running the bridge,
            // which is the same account that can read the DPAPI-protected key.
            // A machine-wide root would let a key only this user holds MITM
            // every other account on the box.
            "windows" => Some(format!(
                "certutil -user -addstore -f Root \"{}\"",
                self.cert_path().display()
            )),
            "linux" => {
                let anchor = linux_anchor_file();
                Some(format!(
                    "sudo cp '{}' '{}' && sudo {}",
                    path,
                    anchor.display(),
                    linux_refresh_command()
                ))
            }
            _ => None,
        }
    }

    /// The command that withdraws the trusted root again.
    ///
    /// Shown beside [`Self::install_command`] so the trust is not a one-way
    /// door: without it the root stays trusted on the user's machine after they
    /// stop using the feature, and there is no documented way to remove it.
    /// Each arm mirrors the scope its install counterpart uses — CurrentUser on
    /// Windows, the machine keychain on macOS, the same anchor file on Linux —
    /// so "uninstall" undoes exactly what "install" did.
    pub fn uninstall_command(&self) -> Option<String> {
        match std::env::consts::OS {
            "macos" => dirs::home_dir().map(|_| format!("sudo security delete-certificate -c \"{CA_COMMON_NAME}\" /Library/Keychains/System.keychain")),
            // `-user -delstore` pairs with the `-user -addstore` above; a
            // machine-store delete would target a store the install no longer
            // writes to, and would silently report success while changing
            // nothing.
            "windows" => Some(format!("certutil -user -delstore Root \"{CA_COMMON_NAME}\"")),
            "linux" => {
                let anchor = linux_anchor_file();
                Some(format!(
                    "sudo rm -f '{}' && sudo {}",
                    anchor.display(),
                    linux_refresh_command()
                ))
            }
            _ => None,
        }
    }

    pub fn initialize_local(&self) -> Result<()> {
        match self.state()? {
            CaState::Invalid => {
                return Err(Error::Config("CA files are incomplete or invalid".into()))
            }
            CaState::Ready => return Ok(()),
            CaState::Missing => self.generate()?,
            CaState::Untrusted => {}
        }
        Ok(())
    }

    fn generate(&self) -> Result<()> {
        fs::create_dir_all(&self.dir)?;
        #[cfg(unix)]
        fs::set_permissions(&self.dir, fs::Permissions::from_mode(0o700))?;

        let key = KeyPair::generate_rsa_for(&PKCS_RSA_SHA256, RsaKeySize::_3072)
            .map_err(|error| Error::Config(format!("generate CA key: {error}")))?;
        let mut params = CertificateParams::new(Vec::<String>::new())
            .map_err(|error| Error::Config(format!("create CA parameters: {error}")))?;
        let mut name = DistinguishedName::new();
        name.push(DnType::CommonName, CA_COMMON_NAME);
        name.push(DnType::OrganizationName, "CodeRelay");
        params.distinguished_name = name;
        params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
        params.key_usages = vec![
            KeyUsagePurpose::DigitalSignature,
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::CrlSign,
        ];
        // Bound the CA to the one domain tree it exists to intercept. See
        // [`permitted_subtrees`] for why this lives in the certificate and not
        // only in the proxy's host check.
        params.name_constraints = Some(NameConstraints {
            permitted_subtrees: permitted_subtrees(),
            excluded_subtrees: Vec::new(),
        });
        params.not_before = OffsetDateTime::now_utc() - Duration::minutes(5);
        params.not_after = OffsetDateTime::now_utc() + Duration::days(3652);
        let cert = params
            .self_signed(&key)
            .map_err(|error| Error::Config(format!("generate CA certificate: {error}")))?;
        // The private key is wrapped with DPAPI on Windows, where the unix mode
        // bits below are ignored and an unwrapped PEM would be readable by any
        // process running as this user. See [`secret`].
        write_atomic(&self.key_path(), &secret::protect(key.serialize_pem().as_bytes())?, 0o600)?;
        write_atomic(&self.cert_path(), cert.pem().as_bytes(), 0o644)?;
        Ok(())
    }
}

/// Moves a CA left behind by an older build out of the data directory.
///
/// The CA used to live at `<data dir>/ca`, next to the disposable SQLite cache.
/// That is exactly the pairing that makes "zip the data directory and send it
/// over" leak the signing key, so it now has its own directory. Rather than
/// regenerating — which would silently invalidate a root the user already
/// trusted, leaving them with a broken proxy and a stale trusted certificate —
/// an existing pair is moved across. The old directory is only removed once both
/// files are safely in the new one.
fn migrate_legacy_location(new_dir: &std::path::Path) -> Result<()> {
    let legacy = crate::config::managed_data_dir()?.join("ca");
    if !legacy.is_dir() {
        return Ok(());
    }
    for name in ["ca.crt", "ca.key"] {
        let source = legacy.join(name);
        let destination = new_dir.join(name);
        // Never overwrite: a CA already in the new location is the live one, and
        // the leftover is a duplicate.
        if source.is_file() && !destination.exists() {
            fs::rename(&source, &destination)?;
        }
    }
    // Only drop the directory when it is genuinely empty of the pair; a partial
    // move leaves it in place so nothing is lost.
    let remaining = ["ca.crt", "ca.key"]
        .iter()
        .any(|name| legacy.join(name).exists());
    if !remaining {
        let _ = fs::remove_dir(&legacy);
    }
    Ok(())
}

fn parse_issuer(cert: &str, key: &str) -> Result<Issuer<'static, KeyPair>> {
    let key =
        KeyPair::from_pem(key).map_err(|error| Error::Config(format!("parse CA key: {error}")))?;
    let pem = pem::parse(cert).map_err(|error| Error::Config(format!("parse CA PEM: {error}")))?;
    let (_, parsed) = x509_parser::certificate::X509Certificate::from_der(pem.contents())
        .map_err(|error| Error::Config(format!("parse CA X.509 certificate: {error}")))?;
    if parsed.public_key().subject_public_key.data.as_ref() != key.public_key_raw() {
        return Err(Error::Config(
            "CA certificate and private key do not match".into(),
        ));
    }
    if !parsed.validity().is_valid() {
        return Err(Error::Config(
            "CA certificate is outside its validity period".into(),
        ));
    }
    if !parsed
        .basic_constraints()
        .map_err(|error| Error::Config(format!("read CA constraints: {error}")))?
        .is_some_and(|constraints| constraints.value.ca)
    {
        return Err(Error::Config("certificate is not a CA".into()));
    }
    Issuer::from_ca_cert_pem(cert, key)
        .map_err(|error| Error::Config(format!("parse CA certificate: {error}")))
}

fn write_atomic(path: &std::path::Path, data: &[u8], _mode: u32) -> Result<()> {
    let temp = path.with_extension("tmp");
    fs::write(&temp, data)?;
    #[cfg(unix)]
    fs::set_permissions(&temp, fs::Permissions::from_mode(_mode))?;
    fs::rename(&temp, path)?;
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(_mode))?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn fingerprint(cert: &str) -> Result<String> {
    let pem = pem::parse(cert).map_err(|error| Error::Config(format!("parse CA PEM: {error}")))?;
    Ok(hex::encode_upper(Sha1::digest(pem.contents())))
}

#[cfg(target_os = "macos")]
fn is_installed(cert: &str) -> Result<bool> {
    let fingerprint = fingerprint(cert)?;
    for keychain in ["login.keychain-db", "/Library/Keychains/System.keychain"] {
        let output = Command::new("security")
            .args(["find-certificate", "-a", "-Z", keychain])
            .output()?;
        if output.status.success() && String::from_utf8_lossy(&output.stdout).contains(&fingerprint)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(target_os = "windows")]
fn is_installed(cert: &str) -> Result<bool> {
    windows::is_installed(cert)
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn is_installed(cert: &str) -> Result<bool> {
    match fs::read_to_string(linux_anchor_file()) {
        Ok(installed) => Ok(installed.trim() == cert.trim()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

const LINUX_ANCHOR_NAME: &str = "coderelay-cursor-bridge-ca.crt";

fn linux_anchor_file() -> PathBuf {
    if PathBuf::from("/etc/pki/ca-trust/source/anchors").is_dir() {
        PathBuf::from("/etc/pki/ca-trust/source/anchors").join(LINUX_ANCHOR_NAME)
    } else if PathBuf::from("/etc/ca-certificates/trust-source/anchors").is_dir() {
        PathBuf::from("/etc/ca-certificates/trust-source/anchors").join(LINUX_ANCHOR_NAME)
    } else {
        PathBuf::from("/usr/local/share/ca-certificates").join(LINUX_ANCHOR_NAME)
    }
}

fn linux_refresh_command() -> &'static str {
    match linux_anchor_file().parent().and_then(|dir| dir.to_str()) {
        Some("/usr/local/share/ca-certificates") => "update-ca-certificates",
        _ => "update-ca-trust extract",
    }
}
