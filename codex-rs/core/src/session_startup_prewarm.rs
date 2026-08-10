use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tokio_util::task::AbortOnDropHandle;
use tracing::Instrument;
use tracing::info;
use tracing::instrument;
use tracing::trace_span;
use tracing::warn;

use crate::client::ModelClientSession;
use crate::client_common::Prompt;
use crate::config::ModelPolicyLane;
use crate::config::locked_model_policy_lane;
use crate::config::managed_background_base_instructions_for_model;
use crate::guardian::routes_approval_to_guardian;
use crate::responses_metadata::CodexResponsesMetadata;
use crate::responses_metadata::CodexResponsesRequestKind;
use crate::session::INITIAL_SUBMIT_ID;
use crate::session::RequestEffortUsage;
use crate::session::session::Session;
use crate::session::step_context::StepContext;
use crate::session::turn::build_prompt;
use crate::session::turn_context::TurnContext;
use codex_features::Feature;
use codex_otel::STARTUP_PREWARM_AGE_AT_FIRST_TURN_METRIC;
use codex_otel::STARTUP_PREWARM_DURATION_METRIC;
use codex_otel::SessionTelemetry;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::models::BaseInstructions;

pub(crate) struct SessionStartupPrewarmHandle {
    task: AbortOnDropHandle<CodexResult<ModelClientSession>>,
    started_at: Instant,
    timeout: Duration,
}

pub(crate) enum SessionStartupPrewarmResolution {
    Cancelled,
    Ready(Box<ModelClientSession>),
    Unavailable {
        status: &'static str,
        prewarm_duration: Option<Duration>,
    },
}

struct PreparedStartupPrewarm {
    guardian_parent_turn: Arc<TurnContext>,
    step_context: Arc<StepContext>,
    prompt: Prompt,
    responses_metadata: CodexResponsesMetadata,
}

impl SessionStartupPrewarmHandle {
    pub(crate) fn new(
        task: JoinHandle<CodexResult<ModelClientSession>>,
        started_at: Instant,
        timeout: Duration,
    ) -> Self {
        Self {
            task: AbortOnDropHandle::new(task),
            started_at,
            timeout,
        }
    }

    pub(crate) async fn abort(self) {
        self.task.abort();
        let _ = self.task.await;
    }

    #[instrument(name = "startup_prewarm.resolve", level = "trace", skip_all)]
    async fn resolve(
        self,
        session_telemetry: &SessionTelemetry,
        cancellation_token: &CancellationToken,
    ) -> SessionStartupPrewarmResolution {
        let resolve_started_at = Instant::now();
        let Self {
            mut task,
            started_at,
            timeout,
        } = self;
        let age_at_first_turn = started_at.elapsed();
        let remaining = timeout.saturating_sub(age_at_first_turn);

        let resolution = if task.is_finished() {
            Self::resolution_from_join_result(task.await, started_at)
        } else {
            match tokio::select! {
                _ = cancellation_token.cancelled() => None,
                result = tokio::time::timeout(remaining, &mut task) => Some(result),
            } {
                Some(Ok(result)) => Self::resolution_from_join_result(result, started_at),
                Some(Err(_elapsed)) => {
                    task.abort();
                    info!("startup websocket prewarm timed out before the first turn could use it");
                    SessionStartupPrewarmResolution::Unavailable {
                        status: "timed_out",
                        prewarm_duration: Some(started_at.elapsed()),
                    }
                }
                None => {
                    task.abort();
                    session_telemetry.record_startup_phase(
                        "startup_prewarm_resolve",
                        resolve_started_at.elapsed(),
                        Some("cancelled"),
                    );
                    session_telemetry.record_duration(
                        STARTUP_PREWARM_AGE_AT_FIRST_TURN_METRIC,
                        age_at_first_turn,
                        &[("status", "cancelled")],
                    );
                    session_telemetry.record_duration(
                        STARTUP_PREWARM_DURATION_METRIC,
                        started_at.elapsed(),
                        &[("status", "cancelled")],
                    );
                    return SessionStartupPrewarmResolution::Cancelled;
                }
            }
        };
        let status = match &resolution {
            SessionStartupPrewarmResolution::Cancelled => "cancelled",
            SessionStartupPrewarmResolution::Ready(_) => "ready",
            SessionStartupPrewarmResolution::Unavailable { status, .. } => status,
        };
        session_telemetry.record_startup_phase(
            "startup_prewarm_resolve",
            resolve_started_at.elapsed(),
            Some(status),
        );

        match resolution {
            SessionStartupPrewarmResolution::Cancelled => {
                SessionStartupPrewarmResolution::Cancelled
            }
            SessionStartupPrewarmResolution::Ready(prewarmed_session) => {
                session_telemetry.record_duration(
                    STARTUP_PREWARM_AGE_AT_FIRST_TURN_METRIC,
                    age_at_first_turn,
                    &[("status", "consumed")],
                );
                SessionStartupPrewarmResolution::Ready(prewarmed_session)
            }
            SessionStartupPrewarmResolution::Unavailable {
                status,
                prewarm_duration,
            } => {
                session_telemetry.record_duration(
                    STARTUP_PREWARM_AGE_AT_FIRST_TURN_METRIC,
                    age_at_first_turn,
                    &[("status", status)],
                );
                if let Some(prewarm_duration) = prewarm_duration {
                    session_telemetry.record_duration(
                        STARTUP_PREWARM_DURATION_METRIC,
                        prewarm_duration,
                        &[("status", status)],
                    );
                }
                SessionStartupPrewarmResolution::Unavailable {
                    status,
                    prewarm_duration,
                }
            }
        }
    }

