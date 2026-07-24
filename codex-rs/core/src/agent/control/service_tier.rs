//! Owns root-tier publication and lane-aware child-tier resolution.

use super::AgentControl;
use crate::config::ModelPolicyLane;
use codex_protocol::config_types::SERVICE_TIER_DEFAULT_REQUEST_VALUE;
use std::sync::Arc;

impl AgentControl {
    /// Returns the latest user-selected tier for this root and all its descendants.
    pub(crate) fn root_service_tier(&self) -> Option<String> {
        self.root_service_tier
            .load_full()
            .map(|service_tier| (*service_tier).clone())
    }

    /// Publishes a root-owned tier without mutating individual child sessions.
    pub(crate) fn set_root_service_tier(&self, service_tier: Option<String>) {
        self.root_service_tier.store(service_tier.map(Arc::new));
    }

    /// Resolves the tier for a spawned child without reinterpreting the root's
    /// valid Priority/Flex selection as child-owned authority.
    pub(crate) fn resolve_child_service_tier(
        &self,
        lane: Option<ModelPolicyLane>,
        configured_child_service_tier: Option<&str>,
    ) -> std::io::Result<Option<String>> {
        let Some(lane) = lane else {
            return Ok(self.root_service_tier());
        };
        if !lane.allows_non_root_sessions() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("{} model policy rejects non-root sessions", lane.as_str()),
            ));
        }
        lane.validate_service_tier(
            configured_child_service_tier,
            /*allow_user_service_tier_selection*/ false,
        )?;
        Ok(Some(SERVICE_TIER_DEFAULT_REQUEST_VALUE.to_string()))
    }
}
