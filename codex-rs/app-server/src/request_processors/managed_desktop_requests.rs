//! Recognition and normalization for automatic requests from Codex Desktop.
//!
//! These predicates match reviewed, signed client request shapes. A match is
//! only a routing hint: the server replaces inference controls itself and owns
//! the resulting internal session source.

use crate::error_code::invalid_request;
use codex_app_server_protocol::AskForApproval;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::ThreadForkParams;
use codex_app_server_protocol::ThreadStartParams;
use codex_core::config::ModelPolicyLane;
use std::collections::HashMap;

const CODEX_DESKTOP_CLIENT_NAME: &str = "Codex Desktop";
const CODEX_DESKTOP_SERVICE_NAME: &str = "codex_desktop";
const CODEX_DESKTOP_METADATA_MODEL: &str = "gpt-5.6-luna";
const CODEX_DESKTOP_AMBIENT_MODEL: &str = "gpt-5.6-terra";
const CODEX_DESKTOP_AMBIENT_SAFETY_INSTRUCTIONS: &str = "Classify Codex ambient suggestion candidates for policy safety. Return only JSON matching the schema.";
const CODEX_DESKTOP_METADATA_FALSE_CONFIG_KEYS: [&str; 7] = [
    "features.enable_fanout",
    "features.hooks",
    "features.multi_agent",
    "features.multi_agent_v2",
    "features.plugins",
    "features.shell_snapshot",
    "features.tool_suggest",
];
const CODEX_DESKTOP_MESSAGE_FALSE_CONFIG_KEYS: [&str; 3] = [
    "features.enable_fanout",
    "features.multi_agent",
    "features.multi_agent_v2",
];
const CODEX_DESKTOP_START_STATSIG_FEATURE_KEYS: [&str; 35] = [
    "unified_exec",
    "unified_image_budget",
    "code_mode_buffered_exec",
    "code_mode_interrupt",
    "executed_tool_call_metadata",
    "shell_snapshot",
    "shell_snapshot_v2",
    "remote_models",
    "responses_websockets_v2",
    "standalone_web_search",
    "collaboration_modes",
    "default_mode_request_user_input",
    "request_rule",
    "image_generation",
    "item_ids",
    "image_detail_original",
    "image_resize_notice",
    "codex_git_commit",
    "workspace_dependencies",
    "guardian_approval",
    "write_stdin_approval",
    "guardian_reuse_parent_compaction",
    "apps_mcp_path_override",
    "mcp_oauth_refresh_coordination",
    "tool_search_always_defer_mcp_tools",
    "deferred_tool_world_state",
    "thread_tools",
    "settings_tools",
    "concurrent_reasoning_summaries",
    "powershell_shell_version",
    "image_generation_sse",
    "compaction_image_budget",
    "enable_mcp_apps",
    "recommended_plugins",
    "guardianv2",
];
const CODEX_DESKTOP_AMBIENT_SAFETY_FALSE_CONFIG_KEYS: [&str; 21] = [
    "features.apps",
    "features.plugins",
    "features.tool_suggest",
    "features.shell_tool",
    "features.unified_exec",
    "features.shell_snapshot",
    "features.apply_patch_freeform",
    "features.js_repl",
    "features.js_repl_tools_only",
    "features.code_mode",
    "features.code_mode_only",
    "features.multi_agent",
    "features.multi_agent_v2",
    "features.enable_fanout",
    "features.memories",
    "features.request_permissions_tool",
    "features.image_generation",
    "features.image_detail_original",
    "features.skill_mcp_dependency_install",
    "features.skill_env_var_dependency_prompt",
    "features.default_mode_request_user_input",
];

pub(super) fn managed_desktop_temporary_structured_lane_for_policy(
    params: &ThreadStartParams,
    app_server_client_name: Option<&str>,
    lane: Option<ModelPolicyLane>,
) -> Option<ModelPolicyLane> {
    let Some(ModelPolicyLane::Subscription) = lane else {
        return None;
    };
    (managed_desktop_metadata_start(params, app_server_client_name)
        || managed_desktop_ambient_safety_start(params, app_server_client_name)
        || managed_desktop_message_start(params, app_server_client_name))
    .then_some(ModelPolicyLane::Subscription)
}

