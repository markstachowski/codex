use std::collections::HashMap;
#[cfg(unix)]
use std::fs::File;
use std::io::ErrorKind;
#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::fd::FromRawFd;
#[cfg(unix)]
use std::os::fd::RawFd;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::Path;
#[cfg(unix)]
use std::process::Command as StdCommand;
#[cfg(unix)]
use std::process::Stdio;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::AtomicBool;
#[cfg(target_os = "linux")]
use std::sync::mpsc as std_mpsc;
use std::time::Duration;

use anyhow::Result;
use portable_pty::CommandBuilder;
#[cfg(not(windows))]
use portable_pty::native_pty_system;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::process::ChildTerminator;
use crate::process::ProcessHandle;
use crate::process::ProcessSignal;
use crate::process::ProcessTerminationStrategy;
use crate::process::PtyHandles;
use crate::process::PtyMasterHandle;
use crate::process::SpawnedProcess;
use crate::process::TerminalSize;
#[cfg(unix)]
use crate::process::exit_code_from_status;

/// Returns true when ConPTY support is available (Windows only).
#[cfg(windows)]
pub fn conpty_supported() -> bool {
    crate::win::conpty_supported()
}

/// Returns true when ConPTY support is available (non-Windows always true).
#[cfg(not(windows))]
pub fn conpty_supported() -> bool {
    true
}

struct PtyChildTerminator {
    killer: Box<dyn portable_pty::ChildKiller + Send + Sync>,
    #[cfg(unix)]
    process_group_id: Option<u32>,
}

impl ChildTerminator for PtyChildTerminator {
    fn signal(&mut self, signal: ProcessSignal) -> std::io::Result<()> {
        match signal {
            ProcessSignal::Interrupt => {
                #[cfg(unix)]
                if let Some(process_group_id) = self.process_group_id {
                    return crate::process_group::interrupt_process_group(process_group_id);
                }

                Err(crate::process::unsupported_signal(signal))
            }
        }
    }

    fn kill(&mut self) -> std::io::Result<()> {
        #[cfg(unix)]
        if let Some(process_group_id) = self.process_group_id {
            // Match the pipe backend's hard-kill behavior so descendant
            // processes from interactive shells/REPLs do not survive shutdown.
            // Also try the direct child killer in case the cached PGID is stale.
            let process_group_kill_result =
                crate::process_group::kill_process_group(process_group_id);
            let child_kill_result = self.killer.kill();
            return match child_kill_result {
                Ok(()) => Ok(()),
                Err(err) if err.kind() == ErrorKind::NotFound => process_group_kill_result,
                Err(err) => process_group_kill_result.or(Err(err)),
            };
        }

        self.killer.kill()
    }
}

#[cfg(unix)]
struct RawPidTerminator {
    process_group_id: u32,
    #[cfg(target_os = "linux")]
    termination_request: LinuxPtyTerminationRequest,
}

#[cfg(target_os = "linux")]
enum LinuxPtyTerminationRequest {
    KillImmediately,
    GracefulThenKill {
        tx: Option<std_mpsc::Sender<()>>,
        state: Arc<StdMutex<LinuxPtyTerminationState>>,
    },
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LinuxPtyTerminationState {
    Running,
    TerminationRequested,
    Reaped(std::process::ExitStatus),
}

#[cfg(unix)]
impl ChildTerminator for RawPidTerminator {
    fn signal(&mut self, signal: ProcessSignal) -> std::io::Result<()> {
        match signal {
            ProcessSignal::Interrupt => {
                crate::process_group::interrupt_process_group(self.process_group_id)
            }
        }
    }

    fn kill(&mut self) -> std::io::Result<()> {
        #[cfg(target_os = "linux")]
        match &mut self.termination_request {
            LinuxPtyTerminationRequest::KillImmediately => {}
            LinuxPtyTerminationRequest::GracefulThenKill { tx, state } => {
                if let Ok(mut state) = state.lock()
                    && matches!(*state, LinuxPtyTerminationState::Running)
                {
                    *state = LinuxPtyTerminationState::TerminationRequested;
                    if let Some(tx) = tx.take() {
                        let _ = tx.send(());
                    }
                }
                return Ok(());
            }
        }

        crate::process_group::kill_process_group(self.process_group_id)
    }
}

#[cfg(target_os = "linux")]
struct LinuxRawPtyChildGuard {
    child: std::process::Child,
    process_group_id: u32,
    _pty_keepalive: Option<File>,
    armed: bool,
}

#[cfg(target_os = "linux")]
impl LinuxRawPtyChildGuard {
    fn new(child: std::process::Child, process_group_id: u32) -> Self {
        Self {
            child,
            process_group_id,
            _pty_keepalive: None,
            armed: true,
        }
    }

