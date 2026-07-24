use super::*;
use pretty_assertions::assert_eq;

#[test]
fn subscription_review_replaces_alternate_root_model_and_effort() {
    let settings = resolve_review_inference_settings(
        "gpt-5.5".to_string(),
        Some(ReasoningEffort::High),
        None,
        Some("priority".to_string()),
        Some(ModelPolicyLane::Subscription),
    )
    .expect("subscription reviews should use the managed delegate settings");

    assert_eq!(
        settings,
        ReviewInferenceSettings {
            model: "gpt-5.6-sol".to_string(),
            reasoning_effort: Some(ReasoningEffort::Ultra),
            reasoning_mode: None,
            service_tier: Some(SERVICE_TIER_DEFAULT_REQUEST_VALUE.to_string()),
        }
    );
}

#[test]
fn unmanaged_review_preserves_requested_inference_settings() {
    let settings = resolve_review_inference_settings(
        "gpt-5.4".to_string(),
        Some(ReasoningEffort::High),
        Some(ReasoningMode::Pro),
        Some("priority".to_string()),
        None,
    )
    .expect("unmanaged reviews should preserve upstream settings");

    assert_eq!(
        settings,
        ReviewInferenceSettings {
            model: "gpt-5.4".to_string(),
            reasoning_effort: Some(ReasoningEffort::High),
            reasoning_mode: Some(ReasoningMode::Pro),
            service_tier: Some("priority".to_string()),
        }
    );
}

#[test]
fn spark_review_is_rejected_before_spawning_a_delegate() {
    let error = resolve_review_inference_settings(
        "gpt-5.3-codex-spark".to_string(),
        Some(ReasoningEffort::XHigh),
        None,
        Some("priority".to_string()),
        Some(ModelPolicyLane::Spark),
    )
    .expect_err("the Spark lane is root-only");

    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    assert_eq!(
        error.to_string(),
        "spark model policy rejects review delegates because the lane is root-only"
    );
}
