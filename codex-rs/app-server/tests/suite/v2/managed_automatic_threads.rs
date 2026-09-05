use super::managed_automatic_test_support::RejectingHttpsProxy;
use anyhow::Context;
use anyhow::Result;
use app_test_support::ChatGptAuthFixture;
use app_test_support::TestAppServer;
use app_test_support::create_fake_rollout;
use app_test_support::write_chatgpt_auth;
use app_test_support::write_models_cache;
use codex_app_server_protocol::ActivePermissionProfile;
use codex_app_server_protocol::AskForApproval;
use codex_app_server_protocol::ClientInfo;
use codex_app_server_protocol::JSONRPCError;
use codex_app_server_protocol::ListMcpServerStatusParams;
use codex_app_server_protocol::ListMcpServerStatusResponse;
use codex_app_server_protocol::McpServerStatusDetail;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::SandboxPolicy;
use codex_app_server_protocol::SessionSource;
use codex_app_server_protocol::ThreadForkParams;
use codex_app_server_protocol::ThreadForkResponse;
use codex_app_server_protocol::ThreadSettingsUpdateParams;
use codex_app_server_protocol::ThreadSource;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_config::types::AuthCredentialsStoreMode;
use codex_core::config::MODEL_POLICY_LANE_ENV;
use codex_protocol::openai_models::ReasoningEffort;
use core_test_support::stdio_server_bin;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::collections::HashMap;
use std::path::Path;
use tempfile::TempDir;
use tokio::time::timeout;

pub(super) const DEFAULT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const ASTRA_MODEL: &str = "gpt-6-astra";
const SOL_MODEL: &str = "gpt-5.6-sol";
pub(super) const LUNA_MODEL: &str = "gpt-5.6-luna";
const TERRA_MODEL: &str = "gpt-5.6-terra";
const STANDARD_SERVICE_TIER: &str = "default";
const DESKTOP_CLIENT_NAME: &str = "Codex Desktop";
const DESKTOP_SERVICE_NAME: &str = "codex_desktop";
const READ_ONLY_MCP_NAME: &str = "read-only-probe";
const AMBIENT_SAFETY_INSTRUCTIONS: &str = "Classify Codex ambient suggestion candidates for policy safety. Return only JSON matching the schema.";

// These fixtures populate the decoded request states captured from the signed Microsoft Store
// package OpenAI.Codex 26.901.4073.0. The extracted renderer carrying these request builders has
// SHA-256 8d056524ea3f5714e5e0a254afd88f018d58465bc6ea026adee3c80ff7799e29.
// They intentionally do not reuse the production classifier's private constants or constructors,
// so a classifier drift can make these tests red. Fields whose signed wire value decodes to a Rust
// default are represented by `ThreadStartParams::default()`.
pub(super) fn desktop_common_start(
    source: &str,
    model: &str,
    config: HashMap<String, Value>,
    cwd: Option<&Path>,
) -> ThreadStartParams {
    ThreadStartParams {
        model: Some(model.to_string()),
        model_provider: None,
        allow_provider_model_fallback: true,
        service_tier: Some(None),
        cwd: cwd.map(|path| path.display().to_string()),
        runtime_workspace_roots: Some(Vec::new()),
        approval_policy: Some(AskForApproval::Never),
        permissions: Some(":read-only".to_string()),
        config: Some(config),
        service_name: Some(DESKTOP_SERVICE_NAME.to_string()),
        ephemeral: Some(true),
        thread_source: Some(ThreadSource::Feature(source.to_string())),
        ..Default::default()
    }
}

fn disabled_codex_app() -> Value {
    json!({
        "enabled": false,
        "command": "",
    })
}

pub(super) fn metadata_config(allow_start_statsig_feature: bool) -> HashMap<String, Value> {
    let mut config = HashMap::from([
        ("features.enable_fanout".to_string(), json!(false)),
        ("features.hooks".to_string(), json!(false)),
        ("features.multi_agent".to_string(), json!(false)),
        ("features.multi_agent_v2".to_string(), json!(false)),
        ("features.plugins".to_string(), json!(false)),
        ("features.shell_snapshot".to_string(), json!(false)),
        ("features.tool_suggest".to_string(), json!(false)),
        ("features.apps".to_string(), json!(true)),
        (
            "apps".to_string(),
            json!({
                "_default": {
                    "enabled": false,
                    "destructive_enabled": false,
                    "open_world_enabled": false,
                },
                "calendar": {
                    "enabled": true,
                    "destructive_enabled": false,
                    "open_world_enabled": false,
                    "default_tools_enabled": true,
                    "tools": {
                        "find_events": { "enabled": true },
                    },
                },
            }),
        ),
        ("web_search".to_string(), json!("disabled")),
        ("mcp_servers.codex_app".to_string(), disabled_codex_app()),
        ("model_reasoning_effort".to_string(), json!("low")),
    ]);
    if allow_start_statsig_feature {
        config.insert("features.remote_models".to_string(), json!(true));
    }
    config
}

