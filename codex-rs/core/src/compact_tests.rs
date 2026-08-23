use super::*;
use crate::session::step_settings::StepSettingsUpdate;
use crate::session::turn_context::NewTurnContextOptions;
use codex_features::Feature;
use codex_history::CodexHarnessMetadata;
use codex_history::ResponseItemEnvelope;
use codex_login::CodexAuth;
use codex_protocol::ResponseItemId;
use codex_protocol::config_types::ReasoningSummary;
use codex_protocol::config_types::SERVICE_TIER_DEFAULT_REQUEST_VALUE;
use codex_protocol::config_types::ServiceTier;
use codex_protocol::models::BaseInstructionsProvenance;
use codex_protocol::models::ContentItemKind;
use codex_protocol::models::DEFAULT_IMAGE_DETAIL;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::InternalChatMessageMetadataPassthrough;
use codex_protocol::openai_models::InputModality;
use codex_protocol::openai_models::ModelServiceTier;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::openai_models::ToolMode;
use codex_protocol::openai_models::TruncationPolicyConfig;
use codex_tools::JsonSchema;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolSpec;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::mount_response_sequence;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::sse;
use core_test_support::responses::sse_response;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use crate::session::step_context::StepContext;
use crate::session::turn_context::TurnContext;
use crate::tools::ToolRouter;
use crate::tools::registry::ToolRegistry;

const STEP_AUTHORITY_MODEL: &str = "gpt-5.6-sol";
const STEP_AUTHORITY_INSTRUCTIONS: &str = "STEP_AUTHORITY_COMPACTION_INSTRUCTIONS";
const STEP_AUTHORITY_TOOL: &str = "step_authority_compaction_tool";
const STEP_AUTHORITY_HISTORY_TEXT: &str = "STEP_AUTHORITY_HISTORY_TEXT";
const STEP_AUTHORITY_IMAGE_URL: &str = "file://step-authority-image.png";
const STEP_AUTHORITY_OUTPUT: &str = "STEP_AUTHORITY_OVERSIZED_OUTPUT";
const STEP_AUTHORITY_TRUNCATED_OUTPUT: &str =
    "Output exceeded the available model context and was truncated";

struct DivergentCompactionFixture {
    session: Arc<crate::session::session::Session>,
    lifecycle_turn: Arc<TurnContext>,
    request_step: Arc<StepContext>,
}

