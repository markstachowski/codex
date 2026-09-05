//! Recognition and normalization for restricted TUI structured requests.
//!
//! The marker returned from this module is server-owned. Client metadata is only
//! used to recognize the exact request shape emitted by the reviewed TUI client;
//! recognized requests are then rebuilt with the managed background contract.

use super::managed_desktop_requests::managed_desktop_temporary_structured_lane_for_policy;
use crate::error_code::invalid_request;
use codex_app_server_protocol::AskForApproval;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::SandboxMode;
use codex_app_server_protocol::ThreadStartParams;
use codex_core::config::ModelPolicyLane;
use std::collections::HashMap;

const CODEX_TUI_CLIENT_NAME: &str = "codex-tui";

pub(super) fn managed_temporary_structured_lane(
    params: &ThreadStartParams,
    app_server_client_name: Option<&str>,
) -> Result<Option<ModelPolicyLane>, JSONRPCErrorError> {
    let lane = codex_core::config::locked_model_policy_lane()
        .map_err(|error| invalid_request(error.to_string()))?;
    Ok(managed_temporary_structured_lane_for_policy(
        params,
        app_server_client_name,
        lane,
    ))
}

fn managed_temporary_structured_lane_for_policy(
    params: &ThreadStartParams,
    app_server_client_name: Option<&str>,
    lane: Option<ModelPolicyLane>,
) -> Option<ModelPolicyLane> {
    let lane = lane?;
    if managed_tui_temporary_structured_start(params, app_server_client_name) {
        return Some(lane);
    }
    managed_desktop_temporary_structured_lane_for_policy(params, app_server_client_name, Some(lane))
}

fn managed_tui_temporary_structured_start(
    params: &ThreadStartParams,
    app_server_client_name: Option<&str>,
) -> bool {
    let system_source = matches!(
        params.thread_source.as_ref(),
        Some(codex_app_server_protocol::ThreadSource::Feature(source)) if source == "system"
    );
    if !system_source || app_server_client_name != Some(CODEX_TUI_CLIENT_NAME) {
        return false;
    }
    let Some(config) = params.config.as_ref() else {
        return false;
    };
    let bool_overrides = codex_core::config::MANAGED_TEMPORARY_STRUCTURED_DISABLED_BOOL_OVERRIDES;
    let bool_overrides_are_disabled = bool_overrides
        .iter()
        .all(|key| config.get(*key).is_some_and(|value| value == false));
    let mcp_servers_are_disabled = config
        .get("mcp_servers")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|servers| {
            servers.values().all(|server| {
                server.as_object().is_some_and(|server| {
                    server.len() == 1
                        && server
                            .get("enabled")
                            .is_some_and(|enabled| enabled == false)
                })
            })
        });
    let exact_config_shape = config.len() == bool_overrides.len() + 5
        && bool_overrides_are_disabled
        && config
            .get("web_search")
            .is_some_and(|value| value == "disabled")
        && mcp_servers_are_disabled
        && config.contains_key("model_reasoning_effort")
        && config.contains_key("plan_mode_reasoning_effort")
        && config.contains_key("service_tier");
    let exact_request_shape = !params.allow_provider_model_fallback
        && params.service_tier.is_none()
        && params.cwd.is_some()
        && params
            .runtime_workspace_roots
            .as_ref()
            .is_some_and(Vec::is_empty)
        && params.approval_policy == Some(AskForApproval::Never)
        && params.approvals_reviewer.is_none()
        && params.sandbox == Some(SandboxMode::ReadOnly)
        && params.permissions.is_none()
        && params.service_name.is_none()
        && params.base_instructions.is_none()
        && params.developer_instructions.is_none()
        && params.personality.is_none()
        && params.multi_agent_mode.is_none()
        && params.ephemeral == Some(true)
        && params.history_mode.is_none()
        && params.session_start_source.is_none()
        && params.project_id.is_none()
        && params.environments.as_ref().is_some_and(Vec::is_empty)
        && params.dynamic_tools.as_ref().is_some_and(Vec::is_empty)
        && params
            .selected_capability_roots
            .as_ref()
            .is_some_and(Vec::is_empty)
        && params.mock_experimental_field.is_none()
        && !params.experimental_raw_events;
    exact_request_shape && exact_config_shape
}

pub(super) fn managed_temporary_structured_config() -> HashMap<String, serde_json::Value> {
    let mut config = codex_core::config::MANAGED_TEMPORARY_STRUCTURED_DISABLED_BOOL_OVERRIDES
        .iter()
        .map(|key| ((*key).to_string(), serde_json::Value::Bool(false)))
        .collect::<HashMap<_, _>>();
    config.extend([
        (
            "include_permissions_instructions".to_string(),
            serde_json::json!(false),
        ),
        (
            "include_apps_instructions".to_string(),
            serde_json::json!(false),
        ),
        (
            "include_environment_context".to_string(),
            serde_json::json!(false),
        ),
        ("project_doc_max_bytes".to_string(), serde_json::json!(0)),
        (
            "skills".to_string(),
            serde_json::json!({
                "bundled": { "enabled": false },
                "config": [],
                "include_instructions": false,
            }),
        ),
        (
            "memories".to_string(),
            serde_json::json!({
                "generate_memories": false,
                "use_memories": false,
            }),
        ),
        ("web_search".to_string(), serde_json::json!("disabled")),
        ("mcp_servers".to_string(), serde_json::json!({})),
        (
            "model_reasoning_effort".to_string(),
            serde_json::json!("ultra"),
        ),
        (
            "plan_mode_reasoning_effort".to_string(),
            serde_json::json!("ultra"),
        ),
        (
            "service_tier".to_string(),
            serde_json::json!(codex_protocol::config_types::SERVICE_TIER_DEFAULT_REQUEST_VALUE),
        ),
    ]);
    config
}

#[cfg(test)]
#[path = "managed_temporary_requests_tests.rs"]
mod tests;
