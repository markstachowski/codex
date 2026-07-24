use std::collections::HashMap;
use std::ffi::OsStr;

use codex_model_provider_info::ModelProviderInfo;
use codex_protocol::auth::AuthMode;
use pretty_assertions::assert_eq;

use super::ManagedProviderBuildMode;
use super::ManagedProviderPolicy;

#[test]
fn exact_managed_provider_setups_are_accepted() {
    let cases = [
        (
            "subscription",
            AuthMode::Chatgpt,
            "https://chatgpt.com/backend-api/codex",
        ),
        ("api", AuthMode::ApiKey, "https://api.openai.com/v1"),
        (
            "spark",
            AuthMode::Chatgpt,
            "https://chatgpt.com/backend-api/codex",
        ),
    ];

    for (marker, auth_mode, expected_base_url) in cases {
        let policy = ManagedProviderPolicy::from_marker(
            Some(OsStr::new(marker)),
            ManagedProviderBuildMode::Release,
        )
        .expect("known managed lane");
        let provider_info = ModelProviderInfo::create_openai_provider(/*base_url*/ None);
        policy
            .validate_before_auth(&provider_info)
            .expect("unmodified provider should pass before auth");
        let api_provider = provider_info
            .to_api_provider(Some(auth_mode))
            .expect("built-in provider should resolve");
        policy
            .validate_resolved_setup(&provider_info, Some(auth_mode), &api_provider)
            .expect("exact managed provider setup should pass");
        assert_eq!(api_provider.base_url, expected_base_url);
    }
}

#[test]
fn exact_http_fallback_provider_setups_are_accepted_for_subscription_and_api() {
    let cases = [
        ("subscription", AuthMode::Chatgpt),
        ("api", AuthMode::ApiKey),
    ];

    for (marker, auth_mode) in cases {
        let policy = ManagedProviderPolicy::from_marker(
            Some(OsStr::new(marker)),
            ManagedProviderBuildMode::Release,
        )
        .expect("known managed lane");
        let mut provider_info = ModelProviderInfo::create_openai_provider(/*base_url*/ None);
        provider_info.supports_websockets = false;

        policy
            .validate_before_auth(&provider_info)
            .expect("exact managed HTTP-fallback provider should pass before auth");
        let api_provider = provider_info
            .to_api_provider(Some(auth_mode))
            .expect("built-in provider should resolve");
        policy
            .validate_resolved_setup(&provider_info, Some(auth_mode), &api_provider)
            .expect("exact managed HTTP-fallback provider setup should pass");
    }
}

#[test]
fn spark_rejects_http_fallback_provider_setups() {
    let policy = ManagedProviderPolicy::from_marker(
        Some(OsStr::new("spark")),
        ManagedProviderBuildMode::Release,
    )
    .expect("known managed lane");
    let mut provider_info = ModelProviderInfo::create_openai_provider(/*base_url*/ None);
    provider_info.supports_websockets = false;

    policy
        .validate_before_auth(&provider_info)
        .expect_err("root-only Spark must reject the HTTP-fallback provider profile");
}

#[test]
fn managed_provider_policy_rejects_mutated_http_fallback_profiles() {
    let policy = ManagedProviderPolicy::from_marker(
        Some(OsStr::new("subscription")),
        ManagedProviderBuildMode::Release,
    )
    .expect("known managed lane");
    let mut http_fallback = ModelProviderInfo::create_openai_provider(/*base_url*/ None);
    http_fallback.supports_websockets = false;

    let mut endpoint_override = http_fallback.clone();
    endpoint_override.base_url = Some("https://example.invalid/v1".to_string());
    let error = policy
        .validate_before_auth(&endpoint_override)
        .expect_err("HTTP-fallback endpoint override must fail closed");
    assert!(error.to_string().contains("base URL override"));

    let mut header_override = http_fallback.clone();
    header_override
        .http_headers
        .as_mut()
        .expect("built-in version header")
        .insert("x-managed-override".to_string(), "rejected".to_string());
    let mut query_override = http_fallback.clone();
    query_override.query_params = Some(HashMap::from([(
        "managed-override".to_string(),
        "rejected".to_string(),
    )]));
    let mut auth_override = http_fallback.clone();
    auth_override.requires_openai_auth = !auth_override.requires_openai_auth;
    let mut capability_override = http_fallback;
    capability_override.supports_standalone_web_search =
        !capability_override.supports_standalone_web_search;

    for (label, provider_info) in [
        ("header override", header_override),
        ("query override", query_override),
        ("authentication override", auth_override),
        ("capability override", capability_override),
    ] {
        let error = policy
            .validate_before_auth(&provider_info)
            .expect_err(label);
        assert!(error.to_string().contains("modified model provider"));
    }
}