async fn divergent_compaction_fixture(base_url: String) -> DivergentCompactionFixture {
    let (session, lifecycle_turn, _rx) =
        crate::session::tests::make_session_and_context_with_auth_and_config_and_rx(
            CodexAuth::from_api_key("Test API Key"),
            Vec::new(),
            move |config| {
                config.model = Some("gpt-5.4".to_string());
                config.model_reasoning_effort = Some(ReasoningEffort::Low);
                config.model_reasoning_summary = Some(ReasoningSummary::Concise);
                config.service_tier = None;
                config.base_instructions = Some("LIFECYCLE_MODEL_INSTRUCTIONS".to_string());
                config.base_instructions_provenance = Some(BaseInstructionsProvenance::Model {
                    model: "gpt-5.4".to_string(),
                });
                config.model_provider.base_url = Some(base_url);
                config.model_provider.supports_websockets = false;
            },
        )
        .await;
    crate::session::tests::set_base_instructions_provenance_for_test(
        session.as_ref(),
        Some(BaseInstructionsProvenance::Model {
            model: "gpt-5.4".to_string(),
        }),
    )
    .await;

    let mut step_model = session
        .services
        .models_manager
        .get_model_info(
            STEP_AUTHORITY_MODEL,
            &lifecycle_turn.config.to_models_manager_config(),
        )
        .await;
    step_model.use_responses_lite = false;
    step_model.input_modalities = vec![InputModality::Text];
    step_model.context_window = Some(64);
    step_model.effective_context_window_percent = 100;
    step_model.truncation_policy = TruncationPolicyConfig::bytes(37);
    step_model.service_tiers = vec![ModelServiceTier {
        id: ServiceTier::Fast.request_value().to_string(),
        name: "Step authority priority".to_string(),
        description: "Divergent request-step service tier".to_string(),
    }];
    let model_messages = step_model
        .model_messages
        .as_mut()
        .expect("Sol test metadata must carry model instructions");
    model_messages.instructions_template = Some(STEP_AUTHORITY_INSTRUCTIONS.to_string());
    model_messages.instructions_variables = None;

    let step_telemetry = lifecycle_turn
        .session_telemetry
        .clone()
        .with_model(STEP_AUTHORITY_MODEL, step_model.slug.as_str());
    let step_tool = ToolSpec::Function(ResponsesApiTool {
        name: STEP_AUTHORITY_TOOL.to_string(),
        description: "Only the request Step exposes this tool".to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::default(),
        output_schema: None,
    });
    let tool_router = Arc::new(ToolRouter::from_parts(
        ToolRegistry::empty_for_test(),
        vec![step_tool],
        ToolMode::Direct,
        BTreeMap::new(),
        /*tool_namespaces_info*/ None,
        &[],
    ));

    let mut request_step = StepContext::for_test(Arc::clone(&lifecycle_turn));
    let step = Arc::get_mut(&mut request_step)
        .expect("fresh divergent request Step must be uniquely owned");
    let step_settings = Arc::make_mut(&mut step.settings);
    step_settings.model_info = Arc::new(step_model);
    step_settings
        .selected_mut()
        .collaboration_mode
        .settings
        .reasoning_effort = Some(ReasoningEffort::High);
    step_settings.reasoning_summary = ReasoningSummary::Detailed;
    step_settings.service_tier = Some(ServiceTier::Fast.request_value().to_string());
    step.session_telemetry = step_telemetry;
    step.tool_router = tool_router;

    assert_ne!(
        request_step.settings.model_info.slug,
        lifecycle_turn.model_info().slug
    );
    assert_ne!(
        request_step.settings.reasoning_effort(),
        lifecycle_turn.reasoning_effort()
    );
    assert_ne!(
        request_step.settings.reasoning_summary,
        lifecycle_turn.reasoning_summary()
    );
    assert_ne!(
        request_step.settings.service_tier.as_deref(),
        lifecycle_turn.config.service_tier.as_deref()
    );
    assert_ne!(
        request_step.settings.model_info.input_modalities,
        lifecycle_turn.model_info().input_modalities
    );
    assert_ne!(
        request_step.settings.model_info.truncation_policy,
        lifecycle_turn.model_info().truncation_policy
    );
    assert!(lifecycle_turn.dynamic_tools.is_empty());
    assert_eq!(request_step.tool_router.model_visible_specs().len(), 1);
    assert_eq!(
        session.get_base_instructions().await.provenance,
        Some(BaseInstructionsProvenance::Model {
            model: lifecycle_turn.model_info().slug.clone(),
        })
    );

    DivergentCompactionFixture {
        session,
        lifecycle_turn,
        request_step,
    }
}

async fn seed_divergent_compaction_history(fixture: &DivergentCompactionFixture) {
    let oversized_output = format!("{STEP_AUTHORITY_OUTPUT}:{}", "x".repeat(2_000));
    let items = vec![
        ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![
                ContentItem::InputText {
                    text: STEP_AUTHORITY_HISTORY_TEXT.to_string(),
                },
                ContentItem::InputImage {
                    image_url: STEP_AUTHORITY_IMAGE_URL.to_string(),
                    detail: Some(DEFAULT_IMAGE_DETAIL),
                },
            ],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        },
        ResponseItem::FunctionCall {
            id: None,
            name: STEP_AUTHORITY_TOOL.to_string(),
            namespace: None,
            arguments: "{}".to_string(),
            call_id: "step-authority-call".to_string(),
            encrypted_function_args: None,
            internal_chat_message_metadata_passthrough: None,
        },
        ResponseItem::FunctionCallOutput {
            id: None,
            call_id: Some("step-authority-call".to_string()),
            name: None,
            namespace: None,
            output: FunctionCallOutputPayload {
                body: FunctionCallOutputBody::Text(oversized_output),
                success: Some(true),
            },
            internal_chat_message_metadata_passthrough: None,
        },
    ];
    fixture
        .session
        .record_conversation_items(
            fixture.lifecycle_turn.as_ref(),
            fixture.lifecycle_turn.model_info(),
            &items,
        )
        .await;
}

