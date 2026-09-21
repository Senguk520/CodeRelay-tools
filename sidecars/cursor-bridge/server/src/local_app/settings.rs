//! Integrates local application settings.
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Error, Result};

const NO_PROXY_KEY: &str = "http.noProxy";
const KEYS: [&str; 5] = [
    "http.proxy",
    "http.proxyKerberosServicePrincipal",
    "http.proxySupport",
    "cursor.general.disableHttp2",
    "http.experimental.systemCertificatesV2",
];

/// Copy of the user's original file, taken before the first managed rewrite.
///
/// Never overwritten once it exists: the point is to preserve the *first*
/// original, so a second takeover cannot replace the backup with an already
/// modified file and quietly destroy the only clean copy.
const SETTINGS_BACKUP_SUFFIX: &str = "json.coderelay-backup";

/// Records what this program removed from `settings.json`, so it can be put back.
///
/// `http.noProxy` is the one setting that is *deleted* rather than overwritten,
/// because a proxy exclusion list would route `cursor.sh` around the very proxy
/// the takeover depends on. Deleting it without a record would lose the user's
/// list permanently — closing the injection does not bring it back, and nothing
/// tells the user it is gone.
const RESIDUE_FILE: &str = "cursor-settings-residue.json";

#[derive(Debug, Default, Deserialize, Serialize)]
struct SettingsResidue {
    /// `true` once a record has been written.
    recorded: bool,
    /// The user's original `http.noProxy`. `None` means the key was absent, which
    /// is why this is an `Option` inside the record rather than a bare `Option`
    /// field: "absent" and "never recorded" have to stay distinguishable or the
    /// restore would fabricate a key the user never had.
    no_proxy: Option<Value>,
}

/// The two file locations the managed rewrite touches.
///
/// Threaded through as a parameter rather than resolved from the environment at
/// each call site for two reasons: the environment is process-global, so tests
/// that redirected it would race every other test in the crate; and keeping the
/// resolution at the outermost functions makes it obvious that only the public
/// entry points depend on where the user's profile lives.
struct Locations {
    settings: PathBuf,
    residue: PathBuf,
}

fn locations() -> Result<Locations> {
    Ok(Locations {
        settings: path()?,
        residue: residue_path()?,
    })
}

fn residue_path() -> Result<PathBuf> {
    Ok(crate::config::managed_data_dir()?.join(RESIDUE_FILE))
}

fn read_residue(residue_path: &Path) -> SettingsResidue {
    let Ok(text) = fs::read_to_string(residue_path) else {
        return SettingsResidue::default();
    };
    serde_json::from_str(&text).unwrap_or_else(|error| {
        // A corrupt record must not block the takeover; the cost is a lost
        // `http.noProxy`, which is recoverable by hand.
        tracing::warn!(%error, "ignoring an unreadable settings-residue record");
        SettingsResidue::default()
    })
}

fn write_residue(residue_path: &Path, residue: &SettingsResidue) -> Result<()> {
    if let Some(parent) = residue_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(residue_path, serde_json::to_vec_pretty(residue)?)?;
    Ok(())
}

fn clear_residue(residue_path: &Path) {
    let _ = fs::remove_file(residue_path);
}

fn path() -> Result<PathBuf> {
    let home = dirs::home_dir()
        .ok_or_else(|| Error::Config("cannot resolve user home directory".into()))?;
    match std::env::consts::OS {
        "macos" => Ok(home.join("Library/Application Support/Cursor/User/settings.json")),
        "windows" => Ok(std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("AppData/Roaming"))
            .join("Cursor/User/settings.json")),
        "linux" => Ok(std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"))
            .join("Cursor/User/settings.json")),
        platform => Err(Error::Config(format!(
            "Cursor settings are unsupported on {platform}"
        ))),
    }
}

fn read_at(path: &Path) -> Result<BTreeMap<String, Value>> {
    let data = match fs::read_to_string(path) {
        Ok(data) => data,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => return Err(error.into()),
    };
    if data.trim().is_empty() {
        return Ok(BTreeMap::new());
    }
    json5::from_str(&data)
        .map_err(|error| Error::Config(format!("parse Cursor settings JSONC: {error}")))
}

