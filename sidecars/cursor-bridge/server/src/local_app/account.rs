//! Integrates local Cursor account state.
use std::{
    fs,
    path::{Path, PathBuf},
};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde_json::json;
use sqlx::{Connection, Row, SqliteConnection};

use crate::{Error, Result};

const EMAIL: &str = "cursor@ai.com";
const SIGN_UP_TYPE: &str = "Google";
const SUBJECT: &str = "cursor-local-user";
const MEMBERSHIP_TYPE: &str = "ultra";
const SUBSCRIPTION_STATUS: &str = "active";

const ACCESS_TOKEN_KEY: &str = "cursorAuth/accessToken";

/// How many pre-injection backups of `state.vscdb` to keep.
///
/// A backup is taken before every injection, and re-attaching after a restart
/// injects again, so without a cap this would accumulate a multi-megabyte copy
/// in the user's Cursor profile on every start.
const STATE_DB_BACKUPS_TO_KEEP: usize = 3;

pub async fn inject_if_missing() -> Result<()> {
    inject_if_missing_at(&state_db_path()?).await
}

fn state_db_path() -> Result<PathBuf> {
    let home = dirs::home_dir()
        .ok_or_else(|| Error::Config("cannot resolve user home directory".into()))?;
    match std::env::consts::OS {
        "macos" => {
            Ok(home.join("Library/Application Support/Cursor/User/globalStorage/state.vscdb"))
        }
        "windows" => Ok(std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("AppData/Roaming"))
            .join("Cursor/User/globalStorage/state.vscdb")),
        "linux" => Ok(std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"))
            .join("Cursor/User/globalStorage/state.vscdb")),
        platform => Err(Error::Config(format!(
            "Cursor account injection is unsupported on {platform}"
        ))),
    }
}

async fn inject_if_missing_at(path: &Path) -> Result<()> {
    // No `create_if_missing`: when Cursor has never run there is no account
    // database, and creating one would fabricate a `state.vscdb` — holding an
    // account that never logged in — inside a profile Cursor has not set up.
    // Refusing to write is the honest outcome; the caller reports it.
    if !path.is_file() {
        return Err(Error::Config(format!(
            "Cursor account database not found at {}: is Cursor installed, and has it been opened at least once?",
            path.display()
        )));
    }
    // A write timeout rather than an instant failure: the usual reason this
    // database is locked is that Cursor is still shutting down, and waiting
    // briefly turns a spurious "cannot inject" into a successful injection.
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(path)
        .busy_timeout(std::time::Duration::from_secs(5));
    let mut connection = SqliteConnection::connect_with(&options).await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS ItemTable (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB)",
    )
    .execute(&mut connection)
    .await?;

    let token = local_token()?;
    let account = sqlx::query("SELECT CAST(value AS TEXT) AS value FROM ItemTable WHERE key = ?")
        .bind(ACCESS_TOKEN_KEY)
        .fetch_optional(&mut connection)
        .await?;
    if let Some(row) = account {
        // Fail closed. The previous guard wrapped `try_get::<String>` in
        // `is_ok_and`, so a read that *failed* — a non-UTF-8 BLOB, say — made the
        // guard `false` and let the injection proceed over a real login. Reading
        // the raw bytes and treating an unreadable value as occupied means the
        // injection can never overwrite a login it failed to understand.
        let bytes = row.try_get::<Vec<u8>, _>("value")?;
        let existing = String::from_utf8_lossy(&bytes);
        let existing = existing.trim();
        if existing.is_empty() {
            // An empty row is not a login; fall through and inject.
        } else if existing == token {
            // Our own token from an earlier attach: fall through so the derived
            // membership fields get refreshed (Cursor clears them).
        } else {
            tracing::info!(
                "skipping Cursor account injection: a different token is already present"
            );
            return Ok(());
        }
    }

    backup_state_db(path)?;

    let values = [
        (ACCESS_TOKEN_KEY, token.as_str()),
        ("cursorAuth/refreshToken", token.as_str()),
        ("cursorAuth/cachedEmail", EMAIL),
        ("cursorAuth/cachedSignUpType", SIGN_UP_TYPE),
        ("cursorAuth/stripeMembershipAuthId", SUBJECT),
        ("cursorAuth/stripeMembershipType", MEMBERSHIP_TYPE),
        ("cursorAuth/stripeSubscriptionStatus", SUBSCRIPTION_STATUS),
    ];
    let mut transaction = connection.begin().await?;
    for (key, value) in values {
        sqlx::query("INSERT OR REPLACE INTO ItemTable(key, value) VALUES(?, ?)")
            .bind(key)
            .bind(value)
            .execute(&mut *transaction)
            .await?;
    }
    transaction.commit().await?;
    tracing::info!(
        email = EMAIL,
        subject = SUBJECT,
        "injected local Cursor account"
    );
    Ok(())
}

/// Copies the account database aside before a write, keeping the newest few.
///
/// The file holds the user's real login state, so an injection that goes wrong
/// has to be recoverable. The name carries a timestamp because Cursor rewrites
/// this database continuously — a single fixed backup name would be replaced by
/// an unrelated later write and stop describing "the state before we touched
/// it". A failure to prune is logged and ignored: failing the injection because
/// a stale backup could not be deleted would be the wrong trade.
fn backup_state_db(path: &Path) -> Result<()> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default();
    let backup = path.with_extension(format!("vscdb.coderelay-backup-{stamp}"));
    fs::copy(path, &backup)?;
    tracing::info!(path = %backup.display(), "backed up Cursor state.vscdb before injecting the local account");
    prune_state_db_backups(path);
    Ok(())
}

