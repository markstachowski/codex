use std::ffi::OsStr;
use std::process::Command;
use std::process::Stdio;

use super::Args;
use super::MODEL_POLICY_LANE_ENV;
use super::ModelPolicyMarkerRequirement;
use super::reject_managed_model_policy_lane;
use super::run_main;

const POLICY_TEST_CHILD_ENV: &str = "CODEX_RESPONSES_API_PROXY_POLICY_TEST_CHILD";
const POLICY_TEST_NAME: &str =
    "model_policy_tests::managed_marker_rejects_proxy_before_auth_or_transport";

#[test]
fn managed_policy_marker_contract_is_exact() {
    reject_managed_model_policy_lane(
        /*lane*/ None,
        ModelPolicyMarkerRequirement::OptionalForUnmanaged,
    )
    .expect("the standalone unmanaged proxy may run without a lane marker");

    let error = reject_managed_model_policy_lane(
        /*lane*/ None,
        ModelPolicyMarkerRequirement::RequiredForManagedRelease,
    )
    .expect_err("a managed release entrypoint must require its lane marker");
    assert!(error.to_string().contains(MODEL_POLICY_LANE_ENV));

    for value in ["subscription", "api", "spark", "", "unknown"] {
        let error = reject_managed_model_policy_lane(
            Some(OsStr::new(value)),
            ModelPolicyMarkerRequirement::OptionalForUnmanaged,
        )
        .expect_err("every present lane marker must block the raw proxy");
        assert!(error.to_string().contains("unavailable"));
    }

    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;

        let error = reject_managed_model_policy_lane(
            Some(OsStr::from_bytes(b"api\xff")),
            ModelPolicyMarkerRequirement::OptionalForUnmanaged,
        )
        .expect_err("a non-UTF-8 lane marker must block the raw proxy");
        assert!(error.to_string().contains("unavailable"));
    }
}

#[test]
fn managed_marker_rejects_proxy_before_auth_or_transport() {
    if std::env::var_os(POLICY_TEST_CHILD_ENV).is_some() {
        let error = run_main(Args {
            port: None,
            server_info: None,
            http_shutdown: false,
            upstream_url: "not an upstream URL".to_string(),
            dump_dir: None,
        })
        .expect_err("a managed marker must reject the proxy");
        assert!(error.to_string().contains("unavailable"));
        return;
    }

    for lane in ["subscription", "api", "spark"] {
        let output = Command::new(
            std::env::current_exe().expect("current test executable should be available"),
        )
        .arg("--exact")
        .arg(POLICY_TEST_NAME)
        .arg("--nocapture")
        .env(POLICY_TEST_CHILD_ENV, /*value*/ "1")
        .env(MODEL_POLICY_LANE_ENV, lane)
        .stdin(Stdio::null())
        .output()
        .expect("managed proxy policy child should run");

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "managed proxy policy child for {lane} failed\nstdout:\n{stdout}\nstderr:\n{stderr}",
        );
        assert!(
            stdout.contains("1 passed"),
            "managed proxy policy child for {lane} did not execute exactly one test\nstdout:\n{stdout}\nstderr:\n{stderr}",
        );
    }
}
