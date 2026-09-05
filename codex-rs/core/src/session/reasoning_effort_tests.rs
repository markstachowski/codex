//! Request-effort pins across initial replay and compaction lookups.

use super::RequestEffortUsage;
use crate::session::step_settings::ResolvedStepSettings;
use crate::session::tests::make_session_and_context;
use codex_features::Feature;
use codex_history::InitialHistory;
use codex_history::ResumedHistory;
use codex_protocol::ThreadId;
use codex_protocol::openai_models::ReasoningEffort;
use pretty_assertions::assert_eq;
use std::sync::Arc;
use test_case::test_case;

#[test_case(InitialHistory::Forked(Vec::new()); "fork")]
#[test_case(InitialHistory::Resumed(ResumedHistory {
    conversation_id: ThreadId::default(),
    history: Arc::new(Vec::new()),
    rollout_path: None,
}); "resume")]
#[tokio::test]
async fn initial_replay_preserves_prewarmed_effort(history: InitialHistory) {
    let (mut session, turn_context) = make_session_and_context().await;
    session
        .features
        .enable(Feature::ReasoningEffortOverride)
        .unwrap();
    let mut model_info = Arc::clone(&turn_context.initial_settings.model_info);
    Arc::make_mut(&mut model_info).use_responses_lite = true;
    let mut selected = turn_context.initial_settings.selected().clone();
    selected.collaboration_mode.settings.reasoning_effort = Some(ReasoningEffort::Medium);
    let prewarm_settings = ResolvedStepSettings::new(
        Arc::new(selected.clone()),
        Arc::clone(&model_info),
        /*fast_mode_enabled*/ false,
    );
    // Force the ordering where prewarm pins its request before initial replay finishes.
    assert_eq!(
        session
            .reasoning_effort_for_request(&prewarm_settings, RequestEffortUsage::Sampling)
            .await,
        Some(ReasoningEffort::Medium),
    );
    session.record_initial_history(history).await;

    selected.collaboration_mode.settings.reasoning_effort = Some(ReasoningEffort::High);
    let turn_settings = ResolvedStepSettings::new(
        Arc::new(selected),
        model_info,
        /*fast_mode_enabled*/ false,
    );
    assert_eq!(
        session
            .reasoning_effort_for_request(&turn_settings, RequestEffortUsage::Sampling)
            .await,
        Some(ReasoningEffort::Medium),
    );
}

#[tokio::test]
async fn compaction_effort_lookup_preserves_pin_for_fallback_models() {
    let (mut session, turn_context) = make_session_and_context().await;
    session
        .features
        .enable(Feature::ReasoningEffortOverride)
        .unwrap();
    session
        .state
        .lock()
        .await
        .reasoning_effort_pin
        .pin("original", ReasoningEffort::Low);
    let mut settings = (*turn_context.initial_settings).clone();
    let effort = ReasoningEffort::Medium;
    let model = Arc::make_mut(&mut settings.model_info);
    model.slug = "fallback".to_string();
    model.use_responses_lite = true;
    model.default_reasoning_level = Some(effort.clone());

    assert_eq!(
        session
            .reasoning_effort_for_request(&settings, RequestEffortUsage::Compaction)
            .await,
        Some(effort)
    );
    assert_eq!(
        session
            .state
            .lock()
            .await
            .reasoning_effort_pin
            .get("original"),
        Some(ReasoningEffort::Low)
    );
}

#[tokio::test]
#[serial_test::serial]
async fn locked_model_policy_reasoning_override_preserves_wire_effort_and_selection() {
    use crate::config::ModelPolicyLane;
    use crate::session::tests::ModelPolicyLaneEnvGuard;
    use codex_protocol::openai_models::ReasoningEffortPreset;

    for (lane, selected, expected) in [
        (None, ReasoningEffort::Ultra, ReasoningEffort::XHigh),
        (
            Some(ModelPolicyLane::Subscription),
            ReasoningEffort::Ultra,
            ReasoningEffort::XHigh,
        ),
        (
            Some(ModelPolicyLane::Api),
            ReasoningEffort::Ultra,
            ReasoningEffort::Max,
        ),
        (
            Some(ModelPolicyLane::Api),
            ReasoningEffort::High,
            ReasoningEffort::High,
        ),
        (
            Some(ModelPolicyLane::Spark),
            ReasoningEffort::High,
            ReasoningEffort::High,
        ),
    ] {
        let _lane_guard = ModelPolicyLaneEnvGuard::unset();
        let (mut session, turn_context) = make_session_and_context().await;
        session
            .features
            .enable(Feature::ReasoningEffortOverride)
            .unwrap();
        let mut settings = (*turn_context.initial_settings).clone();
        settings
            .selected_mut()
            .collaboration_mode
            .settings
            .reasoning_effort = Some(selected.clone());
        let model = Arc::make_mut(&mut settings.model_info);
        model.slug = "gpt-6-astra".to_string();
        model.use_responses_lite = true;
        model.multi_agent_reasoning_effort = Some(ReasoningEffort::XHigh);
        model.supported_reasoning_levels = [
            ReasoningEffort::High,
            ReasoningEffort::XHigh,
            ReasoningEffort::Max,
            ReasoningEffort::Ultra,
        ]
        .into_iter()
        .map(|effort| ReasoningEffortPreset {
            description: effort.to_string(),
            effort,
        })
        .collect();
        if let Some(lane) = lane {
            // SAFETY: serialized, process-isolated test; the guard restores the environment.
            unsafe { std::env::set_var(crate::config::MODEL_POLICY_LANE_ENV, lane.as_str()) };
        }
        assert_eq!(
            session.effort_for_configuration_update(&settings).await,
            Some(expected.clone()),
            "lane={lane:?}"
        );
        assert_eq!(settings.reasoning_effort(), Some(&selected));
    }
}
