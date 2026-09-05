use super::*;
use crate::session::step_settings::StepSettingsUpdate;
use crate::session::turn_context::NewTurnContextOptions;
use anyhow::Result;
use codex_api::ResponseCreateWsRequest;
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
use pretty_assertions::assert_eq;

#[tokio::test]
async fn managed_startup_prewarm_captures_one_coherent_astra_snapshot_for_every_lane() -> Result<()>
{
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
            ModelPolicyLane::Subscription => (
                "gpt-5.5".to_string(),
                ReasoningEffort::High,
                Some(ServiceTier::Fast.request_value().to_string()),
            ),
            ModelPolicyLane::Api => (
                "gpt-5.6-terra".to_string(),
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
                format!("{}-root", lane.as_str()),
                crate::session::SessionSettingsUpdate {
                    step_settings: StepSettingsUpdate {
                        collaboration_mode: Some(original.with_updates(
                            Some(root_model.clone()),
                            Some(Some(root_effort.clone())),
                            /*developer_instructions*/ None,
                        )),
                        reasoning_summary: Some(ReasoningSummary::Detailed),
                        service_tier: root_tier.clone().map(Some),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                NewTurnContextOptions::default(),
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
            managed_turn.environments.to_selections(),
            guardian_parent_turn.environments.to_selections(),
            "startup Guardian parent and managed request must share one environment snapshot"
        );
        assert_eq!(managed_turn.network, guardian_parent_turn.network);
        assert_eq!(
            managed_turn.config.workspace_roots,
            guardian_parent_turn.config.workspace_roots
        );
        assert_eq!(
            guardian_parent_turn.model_info().slug.as_str(),
            root_model.as_str(),
            "Guardian must retain the interactive root turn while managed prewarm uses Astra"
        );
        assert_eq!(guardian_parent_turn.reasoning_effort(), Some(&root_effort));
        assert_eq!(guardian_parent_turn.config.service_tier, root_tier);
        let managed_step = session
            .capture_step_context(Arc::clone(&managed_turn), &CancellationToken::new())
            .await?;
        managed_step
            .validate_managed_background(lane)
            .expect("startup prewarm must be captured from one coherent managed turn");
        assert!(
            !managed_step.tool_router.model_visible_specs().is_empty(),
            "{} managed startup prewarm must construct model-visible tools from the Astra turn",
            lane.as_str()
        );
        assert_eq!(
            managed_step.turn.multi_agent_version,
            lane.required_multi_agent_version(),
            "managed Astra prewarm must retain the lane's reviewed agent capability boundary"
        );

        assert_eq!(
            (
                managed_step.settings.model_info.slug.as_str(),
                managed_step.turn.model_info().slug.as_str(),
                managed_step.turn.config.model.as_deref(),
            ),
            ("gpt-6-astra", "gpt-6-astra", Some("gpt-6-astra"),),
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
                guardian_parent_turn.approval_policy(),
                guardian_parent_turn.config.approvals_reviewer,
            ),
            (
                AskForApproval::Never,
                AskForApproval::Never,
                ApprovalsReviewer::AutoReview,
                ApprovalsReviewer::AutoReview,
                AskForApproval::Never,
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
                /*omit_update_plan_instructions*/
                !managed_step.turn.config.update_plan_enabled
                    && managed_step.turn.config.model_catalog.is_none(),
            ),
            BaseInstructions {
                text: crate::context::without_update_plan_instructions(
                    &managed_step
                        .settings
                        .model_info
                        .get_model_instructions(managed_step.turn.personality()),
                ),
                provenance: Some(BaseInstructionsProvenance::Model {
                    model: "gpt-6-astra".to_string(),
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
                /*omit_update_plan_instructions*/
                !managed_step.turn.config.update_plan_enabled
                    && managed_step.turn.config.model_catalog.is_none(),
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
        .new_startup_prewarm_turn_contexts_with_sub_id(
            "managed-prewarm-default-summary".to_string(),
            Some(ModelPolicyLane::Subscription),
        )
        .await
        .request;

    assert_eq!(managed_turn.model_info().slug, "gpt-6-astra");
    assert_eq!(managed_turn.reasoning_summary(), ReasoningSummary::Auto);
    assert_eq!(
        managed_turn.config.model_reasoning_summary,
        Some(ReasoningSummary::Auto)
    );
    assert_eq!(
        managed_turn.model_info().default_reasoning_summary,
        ReasoningSummary::None,
        "the preserved Auto value must come from the root snapshot, not Astra defaults"
    );
    let root_metadata = session
        .services
        .thread_extension_data
        .get::<ModelInfo>()
        .expect("startup prewarm must seed missing interactive root metadata");
    assert_eq!(root_metadata.slug, "gpt-5.2");
}

#[tokio::test]
async fn startup_prewarm_rejects_divergent_guardian_parent_authority() -> Result<()> {
    let (session, _initial_turn_context, _rx) =
        crate::session::tests::make_session_and_context_with_auth_and_config_and_rx(
            CodexAuth::from_api_key("Test API Key"),
            Vec::new(),
            |_| {},
        )
        .await;
    let mut prepared = prepare_startup_prewarm(
        &session,
        session.get_base_instructions().await,
        Some(ModelPolicyLane::Subscription),
    )
    .await?;
    let guardian_parent = Arc::get_mut(&mut prepared.guardian_parent_turn)
        .expect("prepared Guardian parent should be uniquely owned");
    let guardian_config = Arc::make_mut(&mut guardian_parent.config);
    guardian_config.approvals_reviewer =
        if guardian_config.approvals_reviewer == ApprovalsReviewer::User {
            ApprovalsReviewer::AutoReview
        } else {
            ApprovalsReviewer::User
        };

    let error =
        validate_guardian_parent_authority(&prepared.step_context, &prepared.guardian_parent_turn)
            .expect_err("Guardian parent authority drift must fail closed");
    assert!(
        error
            .to_string()
            .contains("startup prewarm Guardian authority is incoherent"),
        "{error}"
    );
    Ok(())
}

#[tokio::test]
#[serial_test::serial]
async fn locked_model_policy_startup_prewarm_materializes_actual_lane_without_network() -> Result<()>
{
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
        ModelPolicyLane::Subscription => (
            "gpt-5.5".to_string(),
            ReasoningEffort::High,
            Some(ServiceTier::Fast.request_value().to_string()),
        ),
        ModelPolicyLane::Api => (
            "gpt-5.6-terra".to_string(),
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
            format!("{}-priority-root", lane.as_str()),
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
    assert_eq!(root_turn.reasoning_effort(), Some(&root_effort));
    assert_eq!(root_turn.config.service_tier, root_tier);
    lane_env.restore();
    assert_eq!(locked_model_policy_lane()?, Some(lane));

    let astra_model_info = session
        .services
        .models_manager
        .get_model_info(
            lane.required_background_model(),
            &root_turn.config.to_models_manager_config(),
        )
        .await;
    let expected_instructions = astra_model_info.get_model_instructions(root_turn.personality());
    let base_instructions = BaseInstructions {
        text: root_turn
            .model_info()
            .get_model_instructions(root_turn.personality()),
        provenance: Some(BaseInstructionsProvenance::Model {
            model: root_turn.model_info().slug.clone(),
        }),
    };
    let prepared = prepare_startup_prewarm(&session, base_instructions, Some(lane)).await?;
    prepared.step_context.validate_managed_background(lane)?;
    assert_eq!(
        prepared.guardian_parent_turn.model_info().slug,
        root_turn.model_info().slug
    );
    assert_eq!(
        prepared.guardian_parent_turn.config.model,
        root_turn.config.model
    );
    assert_eq!(
        prepared.step_context.settings.approval_policy(),
        prepared.guardian_parent_turn.approval_policy()
    );
    assert_eq!(
        prepared.step_context.settings.approvals_reviewer(),
        prepared.guardian_parent_turn.config.approvals_reviewer
    );
    assert!(matches!(
        prepared.responses_metadata.request_kind,
        Some(CodexResponsesRequestKind::Prewarm)
    ));
    assert_eq!(
        prepared.prompt.base_instructions,
        BaseInstructions {
            text: expected_instructions.clone(),
            provenance: Some(BaseInstructionsProvenance::Model {
                model: "gpt-6-astra".to_string(),
            }),
        }
    );
    assert_eq!(
        prepared.responses_metadata.context_window_id,
        Some(session.current_window().await.2),
        "managed prewarm must carry the active context-window identity"
    );
    assert!(
        !prepared.prompt.tools.is_empty(),
        "managed prewarm must serialize the Astra tool snapshot"
    );

    // This helper materializes only the body. It intentionally does not call current_client_setup,
    // so the test performs no provider I/O; the live path still performs its official endpoint and
    // authentication checks before transport. The body itself uses the actual process lane and the
    // same final fail-closed request validator as production.
    let request = session
        .services
        .model_client
        .materialize_responses_request_for_test(
            &prepared.prompt,
            &prepared.step_context,
            &prepared.responses_metadata,
        )?;
    let http_wire = serde_json::to_value(&request)?;
    let websocket_wire = serde_json::to_value(ResponseCreateWsRequest {
        generate: Some(false),
        ..ResponseCreateWsRequest::from(&request)
    })?;
    let expected_mode = match lane {
        ModelPolicyLane::Api => Some("pro"),
        ModelPolicyLane::Subscription | ModelPolicyLane::Spark => None,
    };
    let expected_tier = match lane {
        ModelPolicyLane::Api => Some(SERVICE_TIER_DEFAULT_REQUEST_VALUE),
        ModelPolicyLane::Subscription | ModelPolicyLane::Spark => None,
    };
    let expected_effort = match lane {
        ModelPolicyLane::Api => "max",
        ModelPolicyLane::Subscription | ModelPolicyLane::Spark => "xhigh",
    };
    for (transport, wire) in [("HTTP", &http_wire), ("WebSocket", &websocket_wire)] {
        assert_eq!(
            (
                wire["model"].as_str(),
                wire["reasoning"]["mode"].as_str(),
                wire["reasoning"]["effort"].as_str(),
                wire.get("service_tier").and_then(|tier| tier.as_str()),
                wire["parallel_tool_calls"].as_bool(),
            ),
            (
                Some("gpt-6-astra"),
                expected_mode,
                Some(expected_effort),
                expected_tier,
                Some(!astra_model_info.use_responses_lite),
            ),
            "{transport} must carry the actual locked lane's managed prewarm contract"
        );
        let serialized_instructions = if astra_model_info.use_responses_lite {
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
            Some(expected_instructions.as_str()),
            "{transport} must serialize the Astra-derived base instructions"
        );
    }
    assert_eq!(http_wire.get("generate"), None);
    assert_eq!(websocket_wire["generate"].as_bool(), Some(false));

    Ok(())
}
