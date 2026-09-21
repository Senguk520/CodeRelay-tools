//! Exposes the local desktop application integration.
mod account;
mod ca;
mod process;
mod proxy;
mod settings;

use std::{net::SocketAddr, sync::Arc};

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::{store::Store, Error, Result};

use self::{ca::CaManager, proxy::ProxyRuntime};

pub(crate) fn proxy_host_allowed(host: &str) -> bool {
    proxy::is_cursor_host(host)
}

pub(crate) fn request_uses_local_cursor_token(headers: &axum::http::HeaderMap) -> bool {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .is_some_and(account::is_local_cursor_authorization)
}

#[cfg(test)]
pub(crate) fn local_cursor_authorization() -> String {
    format!("Bearer {}", account::local_token().unwrap())
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaState {
    Missing,
    Untrusted,
    Ready,
    Invalid,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationState {
    Disabled,
    Enabled,
    Degraded,
}

#[derive(Clone, Debug, Serialize)]
pub struct CursorHarnessStatus {
    pub platform: &'static str,
    pub ca: CaState,
    pub configured_models: usize,
    pub enabled_models: usize,
    pub integration: IntegrationState,
    pub settings_applied: bool,
    pub proxy_url: Option<String>,
    pub ca_install_command: Option<String>,
    /// The persisted, explicitly-set takeover intent.
    ///
    /// Reads `false` when the row is missing, which is the whole point: an
    /// absent row means "never asked", not "asked for". CodeRelay uses this to
    /// re-attach the injection after a restart only when the user really did
    /// turn it on.
    pub takeover_requested: bool,
    /// Whether Cursor had to be terminated on the last attempt and that attempt
    /// failed. Surfaced so the user is told why the proxy settings may not have
    /// taken effect yet instead of silently wondering.
    pub cursor_terminate_failed: bool,
}

#[derive(Clone, Copy, Debug, Deserialize)]
pub struct SetEnabled {
    pub enabled: bool,
}

#[derive(Clone)]
pub struct CursorHarness {
    inner: Arc<Inner>,
}

struct Inner {
    store: Store,
    ca: CaManager,
    ca_initialization: Mutex<()>,
    backend_addr: RwLock<Option<SocketAddr>>,
    proxy: Mutex<ProxyRuntime>,
    /// Set from `enable()` so `status()` can report a failed Cursor shutdown
    /// without re-attempting it.
    cursor_terminate_failed: std::sync::atomic::AtomicBool,
}

impl CursorHarness {
    pub fn new(store: Store) -> Result<Self> {
        Ok(Self {
            inner: Arc::new(Inner {
                store,
                ca: CaManager::managed()?,
                ca_initialization: Mutex::new(()),
                backend_addr: RwLock::new(None),
                proxy: Mutex::new(ProxyRuntime::default()),
                cursor_terminate_failed: std::sync::atomic::AtomicBool::new(false),
            }),
        })
    }

    pub fn set_backend_addr(&self, addr: SocketAddr) {
        *self.inner.backend_addr.write() = Some(addr);
    }

    pub async fn proxy_port(&self) -> Option<u16> {
        self.inner.proxy.lock().await.port()
    }

    pub async fn cleanup_stale_settings(&self) -> Result<()> {
        settings::clear_stale_managed_settings()
    }

    /// Read-only probe of the takeover state.
    ///
    /// This deliberately has **no** side effects. It used to call `enable()` when
    /// the persisted takeover flag was set, which made a plain status read
    /// rewrite Cursor's `settings.json`, terminate Cursor, and inject the local
    /// account. The decision to take over belongs to an explicit command
    /// (`set_enabled`), never to a read.
    pub async fn status(&self) -> Result<CursorHarnessStatus> {
        let models = self.inner.store.models().await?;
        let configured_models = models.len();
        let enabled_models = configured_models;
        let ca = self.inner.ca.state()?;
        let proxy = self.inner.proxy.lock().await;
        let proxy_url = proxy.url();
        let settings_applied = proxy_url
            .as_deref()
            .map(settings::settings_match)
            .transpose()?
            .unwrap_or(false);
        let integration = match (proxy.running(), settings_applied) {
            (false, false) => IntegrationState::Disabled,
            (true, true) => IntegrationState::Enabled,
            _ => IntegrationState::Degraded,
        };
        Ok(CursorHarnessStatus {
            platform: std::env::consts::OS,
            ca,
            configured_models,
            enabled_models,
            integration,
            settings_applied,
            proxy_url,
            ca_install_command: self.inner.ca.install_command(),
            takeover_requested: self.inner.store.cursor_takeover_enabled().await?,
            cursor_terminate_failed: self
                .inner
                .cursor_terminate_failed
                .load(std::sync::atomic::Ordering::SeqCst),
        })
    }

    pub async fn initialize_ca(&self) -> Result<CursorHarnessStatus> {
        let _initialization = self.inner.ca_initialization.lock().await;
        let manager = self.inner.ca.clone();
        tokio::task::spawn_blocking(move || manager.initialize_local())
            .await
            .map_err(|error| Error::Store(format!("CA initialization task failed: {error}")))??;
        self.status().await
    }

    pub async fn set_enabled(&self, enabled: bool) -> Result<CursorHarnessStatus> {
        if enabled {
            self.inner.store.set_cursor_takeover_enabled(true).await?;
            self.enable().await?;
        } else {
            self.inner.store.set_cursor_takeover_enabled(false).await?;
            self.disable().await?;
        }
        self.status().await
    }

    /// Removes the Cursor-side injection without touching the persisted
    /// takeover flag.
    ///
    /// Used by the shutdown and stop paths, where the reason to clear is "this
    /// process is going away and the in-process proxy with it", not "the user
    /// changed their mind". Keeping the flag means a restart can re-attach
    /// without the user having to remember the switch.
    ///
    /// It goes through `clear_stale_managed_settings` rather than
    /// `clear_proxy_settings`, and that distinction matters on this path: the
    /// stale check only removes the keys when they still carry this program's
    /// signature *and* point at a loopback address, so a proxy configuration the
    /// user set for their own reasons is left alone. Turning injection off via
    /// `set_enabled(false)` keeps the unconditional variant, because there the
    /// user has explicitly said "remove what you applied".
    pub async fn clear_injection_only(&self) -> Result<()> {
        settings::clear_stale_managed_settings()
    }

    async fn enable(&self) -> Result<()> {
        if !matches!(self.inner.ca.state()?, CaState::Ready) {
            return Err(Error::Config(
                "initialize and trust the CA before enabling Cursor".into(),
            ));
        }
        let backend_addr = self
            .inner
            .backend_addr
            .read()
            .ok_or_else(|| Error::Config("desktop management server is not ready".into()))?;
        let mut proxy = self.inner.proxy.lock().await;
        let settings_applied = proxy
            .url()
            .as_deref()
            .map(settings::settings_match)
            .transpose()?
            .unwrap_or(false);
        if !settings_applied {
            // Terminating Cursor only makes the freshly written http.proxy take effect
            // sooner; it is optional, so a failed probe or kill must not block takeover.
            // It is reported through `status()` instead: the user needs to know that
            // Cursor is still running with the old settings if the shutdown failed.
            let terminated = process::terminate_cursor().await;
            self.inner
                .cursor_terminate_failed
                .store(terminated.is_err(), std::sync::atomic::Ordering::SeqCst);
            if let Err(error) = terminated {
                tracing::warn!(%error, "could not terminate Cursor before applying proxy settings");
            }
        } else {
            self.inner
                .cursor_terminate_failed
                .store(false, std::sync::atomic::Ordering::SeqCst);
        }
        if proxy.running() {
            if let Some(url) = proxy.url() {
                apply_cursor_configuration(&url).await?;
            }
            return Ok(());
        }
        let ca = self.inner.ca.load()?;
        let requested_port = self.inner.store.port_settings().await?.proxy_port;
        // Drop any residue from a previous run before this one binds. With the
        // default requested port of 0 the OS hands out a different port on every
        // start, so a leftover `http.proxy` necessarily points at a port nothing
        // is listening on. Clearing first means the window between binding and
        // writing the new settings can never leave Cursor aimed at a dead port,
        // even if either step fails.
        settings::clear_stale_managed_settings()?;
        let (url, actual_port) = proxy.start(backend_addr, ca, requested_port).await?;
        if let Err(error) = self.inner.store.set_proxy_port(actual_port).await {
            proxy.stop().await;
            return Err(error);
        }
        if let Err(error) = apply_cursor_configuration(&url).await {
            proxy.stop().await;
            return Err(error);
        }
        Ok(())
    }

    pub async fn disable(&self) -> Result<()> {
        settings::clear_proxy_settings()?;
        self.inner.proxy.lock().await.stop().await;
        Ok(())
    }
}

async fn apply_cursor_configuration(proxy_url: &str) -> Result<()> {
    account::inject_if_missing().await?;
    settings::write_proxy_settings(proxy_url)
}