    fn keep_pty_open(&mut self, pty_keepalive: File) {
        self._pty_keepalive = Some(pty_keepalive);
    }

    fn hard_kill(&mut self) {
        let _ = crate::process_group::kill_process_group(self.process_group_id);
        let _ = self.child.kill();
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

#[cfg(target_os = "linux")]
impl Drop for LinuxRawPtyChildGuard {
    fn drop(&mut self) {
        if self.armed {
            self.hard_kill();
            let _ = self.child.wait();
        }
    }
}

#[cfg(target_os = "linux")]
fn terminate_linux_raw_pty_child(
    child: &mut LinuxRawPtyChildGuard,
    grace_period: Duration,
) -> std::io::Result<std::process::ExitStatus> {
    let _ = crate::process_group::terminate_process_group(child.process_group_id);
    // Do not poll or reap during the grace period: the owned zombie reserves
    // the exact PID and PGID until hard-kill escalation has completed.
    std::thread::sleep(grace_period);
    child.hard_kill();
    child.child.wait()
}

#[cfg(target_os = "linux")]
fn wait_for_linux_raw_pty_child(
    mut child: LinuxRawPtyChildGuard,
    graceful_termination: Option<(
        std_mpsc::Receiver<()>,
        Duration,
        Arc<StdMutex<LinuxPtyTerminationState>>,
    )>,
) -> std::io::Result<std::process::ExitStatus> {
    const EXIT_POLL_INTERVAL: Duration = Duration::from_millis(10);

    let wait_result = if let Some((termination_rx, grace_period, state)) = graceful_termination {
        loop {
            let termination_requested = {
                let mut state = state
                    .lock()
                    .map_err(|_| std::io::Error::other("PTY termination state poisoned"))?;
                match *state {
                    LinuxPtyTerminationState::Running => {
                        if let Some(status) = child.child.try_wait()? {
                            *state = LinuxPtyTerminationState::Reaped(status);
                            break Ok(status);
                        }
                        false
                    }
                    LinuxPtyTerminationState::TerminationRequested => true,
                    LinuxPtyTerminationState::Reaped(status) => break Ok(status),
                }
            };
            if termination_requested {
                match terminate_linux_raw_pty_child(&mut child, grace_period) {
                    Ok(status) => {
                        if let Ok(mut state) = state.lock() {
                            *state = LinuxPtyTerminationState::Reaped(status);
                        }
                        break Ok(status);
                    }
                    Err(err) => break Err(err),
                }
            }

            match termination_rx.recv_timeout(EXIT_POLL_INTERVAL) {
                Ok(()) | Err(std_mpsc::RecvTimeoutError::Timeout) => {}
                Err(std_mpsc::RecvTimeoutError::Disconnected) => {
                    break Err(std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        "PTY termination requester dropped before child exit",
                    ));
                }
            }
        }
    } else {
        child.child.wait()
    };
    if wait_result.is_ok() {
        child.disarm();
    }
    wait_result
}

fn platform_native_pty_system() -> Box<dyn portable_pty::PtySystem + Send> {
    #[cfg(windows)]
    {
        Box::new(crate::win::ConPtySystem::default())
    }

    #[cfg(not(windows))]
    {
        native_pty_system()
    }
}

/// Spawn a process attached to a PTY, preserving selected inherited file
/// descriptors across exec on Unix.
pub async fn spawn_process(
    program: &str,
    args: &[String],
    cwd: &Path,
    env: &HashMap<String, String>,
    arg0: &Option<String>,
    size: TerminalSize,
    inherited_fds: &[i32],
) -> Result<SpawnedProcess> {
    spawn_process_with_termination_strategy(
        program,
        args,
        cwd,
        env,
        arg0,
        size,
        inherited_fds,
        ProcessTerminationStrategy::KillImmediately,
    )
    .await
}

/// Spawn a PTY-backed process with an explicit termination strategy.
#[allow(clippy::too_many_arguments)]
pub async fn spawn_process_with_termination_strategy(
    program: &str,
    args: &[String],
    cwd: &Path,
    env: &HashMap<String, String>,
    arg0: &Option<String>,
    size: TerminalSize,
    inherited_fds: &[i32],
    termination_strategy: ProcessTerminationStrategy,
) -> Result<SpawnedProcess> {
    if program.is_empty() {
        anyhow::bail!("missing program for PTY spawn");
    }

    #[cfg(not(unix))]
    let _ = inherited_fds;

    #[cfg(target_os = "linux")]
    let use_raw_unix_pty = !inherited_fds.is_empty()
        || matches!(
            termination_strategy,
            ProcessTerminationStrategy::GracefulThenKill { .. }
        );
    #[cfg(all(unix, not(target_os = "linux")))]
    let use_raw_unix_pty = !inherited_fds.is_empty();
    #[cfg(unix)]
    if use_raw_unix_pty {
        return spawn_process_preserving_fds(
            program,
            args,
            cwd,
            env,
            arg0,
            size,
            inherited_fds,
            termination_strategy,
        )
        .await;
    }

    #[cfg(not(target_os = "linux"))]
    let _ = termination_strategy;

    spawn_process_portable(program, args, cwd, env, arg0, size).await
}

async fn spawn_process_portable(
    program: &str,
    args: &[String],
    cwd: &Path,
    env: &HashMap<String, String>,
    arg0: &Option<String>,
    size: TerminalSize,
) -> Result<SpawnedProcess> {
    let pty_system = platform_native_pty_system();
    let pair = pty_system.openpty(size.into())?;

    let mut command_builder = CommandBuilder::new(arg0.as_ref().unwrap_or(&program.to_string()));
    command_builder.cwd(cwd);
    command_builder.env_clear();
    for arg in args {
        command_builder.arg(arg);
    }
    for (key, value) in env {
        command_builder.env(key, value);
    }

    let mut child = pair.slave.spawn_command(command_builder)?;
    #[cfg(unix)]
    // portable-pty establishes the spawned PTY child as a new session leader on
    // Unix, so PID == PGID and we can reuse the pipe backend's process-group
    // hard-kill semantics for descendants.
    let process_group_id = child.process_id();
    let killer = child.clone_killer();

    let (writer_tx, mut writer_rx) = mpsc::channel::<Vec<u8>>(128);
    let (stdout_tx, stdout_rx) = mpsc::channel::<Vec<u8>>(128);
    let (_stderr_tx, stderr_rx) = mpsc::channel::<Vec<u8>>(1);
    let mut reader = pair.master.try_clone_reader()?;
    let reader_handle: JoinHandle<()> = tokio::task::spawn_blocking(move || {
        let mut buf = [0u8; 8_192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let _ = stdout_tx.blocking_send(buf[..n].to_vec());
                }
                Err(ref e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(ref e) if e.kind() == ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                }
                Err(_) => break,
            }
        }
    });