pub(super) fn managed_desktop_background_lane(
    params: &ThreadStartParams,
    app_server_client_name: Option<&str>,
) -> Result<Option<ModelPolicyLane>, JSONRPCErrorError> {
    let lane = codex_core::config::locked_model_policy_lane()
        .map_err(|error| invalid_request(error.to_string()))?;
    Ok(managed_desktop_background_lane_for_policy(
        params,
        app_server_client_name,
        lane,
    ))
}

fn managed_desktop_background_lane_for_policy(
    params: &ThreadStartParams,
    app_server_client_name: Option<&str>,
    lane: Option<ModelPolicyLane>,
) -> Option<ModelPolicyLane> {
    let Some(ModelPolicyLane::Subscription) = lane else {
        return None;
    };
    managed_desktop_background_start(params, app_server_client_name)
        .then_some(ModelPolicyLane::Subscription)
}

fn codex_desktop_common_start_is_restricted(
    params: &ThreadStartParams,
    app_server_client_name: Option<&str>,
) -> bool {
    app_server_client_name == Some(CODEX_DESKTOP_CLIENT_NAME)
        && params.model_provider.is_none()
        && params.allow_provider_model_fallback
        && matches!(params.service_tier, Some(None))
        && params
            .runtime_workspace_roots
            .as_ref()
            .is_some_and(Vec::is_empty)
        && params.approval_policy == Some(AskForApproval::Never)
        && params.approvals_reviewer.is_none()
        && params.sandbox.is_none()
        && params.permissions.as_deref() == Some(":read-only")
        && params.service_name.as_deref() == Some(CODEX_DESKTOP_SERVICE_NAME)
        && params.personality.is_none()
        && params.multi_agent_mode.is_none()
        && params.ephemeral == Some(true)
        && params.history_mode.is_none()
        && params.session_start_source.is_none()
        && params.project_id.is_none()
        && params.environments.is_none()
        && params.dynamic_tools.is_none()
        && params.selected_capability_roots.is_none()
        && params.mock_experimental_field.is_none()
        && !params.experimental_raw_events
}

fn codex_desktop_apps_config_is_restricted(value: &serde_json::Value) -> bool {
    let Some(apps) = value.as_object() else {
        return false;
    };
    let Some(default) = apps.get("_default").and_then(serde_json::Value::as_object) else {
        return false;
    };
    let default_is_disabled = default.len() == 3
        && ["enabled", "destructive_enabled", "open_world_enabled"]
            .iter()
            .all(|key| default.get(*key).is_some_and(|value| value == false));
    if !default_is_disabled {
        return false;
    }
    apps.iter().all(|(app_id, value)| {
        if app_id == "_default" {
            return true;
        }
        let Some(app) = value.as_object() else {
            return false;
        };
        let Some(tools) = app.get("tools").and_then(serde_json::Value::as_object) else {
            return false;
        };
        app.len() == 5
            && app.get("enabled").is_some_and(|value| value == true)
            && app
                .get("destructive_enabled")
                .is_some_and(|value| value == false)
            && app
                .get("open_world_enabled")
                .is_some_and(|value| value == false)
            && app
                .get("default_tools_enabled")
                .is_some_and(|value| value == true)
            && tools.values().all(|tool| {
                tool.as_object().is_some_and(|tool| {
                    tool.len() == 1 && tool.get("enabled").is_some_and(|value| value == true)
                })
            })
    })
}

fn start_statsig_value_is_restricted(key: &str, value: &serde_json::Value) -> bool {
    key.strip_prefix("features.").is_some_and(|feature| {
        CODEX_DESKTOP_START_STATSIG_FEATURE_KEYS.contains(&feature)
            && (value.is_boolean() || (feature == "guardianv2" && value.is_object()))
    })
}

