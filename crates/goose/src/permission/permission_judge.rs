use crate::conversation::message::ToolRequest;
use crate::providers::base::Provider;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

async fn resolve_model_config(
    session_manager: &crate::session::SessionManager,
    session_id: &str,
) -> anyhow::Result<goose_providers::model::ModelConfig> {
    if !session_id.is_empty() {
        if let Ok(session) = session_manager.get_session(session_id, false).await {
            if let Some(model_config) = session.model_config {
                return Ok(model_config);
            }
        }
    }

    let config = crate::config::Config::global();
    let provider_name = config
        .get_goose_provider()
        .map_err(|_| anyhow::anyhow!("missing provider"))?;
    let model_name = config
        .get_goose_model()
        .map_err(|_| anyhow::anyhow!("missing model"))?;
    crate::model_config::model_config_from_user_config(&provider_name, &model_name)
}

pub async fn detect_read_only_requests(
    provider: Arc<dyn Provider>,
    session_manager: &crate::session::SessionManager,
    session_id: &str,
    requests: Vec<&ToolRequest>,
) -> Vec<String> {
    let config = match resolve_model_config(session_manager, session_id).await {
        Ok(config) => config,
        Err(error) => {
            tracing::warn!(%error, "Could not resolve permission judge model");
            return Vec::new();
        }
    };
    crate::session_context::with_session_id(
        Some(session_id.to_owned()),
        goose_agent::permission::detect_read_only_requests(provider.as_ref(), &config, requests),
    )
    .await
}

/// Result of permission checking for tool requests
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PermissionCheckResult {
    pub approved: Vec<ToolRequest>,
    pub needs_approval: Vec<ToolRequest>,
    pub denied: Vec<ToolRequest>,
}
