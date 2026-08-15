use anyhow::Result;
use codex_core::CodexThread;
use codex_core::TurnInputRequest;
use codex_protocol::config_types::CollaborationMode;
use codex_protocol::config_types::ModeKind;
use codex_protocol::config_types::Settings;
use codex_protocol::items::TurnItem;
use codex_protocol::models::ContentItem;
use codex_protocol::models::PermissionProfile;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::CodexErrorInfo;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ModelVerification;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::ThreadSettingsOverrides;
use codex_protocol::user_input::UserInput;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_function_call;
use core_test_support::responses::ev_model_verification_metadata;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_response_once;
use core_test_support::responses::mount_response_sequence;
use core_test_support::responses::sse;
use core_test_support::responses::sse_completed;
use core_test_support::responses::sse_response;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::TestCodex;
use core_test_support::test_codex::local_selections;
use core_test_support::test_codex::test_codex;
use core_test_support::test_codex::turn_permission_fields;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use wiremock::ResponseTemplate;

const SERVER_MODEL: &str = "gpt-5.2";
const REQUESTED_MODEL: &str = "gpt-5.3-codex";
const TRUSTED_ACCESS_FOR_CYBER_VERIFICATION: &str = "trusted_access_for_cyber";

const CYBER_POLICY_MESSAGE: &str =
    "This request has been flagged for potentially high-risk cyber activity.";

fn disabled_text_turn(test: &TestCodex, text: &str) -> TurnInputRequest {
    let (sandbox_policy, permission_profile) =
        turn_permission_fields(PermissionProfile::Disabled, test.cwd_path());
    TurnInputRequest::user_input(vec![UserInput::Text {
        text: text.to_string(),
        text_elements: Vec::new(),
    }])
    .with_thread_settings(ThreadSettingsOverrides {
        environments: Some(local_selections(test.config.cwd.clone())),
        approval_policy: Some(AskForApproval::Never),
        sandbox_policy: Some(sandbox_policy),
        permission_profile,
        collaboration_mode: Some(CollaborationMode {
            mode: ModeKind::Default,
            settings: Settings {
                model: test.session_configured.model.clone(),
                reasoning_effort: test.config.model_reasoning_effort.clone(),
                developer_instructions: None,
            },
        }),
        ..Default::default()
    })
}