fn assert_divergent_step_request(
    body: &Value,
    expected_tools: &[&str],
    expected_service_tier: Option<&str>,
) {
    assert_eq!(
        json!({
            "model": body.get("model"),
            "effort": body.pointer("/reasoning/effort"),
            "summary": body.pointer("/reasoning/summary"),
            "service_tier": body.get("service_tier"),
            "instructions": body.get("instructions"),
        }),
        json!({
            "model": STEP_AUTHORITY_MODEL,
            "effort": "high",
            "summary": "detailed",
            "service_tier": expected_service_tier,
            "instructions": STEP_AUTHORITY_INSTRUCTIONS,
        })
    );
    let tool_names = body["tools"]
        .as_array()
        .expect("request tools must be an array")
        .iter()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str))
        .collect::<Vec<_>>();
    assert_eq!(tool_names, expected_tools);

    let serialized = body.to_string();
    assert!(serialized.contains(STEP_AUTHORITY_HISTORY_TEXT));
    assert!(
        !serialized.contains("\"type\":\"input_image\""),
        "the text-only request Step must strip lifecycle history images"
    );
}

fn assert_lifecycle_primary_request(body: &Value) {
    assert_eq!(body["model"].as_str(), Some("gpt-5.4"));
    assert_eq!(body["reasoning"]["effort"].as_str(), Some("low"));
    assert_eq!(body["reasoning"]["summary"].as_str(), Some("concise"));
    assert_eq!(body.get("service_tier").and_then(Value::as_str), None);
    assert_eq!(
        body["instructions"].as_str(),
        Some("LIFECYCLE_MODEL_INSTRUCTIONS")
    );
    assert_eq!(body["tools"], json!([]));
    let serialized = body.to_string();
    assert!(
        serialized.contains("\"type\":\"input_image\""),
        "the image-capable lifecycle Step must preserve an input-image item"
    );
    assert!(serialized.contains(STEP_AUTHORITY_OUTPUT));
    assert!(!serialized.contains(STEP_AUTHORITY_TRUNCATED_OUTPUT));
}