    let writer = pair.master.take_writer()?;
    let writer = Arc::new(tokio::sync::Mutex::new(writer));
    let writer_handle: JoinHandle<()> = tokio::spawn({
        let writer = Arc::clone(&writer);
        async move {
            #[cfg(windows)]
            let mut windows_input = crate::WindowsTtyInputNormalizer::default();
            while let Some(bytes) = writer_rx.recv().await {
                #[cfg(windows)]
                let bytes = windows_input.normalize(&bytes);
                let mut guard = writer.lock().await;
                use std::io::Write;
                let _ = guard.write_all(&bytes);
                let _ = guard.flush();
            }
        }
    });

    let (exit_tx, exit_rx) = oneshot::channel::<i32>();
    let exit_status = Arc::new(AtomicBool::new(false));
    let wait_exit_status = Arc::clone(&exit_status);
    let exit_code = Arc::new(StdMutex::new(None));
    let wait_exit_code = Arc::clone(&exit_code);
    let wait_handle: JoinHandle<()> = tokio::task::spawn_blocking(move || {
        let code = match child.wait() {
            Ok(status) => status.exit_code() as i32,
            Err(_) => -1,
        };
        wait_exit_status.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Ok(mut guard) = wait_exit_code.lock() {
            *guard = Some(code);
        }
        let _ = exit_tx.send(code);
    });

    let handles = PtyHandles {
        _slave: if cfg!(windows) {
            Some(pair.slave)
        } else {
            None
        },
        _master: PtyMasterHandle::Resizable(pair.master),
    };