/// Copies the user's file aside before the first rewrite.
///
/// The rewrite is lossy by construction — comments, formatting and key order
/// all go, because a `json5` read is written back as plain pretty JSON. A backup
/// is what makes that recoverable rather than destructive.
fn backup_once_at(path: &Path) {
    let backup = path.with_extension(SETTINGS_BACKUP_SUFFIX);
    if backup.exists() || !path.is_file() {
        return;
    }
    match fs::copy(path, &backup) {
        Ok(_) => tracing::info!(path = %backup.display(), "backed up Cursor settings.json before rewriting"),
        Err(error) => tracing::warn!(%error, "could not back up Cursor settings.json"),
    }
}

fn write_at(path: &Path, settings: &BTreeMap<String, Value>) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let data = serde_json::to_vec_pretty(settings)?;
    let temp = path.with_extension("json.tmp");
    fs::write(&temp, [data.as_slice(), b"\n"].concat())?;
    fs::rename(temp, path)?;
    Ok(())
}

pub fn write_proxy_settings(proxy_url: &str) -> Result<()> {
    let locations = locations()?;
    write_proxy_settings_at(&locations, proxy_url)
}

fn write_proxy_settings_at(locations: &Locations, proxy_url: &str) -> Result<()> {
    let mut settings = read_at(&locations.settings)?;
    backup_once_at(&locations.settings);
    // Record what is about to be deleted, but only on the first takeover: a
    // later call would otherwise record the already-cleared state and erase the
    // user's list from the record too.
    let residue = read_residue(&locations.residue);
    if !residue.recorded {
        write_residue(
            &locations.residue,
            &SettingsResidue {
                recorded: true,
                no_proxy: settings.get(NO_PROXY_KEY).cloned(),
            },
        )?;
    }
    settings.remove(NO_PROXY_KEY);
    settings.insert(KEYS[0].into(), Value::String(proxy_url.into()));
    settings.insert(KEYS[1].into(), Value::String(proxy_url.into()));
    settings.insert(KEYS[2].into(), Value::String("on".into()));
    settings.insert(KEYS[3].into(), Value::Bool(true));
    settings.insert(KEYS[4].into(), Value::Bool(true));
    write_at(&locations.settings, &settings)
}

pub fn clear_proxy_settings() -> Result<()> {
    let locations = locations()?;
    clear_proxy_settings_at(&locations)
}

fn clear_proxy_settings_at(locations: &Locations) -> Result<()> {
    let mut settings = read_at(&locations.settings)?;
    let before = settings.len();
    for key in KEYS {
        settings.remove(key);
    }
    // Put back what `write_proxy_settings` removed. Done here rather than only on
    // the explicit-off path so every clear — explicit disable, stale cleanup,
    // shutdown — restores it.
    let residue = read_residue(&locations.residue);
    if residue.recorded {
        match residue.no_proxy {
            Some(value) => {
                settings.insert(NO_PROXY_KEY.into(), value);
            }
            None => {
                settings.remove(NO_PROXY_KEY);
            }
        }
        clear_residue(&locations.residue);
        write_at(&locations.settings, &settings)?;
        return Ok(());
    }
    if settings.len() != before {
        write_at(&locations.settings, &settings)?;
    }
    Ok(())
}

pub fn settings_match(proxy_url: &str) -> Result<bool> {
    settings_match_at(&path()?, proxy_url)
}

fn settings_match_at(settings_path: &Path, proxy_url: &str) -> Result<bool> {
    let settings = read_at(settings_path)?;
    Ok(
        settings.get(KEYS[0]) == Some(&Value::String(proxy_url.into()))
            && settings.get(KEYS[1]) == Some(&Value::String(proxy_url.into()))
            && settings.get(KEYS[2]) == Some(&Value::String("on".into()))
            && settings.get(KEYS[3]) == Some(&Value::Bool(true))
            && settings.get(KEYS[4]) == Some(&Value::Bool(true)),
    )
}