fn message_config(include_start_statsig_feature: bool) -> HashMap<String, Value> {
    let mut config = HashMap::from([
        ("features.enable_fanout".to_string(), json!(false)),
        ("features.multi_agent".to_string(), json!(false)),
        ("features.multi_agent_v2".to_string(), json!(false)),
        ("web_search".to_string(), json!("disabled")),
        ("mcp_servers.codex_app".to_string(), disabled_codex_app()),
        ("model_reasoning_effort".to_string(), json!("low")),
    ]);
    if include_start_statsig_feature {
        config.insert("features.remote_models".to_string(), json!(true));
    }
    config
}

fn ambient_safety_config(include_start_statsig_feature: bool) -> HashMap<String, Value> {
    let mut config = [
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
    ]
    .into_iter()
    .map(|key| (key.to_string(), json!(false)))
    .collect::<HashMap<_, _>>();
    config.extend([
        ("include_permissions_instructions".to_string(), json!(false)),
        ("include_apps_instructions".to_string(), json!(false)),
        ("include_environment_context".to_string(), json!(false)),
        ("project_doc_max_bytes".to_string(), json!(0)),
        (
            "apps".to_string(),
            json!({
                "_default": {
                    "enabled": false,
                    "destructive_enabled": false,
                    "open_world_enabled": false,
                },
            }),
        ),
        (
            "skills".to_string(),
            json!({
                "bundled": { "enabled": false },
                "config": [],
            }),
        ),
        (
            "memories".to_string(),
            json!({
                "use_memories": false,
                "generate_memories": false,
            }),
        ),
        ("web_search".to_string(), json!("disabled")),
        ("mcp_servers.codex_app".to_string(), disabled_codex_app()),
        ("model_reasoning_effort".to_string(), json!("low")),
    ]);
    if include_start_statsig_feature {
        config.insert("features.remote_models".to_string(), json!(true));
    }
    config
}

fn ambient_config(effort: &str, include_start_statsig_feature: bool) -> HashMap<String, Value> {
    let mut config = HashMap::from([
        ("mcp_servers.codex_app".to_string(), disabled_codex_app()),
        ("model_reasoning_effort".to_string(), json!(effort)),
    ]);
    if include_start_statsig_feature {
        config.insert("features.remote_models".to_string(), json!(true));
    }
    config
}

fn desktop_metadata_fork(
    source_thread_id: &str,
    cwd: Option<&Path>,
    source: &str,
) -> ThreadForkParams {
    ThreadForkParams {
        thread_id: source_thread_id.to_string(),
        path: None,
        model: Some(LUNA_MODEL.to_string()),
        model_provider: None,
        service_tier: Some(None),
        cwd: cwd.map(|path| path.display().to_string()),
        runtime_workspace_roots: Some(Vec::new()),
        approval_policy: Some(AskForApproval::Never),
        permissions: Some(":read-only".to_string()),
        config: Some(metadata_config(/*allow_start_statsig_feature*/ false)),
        ephemeral: true,
        thread_source: Some(ThreadSource::Feature(source.to_string())),
        // The signed Desktop helper explicitly asks for the metadata-only cheap fork path.
        exclude_turns: true,
        ..Default::default()
    }
}