    let handle = ProcessHandle::new(
        writer_tx,
        Box::new(PtyChildTerminator {
            killer,
            #[cfg(unix)]
            process_group_id,
        }),
        reader_handle,
        Vec::new(),
        writer_handle,
        wait_handle,
        exit_status,
        exit_code,
        Some(handles),
        /*resizer*/ None,
    );

    Ok(SpawnedProcess {
        session: handle,
        stdout_rx,
        stderr_rx,
        exit_rx,
    })
}

#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
async fn spawn_process_preserving_fds(
    program: &str,
    args: &[String],
    cwd: &Path,
    env: &HashMap<String, String>,
    arg0: &Option<String>,
    size: TerminalSize,
    inherited_fds: &[RawFd],
    termination_strategy: ProcessTerminationStrategy,
) -> Result<SpawnedProcess> {
    let (master, slave) = open_unix_pty(size)?;
    let mut command = StdCommand::new(program);
    if let Some(arg0) = arg0 {
        command.arg0(arg0);
    }
    command.current_dir(cwd);
    command.env_clear();
    for arg in args {
        command.arg(arg);
    }
    for (key, value) in env {
        command.env(key, value);
    }

    // The child should see one terminal on all three stdio streams. Cloning
    // the slave fd gives us three owned handles to the same PTY slave device
    // so Command can wire them up independently as stdin/stdout/stderr.
    let stdin = slave.try_clone()?;
    let stdout = slave.try_clone()?;
    let stderr = slave.try_clone()?;
    let inherited_fds = inherited_fds.to_vec();
    #[cfg(target_os = "linux")]
    let parent_pid = unsafe { libc::getpid() };

    unsafe {
        command
            .stdin(Stdio::from(stdin))
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .pre_exec(move || {
                for signo in &[
                    libc::SIGCHLD,
                    libc::SIGHUP,
                    libc::SIGINT,
                    libc::SIGQUIT,
                    libc::SIGTERM,
                    libc::SIGALRM,
                ] {
                    libc::signal(*signo, libc::SIG_DFL);
                }

                let empty_set: libc::sigset_t = std::mem::zeroed();
                libc::sigprocmask(libc::SIG_SETMASK, &empty_set, std::ptr::null_mut());

                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }

                // stdin now refers to the PTY slave, so make that fd the
                // controlling terminal for the child's new session. stdout and
                // stderr point at clones of the same slave device.
                #[allow(clippy::cast_lossless)]
                if libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }

                #[cfg(target_os = "linux")]
                crate::process_group::set_parent_death_signal_to_kill(parent_pid)?;

                close_inherited_fds_except(&inherited_fds);
                Ok(())
            });
    }

    #[cfg(target_os = "linux")]
    let child = command.spawn()?;
    #[cfg(not(target_os = "linux"))]
    let mut child = command.spawn()?;
    drop(slave);
    let process_group_id = child.id();
    #[cfg(target_os = "linux")]
    let (termination_request, graceful_termination) = match termination_strategy {
        ProcessTerminationStrategy::KillImmediately => {
            (LinuxPtyTerminationRequest::KillImmediately, None)
        }
        ProcessTerminationStrategy::GracefulThenKill { grace_period } => {
            let (tx, rx) = std_mpsc::channel();
            let state = Arc::new(StdMutex::new(LinuxPtyTerminationState::Running));
            (
                LinuxPtyTerminationRequest::GracefulThenKill {
                    tx: Some(tx),
                    state: Arc::clone(&state),
                },
                Some((rx, grace_period, state)),
            )
        }
    };
    #[cfg(not(target_os = "linux"))]
    let _ = termination_strategy;

    // Arm child cleanup before any post-spawn fallible setup, then complete all
    // PTY cloning before transferring ownership to the waiter.
    #[cfg(target_os = "linux")]
    let mut child = LinuxRawPtyChildGuard::new(child, process_group_id);
    let mut reader = master.try_clone()?;
    let writer = Arc::new(tokio::sync::Mutex::new(master.try_clone()?));
    #[cfg(target_os = "linux")]
    child.keep_pty_open(master.try_clone()?);

    let (exit_tx, exit_rx) = oneshot::channel::<i32>();
    let exit_status = Arc::new(AtomicBool::new(false));
    let wait_exit_status = Arc::clone(&exit_status);
    let exit_code = Arc::new(StdMutex::new(None));
    let wait_exit_code = Arc::clone(&exit_code);
    // Enqueue the owning waiter before any blocking reader. This ensures a
    // saturated one-thread blocking pool cannot strand the child behind PTY I/O.
    let wait_handle: JoinHandle<()> = tokio::task::spawn_blocking(move || {
        #[cfg(target_os = "linux")]
        let wait_result = wait_for_linux_raw_pty_child(child, graceful_termination);
        #[cfg(not(target_os = "linux"))]
        let wait_result = child.wait();
        let code = match wait_result {
            Ok(status) => exit_code_from_status(status),
            Err(_) => -1,
        };
        wait_exit_status.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Ok(mut guard) = wait_exit_code.lock() {
            *guard = Some(code);
        }
        let _ = exit_tx.send(code);
    });

    let (writer_tx, mut writer_rx) = mpsc::channel::<Vec<u8>>(128);
    let (stdout_tx, stdout_rx) = mpsc::channel::<Vec<u8>>(128);
    let (_stderr_tx, stderr_rx) = mpsc::channel::<Vec<u8>>(1);
    let reader_handle: JoinHandle<()> = tokio::task::spawn_blocking(move || {
        let mut buf = [0u8; 8_192];
        loop {
            match std::io::Read::read(&mut reader, &mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let _ = stdout_tx.blocking_send(buf[..n].to_vec());
                }
                Err(ref e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(ref e) if e.kind() == ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                }
                Err(_) => break,
            }
        }
    });

    let writer_handle: JoinHandle<()> = tokio::spawn({
        let writer = Arc::clone(&writer);
        async move {
            while let Some(bytes) = writer_rx.recv().await {
                let mut guard = writer.lock().await;
                use std::io::Write;
                let _ = guard.write_all(&bytes);
                let _ = guard.flush();
            }
        }
    });

    let handles = PtyHandles {
        _slave: None,
        _master: PtyMasterHandle::Opaque {
            raw_fd: master.as_raw_fd(),
            _handle: Box::new(master),
        },
    };

    let handle = ProcessHandle::new(
        writer_tx,
        Box::new(RawPidTerminator {
            process_group_id,
            #[cfg(target_os = "linux")]
            termination_request,
        }),
        reader_handle,
        Vec::new(),
        writer_handle,
        wait_handle,
        exit_status,
        exit_code,
        Some(handles),
        /*resizer*/ None,
    );

    Ok(SpawnedProcess {
        session: handle,
        stdout_rx,
        stderr_rx,
        exit_rx,
    })
}