pub fn clear_stale_managed_settings() -> Result<()> {
    let locations = locations()?;
    clear_stale_managed_settings_at(&locations)
}

fn clear_stale_managed_settings_at(locations: &Locations) -> Result<()> {
    let settings = read_at(&locations.settings)?;
    let managed_signature = settings.get(KEYS[2]) == Some(&Value::String("on".into()))
        && settings.get(KEYS[3]) == Some(&Value::Bool(true))
        && settings.get(KEYS[4]) == Some(&Value::Bool(true));
    let loopback = settings
        .get(KEYS[0])
        .and_then(Value::as_str)
        .and_then(|value| value.parse::<reqwest::Url>().ok())
        .and_then(|url| url.host_str().map(str::to_owned))
        .is_some_and(|host| matches!(host.as_str(), "127.0.0.1" | "localhost" | "::1"));
    if managed_signature && loopback {
        clear_proxy_settings_at(locations)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANAGED_URL: &str = "http://127.0.0.1:6538";

    /// A throwaway profile: one `settings.json` and one residue record, both
    /// under a temp directory. Passing paths in avoids touching the process
    /// environment, which every other test in the crate shares.
    struct Fixture {
        directory: tempfile::TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            Self { directory }
        }

        fn locations(&self) -> Locations {
            Locations {
                settings: self.directory.path().join("Cursor/User/settings.json"),
                residue: self.directory.path().join("data/cursor-settings-residue.json"),
            }
        }

        fn seed(&self, text: &str) {
            let locations = self.locations();
            fs::create_dir_all(locations.settings.parent().unwrap()).unwrap();
            fs::write(&locations.settings, text).unwrap();
        }

        fn settings(&self) -> BTreeMap<String, Value> {
            read_at(&self.locations().settings).unwrap()
        }

        fn raw(&self) -> String {
            fs::read_to_string(self.locations().settings).unwrap()
        }
    }

    #[test]
    fn the_first_takeover_backs_up_the_original_file() {
        let fixture = Fixture::new();
        let original = "{ // a comment json5 tolerates\n  \"editor.fontSize\": 15\n}\n";
        fixture.seed(original);

        write_proxy_settings_at(&fixture.locations(), MANAGED_URL).unwrap();

        let backup = fixture
            .locations()
            .settings
            .with_extension(SETTINGS_BACKUP_SUFFIX);
        assert_eq!(
            fs::read_to_string(&backup).unwrap(),
            original,
            "the backup must be a byte-for-byte copy, comments included"
        );
    }

    #[test]
    fn a_second_takeover_does_not_replace_the_backup() {
        let fixture = Fixture::new();
        let original = "{\n  \"editor.fontSize\": 15\n}\n";
        fixture.seed(original);

        write_proxy_settings_at(&fixture.locations(), MANAGED_URL).unwrap();
        // The file now holds the managed rewrite; a second takeover must not
        // copy *that* over the only clean original.
        write_proxy_settings_at(&fixture.locations(), "http://127.0.0.1:9999").unwrap();

        let backup = fixture
            .locations()
            .settings
            .with_extension(SETTINGS_BACKUP_SUFFIX);
        assert_eq!(fs::read_to_string(&backup).unwrap(), original);
    }

    #[test]
    fn no_proxy_is_restored_after_a_clear() {
        let fixture = Fixture::new();
        fixture.seed(
            "{\n  \"http.noProxy\": \"localhost,*.internal.example\",\n  \"editor.fontSize\": 15\n}\n",
        );
        let locations = fixture.locations();

        write_proxy_settings_at(&locations, MANAGED_URL).unwrap();
        assert_eq!(
            fixture.settings().get(NO_PROXY_KEY),
            None,
            "the exclusion list must be removed while the takeover is active"
        );

        clear_proxy_settings_at(&locations).unwrap();
        let settings = fixture.settings();
        assert_eq!(
            settings.get(NO_PROXY_KEY),
            Some(&Value::String("localhost,*.internal.example".into())),
            "the user's exclusion list must come back"
        );
        assert_eq!(settings.get("editor.fontSize"), Some(&Value::from(15)));
    }

    #[test]
    fn a_missing_no_proxy_is_not_fabricated_on_restore() {
        let fixture = Fixture::new();
        fixture.seed("{\n  \"editor.fontSize\": 15\n}\n");
        let locations = fixture.locations();

        write_proxy_settings_at(&locations, MANAGED_URL).unwrap();
        clear_proxy_settings_at(&locations).unwrap();

        assert_eq!(
            fixture.settings().get(NO_PROXY_KEY),
            None,
            "a key the user never had must not appear after a round trip"
        );
    }

    #[test]
    fn a_clear_removes_every_managed_key_and_keeps_user_keys() {
        let fixture = Fixture::new();
        fixture.seed("{\n  \"workbench.colorTheme\": \"Default Dark+\"\n}\n");
        let locations = fixture.locations();

        write_proxy_settings_at(&locations, MANAGED_URL).unwrap();
        clear_proxy_settings_at(&locations).unwrap();

        let settings = fixture.settings();
        for key in KEYS {
            assert_eq!(settings.get(key), None, "{key} must be gone");
        }
        assert_eq!(
            settings.get("workbench.colorTheme"),
            Some(&Value::String("Default Dark+".into()))
        );
    }

    #[test]
    fn the_residue_record_is_cleared_once_restored() {
        let fixture = Fixture::new();
        fixture.seed("{\n  \"http.noProxy\": \"localhost\"\n}\n");
        let locations = fixture.locations();

        write_proxy_settings_at(&locations, MANAGED_URL).unwrap();
        assert!(locations.residue.is_file());

        clear_proxy_settings_at(&locations).unwrap();
        assert!(
            !locations.residue.is_file(),
            "a stale record would re-insert the list on a later clear"
        );
    }

    #[test]
    fn a_users_own_proxy_is_left_alone_by_the_stale_cleanup() {
        let fixture = Fixture::new();
        // Same key, but no managed signature: this is the user's own config.
        fixture.seed("{\n  \"http.proxy\": \"http://127.0.0.1:8080\"\n}\n");

        clear_stale_managed_settings_at(&fixture.locations()).unwrap();

        assert_eq!(
            fixture.settings().get(KEYS[0]),
            Some(&Value::String("http://127.0.0.1:8080".into())),
            "an unsigned proxy configuration must survive the stale cleanup"
        );
    }

    #[test]
    fn a_managed_rewrite_is_recognised_as_stale() {
        let fixture = Fixture::new();
        let locations = fixture.locations();
        write_proxy_settings_at(&locations, MANAGED_URL).unwrap();

        clear_stale_managed_settings_at(&locations).unwrap();

        assert_eq!(fixture.settings().get(KEYS[0]), None);
    }

    #[test]
    fn settings_match_reports_the_managed_rewrite() {
        let fixture = Fixture::new();
        let locations = fixture.locations();

        write_proxy_settings_at(&locations, MANAGED_URL).unwrap();

        assert!(settings_match_at(&locations.settings, MANAGED_URL).unwrap());
        assert!(!settings_match_at(&locations.settings, "http://127.0.0.1:1").unwrap());
    }

    #[test]
    fn the_original_file_survives_a_round_trip_through_the_backup() {
        let fixture = Fixture::new();
        let original = "{\n  \"http.noProxy\": \"localhost\",\n  \"editor.fontSize\": 15\n}\n";
        fixture.seed(original);
        let locations = fixture.locations();

        write_proxy_settings_at(&locations, MANAGED_URL).unwrap();
        clear_proxy_settings_at(&locations).unwrap();

        // The managed write is a normalising rewrite (JSONC -> pretty JSON), so
        // the file is not byte-identical; the backup is what preserves the
        // original exactly.
        let backup = locations.settings.with_extension(SETTINGS_BACKUP_SUFFIX);
        assert_eq!(fs::read_to_string(backup).unwrap(), original);
        assert!(
            fixture.raw().contains("\"http.noProxy\": \"localhost\""),
            "the exclusion list must be present after the round trip"
        );
    }
}
