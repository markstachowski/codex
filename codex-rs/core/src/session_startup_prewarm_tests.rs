use super::*;
use anyhow::Result;
use codex_config::Constrained;
use codex_features::Feature;
use codex_login::CodexAuth;
use codex_protocol::config_types::ApprovalsReviewer;
use codex_protocol::config_types::ReasoningSummary;
use codex_protocol::config_types::SERVICE_TIER_DEFAULT_REQUEST_VALUE;
use codex_protocol::config_types::ServiceTier;
use codex_protocol::models::BaseInstructions;
use codex_protocol::models::BaseInstructionsProvenance;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::protocol::AskForApproval;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::start_websocket_server;
use core_test_support::skip_if_no_network;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn managed_startup_prewarm_captures_one_coherent_sol_snapshot_for_every_lane() -> Result<()> {
    for lane in [
        ModelPolicyLane::Api,
        ModelPolicyLane::Subscription,
        ModelPolicyLane::Spark,
    ] {
        let (session, _initial_turn_context, _rx) =
            crate::session::tests::make_session_and_context_with_auth_and_config_and_rx(
                CodexAuth::from_api_key("Test API Key"),
                Vec::new(),
                |config| {
                    config.model_reasoning_summary = Some(ReasoningSummary::Detailed);
                    config.permissions.approval_policy =
                        Constrained::allow_any(AskForApproval::Never);
                    config.approvals_reviewer = ApprovalsReviewer::AutoReview;
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
        let root_turn = session
            .new_turn_with_sub_id(
                format!("{}-root", lane.as_str()),
                crate::session::SessionSettingsUpdate {
                    collaboration_mode: Some(original.with_updates(
                        Some(root_model.clone()),
                        Some(Some(root_effort.clone())),
                        /*developer_instructions*/ None,
                    )),
                    reasoning_summary: Some(ReasoningSummary::Detailed),
                    service_tier: root_tier.map(Some),
                    ..Default::default()
                },
            )
            .await?;
        let root_metadata = session
            .services
            .thread_extension_data
            .get::<ModelInfo>()
            .expect("ordinary root turn must publish model metadata");
        assert_eq!(root_metadata.as_ref(), root_turn.model_info().as_ref());

        let startup_turns = session
            .new_startup_prewarm_turn_contexts_with_sub_id(
                format!("{}-managed-prewarm", lane.as_str()),
                Some(lane),
            )
            .await;
        let guardian_parent_turn = startup_turns.guardian_parent;
        let managed_turn = startup_turns.request;
        assert_eq!(
            guardian_parent_turn.model_info().slug.as_str(),
            root_model.as_str(),
            "Guardian must retain the interactive root turn while managed prewarm uses Sol"
        );
        let managed_step = session
            .capture_step_context(Arc::clone(&managed_turn), &CancellationToken::new())
            .await?;
        managed_step
            .validate_managed_background(lane)
            .expect("startup prewarm must be captured from one coherent managed turn");
        assert!(
            !managed_step.tool_router.model_visible_specs().is_empty(),
            "{} managed startup prewarm must construct model-visible tools from the Sol turn",
            lane.as_str()
        );
        assert_eq!(
            managed_step.turn.multi_agent_version,
            lane.required_multi_agent_version(),
            "managed Sol prewarm must retain the lane's reviewed agent capability boundary"
        );

        assert_eq!(
            (
                managed_step.settings.model_info.slug.as_str(),
                managed_step.turn.model_info().slug.as_str(),
                managed_step.turn.config.model.as_deref(),
            ),
            ("gpt-5.6-sol", "gpt-5.6-sol", Some("gpt-5.6-sol"),),
            "{} must not retain any root-model authority in the managed snapshot",
            lane.as_str()
        );
        assert_eq!(
            (
                managed_step.settings.reasoning_effort().cloned(),
                managed_step.turn.reasoning_effort().cloned(),
                managed_step.turn.config.model_reasoning_effort.clone(),
            ),
            (
                Some(ReasoningEffort::Ultra),
                Some(ReasoningEffort::Ultra),
                Some(ReasoningEffort::Ultra),
            )
        );
        assert_eq!(
            (
                managed_step.settings.service_tier.as_deref(),
                managed_step.turn.config.service_tier.as_deref(),
            ),
            (
                Some(SERVICE_TIER_DEFAULT_REQUEST_VALUE),
                Some(SERVICE_TIER_DEFAULT_REQUEST_VALUE),
            )
        );
        assert_eq!(
            (
                managed_step.settings.reasoning_summary,
                managed_step.turn.reasoning_summary(),
                managed_step.turn.config.model_reasoning_summary,
            ),
            (
                ReasoningSummary::Detailed,
                ReasoningSummary::Detailed,
                Some(ReasoningSummary::Detailed),
            )
        );
        assert_eq!(
            (
                managed_step.settings.approval_policy(),
                managed_step.turn.approval_policy(),
                managed_step.settings.approvals_reviewer(),
                managed_step.turn.config.approvals_reviewer,
            ),
            (
                AskForApproval::Never,
                AskForApproval::Never,
                ApprovalsReviewer::AutoReview,
                ApprovalsReviewer::AutoReview,
            )
        );

        let after_prewarm_metadata = session
            .services
            .thread_extension_data
            .get::<ModelInfo>()
            .expect("managed prewarm must preserve root model metadata");
        assert_eq!(after_prewarm_metadata.as_ref(), root_metadata.as_ref());

        let root_derived_instructions = BaseInstructions {
            text: root_turn
                .model_info()
                .get_model_instructions(root_turn.personality()),
            provenance: Some(BaseInstructionsProvenance::Model {
                model: root_turn.model_info().slug.clone(),
            }),
        };
        assert_eq!(
            managed_background_base_instructions_for_model(
                root_derived_instructions,
                &managed_step.settings.model_info,
                managed_step.turn.personality(),
            ),
            BaseInstructions {
                text: managed_step
                    .settings
                    .model_info
                    .get_model_instructions(managed_step.turn.personality()),
                provenance: Some(BaseInstructionsProvenance::Model {
                    model: "gpt-5.6-sol".to_string(),
                }),
            }
        );
        let custom_instructions = BaseInstructions {
            text: "custom instructions must survive a managed model switch".to_string(),
            provenance: Some(BaseInstructionsProvenance::Custom),
        };
        assert_eq!(
            managed_background_base_instructions_for_model(
                custom_instructions.clone(),
                &managed_step.settings.model_info,
                managed_step.turn.personality(),
            ),
            custom_instructions
        );
    }

    Ok(())
}

#[tokio::test]
async fn managed_startup_prewarm_seeds_missing_root_metadata_and_preserves_resolved_summary() {
    let (session, _initial_turn_context, _rx) =
        crate::session::tests::make_session_and_context_with_auth_and_config_and_rx(
            CodexAuth::from_api_key("Test API Key"),
            Vec::new(),
            |config| {
                config.model = Some("gpt-5.2".to_string());
                config.model_reasoning_summary = None;
            },
        )
        .await;
    assert_eq!(
        session.services.thread_extension_data.get::<ModelInfo>(),
        None,
        "the fixture must exercise the absent-root-metadata path"
    );

    let managed_turn = session
        .new_startup_prewarm_turn_with_sub_id(
            "managed-prewarm-default-summary".to_string(),
            Some(ModelPolicyLane::Subscription),
        )
        .await;

    assert_eq!(managed_turn.model_info().slug, "gpt-5.6-sol");
    assert_eq!(managed_turn.reasoning_summary(), ReasoningSummary::Auto);
    assert_eq!(
        managed_turn.config.model_reasoning_summary,
        Some(ReasoningSummary::Auto)
    );
    assert_eq!(
        managed_turn.model_info().default_reasoning_summary,
        ReasoningSummary::None,
        "the preserved Auto value must come from the root snapshot, not Sol defaults"
    );
    let root_metadata = session
        .services
        .thread_extension_data
        .get::<ModelInfo>()
        .expect("startup prewarm must seed missing interactive root metadata");
    assert_eq!(root_metadata.slug, "gpt-5.2");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn locked_model_policy_managed_startup_prewarm_does_not_inherit_root() -> Result<()> {
    skip_if_no_network!(Ok(()));

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
    let response_id = format!("{}-managed-warmup", lane.as_str());
    let server = start_websocket_server(vec![vec![vec![
        ev_response_created(&response_id),
        ev_completed(&response_id),
    ]]])
    .await;
    let base_url = format!("{}/v1", server.uri());
    let (session, _initial_turn_context, _rx) =
        crate::session::tests::make_session_and_context_with_auth_and_config_and_rx(
            auth,
            Vec::new(),
            move |config| {
                config.model_provider.base_url = Some(base_url);
                config.model_provider.supports_websockets = true;
                config
                    .features
                    .enable(Feature::ResponsesWebsocketsV2)
                    .expect("test config should allow WebSocket v2");
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
    let root_turn = session
        .new_turn_with_sub_id(
            format!("{}-priority-root", lane.as_str()),
            crate::session::SessionSettingsUpdate {
                collaboration_mode: Some(original.with_updates(
                    Some(root_model.clone()),
                    Some(Some(root_effort.clone())),
                    /*developer_instructions*/ None,
                )),
                service_tier: root_tier.clone().map(Some),
                ..Default::default()
            },
        )
        .await?;
    assert_eq!(root_turn.model_info().slug, root_model);
    assert_eq!(root_turn.reasoning_effort(), Some(&root_effort));
    assert_eq!(root_turn.config.service_tier, root_tier);

    let sol_model_info = session
        .services
        .models_manager
        .get_model_info(
            lane.required_background_model(),
            &root_turn.config.to_models_manager_config(),
        )
        .await;
    let expected_instructions = sol_model_info.get_model_instructions(root_turn.personality());
    let base_instructions = BaseInstructions {
        text: root_turn
            .model_info()
            .get_model_instructions(root_turn.personality()),
        provenance: Some(BaseInstructionsProvenance::Model {
            model: root_turn.model_info().slug.clone(),
        }),
    };
    let _managed_client_session =
        schedule_startup_prewarm_inner(Arc::clone(&session), base_instructions, Some(lane)).await?;
    let managed = server
        .wait_for_request(/*connection_index*/ 0, /*request_index*/ 0)
        .await
        .body_json();
    let managed_metadata: serde_json::Value = serde_json::from_str(
        managed["client_metadata"]["x-codex-turn-metadata"]
            .as_str()
            .expect("managed prewarm turn metadata"),
    )?;
    let expected_context_window_id = session.current_window().await.1.to_string();

    assert_eq!(managed["generate"].as_bool(), Some(false));
    assert_eq!(managed["model"].as_str(), Some("gpt-5.6-sol"));
    assert_eq!(managed["reasoning"]["effort"].as_str(), Some("max"));
    assert_eq!(managed_metadata["request_kind"].as_str(), Some("prewarm"));
    assert_eq!(
        managed_metadata["context_window_id"].as_str(),
        Some(expected_context_window_id.as_str()),
        "managed prewarm must carry the active context-window identity"
    );
    assert_eq!(
        managed["reasoning"]
            .get("mode")
            .and_then(|mode| mode.as_str()),
        match lane {
            ModelPolicyLane::Api => Some("pro"),
            ModelPolicyLane::Subscription | ModelPolicyLane::Spark => None,
        }
    );
    assert_eq!(
        managed["parallel_tool_calls"].as_bool(),
        Some(!sol_model_info.use_responses_lite)
    );
    let serialized_instructions = if sol_model_info.use_responses_lite {
        managed["input"]
            .as_array()
            .and_then(|input| {
                input
                    .iter()
                    .find(|item| item["type"] == "message" && item["role"] == "developer")
            })
            .and_then(|item| item["content"][0]["text"].as_str())
    } else {
        managed["instructions"].as_str()
    };
    assert_eq!(
        serialized_instructions,
        Some(expected_instructions.as_str())
    );
    assert_eq!(
        managed.get("service_tier").and_then(|tier| tier.as_str()),
        match lane {
            ModelPolicyLane::Api => Some(SERVICE_TIER_DEFAULT_REQUEST_VALUE),
            ModelPolicyLane::Subscription | ModelPolicyLane::Spark => None,
        },
        "wire tier policy must come from the actual locked lane"
    );

    server.shutdown().await;

    Ok(())
}