#[cfg(unix)]
fn open_unix_pty(size: TerminalSize) -> Result<(File, File)> {
    let mut master: RawFd = -1;
    let mut slave: RawFd = -1;
    let mut size = libc::winsize {
        ws_row: size.rows,
        ws_col: size.cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let winp = std::ptr::addr_of_mut!(size);

    let result = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            winp,
        )
    };
    if result != 0 {
        anyhow::bail!("failed to openpty: {:?}", std::io::Error::last_os_error());
    }

    set_cloexec(master)?;
    set_cloexec(slave)?;

    Ok(unsafe { (File::from_raw_fd(master), File::from_raw_fd(slave)) })
}

#[cfg(unix)]
fn set_cloexec(fd: RawFd) -> std::io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags == -1 {
        return Err(std::io::Error::last_os_error());
    }
    let result = unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) };
    if result == -1 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
pub(crate) fn close_inherited_fds_except(preserved_fds: &[RawFd]) {
    if let Ok(dir) = std::fs::read_dir("/dev/fd") {
        let mut fds = Vec::new();
        for entry in dir {
            let num = entry
                .ok()
                .map(|entry| entry.file_name())
                .and_then(|name| name.into_string().ok())
                .and_then(|name| name.parse::<RawFd>().ok());
            if let Some(num) = num {
                if num <= 2 || preserved_fds.contains(&num) {
                    continue;
                }
                // Keep CLOEXEC descriptors open so std::process can still use
                // its internal exec-error pipe to report spawn failures.
                let flags = unsafe { libc::fcntl(num, libc::F_GETFD) };
                if flags == -1 || flags & libc::FD_CLOEXEC != 0 {
                    continue;
                }
                fds.push(num);
            }
        }
        for fd in fds {
            unsafe {
                libc::close(fd);
            }
        }
    }
}
