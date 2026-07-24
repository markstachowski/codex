use super::*;
use crate::app::test_support::make_test_app_with_event_receiver;
use codex_app_server_protocol::ThreadLoadedListParams;
use pretty_assertions::assert_eq;

#[tokio::test]
#[serial_test::serial]
async fn invalid_managed_worktree_defaults_preserve_current_thread() -> Result<()> {
    struct RestoreLane(Option<std::ffi::OsString>);
    impl Drop for RestoreLane {
        fn drop(&mut self) {
            // SAFETY: nextest isolates this serialized test and the guard restores its marker.
            unsafe {
                match self.0.take() {
                    Some(value) => std::env::set_var("CDX_MODEL_POLICY_LANE", value),
                    None => std::env::remove_var("CDX_MODEL_POLICY_LANE"),
                }
            }
        }
    }

    let lane_guard = RestoreLane(std::env::var_os("CDX_MODEL_POLICY_LANE"));
    // SAFETY: fixture construction intentionally starts unmanaged under the restoring guard.
    unsafe { std::env::remove_var("CDX_MODEL_POLICY_LANE") };
    let (mut app, mut events) = make_test_app_with_event_receiver().await;
    let mut server = crate::start_embedded_app_server_for_picker(&app.config).await?;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    app.start_fresh_session_with_summary_hint(
        &mut tui,
        &mut server,
        /*session_start_source*/ None,
        /*initial_user_message*/ None,
        /*new_thread_name*/ None,
    )
    .await;
    let original = app.chat_widget.thread_id().expect("initial thread");
    let original_cwd = app.config.cwd.clone();
    let loaded_before = server
        .thread_loaded_list(ThreadLoadedListParams::default())
        .await?;
    let destination = tempfile::tempdir()?;
    let cwd = AbsolutePathBuf::from_absolute_path(destination.path())?;
    let mut config = app.config.clone();
    config.cwd = cwd.clone();
    config.active_project.trust_level = Some(codex_protocol::config_types::TrustLevel::Trusted);
    let manager = codex_worktree::WorktreeManager::new(
        codex_worktree::WorktreeSettings::for_cli(&app.config.codex_home, /*desktop*/ None)
            .map_err(|error| color_eyre::eyre::eyre!(error.to_string()))?,
    );
    let checkout = codex_worktree::ManagedWorktree {
        root: cwd.to_path_buf(),
        cwd: cwd.to_path_buf(),
        source_root: original_cwd.to_path_buf(),
        source_cwd: original_cwd.to_path_buf(),
        head_sha: String::new(),
        branch: None,
    };
    while events.try_recv().is_ok() {}
    // SAFETY: exercise the real fallible defaults boundary, then restore before shutdown.
    unsafe { std::env::set_var("CDX_MODEL_POLICY_LANE", "unsupported") };
    app.change_working_directory_with_managed(
        &mut tui,
        &mut server,
        cwd,
        Some((
            manager,
            checkout,
            crate::app_event::ManagedWorktreeMode::New,
            None,
        )),
        DestinationConfig::Prepared(Box::new(config)),
    )
    .await;
    drop(lane_guard);
    assert_eq!(app.chat_widget.thread_id(), Some(original));
    assert_eq!(app.config.cwd, original_cwd);
    assert_eq!(
        server
            .thread_loaded_list(ThreadLoadedListParams::default())
            .await?,
        loaded_before
    );
    let rendered = std::iter::from_fn(|| events.try_recv().ok())
        .filter_map(|event| match event {
            AppEvent::InsertHistoryCell(cell) => Some(
                cell.display_lines(/*width*/ 160)
                    .iter()
                    .map(|line| {
                        line.spans
                            .iter()
                            .map(|span| span.content.as_ref())
                            .collect::<String>()
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        rendered.contains("Failed to validate managed new-worktree defaults"),
        "{rendered}"
    );
    insta::assert_snapshot!("invalid_managed_worktree_defaults", rendered);
    server.shutdown().await?;
    Ok(())
}
