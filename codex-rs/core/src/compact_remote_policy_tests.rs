use super::*;
use crate::config::SOL_MODEL;
use codex_protocol::config_types::SERVICE_TIER_DEFAULT_REQUEST_VALUE;
use pretty_assertions::assert_eq;

#[test]
fn managed_api_and_spark_remote_compaction_use_sol_max_standard() {
    for lane in [ModelPolicyLane::Api, ModelPolicyLane::Spark] {
        let actual = remote_compaction_policy_for_lane(
            "interactive-root".to_string(),
            Some(ReasoningEffort::Low),
            Some("priority".to_string()),
            Some(lane),
        );

        assert_eq!(
            actual,
            RemoteCompactionPolicy {
                model: SOL_MODEL.to_string(),
                reasoning_effort: Some(ReasoningEffort::Max),
                service_tier: Some(SERVICE_TIER_DEFAULT_REQUEST_VALUE.to_string()),
                parallel_tool_calls: true,
            },
            "{} remote compaction policy",
            lane.as_str()
        );
    }
}

#[test]
fn unmanaged_remote_compaction_preserves_inference_and_enables_parallel_tools() {
    let actual = remote_compaction_policy_for_lane(
        "unmanaged-model".to_string(),
        Some(ReasoningEffort::Custom("unmanaged-effort".to_string())),
        Some("unmanaged-tier".to_string()),
        /*lane*/ None,
    );

    assert_eq!(
        actual,
        RemoteCompactionPolicy {
            model: "unmanaged-model".to_string(),
            reasoning_effort: Some(ReasoningEffort::Custom("unmanaged-effort".to_string())),
            service_tier: Some("unmanaged-tier".to_string()),
            parallel_tool_calls: true,
        }
    );
}
