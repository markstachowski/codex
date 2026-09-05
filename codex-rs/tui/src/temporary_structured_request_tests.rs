use super::STRUCTURED_RESPONSE_MAX_BYTES;
use super::TemporaryStructuredThreadOptions;
use super::collect_structured_response;
use super::start_temporary_thread;
use super::structured_request_timeout_for_lane;
use super::temporary_inference_settings_for_lane;
use crate::legacy_core::config::ModelPolicyLane;
use crate::test_support::PathBufExt;
use codex_app_server_protocol::ItemCompletedNotification;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::Turn;
use codex_app_server_protocol::TurnCompletedNotification;
use codex_app_server_protocol::TurnStatus;
use codex_protocol::openai_models::ReasoningEffort;
use pretty_assertions::assert_eq;
use tempfile::tempdir;
use tokio::sync::mpsc::unbounded_channel;

#[test]
fn managed_temporary_requests_receive_an_ultra_compatible_timeout() {
    assert_eq!(
        structured_request_timeout_for_lane(Some(ModelPolicyLane::Subscription)),
        std::time::Duration::from_secs(120)
    );
    assert_eq!(
        structured_request_timeout_for_lane(/*lane*/ None),
        std::time::Duration::from_secs(30)
    );
}

fn agent_message_notification(turn_id: &str, text: &str) -> ServerNotification {
    ServerNotification::ItemCompleted(ItemCompletedNotification {
        item: ThreadItem::AgentMessage {
            id: "message-1".to_string(),
            text: text.to_string(),
            phase: None,
            memory_citation: None,
            delivery: None,
            questions: None,
        },
        thread_id: "thread-1".to_string(),
        turn_id: turn_id.to_string(),
        completed_at_ms: 0,
    })
}

fn turn_completed_notification(turn_id: &str, status: TurnStatus) -> ServerNotification {
    ServerNotification::TurnCompleted(TurnCompletedNotification {
        thread_id: "thread-1".to_string(),
        turn: Turn {
            id: turn_id.to_string(),
            items: Vec::new(),
            items_view: Default::default(),
            status,
            error: None,
            started_at: None,
            completed_at: None,
            duration_ms: None,
        },
    })
}

#[test]
fn managed_temporary_threads_preserve_model_and_effort() {
    for lane in [
        ModelPolicyLane::Subscription,
        ModelPolicyLane::Api,
        ModelPolicyLane::Spark,
    ] {
        let settings = temporary_inference_settings_for_lane(
            "gpt-5.6-luna".to_string(),
            Some(ReasoningEffort::Low),
            Some(lane),
        );
        assert_eq!(
            (settings.model, settings.reasoning_effort),
            ("gpt-5.6-luna".to_string(), Some(ReasoningEffort::Low)),
            "managed automatic inference preserves operator choices for {lane:?}",
        );
    }

    let settings = temporary_inference_settings_for_lane(
        "gpt-5.6-luna".to_string(),
        Some(ReasoningEffort::Low),
        /*lane*/ None,
    );
    assert_eq!(
        (settings.model, settings.reasoning_effort),
        ("gpt-5.6-luna".to_string(), Some(ReasoningEffort::Low)),
        "unmanaged temporary inference must preserve upstream behavior",
    );
}