fn write_managed_subscription_home(codex_home: &Path) -> Result<()> {
    let stdio_server = toml::Value::String(stdio_server_bin()?);
    std::fs::write(
        codex_home.join("config.toml"),
        format!(
            r#"model = "{ASTRA_MODEL}"
review_model = "{ASTRA_MODEL}"
model_provider = "openai"
chatgpt_base_url = "https://chatgpt.com/backend-api"
forced_login_method = "chatgpt"
model_reasoning_effort = "ultra"
plan_mode_reasoning_effort = "ultra"
approval_policy = "never"
approvals_reviewer = "user"
sandbox_mode = "danger-full-access"
check_for_update_on_startup = false

[agents]
enabled = true

[features]
fast_mode = true
multi_agent = true
memories = false

[features.multi_agent_v2]
enabled = true
expose_spawn_agent_model_overrides = false

[mcp_servers.{READ_ONLY_MCP_NAME}]
command = {stdio_server}
startup_timeout_sec = 5
"#,
        ),
    )?;
    write_chatgpt_auth(
        codex_home,
        ChatGptAuthFixture::new("chatgpt-token")
            .plan_type("plus")
            .chatgpt_account_id("account-123")
            .account_id("account-123"),
        AuthCredentialsStoreMode::File,
    )?;
    write_models_cache(codex_home)?;
    Ok(())
}

async fn managed_desktop_server(codex_home: &Path) -> Result<(TestAppServer, RejectingHttpsProxy)> {
    let proxy = RejectingHttpsProxy::start().await?;
    let user_config_home = codex_home.to_string_lossy().into_owned();
    let mut server = TestAppServer::builder()
        .with_codex_home(codex_home)
        .without_auto_env()
        .with_env_overrides(&[
            (MODEL_POLICY_LANE_ENV, Some("subscription")),
            ("CDX_USER_CONFIG_HOMES", Some(user_config_home.as_str())),
            ("OPENAI_API_KEY", None),
            ("CODEX_API_KEY", None),
            ("HTTPS_PROXY", Some(proxy.uri())),
            ("https_proxy", Some(proxy.uri())),
            ("NO_PROXY", Some("")),
            ("no_proxy", Some("")),
        ])
        .build()
        .await?;
    timeout(
        DEFAULT_READ_TIMEOUT,
        server.initialize_with_client_info(ClientInfo {
            name: DESKTOP_CLIENT_NAME.to_string(),
            title: None,
            version: "26.901.4073.0".to_string(),
        }),
    )
    .await??;
    Ok((server, proxy))
}

pub(super) fn assert_managed_start(
    response: &ThreadStartResponse,
    expected_source: &str,
    expected_active_permission_profile: Option<ActivePermissionProfile>,
) {
    assert_eq!(
        (
            response.model.as_str(),
            response.model_provider.as_str(),
            response.reasoning_effort.clone(),
            response.service_tier.as_deref(),
            response.approval_policy,
            response.sandbox.clone(),
            response.active_permission_profile.clone(),
            response.runtime_workspace_roots.as_slice(),
        ),
        (
            ASTRA_MODEL,
            "openai",
            Some(ReasoningEffort::Ultra),
            Some(STANDARD_SERVICE_TIER),
            AskForApproval::Never,
            SandboxPolicy::ReadOnly {
                network_access: false,
            },
            expected_active_permission_profile,
            &[][..],
        ),
    );
    assert_eq!(
        (
            response.thread.model.as_deref(),
            response.thread.reasoning_effort.clone(),
            response.thread.source.clone(),
            response.thread.thread_source.clone(),
            response.thread.ephemeral,
        ),
        (
            Some(ASTRA_MODEL),
            Some(ReasoningEffort::Ultra),
            SessionSource::Unknown,
            Some(ThreadSource::Feature(expected_source.to_string())),
            true,
        ),
    );
}

async fn mcp_status(
    server: &mut TestAppServer,
    thread_id: String,
) -> Result<ListMcpServerStatusResponse> {
    let request_id = server
        .send_list_mcp_server_status_request(ListMcpServerStatusParams {
            cursor: None,
            limit: None,
            detail: Some(McpServerStatusDetail::ToolsAndAuthOnly),
            thread_id: Some(thread_id),
        })
        .await?;
    timeout(DEFAULT_READ_TIMEOUT, server.read_response(request_id)).await?
}

