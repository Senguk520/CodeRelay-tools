//! Implements model configuration endpoints.
use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;

use crate::{
    model::{ModelConfig, ModelConfigInput},
    Result,
};

use super::{
    ControlService, DiscoveredModels, ModelConnectivityResult, ModelDiscoveryInput,
};

#[derive(Deserialize)]
pub struct SaveModels {
    pub models: Vec<ModelConfigInput>,
}

#[derive(Deserialize)]
pub struct ModelOrder {
    pub model_hashes: Vec<String>,
}

pub async fn list(State(service): State<ControlService>) -> Result<Json<Vec<ModelConfig>>> {
    Ok(Json(service.models().await?))
}

pub async fn create(
    State(service): State<ControlService>,
    Json(input): Json<SaveModels>,
) -> Result<(StatusCode, Json<Vec<ModelConfig>>)> {
    Ok((
        StatusCode::CREATED,
        Json(service.create_models(&input.models).await?),
    ))
}

/// Replaces every configured model with the posted set.
///
/// CodeRelay calls this instead of `create` when it owns the binding list: the
/// bridge's rows are a cache of that list, so anything not in the request is
/// stale and must go. `PUT` (rather than `POST`) marks the replace semantics.
pub async fn reconcile(
    State(service): State<ControlService>,
    Json(input): Json<SaveModels>,
) -> Result<Json<Vec<ModelConfig>>> {
    Ok(Json(service.reconcile_models(&input.models).await?))
}

pub async fn reorder(
    State(service): State<ControlService>,
    Json(input): Json<ModelOrder>,
) -> Result<Json<Vec<ModelConfig>>> {
    Ok(Json(service.reorder_models(&input.model_hashes).await?))
}

pub async fn remove(
    State(service): State<ControlService>,
    Path(model_hash): Path<String>,
) -> Result<StatusCode> {
    service.delete_model(&model_hash).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn update(
    State(service): State<ControlService>,
    Path(model_hash): Path<String>,
    Json(input): Json<ModelConfigInput>,
) -> Result<Json<ModelConfig>> {
    Ok(Json(service.update_model(&model_hash, &input).await?))
}

pub async fn test(
    State(service): State<ControlService>,
    Path((model_hash, test_id)): Path<(String, String)>,
) -> Result<Json<ModelConnectivityResult>> {
    Ok(Json(service.test_model(&model_hash, &test_id).await?))
}

pub async fn cancel(
    State(service): State<ControlService>,
    Path((_model_hash, test_id)): Path<(String, String)>,
) -> Result<StatusCode> {
    service.cancel_model_test(&test_id);
    Ok(StatusCode::NO_CONTENT)
}

pub async fn discover(
    State(service): State<ControlService>,
    Json(input): Json<ModelDiscoveryInput>,
) -> Result<Json<DiscoveredModels>> {
    Ok(Json(service.discover_models(&input).await?))
}
