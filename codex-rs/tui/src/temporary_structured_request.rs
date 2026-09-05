//! Run isolated structured requests through existing app-server methods.

use crate::legacy_core::config::MANAGED_TEMPORARY_STRUCTURED_DISABLED_BOOL_OVERRIDES;
use crate::legacy_core::config::ManagedBackgroundInferenceSettings;
use crate::legacy_core::config::ModelPolicyLane;
use crate::legacy_core::config::locked_model_policy_lane;
use crate::legacy_core::config::managed_background_inference_for_lane;
use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_client::TypedRequestError;
use codex_app_server_protocol::AskForApproval;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::ConfigReadParams;
use codex_app_server_protocol::ConfigReadResponse;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::SandboxMode;
use codex_app_server_protocol::SandboxPolicy;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::ThreadSource;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::ThreadUnsubscribeParams;
use codex_app_server_protocol::ThreadUnsubscribeResponse;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::TurnStatus;
use codex_app_server_protocol::UserInput;
use codex_protocol::openai_models::ReasoningEffort;
use color_eyre::eyre::eyre;
use serde_json::Value;
use std::time::Duration;
use tokio::sync::mpsc::UnboundedReceiver;
use uuid::Uuid;

const STRUCTURED_REQUEST_TIMEOUT: Duration = Duration::from_secs(/*secs*/ 30);
const MANAGED_STRUCTURED_REQUEST_TIMEOUT: Duration = Duration::from_secs(/*secs*/ 120);
const STRUCTURED_RESPONSE_MAX_BYTES: usize = 8 * 1024;

/// Preserve the visible thread's provider, permissions, and external-tool isolation.
pub(crate) struct TemporaryStructuredThreadOptions {
    pub(crate) model: String,
    pub(crate) model_provider: String,
    pub(crate) cwd: String,
    pub(crate) active_permission_profile: Option<String>,
    pub(crate) mcp_server_names: Vec<String>,
}

fn temporary_inference_settings_for_lane(
    model: String,
    effort: Option<ReasoningEffort>,
    lane: Option<ModelPolicyLane>,
) -> ManagedBackgroundInferenceSettings {
    managed_background_inference_for_lane(model, effort, None, lane)
}

fn structured_request_timeout_for_lane(lane: Option<ModelPolicyLane>) -> Duration {
    if lane.is_some() {
        MANAGED_STRUCTURED_REQUEST_TIMEOUT
    } else {
        STRUCTURED_REQUEST_TIMEOUT
    }
}

/// Start an ephemeral thread without widening permissions or exposing tools and environment access.
///
/// Structured prompts can contain untrusted transcript text, so the effective app-server config is
/// read first and every MCP server is explicitly disabled alongside built-in and extension tools.
pub(crate) async fn start_temporary_thread(
    request_handle: &AppServerRequestHandle,
    options: TemporaryStructuredThreadOptions,
) -> color_eyre::Result<ThreadStartResponse> {
    let TemporaryStructuredThreadOptions {
        model,
        model_provider,
        cwd,
        active_permission_profile,
        mcp_server_names,
    } = options;
    let lane = locked_model_policy_lane()?;
    let request_timeout = structured_request_timeout_for_lane(lane);
    let settings = temporary_inference_settings_for_lane(model, /*effort*/ None, lane);
    let model = settings.model;
    let custom_permission_profile = lane
        .is_none()
        .then_some(active_permission_profile)
        .flatten()
        .filter(|profile| !profile.starts_with(':'));
    let mut config = MANAGED_TEMPORARY_STRUCTURED_DISABLED_BOOL_OVERRIDES
        .iter()
        .map(|key| ((*key).to_string(), false.into()))
        .collect::<std::collections::HashMap<_, _>>();
    config.insert("web_search".to_string(), "disabled".into());
    if let Some(reasoning_effort) = settings.reasoning_effort {
        config.insert(
            "model_reasoning_effort".to_string(),
            serde_json::to_value(reasoning_effort.clone())?,
        );
        config.insert(
            "plan_mode_reasoning_effort".to_string(),
            serde_json::to_value(reasoning_effort)?,
        );
    }
    if let Some(service_tier) = settings.service_tier {
        config.insert("service_tier".to_string(), service_tier.into());
    }
    let response: ThreadStartResponse = tokio::time::timeout(request_timeout, async {
        // Fail closed if the remote-effective MCP configuration cannot be read.
        let effective_config: ConfigReadResponse = request_handle
            .request_typed(ClientRequest::ConfigRead {
                request_id: RequestId::String(format!("temporary-config-{}", Uuid::new_v4())),
                params: ConfigReadParams {
                    include_layers: false,
                    cwd: Some(cwd.clone()),
                },
            })
            .await?;
        let mut mcp_server_names = mcp_server_names
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        if let Some(effective_mcp_servers) = effective_config
            .config
            .additional
            .get("mcp_servers")
            .and_then(Value::as_object)
        {
            mcp_server_names.extend(effective_mcp_servers.keys().cloned());
        }
        config.insert(
            "mcp_servers".to_string(),
            Value::Object(
                mcp_server_names
                    .into_iter()
                    .map(|name| (name, serde_json::json!({ "enabled": false })))
                    .collect(),
            ),
        );

        request_handle
            .request_typed(ClientRequest::ThreadStart {
                request_id: RequestId::String(format!("temporary-structured-{}", Uuid::new_v4())),
                params: ThreadStartParams {
                    model: Some(model),
                    model_provider: Some(model_provider),
                    cwd: Some(cwd),
                    approval_policy: Some(AskForApproval::Never),
                    sandbox: custom_permission_profile
                        .is_none()
                        .then_some(SandboxMode::ReadOnly),
                    permissions: custom_permission_profile.clone(),
                    runtime_workspace_roots: Some(Vec::new()),
                    ephemeral: Some(true),
                    thread_source: Some(ThreadSource::Feature("system".to_string())),
                    environments: Some(Vec::new()),
                    dynamic_tools: Some(Vec::new()),
                    selected_capability_roots: Some(Vec::new()),
                    config: Some(config),
                    ..ThreadStartParams::default()
                },
            })
            .await
    })
    .await
    .map_err(|_| eyre!("temporary structured thread start timed out"))??;

    if let Some(expected_profile) = custom_permission_profile {
        if response
            .active_permission_profile
            .as_ref()
            .is_none_or(|profile| profile.id != expected_profile)
        {
            return Err(eyre!(
                "temporary structured thread did not preserve permission profile {expected_profile}"
            ));
        }
    } else if !matches!(response.sandbox, SandboxPolicy::ReadOnly { .. }) {
        return Err(eyre!(
            "temporary structured thread did not start with read-only permissions"
        ));
    }

    Ok(response)
}

