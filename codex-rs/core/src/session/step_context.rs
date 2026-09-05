//! Request-scoped settings and capabilities, including the durable context snapshot.

use std::sync::Arc;

use crate::agents_md::LoadedAgentsMd;
use crate::config::ModelPolicyLane;
use crate::config::TokenBudgetConfig;
use crate::environment_selection::TurnEnvironmentSnapshot;
use crate::session::step_settings::ResolvedStepSettings;
use crate::session::turn_context::TurnContext;
use crate::tools::router::ToolRouter;
use codex_exec_server::ExecutorCapabilityDiscoverySnapshot;
use codex_exec_server::ResolvedSelectedCapabilityRoot;
use codex_mcp::McpBinding;
use codex_otel::SessionTelemetry;
use codex_protocol::config_types::SERVICE_TIER_DEFAULT_REQUEST_VALUE;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::protocol::TurnContextItem;

/// Request-scoped state that may change between model sampling requests.
pub(crate) struct StepContext {
    pub(crate) turn: Arc<TurnContext>,
    /// One immutable settings version captured before request preparation.
    pub(crate) settings: Arc<ResolvedStepSettings>,
    /// Frozen turn preferences resolved against this step's captured model.
    pub(crate) token_budget: Option<TokenBudgetConfig>,
    /// Telemetry context tagged with this sampling request's model.
    pub(crate) session_telemetry: SessionTelemetry,
    pub(crate) environments: TurnEnvironmentSnapshot,
    /// Capability roots bound to ready environments in this exact step.
    pub(crate) selected_capability_roots: Vec<ResolvedSelectedCapabilityRoot>,
    /// Executor-materialized capability files shared by MCP and skills in this exact step.
    pub(crate) executor_capability_discovery: Option<Arc<ExecutorCapabilityDiscoverySnapshot>>,
    /// The exact MCP connections, configuration, and catalog captured for this step.
    pub(crate) mcp: Arc<McpBinding>,
    /// The finalized tool plan advertised and executed for this exact sampling request.
    pub(crate) tool_router: Arc<ToolRouter>,
    /// The canonical AGENTS.md value observed with this environment snapshot.
    pub(crate) loaded_agents_md: Option<Arc<LoadedAgentsMd>>,
}

impl StepContext {
    /// Persist the summary captured for this request, even after a live settings update.
    pub(crate) fn to_turn_context_item(&self) -> TurnContextItem {
        let mut item = self.turn.to_turn_context_item();
        item.summary = self.settings.reasoning_summary;
        item
    }
}

impl StepContext {
    /// Proves that a managed background request was captured from one coherent turn.
    ///
    /// Approval enforcement intentionally remains Turn/config-owned, so parity here
    /// is a security boundary rather than a debug-only assertion.
    pub(crate) fn validate_managed_background(&self, lane: ModelPolicyLane) -> CodexResult<()> {
        let required_model = self.turn.config.model.as_deref().unwrap_or_default();
        let required_effort = self.turn.config.model_reasoning_effort.clone();
        let required_tier = Some(SERVICE_TIER_DEFAULT_REQUEST_VALUE);
        let step_model = self.settings.model_info.slug.as_str();
        let turn_model = self.turn.model_info().slug.as_str();
        let config_model = self.turn.config.model.as_deref();
        let step_effort = self.settings.reasoning_effort().cloned();
        let turn_effort = self.turn.reasoning_effort().cloned();
        let config_effort = self.turn.config.model_reasoning_effort.clone();
        let step_summary = self.settings.reasoning_summary;
        let turn_summary = self.turn.reasoning_summary();
        let config_summary = self.turn.config.model_reasoning_summary;
        let step_tier = self.settings.service_tier.as_deref();
        let turn_tier = self.turn.initial_settings.service_tier.as_deref();
        let config_tier = self.turn.config.service_tier.as_deref();
        let step_approval = self.settings.approval_policy();
        let turn_approval = self.turn.initial_settings.approval_policy();
        let config_approval = self.turn.approval_policy();
        let step_reviewer = self.settings.approvals_reviewer();
        let turn_reviewer = self.turn.initial_settings.approvals_reviewer();
        let config_reviewer = self.turn.config.approvals_reviewer;
        let coherent = step_model == required_model
            && turn_model == required_model
            && config_model == Some(required_model)
            && step_effort == required_effort
            && turn_effort == required_effort
            && config_effort == required_effort
            && step_summary == turn_summary
            && config_summary == Some(step_summary)
            && step_tier == required_tier
            && turn_tier == required_tier
            && config_tier == required_tier
            && step_approval == turn_approval
            && step_approval == config_approval
            && step_reviewer == turn_reviewer
            && step_reviewer == config_reviewer;
        if coherent {
            return Ok(());
        }
        Err(CodexErr::InvalidRequest(format!(
            "{} managed background snapshot is incoherent: step_model={step_model} turn_model={turn_model} config_model={config_model:?} step_effort={step_effort:?} turn_effort={turn_effort:?} config_effort={config_effort:?} step_summary={step_summary:?} turn_summary={turn_summary:?} config_summary={config_summary:?} step_tier={step_tier:?} turn_tier={turn_tier:?} config_tier={config_tier:?} step_approval={step_approval:?} turn_approval={turn_approval:?} config_approval={config_approval:?} step_reviewer={step_reviewer:?} turn_reviewer={turn_reviewer:?} config_reviewer={config_reviewer:?}",
            lane.as_str(),
        )))
    }
}
