use crate::config::ModelPolicyLane;
use crate::config::managed_background_inference_for_lane;
use codex_protocol::openai_models::ReasoningEffort;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RemoteCompactionPolicy {
    model: String,
    reasoning_effort: Option<ReasoningEffort>,
    service_tier: Option<String>,
    parallel_tool_calls: bool,
}

impl RemoteCompactionPolicy {
    pub(crate) fn model(&self) -> &str {
        &self.model
    }

    pub(crate) fn reasoning_effort(&self) -> Option<&ReasoningEffort> {
        self.reasoning_effort.as_ref()
    }

    pub(crate) fn service_tier(&self) -> Option<&str> {
        self.service_tier.as_deref()
    }

    pub(crate) const fn parallel_tool_calls(&self) -> bool {
        self.parallel_tool_calls
    }
}

/// Resolve the shared request policy for both remote-compaction transports.
/// Managed background work uses the wire-level effort, while unmanaged
/// execution preserves the caller's inference settings.
pub(crate) fn remote_compaction_policy_for_lane(
    inherited_model: String,
    inherited_reasoning_effort: Option<ReasoningEffort>,
    inherited_service_tier: Option<String>,
    lane: Option<ModelPolicyLane>,
) -> RemoteCompactionPolicy {
    let inference = managed_background_inference_for_lane(
        inherited_model,
        inherited_reasoning_effort,
        inherited_service_tier,
        lane,
    );
    let reasoning_effort = match lane {
        Some(lane) => Some(lane.required_background_wire_effort()),
        None => inference.reasoning_effort,
    };
    RemoteCompactionPolicy {
        model: inference.model,
        reasoning_effort,
        service_tier: inference.service_tier,
        parallel_tool_calls: true,
    }
}

#[cfg(test)]
#[path = "compact_remote_policy_tests.rs"]
mod tests;