fn invalid_compaction_request(message: &str) -> wiremock::ResponseTemplate {
    wiremock::ResponseTemplate::new(/*status*/ 400).set_body_json(json!({
        "detail": message,
    }))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_compaction_request_uses_divergent_step_authority() {
    skip_if_no_network!();

    let server = start_mock_server().await;
    let response_mock = mount_sse_once(
        &server,
        sse(vec![
            ev_assistant_message("step-authority-summary", "summary"),
            ev_completed("step-authority-response"),
        ]),
    )
    .await;
    let fixture = divergent_compaction_fixture(format!("{}/v1", server.uri())).await;
    seed_divergent_compaction_history(&fixture).await;

    let attempt = CompactionAttemptContext::prepare(
        &fixture.session,
        &fixture.request_step,
        /*lane*/ None,
        &CancellationToken::new(),
    )
    .await
    .expect("unmanaged local compaction must preserve the source Step");
    assert!(Arc::ptr_eq(&attempt.request_step, &fixture.request_step));
    assert_eq!(
        attempt.request_step.settings.model_info.truncation_policy,
        TruncationPolicyConfig::bytes(37)
    );

    run_compact_task_inner(
        Arc::clone(&fixture.session),
        Arc::clone(&fixture.request_step),
        vec![UserInput::Text {
            text: "compact under the divergent Step".to_string(),
            text_elements: Vec::new(),
        }],
        InitialContextInjection::DoNotInject,
        /*lane*/ None,
        &CancellationToken::new(),
        CompactionTrigger::Manual,
        CompactionReason::UserRequested,
        CompactionPhase::StandaloneTurn,
    )
    .await
    .expect("local compaction should complete");

    let body = response_mock.single_request().body_json();
    assert_divergent_step_request(&body, &[], Some(ServiceTier::Fast.request_value()));
    assert!(
        body.to_string().contains(STEP_AUTHORITY_OUTPUT),
        "local compaction does not run remote context-window output rewriting"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_v2_compaction_request_uses_divergent_step_authority() {
    skip_if_no_network!();

    let server = start_mock_server().await;
    let response_mock = mount_sse_once(
        &server,
        sse(vec![
            json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "compaction",
                    "encrypted_content": "remote-v2-encrypted-summary",
                }
            }),
            ev_completed("remote-v2-step-authority"),
        ]),
    )
    .await;
    let fixture = divergent_compaction_fixture(format!("{}/v1", server.uri())).await;
    seed_divergent_compaction_history(&fixture).await;

    crate::compact_remote_v2::run_remote_compact_task(
        Arc::clone(&fixture.session),
        Arc::clone(&fixture.request_step),
        &CancellationToken::new(),
    )
    .await
    .expect("remote v2 compaction should complete");

    let body = response_mock.single_request().body_json();
    assert_divergent_step_request(
        &body,
        &[STEP_AUTHORITY_TOOL],
        Some(ServiceTier::Fast.request_value()),
    );
    let serialized = body.to_string();
    assert!(serialized.contains(STEP_AUTHORITY_TRUNCATED_OUTPUT));
    assert!(!serialized.contains(STEP_AUTHORITY_OUTPUT));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_v2_fallback_request_uses_its_own_divergent_step_authority() {
    skip_if_no_network!();

    let server = start_mock_server().await;
    let response_mock = mount_response_sequence(
        &server,
        vec![
            invalid_compaction_request("primary model compaction was rejected"),
            sse_response(sse(vec![
                json!({
                    "type": "response.output_item.done",
                    "item": {
                        "type": "compaction",
                        "encrypted_content": "remote-v2-fallback-summary",
                    }
                }),
                ev_completed("remote-v2-fallback-response"),
            ])),
        ],
    )
    .await;
    let fixture = divergent_compaction_fixture(format!("{}/v1", server.uri())).await;
    seed_divergent_compaction_history(&fixture).await;
    let primary_step = StepContext::for_test(Arc::clone(&fixture.lifecycle_turn));
    assert!(
        primary_step
            .settings
            .model_info
            .resolved_context_window()
            .is_some_and(|window| window > 64)
    );
    assert_eq!(
        fixture
            .request_step
            .settings
            .model_info
            .resolved_context_window(),
        Some(64)
    );
    let mut client_session = fixture.session.services.model_client.new_session();

    crate::compact_remote_v2::run_inline_remote_auto_compact_task(
        Arc::clone(&fixture.session),
        primary_step,
        Some(Arc::clone(&fixture.request_step)),
        &CancellationToken::new(),
        &mut client_session,
        InitialContextInjection::DoNotInject,
        CompactionReason::ModelDownshift,
        CompactionPhase::PreTurn,
    )
    .await
    .expect("remote v2 must retry with the independently captured fallback Step");

    let requests = response_mock.requests();
    assert_eq!(requests.len(), 2);
    assert_lifecycle_primary_request(&requests[0].body_json());
    let fallback = requests[1].body_json();
    assert_divergent_step_request(
        &fallback,
        &[STEP_AUTHORITY_TOOL],
        Some(ServiceTier::Fast.request_value()),
    );
    let serialized = fallback.to_string();
    assert!(serialized.contains(STEP_AUTHORITY_TRUNCATED_OUTPUT));
    assert!(!serialized.contains(STEP_AUTHORITY_OUTPUT));
}

#[tokio::test]
#[serial_test::serial]
async fn locked_model_policy_managed_local_compaction_materializes_actual_lane_without_network()
-> anyhow::Result<()> {
    let Some(lane) = locked_model_policy_lane()? else {
        return Ok(());
    };
    let auth = match lane {
        ModelPolicyLane::Api => CodexAuth::from_api_key("Test API Key"),
        ModelPolicyLane::Subscription | ModelPolicyLane::Spark => {
            CodexAuth::from_external_chatgpt_tokens(
                "header.e30.signature",
                "test-workspace",
                Some("pro"),
            )?
        }
    };
    let lane_env = crate::session::tests::ModelPolicyLaneEnvGuard::unset();
    let (session, _initial_turn_context, _rx) =
        crate::session::tests::make_session_and_context_with_auth_and_config_and_rx(
            auth,
            Vec::new(),
            |config| {
                config.agents_enabled = lane.multi_agent_enabled();
                if lane.multi_agent_enabled() {
                    config
                        .features
                        .enable(Feature::MultiAgentV2)
                        .expect("managed API and subscription test lanes allow multi-agent v2");
                }
            },
        )
        .await;
    let original = session.collaboration_mode().await;
    let (root_model, root_effort, root_tier) = match lane {
        ModelPolicyLane::Api | ModelPolicyLane::Subscription => (
            "gpt-5.5".to_string(),
            ReasoningEffort::High,
            Some(ServiceTier::Fast.request_value().to_string()),
        ),
        ModelPolicyLane::Spark => (
            "gpt-5.3-codex-spark".to_string(),
            ReasoningEffort::XHigh,
            None,
        ),
    };
    let (root_turn, _) = session
        .new_turn_with_sub_id(
            format!("{}-priority-compact-root", lane.as_str()),
            crate::session::SessionSettingsUpdate {
                step_settings: StepSettingsUpdate {
                    collaboration_mode: Some(original.with_updates(
                        Some(root_model.clone()),
                        Some(Some(root_effort.clone())),
                        /*developer_instructions*/ None,
                    )),
                    service_tier: root_tier.clone().map(Some),
                    ..Default::default()
                },
                ..Default::default()
            },
            NewTurnContextOptions::default(),
        )
        .await?;
    assert_eq!(root_turn.model_info().slug, root_model);
    assert_eq!(root_turn.reasoning_effort().cloned(), Some(root_effort));
    assert_eq!(root_turn.config.service_tier, root_tier);
    lane_env.restore();
    assert_eq!(locked_model_policy_lane()?, Some(lane));

    let cancellation_token = CancellationToken::new();
    let source_step = session
        .capture_step_context(Arc::clone(&root_turn), &cancellation_token)
        .await?;
    let attempt =
        CompactionAttemptContext::prepare(&session, &source_step, Some(lane), &cancellation_token)
            .await?;
    let request_step = attempt.request_step;
    request_step.validate_managed_background(lane)?;
    assert_eq!(request_step.settings.model_info.slug, "gpt-5.6-sol");
    assert_eq!(request_step.turn.model_info().slug, "gpt-5.6-sol");
    assert_eq!(
        request_step.turn.config.model.as_deref(),
        Some("gpt-5.6-sol")
    );
    assert_eq!(
        request_step.settings.reasoning_effort().cloned(),
        Some(ReasoningEffort::Ultra)
    );
    assert_eq!(
        request_step.settings.service_tier.as_deref(),
        Some(SERVICE_TIER_DEFAULT_REQUEST_VALUE)
    );

    let base_instructions = managed_background_base_instructions_for_model(
        session.get_prompt_base_instructions().await,
        &request_step.settings.model_info,
        request_step.turn.personality(),
        /*omit_update_plan_instructions*/
        !request_step.turn.config.update_plan_enabled
            && request_step.turn.config.model_catalog.is_none(),
    );
    let prompt = Prompt {
        base_instructions: base_instructions.clone(),
        ..Default::default()
    };
    let responses_metadata = session
        .responses_metadata(
            request_step.turn.as_ref(),
            CodexResponsesRequestKind::Compaction(CompactionTurnMetadata::new(
                CompactionTrigger::Manual,
                CompactionReason::UserRequested,
                CompactionImplementation::Responses,
                CompactionPhase::StandaloneTurn,
            )),
        )
        .await;
    assert_eq!(
        responses_metadata.context_window_id,
        Some(session.current_window().await.2),
        "managed compaction must carry the active context-window identity"
    );
    let request = session
        .services
        .model_client
        .materialize_responses_request_for_test(
            &prompt,
            request_step.as_ref(),
            &responses_metadata,
        )?;
    let wire = serde_json::to_value(&request)?;
    let expected_mode = match lane {
        ModelPolicyLane::Api => Some("pro"),
        ModelPolicyLane::Subscription | ModelPolicyLane::Spark => None,
    };
    let expected_tier = match lane {
        ModelPolicyLane::Api => Some(SERVICE_TIER_DEFAULT_REQUEST_VALUE),
        ModelPolicyLane::Subscription | ModelPolicyLane::Spark => None,
    };
    assert_eq!(
        (
            wire["model"].as_str(),
            wire["reasoning"]["mode"].as_str(),
            wire["reasoning"]["effort"].as_str(),
            wire.get("service_tier").and_then(Value::as_str),
        ),
        (
            Some("gpt-5.6-sol"),
            expected_mode,
            Some("max"),
            expected_tier
        ),
        "the materialized compaction request must follow the actual locked process lane"
    );
    let serialized_instructions = if request_step.settings.model_info.use_responses_lite {
        wire["input"]
            .as_array()
            .and_then(|input| {
                input
                    .iter()
                    .find(|item| item["type"] == "message" && item["role"] == "developer")
            })
            .and_then(|item| item["content"][0]["text"].as_str())
    } else {
        wire["instructions"].as_str()
    };
    assert_eq!(
        serialized_instructions,
        Some(base_instructions.text.as_str())
    );
    assert!(
        wire.get("tools").is_none_or(|tools| tools == &json!([])),
        "local compaction must omit model-visible tools"
    );
    Ok(())
}

fn annotated(items: Vec<ResponseItem>) -> Vec<ResponseItemEnvelope> {
    items.into_iter().map(ResponseItemEnvelope::new).collect()
}

fn raw(items: Vec<ResponseItemEnvelope>) -> Vec<ResponseItem> {
    items
        .into_iter()
        .map(ResponseItemEnvelope::into_item)
        .collect()
}

fn user_message(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }
}

