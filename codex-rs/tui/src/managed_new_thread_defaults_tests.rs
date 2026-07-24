use super::*;
use crate::legacy_core::config::ConfigBuilder;
use codex_protocol::config_types::SERVICE_TIER_DEFAULT_REQUEST_VALUE;
use codex_protocol::openai_models::ReasoningEffort;
use pretty_assertions::assert_eq;

async fn test_config() -> Config {
    let codex_home = tempfile::tempdir().expect("tempdir").keep();
    ConfigBuilder::default()
        .codex_home(codex_home)
        .build()
        .await
        .expect("config")
}

fn defaults() -> NewThreadModelDefaults {
    NewThreadModelDefaults {
        model: Some("managed-model".to_string()),
        model_reasoning_effort: Some(ReasoningEffort::High),
        service_tier: Some("fast".to_string()),
    }
}

#[tokio::test]
async fn applies_managed_defaults_to_a_new_thread_config() {
    let mut actual = test_config().await;
    actual.model = Some("configured-model".to_string());
    actual.model_reasoning_effort = Some(ReasoningEffort::Low);
    actual.service_tier = Some("flex".to_string());
    let mut expected = actual.clone();
    expected.model = Some("managed-model".to_string());
    expected.model_reasoning_effort = Some(ReasoningEffort::High);
    expected.service_tier = Some(ServiceTier::Fast.request_value().to_string());

    apply_managed_new_thread_defaults_for_lane(
        &mut actual,
        Some(&defaults()),
        &[],
        &ConfigOverrides::default(),
        None,
    );

    assert_eq!(actual, expected);
}

#[tokio::test]
async fn explicit_model_skips_managed_model_and_reasoning_effort() {
    let mut actual = test_config().await;
    actual.model = Some("explicit-model".to_string());
    actual.model_reasoning_effort = None;
    actual.service_tier = Some("flex".to_string());
    let mut expected = actual.clone();
    expected.service_tier = Some(ServiceTier::Fast.request_value().to_string());
    let harness_overrides = ConfigOverrides {
        model: Some("explicit-model".to_string()),
        ..ConfigOverrides::default()
    };

    apply_managed_new_thread_defaults_for_lane(
        &mut actual,
        Some(&defaults()),
        &[],
        &harness_overrides,
        None,
    );

    assert_eq!(actual, expected);
}

#[tokio::test]
async fn explicit_reasoning_effort_skips_managed_model_and_reasoning_effort() {
    let mut actual = test_config().await;
    actual.model = Some("configured-model".to_string());
    actual.model_reasoning_effort = Some(ReasoningEffort::Low);
    actual.service_tier = Some("flex".to_string());
    let mut expected = actual.clone();
    expected.service_tier = Some(ServiceTier::Fast.request_value().to_string());
    let cli_kv_overrides = vec![(
        "model_reasoning_effort".to_string(),
        TomlValue::String("low".to_string()),
    )];

    apply_managed_new_thread_defaults_for_lane(
        &mut actual,
        Some(&defaults()),
        &cli_kv_overrides,
        &ConfigOverrides::default(),
        None,
    );

    assert_eq!(actual, expected);
}

#[tokio::test]
async fn explicit_launch_overrides_take_precedence() {
    let mut actual = test_config().await;
    actual.model = Some("explicit-model".to_string());
    actual.model_reasoning_effort = Some(ReasoningEffort::Low);
    actual.service_tier = Some("flex".to_string());
    let expected = actual.clone();
    let cli_kv_overrides = vec![(
        "model_reasoning_effort".to_string(),
        TomlValue::String("low".to_string()),
    )];
    let harness_overrides = ConfigOverrides {
        model: Some("explicit-model".to_string()),
        service_tier: Some(Some("flex".to_string())),
        ..ConfigOverrides::default()
    };

    apply_managed_new_thread_defaults_for_lane(
        &mut actual,
        Some(&defaults()),
        &cli_kv_overrides,
        &harness_overrides,
        None,
    );

    assert_eq!(actual, expected);
}

#[tokio::test]
async fn managed_lanes_reset_new_threads_to_required_defaults() {
    for lane in [
        ModelPolicyLane::Subscription,
        ModelPolicyLane::Api,
        ModelPolicyLane::Spark,
    ] {
        let mut actual = test_config().await;
        actual.model = Some("previous-conversation-model".to_string());
        actual.model_reasoning_effort = Some(ReasoningEffort::Low);
        actual.plan_mode_reasoning_effort = Some(ReasoningEffort::Low);
        actual.service_tier = Some(ServiceTier::Fast.request_value().to_string());
        let mut expected = actual.clone();
        expected.model = Some(lane.required_model().to_string());
        expected.model_reasoning_effort = Some(lane.required_local_effort());
        expected.plan_mode_reasoning_effort = Some(lane.required_local_effort());
        expected.service_tier = Some(lane.required_root_service_tier().to_string());

        apply_managed_new_thread_defaults_for_lane(
            &mut actual,
            Some(&defaults()),
            &[],
            &ConfigOverrides::default(),
            Some(lane),
        );

        assert_eq!(actual, expected, "lane: {}", lane.as_str());
    }
}

#[tokio::test]
async fn api_fast_toggle_is_conversation_local_and_fresh_roots_return_to_priority() {
    let mut config = test_config().await;
    config.service_tier = Some(SERVICE_TIER_DEFAULT_REQUEST_VALUE.to_string());

    apply_managed_new_thread_defaults_for_lane(
        &mut config,
        Some(&defaults()),
        &[],
        &ConfigOverrides::default(),
        Some(ModelPolicyLane::Api),
    );

    assert_eq!(
        config.service_tier.as_deref(),
        Some(ServiceTier::Fast.request_value())
    );
}

#[tokio::test]
async fn subscription_lane_does_not_inherit_conversation_model_selection() {
    let mut actual = test_config().await;
    actual.model = Some("gpt-5.5".to_string());
    actual.model_reasoning_effort = Some(ReasoningEffort::High);
    actual.plan_mode_reasoning_effort = Some(ReasoningEffort::High);
    actual.service_tier = Some(ServiceTier::Fast.request_value().to_string());
    let mut expected = actual.clone();
    expected.model = Some(ModelPolicyLane::Subscription.required_model().to_string());
    expected.model_reasoning_effort = Some(ModelPolicyLane::Subscription.required_local_effort());
    expected.plan_mode_reasoning_effort =
        Some(ModelPolicyLane::Subscription.required_local_effort());
    expected.service_tier = Some(SERVICE_TIER_DEFAULT_REQUEST_VALUE.to_string());

    apply_managed_new_thread_defaults_for_lane(
        &mut actual,
        Some(&defaults()),
        &[],
        &ConfigOverrides::default(),
        Some(ModelPolicyLane::Subscription),
    );

    assert_eq!(actual, expected);
}
