use super::*;
use codex_app_server_protocol::ThreadSource;
use pretty_assertions::assert_eq;
use serde_json::json;

fn desktop_apps_disabled() -> serde_json::Value {
    json!({
        "_default": {
            "enabled": false,
            "destructive_enabled": false,
            "open_world_enabled": false,
        }
    })
}

fn desktop_metadata_config() -> HashMap<String, serde_json::Value> {
    let mut config = CODEX_DESKTOP_METADATA_FALSE_CONFIG_KEYS
        .iter()
        .map(|key| ((*key).to_string(), json!(false)))
        .collect::<HashMap<_, _>>();
    config.extend([
        ("model_reasoning_effort".to_string(), json!("low")),
        (
            "mcp_servers.codex_app".to_string(),
            json!({ "enabled": false, "command": "" }),
        ),
        ("features.apps".to_string(), json!(false)),
        ("apps".to_string(), desktop_apps_disabled()),
        ("web_search".to_string(), json!("disabled")),
    ]);
    config
}

fn desktop_metadata_start() -> ThreadStartParams {
    let mut config = desktop_metadata_config();
    config.insert("features.remote_models".to_string(), json!(true));
    ThreadStartParams {
        model: Some(CODEX_DESKTOP_METADATA_MODEL.to_string()),
        allow_provider_model_fallback: true,
        service_tier: Some(None),
        cwd: Some("/tmp".to_string()),
        runtime_workspace_roots: Some(Vec::new()),
        approval_policy: Some(AskForApproval::Never),
        permissions: Some(":read-only".to_string()),
        config: Some(config),
        service_name: Some(CODEX_DESKTOP_SERVICE_NAME.to_string()),
        ephemeral: Some(true),
        thread_source: Some(ThreadSource::Feature("thread_summary".to_string())),
        ..Default::default()
    }
}

fn desktop_metadata_fork() -> ThreadForkParams {
    ThreadForkParams {
        thread_id: "source-thread".to_string(),
        last_turn_id: None,
        before_turn_id: None,
        path: None,
        model: Some(CODEX_DESKTOP_METADATA_MODEL.to_string()),
        model_provider: None,
        service_tier: Some(None),
        cwd: Some("/tmp".to_string()),
        runtime_workspace_roots: Some(Vec::new()),
        approval_policy: Some(AskForApproval::Never),
        approvals_reviewer: None,
        sandbox: None,
        permissions: Some(":read-only".to_string()),
        config: Some(desktop_metadata_config()),
        base_instructions: None,
        developer_instructions: None,
        ephemeral: true,
        thread_source: Some(ThreadSource::Feature("thread_description".to_string())),
        exclude_turns: true,
        defer_goal_continuation: false,
    }
}

fn desktop_ambient_safety_config() -> HashMap<String, serde_json::Value> {
    let mut config = CODEX_DESKTOP_AMBIENT_SAFETY_FALSE_CONFIG_KEYS
        .iter()
        .map(|key| ((*key).to_string(), json!(false)))
        .collect::<HashMap<_, _>>();
    config.extend([
        ("include_permissions_instructions".to_string(), json!(false)),
        ("include_apps_instructions".to_string(), json!(false)),
        ("include_environment_context".to_string(), json!(false)),
        ("project_doc_max_bytes".to_string(), json!(0)),
        ("apps".to_string(), desktop_apps_disabled()),
        (
            "skills".to_string(),
            json!({ "bundled": { "enabled": false }, "config": [] }),
        ),
        (
            "memories".to_string(),
            json!({ "use_memories": false, "generate_memories": false }),
        ),
        ("web_search".to_string(), json!("disabled")),
        (
            "mcp_servers.codex_app".to_string(),
            json!({ "enabled": false, "command": "" }),
        ),
        ("model_reasoning_effort".to_string(), json!("low")),
        ("features.remote_models".to_string(), json!(true)),
    ]);
    config
}

fn desktop_ambient_safety_start() -> ThreadStartParams {
    ThreadStartParams {
        model: Some(CODEX_DESKTOP_METADATA_MODEL.to_string()),
        allow_provider_model_fallback: true,
        service_tier: Some(None),
        runtime_workspace_roots: Some(Vec::new()),
        approval_policy: Some(AskForApproval::Never),
        permissions: Some(":read-only".to_string()),
        config: Some(desktop_ambient_safety_config()),
        service_name: Some(CODEX_DESKTOP_SERVICE_NAME.to_string()),
        base_instructions: Some(CODEX_DESKTOP_AMBIENT_SAFETY_INSTRUCTIONS.to_string()),
        developer_instructions: Some(String::new()),
        ephemeral: Some(true),
        thread_source: Some(ThreadSource::Feature(
            "ambient_suggestion_safety".to_string(),
        )),
        ..Default::default()
    }
}