async fn assert_hidden_start_is_not_classified(
    server: &mut TestAppServer,
    params: ThreadStartParams,
    discriminator: &str,
) -> Result<()> {
    let request_id = server.send_thread_start_request(params).await?;
    let error: JSONRPCError = timeout(
        DEFAULT_READ_TIMEOUT,
        server.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;
    assert!(
        error
            .error
            .message
            .contains("features.multi_agent=false; required true"),
        "the {discriminator} near miss must not acquire the automatic background contract",
    );
    Ok(())
}

async fn assert_background_rejects_root_choices(
    server: &mut TestAppServer,
    thread_id: &str,
) -> Result<()> {
    let model_update_id = server
        .send_thread_settings_update_request(ThreadSettingsUpdateParams {
            thread_id: thread_id.to_string(),
            model: Some(SOL_MODEL.to_string()),
            ..Default::default()
        })
        .await?;
    let model_error: JSONRPCError = timeout(
        DEFAULT_READ_TIMEOUT,
        server.read_stream_until_error_message(RequestId::Integer(model_update_id)),
    )
    .await??;
    assert!(
        model_error.error.message.contains(
            "subscription model policy rejected model `gpt-5.6-sol`; required `gpt-6-astra`",
        ),
        "unexpected model override error: {}",
        model_error.error.message,
    );

    let effort_update_id = server
        .send_thread_settings_update_request(ThreadSettingsUpdateParams {
            thread_id: thread_id.to_string(),
            effort: Some(ReasoningEffort::High),
            ..Default::default()
        })
        .await?;
    let effort_error: JSONRPCError = timeout(
        DEFAULT_READ_TIMEOUT,
        server.read_stream_until_error_message(RequestId::Integer(effort_update_id)),
    )
    .await??;
    assert!(
        effort_error.error.message.contains(
            "subscription model policy rejected local reasoning effort Some(High); required ultra",
        ),
        "unexpected effort override error: {}",
        effort_error.error.message,
    );

    let tier_update_id = server
        .send_thread_settings_update_request(ThreadSettingsUpdateParams {
            thread_id: thread_id.to_string(),
            service_tier: Some(Some("priority".to_string())),
            ..Default::default()
        })
        .await?;
    let tier_error: JSONRPCError = timeout(
        DEFAULT_READ_TIMEOUT,
        server.read_stream_until_error_message(RequestId::Integer(tier_update_id)),
    )
    .await??;
    assert!(
        tier_error
            .error
            .message
            .contains("rejected service tier `priority`"),
        "unexpected tier override error: {}",
        tier_error.error.message,
    );
    Ok(())
}

#[tokio::test]
async fn signed_desktop_no_tool_starts_normalize_to_astra_ultra_standard() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_managed_subscription_home(codex_home.path())?;
    let (mut server, _proxy) = managed_desktop_server(codex_home.path()).await?;

    for include_start_statsig_feature in [true, false] {
        let cases = [
            (
                "thread_summary",
                desktop_common_start(
                    "thread_summary",
                    LUNA_MODEL,
                    metadata_config(include_start_statsig_feature),
                    Some(codex_home.path()),
                ),
            ),
            (
                "commit_message",
                desktop_common_start(
                    "commit_message",
                    LUNA_MODEL,
                    message_config(include_start_statsig_feature),
                    Some(codex_home.path()),
                ),
            ),
            ("ambient_suggestion_safety", {
                let mut params = desktop_common_start(
                    "ambient_suggestion_safety",
                    LUNA_MODEL,
                    ambient_safety_config(include_start_statsig_feature),
                    /*cwd*/ None,
                );
                params.base_instructions = Some(AMBIENT_SAFETY_INSTRUCTIONS.to_string());
                params.developer_instructions = Some(String::new());
                params
            }),
        ];

        for (source, params) in cases {
            let request_id = server.send_thread_start_request(params).await?;
            let response: ThreadStartResponse =
                timeout(DEFAULT_READ_TIMEOUT, server.read_response(request_id)).await??;
            assert_managed_start(
                &response, source, /*expected_active_permission_profile*/ None,
            );
            assert_background_rejects_root_choices(&mut server, &response.thread.id).await?;
        }
    }

    let mut wrong_service_name = desktop_common_start(
        "thread_summary",
        LUNA_MODEL,
        metadata_config(/*include_start_statsig_feature*/ true),
        Some(codex_home.path()),
    );
    wrong_service_name.service_name = Some("not_codex_desktop".to_string());
    assert_hidden_start_is_not_classified(&mut server, wrong_service_name, "service name").await?;

    let mut unexpected_config = metadata_config(/*include_start_statsig_feature*/ true);
    unexpected_config.insert("unexpected.signed.shape".to_string(), json!(true));
    assert_hidden_start_is_not_classified(
        &mut server,
        desktop_common_start(
            "thread_summary",
            LUNA_MODEL,
            unexpected_config,
            Some(codex_home.path()),
        ),
        "config member",
    )
    .await?;

    assert!(
        timeout(DEFAULT_READ_TIMEOUT, server.shutdown_gracefully())
            .await??
            .success()
    );
    Ok(())
}