fn codex_desktop_metadata_config_is_restricted(
    config: &HashMap<String, serde_json::Value>,
    allow_start_statsig_features: bool,
) -> bool {
    let fixed_keys = [
        "model_reasoning_effort",
        "mcp_servers.codex_app",
        "features.apps",
        "apps",
        "web_search",
    ];
    if !config.keys().all(|key| {
        fixed_keys.contains(&key.as_str())
            || CODEX_DESKTOP_METADATA_FALSE_CONFIG_KEYS.contains(&key.as_str())
            || (allow_start_statsig_features
                && CODEX_DESKTOP_START_STATSIG_FEATURE_KEYS
                    .contains(&key.strip_prefix("features.").unwrap_or_default()))
    }) || !CODEX_DESKTOP_METADATA_FALSE_CONFIG_KEYS
        .iter()
        .all(|key| config.get(*key).is_some_and(|value| value == false))
    {
        return false;
    }
    let mcp_app_is_disabled = config
        .get("mcp_servers.codex_app")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|server| {
            server.len() == 2
                && server.get("enabled").is_some_and(|value| value == false)
                && server.get("command").is_some_and(|value| value == "")
        });
    let Some(apps) = config.get("apps") else {
        return false;
    };
    let apps_enabled_matches_allowlist = config
        .get("features.apps")
        .and_then(serde_json::Value::as_bool)
        .is_some_and(|enabled| {
            enabled
                == apps
                    .as_object()
                    .is_some_and(|apps| apps.keys().any(|key| key != "_default"))
        });
    let statsig_types_are_valid = config.iter().all(|(key, value)| {
        CODEX_DESKTOP_METADATA_FALSE_CONFIG_KEYS.contains(&key.as_str())
            || key == "features.apps"
            || !key.starts_with("features.")
            || start_statsig_value_is_restricted(key, value)
    });
    config
        .get("model_reasoning_effort")
        .is_some_and(|value| value == "low")
        && config
            .get("web_search")
            .is_some_and(|value| value == "disabled")
        && mcp_app_is_disabled
        && codex_desktop_apps_config_is_restricted(apps)
        && apps_enabled_matches_allowlist
        && statsig_types_are_valid
}

fn managed_desktop_metadata_start(
    params: &ThreadStartParams,
    app_server_client_name: Option<&str>,
) -> bool {
    codex_desktop_common_start_is_restricted(params, app_server_client_name)
        && params.model.as_deref() == Some(CODEX_DESKTOP_METADATA_MODEL)
        && params.base_instructions.is_none()
        && params.developer_instructions.is_none()
        && matches!(
            params.thread_source.as_ref(),
            Some(codex_app_server_protocol::ThreadSource::Feature(source))
                if matches!(source.as_str(), "thread_title" | "thread_summary")
        )
        && params.config.as_ref().is_some_and(|config| {
            codex_desktop_metadata_config_is_restricted(
                config, /*allow_start_statsig_features*/ true,
            )
        })
}

fn codex_desktop_ambient_safety_config_is_restricted(
    config: &HashMap<String, serde_json::Value>,
) -> bool {
    let fixed_keys = [
        "include_permissions_instructions",
        "include_apps_instructions",
        "include_environment_context",
        "project_doc_max_bytes",
        "apps",
        "skills",
        "memories",
        "web_search",
        "mcp_servers.codex_app",
        "model_reasoning_effort",
    ];
    if !config.keys().all(|key| {
        fixed_keys.contains(&key.as_str())
            || CODEX_DESKTOP_AMBIENT_SAFETY_FALSE_CONFIG_KEYS.contains(&key.as_str())
            || CODEX_DESKTOP_START_STATSIG_FEATURE_KEYS
                .contains(&key.strip_prefix("features.").unwrap_or_default())
    }) || !CODEX_DESKTOP_AMBIENT_SAFETY_FALSE_CONFIG_KEYS
        .iter()
        .all(|key| config.get(*key).is_some_and(|value| value == false))
    {
        return false;
    }
    let fixed_scalars_match = [
        "include_permissions_instructions",
        "include_apps_instructions",
        "include_environment_context",
    ]
    .iter()
    .all(|key| config.get(*key).is_some_and(|value| value == false))
        && config
            .get("project_doc_max_bytes")
            .is_some_and(|value| value == 0)
        && config
            .get("web_search")
            .is_some_and(|value| value == "disabled")
        && config
            .get("model_reasoning_effort")
            .is_some_and(|value| value == "low");
    let apps_are_disabled = config.get("apps").is_some_and(|apps| {
        apps.as_object().is_some_and(|apps| apps.len() == 1)
            && codex_desktop_apps_config_is_restricted(apps)
    });
    let skills_are_disabled = config
        .get("skills")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|skills| {
            skills.len() == 2
                && skills
                    .get("bundled")
                    .and_then(serde_json::Value::as_object)
                    .is_some_and(|bundled| {
                        bundled.len() == 1
                            && bundled.get("enabled").is_some_and(|value| value == false)
                    })
                && skills
                    .get("config")
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(Vec::is_empty)
        });
    let memories_are_disabled = config
        .get("memories")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|memories| {
            memories.len() == 2
                && ["use_memories", "generate_memories"]
                    .iter()
                    .all(|key| memories.get(*key).is_some_and(|value| value == false))
        });
    let mcp_app_is_disabled = config
        .get("mcp_servers.codex_app")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|server| {
            server.len() == 2
                && server.get("enabled").is_some_and(|value| value == false)
                && server.get("command").is_some_and(|value| value == "")
        });
    let statsig_types_are_valid = config.iter().all(|(key, value)| {
        CODEX_DESKTOP_AMBIENT_SAFETY_FALSE_CONFIG_KEYS.contains(&key.as_str())
            || !key.starts_with("features.")
            || start_statsig_value_is_restricted(key, value)
    });
    fixed_scalars_match
        && apps_are_disabled
        && skills_are_disabled
        && memories_are_disabled
        && mcp_app_is_disabled
        && statsig_types_are_valid
}

