use std::io;

use crate::legacy_core::config::Config;
use crate::legacy_core::config::ConfigOverrides;
use crate::legacy_core::config::ModelPolicyLane;
use crate::legacy_core::config::locked_model_policy_lane;
use crate::resume_picker::SessionSelection;
use codex_app_server_protocol::NewThreadModelDefaults;
use codex_protocol::config_types::ServiceTier;
use toml::Value as TomlValue;

pub(crate) fn apply_managed_new_root_model_defaults(config: &mut Config) -> std::io::Result<()> {
    if let Some(lane) = locked_model_policy_lane()? {
        apply_lane_model_defaults(config, lane);
    }
    Ok(())
}

pub(crate) fn apply_managed_new_thread_defaults_for_selection(
    session_selection: &SessionSelection,
    apply: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    if matches!(
        session_selection,
        SessionSelection::StartFresh | SessionSelection::Fork(_) | SessionSelection::Exit
    ) {
        apply()?;
    }
    Ok(())
}

fn apply_lane_model_defaults(config: &mut Config, lane: ModelPolicyLane) {
    config.model = Some(lane.required_model().to_string());
    config.model_reasoning_effort = Some(lane.required_local_effort());
    config.plan_mode_reasoning_effort = Some(lane.required_local_effort());
    config.service_tier = Some(lane.required_root_service_tier().to_string());
}

pub(crate) fn apply_managed_new_thread_defaults(
    config: &mut Config,
    defaults: Option<&NewThreadModelDefaults>,
    cli_kv_overrides: &[(String, TomlValue)],
    harness_overrides: &ConfigOverrides,
) -> std::io::Result<()> {
    apply_managed_new_thread_defaults_for_lane(
        config,
        defaults,
        cli_kv_overrides,
        harness_overrides,
        locked_model_policy_lane()?,
    );
    Ok(())
}

fn apply_managed_new_thread_defaults_for_lane(
    config: &mut Config,
    defaults: Option<&NewThreadModelDefaults>,
    cli_kv_overrides: &[(String, TomlValue)],
    harness_overrides: &ConfigOverrides,
    lane: Option<ModelPolicyLane>,
) {
    // Interactive subscription model changes belong only to the active conversation. Every new,
    // cleared, forked, or side root starts from the lane's managed pair; app-server defaults must
    // not carry the prior conversation selection into a new root.
    if let Some(lane) = lane {
        apply_lane_model_defaults(config, lane);
        return;
    }
    let Some(defaults) = defaults else {
        return;
    };
    // Managed values are defaults rather than enforcement. Preserve explicit launch choices from
    // dedicated flags such as `-m` (`harness_overrides`) and generic `-c key=value` settings
    // (`cli_kv_overrides`), then fill only the fields that were not selected for this invocation.
    // Model and reasoning effort are a compatibility-sensitive pair, so an explicit override of
    // either opts out of both managed values. For example, `codex -m gpt-5.4` keeps that model and
    // its existing/default effort, while `-c model_reasoning_effort=low` does not switch to the
    // managed model. Service tier remains independent and is resolved against the selected model
    // before the thread starts.
    let has_cli_override = |key: &str| cli_kv_overrides.iter().any(|(path, _value)| path == key);
    let has_explicit_model_settings = harness_overrides.model.is_some()
        || has_cli_override("model")
        || has_cli_override("model_reasoning_effort");

    if !has_explicit_model_settings && let Some(model) = defaults.model.as_ref() {
        config.model = Some(model.clone());
    }
    if !has_explicit_model_settings
        && let Some(reasoning_effort) = defaults.model_reasoning_effort.as_ref()
    {
        config.model_reasoning_effort = Some(reasoning_effort.clone());
    }
    if harness_overrides.service_tier.is_none()
        && !has_cli_override("service_tier")
        && let Some(service_tier) = defaults.service_tier.as_ref()
    {
        config.service_tier = Some(
            ServiceTier::from_request_value(service_tier)
                .map(|tier| tier.request_value().to_string())
                .unwrap_or_else(|| service_tier.clone()),
        );
    }
}

#[cfg(test)]
#[path = "managed_new_thread_defaults_tests.rs"]
mod tests;
