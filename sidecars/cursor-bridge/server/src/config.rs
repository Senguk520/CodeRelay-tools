//! Loads and validates process-level server configuration.
use std::{env, fs, net::SocketAddr, path::{Path, PathBuf}, time::Duration};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use crate::{Error, Result};

const DATA_DIR_NAME: &str = ".coderelay-cursor-bridge";
const DATA_DIR_ENV: &str = "CODERELAY_CURSOR_DATA_DIR";
/// Separate directory for the certificate authority. See [`managed_ca_dir`].
const CA_DIR_NAME: &str = ".coderelay-cursor-bridge-ca";
const CA_DIR_ENV: &str = "CODERELAY_CURSOR_CA_DIR";
const DATABASE_FILE_NAME: &str = "cursor-bridge.db";
/// Shared secret CodeRelay passes so the control API can tell "the desktop app
/// asked" from "any local process asked". See [`ControlAuth`](crate::control).
const CONTROL_TOKEN_ENV: &str = "CODERELAY_CURSOR_CONTROL_TOKEN";
const DEFAULT_PROVIDER_REQUEST_TIMEOUT: Duration = Duration::from_secs(60 * 60);
const DEFAULT_PROVIDER_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// Resolves the directory holding the bridge database and the local certificate authority.
///
/// `CODERELAY_CURSOR_DATA_DIR` takes precedence when set: CodeRelay passes its own
/// application data directory so the bridge can never share a SQLite file with a
/// separately installed copy of the upstream project.
pub fn managed_data_dir() -> Result<PathBuf> {
    let data_dir = match std::env::var_os(DATA_DIR_ENV) {
        Some(value) if !value.is_empty() => {
            let path = PathBuf::from(value);
            // Validated rather than used as-is. `CODERELAY_CURSOR_DATA_DIR` is an
            // environment variable, so it is attacker-influenced in exactly the
            // scenario this program has to survive: another local process
            // spawning the bridge. A relative path resolves against whatever the
            // working directory happens to be (so the "bridge state lives under
            // one known directory" contract breaks), and a filesystem root would
            // have the bridge put its SQLite database — and on unix a `0o700`
            // directory mode — at `/` or `C:\`. Both are refused at startup.
            validate_data_dir(&path)?;
            path
        }
        _ => {
            let home_dir = dirs::home_dir()
                .ok_or_else(|| Error::Config("cannot resolve user home directory".into()))?;
            home_dir.join(DATA_DIR_NAME)
        }
    };
    fs::create_dir_all(&data_dir)?;
    #[cfg(unix)]
    fs::set_permissions(&data_dir, fs::Permissions::from_mode(0o700))?;
    Ok(data_dir)
}

/// Refuses a data directory that is relative or is a filesystem root.
///
/// The bar is deliberately about *shape* rather than an exhaustive path
/// allowlist. Refusing relative paths keeps the "all bridge state lives under one
/// known directory" contract from depending on the working directory, which is
/// not ours to choose; refusing roots keeps the SQLite database — and on unix the
/// `0o700` that is applied to this directory — off `/` and `C:\`.
///
/// This is not an allowlist of safe locations, and not a blocklist of sensitive
/// ones: a path like `C:\Windows` passes, and a caller who points the variable
/// there gets what they asked for. Enumerating sensitive locations would give a
/// false sense of completeness without changing that.
fn validate_data_dir(path: &Path) -> Result<()> {
    if !path.is_absolute() {
        return Err(Error::Config(format!(
            "CODERELAY_CURSOR_DATA_DIR must be an absolute path, got {}",
            path.display()
        )));
    }
    // `Path::parent()` is `None` for a filesystem root: `/`, `C:\`, `C:/`,
    // `D:/` and `\\server\share` all take that arm. The `file_name()` arm of the
    // match catches `C:\..`, whose parent (`C:\`) is not empty — a root spelled
    // by walking up. Relative single-segment paths such as `bridge-data` do leave
    // an empty parent, but the absolute check above has already refused them.
    let is_root = match (path.parent(), path.file_name()) {
        (None, _) => true,
        (Some(parent), Some(_)) => parent.as_os_str().is_empty(),
        (Some(_), None) => true,
    };
    if is_root {
        return Err(Error::Config(format!(
            "CODERELAY_CURSOR_DATA_DIR must not be a filesystem root, got {}",
            path.display()
        )));
    }
    Ok(())
}