/// The name prefix every backup of `path` shares, used to write and to find them.
fn backup_prefix(path: &Path) -> Option<String> {
    Some(format!("{}.coderelay-backup-", path.file_name()?.to_str()?))
}

fn prune_state_db_backups(path: &Path) {
    let (Some(parent), Some(prefix)) = (path.parent(), backup_prefix(path)) else {
        return;
    };
    let Ok(entries) = fs::read_dir(parent) else {
        return;
    };
    let mut backups: Vec<_> = entries
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with(&prefix))
        })
        .map(|entry| entry.path())
        .collect();
    // The timestamp is fixed-width, so a lexical sort is a chronological one.
    backups.sort();
    for stale in backups.iter().rev().skip(STATE_DB_BACKUPS_TO_KEEP) {
        if let Err(error) = fs::remove_file(stale) {
            tracing::warn!(%error, path = %stale.display(), "could not remove an old state.vscdb backup");
        }
    }
}

pub(crate) fn is_local_cursor_authorization(authorization: &str) -> bool {
    authorization
        .strip_prefix("Bearer ")
        .is_some_and(is_local_cursor_token)
}

fn is_local_cursor_token(token: &str) -> bool {
    local_token().is_ok_and(|local| local == token)
}

pub(super) fn local_token() -> Result<String> {
    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"HS256","typ":"JWT"}"#);
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&json!({
        "sub": SUBJECT,
        "email": EMAIL,
        "type": "session",
        "iss": "cursor-client",
        "scope": "openid profile email",
        "exp": 4070908800_u64
    }))?);
    Ok(format!("{header}.{payload}.{SUBJECT}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn database_url(path: &Path) -> String {
        format!("sqlite:{}", path.display())
    }

    /// Creates an empty SQLite database at `path`, standing in for a Cursor
    /// profile that has been opened at least once.
    fn seed_empty_database(path: &Path) {
        fs::write(path, b"").unwrap();
    }

    async fn connect(path: &Path) -> SqliteConnection {
        SqliteConnection::connect(&database_url(path)).await.unwrap()
    }

    async fn scalar(path: &Path, key: &str) -> String {
        let mut connection = connect(path).await;
        sqlx::query_scalar("SELECT CAST(value AS TEXT) FROM ItemTable WHERE key = ?")
            .bind(key)
            .fetch_one(&mut connection)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn reinjection_repairs_local_membership_cache() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.vscdb");
        seed_empty_database(&path);
        inject_if_missing_at(&path).await.unwrap();

        let mut connection = connect(&path).await;
        sqlx::query("UPDATE ItemTable SET value = 'free' WHERE key = ?")
            .bind("cursorAuth/stripeMembershipType")
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::query("DELETE FROM ItemTable WHERE key = ?")
            .bind("cursorAuth/stripeMembershipAuthId")
            .execute(&mut connection)
            .await
            .unwrap();
        drop(connection);

        inject_if_missing_at(&path).await.unwrap();

        assert_eq!(
            scalar(&path, "cursorAuth/stripeMembershipType").await,
            MEMBERSHIP_TYPE
        );
        assert_eq!(scalar(&path, "cursorAuth/stripeMembershipAuthId").await, SUBJECT);
    }

    #[tokio::test]
    async fn a_missing_database_is_refused_instead_of_created() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.vscdb");

        assert!(inject_if_missing_at(&path).await.is_err());
        assert!(
            !path.exists(),
            "the injection must not fabricate a Cursor profile database"
        );
    }

    #[tokio::test]
    async fn an_existing_login_is_never_overwritten() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.vscdb");
        seed_empty_database(&path);
        let mut connection = connect(&path).await;
        sqlx::query("CREATE TABLE ItemTable (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB)")
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::query("INSERT INTO ItemTable(key, value) VALUES(?, ?)")
            .bind(ACCESS_TOKEN_KEY)
            // Non-UTF-8: exactly the read that used to fail open and let the
            // injection clobber a real login.
            .bind(vec![0xff, 0xfe, 0x00, 0x41])
            .execute(&mut connection)
            .await
            .unwrap();
        drop(connection);

        inject_if_missing_at(&path).await.unwrap();

        let mut connection = connect(&path).await;
        let stored: Vec<u8> = sqlx::query_scalar("SELECT value FROM ItemTable WHERE key = ?")
            .bind(ACCESS_TOKEN_KEY)
            .fetch_one(&mut connection)
            .await
            .unwrap();
        assert_eq!(stored, vec![0xff, 0xfe, 0x00, 0x41]);
    }

    #[tokio::test]
    async fn state_backups_are_bounded() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.vscdb");
        seed_empty_database(&path);

        for _ in 0..(STATE_DB_BACKUPS_TO_KEEP + 2) {
            inject_if_missing_at(&path).await.unwrap();
        }

        let prefix = backup_prefix(&path).unwrap();
        let count = fs::read_dir(directory.path())
            .unwrap()
            .flatten()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with(&prefix))
            })
            .count();
        assert!(count <= STATE_DB_BACKUPS_TO_KEEP, "kept {count} backups");
    }

    #[test]
    fn recognizes_only_the_injected_cursor_token() {
        let token = local_token().unwrap();
        assert!(is_local_cursor_token(&token));
        assert!(is_local_cursor_authorization(&format!("Bearer {token}")));
        assert!(!is_local_cursor_authorization(&token));
        assert!(!is_local_cursor_authorization(
            "Bearer official-cursor-token"
        ));
        assert!(!is_local_cursor_token("official-cursor-token"));
        assert!(!is_local_cursor_token(""));
    }
}