#[test]
fn managed_provider_policy_rejects_overrides_before_auth() {
    let policy = ManagedProviderPolicy::from_marker(
        Some(OsStr::new("subscription")),
        ManagedProviderBuildMode::Release,
    )
    .expect("known managed lane");

    let mut explicit_official_url = ModelProviderInfo::create_openai_provider(/*base_url*/ None);
    explicit_official_url.supports_websockets = false;
    explicit_official_url.base_url = Some("https://chatgpt.com/backend-api/codex".into());
    let error = policy
        .validate_before_auth(&explicit_official_url)
        .expect_err("managed providers must leave base URL selection to resolved auth");
    assert!(error.to_string().contains("base URL override"));

    let mut modified = ModelProviderInfo::create_openai_provider(/*base_url*/ None);
    modified.requires_openai_auth = false;
    let error = policy
        .validate_before_auth(&modified)
        .expect_err("managed providers must be the exact built-in provider");
    assert!(error.to_string().contains("modified model provider"));
}

#[test]
fn managed_provider_policy_rejects_wrong_auth_and_effective_url() {
    let policy = ManagedProviderPolicy::from_marker(
        Some(OsStr::new("api")),
        ManagedProviderBuildMode::Release,
    )
    .expect("known managed lane");
    let provider_info = ModelProviderInfo::create_openai_provider(/*base_url*/ None);
    let api_provider = provider_info
        .to_api_provider(Some(AuthMode::Chatgpt))
        .expect("built-in provider should resolve");
    let error = policy
        .validate_resolved_setup(&provider_info, Some(AuthMode::Chatgpt), &api_provider)
        .expect_err("API lane must reject ChatGPT auth");
    assert!(error.to_string().contains("authentication mode"));

    let mut wrong_url = provider_info
        .to_api_provider(Some(AuthMode::ApiKey))
        .expect("built-in provider should resolve");
    wrong_url.base_url = "https://example.invalid/v1".to_string();
    let error = policy
        .validate_resolved_setup(&provider_info, Some(AuthMode::ApiKey), &wrong_url)
        .expect_err("managed provider must reject an unexpected effective URL");
    assert!(error.to_string().contains("effective API base URL"));
}

#[test]
fn unmanaged_provider_policy_preserves_overrides() {
    let policy =
        ManagedProviderPolicy::from_marker(/*marker*/ None, ManagedProviderBuildMode::Debug)
            .expect("missing marker denotes unmanaged debug execution");
    let provider_info =
        ModelProviderInfo::create_openai_provider(Some("https://example.invalid/v1".into()));
    let api_provider = provider_info
        .to_api_provider(Some(AuthMode::Headers))
        .expect("unmanaged provider should resolve");

    policy
        .validate_before_auth(&provider_info)
        .expect("unmanaged provider overrides remain supported");
    policy
        .validate_resolved_setup(&provider_info, Some(AuthMode::Headers), &api_provider)
        .expect("unmanaged auth and URL remain supported");
}

#[test]
fn managed_provider_policy_rejects_unknown_marker() {
    let error = ManagedProviderPolicy::from_marker(
        Some(OsStr::new("automatic")),
        ManagedProviderBuildMode::Release,
    )
    .expect_err("unknown managed marker must fail closed");
    assert!(
        error
            .to_string()
            .contains("required subscription, api, or spark")
    );
}

#[test]
fn managed_release_rejects_missing_marker() {
    let error =
        ManagedProviderPolicy::from_marker(/*marker*/ None, ManagedProviderBuildMode::Release)
            .expect_err("managed release execution must require an explicit lane marker");

    assert!(error.to_string().contains("managed release execution"));
}

#[cfg(unix)]
#[test]
fn managed_provider_policy_rejects_non_utf8_marker() {
    use std::os::unix::ffi::OsStrExt;

    let error = ManagedProviderPolicy::from_marker(
        Some(OsStr::from_bytes(b"api\xff")),
        ManagedProviderBuildMode::Release,
    )
    .expect_err("non-UTF-8 managed marker must fail closed");
    assert!(error.to_string().contains("non-UTF-8"));
}
