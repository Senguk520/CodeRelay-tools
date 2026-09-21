//! Watches the process that spawned this bridge and shuts down when it exits.
//!
//! The bridge is a sidecar: CodeRelay owns its lifetime and stops it through
//! [`crate::app`]'s shutdown path. That path only runs when CodeRelay exits
//! *cooperatively*. If CodeRelay is killed from Task Manager, crashes, or is
//! force-terminated, nothing tells the bridge to stop, and it keeps running:
//! listening on its port, holding the SQLite connection to `cursor-bridge.db`,
//! and keeping the MITM injection it applied to Cursor alive. The next
//! CodeRelay launch then opens the same database as a second writer and can
//! fail its migration stage on the WAL contention, with a cause that is hard to
//! trace back to the orphan.
//!
//! `--parent-pid <pid>` closes that hole. It mirrors the Go relay sidecar
//! (`sidecars/coderelay-proxy/parent_monitor_windows.go`): same flag name, same
//! 1-second `WaitForSingleObject` polling shape, and the same fail-toward-exit
//! behaviour when the parent handle cannot be opened.
//!
//! The token passed in is the server's own shutdown token, so discovering the
//! parent's death takes the ordinary graceful path — the harness is disabled
//! and Cursor's `settings.json` is reverted before the process ends.

use tokio_util::sync::CancellationToken;

/// How long each platform wait blocks before re-checking for cancellation.
const POLL_INTERVAL_MS: u32 = 1_000;

/// Starts the parent watchdog on a dedicated thread.
///
/// A dedicated OS thread rather than a tokio task on purpose: the wait is a
/// blocking Win32 call, and parking a runtime worker on it for the whole
/// lifetime of the process would be a poor trade for a watchdog that does
/// nothing until the end.
pub fn watch(parent_pid: u32, shutdown: CancellationToken) {
    if parent_pid == 0 || parent_pid == std::process::id() {
        // A zero or self-referential pid would mean "the parent is me", which
        // would cancel the token immediately and make the bridge exit right
        // after announcing ready. Go's monitor ignores the same two cases.
        return;
    }
    std::thread::spawn(move || platform::watch(parent_pid, shutdown));
}

#[cfg(windows)]
mod platform {
    use std::io;

    use tokio_util::sync::CancellationToken;
    use windows_sys::Win32::{
        Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT},
        System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE},
    };

    use super::POLL_INTERVAL_MS;

    pub(super) fn watch(parent_pid: u32, shutdown: CancellationToken) {
        // SYNCHRONIZE is the least privilege that still allows waiting on the
        // handle. Requesting more would fail for processes owned by another
        // user, turning a permission detail into a spurious shutdown.
        let handle: HANDLE = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, parent_pid) };
        if handle.is_null() {
            let error = io::Error::last_os_error();
            // Fail toward exit, matching the Go sidecar. A pid we cannot open
            // most often means the parent is already gone — which is the exact
            // orphan this watchdog exists to prevent. Staying alive "because we
            // could not tell" is the outcome that leaves a stuck process behind.
            tracing::warn!(parent_pid, %error, "cannot open parent process handle; shutting down");
            shutdown.cancel();
            return;
        }

        loop {
            if shutdown.is_cancelled() {
                break;
            }
            match unsafe { WaitForSingleObject(handle, POLL_INTERVAL_MS) } {
                WAIT_OBJECT_0 => {
                    tracing::info!(parent_pid, "parent process exited; shutting down");
                    shutdown.cancel();
                    break;
                }
                WAIT_TIMEOUT => continue,
                status => {
                    let error = io::Error::last_os_error();
                    tracing::warn!(parent_pid, status, %error, "waiting on parent process failed; shutting down");
                    shutdown.cancel();
                    break;
                }
            }
        }

        unsafe { CloseHandle(handle) };
    }
}

#[cfg(unix)]
mod platform {
    use std::{path::Path, time::Duration};

    use tokio_util::sync::CancellationToken;

    use super::POLL_INTERVAL_MS;

    /// Whether the given pid is still present.
    ///
    /// Linux exposes this as a directory; on other unixes there is no portable
    /// std API for it and this reports "alive", i.e. it declines to act rather
    /// than guessing. The bridge ships on Windows, so the Windows handle-based
    /// implementation above is the one that carries the guarantee.
    fn is_alive(pid: u32) -> bool {
        if cfg!(target_os = "linux") {
            return Path::new(&format!("/proc/{pid}")).exists();
        }
        true
    }

    pub(super) fn watch(parent_pid: u32, shutdown: CancellationToken) {
        if !cfg!(target_os = "linux") {
            tracing::warn!(
                parent_pid,
                "parent process monitoring is only implemented on Windows and Linux; \
                 the bridge will not self-terminate if its parent dies"
            );
            return;
        }
        let interval = Duration::from_millis(u64::from(POLL_INTERVAL_MS));
        loop {
            if shutdown.is_cancelled() {
                break;
            }
            if !is_alive(parent_pid) {
                tracing::info!(parent_pid, "parent process exited; shutting down");
                shutdown.cancel();
                break;
            }
            std::thread::sleep(interval);
        }
    }
}