fn managed_desktop_ambient_safety_start(
    params: &ThreadStartParams,
    app_server_client_name: Option<&str>,
) -> bool {
    codex_desktop_common_start_is_restricted(params, app_server_client_name)
        && params.model.as_deref() == Some(CODEX_DESKTOP_METADATA_MODEL)
        && params.cwd.is_none()
        && params.base_instructions.as_deref() == Some(CODEX_DESKTOP_AMBIENT_SAFETY_INSTRUCTIONS)
        && params.developer_instructions.as_deref() == Some("")
        && matches!(
            params.thread_source.as_ref(),
            Some(codex_app_server_protocol::ThreadSource::Feature(source))
                if source == "ambient_suggestion_safety"
        )
        && params
            .config
            .as_ref()
            .is_some_and(codex_desktop_ambient_safety_config_is_restricted)
}

fn codex_desktop_message_config_is_restricted(config: &HashMap<String, serde_json::Value>) -> bool {
    let fixed_keys = [
        "web_search",
        "mcp_servers.codex_app",
        "model_reasoning_effort",
    ];
    if !config.keys().all(|key| {
        fixed_keys.contains(&key.as_str())
            || CODEX_DESKTOP_MESSAGE_FALSE_CONFIG_KEYS.contains(&key.as_str())
            || CODEX_DESKTOP_START_STATSIG_FEATURE_KEYS
                .contains(&key.strip_prefix("features.").unwrap_or_default())
    }) || !CODEX_DESKTOP_MESSAGE_FALSE_CONFIG_KEYS
        .iter()
        .all(|key| config.get(*key).is_some_and(|value| value == false))
    {
        return false;
    }
    let mcp_app_is_disabled = config
        .get("mcp_servers.codex_app")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|server| {
            server.len() == 2
                && server.get("enabled").is_some_and(|value| value == false)
                && server.get("command").is_some_and(|value| value == "")
        });
    let statsig_types_are_valid = config.iter().all(|(key, value)| {
        CODEX_DESKTOP_MESSAGE_FALSE_CONFIG_KEYS.contains(&key.as_str())
            || !key.starts_with("features.")
            || start_statsig_value_is_restricted(key, value)
    });
    config
        .get("web_search")
        .is_some_and(|value| value == "disabled")
        && config
            .get("model_reasoning_effort")
            .is_some_and(|value| value == "low")
        && mcp_app_is_disabled
        && statsig_types_are_valid
}

fn managed_desktop_message_start(
    params: &ThreadStartParams,
    app_server_client_name: Option<&str>,
) -> bool {
    codex_desktop_common_start_is_restricted(params, app_server_client_name)
        && params.model.as_deref() == Some(CODEX_DESKTOP_METADATA_MODEL)
        && params.base_instructions.is_none()
        && params.developer_instructions.is_none()
        && matches!(
            params.thread_source.as_ref(),
            Some(codex_app_server_protocol::ThreadSource::Feature(source))
                if matches!(
                    source.as_str(),
                    "commit_message" | "pull_request_message" | "commit_pull_request_message"
                )
        )
        && params
            .config
            .as_ref()
            .is_some_and(codex_desktop_message_config_is_restricted)
}

