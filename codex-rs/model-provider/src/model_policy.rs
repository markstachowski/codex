use std::ffi::OsStr;

use codex_api::Provider;
use codex_model_provider_info::CHATGPT_CODEX_BASE_URL;
use codex_model_provider_info::ModelProviderInfo;
use codex_protocol::auth::AuthMode;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CoreResult;

const MODEL_POLICY_LANE_ENV: &str = "CDX_MODEL_POLICY_LANE";
const API_BASE_URL: &str = "https://api.openai.com/v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ManagedProviderLane {
    Subscription,
    Api,
    Spark,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ManagedProviderBuildMode {
    Debug,
    Release,
}

impl ManagedProviderBuildMode {
    const fn current() -> Self {
        if cfg!(debug_assertions) {
            Self::Debug
        } else {
            Self::Release
        }
    }
}

impl ManagedProviderLane {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Subscription => "subscription",
            Self::Api => "api",
            Self::Spark => "spark",
        }
    }

    const fn required_auth_mode(self) -> AuthMode {
        match self {
            Self::Api => AuthMode::ApiKey,
            Self::Subscription | Self::Spark => AuthMode::Chatgpt,
        }
    }

    const fn required_base_url(self) -> &'static str {
        match self {
            Self::Api => API_BASE_URL,
            Self::Subscription | Self::Spark => CHATGPT_CODEX_BASE_URL,
        }
    }

    fn invalid(self, message: impl Into<String>) -> CodexErr {
        CodexErr::InvalidRequest(format!(
            "{} model policy rejected {}",
            self.as_str(),
            message.into()
        ))
    }
}

/// Immutable managed-provider policy captured before resolving credentials.
///
/// Keeping the parsed lane in one value prevents a process-environment change
/// from weakening the second validation performed after auth resolution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ManagedProviderPolicy {
    lane: Option<ManagedProviderLane>,
}

impl ManagedProviderPolicy {
    pub(crate) fn from_environment() -> CoreResult<Self> {
        let marker = std::env::var_os(MODEL_POLICY_LANE_ENV);
        Self::from_marker(marker.as_deref(), ManagedProviderBuildMode::current())
    }

    pub(crate) fn from_marker(
        marker: Option<&OsStr>,
        build_mode: ManagedProviderBuildMode,
    ) -> CoreResult<Self> {
        let Some(marker) = marker else {
            return match build_mode {
                ManagedProviderBuildMode::Debug => Ok(Self { lane: None }),
                ManagedProviderBuildMode::Release => Err(CodexErr::InvalidRequest(format!(
                    "model policy requires {MODEL_POLICY_LANE_ENV} for managed release execution; use the codex, cdxpro, or cdxspark launcher"
                ))),
            };
        };
        let marker = marker.to_str().ok_or_else(|| {
            CodexErr::InvalidRequest(format!(
                "model policy rejected non-UTF-8 {MODEL_POLICY_LANE_ENV}"
            ))
        })?;
        let lane = match marker.trim() {
            "subscription" => ManagedProviderLane::Subscription,
            "api" => ManagedProviderLane::Api,
            "spark" => ManagedProviderLane::Spark,
            _ => {
                return Err(CodexErr::InvalidRequest(format!(
                    "model policy rejected {MODEL_POLICY_LANE_ENV}={marker:?}; required subscription, api, or spark"
                )));
            }
        };
        Ok(Self { lane: Some(lane) })
    }

    pub(crate) const fn is_managed(self) -> bool {
        self.lane.is_some()
    }

    /// Rejects a provider override before credentials are loaded or refreshed.
    pub(crate) fn validate_before_auth(self, provider_info: &ModelProviderInfo) -> CoreResult<()> {
        let Some(lane) = self.lane else {
            return Ok(());
        };
        if provider_info.base_url.is_some() {
            return Err(lane.invalid(format!(
                "model provider base URL override {:?}; required unset",
                provider_info.base_url
            )));
        }
        let expected = ModelProviderInfo::create_openai_provider(/*base_url*/ None);
        let mut http_fallback = expected.clone();
        http_fallback.supports_websockets = false;
        let http_fallback_profile_allowed = matches!(
            lane,
            ManagedProviderLane::Subscription | ManagedProviderLane::Api
        ) && provider_info == &http_fallback;
        if provider_info != &expected && !http_fallback_profile_allowed {
            let required_profile = match lane {
                ManagedProviderLane::Subscription | ManagedProviderLane::Api => {
                    "the unmodified built-in OpenAI provider or its exact HTTP-fallback profile"
                }
                ManagedProviderLane::Spark => "the unmodified built-in OpenAI provider",
            };
            return Err(lane.invalid(format!(
                "modified model provider; required {required_profile}"
            )));
        }
        Ok(())
    }

    pub(crate) fn resolve_api_provider(
        self,
        provider_info: &ModelProviderInfo,
        auth_mode: Option<AuthMode>,
    ) -> CoreResult<Provider> {
        self.validate_before_auth(provider_info)?;
        let api_provider = provider_info.to_api_provider(auth_mode)?;
        self.validate_resolved_setup(provider_info, auth_mode, &api_provider)?;
        Ok(api_provider)
    }

    /// Validates the resolved auth mode and the effective URL before transport.
    pub(crate) fn validate_resolved_setup(
        self,
        provider_info: &ModelProviderInfo,
        auth_mode: Option<AuthMode>,
        api_provider: &Provider,
    ) -> CoreResult<()> {
        let Some(lane) = self.lane else {
            return Ok(());
        };
        self.validate_before_auth(provider_info)?;
        let required_auth_mode = lane.required_auth_mode();
        if auth_mode != Some(required_auth_mode) {
            return Err(lane.invalid(format!(
                "resolved authentication mode {auth_mode:?}; required {required_auth_mode}"
            )));
        }
        let base_url = api_provider.base_url.trim_end_matches('/');
        let required_base_url = lane.required_base_url();
        if base_url != required_base_url {
            return Err(lane.invalid(format!(
                "effective API base URL `{base_url}`; required `{required_base_url}`"
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "model_policy_tests.rs"]
mod tests;
