use std::sync::Arc;

use super::RemoteCompactionV2Output;
use super::run_remote_compaction_request_v2;
use crate::Prompt;
use crate::client::ModelClientSession;
use crate::compact::CompactionAnalyticsDetails;
use crate::compact_remote::trim_function_call_history_to_fit_context_window;
use crate::config::locked_model_policy_lane;
use crate::config::managed_background_base_instructions_for_model;
use crate::config::managed_background_inference_for_lane;
use crate::responses_metadata::CodexResponsesRequestKind;
use crate::responses_metadata::CompactionTurnMetadata;
use crate::session::session::Session;
use crate::session::step_context::StepContext;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::RawResponseCompletedEvent;
use codex_protocol::protocol::TokenUsage;
use codex_rollout_trace::CompactionTraceContext;
use tracing::info;

pub(super) struct RemoteCompactV2Attempt {
    pub(super) trace_input_history: Option<Vec<ResponseItem>>,
    pub(super) prompt_input: Vec<ResponseItem>,
    pub(super) compaction_output: ResponseItem,
    pub(super) token_usage: Option<TokenUsage>,
    /// Keeps a session created for standalone compaction alive through lifecycle completion.
    pub(super) owned_client_session: Option<ModelClientSession>,
}

pub(super) async fn run_remote_compact_v2_attempt(
    sess: &Arc<Session>,
    step_context: &Arc<StepContext>,
    client_session: Option<&mut ModelClientSession>,
    compaction_trace: &CompactionTraceContext,
    compaction_metadata: CompactionTurnMetadata,
    analytics_details: &mut CompactionAnalyticsDetails,
) -> CodexResult<RemoteCompactV2Attempt> {
    let turn_context = &step_context.turn;
    let mut history = sess.clone_history().await;
    let base_instructions = sess.get_base_instructions().await;
    let (rewritten_outputs, estimated_deleted_tokens) =
        trim_function_call_history_to_fit_context_window(
            &mut history,
            turn_context.as_ref(),
            &base_instructions,
        );
    if rewritten_outputs > 0 {
        info!(
            turn_id = %turn_context.sub_id,
            rewritten_outputs,
            "rewrote history outputs before remote compaction v2"
        );
    }
    if estimated_deleted_tokens > 0 {
        let max_local_deleted_tokens = sess
            .estimated_tokens_after_last_model_generated_item()
            .await;
        analytics_details.active_context_tokens_before = analytics_details
            .active_context_tokens_before
            .map(|active_context_tokens_before| {
                active_context_tokens_before
                    .saturating_sub(estimated_deleted_tokens.min(max_local_deleted_tokens))
            });
    }

    let trace_input_history = compaction_trace
        .is_enabled()
        .then(|| history.raw_items().to_vec());
    let lane = locked_model_policy_lane()?;
    let inference = managed_background_inference_for_lane(
        turn_context.model_info.slug.clone(),
        turn_context.reasoning_effort.clone(),
        turn_context.config.service_tier.clone(),
        lane,
    );
    let model_info = match lane {
        Some(_) => {
            sess.services
                .models_manager
                .get_model_info(
                    inference.model.as_str(),
                    &turn_context.config.to_models_manager_config(),
                )
                .await
        }
        None => turn_context.model_info.clone(),
    };
    let session_telemetry = match lane {
        Some(_) => turn_context
            .session_telemetry
            .clone()
            .with_model(model_info.slug.as_str(), model_info.slug.as_str()),
        None => turn_context.session_telemetry.clone(),
    };
    let base_instructions = managed_background_base_instructions_for_model(
        base_instructions,
        &model_info,
        turn_context.personality,
        lane,
    );
    let mut input = history.for_prompt(&model_info.input_modalities);
    let tool_router = &step_context.tool_router;
    input.push(ResponseItem::CompactionTrigger {});
    let prompt = Prompt {
        input,
        tools: tool_router.model_visible_specs(),
        parallel_tool_calls: model_info.supports_parallel_tool_calls,
        base_instructions,
        output_schema: None,
        output_schema_strict: true,
    };

    let window_id = sess.current_window_id().await;
    let responses_metadata = turn_context.turn_metadata_state.to_responses_metadata(
        sess.installation_id.clone(),
        window_id,
        CodexResponsesRequestKind::Compaction(compaction_metadata),
    );
    let trace_attempt = compaction_trace.start_attempt(&serde_json::json!({
        "model": model_info.slug.as_str(),
        "instructions": prompt.base_instructions.text.as_str(),
        "input": &prompt.input,
        "parallel_tool_calls": prompt.parallel_tool_calls,
    }));
    let mut owned_client_session = None;
    let client_session = match client_session {
        Some(client_session) => client_session,
        None => owned_client_session.insert(sess.services.model_client.new_session()),
    };
    let compaction_output_result = run_remote_compaction_request_v2(
        sess,
        turn_context.as_ref(),
        client_session,
        &prompt,
        &model_info,
        &session_telemetry,
        inference.reasoning_effort,
        inference.service_tier,
        &responses_metadata,
    )
    .await;
    trace_attempt.record_result(
        compaction_output_result
            .as_ref()
            .map(|output| std::slice::from_ref(&output.compaction_output)),
    );
    let RemoteCompactionV2Output {
        compaction_output,
        response_id,
        token_usage,
    } = compaction_output_result?;
    // TODO: Emit this before compaction output validation so malformed completed
    // responses still surface their raw upstream usage.
    sess.send_event(
        turn_context,
        EventMsg::RawResponseCompleted(RawResponseCompletedEvent {
            response_id,
            token_usage: token_usage.clone(),
        }),
    )
    .await;
    let mut prompt_input = prompt.input;
    prompt_input.pop();
    Ok(RemoteCompactV2Attempt {
        trace_input_history,
        prompt_input,
        compaction_output,
        token_usage,
        owned_client_session,
    })
}