fn codex_desktop_ambient_config_is_restricted(
    config: &HashMap<String, serde_json::Value>,
    model: &str,
) -> bool {
    let expected_effort = match model {
        CODEX_DESKTOP_AMBIENT_MODEL => "medium",
        CODEX_DESKTOP_METADATA_MODEL => "low",
        _ => return false,
    };
    if config
        .get("model_reasoning_effort")
        .is_none_or(|value| value != expected_effort)
    {
        return false;
    }
    let mut saw_codex_app = false;
    for (key, value) in config {
        if key == "model_reasoning_effort" {
            continue;
        }
        if key == "mcp_servers.codex_app" {
            saw_codex_app = value.as_object().is_some_and(|server| {
                server.len() == 2
                    && server.get("enabled").is_some_and(|value| value == false)
                    && server.get("command").is_some_and(|value| value == "")
            });
            if !saw_codex_app {
                return false;
            }
            continue;
        }
        if key.starts_with("features.") {
            if !start_statsig_value_is_restricted(key, value) {
                return false;
            }
            continue;
        }
        if key == "apps" {
            if value.as_object().is_none_or(|apps| apps.len() != 1)
                || !codex_desktop_apps_config_is_restricted(value)
            {
                return false;
            }
            continue;
        }
        if let Some(app_id) = key
            .strip_prefix("apps.")
            .and_then(|key| key.strip_suffix(".enabled"))
        {
            if app_id.is_empty() || value != false {
                return false;
            }
            continue;
        }
        if let Some(plugin_id) = key
            .strip_prefix("plugins.")
            .and_then(|key| key.strip_suffix(".enabled"))
        {
            if plugin_id.is_empty() || value != false {
                return false;
            }
            continue;
        }
        if let Some(server_id) = key.strip_prefix("mcp_servers.") {
            if server_id.is_empty()
                || !value
                    .as_object()
                    .is_some_and(|server| server.get("enabled").is_some_and(|value| value == false))
            {
                return false;
            }
            continue;
        }
        return false;
    }
    saw_codex_app
}

fn managed_desktop_background_start(
    params: &ThreadStartParams,
    app_server_client_name: Option<&str>,
) -> bool {
    codex_desktop_common_start_is_restricted(params, app_server_client_name)
        && params.base_instructions.is_none()
        && params.developer_instructions.is_none()
        && matches!(
            params.thread_source.as_ref(),
            Some(codex_app_server_protocol::ThreadSource::Feature(source))
                if source == "ambient_suggestions"
        )
        && params.config.as_ref().is_some_and(|config| {
            params
                .model
                .as_deref()
                .is_some_and(|model| codex_desktop_ambient_config_is_restricted(config, model))
        })
}

pub(super) fn managed_background_config(
    config: Option<HashMap<String, serde_json::Value>>,
) -> HashMap<String, serde_json::Value> {
    let mut config = config.unwrap_or_default();
    config.extend([
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

pub(super) fn managed_desktop_temporary_structured_fork(
    params: &ThreadForkParams,
    app_server_client_name: Option<&str>,
    lane: Option<ModelPolicyLane>,
) -> bool {
    matches!(lane, Some(ModelPolicyLane::Subscription))
        && app_server_client_name == Some(CODEX_DESKTOP_CLIENT_NAME)
        && params.last_turn_id.is_none()
        && params.before_turn_id.is_none()
        && params.path.is_none()
        && params.model.as_deref() == Some(CODEX_DESKTOP_METADATA_MODEL)
        && params.model_provider.is_none()
        && matches!(params.service_tier, Some(None))
        && params
            .runtime_workspace_roots
            .as_ref()
            .is_some_and(Vec::is_empty)
        && params.approval_policy == Some(AskForApproval::Never)
        && params.approvals_reviewer.is_none()
        && params.sandbox.is_none()
        && params.permissions.as_deref() == Some(":read-only")
        && params.base_instructions.is_none()
        && params.developer_instructions.is_none()
        && params.ephemeral
        && matches!(
            params.thread_source.as_ref(),
            Some(codex_app_server_protocol::ThreadSource::Feature(source))
                if matches!(
                    source.as_str(),
                    "thread_title" | "thread_description" | "thread_title_reconsideration"
                )
        )
        && params.exclude_turns
        && !params.defer_goal_continuation
        && params.config.as_ref().is_some_and(|config| {
            codex_desktop_metadata_config_is_restricted(
                config, /*allow_start_statsig_features*/ false,
            )
        })
}

#[cfg(test)]
#[path = "managed_desktop_requests_tests.rs"]
mod tests;