#[tokio::test]
async fn signed_desktop_metadata_fork_normalizes_to_no_tool_background_policy() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_managed_subscription_home(codex_home.path())?;
    let source_thread_id = create_fake_rollout(
        codex_home.path(),
        "2026-09-04T12-00-00",
        "2026-09-04T12:00:00Z",
        "source thread for a signed metadata fork",
        Some("openai"),
        /*git_info*/ None,
    )?;
    let (mut server, _proxy) = managed_desktop_server(codex_home.path()).await?;

    let mut wrong_hydration_shape =
        desktop_metadata_fork(&source_thread_id, /*cwd*/ None, "thread_title");
    wrong_hydration_shape.exclude_turns = false;
    let wrong_shape_id = server
        .send_thread_fork_request(wrong_hydration_shape)
        .await?;
    let wrong_shape_error: JSONRPCError = timeout(
        DEFAULT_READ_TIMEOUT,
        server.read_stream_until_error_message(RequestId::Integer(wrong_shape_id)),
    )
    .await??;
    assert!(
        wrong_shape_error
            .error
            .message
            .contains("features.multi_agent=false; required true"),
        "an include-turns fork must not acquire the automatic background contract: {}",
        wrong_shape_error.error.message,
    );

    let request_id = server
        .send_thread_fork_request(desktop_metadata_fork(
            &source_thread_id,
            /*cwd*/ None,
            "thread_title",
        ))
        .await?;
    let response: ThreadForkResponse =
        timeout(DEFAULT_READ_TIMEOUT, server.read_response(request_id)).await??;
    assert_eq!(
        (
            response.model.as_str(),
            response.model_provider.as_str(),
            response.reasoning_effort.clone(),
            response.service_tier.as_deref(),
            response.approval_policy,
            response.sandbox.clone(),
            response.active_permission_profile.clone(),
            response.runtime_workspace_roots.as_slice(),
        ),
        (
            ASTRA_MODEL,
            "openai",
            Some(ReasoningEffort::Ultra),
            Some(STANDARD_SERVICE_TIER),
            AskForApproval::Never,
            SandboxPolicy::ReadOnly {
                network_access: false,
            },
            None,
            &[][..],
        ),
    );
    assert_eq!(
        (
            response.thread.forked_from_id.as_deref(),
            response.thread.model.as_deref(),
            response.thread.reasoning_effort.clone(),
            response.thread.source.clone(),
            response.thread.thread_source.clone(),
            response.thread.ephemeral,
        ),
        (
            Some(source_thread_id.as_str()),
            Some(ASTRA_MODEL),
            Some(ReasoningEffort::Ultra),
            SessionSource::Unknown,
            Some(ThreadSource::Feature("thread_title".to_string())),
            true,
        ),
    );
    assert_background_rejects_root_choices(&mut server, &response.thread.id).await?;

    assert!(
        timeout(DEFAULT_READ_TIMEOUT, server.shutdown_gracefully())
            .await??
            .success()
    );
    Ok(())
}

#[tokio::test]
async fn signed_desktop_ambient_keeps_read_only_tools_but_not_root_choices() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_managed_subscription_home(codex_home.path())?;
    let (mut server, _proxy) = managed_desktop_server(codex_home.path()).await?;

    for include_start_statsig_feature in [true, false] {
        for (model, effort) in [(TERRA_MODEL, "medium"), (LUNA_MODEL, "low")] {
            let request_id = server
                .send_thread_start_request(desktop_common_start(
                    "ambient_suggestions",
                    model,
                    ambient_config(effort, include_start_statsig_feature),
                    /*cwd*/ None,
                ))
                .await?;
            let response: ThreadStartResponse =
                timeout(DEFAULT_READ_TIMEOUT, server.read_response(request_id)).await??;
            assert_managed_start(
                &response,
                "ambient_suggestions",
                Some(ActivePermissionProfile::read_only()),
            );

            let status = mcp_status(&mut server, response.thread.id.clone()).await?;
            assert_eq!(status.next_cursor, None);
            let mcp = status
                .data
                .iter()
                .find(|mcp| mcp.name == READ_ONLY_MCP_NAME)
                .context("ambient background should retain the configured read-only MCP server")?;
            let echo = mcp
                .tools
                .get("echo")
                .context("ambient background should retain the read-only echo tool")?;
            assert_eq!(
                (
                    mcp.name.as_str(),
                    echo.name.as_str(),
                    echo.annotations.as_ref(),
                ),
                (
                    READ_ONLY_MCP_NAME,
                    "echo",
                    Some(&json!({ "readOnlyHint": true })),
                ),
            );
            assert_background_rejects_root_choices(&mut server, &response.thread.id).await?;
        }
    }

    assert!(
        timeout(DEFAULT_READ_TIMEOUT, server.shutdown_gracefully())
            .await??
            .success()
    );
    Ok(())
}