fn desktop_message_config() -> HashMap<String, serde_json::Value> {
    let mut config = CODEX_DESKTOP_MESSAGE_FALSE_CONFIG_KEYS
        .iter()
        .map(|key| ((*key).to_string(), json!(false)))
        .collect::<HashMap<_, _>>();
    config.extend([
        ("web_search".to_string(), json!("disabled")),
        (
            "mcp_servers.codex_app".to_string(),
            json!({ "enabled": false, "command": "" }),
        ),
        ("model_reasoning_effort".to_string(), json!("low")),
        ("features.remote_models".to_string(), json!(true)),
    ]);
    config
}

fn desktop_message_start() -> ThreadStartParams {
    ThreadStartParams {
        model: Some(CODEX_DESKTOP_METADATA_MODEL.to_string()),
        allow_provider_model_fallback: true,
        service_tier: Some(None),
        cwd: Some("/tmp".to_string()),
        runtime_workspace_roots: Some(Vec::new()),
        approval_policy: Some(AskForApproval::Never),
        permissions: Some(":read-only".to_string()),
        config: Some(desktop_message_config()),
        service_name: Some(CODEX_DESKTOP_SERVICE_NAME.to_string()),
        ephemeral: Some(true),
        thread_source: Some(ThreadSource::Feature("commit_message".to_string())),
        ..Default::default()
    }
}

fn desktop_ambient_start() -> ThreadStartParams {
    let config = HashMap::from([
        ("model_reasoning_effort".to_string(), json!("medium")),
        (
            "mcp_servers.codex_app".to_string(),
            json!({ "enabled": false, "command": "" }),
        ),
        (
            "mcp_servers.untrusted".to_string(),
            json!({ "enabled": false, "command": "ignored" }),
        ),
        ("plugins.example.enabled".to_string(), json!(false)),
        ("apps.example.enabled".to_string(), json!(false)),
        ("features.remote_models".to_string(), json!(true)),
    ]);
    ThreadStartParams {
        model: Some(CODEX_DESKTOP_AMBIENT_MODEL.to_string()),
        allow_provider_model_fallback: true,
        service_tier: Some(None),
        cwd: Some("/tmp".to_string()),
        runtime_workspace_roots: Some(Vec::new()),
        approval_policy: Some(AskForApproval::Never),
        permissions: Some(":read-only".to_string()),
        config: Some(config),
        service_name: Some(CODEX_DESKTOP_SERVICE_NAME.to_string()),
        ephemeral: Some(true),
        thread_source: Some(ThreadSource::Feature("ambient_suggestions".to_string())),
        ..Default::default()
    }
}

#[test]
fn desktop_metadata_start_and_fork_require_exact_signed_shapes() {
    let start = desktop_metadata_start();
    assert_eq!(
        managed_desktop_temporary_structured_lane_for_policy(
            &start,
            Some(CODEX_DESKTOP_CLIENT_NAME),
            Some(ModelPolicyLane::Subscription),
        ),
        Some(ModelPolicyLane::Subscription)
    );
    let mut missing_service_name = start.clone();
    missing_service_name.service_name = None;
    assert_eq!(
        managed_desktop_temporary_structured_lane_for_policy(
            &missing_service_name,
            Some(CODEX_DESKTOP_CLIENT_NAME),
            Some(ModelPolicyLane::Subscription),
        ),
        None
    );
    let mut priority = start;
    priority.service_tier = Some(Some("priority".to_string()));
    assert_eq!(
        managed_desktop_temporary_structured_lane_for_policy(
            &priority,
            Some(CODEX_DESKTOP_CLIENT_NAME),
            Some(ModelPolicyLane::Subscription),
        ),
        None
    );

    let fork = desktop_metadata_fork();
    assert!(managed_desktop_temporary_structured_fork(
        &fork,
        Some(CODEX_DESKTOP_CLIENT_NAME),
        Some(ModelPolicyLane::Subscription),
    ));
    let mut includes_turns = fork.clone();
    includes_turns.exclude_turns = false;
    assert!(!managed_desktop_temporary_structured_fork(
        &includes_turns,
        Some(CODEX_DESKTOP_CLIENT_NAME),
        Some(ModelPolicyLane::Subscription),
    ));
    for source in [
        "thread_title",
        "thread_description",
        "thread_title_reconsideration",
    ] {
        let mut signed_source = fork.clone();
        signed_source.thread_source = Some(ThreadSource::Feature(source.to_string()));
        assert!(managed_desktop_temporary_structured_fork(
            &signed_source,
            Some(CODEX_DESKTOP_CLIENT_NAME),
            Some(ModelPolicyLane::Subscription),
        ));
    }
    let mut wrong_source = fork.clone();
    wrong_source.thread_source = Some(ThreadSource::Feature("thread_summary".to_string()));
    assert!(!managed_desktop_temporary_structured_fork(
        &wrong_source,
        Some(CODEX_DESKTOP_CLIENT_NAME),
        Some(ModelPolicyLane::Subscription),
    ));
    let mut fork_with_statsig = fork;
    fork_with_statsig
        .config
        .as_mut()
        .expect("fork config")
        .insert("features.remote_models".to_string(), json!(true));
    assert!(!managed_desktop_temporary_structured_fork(
        &fork_with_statsig,
        Some(CODEX_DESKTOP_CLIENT_NAME),
        Some(ModelPolicyLane::Subscription),
    ));
}