    fn resolution_from_join_result(
        result: std::result::Result<CodexResult<ModelClientSession>, tokio::task::JoinError>,
        started_at: Instant,
    ) -> SessionStartupPrewarmResolution {
        match result {
            Ok(Ok(prewarmed_session)) => {
                SessionStartupPrewarmResolution::Ready(Box::new(prewarmed_session))
            }
            Ok(Err(err)) => {
                warn!("startup websocket prewarm setup failed: {err:#}");
                SessionStartupPrewarmResolution::Unavailable {
                    status: "failed",
                    prewarm_duration: None,
                }
            }
            Err(err) => {
                warn!("startup websocket prewarm setup join failed: {err}");
                SessionStartupPrewarmResolution::Unavailable {
                    status: "join_failed",
                    prewarm_duration: Some(started_at.elapsed()),
                }
            }
        }
    }
}

impl Session {
    pub(crate) async fn schedule_startup_prewarm(
        self: &Arc<Self>,
        base_instructions: BaseInstructions,
    ) {
        if self.features().enabled(Feature::CodeModePrewarm)
            && self.services.code_mode_service.is_available()
        {
            let session = Arc::clone(self);
            tokio::spawn(async move {
                if session.services.code_mode_service.session().await.is_err() {
                    warn!("code-mode host startup prewarm failed");
                }
            });
        }

        if !self.services.model_client.responses_websocket_enabled() {
            // Without websocket prewarm, resolve auth once so Agent Identity bootstrap can
            // register or engage this session's bearer fallback before the first user request.
            let model_client = self.services.model_client.clone();
            tokio::spawn(async move {
                if let Err(err) = model_client.prewarm_auth().await {
                    warn!("startup auth prewarm failed: {err:#}");
                }
            });
            return;
        }

        let session_telemetry = self.services.session_telemetry.clone();
        let websocket_connect_timeout = self.provider().await.websocket_connect_timeout();
        let started_at = Instant::now();
        let startup_prewarm_session = Arc::clone(self);
        let startup_prewarm = tokio::spawn(
            async move {
                let result = match locked_model_policy_lane() {
                    Ok(lane) => {
                        schedule_startup_prewarm_inner(
                            startup_prewarm_session,
                            base_instructions,
                            lane,
                        )
                        .await
                    }
                    Err(err) => Err(err.into()),
                };
                let status = if result.is_ok() { "ready" } else { "failed" };
                session_telemetry.record_startup_phase(
                    "startup_prewarm_total",
                    started_at.elapsed(),
                    Some(status),
                );
                session_telemetry.record_duration(
                    STARTUP_PREWARM_DURATION_METRIC,
                    started_at.elapsed(),
                    &[("status", status)],
                );
                result
            }
            .instrument(trace_span!(
                "startup_prewarm",
                otel.name = "startup_prewarm",
                thread.id = %self.thread_id(),
            )),
        );
        self.set_session_startup_prewarm(SessionStartupPrewarmHandle::new(
            startup_prewarm,
            started_at,
            websocket_connect_timeout,
        ))
        .await;
    }

    pub(crate) async fn consume_startup_prewarm_for_regular_turn(
        &self,
        cancellation_token: &CancellationToken,
    ) -> SessionStartupPrewarmResolution {
        let Some(startup_prewarm) = self.take_session_startup_prewarm().await else {
            return SessionStartupPrewarmResolution::Unavailable {
                status: "not_scheduled",
                prewarm_duration: None,
            };
        };
        startup_prewarm
            .resolve(&self.services.session_telemetry, cancellation_token)
            .await
    }
}