async fn assert_server_model_mismatch_is_terminal(codex: &CodexThread) {
    let mut mismatch_error = None;
    let turn_complete = loop {
        let event = wait_for_event(codex, |_| true).await;
        match event {
            EventMsg::Error(error) if error.message.contains("server-reported model") => {
                mismatch_error = Some(error);
            }
            EventMsg::AgentMessage(_)
            | EventMsg::AgentMessageContentDelta(_)
            | EventMsg::ItemStarted(codex_protocol::protocol::ItemStartedEvent {
                item: TurnItem::AgentMessage(_),
                ..
            })
            | EventMsg::ItemCompleted(codex_protocol::protocol::ItemCompletedEvent {
                item: TurnItem::AgentMessage(_),
                ..
            }) => panic!("mismatched server output must not be accepted"),
            EventMsg::RawResponseItem(raw)
                if matches!(
                    &raw.item,
                    ResponseItem::Message { role, .. } if role == "assistant"
                ) || matches!(&raw.item, ResponseItem::FunctionCall { .. }) =>
            {
                panic!("mismatched raw model output must not be accepted");
            }
            EventMsg::ExecCommandBegin(_) => {
                panic!("tool output from a mismatched model must not be executed");
            }
            EventMsg::ModelReroute(_) => {
                panic!("model mismatch must fail instead of emitting a reroute event");
            }
            EventMsg::TurnComplete(event) => break event,
            _ => {}
        }
    };

    let mismatch_error = mismatch_error.expect("expected terminal server-model mismatch error");
    assert!(mismatch_error.message.contains(REQUESTED_MODEL));
    assert!(mismatch_error.message.contains(SERVER_MODEL));
    assert!(mismatch_error.message.contains("silently rerouted model"));
    assert_eq!(mismatch_error.codex_error_info, Some(CodexErrorInfo::Other));
    assert_eq!(turn_complete.last_agent_message, None);
    assert!(
        turn_complete
            .error
            .as_ref()
            .is_some_and(|error| error.message == mismatch_error.message)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn openai_model_header_mismatch_is_terminal_before_assistant_output() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let response = sse_response(sse(vec![
        ev_response_created("resp-1"),
        ev_assistant_message("msg-1", "must not be accepted"),
        core_test_support::responses::ev_completed("resp-1"),
    ]))
    .insert_header("OpenAI-Model", SERVER_MODEL);
    let mock = mount_response_once(&server, response).await;

    let mut builder = test_codex().with_model(REQUESTED_MODEL);
    let test = builder.build(&server).await?;

    test.codex
        .start_or_steer_turn(disabled_text_turn(&test, "trigger safety check"))
        .await?;

    assert_server_model_mismatch_is_terminal(&test.codex).await;
    mock.single_request();

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cyber_policy_response_emits_typed_error_without_retry() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let response = ResponseTemplate::new(400).set_body_json(serde_json::json!({
        "error": {
            "message": CYBER_POLICY_MESSAGE,
            "type": "invalid_request",
            "param": null,
            "code": "cyber_policy"
        }
    }));
    let mock = mount_response_once(&server, response).await;

    let mut builder = test_codex().with_model(REQUESTED_MODEL);
    let test = builder.build(&server).await?;

    test.codex
        .start_or_steer_turn(disabled_text_turn(&test, "trigger cyber policy error"))
        .await?;

    let error = wait_for_event(&test.codex, |event| matches!(event, EventMsg::Error(_))).await;
    let EventMsg::Error(error) = error else {
        panic!("expected error event");
    };
    assert_eq!(error.message, CYBER_POLICY_MESSAGE);
    assert_eq!(error.codex_error_info, Some(CodexErrorInfo::CyberPolicy));

    mock.single_request();

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn response_model_field_mismatch_is_terminal_when_header_matches_requested() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let response = sse_response(sse(vec![
        serde_json::json!({
            "type": "response.created",
            "response": {
                "id": "resp-1",
                "headers": {
                    "OpenAI-Model": SERVER_MODEL
                }
            }
        }),
        ev_assistant_message("msg-1", "must not be accepted"),
        core_test_support::responses::ev_completed("resp-1"),
    ]))
    .insert_header("OpenAI-Model", REQUESTED_MODEL);
    let mock = mount_response_once(&server, response).await;

    let mut builder = test_codex().with_model(REQUESTED_MODEL);
    let test = builder.build(&server).await?;

    test.codex
        .start_or_steer_turn(disabled_text_turn(&test, "trigger response model check"))
        .await?;

    assert_server_model_mismatch_is_terminal(&test.codex).await;
    mock.single_request();

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn openai_model_header_mismatch_blocks_tool_execution() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let tool_args = serde_json::json!({
        "command": "echo hello",
        "timeout_ms": 1_000
    });

    let first_response = sse_response(sse(vec![
        ev_response_created("resp-1"),
        ev_function_call(
            "call-1",
            "shell_command",
            &serde_json::to_string(&tool_args)?,
        ),
        core_test_support::responses::ev_completed("resp-1"),
    ]))
    .insert_header("OpenAI-Model", SERVER_MODEL);
    let mock = mount_response_once(&server, first_response).await;

    let mut builder = test_codex().with_model(REQUESTED_MODEL);
    let test = builder.build(&server).await?;

    test.codex
        .start_or_steer_turn(disabled_text_turn(&test, "trigger tool call"))
        .await?;

    assert_server_model_mismatch_is_terminal(&test.codex).await;
    mock.single_request();

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn follow_up_model_header_mismatch_stops_before_second_tool_execution() -> Result<()> {
    skip_if_no_network!(Ok(()));

    const ALLOWED_CALL_ID: &str = "call-allowed";
    const BLOCKED_CALL_ID: &str = "call-blocked";

    let allowed_sentinel = "allowed-tool-sentinel";
    let blocked_sentinel = "blocked-tool-sentinel";
    let server = start_mock_server().await;
    let allowed_tool_args = serde_json::json!({
        "command": format!("echo allowed > {allowed_sentinel}"),
        "timeout_ms": 1_000
    });
    let blocked_tool_args = serde_json::json!({
        "command": format!("echo blocked > {blocked_sentinel}"),
        "timeout_ms": 1_000
    });
    let first_response = sse_response(sse(vec![
        ev_response_created("resp-1"),
        ev_function_call(
            ALLOWED_CALL_ID,
            "shell_command",
            &serde_json::to_string(&allowed_tool_args)?,
        ),
        core_test_support::responses::ev_completed("resp-1"),
    ]))
    .insert_header("OpenAI-Model", REQUESTED_MODEL);
    let second_response = sse_response(sse(vec![
        ev_response_created("resp-2"),
        ev_assistant_message("msg-blocked", "must not be accepted"),
        ev_function_call(
            BLOCKED_CALL_ID,
            "shell_command",
            &serde_json::to_string(&blocked_tool_args)?,
        ),
        core_test_support::responses::ev_completed("resp-2"),
    ]))
    .insert_header("OpenAI-Model", SERVER_MODEL);
    let request_log = mount_response_sequence(&server, vec![first_response, second_response]).await;

    let mut builder = test_codex().with_model(REQUESTED_MODEL);
    let test = builder.build(&server).await?;
    let allowed_path = test.cwd.path().join(allowed_sentinel);
    let blocked_path = test.cwd.path().join(blocked_sentinel);
    assert!(!allowed_path.exists());
    assert!(!blocked_path.exists());

    test.codex
        .start_or_steer_turn(disabled_text_turn(&test, "trigger follow-up model check"))
        .await?;

    let mut allowed_exec_begin_count = 0;
    let allowed_exec_end = loop {
        let event = wait_for_event(&test.codex, |_| true).await;
        match event {
            EventMsg::ExecCommandBegin(event) => {
                assert_eq!(event.call_id, ALLOWED_CALL_ID);
                allowed_exec_begin_count += 1;
            }
            EventMsg::ExecCommandEnd(event) if event.call_id == ALLOWED_CALL_ID => {
                assert_eq!(event.call_id, ALLOWED_CALL_ID);
                assert_eq!(event.exit_code, 0);
                break event;
            }
            EventMsg::ExecCommandEnd(event) => {
                panic!("unexpected tool execution completed: {}", event.call_id);
            }
            _ => {}
        }
    };

    assert_eq!(allowed_exec_begin_count, 1);
    assert_eq!(allowed_exec_end.call_id, ALLOWED_CALL_ID);
    assert!(
        allowed_path.exists(),
        "the matching response tool must execute"
    );

    assert_server_model_mismatch_is_terminal(&test.codex).await;
    assert!(
        request_log
            .function_call_output_text(ALLOWED_CALL_ID)
            .is_some(),
        "the matching response tool output must reach exactly one follow-up request"
    );
    assert_eq!(request_log.requests().len(), 2);

    test.codex.submit(Op::Shutdown).await?;
    loop {
        let event = wait_for_event(&test.codex, |_| true).await;
        match event {
            EventMsg::ExecCommandBegin(_)
            | EventMsg::ExecCommandOutputDelta(_)
            | EventMsg::TerminalInteraction(_)
            | EventMsg::ExecCommandEnd(_) => {
                panic!("mismatched tool execution arrived after turn completion");
            }
            EventMsg::ShutdownComplete => break,
            _ => {}
        }
    }
    test.codex.wait_until_terminated().await;

    assert!(
        !blocked_path.exists(),
        "the mismatched response tool must remain unexecuted after shutdown"
    );
    assert_eq!(
        request_log.requests().len(),
        2,
        "the terminal mismatch must not issue another model request"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn openai_model_header_casing_only_difference_is_accepted() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let requested_header = REQUESTED_MODEL.to_ascii_uppercase();
    let response = sse_response(sse_completed("resp-1"))
        .insert_header("OpenAI-Model", requested_header.as_str());
    let mock = mount_response_once(&server, response).await;

    let mut builder = test_codex().with_model(REQUESTED_MODEL);
    let test = builder.build(&server).await?;

    test.codex
        .start_or_steer_turn(disabled_text_turn(&test, "trigger casing check"))
        .await?;

    let mut error_count = 0;
    let mut reroute_count = 0;
    let mut model_warning_messages = Vec::new();
    let mut warning_item_count = 0;
    let turn_complete = loop {
        let event = wait_for_event(&test.codex, |_| true).await;
        match event {
            EventMsg::Error(_) => error_count += 1,
            EventMsg::ModelReroute(_) => reroute_count += 1,
            EventMsg::Warning(warning)
                if warning.message.contains("server-reported model")
                    || warning.message.contains("silently rerouted model") =>
            {
                model_warning_messages.push(warning.message);
            }
            EventMsg::RawResponseItem(raw)
                if matches!(
                    &raw.item,
                    ResponseItem::Message { content, .. }
                        if content.iter().any(|item| matches!(
                            item,
                            ContentItem::InputText { text } | ContentItem::OutputText { text }
                                if text.starts_with("Warning: ")
                        ))
                ) =>
            {
                warning_item_count += 1;
            }
            EventMsg::TurnComplete(event) => break event,
            _ => {}
        }
    };

    assert_eq!(error_count, 0);
    assert_eq!(reroute_count, 0);
    assert_eq!(model_warning_messages, Vec::<String>::new());
    assert_eq!(warning_item_count, 0);
    assert!(turn_complete.error.is_none());
    assert_eq!(turn_complete.last_agent_message, None);
    mock.single_request();

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_verification_emits_structured_event_without_reroute_or_warning() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let response = sse_response(sse(vec![
        ev_response_created("resp-1"),
        ev_model_verification_metadata("resp-1", vec![TRUSTED_ACCESS_FOR_CYBER_VERIFICATION]),
        core_test_support::responses::ev_completed("resp-1"),
    ]));
    let _mock = mount_response_once(&server, response).await;

    let mut builder = test_codex().with_model(SERVER_MODEL);
    let test = builder.build(&server).await?;

    test.codex
        .start_or_steer_turn(disabled_text_turn(&test, "trigger model verification"))
        .await?;

    let mut verification_count = 0;
    let mut reroute_count = 0;
    let mut warning_count = 0;
    let mut warning_item_count = 0;
    loop {
        let event = wait_for_event(&test.codex, |_| true).await;
        match event {
            EventMsg::ModelVerification(event) => {
                assert_eq!(
                    event.verifications,
                    vec![ModelVerification::TrustedAccessForCyber]
                );
                verification_count += 1;
            }
            EventMsg::Warning(_) => warning_count += 1,
            EventMsg::ModelReroute(_) => reroute_count += 1,
            EventMsg::RawResponseItem(raw)
                if matches!(
                    &raw.item,
                    ResponseItem::Message { content, .. }
                        if content.iter().any(|item| matches!(
                            item,
                            ContentItem::InputText { text } if text.starts_with("Warning: ")
                        ))
                ) =>
            {
                warning_item_count += 1;
            }
            EventMsg::TurnComplete(_) => break,
            _ => {}
        }
    }

    assert_eq!(verification_count, 1);
    assert_eq!(reroute_count, 0);
    assert_eq!(warning_count, 0);
    assert_eq!(warning_item_count, 0);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_verification_only_emits_once_per_turn() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let tool_args = serde_json::json!({
        "command": "echo hello",
        "timeout_ms": 1_000
    });

    let first_response = sse_response(sse(vec![
        ev_response_created("resp-1"),
        ev_function_call(
            "call-1",
            "shell_command",
            &serde_json::to_string(&tool_args)?,
        ),
        ev_model_verification_metadata("resp-1", vec![TRUSTED_ACCESS_FOR_CYBER_VERIFICATION]),
        core_test_support::responses::ev_completed("resp-1"),
    ]));
    let second_response = sse_response(sse(vec![
        ev_response_created("resp-2"),
        ev_model_verification_metadata("resp-2", vec![TRUSTED_ACCESS_FOR_CYBER_VERIFICATION]),
        ev_assistant_message("msg-1", "done"),
        core_test_support::responses::ev_completed("resp-2"),
    ]));
    let _mock = mount_response_sequence(&server, vec![first_response, second_response]).await;

    let mut builder = test_codex().with_model(SERVER_MODEL);
    let test = builder.build(&server).await?;

    test.codex
        .start_or_steer_turn(disabled_text_turn(
            &test,
            "trigger follow-up model verification",
        ))
        .await?;

    let mut verification_count = 0;
    loop {
        let event = wait_for_event(&test.codex, |_| true).await;
        match event {
            EventMsg::ModelVerification(_) => verification_count += 1,
            EventMsg::Warning(warning) if warning.message.contains("high-risk cyber activity") => {
                panic!("model verification should not emit a warning event");
            }
            EventMsg::TurnComplete(_) => break,
            _ => {}
        }
    }

    assert_eq!(verification_count, 1);

    Ok(())
}