#[tokio::test]
async fn preserves_custom_permissions_and_disables_required_mcp_servers() -> color_eyre::Result<()>
{
    let (chat_widget, _, _, _) =
        crate::chatwidget::tests::make_chatwidget_manual_with_sender().await;
    let mut config = chat_widget.config_ref().clone();
    let codex_home = tempdir()?;
    let denied_path = codex_home.path().join("denied");
    let denied_key = toml::Value::String(denied_path.display().to_string());
    std::fs::write(
        codex_home.path().join("config.toml"),
        format!(
            "model_reasoning_effort = \"low\"\n\
             plan_mode_reasoning_effort = \"high\"\n\
             default_permissions = \"title-restricted\"\n\n\
             [permissions.title-restricted.filesystem]\n\
             \":root\" = \"read\"\n\
             {denied_key} = \"deny\"\n\n\
             [mcp_servers.forbidden]\n\
             command = \"codex-auto-title-missing-mcp\"\n\
             required = true\n"
        ),
    )?;
    config.codex_home = codex_home.path().to_path_buf().abs();
    config.sqlite =
        codex_state::SqliteConfig::new_for_testing(codex_home.path().to_path_buf().abs());

    let app_server = crate::start_embedded_app_server_for_picker(&config).await?;
    let response = start_temporary_thread(
        &app_server.request_handle(),
        TemporaryStructuredThreadOptions {
            model: "gpt-5.2".to_string(),
            model_provider: config.model_provider_id.clone(),
            cwd: config.cwd.display().to_string(),
            active_permission_profile: Some("title-restricted".to_string()),
            mcp_server_names: vec!["forbidden".to_string()],
        },
    )
    .await?;

    assert_eq!(
        (
            response.active_permission_profile.map(|profile| profile.id),
            response.model_provider,
            response.thread.ephemeral,
            response.model,
            response.reasoning_effort,
        ),
        (
            Some("title-restricted".to_string()),
            config.model_provider_id,
            true,
            "gpt-5.2".to_string(),
            Some(ReasoningEffort::Low),
        )
    );

    app_server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn returns_latest_matching_assistant_message() {
    let (sender, receiver) = unbounded_channel();
    sender
        .send(agent_message_notification("other-turn", "ignore me"))
        .expect("send unrelated assistant message");
    sender
        .send(turn_completed_notification(
            "other-turn",
            TurnStatus::Completed,
        ))
        .expect("send unrelated completion");
    sender
        .send(agent_message_notification("turn-1", "first"))
        .expect("send first assistant message");
    sender
        .send(agent_message_notification("turn-1", "final"))
        .expect("send final assistant message");
    sender
        .send(turn_completed_notification("turn-1", TurnStatus::Completed))
        .expect("send matching completion");

    let response = collect_structured_response(receiver, "turn-1")
        .await
        .expect("collect structured response");

    assert_eq!(response, "final");
}

#[tokio::test]
async fn rejects_failed_turn() {
    let (sender, receiver) = unbounded_channel();
    sender
        .send(turn_completed_notification("turn-1", TurnStatus::Failed))
        .expect("send failed completion");

    let error = collect_structured_response(receiver, "turn-1")
        .await
        .expect_err("failed turn should not produce a response");

    assert_eq!(
        error.to_string(),
        "temporary structured turn ended with status Failed"
    );
}

#[tokio::test]
async fn rejects_completion_without_assistant_message() {
    let (sender, receiver) = unbounded_channel();
    sender
        .send(turn_completed_notification("turn-1", TurnStatus::Completed))
        .expect("send completion");

    let error = collect_structured_response(receiver, "turn-1")
        .await
        .expect_err("completion without assistant message should fail");

    assert_eq!(
        error.to_string(),
        "temporary structured turn completed without a response"
    );
}

#[tokio::test]
async fn rejects_closed_notification_channel() {
    let (sender, receiver) = unbounded_channel();
    drop(sender);

    let error = collect_structured_response(receiver, "turn-1")
        .await
        .expect_err("closed notification channel should fail");

    assert_eq!(
        error.to_string(),
        "temporary structured turn notification channel closed"
    );
}

#[tokio::test]
async fn rejects_oversized_assistant_message() {
    let (sender, receiver) = unbounded_channel();
    let oversized = "x".repeat(STRUCTURED_RESPONSE_MAX_BYTES + 1);
    sender
        .send(agent_message_notification("turn-1", &oversized))
        .expect("send oversized assistant message");
    sender
        .send(turn_completed_notification("turn-1", TurnStatus::Completed))
        .expect("send matching completion");

    let error = collect_structured_response(receiver, "turn-1")
        .await
        .expect_err("oversized assistant message should be rejected");

    assert_eq!(
        error.to_string(),
        "temporary structured response exceeds 8192 bytes"
    );
}
