//! Managed lanes keep synchronous approval review without starting Luna transport.

use super::*;
use codex_core::config::ModelPolicyLane;
use codex_core::context::GuardianReviewEvidence;
use codex_extension_api::ApprovalDecision;
use codex_extension_api::ApprovalDecisionInput;
use codex_extension_api::ExtensionEventSink;
use codex_extension_api::ExtensionWarning;
use codex_extension_api::GuardianV2Enabled;
use codex_extension_api::SynchronousApprovalReviewer;
use codex_protocol::approvals::GuardianReviewReason;
use codex_protocol::openai_models::GuardianModelPolicy;
use codex_protocol::openai_models::GuardianReviewMode;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::Event;
use pretty_assertions::assert_eq;

#[derive(Default)]
struct Warnings(Mutex<Vec<ExtensionWarning>>);

impl ExtensionEventSink for Warnings {
    fn emit(&self, _: Event) {}

    fn emit_warning(&self, warning: ExtensionWarning) {
        self.0.lock().unwrap().push(warning);
    }
}

struct Reviewer {
    decision: Option<ReviewDecision>,
    reasons: Mutex<Vec<GuardianReviewReason>>,
}

impl SynchronousApprovalReviewer for Reviewer {
    fn review(
        &self,
        reason: GuardianReviewReason,
    ) -> codex_extension_api::ExtensionFuture<'_, Option<ReviewDecision>> {
        self.reasons.lock().unwrap().push(reason);
        Box::pin(async { self.decision.clone() })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn managed_lanes_skip_luna_and_preserve_synchronous_denial() -> Result<()> {
    // Local mock transport only; do not skip this managed-policy proof floor.
    let server = responses::start_mock_server().await;
    let test = test_codex().build_with_auto_env(&server).await?;
    let mut config = test.config.clone();
    config.features.enable(Feature::GuardianApproval)?;
    config.approvals_reviewer = ApprovalsReviewer::AutoReview;
    let warnings = Arc::new(Warnings::default());
    let auth_manager = AuthManager::from_auth_for_testing(CodexAuth::from_api_key("test-api-key"));
    let extension = GuardianV2Extension {
        auth_manager: auth_manager.clone(),
        event_sink: warnings.clone(),
        thread_manager: Arc::downgrade(&test.thread_manager),
    };
    let mut builder = ExtensionRegistryBuilder::new();
    super::super::install(
        &mut builder,
        auth_manager,
        Arc::downgrade(&test.thread_manager),
    );
    let registry = builder.build();
    let session_store = ExtensionData::new("managed-startup");
    let store = test.codex.thread_extension_data();
    let mut model = store.get::<ModelInfo>().unwrap().as_ref().clone();
    model.guardian = Some(GuardianModelPolicy {
        shell: Some(GuardianReviewMode::Adaptive),
        ..Default::default()
    });
    store.insert(model);
    let requests_before = server.received_requests().await.unwrap().len();
    let evidence = store.get_or_init(GuardianReviewEvidence::default);
    let action = json!({"tool": "exec_command", "command": "untrusted command"});
    for lane in [
        ModelPolicyLane::Subscription,
        ModelPolicyLane::Api,
        ModelPolicyLane::Spark,
    ] {
        store.insert(GuardianV2Enabled);
        store.insert(GuardianV2ScoreProgress::default());
        store.insert(SecurityRiskScore {
            scores: BTreeMap::from([("action_risk".to_owned(), 0.0)]),
            call_id: Some("old-call".to_owned()),
            action: Some(action.clone()),
            sampled_at: None,
        });
        extension
            .start_thread(
                ThreadStartInput {
                    config: &config,
                    session_source: &SessionSource::Exec,
                    persistent_thread_state_available: false,
                    environments: &[],
                    mcp_resource_client: None,
                    extension_metrics: None,
                    session_store: &session_store,
                    thread_store: store,
                },
                Ok(Some(lane)),
            )
            .await;
        assert!(warnings.0.lock().unwrap().is_empty());
        assert!(store.get::<LunaSampler>().is_none());
        assert!(store.get::<GuardianV2Enabled>().is_none());
        assert!(store.get::<GuardianV2ScoreProgress>().is_none());
        assert!(store.get::<SecurityRiskScore>().is_none());
        assert!(store.get::<GuardianV2Config>().is_some());
        assert!(
            store
                .get::<crate::async_scorer::trusted_skills::TrustedSkillRoots>()
                .is_some()
        );
        assert!(Arc::ptr_eq(
            &evidence,
            &store.get::<GuardianReviewEvidence>().unwrap()
        ));

        for decision in [Some(ReviewDecision::denied("unsafe action")), None] {
            let expected = decision
                .clone()
                .map_or(ApprovalDecision::AskUser, ApprovalDecision::Reviewed);
            let reviewer = Reviewer {
                decision,
                reasons: Mutex::default(),
            };
            let input = ApprovalDecisionInput {
                approval_id: "managed-startup-proof",
                tool_call_id: Some("old-call"),
                action: &action,
                thread_id: ThreadId::from_string(store.level_id())?,
                thread_store: store,
                category: GuardianScope::Shell,
                approval_policy: AskForApproval::OnRequest,
                approvals_reviewer: ApprovalsReviewer::AutoReview,
                require_guardian: true,
                require_fresh_review: false,
                full_access: false,
                metrics: None,
                synchronous_reviewer: &reviewer,
            };
            assert_eq!(registry.decide_approval(&input).await, Some(expected));
            assert_eq!(
                *reviewer.reasons.lock().unwrap(),
                vec![GuardianReviewReason::MissingScore]
            );
        }
    }
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        requests_before
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_managed_policy_is_not_silently_treated_as_supported() -> Result<()> {
    let server = responses::start_mock_server().await;
    let test = test_codex().build_with_auto_env(&server).await?;
    let mut config = test.config.clone();
    config.features.enable(Feature::GuardianApproval)?;
    let warnings = Arc::new(Warnings::default());
    let extension = GuardianV2Extension {
        auth_manager: AuthManager::from_auth_for_testing(CodexAuth::from_api_key("test-api-key")),
        event_sink: warnings.clone(),
        thread_manager: Arc::downgrade(&test.thread_manager),
    };
    let session_store = ExtensionData::new("invalid-managed-startup");
    let store = test.codex.thread_extension_data();
    let requests_before = server.received_requests().await.unwrap().len();
    extension
        .start_thread(
            ThreadStartInput {
                config: &config,
                session_source: &SessionSource::Exec,
                persistent_thread_state_available: false,
                environments: &[],
                mcp_resource_client: None,
                extension_metrics: None,
                session_store: &session_store,
                thread_store: store,
            },
            Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "invalid lane marker",
            )),
        )
        .await;
    assert!(store.get::<LunaSampler>().is_none());
    assert!(store.get::<GuardianV2ScoreProgress>().is_none());
    {
        let observed = warnings.0.lock().unwrap();
        assert_eq!(observed.len(), 1);
        assert_eq!(
            observed[0].message,
            "Guardian model policy initialization failed: invalid lane marker"
        );
    }
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        requests_before
    );
    Ok(())
}
