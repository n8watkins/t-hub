use super::*;

struct FakeHarnessCommand {
    fixture_dir: PathBuf,
    command: String,
}

impl FakeHarnessCommand {
    fn new(harness: Harness, flags: &str) -> Self {
        let fixture_dir = std::env::temp_dir().join(format!(
            "t-hub-dispatch-harness-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&fixture_dir).unwrap();
        let executable = fixture_dir.join(harness.as_provider());
        let invocation_path = fixture_dir.join("invoked");
        std::fs::write(
            &executable,
            format!(
                "#!/usr/bin/env node\nrequire('fs').writeFileSync({}, 'invoked\\n');\nsetInterval(() => {{}}, 1000);\n",
                serde_json::to_string(&invocation_path).unwrap()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let command = if flags.is_empty() {
            executable.display().to_string()
        } else {
            format!("{} {flags}", executable.display())
        };
        Self {
            fixture_dir,
            command,
        }
    }
}

impl Drop for FakeHarnessCommand {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.fixture_dir);
    }
}

struct FakeCodexObserver {
    fixture_dir: PathBuf,
    command: String,
    invocation_path: PathBuf,
    identity_path: PathBuf,
}

impl FakeCodexObserver {
    fn new(exit_code: i32) -> Self {
        let fixture_dir = std::env::temp_dir().join(format!(
            "t-hub-codex-observer-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&fixture_dir).unwrap();
        let executable = fixture_dir.join("t-hub-agent");
        let invocation_path = fixture_dir.join("invocation");
        let identity_path = fixture_dir.join("identity");
        std::fs::write(
            &executable,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" > '{}'\nprintf '%s\\t%s\\n' \"$TMUX_PANE\" \"$(tmux display-message -p '#{{session_name}}')\" > '{}'\nexit {exit_code}\n",
                invocation_path.display(),
                identity_path.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        Self {
            fixture_dir,
            command: format!("{} --codex-unobserved", executable.display()),
            invocation_path,
            identity_path,
        }
    }

    fn wait_for_invocation(&self) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !self.invocation_path.exists() || !self.identity_path.exists() {
            assert!(
                Instant::now() < deadline,
                "fake Codex observer was not invoked"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for FakeCodexObserver {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.fixture_dir);
    }
}

fn tmux_pane_identity(target: &str) -> String {
    let output = std::process::Command::new("tmux")
        .args([
            "-L",
            tmux::socket(),
            "display-message",
            "-p",
            "-t",
            target,
            "#{pane_id}\t#{session_name}",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn process_permission_attestation(
    expected: Harness,
    executable_name: &str,
    flags: &str,
) -> Option<Result<HarnessPermissionAttestation, crate::harness::LaunchAttestationError>> {
    if !std::process::Command::new("tmux")
        .arg("-V")
        .output()
        .is_ok_and(|output| output.status.success())
        || !std::process::Command::new("node")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
    {
        eprintln!("process_permission_attestation: tmux or node not on PATH - skipping");
        return None;
    }
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let fixture_dir = std::env::temp_dir().join(format!(
        "t-hub-permission-harness-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&fixture_dir).unwrap();
    let executable = fixture_dir.join(executable_name);
    let invocation = fixture_dir.join("invoked");
    std::fs::write(
        &executable,
        format!(
            "#!/usr/bin/env node\nrequire('fs').writeFileSync({}, 'invoked');\nsetInterval(() => {{}}, 1000);\n",
            serde_json::to_string(&invocation).unwrap()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    let session_id = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    let target = tmux_target(&session_id);
    create_test_tmux_session(&target).unwrap();
    let before = observe_harness_process(&target).unwrap();
    let command = format!("{} {} 'api_key=supersecret'", executable.display(), flags);
    tmux::send_text(&target, &command, true).unwrap();

    let deadline = Instant::now() + Duration::from_secs(30);
    while !invocation.exists() {
        assert!(
            Instant::now() < deadline,
            "fake Harness did not start its provider process"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let result = loop {
        if let Ok(after) = observe_harness_process(&target) {
            match attest_launch_permissions(
                expected.adapter(),
                &before,
                &after,
                PermMode::BypassPermissions,
            ) {
                Err(error)
                    if error == crate::harness::LaunchAttestationError::StaleEvidence
                        || (error == crate::harness::LaunchAttestationError::WrapperObscured
                            && executable_name != "wrapper") =>
                {
                    assert!(
                        Instant::now() < deadline,
                        "fake Harness did not produce process evidence"
                    );
                }
                result => break result,
            }
        }
        assert!(
            Instant::now() < deadline,
            "fake Harness did not produce process evidence"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    reap_test_tmux_session_and_assert_absent(&target);
    let _ = std::fs::remove_dir_all(fixture_dir);
    Some(result)
}

fn wait_for_process_permission_attestation(
    target: &str,
    before: &crate::harness::HarnessProcessEvidence,
    harness: Harness,
) -> Result<HarnessPermissionAttestation, crate::harness::LaunchAttestationError> {
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        if let Ok(after) = observe_harness_process(target) {
            match attest_launch_permissions(
                harness.adapter(),
                before,
                &after,
                PermMode::BypassPermissions,
            ) {
                Err(crate::harness::LaunchAttestationError::StaleEvidence)
                    if Instant::now() < deadline => {}
                result => return result,
            }
        }
        assert!(
            Instant::now() < deadline,
            "fake Harness did not replace the foreground shell"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn dispatch_crew_launches_both_harnesses_with_explicit_unrestricted_permissions() {
    let prompt = "work card";
    let codex = crew_launch_argv(Harness::Codex, prompt);
    let claude = crew_launch_argv(Harness::Claude, prompt);

    assert_eq!(
            codex,
            "t-hub-agent --codex-unobserved && exec codex --dangerously-bypass-approvals-and-sandbox 'work card'"
        );
    assert_eq!(claude, "claude --dangerously-skip-permissions 'work card'");
    assert_ne!(codex, Harness::Codex.adapter().fresh_argv(prompt));
    assert_ne!(claude, Harness::Claude.adapter().fresh_argv(prompt));
}

#[test]
fn process_level_permission_attestation_accepts_codex_and_claude_native_flags() {
    let Some(codex) = process_permission_attestation(
        Harness::Codex,
        "codex",
        "--dangerously-bypass-approvals-and-sandbox",
    ) else {
        return;
    };
    assert_eq!(codex.unwrap().permission, PermMode::BypassPermissions);

    let claude =
        process_permission_attestation(Harness::Claude, "claude", "--dangerously-skip-permissions")
            .unwrap();
    assert_eq!(claude.unwrap().permission, PermMode::BypassPermissions);
}

#[test]
fn process_level_permission_attestation_rejects_missing_wrong_provider_and_wrappers() {
    let Some(missing) = process_permission_attestation(Harness::Codex, "codex", "") else {
        return;
    };
    assert_eq!(
        missing.unwrap_err(),
        crate::harness::LaunchAttestationError::MissingPermission
    );

    let wrong =
        process_permission_attestation(Harness::Codex, "codex", "--sandbox workspace-write")
            .unwrap();
    assert_eq!(
        wrong.unwrap_err(),
        crate::harness::LaunchAttestationError::WrongPermission
    );

    let wrong_provider =
        process_permission_attestation(Harness::Codex, "claude", "--dangerously-skip-permissions")
            .unwrap();
    assert_eq!(
        wrong_provider.unwrap_err(),
        crate::harness::LaunchAttestationError::WrongProvider
    );

    let wrapper = process_permission_attestation(
        Harness::Codex,
        "wrapper",
        "--dangerously-bypass-approvals-and-sandbox",
    )
    .unwrap();
    let error = wrapper.unwrap_err();
    assert_eq!(
        error,
        crate::harness::LaunchAttestationError::WrapperObscured
    );
    assert!(!error.to_string().contains("supersecret"));
    assert!(!error.to_string().contains("api_key"));
}

#[test]
fn process_level_permission_attestation_rejects_missing_repeated_and_conflicting_flags() {
    let Some(codex_missing_value) =
        process_permission_attestation(Harness::Codex, "codex", "--sandbox --")
    else {
        return;
    };
    assert_eq!(
        codex_missing_value.unwrap_err(),
        crate::harness::LaunchAttestationError::MissingPermission
    );
    let codex_repeated = process_permission_attestation(
        Harness::Codex,
        "codex",
        "--dangerously-bypass-approvals-and-sandbox --dangerously-bypass-approvals-and-sandbox",
    )
    .unwrap();
    assert_eq!(
        codex_repeated.unwrap_err(),
        crate::harness::LaunchAttestationError::ConflictingPermission
    );
    let codex_conflicting = process_permission_attestation(
        Harness::Codex,
        "codex",
        "--dangerously-bypass-approvals-and-sandbox --sandbox read-only",
    )
    .unwrap();
    assert_eq!(
        codex_conflicting.unwrap_err(),
        crate::harness::LaunchAttestationError::ConflictingPermission
    );

    let claude_missing_value =
        process_permission_attestation(Harness::Claude, "claude", "--permission-mode --").unwrap();
    assert_eq!(
        claude_missing_value.unwrap_err(),
        crate::harness::LaunchAttestationError::MissingPermission
    );
    let claude_repeated = process_permission_attestation(
        Harness::Claude,
        "claude",
        "--dangerously-skip-permissions --dangerously-skip-permissions",
    )
    .unwrap();
    assert_eq!(
        claude_repeated.unwrap_err(),
        crate::harness::LaunchAttestationError::ConflictingPermission
    );
    let claude_conflicting = process_permission_attestation(
        Harness::Claude,
        "claude",
        "--dangerously-skip-permissions --permission-mode acceptEdits",
    )
    .unwrap();
    assert_eq!(
        claude_conflicting.unwrap_err(),
        crate::harness::LaunchAttestationError::ConflictingPermission
    );
}

#[test]
fn process_level_permission_attestation_rejects_native_alias_and_inline_conflicts() {
    let Some(codex_sandbox_alias) = process_permission_attestation(
        Harness::Codex,
        "codex",
        "--dangerously-bypass-approvals-and-sandbox -s read-only",
    ) else {
        return;
    };
    assert_eq!(
        codex_sandbox_alias.unwrap_err(),
        crate::harness::LaunchAttestationError::ConflictingPermission
    );
    let codex_approval_alias = process_permission_attestation(
        Harness::Codex,
        "codex",
        "--dangerously-bypass-approvals-and-sandbox -a=never",
    )
    .unwrap();
    assert_eq!(
        codex_approval_alias.unwrap_err(),
        crate::harness::LaunchAttestationError::ConflictingPermission
    );
    let claude_inline_false = process_permission_attestation(
        Harness::Claude,
        "claude",
        "--dangerously-skip-permissions --dangerously-skip-permissions=false",
    )
    .unwrap();
    assert_eq!(
        claude_inline_false.unwrap_err(),
        crate::harness::LaunchAttestationError::MalformedPermission
    );
}

#[test]
fn codex_unobserved_marker_runs_in_owning_pane_and_fails_closed_without_affecting_claude() {
    if !tmux_process_tests_available() {
        eprintln!(
                "codex_unobserved_marker_runs_in_owning_pane_and_fails_closed_without_affecting_claude: tmux or node not on PATH - skipping"
            );
        return;
    }
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();

    let observer = FakeCodexObserver::new(0);
    let codex =
        FakeHarnessCommand::new(Harness::Codex, "--dangerously-bypass-approvals-and-sandbox");
    let codex_session = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    let codex_target = tmux_target(&codex_session);
    create_test_tmux_session(&codex_target).unwrap();
    let codex_before = observe_harness_process(&codex_target).unwrap();
    let expected_identity = tmux_pane_identity(&codex_target);
    let launch = crew_interactive_launch(Harness::Codex, &codex.command, &observer.command);
    tmux::send_text(&codex_target, &launch, true).unwrap();
    wait_for_harness_started(&codex_session, "codex").unwrap();
    observer.wait_for_invocation();
    assert_eq!(
        std::fs::read_to_string(&observer.invocation_path)
            .unwrap()
            .trim(),
        "--codex-unobserved"
    );
    assert_eq!(
        std::fs::read_to_string(&observer.identity_path)
            .unwrap()
            .trim(),
        expected_identity
    );
    assert_eq!(
        wait_for_process_permission_attestation(&codex_target, &codex_before, Harness::Codex,)
            .unwrap()
            .permission,
        PermMode::BypassPermissions
    );
    reap_test_tmux_session_and_assert_absent(&codex_target);

    let failed_observer = FakeCodexObserver::new(23);
    let blocked_codex =
        FakeHarnessCommand::new(Harness::Codex, "--dangerously-bypass-approvals-and-sandbox");
    let blocked_session = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    let blocked_target = tmux_target(&blocked_session);
    create_test_tmux_session(&blocked_target).unwrap();
    let blocked_launch = crew_interactive_launch(
        Harness::Codex,
        &blocked_codex.command,
        &failed_observer.command,
    );
    tmux::send_text(&blocked_target, &blocked_launch, true).unwrap();
    failed_observer.wait_for_invocation();
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(
        tmux::harness_liveness(&blocked_target, "codex"),
        tmux::SessionLiveness::Gone,
        "Codex must not exec after the degraded marker fails"
    );
    assert_eq!(
        tmux::session_liveness(&blocked_target),
        tmux::SessionLiveness::Alive,
        "the owning shell remains available for transactional rollback"
    );
    reap_test_tmux_session_and_assert_absent(&blocked_target);

    let claude_observer = FakeCodexObserver::new(23);
    let claude = FakeHarnessCommand::new(Harness::Claude, "--dangerously-skip-permissions");
    let claude_session = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    let claude_target = tmux_target(&claude_session);
    create_test_tmux_session(&claude_target).unwrap();
    let claude_before = observe_harness_process(&claude_target).unwrap();
    let claude_launch =
        crew_interactive_launch(Harness::Claude, &claude.command, &claude_observer.command);
    tmux::send_text(&claude_target, &claude_launch, true).unwrap();
    wait_for_harness_started(&claude_session, "claude").unwrap();
    assert!(
        !claude_observer.invocation_path.exists(),
        "Claude launch must not invoke the Codex degraded marker"
    );
    assert_eq!(
        wait_for_process_permission_attestation(&claude_target, &claude_before, Harness::Claude,)
            .unwrap()
            .permission,
        PermMode::BypassPermissions
    );
    reap_test_tmux_session_and_assert_absent(&claude_target);
}

#[test]
fn crew_launch_attestation_persists_separate_permission_axes() {
    let path = captains_tmp("crew-launch-attestation");
    let _ = std::fs::remove_file(&path);
    let registry = powder_lifecycle_registry(Some(path.clone()));
    assert!(registry
        .record_crew_launch_attestation(
            "crew-powder",
            HarnessPermissionAttestation {
                provider: Harness::Claude,
                permission: PermMode::BypassPermissions,
            },
            "read",
        )
        .unwrap_err()
        .contains("provider binding conflicts"));
    assert!(registry
        .record_crew_launch_attestation(
            "crew-powder",
            HarnessPermissionAttestation {
                provider: Harness::Codex,
                permission: PermMode::Default,
            },
            "read",
        )
        .unwrap_err()
        .contains("conflicts with the fleet default"));
    assert!(registry
        .record_crew_launch_attestation(
            "crew-powder",
            HarnessPermissionAttestation {
                provider: Harness::Codex,
                permission: PermMode::BypassPermissions,
            },
            "admin",
        )
        .unwrap_err()
        .contains("capability is invalid"));
    let crew = registry
        .record_crew_launch_attestation(
            "crew-powder",
            HarnessPermissionAttestation {
                provider: Harness::Codex,
                permission: PermMode::BypassPermissions,
            },
            "read",
        )
        .unwrap();
    assert_eq!(crew.harness_permission, Some(PermMode::BypassPermissions));
    assert_eq!(crew.t_hub_capability.as_deref(), Some("read"));

    assert!(registry
        .record_crew_launch_attestation(
            "crew-powder",
            HarnessPermissionAttestation {
                provider: Harness::Codex,
                permission: PermMode::Default,
            },
            "read",
        )
        .unwrap_err()
        .contains("conflicts with the fleet default"));
    assert!(registry
        .record_crew_launch_attestation(
            "crew-powder",
            HarnessPermissionAttestation {
                provider: Harness::Codex,
                permission: PermMode::BypassPermissions,
            },
            "read",
        )
        .unwrap_err()
        .contains("already has launch permission evidence"));

    let restored = CaptainsRegistry::load(path.clone()).snapshot();
    let crew = &restored.captains[0].crew[0];
    assert_eq!(crew.harness_permission, Some(PermMode::BypassPermissions));
    assert_eq!(crew.t_hub_capability.as_deref(), Some("read"));
    assert_eq!(
        serde_json::to_value(crew).unwrap()["harnessPermission"],
        "bypassPermissions"
    );
    assert_eq!(
        serde_json::to_value(crew).unwrap()["tHubCapability"],
        "read"
    );
    let _ = std::fs::remove_file(path);
}
