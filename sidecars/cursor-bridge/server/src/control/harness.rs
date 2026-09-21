//! Implements local application control endpoints.
use axum::{extract::State, Json};

use crate::{
    local_app::{CursorHarnessStatus, SetEnabled},
    Result,
};

use super::ControlService;

pub async fn status(State(service): State<ControlService>) -> Result<Json<CursorHarnessStatus>> {
    Ok(Json(service.cursor_harness().status().await?))
}

pub async fn initialize_ca(
    State(service): State<ControlService>,
) -> Result<Json<CursorHarnessStatus>> {
    Ok(Json(service.cursor_harness().initialize_ca().await?))
}

pub async fn set_enabled(
    State(service): State<ControlService>,
    Json(input): Json<SetEnabled>,
) -> Result<Json<CursorHarnessStatus>> {
    Ok(Json(
        service.cursor_harness().set_enabled(input.enabled).await?,
    ))
}

/// Removes the Cursor-side injection while keeping the takeover flag.
///
/// CodeRelay calls this on "stop bridge" and on application exit: the proxy is
/// an in-process instance of this server, so once the process is gone the
/// settings it wrote point at nothing and Cursor loses its network. This is the
/// last moment at which the revert can still work.
pub async fn clear_injection(
    State(service): State<ControlService>,
) -> Result<Json<CursorHarnessStatus>> {
    service.cursor_harness().clear_injection_only().await?;
    Ok(Json(service.cursor_harness().status().await?))
}
