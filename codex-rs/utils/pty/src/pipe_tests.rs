use super::*;
#[cfg(target_os = "linux")]
use pretty_assertions::assert_eq;

#[cfg(windows)]
#[test]
fn process_fallback_interrupt_terminates_root() -> anyhow::Result<()> {
    let mut child = std::process::Command::new("ping.exe")
        .args(["-n", "60", "127.0.0.1"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let mut terminator = PipeChildTerminator {
        windows: WindowsChildTerminator::Process(child.id()),
    };

    terminator.signal(ProcessSignal::Interrupt)?;

    assert!(!child.wait()?.success());
    Ok(())
}

#[cfg(target_os = "linux")]
struct TestDir {
    path: std::path::PathBuf,
}

#[cfg(target_os = "linux")]
impl TestDir {
    fn new(name: &str) -> anyhow::Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "codex-pipe-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        std::fs::create_dir(&path)?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(target_os = "linux")]
impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[cfg(target_os = "linux")]
async fn wait_for_file(path: &Path) -> anyhow::Result<()> {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .map_err(|_| anyhow::anyhow!("timed out waiting for {}", path.display()))
}

#[cfg(target_os = "linux")]
async fn wait_for_process_exit(pid: libc::pid_t) -> anyhow::Result<()> {
    tokio::time::timeout(Duration::from_secs(2), async move {
        loop {
            let result = unsafe { libc::kill(pid, 0) };
            if result == -1 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .map_err(|_| anyhow::anyhow!("process {pid} survived termination"))
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn graceful_termination_allows_term_trap_cleanup() -> anyhow::Result<()> {
    let test_dir = TestDir::new("graceful-cleanup")?;
    let ready_path = test_dir.path().join("ready");
    let cleanup_path = test_dir.path().join("cleaned");
    let mut env = std::env::vars().collect::<HashMap<_, _>>();
    env.insert("READY_PATH".to_string(), ready_path.display().to_string());
    env.insert(
        "CLEANUP_PATH".to_string(),
        cleanup_path.display().to_string(),
    );
    let args = vec![
        "-c".to_string(),
        concat!(
            "trap '' PIPE; ",
            "trap 'printf cleaned >\"$CLEANUP_PATH\"; exit 0' TERM; ",
            "printf ready >\"$READY_PATH\"; ",
            "while :; do sleep 1; done"
        )
        .to_string(),
    ];
    let spawned = spawn_process_with_termination_strategy(
        "/bin/sh",
        &args,
        test_dir.path(),
        &env,
        &None,
        &[],
        ProcessTerminationStrategy::GracefulThenKill {
            grace_period: Duration::from_millis(150),
        },
    )
    .await?;
    let SpawnedProcess {
        session,
        stdout_rx: _stdout_rx,
        stderr_rx: _stderr_rx,
        exit_rx,
    } = spawned;

    wait_for_file(&ready_path).await?;
    let started_at = tokio::time::Instant::now();
    session.terminate();
    let exit_code = tokio::time::timeout(Duration::from_secs(3), exit_rx)
        .await
        .map_err(|_| anyhow::anyhow!("timed out waiting for graceful termination"))??;

    assert_eq!(exit_code, 0);
    assert!(
        started_at.elapsed() >= Duration::from_millis(150),
        "graceful exit was reaped before the complete grace period"
    );
    assert_eq!(std::fs::read_to_string(&cleanup_path)?, "cleaned");
    Ok(())
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn graceful_termination_escalates_for_term_ignoring_tree() -> anyhow::Result<()> {
    let test_dir = TestDir::new("graceful-escalation")?;
    let child_pid_path = test_dir.path().join("child-pid");
    let ready_path = test_dir.path().join("ready");
    let mut env = std::env::vars().collect::<HashMap<_, _>>();
    env.insert(
        "CHILD_PID_PATH".to_string(),
        child_pid_path.display().to_string(),
    );
    env.insert("READY_PATH".to_string(), ready_path.display().to_string());
    let args = vec![
        "-c".to_string(),
        concat!(
            "trap '' TERM; ",
            "sh -c 'trap \"\" TERM; while :; do sleep 1; done' & ",
            "child=$!; printf '%s' \"$child\" >\"$CHILD_PID_PATH\"; ",
            "printf ready >\"$READY_PATH\"; wait"
        )
        .to_string(),
    ];
    let spawned = spawn_process_with_termination_strategy(
        "/bin/sh",
        &args,
        test_dir.path(),
        &env,
        &None,
        &[],
        ProcessTerminationStrategy::GracefulThenKill {
            grace_period: Duration::from_millis(150),
        },
    )
    .await?;
    let SpawnedProcess {
        session,
        stdout_rx: _stdout_rx,
        stderr_rx: _stderr_rx,
        exit_rx,
    } = spawned;

    wait_for_file(&ready_path).await?;
    let child_pid = std::fs::read_to_string(&child_pid_path)?.parse::<libc::pid_t>()?;
    let started_at = tokio::time::Instant::now();
    session.terminate();
    let exit_code = tokio::time::timeout(Duration::from_secs(4), exit_rx)
        .await
        .map_err(|_| anyhow::anyhow!("timed out waiting for hard-kill escalation"))??;

    assert_eq!(exit_code, 128 + libc::SIGKILL);
    assert!(
        started_at.elapsed() >= Duration::from_millis(150),
        "TERM-ignoring process exited before the escalation deadline"
    );
    wait_for_process_exit(child_pid).await?;
    Ok(())
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn graceful_termination_is_idempotent_after_normal_exit() -> anyhow::Result<()> {
    let env = std::env::vars().collect::<HashMap<_, _>>();
    let args = vec!["-c".to_string(), "exit 23".to_string()];
    let spawned = spawn_process_with_termination_strategy(
        "/bin/sh",
        &args,
        Path::new("."),
        &env,
        &None,
        &[],
        ProcessTerminationStrategy::GracefulThenKill {
            grace_period: Duration::from_millis(50),
        },
    )
    .await?;
    let SpawnedProcess {
        session,
        stdout_rx: _stdout_rx,
        stderr_rx: _stderr_rx,
        exit_rx,
    } = spawned;

    let exit_code = tokio::time::timeout(Duration::from_secs(2), exit_rx)
        .await
        .map_err(|_| anyhow::anyhow!("timed out waiting for normal exit"))??;
    assert_eq!(exit_code, 23);

    // The process has already been reaped and its completion cached. Repeated
    // termination requests must remain no-ops and preserve that status.
    session.request_terminate();
    session.request_terminate();
    assert_eq!(session.exit_code(), Some(23));
    Ok(())
}
