use super::*;
use codex_app_server_protocol::ThreadSource;
use pretty_assertions::assert_eq;
use serde_json::json;

fn tui_temporary_params() -> ThreadStartParams {
    let mut config = codex_core::config::MANAGED_TEMPORARY_STRUCTURED_DISABLED_BOOL_OVERRIDES
        .iter()
        .map(|key| ((*key).to_string(), json!(false)))
        .collect::<HashMap<_, _>>();
    config.extend([
        ("web_search".to_string(), json!("disabled")),
        ("mcp_servers".to_string(), json!({})),
        ("model_reasoning_effort".to_string(), json!("ultra")),
        ("plan_mode_reasoning_effort".to_string(), json!("ultra")),
        ("service_tier".to_string(), json!("default")),
    ]);
    ThreadStartParams {
        model: Some("caller-selected-model".to_string()),
        model_provider: Some("caller-selected-provider".to_string()),
        cwd: Some("/tmp".to_string()),
        runtime_workspace_roots: Some(Vec::new()),
        approval_policy: Some(AskForApproval::Never),
        sandbox: Some(SandboxMode::ReadOnly),
        config: Some(config),
        ephemeral: Some(true),
        thread_source: Some(ThreadSource::Feature("system".to_string())),
        environments: Some(Vec::new()),
        dynamic_tools: Some(Vec::new()),
        selected_capability_roots: Some(Vec::new()),
        ..Default::default()
    }
}

#[test]
fn tui_temporary_structured_classifier_requires_exact_restricted_shape() {
    let params = tui_temporary_params();
    assert_eq!(
        managed_temporary_structured_lane_for_policy(
            &params,
            Some(CODEX_TUI_CLIENT_NAME),
            Some(ModelPolicyLane::Spark),
        ),
        Some(ModelPolicyLane::Spark)
    );

    let mut durable = params.clone();
    durable.ephemeral = Some(false);
    assert_eq!(
        managed_temporary_structured_lane_for_policy(
            &durable,
            Some(CODEX_TUI_CLIENT_NAME),
            Some(ModelPolicyLane::Spark),
        ),
        None
    );

    let mut tool_shape_changed = params;
    tool_shape_changed.dynamic_tools = None;
    assert_eq!(
        managed_temporary_structured_lane_for_policy(
            &tool_shape_changed,
            Some(CODEX_TUI_CLIENT_NAME),
            Some(ModelPolicyLane::Spark),
        ),
        None
    );
}