/// Resolves the directory holding the local certificate authority.
///
/// Deliberately **not** inside [`managed_data_dir`]. The CA directory contains
/// the private key that is trusted system-wide, while the data directory holds a
/// disposable SQLite cache. Keeping them apart means the "zip up
/// `cursor-bridge/` and send me the logs" debugging workflow cannot accidentally
/// ship the signing key along with it. `CODERELAY_CURSOR_CA_DIR` overrides,
/// mirroring the data-dir contract so tests can isolate it.
pub fn managed_ca_dir() -> Result<PathBuf> {
    let ca_dir = match std::env::var_os(CA_DIR_ENV) {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => {
            let home_dir = dirs::home_dir()
                .ok_or_else(|| Error::Config("cannot resolve user home directory".into()))?;
            home_dir.join(CA_DIR_NAME)
        }
    };
    let created = !ca_dir.is_dir();
    fs::create_dir_all(&ca_dir)?;
    #[cfg(unix)]
    fs::set_permissions(&ca_dir, fs::Permissions::from_mode(0o700))?;
    // On Windows the mode bits above are inert, so the directory would otherwise
    // keep whatever the profile inherited. Only done at creation: re-running it
    // on every access would spawn a process per call for no gain.
    #[cfg(windows)]
    if created {
        restrict_directory_to_current_user(&ca_dir);
    }
    Ok(ca_dir)
}