fn compacted_user_message(text: &str) -> CompactedUserMessage {
    CompactedUserMessage {
        id: None,
        message: text.to_string(),
        internal_chat_message_metadata_passthrough: None,
        harness_metadata: None,
    }
}

#[test]
fn content_items_to_text_joins_non_empty_segments() {
    let items = vec![
        ContentItem::InputText {
            text: "hello".to_string(),
        },
        ContentItem::OutputText {
            text: String::new(),
        },
        ContentItem::OutputText {
            text: "world".to_string(),
        },
    ];

    let joined = content_items_to_text(&items);

    assert_eq!(Some("hello\nworld".to_string()), joined);
}

#[test]
fn content_items_to_text_ignores_image_only_content() {
    let items = vec![ContentItem::InputImage {
        image_url: "file://image.png".to_string(),
        detail: Some(DEFAULT_IMAGE_DETAIL),
    }];

    let joined = content_items_to_text(&items);

    assert_eq!(None, joined);
}

#[test]
fn collect_user_messages_extracts_user_text_only() {
    let items = vec![
        ResponseItem::Message {
            id: Some(ResponseItemId::with_suffix("msg", "assistant")),
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: "ignored".to_string(),
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        },
        ResponseItem::Message {
            id: Some(ResponseItemId::with_suffix("msg", "user")),
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "first".to_string(),
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        },
        ResponseItem::Other,
    ];

    let collected = collect_user_messages(&items);

    assert_eq!(
        vec![CompactedUserMessage {
            id: Some(ResponseItemId::with_suffix("msg", "user")),
            ..compacted_user_message("first")
        }],
        collected,
    );
}