async fn schedule_startup_prewarm_inner(
    session: Arc<Session>,
    base_instructions: BaseInstructions,
    lane: Option<ModelPolicyLane>,
) -> CodexResult<ModelClientSession> {
    let PreparedStartupPrewarm {
        guardian_parent_turn,
        step_context,
        prompt,
        responses_metadata,
    } = prepare_startup_prewarm(&session, base_instructions, lane).await?;
    let startup_turn_context = Arc::clone(&step_context.turn);
    // Guardian still enforces Turn/config-owned approval policy. Initialize it only
    // after the managed Step/Turn parity boundary passes, and install it before the regular
    // websocket warmup so the first review reuses the prewarmed session.
    if routes_approval_to_guardian(&guardian_parent_turn)
        && let Err(err) = crate::guardian::prewarm_guardian_review_session(
            Arc::clone(&session),
            guardian_parent_turn,
        )
        .await
    {
        warn!("failed to initialize guardian review session: {err:#}");
    }
    let mut client_session = session.services.model_client.new_session();
    let websocket_warmup_started_at = Instant::now();
    client_session
        .prewarm_websocket(
            &prompt,
            &step_context.settings.model_info,
            &step_context.session_telemetry,
            step_context.settings.reasoning_effort().cloned(),
            step_context.settings.reasoning_summary,
            step_context.settings.service_tier.clone(),
            &responses_metadata,
        )
        .await?;
    startup_turn_context.session_telemetry.record_startup_phase(
        "startup_prewarm_websocket_warmup",
        websocket_warmup_started_at.elapsed(),
        /*status*/ None,
    );
    Ok(client_session)
}

async fn prepare_startup_prewarm(
    session: &Arc<Session>,
    base_instructions: BaseInstructions,
    lane: Option<ModelPolicyLane>,
) -> CodexResult<PreparedStartupPrewarm> {
    let prewarm_started_at = Instant::now();
    let startup_turn_contexts = session
        .new_startup_prewarm_turn_contexts_with_sub_id(INITIAL_SUBMIT_ID.to_owned(), lane)
        .await;
    let startup_turn_context = startup_turn_contexts.request;
    let guardian_parent_turn = startup_turn_contexts.guardian_parent;
    startup_turn_context.session_telemetry.record_startup_phase(
        "startup_prewarm_create_turn_context",
        prewarm_started_at.elapsed(),
        /*status*/ None,
    );
    let startup_cancellation_token = CancellationToken::new();
    let built_tools_started_at = Instant::now();
    // Startup prewarm runs before run_turn and needs its own tool-building snapshot.
    let step_context = session
        .capture_step_context(
            Arc::clone(&startup_turn_context),
            &startup_cancellation_token,
        )
        .await?;
    if let Some(lane) = lane {
        step_context.validate_managed_background(lane)?;
    }
    validate_guardian_parent_authority(&step_context, &guardian_parent_turn)?;
    let base_instructions = managed_background_base_instructions_for_model(
        base_instructions,
        &step_context.settings.model_info,
        startup_turn_context.personality(),
    );
    startup_turn_context.session_telemetry.record_startup_phase(
        "startup_prewarm_build_tools",
        built_tools_started_at.elapsed(),
        /*status*/ None,
    );
    let build_prompt_started_at = Instant::now();
    let startup_prompt = build_prompt(Vec::new(), step_context.as_ref(), base_instructions);
    startup_turn_context.session_telemetry.record_startup_phase(
        "startup_prewarm_build_prompt",
        build_prompt_started_at.elapsed(),
        /*status*/ None,
    );
    let responses_metadata = session
        .responses_metadata(
            step_context.turn.as_ref(),
            CodexResponsesRequestKind::Prewarm,
        )
        .await;
    Ok(PreparedStartupPrewarm {
        guardian_parent_turn,
        step_context,
        prompt: startup_prompt,
        responses_metadata,
    })
}

fn validate_guardian_parent_authority(
    step_context: &StepContext,
    guardian_parent_turn: &TurnContext,
) -> CodexResult<()> {
    if step_context.settings.approval_policy() == guardian_parent_turn.approval_policy()
        && step_context.settings.approvals_reviewer()
            == guardian_parent_turn.config.approvals_reviewer
    {
        return Ok(());
    }
    Err(CodexErr::InvalidRequest(format!(
        "startup prewarm Guardian authority is incoherent: step_approval={:?} guardian_approval={:?} step_reviewer={:?} guardian_reviewer={:?}",
        step_context.settings.approval_policy(),
        guardian_parent_turn.approval_policy(),
        step_context.settings.approvals_reviewer(),
        guardian_parent_turn.config.approvals_reviewer,
    )))
}

#[cfg(test)]
#[path = "session_startup_prewarm_tests.rs"]
mod tests;