/// Replaces the inherited ACL on `dir` with one that names this account.
///
/// Windows ignores the `0o700` that the unix path applies, so without this the
/// CA directory simply inherits the profile's ACL. In practice that already
/// excludes other standard users, so this is defence in depth rather than the
/// load-bearing control — the load-bearing ones are DPAPI on the key and the
/// CurrentUser trust scope. It is stated plainly here so nobody reads
/// "hardened" as stronger than it is: **any process running as this user can
/// still read the key**, and that is inherent to a local MITM proxy.
///
/// `icacls` is used instead of `SetNamedSecurityInfoW` because building the ACL
/// by hand needs the current user's SID, and the short path to that is
/// `OpenProcessToken` + `GetTokenInformation` + `CopySid` — considerably more
/// unsafe surface than one documented command line. Failure is logged and
/// ignored: a CA that works with inherited permissions beats a bridge that will
/// not start because `icacls` was unavailable.
#[cfg(windows)]
fn restrict_directory_to_current_user(dir: &std::path::Path) {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};

    /// Keeps the console from flashing on screen for a windowless sidecar.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let Ok(user) = std::env::var("USERNAME") else {
        return;
    };
    if user.trim().is_empty() {
        return;
    }
    // `*S-1-5-18` is SYSTEM by well-known SID: naming it as a literal would break
    // on a non-English Windows, where the account is not called "SYSTEM".
    let outcome = Command::new("icacls")
        .arg(dir)
        .arg("/inheritance:r")
        .arg("/grant:r")
        .arg(format!("{user}:(OI)(CI)F"))
        .arg("*S-1-5-18:(OI)(CI)F")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .status();
    match outcome {
        Ok(status) if status.success() => {}
        Ok(status) => tracing::warn!(%status, "icacls could not restrict the CA directory ACL"),
        Err(error) => tracing::warn!(%error, "could not run icacls to restrict the CA directory ACL"),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderKind {
    OpenAiChat,
    OpenAiResponses,
    Anthropic,
}

#[derive(Clone)]
pub struct ProviderConfig {
    pub kind: ProviderKind,
    pub request_url: String,
    pub api_key: String,
    pub custom_headers: reqwest::header::HeaderMap,
    pub max_output_tokens: Option<u64>,
    pub request_timeout: Duration,
    pub allowed_body_fields: Option<std::collections::HashSet<String>>,
}

#[derive(Clone)]
pub struct Config {
    /// The address the service binds, from `CODERELAY_CURSOR_LISTEN_ADDR`.
    ///
    /// There is deliberately **no** persisted-port mode. Upstream has a
    /// `Config::desktop()` that read the port back from the settings table and
    /// a `use_persisted_ports` switch gating it, but nothing in this build ever
    /// turned it on: CodeRelay always passes an explicit
    /// `CODERELAY_CURSOR_LISTEN_ADDR` (its own preferences are the source of
    /// truth for the port) and reads the port the bridge actually bound from
    /// the `ready` line. Keeping the switch would advertise a second, never
    /// executed port-resolution path, so it was removed rather than left
    /// looking live.
    pub listen_addr: SocketAddr,
    pub database_url: String,
    pub provider_request_timeout: Duration,
    pub provider_stream_idle_timeout: Duration,
    pub console: Option<ConsoleSource>,
    /// 面向用户的应用版本;桌面壳会覆盖为自身版本,用于插件 minAppVersion 门控。
    pub app_version: String,
    /// PID of the process that spawned this bridge, from `--parent-pid`.
    ///
    /// `None` means "nobody asked us to watch a parent". When set, the bridge
    /// shuts itself down if that process disappears, so a CodeRelay crash or a
    /// Task Manager kill cannot leave an orphaned bridge holding the database
    /// and the Cursor injection. Same flag and semantics as the Go relay
    /// sidecar.
    pub parent_pid: Option<u32>,
    /// Shared secret required on every control-API request.
    ///
    /// `None` means the token was **not configured**, and in that case the
    /// control API refuses every request rather than falling open. The control
    /// API shares its port with the Cursor protocol and can read model
    /// credentials and toggle the system-wide MITM injection, so "no token" has
    /// to mean "closed", not "unauthenticated but available".
    pub control_token: Option<String>,
}

#[derive(Clone)]
pub enum ConsoleSource {
    Directory(PathBuf),
    Proxy(url::Url),
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let listen_addr = env::var("CODERELAY_CURSOR_LISTEN_ADDR")
            .unwrap_or_else(|_| "127.0.0.1:3000".into())
            .parse()
            .map_err(|error| error_config("CODERELAY_CURSOR_LISTEN_ADDR", error))?;
        require_loopback(listen_addr)?;
        let request_timeout = match env::var("CODERELAY_CURSOR_PROVIDER_TIMEOUT_SECONDS") {
            Ok(value) => Duration::from_secs(value.parse().map_err(|error| {
                error_config("CODERELAY_CURSOR_PROVIDER_TIMEOUT_SECONDS", error)
            })?),
            Err(env::VarError::NotPresent) => DEFAULT_PROVIDER_REQUEST_TIMEOUT,
            Err(error) => {
                return Err(error_config("CODERELAY_CURSOR_PROVIDER_TIMEOUT_SECONDS", error))
            }
        };
        let console_dir = env::var_os("CODERELAY_CURSOR_CONSOLE_DIR").map(PathBuf::from);
        let console_proxy = env::var("CODERELAY_CURSOR_CONSOLE_PROXY")
            .ok()
            .map(|value| {
                value
                    .parse()
                    .map_err(|error| error_config("CODERELAY_CURSOR_CONSOLE_PROXY", error))
            })
            .transpose()?;
        let console = match (console_dir, console_proxy) {
            (Some(_), Some(_)) => {
                return Err(Error::Config(
                    "CODERELAY_CURSOR_CONSOLE_DIR and CODERELAY_CURSOR_CONSOLE_PROXY cannot both be set"
                        .into(),
                ))
            }
            (Some(directory), None) => Some(ConsoleSource::Directory(directory)),
            (None, Some(proxy)) => Some(ConsoleSource::Proxy(proxy)),
            (None, None) => None,
        };
        Ok(Self {
            listen_addr,
            database_url: database_url_from_env()?,
            provider_request_timeout: request_timeout,
            provider_stream_idle_timeout: DEFAULT_PROVIDER_STREAM_IDLE_TIMEOUT,
            console,
            app_version: env!("CARGO_PKG_VERSION").into(),
            parent_pid: parent_pid_from_args()?,
            control_token: control_token_from_env(),
        })
    }
}

/// Rejects a listen address that would expose the server beyond this machine.
///
/// The control API shares this port with the Cursor protocol, and it can be
/// used to read model credentials and toggle the system-wide MITM injection. The
/// CORS allowlist and the control token are the browser-side and local-process
/// defences; binding is what keeps the whole thing off the network, and it must
/// therefore fail closed at startup rather than accept `0.0.0.0`.
fn require_loopback(address: SocketAddr) -> Result<()> {
    if address.ip().is_loopback() {
        return Ok(());
    }
    Err(Error::Config(format!(
        "CODERELAY_CURSOR_LISTEN_ADDR must be a loopback address, got {address}; the control API \
         must not be reachable from the network"
    )))
}