#[test]
fn collect_annotated_user_messages_extracts_user_text_only() {
    let items = vec![
        ResponseItemEnvelope {
            item: user_message("first"),
            metadata: Some(CodexHarnessMetadata::default()),
        },
        ResponseItemEnvelope::new(ResponseItem::Other),
    ];

    let collected = collect_annotated_user_messages(&items, CompactedMessageIdentity::Preserve);

    assert_eq!(
        vec![CompactedUserMessage {
            id: None,
            message: "first".to_string(),
            internal_chat_message_metadata_passthrough: None,
            harness_metadata: Some(CodexHarnessMetadata::default()),
        }],
        collected
    );
}

#[test]
fn collect_user_messages_filters_session_prefix_entries() {
    let items = vec![
        ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: r#"# AGENTS.md instructions for project

<INSTRUCTIONS>
do things
</INSTRUCTIONS>"#
                    .to_string(),
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        },
        ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "<ENVIRONMENT_CONTEXT>cwd=/tmp</ENVIRONMENT_CONTEXT>".to_string(),
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        },
        ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "real user message".to_string(),
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        },
    ];

    let collected = collect_user_messages(&items);

    assert_eq!(vec![compacted_user_message("real user message")], collected);
}

#[test]
fn collect_user_messages_filters_legacy_warnings() {
    let items = vec![
        user_message(
            "Warning: The maximum number of unified exec processes you can keep open is 60 and you currently have 61 processes open. Reuse older processes or close them to prevent automatic pruning of old processes",
        ),
        user_message(
            "Warning: apply_patch was requested via exec_command. Use the apply_patch tool instead of exec_command.",
        ),
        user_message(
            "Warning: Your account was flagged for potentially high-risk cyber activity and this request was routed to gpt-5.2 as a fallback. To regain access to gpt-5.3-codex, apply for trusted access: https://chatgpt.com/cyber or learn more: https://developers.openai.com/codex/concepts/cyber-safety",
        ),
        user_message("real user message"),
    ];

    let collected = collect_user_messages(&items);

    assert_eq!(vec![compacted_user_message("real user message")], collected);
}

