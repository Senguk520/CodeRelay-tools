//! Assembles server dependencies and starts the application services.
use std::{future::IntoFuture, time::Duration};

use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use crate::{
    api,
    config::{Config, ConsoleSource},
    control,
    cursor::{
        prompting::{PromptAssets, PromptCompiler},
        transport::TransportRegistry,
    },
    local_app::CursorHarness,
    provider::ProviderRouter,
    store::Store,
    Result,
};

pub struct App {
    config: Config,
    router: axum::Router,
    registry: TransportRegistry,
    harness: CursorHarness,
    store: Store,
}

impl App {
    pub async fn new(config: Config) -> Result<Self> {
        let store = Store::connect(&config.database_url).await?;
        let assets = PromptAssets::embedded()?;
        let compiler = PromptCompiler::new(assets);
        let clients = crate::network::NetworkClients::new(store.clone());
        let provider = std::sync::Arc::new(ProviderRouter::new(
            store.clone(),
            clients.clone(),
            config.provider_request_timeout,
            config.provider_stream_idle_timeout,
        ));
        let registry = TransportRegistry::with_local_rules(
            store.clone(),
            provider.clone(),
            compiler,
            crate::config::managed_data_dir()?.join("rules"),
        );
        let control = control::ControlService::new(store.clone(), provider, clients.clone())?;
        let harness = control.cursor_harness().clone();
        // Self-heal: a previous run may have died (crash, taskkill, power loss)
        // while Cursor's settings.json still pointed at its in-process proxy.
        // The port is gone with the process, so those settings only mean "Cursor
        // has no working network". Clearing them here — before anything can serve
        // a status read — is what makes the failure recoverable without the user
        // having to know what happened. The persisted takeover flag is left
        // alone, so CodeRelay can re-attach the injection deliberately.
        if let Err(error) = harness.cleanup_stale_settings().await {
            tracing::warn!(%error, "failed to clear stale Cursor proxy settings");
        }
        let mut router = api::router(registry.clone(), clients)?;
        router = match &config.console {
            Some(ConsoleSource::Directory(directory)) => router.merge(control::web_router(
                control.clone(),
                directory,
                config.control_token.clone(),
            )),
            Some(ConsoleSource::Proxy(target)) => router.merge(control::proxy_web_router(
                control.clone(),
                target.clone(),
                config.control_token.clone(),
            )),
            None => router.merge(control::api_router(control.clone(), config.control_token.clone())),
        };
        Ok(Self {
            router,
            registry,
            harness,
            store,
            config,
        })
    }

    pub fn merge_router(mut self, router: axum::Router) -> Self {
        self.router = self.router.merge(router);
        self
    }

    pub async fn bind(&self) -> Result<TcpListener> {
        // Fails closed when the requested port is taken. CodeRelay chose that
        // port — or left it at 0 for the OS to assign — and learns the bound
        // value from the `ready` line, so silently binding a different one would
        // make its own preference look honoured when it was not.
        Ok(TcpListener::bind(self.config.listen_addr).await?)
    }

    pub fn harness(&self) -> CursorHarness {
        self.harness.clone()
    }

    pub fn store(&self) -> Store {
        self.store.clone()
    }

    pub async fn serve(self) -> Result<()> {
        let listener = self.bind().await?;
        self.serve_bound(listener).await
    }

    /// Binds, announces the bound port on stdout, then serves.
    ///
    /// CodeRelay's process manager performs the ready handshake by reading one
    /// JSON object per line from stdout, the same contract the Go relay sidecar
    /// uses, so the port must be known before the first line is written.
    pub async fn serve_announcing_ready(self) -> Result<()> {
        let listener = self.bind().await?;
        println!("{{\"type\":\"ready\",\"port\":{}}}", listener.local_addr()?.port());
        self.serve_bound(listener).await
    }

    async fn serve_bound(self, listener: TcpListener) -> Result<()> {
        let shutdown = CancellationToken::new();
        let signal_shutdown = shutdown.clone();
        // A sidecar must not outlive CodeRelay. If the parent dies
        // uncooperatively (crash, Task Manager kill, force-terminate), nothing
        // calls our shutdown hook, so the watchdog is what turns that into a
        // clean exit instead of an orphan holding the database file and Cursor's
        // injected proxy settings.
        if let Some(parent_pid) = self.config.parent_pid {
            crate::parent_monitor::watch(parent_pid, shutdown.clone());
        }
        let parent_shutdown = shutdown.clone();
        let running = self.serve_on(listener, shutdown);
        tokio::pin!(running);
        tokio::select! {
            result = &mut running => result,
            () = shutdown_signal() => {
                tracing::info!("shutdown signal received; cancelling active runs");
                signal_shutdown.cancel();
                running.await
            }
            () = parent_shutdown.cancelled() => {
                // The watchdog has already logged why. Awaiting `running` lets
                // `serve_on` run its graceful path, which disables the harness
                // and reverts Cursor's settings before the process ends.
                tracing::info!("shutdown requested by the parent watchdog");
                running.await
            }
        }
    }

    pub async fn serve_on(self, listener: TcpListener, shutdown: CancellationToken) -> Result<()> {
        let address = listener.local_addr()?;
        self.registry.web_cache().set_service_addr(address);
        self.harness.set_backend_addr(address);
        tracing::info!(%address, "cursor server listening");
        let registry = self.registry;
        let harness = self.harness;
        let graceful = shutdown.clone();
        let server = axum::serve(listener, self.router)
            .with_graceful_shutdown(async move {
                graceful.cancelled().await;
            })
            .into_future();
        tokio::pin!(server);

        tokio::select! {
            result = &mut server => {
                if let Err(error) = harness.disable().await {
                    tracing::warn!(%error, "failed to disable Cursor harness after server stop");
                }
                result?
            },
            () = shutdown.cancelled() => {
                if let Err(error) = harness.disable().await {
                    tracing::warn!(%error, "failed to disable Cursor harness during shutdown");
                }
                registry.shutdown().await;
                match tokio::time::timeout(Duration::from_secs(10), &mut server).await {
                    Ok(result) => result?,
                    Err(_) => tracing::warn!("graceful shutdown timed out; forcing server close"),
                }
            }
        }
        Ok(())
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            signal.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = terminate => {} }
}