/// Reads the control-API token from the environment.
fn control_token_from_env() -> Option<String> {
    parse_control_token(env::var(CONTROL_TOKEN_ENV).ok())
}

/// The testable half of [`control_token_from_env`].
///
/// An empty value is treated as "not configured" rather than as a valid
/// zero-length secret, because `set FOO=` is a common way to clear a variable
/// and must not silently produce a token that any caller can guess.
fn parse_control_token(value: Option<String>) -> Option<String> {
    let value = value?;
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

/// Reads `--parent-pid <pid>` from the command line.
///
/// Deliberately a hand-parsed scan rather than a CLI framework: the bridge takes
/// exactly one optional argument beside the environment configuration, and
/// adding a parser dependency to read one integer would be a poor trade. The
/// flag name and the "absent means no watchdog" behaviour match
/// `sidecars/coderelay-proxy`, which is the sidecar CodeRelay already spawns.
///
/// `--parent-pid` without a value, or with a value that is not a pid, is an
/// error rather than a silent ignore: a typo would otherwise disable the
/// watchdog without anyone noticing until an orphan showed up.
fn parent_pid_from_args() -> Result<Option<u32>> {
    parse_parent_pid(env::args_os().skip(1))
}

/// The testable half of [`parent_pid_from_args`].
fn parse_parent_pid<I, S>(args: I) -> Result<Option<u32>>
where
    I: IntoIterator<Item = S>,
    S: Into<std::ffi::OsString>,
{
    let mut args = args.into_iter().map(Into::into);
    while let Some(argument) = args.next() {
        let argument = argument.to_string_lossy().into_owned();
        let value = match argument.strip_prefix("--parent-pid=") {
            Some(inline) => inline.to_string(),
            None if argument == "--parent-pid" => args
                .next()
                .map(|value| value.to_string_lossy().into_owned())
                .ok_or_else(|| Error::Config("--parent-pid requires a value".into()))?,
            None => continue,
        };
        let pid = value
            .trim()
            .parse::<u32>()
            .map_err(|error| error_config("--parent-pid", error))?;
        if pid == 0 {
            return Err(Error::Config("--parent-pid must be a positive pid".into()));
        }
        return Ok(Some(pid));
    }
    Ok(None)
}

fn database_url_from_env() -> Result<String> {
    match env::var("CODERELAY_CURSOR_DATABASE_URL") {
        Ok(database_url) => Ok(database_url),
        Err(env::VarError::NotPresent) => default_database_url(),
        Err(error) => Err(error_config("CODERELAY_CURSOR_DATABASE_URL", error)),
    }
}

fn default_database_url() -> Result<String> {
    let data_dir = managed_data_dir()?;
    database_url_for_dir(&data_dir)
}

fn database_url_for_dir(data_dir: &std::path::Path) -> Result<String> {
    let database_path = data_dir.join(DATABASE_FILE_NAME);
    let database_path = database_path
        .to_str()
        .ok_or_else(|| Error::Config("database path is not valid UTF-8".into()))?;
    Ok(format!("sqlite://{database_path}"))
}

/// Formats an invalid-environment-variable error with the variable name.
///
/// The environment prefix is `CODERELAY_CURSOR_`, so these names must stay in sync
/// with the reads above; a mismatch would send users looking for a variable that
/// does not exist.
fn error_config(value: &str, error: impl std::fmt::Display) -> Error {
    Error::Config(format!("invalid {value}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_dir_rejects_relative_paths() {
        // A relative path resolves against whatever the working directory
        // happens to be, which breaks the "state lives in one known directory"
        // contract — and the working directory is not ours to choose.
        assert!(validate_data_dir(Path::new("bridge-data")).is_err());
        assert!(validate_data_dir(Path::new("./bridge-data")).is_err());
        assert!(validate_data_dir(Path::new("..")).is_err());
    }

    #[test]
    fn data_dir_rejects_filesystem_roots() {
        // Writing the database to a root, and then applying an ACL to it on
        // Windows, is never the intent.
        #[cfg(unix)]
        assert!(validate_data_dir(Path::new("/")).is_err());
        #[cfg(windows)]
        {
            assert!(validate_data_dir(Path::new("C:\\")).is_err());
            assert!(validate_data_dir(Path::new("C:/")).is_err());
        }
    }

    #[test]
    fn data_dir_accepts_a_normal_absolute_directory() {
        #[cfg(unix)]
        let path = Path::new("/var/lib/coderelay-cursor-bridge");
        #[cfg(windows)]
        let path = Path::new("C:\\Users\\Example\\AppData\\Roaming\\coderelay");
        assert!(validate_data_dir(path).is_ok());
    }

    #[test]
    fn provider_timeout_defaults_match_runtime_boundaries() {
        assert_eq!(
            DEFAULT_PROVIDER_STREAM_IDLE_TIMEOUT,
            Duration::from_secs(30 * 60)
        );
        assert_eq!(
            DEFAULT_PROVIDER_REQUEST_TIMEOUT,
            Duration::from_secs(60 * 60)
        );
    }

    #[test]
    fn parent_pid_is_absent_unless_requested() {
        // Absent means "no watchdog", which is what every existing caller that
        // does not pass the flag must keep meaning.
        assert_eq!(parse_parent_pid(Vec::<String>::new()).unwrap(), None);
        assert_eq!(
            parse_parent_pid(vec!["--other".to_string()]).unwrap(),
            None
        );
    }

    #[test]
    fn parent_pid_accepts_both_flag_spellings() {
        assert_eq!(
            parse_parent_pid(vec!["--parent-pid".to_string(), "4242".to_string()]).unwrap(),
            Some(4242)
        );
        assert_eq!(
            parse_parent_pid(vec!["--parent-pid=4242".to_string()]).unwrap(),
            Some(4242)
        );
        // Order must not matter: the flag is scanned for, not positional.
        assert_eq!(
            parse_parent_pid(vec![
                "--verbose".to_string(),
                "--parent-pid".to_string(),
                "7".to_string()
            ])
            .unwrap(),
            Some(7)
        );
    }

    #[test]
    fn a_malformed_parent_pid_is_an_error_not_a_silent_ignore() {
        // Silently dropping these would disable the orphan watchdog without
        // anyone noticing until a stuck bridge showed up.
        assert!(parse_parent_pid(vec!["--parent-pid".to_string()]).is_err());
        assert!(parse_parent_pid(vec!["--parent-pid".to_string(), "abc".to_string()]).is_err());
        assert!(parse_parent_pid(vec!["--parent-pid=0".to_string()]).is_err());
    }

    #[test]
    fn an_absent_or_blank_control_token_means_unconfigured() {
        // `None` is what makes the control API refuse every request, so these
        // must not accidentally become `Some("")` — an empty secret that any
        // caller could guess.
        assert_eq!(parse_control_token(None), None);
        assert_eq!(parse_control_token(Some(String::new())), None);
        assert_eq!(parse_control_token(Some("   ".to_string())), None);
        // `set FOO=` on Windows and `FOO=` on POSIX both land here.
        assert_eq!(parse_control_token(Some("\t\n ".to_string())), None);
    }

    #[test]
    fn a_configured_control_token_is_trimmed_not_dropped() {
        // Surrounding whitespace is an artifact of how the variable was set (a
        // shell quirk or a trailing newline in a script), not part of the
        // secret, so it is stripped rather than rejecting a usable token.
        assert_eq!(
            parse_control_token(Some("s3cret".to_string())),
            Some("s3cret".to_string())
        );
        assert_eq!(
            parse_control_token(Some(" s3cret \n".to_string())),
            Some("s3cret".to_string())
        );
        // Interior whitespace is significant; only the ends are trimmed.
        assert_eq!(
            parse_control_token(Some(" s3 cret ".to_string())),
            Some("s3 cret".to_string())
        );
    }

    #[test]
    fn only_loopback_listen_addresses_are_accepted() {
        // The control API shares this port and can toggle a system-wide MITM, so
        // a routable bind has to be a startup error rather than a warning.
        assert!(require_loopback("127.0.0.1:3000".parse().unwrap()).is_ok());
        assert!(require_loopback("127.0.0.1:0".parse().unwrap()).is_ok());
        assert!(require_loopback("[::1]:3000".parse().unwrap()).is_ok());

        assert!(require_loopback("0.0.0.0:3000".parse().unwrap()).is_err());
        assert!(require_loopback("192.168.1.10:3000".parse().unwrap()).is_err());
        assert!(require_loopback("[::]:3000".parse().unwrap()).is_err());
    }
}
