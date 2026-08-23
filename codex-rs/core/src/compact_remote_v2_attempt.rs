use std::sync::Arc;

use super::RemoteCompactionV2Output;
use super::run_remote_compaction_request_v2;
use crate::Prompt;
use crate::client::ModelClientSession;
use crate::compact::CompactionAnalyticsDetails;
use crate::compact::CompactionAttemptContext;
use crate::compact_remote_history::trim_function_call_history_to_fit_context_window;
use crate::config::managed_background_base_instructions_for_model;
use crate::responses_metadata::CodexResponsesRequestKind;
use crate::responses_metadata::CompactionTurnMetadata;
use crate::session::session::Session;
use codex_history::CodexHarnessMetadata;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::TokenUsage;
use codex_rollout_trace::CompactionTraceContext;
use tracing::info;

pub(super) struct RemoteCompactV2Attempt {
    pub(super) trace_input_history: Option<Vec<ResponseItem>>,
    pub(super) prompt_input: Vec<ResponseItem>,
    pub(super) prompt_input_metadata: Vec<Option<CodexHarnessMetadata>>,
    pub(super) compaction_output: ResponseItem,
    pub(super) compaction_response_id: String,
    pub(super) token_usage: Option<TokenUsage>,
    /// Keeps a session created for standalone compaction alive through lifecycle completion.
    pub(super) owned_client_session: Option<ModelClientSession>,
}

pub(super) async fn run_remote_compact_v2_attempt(
    sess: &Arc<Session>,
    attempt_context: &CompactionAttemptContext,
    client_session: Option<&mut ModelClientSession>,
    compaction_trace: &CompactionTraceContext,
    compaction_metadata: CompactionTurnMetadata,
    analytics_details: &mut CompactionAnalyticsDetails,
) -> CodexResult<RemoteCompactV2Attempt> {
    let request_step = &attempt_context.request_step;
    let lifecycle_turn = &attempt_context.lifecycle_turn;
    let mut history = sess.clone_history().await;
    let base_instructions = managed_background_base_instructions_for_model(
        sess.get_prompt_base_instructions().await,
        &request_step.settings.model_info,
        request_step.turn.personality(),
        /*omit_update_plan_instructions*/
        !request_step.turn.config.update_plan_enabled
            && request_step.turn.config.model_catalog.is_none(),
    );
    let (rewritten_outputs, estimated_deleted_tokens) =
        trim_function_call_history_to_fit_context_window(
            &mut history,
            &request_step.settings.model_info,
            &base_instructions,
        );
    if rewritten_outputs > 0 {
        info!(
            turn_id = %lifecycle_turn.sub_id,
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
        .then(|| history.raw_items().cloned().collect());
    let (mut input, prompt_input_metadata): (Vec<_>, Vec<_>) = history
        .for_prompt_annotated(&request_step.settings.model_info.input_modalities)
        .into_iter()
        .map(|envelope| (envelope.item, envelope.metadata))
        .unzip();
    let tool_router = &request_step.tool_router;
    input.push(ResponseItem::CompactionTrigger {});
    let prompt = Prompt {
        input,
        tools: tool_router.model_visible_specs(),
        parallel_tool_calls: true,
        base_instructions,
        output_schema: None,
        output_schema_strict: true,
        cyber_access_program: request_step.turn.cyber_access_program,
    };

    let responses_metadata = sess
        .responses_metadata(
            request_step.turn.as_ref(),
            CodexResponsesRequestKind::Compaction(compaction_metadata),
        )
        .await;
    let trace_attempt = compaction_trace.start_attempt(&serde_json::json!({
        "model": request_step.settings.model_info.slug.as_str(),
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
        request_step.as_ref(),
        lifecycle_turn.as_ref(),
        client_session,
        &prompt,
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
    let mut prompt_input = prompt.input;
    prompt_input.pop();
    Ok(RemoteCompactV2Attempt {
        trace_input_history,
        prompt_input,
        prompt_input_metadata,
        compaction_output,
        compaction_response_id: response_id,
        token_usage,
        owned_client_session,
    })
}
