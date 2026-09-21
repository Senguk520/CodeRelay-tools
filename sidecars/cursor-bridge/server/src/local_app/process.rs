//! Terminates the Cursor desktop process before an explicit takeover.

use tokio::process::Command;

use crate::{Error, Result};

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// How long to give Cursor to close itself after a graceful request.
///
/// Cursor flushes `state.vscdb` while shutting down, and that flush is what
/// keeps the account database consistent. Long enough to cover a normal quit on
/// a busy profile, short enough that the user is not left staring at a spinner.
#[cfg(windows)]
const GRACEFUL_SHUTDOWN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[cfg(windows)]
const GRACEFUL_SHUTDOWN_POLL: std::time::Duration = std::time::Duration::from_millis(500);

#[cfg(windows)]
fn hide_console(command: &mut Command) {
    command.creation_flags(CREATE_NO_WINDOW);
}

pub async fn terminate_cursor() -> Result<()> {
    terminate_platform_cursor().await
}

/// Whether the Cursor desktop process is currently running.
///
/// Exposed so the injection path can refuse to write `state.vscdb` while Cursor
/// holds it open, instead of assuming the termination above succeeded.
pub async fn cursor_running() -> Result<bool> {
    #[cfg(target_os = "windows")]
    {
        cursor_is_running().await
    }
    #[cfg(target_os = "macos")]
    {
        unix_process_running("Cursor").await
    }
    #[cfg(target_os = "linux")]
    {
        Ok(unix_process_running("cursor").await? || unix_process_running("Cursor").await?)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        Ok(false)
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
async fn unix_process_running(name: &str) -> Result<bool> {
    let running = Command::new("pgrep").args(["-x", name]).status().await?;
    match running.code() {
        // `pgrep` uses exit code 1 for "no match", which is a normal answer.
        Some(1) => Ok(false),
        _ if running.success() => Ok(true),
        _ => Err(Error::Config(format!(
            "failed to inspect the {name} process"
        ))),
    }
}

#[cfg(target_os = "macos")]
async fn terminate_platform_cursor() -> Result<()> {
    terminate_unix_process("Cursor").await
}

#[cfg(target_os = "linux")]
async fn terminate_platform_cursor() -> Result<()> {
    terminate_unix_process("cursor").await?;
    terminate_unix_process("Cursor").await
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
async fn terminate_unix_process(name: &str) -> Result<()> {
    let running = Command::new("pgrep").args(["-x", name]).status().await?;
    if !running.success() {
        return match running.code() {
            Some(1) => Ok(()),
            _ => Err(Error::Config(format!(
                "failed to inspect the {name} process"
            ))),
        };
    }
    // `pkill -x` without `-9` asks for a normal termination first. Cursor writes
    // its account database on the way out, so a SIGKILL here risks the same
    // inconsistent state the Windows path avoids.
    let terminated = Command::new("pkill").args(["-x", name]).status().await?;
    if terminated.success() || terminated.code() == Some(1) {
        Ok(())
    } else {
        Err(Error::Config(format!(
            "failed to terminate the {name} process"
        )))
    }
}

/// Whether a `Cursor.exe` process is alive right now.
///
/// Kept separate from the kill sequence so the same probe can answer both "is
/// there anything to do" and "did the graceful request work".
#[cfg(windows)]
async fn cursor_is_running() -> Result<bool> {
    let mut list = Command::new("tasklist");
    hide_console(&mut list);
    let processes = list
        .args(["/FI", "IMAGENAME eq Cursor.exe", "/NH", "/FO", "CSV"])
        .output()
        .await?;
    if !processes.status.success() {
        return Err(Error::Config(
            "failed to inspect the Cursor.exe process".into(),
        ));
    }
    // `tasklist` prints an informational line instead of rows when nothing
    // matches, so the image name has to be looked for explicitly rather than
    // treating "non-empty output" as "running".
    Ok(String::from_utf8_lossy(&processes.stdout)
        .to_ascii_lowercase()
        .contains("cursor.exe"))
}

#[cfg(windows)]
async fn taskkill(args: &[&str]) -> Result<std::process::ExitStatus> {
    let mut kill = Command::new("taskkill");
    hide_console(&mut kill);
    Ok(kill.args(args).status().await?)
}

/// Shuts Cursor down, asking politely before insisting.
///
/// The forced path (`/F`) is what the previous implementation used on its own.
/// It works, but it skips Cursor's shutdown sequence — the one that flushes and
/// closes `state.vscdb`. Since the very next step writes that database to inject
/// the local account, a forced kill is the difference between "Cursor saved
/// cleanly and is gone" and "Cursor was cut off mid-write and the database needs
/// recovery". So: request a normal close, give it a bounded window, then
/// escalate only if it is still alive.
#[cfg(target_os = "windows")]
async fn terminate_platform_cursor() -> Result<()> {
    if !cursor_is_running().await? {
        return Ok(());
    }

    // No `/F`: this posts a close request rather than terminating.
    let graceful = taskkill(&["/T", "/IM", "Cursor.exe"]).await?;
    // A non-zero exit here means the request itself was rejected, not that
    // Cursor refused to close — that distinction is settled by the poll below,
    // so the status is only worth a log line.
    if !graceful.success() {
        tracing::debug!(code = ?graceful.code(), "graceful Cursor close request was rejected");
    }

    let deadline = std::time::Instant::now() + GRACEFUL_SHUTDOWN_TIMEOUT;
    while std::time::Instant::now() < deadline {
        tokio::time::sleep(GRACEFUL_SHUTDOWN_POLL).await;
        if !cursor_is_running().await? {
            tracing::info!("Cursor closed gracefully");
            return Ok(());
        }
    }

    // Still there after the window: it is either showing a save prompt or hung.
    // Waiting longer would leave the injection blocked indefinitely, so force it.
    tracing::warn!("Cursor did not close within the graceful window; forcing termination");
    let forced = taskkill(&["/F", "/T", "/IM", "Cursor.exe"]).await?;
    if forced.success() || !cursor_is_running().await? {
        Ok(())
    } else {
        Err(Error::Config(
            "failed to terminate the Cursor.exe process".into(),
        ))
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
async fn terminate_platform_cursor() -> Result<()> {
    Err(Error::Config(format!(
        "terminating Cursor is unsupported on {}",
        std::env::consts::OS
    )))
}