#[test]
fn desktop_ambient_safety_requires_exact_signed_shape() {
    let safety = desktop_ambient_safety_start();
    assert_eq!(
        managed_desktop_temporary_structured_lane_for_policy(
            &safety,
            Some(CODEX_DESKTOP_CLIENT_NAME),
            Some(ModelPolicyLane::Subscription),
        ),
        Some(ModelPolicyLane::Subscription)
    );
    let mut prompt_changed = safety.clone();
    prompt_changed.base_instructions = Some("different prompt".to_string());
    assert_eq!(
        managed_desktop_temporary_structured_lane_for_policy(
            &prompt_changed,
            Some(CODEX_DESKTOP_CLIENT_NAME),
            Some(ModelPolicyLane::Subscription),
        ),
        None
    );
    let mut feature_enabled = safety;
    feature_enabled
        .config
        .as_mut()
        .expect("safety config")
        .insert("features.unified_exec".to_string(), json!(true));
    assert_eq!(
        managed_desktop_temporary_structured_lane_for_policy(
            &feature_enabled,
            Some(CODEX_DESKTOP_CLIENT_NAME),
            Some(ModelPolicyLane::Subscription),
        ),
        None
    );
}

#[test]
fn desktop_commit_and_pull_request_messages_require_exact_signed_shape() {
    for source in [
        "commit_message",
        "pull_request_message",
        "commit_pull_request_message",
    ] {
        let mut params = desktop_message_start();
        params.thread_source = Some(ThreadSource::Feature(source.to_string()));
        assert_eq!(
            managed_desktop_temporary_structured_lane_for_policy(
                &params,
                Some(CODEX_DESKTOP_CLIENT_NAME),
                Some(ModelPolicyLane::Subscription),
            ),
            Some(ModelPolicyLane::Subscription)
        );
    }
    let mut unknown_config = desktop_message_start();
    unknown_config
        .config
        .as_mut()
        .expect("message config")
        .insert("features.unknown".to_string(), json!(false));
    assert_eq!(
        managed_desktop_temporary_structured_lane_for_policy(
            &unknown_config,
            Some(CODEX_DESKTOP_CLIENT_NAME),
            Some(ModelPolicyLane::Subscription),
        ),
        None
    );
}

#[test]
fn desktop_ambient_background_preserves_only_bounded_disable_config() {
    let ambient = desktop_ambient_start();
    assert_eq!(
        managed_desktop_background_lane_for_policy(
            &ambient,
            Some(CODEX_DESKTOP_CLIENT_NAME),
            Some(ModelPolicyLane::Subscription),
        ),
        Some(ModelPolicyLane::Subscription)
    );
    assert_eq!(
        managed_desktop_background_lane_for_policy(
            &ambient,
            Some(CODEX_DESKTOP_CLIENT_NAME),
            Some(ModelPolicyLane::Api),
        ),
        None
    );

    let mut first_plugin_connect = ambient.clone();
    first_plugin_connect.model = Some(CODEX_DESKTOP_METADATA_MODEL.to_string());
    first_plugin_connect
        .config
        .as_mut()
        .expect("ambient config")
        .insert("model_reasoning_effort".to_string(), json!("low"));
    assert!(managed_desktop_background_start(
        &first_plugin_connect,
        Some(CODEX_DESKTOP_CLIENT_NAME),
    ));
    let mut first_plugin_wrong_effort = first_plugin_connect;
    first_plugin_wrong_effort
        .config
        .as_mut()
        .expect("ambient config")
        .insert("model_reasoning_effort".to_string(), json!("medium"));
    assert!(!managed_desktop_background_start(
        &first_plugin_wrong_effort,
        Some(CODEX_DESKTOP_CLIENT_NAME),
    ));
    let mut mismatched_effort = ambient.clone();
    mismatched_effort
        .config
        .as_mut()
        .expect("ambient config")
        .insert("model_reasoning_effort".to_string(), json!("low"));
    assert!(!managed_desktop_background_start(
        &mismatched_effort,
        Some(CODEX_DESKTOP_CLIENT_NAME),
    ));
    let mut enabled_plugin = ambient.clone();
    enabled_plugin
        .config
        .as_mut()
        .expect("ambient config")
        .insert("plugins.example.enabled".to_string(), json!(true));
    assert!(!managed_desktop_background_start(
        &enabled_plugin,
        Some(CODEX_DESKTOP_CLIENT_NAME),
    ));

    let normalized = managed_background_config(ambient.config);
    assert_eq!(
        normalized.get("plugins.example.enabled"),
        Some(&json!(false))
    );
    assert_eq!(
        (
            normalized.get("model_reasoning_effort"),
            normalized.get("plan_mode_reasoning_effort"),
            normalized.get("service_tier"),
        ),
        (
            Some(&json!("ultra")),
            Some(&json!("ultra")),
            Some(&json!("default")),
        )
    );
}
