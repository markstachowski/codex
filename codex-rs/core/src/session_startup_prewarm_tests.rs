use super::*;
use anyhow::Result;
use codex_features::Feature;
use codex_login::CodexAuth;
use codex_protocol::config_types::ServiceTier;
use codex_protocol::models::BaseInstructions;
use codex_protocol::models::BaseInstructionsProvenance;
use codex_protocol::openai_models::ReasoningEffort;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::start_websocket_server;
use core_test_support::skip_if_no_network;
use pretty_assertions::assert_eq;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn locked_model_policy_managed_startup_prewarm_does_not_inherit_root() -> Result<()> {
    skip_if_no_network!(Ok(()));

    for lane in [ModelPolicyLane::Api, ModelPolicyLane::Spark] {
        let response_id = format!("{}-managed-warmup", lane.as_str());
        let server = start_websocket_server(vec![vec![vec![
            ev_response_created(&response_id),
            ev_completed(&response_id),
        ]]])
        .await;
        let base_url = format!("{}/v1", server.uri());
        let (session, _initial_turn_context, _rx) =
            crate::session::tests::make_session_and_context_with_auth_and_config_and_rx(
                CodexAuth::from_api_key("Test API Key"),
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
            ModelPolicyLane::Api => (
                original.model().to_string(),
                ReasoningEffort::High,
                Some(ServiceTier::Fast.request_value().to_string()),
            ),
            ModelPolicyLane::Spark => (
                "gpt-5.3-codex-spark".to_string(),
                ReasoningEffort::XHigh,
                None,
            ),
            ModelPolicyLane::Subscription => unreachable!("test lane matrix is explicit"),
        };
        let alternate_root = original.with_updates(
            Some(root_model.clone()),
            Some(Some(root_effort.clone())),
            /*developer_instructions*/ None,
        );
        let root_turn = session
            .new_turn_with_sub_id(
                format!("{}-priority-root", lane.as_str()),
                crate::session::SessionSettingsUpdate {
                    collaboration_mode: Some(alternate_root),
                    service_tier: root_tier.clone().map(Some),
                    ..Default::default()
                },
            )
            .await?;
        assert_eq!(root_turn.model_info.slug, root_model);
        assert_eq!(root_turn.reasoning_effort, Some(root_effort));
        assert_eq!(
            root_turn.config.service_tier, root_tier,
            "API must start premium; Spark starts on its supported Standard tier"
        );

        let sol_model_info = session
            .services
            .models_manager
            .get_model_info(
                ModelPolicyLane::Api.required_background_model(),
                &root_turn.config.to_models_manager_config(),
            )
            .await;
        let expected_instructions = sol_model_info.get_model_instructions(root_turn.personality);
        let base_instructions = BaseInstructions {
            text: root_turn
                .model_info
                .get_model_instructions(root_turn.personality),
            provenance: Some(BaseInstructionsProvenance::Model {
                model: root_turn.model_info.slug.clone(),
            }),
        };
        let _managed_client_session =
            schedule_startup_prewarm_inner(Arc::clone(&session), base_instructions, Some(lane))
                .await?;
        let managed = server
            .wait_for_request(/*connection_index*/ 0, /*request_index*/ 0)
            .await
            .body_json();

        assert_eq!(managed["generate"].as_bool(), Some(false));
        assert_eq!(managed["model"].as_str(), Some("gpt-5.6-sol"));
        assert_eq!(managed["reasoning"]["effort"].as_str(), Some("max"));
        assert_eq!(
            managed["parallel_tool_calls"].as_bool(),
            Some(sol_model_info.supports_parallel_tool_calls && !sol_model_info.use_responses_lite)
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
            managed.get("service_tier"),
            None,
            "{} managed startup prewarm must replace root Priority with the Standard sentinel; the unmanaged test serializer omits that sentinel",
            lane.as_str()
        );

        server.shutdown().await;
    }

    Ok(())
}