/// Submit a structured turn using the temporary thread's existing settings.
pub(crate) async fn start_structured_turn(
    request_handle: &AppServerRequestHandle,
    thread_id: String,
    prompt: String,
    output_schema: Value,
    effort: Option<ReasoningEffort>,
) -> Result<TurnStartResponse, TypedRequestError> {
    request_handle
        .request_typed(ClientRequest::TurnStart {
            request_id: RequestId::String(format!("temporary-structured-turn-{}", Uuid::new_v4())),
            params: TurnStartParams {
                thread_id,
                input: vec![UserInput::Text {
                    text: prompt,
                    text_elements: Vec::new(),
                }],
                output_schema: Some(output_schema),
                effort,
                ..TurnStartParams::default()
            },
        })
        .await
}

/// Return the latest assistant message when the requested turn completes.
pub(crate) async fn collect_structured_response(
    mut notifications: UnboundedReceiver<ServerNotification>,
    turn_id: &str,
) -> color_eyre::Result<String> {
    let mut response = None;

    while let Some(notification) = notifications.recv().await {
        match notification {
            ServerNotification::ItemCompleted(completed) if completed.turn_id == turn_id => {
                if let ThreadItem::AgentMessage { text, .. } = completed.item {
                    if text.len() > STRUCTURED_RESPONSE_MAX_BYTES {
                        return Err(eyre!(
                            "temporary structured response exceeds {STRUCTURED_RESPONSE_MAX_BYTES} bytes"
                        ));
                    }
                    response = Some(text);
                }
            }
            ServerNotification::TurnCompleted(completed) if completed.turn.id == turn_id => {
                if completed.turn.status != TurnStatus::Completed {
                    return Err(eyre!(
                        "temporary structured turn ended with status {:?}",
                        completed.turn.status,
                    ));
                }

                return response.ok_or_else(|| {
                    eyre!("temporary structured turn completed without a response")
                });
            }
            _ => {}
        }
    }

    Err(eyre!(
        "temporary structured turn notification channel closed"
    ))
}

/// Make a bounded best-effort attempt to detach an ephemeral thread.
pub(crate) async fn unsubscribe_temporary_thread(
    request_handle: &AppServerRequestHandle,
    thread_id: String,
) {
    match tokio::time::timeout(
        STRUCTURED_REQUEST_TIMEOUT,
        request_handle.request_typed::<ThreadUnsubscribeResponse>(
            ClientRequest::ThreadUnsubscribe {
                request_id: RequestId::String(format!(
                    "temporary-structured-unsubscribe-{}",
                    Uuid::new_v4()
                )),
                params: ThreadUnsubscribeParams { thread_id },
            },
        ),
    )
    .await
    {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => {
            tracing::debug!(%error, "failed to unsubscribe from temporary structured thread");
        }
        Err(_) => {
            tracing::debug!("temporary structured thread unsubscribe timed out");
        }
    }
}

/// Run a bounded structured turn and make a bounded temporary-thread cleanup attempt.
pub(crate) async fn run_temporary_structured_turn(
    request_handle: AppServerRequestHandle,
    thread_id: String,
    prompt: String,
    output_schema: Value,
    effort: Option<ReasoningEffort>,
    notifications: UnboundedReceiver<ServerNotification>,
) -> color_eyre::Result<String> {
    let lane = locked_model_policy_lane()?;
    let request_timeout = structured_request_timeout_for_lane(lane);
    let effort =
        temporary_inference_settings_for_lane(String::new(), effort, lane).reasoning_effort;
    let result = tokio::time::timeout(request_timeout, async {
        let turn = start_structured_turn(
            &request_handle,
            thread_id.clone(),
            prompt,
            output_schema,
            effort,
        )
        .await?;

        collect_structured_response(notifications, &turn.turn.id).await
    })
    .await
    .unwrap_or_else(|_| Err(eyre!("temporary structured turn timed out")));

    unsubscribe_temporary_thread(&request_handle, thread_id).await;

    result
}

#[cfg(test)]
#[path = "temporary_structured_request_tests.rs"]
mod tests;