#[tokio::test]
async fn visible_root_keeps_picker_choice_while_explicit_fallback_stays_rejected() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_managed_subscription_home(codex_home.path())?;
    let (mut desktop, _proxy) = managed_desktop_server(codex_home.path()).await?;

    let request_id = desktop
        .send_thread_start_request(ThreadStartParams {
            model: Some(LUNA_MODEL.to_string()),
            cwd: Some(codex_home.path().display().to_string()),
            runtime_workspace_roots: Some(Vec::new()),
            approval_policy: Some(AskForApproval::Never),
            permissions: Some(":read-only".to_string()),
            config: Some(HashMap::from([
                ("model_reasoning_effort".to_string(), json!("low")),
                ("plan_mode_reasoning_effort".to_string(), json!("low")),
            ])),
            service_name: Some(DESKTOP_SERVICE_NAME.to_string()),
            ephemeral: Some(true),
            ..Default::default()
        })
        .await?;
    let selected: ThreadStartResponse =
        timeout(DEFAULT_READ_TIMEOUT, desktop.read_response(request_id)).await??;
    assert_eq!(
        (
            selected.model.as_str(),
            selected.reasoning_effort.clone(),
            selected.service_tier.as_deref(),
            selected.thread.source.clone(),
            selected.thread.model.as_deref(),
            selected.thread.reasoning_effort.clone(),
        ),
        (
            LUNA_MODEL,
            Some(ReasoningEffort::Low),
            Some(STANDARD_SERVICE_TIER),
            SessionSource::VsCode,
            Some(LUNA_MODEL),
            Some(ReasoningEffort::Low),
        ),
    );
    assert!(
        timeout(DEFAULT_READ_TIMEOUT, desktop.shutdown_gracefully())
            .await??
            .success()
    );

    let user_config_home = codex_home.path().to_string_lossy().into_owned();
    let mut non_desktop = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .with_env_overrides(&[
            (MODEL_POLICY_LANE_ENV, Some("subscription")),
            ("CDX_USER_CONFIG_HOMES", Some(user_config_home.as_str())),
            ("OPENAI_API_KEY", None),
            ("CODEX_API_KEY", None),
        ])
        .build()
        .await?;
    timeout(DEFAULT_READ_TIMEOUT, non_desktop.initialize()).await??;
    assert_hidden_start_is_not_classified(
        &mut non_desktop,
        desktop_common_start(
            "thread_summary",
            LUNA_MODEL,
            metadata_config(/*include_start_statsig_feature*/ true),
            Some(codex_home.path()),
        ),
        "client identity",
    )
    .await?;
    let fallback_id = non_desktop
        .send_thread_start_request(ThreadStartParams {
            model: Some(LUNA_MODEL.to_string()),
            allow_provider_model_fallback: true,
            cwd: Some(codex_home.path().display().to_string()),
            runtime_workspace_roots: Some(Vec::new()),
            approval_policy: Some(AskForApproval::Never),
            permissions: Some(":read-only".to_string()),
            config: Some(HashMap::from([
                ("model_reasoning_effort".to_string(), json!("low")),
                ("plan_mode_reasoning_effort".to_string(), json!("low")),
            ])),
            ..Default::default()
        })
        .await?;
    let fallback_error: JSONRPCError = timeout(
        DEFAULT_READ_TIMEOUT,
        non_desktop.read_stream_until_error_message(RequestId::Integer(fallback_id)),
    )
    .await??;
    assert_eq!(
        fallback_error.error.message,
        "locked model lanes reject provider model fallback",
    );
    assert!(
        timeout(DEFAULT_READ_TIMEOUT, non_desktop.shutdown_gracefully())
            .await??
            .success()
    );
    Ok(())
}