#[test]
fn build_token_limited_compacted_history_truncates_overlong_user_messages() {
    // Use a small truncation limit so the test remains fast while still validating
    // that oversized user content is truncated.
    let max_tokens = 16;
    let big = "word ".repeat(200);
    let user_message = CompactedUserMessage {
        id: Some(ResponseItemId::with_suffix("msg", "long-user")),
        message: big.clone(),
        internal_chat_message_metadata_passthrough: None,
        harness_metadata: Some(CodexHarnessMetadata::default()),
    };
    let history = super::build_compacted_history_with_limit(
        Vec::new(),
        std::slice::from_ref(&user_message),
        "SUMMARY",
        max_tokens,
    );
    assert_eq!(history.len(), 2);

    let truncated_message = &history[0].item;
    let summary_message = &history[1].item;

    let truncated_text = match truncated_message {
        ResponseItem::Message { role, content, .. } if role == "user" => {
            content_items_to_text(content).unwrap_or_default()
        }
        other => panic!("unexpected item in history: {other:?}"),
    };

    assert!(
        truncated_text.contains("tokens truncated"),
        "expected truncation marker in truncated user message"
    );
    assert!(
        !truncated_text.contains(&big),
        "truncated user message should not include the full oversized user text"
    );

    let summary_text = match summary_message {
        ResponseItem::Message { role, content, .. } if role == "user" => {
            content_items_to_text(content).unwrap_or_default()
        }
        other => panic!("unexpected item in history: {other:?}"),
    };
    assert_eq!(summary_text, "SUMMARY");
    assert_eq!(history[0].id(), user_message.id.as_ref());
    assert_eq!(history[0].metadata, Some(CodexHarnessMetadata::default()));
    assert_eq!(history[1].metadata, None);
}

#[test]
fn build_token_limited_compacted_history_appends_summary_message() {
    let initial_context: Vec<ResponseItemEnvelope> = Vec::new();
    let user_messages = vec![compacted_user_message("first user message")];
    let summary_text = "summary text";

    let history = build_compacted_history(initial_context, &user_messages, summary_text);
    assert!(
        !history.is_empty(),
        "expected compacted history to include summary"
    );

    let last = history.last().expect("history should have a summary entry");
    let summary = match &last.item {
        ResponseItem::Message { role, content, .. } if role == "user" => {
            content_items_to_text(content).unwrap_or_default()
        }
        other => panic!("expected summary message, found {other:?}"),
    };
    assert_eq!(summary, summary_text);
}

