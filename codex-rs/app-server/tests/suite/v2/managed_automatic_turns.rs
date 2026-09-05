use super::managed_automatic_threads::DEFAULT_READ_TIMEOUT;
use super::managed_automatic_threads::LUNA_MODEL;
use super::managed_automatic_threads::assert_managed_start;
use super::managed_automatic_threads::desktop_common_start;
use super::managed_automatic_threads::metadata_config;
use anyhow::Result;
use app_test_support::ChatGptAuthFixture;
use app_test_support::TestAppServer;
use app_test_support::write_chatgpt_auth;
use app_test_support::write_models_cache;
use codex_app_server_protocol::ClientInfo;
use codex_app_server_protocol::JSONRPCError;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::TurnStatus;
use codex_app_server_protocol::UserInput;
use codex_config::types::AuthCredentialsStoreMode;
use codex_core::config::MODEL_POLICY_LANE_ENV;
use codex_protocol::config_types::ReasoningSummary;
use codex_protocol::openai_models::ReasoningEffort;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::path::Path;
use tempfile::TempDir;
use tokio::io::AsyncBufReadExt;
use tokio::io::BufReader;
use tokio::net::TcpListener;
use tokio::time::timeout;

const ASTRA_MODEL: &str = "gpt-6-astra";
const SOL_MODEL: &str = "gpt-5.6-sol";
const DESKTOP_CLIENT_NAME: &str = "Codex Desktop";

fn write_managed_subscription_home(codex_home: &Path) -> Result<()> {
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

fn signed_turn(thread_id: &str) -> TurnStartParams {
    TurnStartParams {
        thread_id: thread_id.to_string(),
        client_user_message_id: Some("00000000-0000-4000-8000-000000000001".to_string()),
        input: vec![UserInput::Text {
            text: "Summarize this thread.".to_string(),
            text_elements: Vec::new(),
        }],
        turn_trigger: Some("thread_summary".to_string()),
        cwd: None,
        runtime_workspace_roots: Some(Vec::new()),
        approval_policy: None,
        permissions: Some(":read-only".to_string()),
        model: None,
        service_tier: Some(None),
        effort: None,
        summary: Some(ReasoningSummary::None),
        personality: None,
        output_schema: Some(json!({
            "type": "object",
            "properties": {
                "summary": { "type": "string" },
            },
            "required": ["summary"],
            "additionalProperties": false,
        })),
        collaboration_mode: None,
        ..Default::default()
    }
}

async fn assert_turn_override_is_rejected(
    server: &mut TestAppServer,
    params: TurnStartParams,
    expected_error: &str,
) -> Result<()> {
    let request_id = server.send_turn_start_request(params).await?;
    let error: JSONRPCError = timeout(
        DEFAULT_READ_TIMEOUT,
        server.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;
    assert!(
        error.error.message.contains(expected_error),
        "unexpected turn override error: {}",
        error.error.message,
    );
    Ok(())
}

#[tokio::test]
async fn signed_desktop_turn_inherits_astra_ultra_standard_and_rejects_overrides() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_managed_subscription_home(codex_home.path())?;
    let proxy = TcpListener::bind("127.0.0.1:0").await?;
    let proxy_uri = format!("http://{}", proxy.local_addr()?);
    let user_config_home = codex_home.path().to_string_lossy().into_owned();
    let mut server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .with_env_overrides(&[
            (MODEL_POLICY_LANE_ENV, Some("subscription")),
            ("CDX_USER_CONFIG_HOMES", Some(user_config_home.as_str())),
            ("OPENAI_API_KEY", None),
            ("CODEX_API_KEY", None),
            ("HTTPS_PROXY", Some(proxy_uri.as_str())),
            ("https_proxy", Some(proxy_uri.as_str())),
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

    let start_id = server
        .send_thread_start_request(desktop_common_start(
            "thread_summary",
            LUNA_MODEL,
            metadata_config(/*allow_start_statsig_feature*/ true),
            Some(codex_home.path()),
        ))
        .await?;
    let started = timeout(DEFAULT_READ_TIMEOUT, server.read_response(start_id)).await??;
    assert_managed_start(
        &started,
        "thread_summary",
        /*expected_active_permission_profile*/ None,
    );

    let mut model_override = signed_turn(&started.thread.id);
    model_override.model = Some(SOL_MODEL.to_string());
    assert_turn_override_is_rejected(
        &mut server,
        model_override,
        "subscription model policy rejected model `gpt-5.6-sol`; required `gpt-6-astra`",
    )
    .await?;

    let mut effort_override = signed_turn(&started.thread.id);
    effort_override.effort = Some(ReasoningEffort::High);
    assert_turn_override_is_rejected(
        &mut server,
        effort_override,
        "subscription model policy rejected local reasoning effort Some(High); required ultra",
    )
    .await?;

    let mut tier_override = signed_turn(&started.thread.id);
    tier_override.service_tier = Some(Some("priority".to_string()));
    assert_turn_override_is_rejected(
        &mut server,
        tier_override,
        "rejected service tier `priority`",
    )
    .await?;

    let turn_id = server
        .send_turn_start_request(signed_turn(&started.thread.id))
        .await?;
    let TurnStartResponse { turn } =
        timeout(DEFAULT_READ_TIMEOUT, server.read_response(turn_id)).await??;
    assert_eq!(turn.status, TurnStatus::InProgress);

    let (stream, _) = timeout(DEFAULT_READ_TIMEOUT, proxy.accept()).await??;
    let mut stream = BufReader::new(stream);
    let mut request = String::new();
    timeout(DEFAULT_READ_TIMEOUT, stream.read_line(&mut request)).await??;
    assert!(
        request.starts_with("CONNECT chatgpt.com:443 "),
        "expected the official ChatGPT endpoint through the local blocking proxy, got {request:?}",
    );

    server
        .interrupt_turn_and_wait_for_aborted(started.thread.id, turn.id, DEFAULT_READ_TIMEOUT)
        .await?;
    assert!(
        timeout(DEFAULT_READ_TIMEOUT, server.shutdown_gracefully())
            .await??
            .success()
    );
    Ok(())
}