#[test]
fn build_compacted_history_preserves_user_message_passthrough_metadata() {
    let history = build_compacted_history(
        Vec::new(),
        &[CompactedUserMessage {
            id: Some(ResponseItemId::with_suffix("msg", "user")),
            message: "first user message".to_string(),
            internal_chat_message_metadata_passthrough: Some(
                InternalChatMessageMetadataPassthrough {
                    turn_id: Some("turn-1".to_string()),
                    content_item_kinds: Some(vec![
                        ContentItemKind("user.image".to_string()),
                        ContentItemKind("user.text".to_string()),
                        ContentItemKind("user.audio".to_string()),
                    ]),
                    ..Default::default()
                },
            ),
            harness_metadata: Some(CodexHarnessMetadata::default()),
        }],
        "summary text",
    );

    assert_eq!(
        history,
        vec![
            ResponseItemEnvelope {
                item: ResponseItem::Message {
                    id: Some(ResponseItemId::with_suffix("msg", "user")),
                    role: "user".to_string(),
                    content: vec![ContentItem::InputText {
                        text: "first user message".to_string(),
                    }],
                    phase: None,
                    internal_chat_message_metadata_passthrough: Some(
                        InternalChatMessageMetadataPassthrough {
                            turn_id: Some("turn-1".to_string()),
                            content_item_kinds: Some(vec![ContentItemKind(
                                "user.text".to_string()
                            )]),
                            ..Default::default()
                        },
                    ),
                },
                metadata: Some(CodexHarnessMetadata::default()),
            },
            ResponseItemEnvelope::new(ContextualUserFragment::into(CompactionSummary::new(
                "summary text",
            ))),
        ]
    );
}

#[test]
fn insert_initial_context_before_last_real_user_or_summary_keeps_summary_last() {
    let agent_completion = ResponseItem::AgentMessage {
        id: None,
        author: "child".to_string(),
        recipient: "parent".to_string(),
        content: vec![AgentMessageInputContent::InputText {
            text: "Message Type: FINAL_ANSWER\nPayload:\nchild completion".to_string(),
        }],
        internal_chat_message_metadata_passthrough: None,
    };
    let compacted_history = vec![
        ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "older user".to_string(),
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        },
        ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "latest user".to_string(),
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        },
        agent_completion.clone(),
        ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: format!("{SUMMARY_PREFIX}\nsummary text"),
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        },
    ];
    let initial_context = vec![ResponseItem::Message {
        id: None,
        role: "developer".to_string(),
        content: vec![ContentItem::InputText {
            text: "fresh permissions".to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }];

    let refreshed = raw(insert_initial_context_before_last_real_user_or_summary(
        annotated(compacted_history),
        annotated(initial_context),
    ));
    let expected = vec![
        ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "older user".to_string(),
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        },
        ResponseItem::Message {
            id: None,
            role: "developer".to_string(),
            content: vec![ContentItem::InputText {
                text: "fresh permissions".to_string(),
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        },
        ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "latest user".to_string(),
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        },
        agent_completion,
        ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: format!("{SUMMARY_PREFIX}\nsummary text"),
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        },
    ];
    assert_eq!(refreshed, expected);
}

#[test]
fn insert_initial_context_before_last_real_user_or_summary_keeps_compaction_last() {
    let agent_task = ResponseItem::AgentMessage {
        id: None,
        author: "parent".to_string(),
        recipient: "child".to_string(),
        content: Vec::new(),
        internal_chat_message_metadata_passthrough: None,
    };
    let compacted_history = vec![
        agent_task.clone(),
        ResponseItem::Compaction {
            id: None,
            encrypted_content: "encrypted".to_string(),
            internal_chat_message_metadata_passthrough: None,
        },
    ];
    let initial_context = vec![ResponseItem::Message {
        id: None,
        role: "developer".to_string(),
        content: vec![ContentItem::InputText {
            text: "fresh permissions".to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }];

    let refreshed = raw(insert_initial_context_before_last_real_user_or_summary(
        annotated(compacted_history),
        annotated(initial_context),
    ));
    let expected = vec![
        ResponseItem::Message {
            id: None,
            role: "developer".to_string(),
            content: vec![ContentItem::InputText {
                text: "fresh permissions".to_string(),
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        },
        agent_task,
        ResponseItem::Compaction {
            id: None,
            encrypted_content: "encrypted".to_string(),
            internal_chat_message_metadata_passthrough: None,
        },
    ];
    assert_eq!(refreshed, expected);
}
