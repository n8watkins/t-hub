use super::*;

#[cfg(unix)]
fn compile_scoped_attestation_harness() -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    let directory = std::env::temp_dir().join(format!(
        "t-hub-scoped-harness-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let source = directory.join("harness.c");
    let executable = directory.join("codex");
    std::fs::write(
        &source,
        r#"
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <termios.h>
#include <unistd.h>

static void wait_marker(const char *path) {
    while (access(path, F_OK) != 0) usleep(10000);
}

static void write_marker(const char *path) {
    FILE *marker = fopen(path, "w");
    if (marker == NULL) _exit(17);
    if (fputs("ready", marker) < 0 || fclose(marker) != 0) _exit(18);
}

int main(int argc, char **argv) {
    if (argc >= 2 && strcmp(argv[1], "changed") == 0) {
        if (argc >= 3) write_marker(argv[2]);
        for (;;) sleep(1);
    }
    if (argc >= 2 && strcmp(argv[1], "same-group") == 0) {
        if (argc < 4) return 19;
        execl(argv[3], "codex", "changed", argv[2], (char *)0);
        return 20;
    }
    if (argc < 3) return 2;
    signal(SIGTTOU, SIG_IGN);
    if (setpgid(0, 0) != 0 && errno != EACCES && getpgrp() != getpid()) return 8;
    if (tcsetpgrp(STDIN_FILENO, getpgrp()) != 0) return 9;
    const char *mode = argv[1];
    const char *marker = argv[2];
    if (strcmp(mode, "busy") == 0) {
        volatile unsigned long counter = 0;
        write_marker(marker);
        for (;;) counter++;
    }
    if (strcmp(mode, "foreign-first") == 0) {
        if (argc < 4) return 15;
        execl(argv[3], "codex", "changed", marker, (char *)0);
        return 16;
    }
    wait_marker(marker);
    if (strcmp(mode, "exit") == 0) {
        tcsetpgrp(STDIN_FILENO, getpgid(getppid()));
        return 0;
    }
    if (strcmp(mode, "exec-executable") == 0) {
        if (argc < 4) return 10;
        execl(argv[3], "codex", "changed", (char *)0);
        return 3;
    }
    if (strcmp(mode, "exec-argv") == 0) {
        execl(argv[0], "codex", "changed", (char *)0);
        return 4;
    }
    if (strcmp(mode, "tool") == 0 || strcmp(mode, "exit-tool") == 0) {
        pid_t child = fork();
        if (child < 0) return 5;
        if (child == 0) {
            signal(SIGHUP, SIG_IGN);
            setpgid(0, 0);
            execl("/bin/sleep", "tool-child", "60", (char *)0);
            _exit(6);
        }
        setpgid(child, child);
        if (tcsetpgrp(STDIN_FILENO, child) != 0) return 7;
        if (strcmp(mode, "exit-tool") == 0) _exit(0);
        int status = 0;
        waitpid(child, &status, 0);
        tcsetpgrp(STDIN_FILENO, getpgrp());
        return status;
    }
    if (strcmp(mode, "foreign") == 0) {
        if (argc < 4) return 11;
        pid_t child = fork();
        if (child < 0) return 12;
        if (child == 0) {
            setpgid(0, 0);
            execl(argv[3], "codex", "changed", (char *)0);
            _exit(13);
        }
        setpgid(child, child);
        if (tcsetpgrp(STDIN_FILENO, child) != 0) return 14;
        int status = 0;
        waitpid(child, &status, 0);
        tcsetpgrp(STDIN_FILENO, getpgrp());
        return status;
    }
    for (;;) sleep(1);
}
"#,
    )
    .unwrap();
    let output = std::process::Command::new("cc")
        .args(["-O2", "-o"])
        .arg(&executable)
        .arg(&source)
        .output()
        .ok()?;
    if !output.status.success() {
        std::fs::remove_dir_all(&directory).ok();
        return None;
    }
    Some((directory, executable))
}

#[cfg(unix)]
fn test_codex_package_paths(
    node_modules: &std::path::Path,
) -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    let (platform_package, target_triple) = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux" | "android", "x86_64") => ("codex-linux-x64", "x86_64-unknown-linux-musl"),
        ("linux" | "android", "aarch64") => ("codex-linux-arm64", "aarch64-unknown-linux-musl"),
        ("macos", "x86_64") => ("codex-darwin-x64", "x86_64-apple-darwin"),
        ("macos", "aarch64") => ("codex-darwin-arm64", "aarch64-apple-darwin"),
        _ => return None,
    };
    Some((
        node_modules.join("@openai/codex/bin/codex.js"),
        node_modules
            .join("@openai")
            .join(platform_package)
            .join("vendor")
            .join(target_triple)
            .join("bin/codex"),
    ))
}

#[cfg(unix)]
struct ManagedCortanaTestCleanup {
    ctx: Arc<ControlContext>,
}

#[cfg(unix)]
impl ManagedCortanaTestCleanup {
    fn new(ctx: Arc<ControlContext>) -> Self {
        Self { ctx }
    }
}

#[cfg(unix)]
impl Drop for ManagedCortanaTestCleanup {
    fn drop(&mut self) {
        let durable = self.ctx.captains.cortana_identity();
        if let (Some(launch), Some(owner)) =
            (durable.managed_launch.as_ref(), durable.owner.as_ref())
        {
            let _ = tmux::retire_managed_runtime(&launch.tmux_target, &tmux_cortana_owner(owner));
        }
    }
}

#[test]
fn cortana_managed_launch_wal_never_partially_publishes_owner_or_terminal() {
    let path = captains_tmp("cortana-managed-launch-wal");
    let _ = std::fs::remove_file(&path);
    let registry = powder_lifecycle_registry(Some(path.clone()));
    registry.begin_cortana_recovery("wal-operation").unwrap();
    let owner = synthetic_cortana_managed_owner();
    let launch = tmux::ManagedRuntimeLaunchSpec {
        unit_name: owner.unit_name.clone(),
        launch_nonce: owner.launch_nonce.clone(),
        tools: tmux::ManagedSystemTools {
            python: tmux::ManagedExecutableIdentity {
                path: owner.tools.python.path.clone(),
                device: owner.tools.python.device,
                inode: owner.tools.python.inode,
            },
            systemctl: tmux::ManagedExecutableIdentity {
                path: owner.tools.systemctl.path.clone(),
                device: owner.tools.systemctl.device,
                inode: owner.tools.systemctl.inode,
            },
            systemd_run: tmux::ManagedExecutableIdentity {
                path: owner.tools.systemd_run.path.clone(),
                device: owner.tools.systemd_run.device,
                inode: owner.tools.systemd_run.inode,
            },
        },
    };

    registry.fail_next_persist("before prepared WAL");
    assert!(registry
        .prepare_cortana_managed_launch(
            "wal-operation",
            "wal00001",
            "identity-wal",
            1,
            "codex",
            &launch,
            synthetic_cortana_expected_harness_launch("codex"),
        )
        .is_err());
    assert!(registry.cortana_identity().managed_launch.is_none());

    let prepared = registry
        .prepare_cortana_managed_launch(
            "wal-operation",
            "wal00001",
            "identity-wal",
            1,
            "codex",
            &launch,
            synthetic_cortana_expected_harness_launch("codex"),
        )
        .unwrap();
    assert!(prepared.owner.is_none());
    assert!(prepared.terminal_id.is_none());
    assert_eq!(
        prepared.managed_launch.as_ref().unwrap().phase,
        crate::cortana_reconcile::CortanaManagedLaunchPhase::Prepared
    );

    registry.fail_next_persist("after effect before observed WAL");
    assert!(registry
        .record_cortana_runtime_owner("wal-operation", "wal00001", owner.clone())
        .is_err());
    let still_prepared = registry.cortana_identity();
    assert!(still_prepared.owner.is_none());
    assert!(still_prepared.terminal_id.is_none());
    assert_eq!(
        still_prepared.managed_launch.as_ref().unwrap().phase,
        crate::cortana_reconcile::CortanaManagedLaunchPhase::Prepared
    );

    let observed = registry
        .record_cortana_runtime_owner("wal-operation", "wal00001", owner)
        .unwrap();
    assert_eq!(observed.terminal_id.as_deref(), Some("wal00001"));
    assert!(observed.owner.is_some());
    assert_eq!(
        observed.managed_launch.as_ref().unwrap().phase,
        crate::cortana_reconcile::CortanaManagedLaunchPhase::OwnerObserved
    );
    let reloaded = powder_lifecycle_registry(Some(path.clone())).cortana_identity();
    assert_eq!(reloaded, observed);
    let _ = std::fs::remove_file(path);
}

#[test]
fn v2_launch_policy_enrichment_preserves_live_wal_identity_across_restart() {
    let path = captains_tmp("cortana-schema27-provenance-enrichment");
    let _ = std::fs::remove_file(&path);
    let registry = powder_lifecycle_registry(Some(path.clone()));
    registry
        .begin_cortana_recovery("schema27-operation")
        .unwrap();
    let owner = synthetic_cortana_managed_owner();
    let launch = tmux::ManagedRuntimeLaunchSpec {
        unit_name: owner.unit_name.clone(),
        launch_nonce: owner.launch_nonce.clone(),
        tools: tmux::ManagedSystemTools {
            python: tmux::ManagedExecutableIdentity {
                path: owner.tools.python.path.clone(),
                device: owner.tools.python.device,
                inode: owner.tools.python.inode,
            },
            systemctl: tmux::ManagedExecutableIdentity {
                path: owner.tools.systemctl.path.clone(),
                device: owner.tools.systemctl.device,
                inode: owner.tools.systemctl.inode,
            },
            systemd_run: tmux::ManagedExecutableIdentity {
                path: owner.tools.systemd_run.path.clone(),
                device: owner.tools.systemd_run.device,
                inode: owner.tools.systemd_run.inode,
            },
        },
    };
    let expected = synthetic_cortana_expected_harness_launch("codex");
    registry
        .prepare_cortana_managed_launch(
            "schema27-operation",
            "a1b2c3d4",
            "schema27-identity",
            1,
            "codex",
            &launch,
            expected.clone(),
        )
        .unwrap();
    let owner_observed = registry
        .record_cortana_runtime_owner("schema27-operation", "a1b2c3d4", owner.clone())
        .unwrap();
    let mut snapshot = registry.snapshot();
    let legacy_launch = snapshot.cortana.managed_launch.as_mut().unwrap();
    legacy_launch.version = 4;
    let legacy_expected = legacy_launch
        .expected_harness_launch_provenance
        .as_mut()
        .unwrap();
    legacy_expected.version = 2;
    legacy_expected.launch_policy_sha256 = None;
    legacy_expected.semantic_argv_sha256 = None;
    std::fs::write(&path, serde_json::to_vec(&snapshot).unwrap()).unwrap();

    let legacy = CaptainsRegistry::load(path.clone());
    let reloaded_legacy = legacy.cortana_identity();
    let reloaded_launch = reloaded_legacy.managed_launch.as_ref().unwrap();
    assert_eq!(
        reloaded_launch.phase,
        crate::cortana_reconcile::CortanaManagedLaunchPhase::OwnerObserved
    );
    assert_eq!(
        reloaded_legacy.owner.as_ref(),
        owner_observed.owner.as_ref()
    );
    assert_eq!(
        reloaded_legacy.terminal_id.as_ref(),
        owner_observed.terminal_id.as_ref()
    );
    let reloaded_expected = reloaded_launch
        .expected_harness_launch_provenance
        .as_ref()
        .unwrap();
    assert_eq!(reloaded_expected.version, 2);
    assert!(reloaded_expected.launch_policy_sha256.is_none());
    assert!(reloaded_expected.semantic_argv_sha256.is_none());
    assert!(crate::harness::valid_expected_harness_launch_provenance(
        reloaded_expected
    ));
    for (identity_id, generation) in [("replayed-identity", 1), ("schema27-identity", 2)] {
        assert!(legacy
            .record_cortana_expected_harness_launch_provenance(
                "schema27-operation",
                "a1b2c3d4",
                identity_id,
                generation,
                expected.clone(),
            )
            .unwrap_err()
            .contains("does not match its launch"));
    }
    let mut changed = expected.clone();
    changed.executable.inode += 1;
    assert!(legacy
        .record_cortana_expected_harness_launch_provenance(
            "schema27-operation",
            "a1b2c3d4",
            "schema27-identity",
            1,
            changed,
        )
        .unwrap_err()
        .contains("different expected"));
    assert_eq!(legacy.cortana_identity().managed_launch.unwrap().version, 4);

    let enriched = legacy
        .record_cortana_expected_harness_launch_provenance(
            "schema27-operation",
            "a1b2c3d4",
            "schema27-identity",
            1,
            expected,
        )
        .unwrap();
    let enriched_launch = enriched.managed_launch.as_ref().unwrap();
    assert_eq!(enriched_launch.version, 4);
    assert_eq!(
        enriched_launch.phase,
        crate::cortana_reconcile::CortanaManagedLaunchPhase::OwnerObserved
    );
    assert_eq!(enriched.owner.as_ref(), owner_observed.owner.as_ref());
    assert_eq!(
        enriched.terminal_id.as_ref(),
        owner_observed.terminal_id.as_ref()
    );
    assert_eq!(
        enriched_launch
            .expected_harness_launch_provenance
            .as_ref()
            .unwrap()
            .version,
        crate::harness::EXPECTED_HARNESS_LAUNCH_PROVENANCE_VERSION
    );
    assert_eq!(
        enriched_launch
            .expected_harness_launch_provenance
            .as_ref()
            .unwrap()
            .launch_policy_sha256,
        Some(crate::harness::cortana_codex_launch_policy_sha256())
    );
    assert!(enriched_launch
        .expected_harness_launch_provenance
        .as_ref()
        .unwrap()
        .semantic_argv_sha256
        .is_some());
    assert_eq!(
        CaptainsRegistry::load(path.clone()).cortana_identity(),
        enriched
    );
    let _ = std::fs::remove_file(path);
}

#[cfg(unix)]
#[test]
fn scoped_harness_attestation_rejects_live_process_substitution_and_allows_tool_children() {
    if tmux::managed_runtime_preflight().is_err() {
        return;
    }
    let Some((fixture_dir, executable)) = compile_scoped_attestation_harness() else {
        eprintln!("scoped Harness attestation: cc is unavailable - skipping");
        return;
    };
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let token = format!("scoped-token-{}", uuid::Uuid::new_v4().simple());
    let identity_id = format!("scoped-identity-{}", uuid::Uuid::new_v4().simple());
    let fake_dir = fixture_dir.join("fake");
    std::fs::create_dir_all(&fake_dir).unwrap();
    let fake = fake_dir.join("codex");
    std::fs::copy(&executable, &fake).unwrap();
    let expected = crate::harness::resolve_expected_harness_launch_provenance(
        &format!("{} hold", executable.display()),
        Harness::Codex,
    )
    .unwrap();

    let start = |mode: &str, with_process_token: bool| {
        let terminal_id = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
        let target = tmux_target(&terminal_id);
        let marker = fixture_dir.join(format!("{mode}-{terminal_id}.marker"));
        let prefix = if with_process_token {
            String::new()
        } else {
            format!("env -u {} ", crate::identity::SESSION_TOKEN_ENV)
        };
        let command = format!(
            "{prefix}{} {mode} {} {}",
            executable.display(),
            marker.display(),
            fake.display()
        );
        let launch = tmux::prepare_managed_runtime_launch().unwrap();
        let owner = tmux::new_prepared_managed_session_with_env(
            &target,
            fixture_dir.to_str().unwrap(),
            Some(&command),
            &[(crate::identity::SESSION_TOKEN_ENV.into(), token.clone())],
            &launch,
        )
        .unwrap();
        (target, marker, owner)
    };
    let observe = |target: &str, owner: &tmux::ManagedRuntimeOwnerToken| {
        crate::harness::observe_scoped_harness_process(
            target,
            Harness::Codex,
            &expected,
            &identity_id,
            &token,
            &owner.cgroup_path,
            owner.tmux.pane_start_ticks,
            Instant::now() + Duration::from_secs(2),
        )
    };
    let wait_observed = |target: &str, owner: &tmux::ManagedRuntimeOwnerToken| {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match observe(target, owner) {
                Ok(observed) => break observed,
                Err(error) => assert!(
                    Instant::now() < deadline,
                    "Harness was not observed: {error:?}"
                ),
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    let wait_changed = |target: &str,
                        owner: &tmux::ManagedRuntimeOwnerToken,
                        baseline: &crate::harness::HarnessProcessIdentity| {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match observe(target, owner) {
                Ok(observed) if &observed != baseline => break Ok(observed),
                Err(error @ crate::harness::LaunchAttestationError::ExpectedProvenanceMismatch) => {
                    break Err(error);
                }
                Err(error) => assert!(
                    Instant::now() < deadline,
                    "Harness identity did not settle after a process transition: {error:?}"
                ),
                _ => {}
            }
            assert!(Instant::now() < deadline, "Harness identity did not change");
            std::thread::sleep(Duration::from_millis(20));
        }
    };

    let (target, _, owner) = start("foreign-first", true);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match observe(&target, &owner) {
            Err(crate::harness::LaunchAttestationError::ExpectedProvenanceMismatch) => break,
            Err(_) => {}
            Ok(observed) => {
                panic!("foreign same-name provider was trusted on first observation: {observed:?}")
            }
        }
        assert!(
            Instant::now() < deadline,
            "foreign first provider never reached a stable rejected state"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    tmux::retire_managed_runtime(&target, &owner).unwrap();

    // A live provider changes the CPU accounting fields in /proc/<pid>/stat.
    // Attestation must pin immutable identity and topology fields rather than
    // treating those volatile counters as evidence of process substitution.
    let (target, marker, owner) = start("busy", true);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !marker.exists() {
        assert!(Instant::now() < deadline, "busy Harness did not start");
        std::thread::sleep(Duration::from_millis(20));
    }
    let baseline = wait_observed(&target, &owner);
    for _ in 0..8 {
        assert_eq!(
            observe(&target, &owner).unwrap(),
            baseline,
            "CPU activity changed the attested Harness identity"
        );
    }
    tmux::retire_managed_runtime(&target, &owner).unwrap();

    for mode in ["exec-executable", "exec-argv"] {
        let (target, marker, owner) = start(mode, true);
        let baseline = wait_observed(&target, &owner);
        let durable_wire = serde_json::to_string(&baseline).unwrap();
        assert!(!durable_wire.contains(&token));
        assert!(baseline.argv_sha256.starts_with("sha256:"));
        assert!(baseline.session_token_sha256.starts_with("sha256:"));
        std::fs::write(&marker, b"go").unwrap();
        if mode == "exec-executable" {
            assert_eq!(
                wait_changed(&target, &owner, &baseline).unwrap_err(),
                crate::harness::LaunchAttestationError::ExpectedProvenanceMismatch
            );
        } else {
            let changed = wait_changed(&target, &owner, &baseline).unwrap();
            assert_eq!(changed.pid, baseline.pid);
            assert_eq!(changed.start_ticks, baseline.start_ticks);
            assert_eq!(changed.executable, baseline.executable);
            assert_ne!(changed.argv_sha256, baseline.argv_sha256);
        }
        tmux::retire_managed_runtime(&target, &owner).unwrap();
    }

    let (target, marker, owner) = start("foreign", true);
    let baseline = wait_observed(&target, &owner);
    std::fs::write(&marker, b"go").unwrap();
    assert_eq!(
        wait_changed(&target, &owner, &baseline).unwrap_err(),
        crate::harness::LaunchAttestationError::ExpectedProvenanceMismatch
    );
    tmux::retire_managed_runtime(&target, &owner).unwrap();

    let (target, marker, owner) = start("tool", true);
    let baseline = wait_observed(&target, &owner);
    std::fs::write(&marker, b"go").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while tmux::observe_session_effect_identity(&target)
        .is_ok_and(|effect| effect.foreground_pid == baseline.pid)
    {
        assert!(
            Instant::now() < deadline,
            "tool child never became foreground"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    // The ordinary tool child must not change the attested harness identity.
    // Poll to a deadline rather than sampling once: under load the observation
    // can transiently fail to read before it settles back on the baseline.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match observe(&target, &owner) {
            Ok(observed) if observed == baseline => break,
            Ok(observed) => {
                panic!("ordinary tool child changed the attested harness identity: {observed:?}")
            }
            Err(error) => assert!(
                Instant::now() < deadline,
                "attested harness identity was not observable after the tool child: {error:?}"
            ),
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    tmux::retire_managed_runtime(&target, &owner).unwrap();

    let (target, marker, owner) = start("exit-tool", true);
    let baseline = wait_observed(&target, &owner);
    std::fs::write(&marker, b"go").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while std::path::Path::new(&format!("/proc/{}", baseline.pid)).exists() {
        assert!(Instant::now() < deadline, "Harness did not exit");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(observe(&target, &owner).is_err());
    tmux::retire_managed_runtime(&target, &owner).unwrap();

    let (target, marker, owner) = start("stable", false);
    assert_eq!(
        tmux::session_environment(&target, crate::identity::SESSION_TOKEN_ENV).unwrap(),
        Some(token.clone())
    );
    let foreground_pid = tmux::observe_session_effect_identity(&target)
        .unwrap()
        .foreground_pid;
    let environment = std::fs::read(format!("/proc/{foreground_pid}/environ")).unwrap();
    assert!(!environment
        .split(|byte| *byte == 0)
        .any(|entry| entry.starts_with(b"T_HUB_SESSION_TOKEN=")));
    assert!(observe(&target, &owner).is_err());
    std::fs::write(marker, b"go").unwrap();
    tmux::retire_managed_runtime(&target, &owner).unwrap();

    if std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
    {
        use std::os::unix::fs::PermissionsExt;

        let package_modules = fixture_dir.join("production-node-modules");
        let Some((package_script, package_native)) = test_codex_package_paths(&package_modules)
        else {
            return;
        };
        std::fs::create_dir_all(package_script.parent().unwrap()).unwrap();
        std::fs::create_dir_all(package_native.parent().unwrap()).unwrap();
        std::fs::copy(&executable, &package_native).unwrap();
        let tool_marker = fixture_dir.join("production-native-tool.marker");
        let package_source = format!(
                "#!/usr/bin/env node\nconst {{ spawn }} = require('child_process');\nspawn({}, ['tool', {}], {{ stdio: 'inherit' }});\nsetInterval(() => {{}}, 1000);\n",
                serde_json::to_string(&package_native).unwrap(),
                serde_json::to_string(&tool_marker).unwrap(),
            );
        std::fs::write(&package_script, package_source).unwrap();
        let mut permissions = std::fs::metadata(&package_script).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&package_script, permissions).unwrap();
        let package_launcher = package_modules.join(".bin/codex");
        std::fs::create_dir_all(package_launcher.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&package_script, &package_launcher).unwrap();
        let package_command = package_launcher.display().to_string();
        let package_expected = crate::harness::resolve_expected_harness_launch_provenance(
            &package_command,
            Harness::Codex,
        )
        .unwrap();
        assert!(package_expected.trusted_child_executable.is_some());
        let pane = crate::commands::pane_command(None, Some(&package_command));
        let terminal_id = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
        let package_target = tmux_target(&terminal_id);
        let launch = tmux::prepare_managed_runtime_launch().unwrap();
        let package_owner = tmux::new_prepared_managed_session_with_env(
            &package_target,
            fixture_dir.to_str().unwrap(),
            pane.as_deref(),
            &[(crate::identity::SESSION_TOKEN_ENV.into(), token.clone())],
            &launch,
        )
        .unwrap();
        let observe_package = || {
            crate::harness::observe_scoped_harness_process(
                &package_target,
                Harness::Codex,
                &package_expected,
                &identity_id,
                &token,
                &package_owner.cgroup_path,
                package_owner.tmux.pane_start_ticks,
                Instant::now() + Duration::from_secs(2),
            )
        };
        // The node launcher spawns the native Codex child asynchronously, so an
        // early observation can still bind to the launcher before the native
        // child appears in the scoped process list. Wait for the observation to
        // converge on the trusted native executable so the baseline is the
        // settled state, not a mid-spawn sample. (Breaking on the first Ok here
        // is what made this test flake under CI parallel load.)
        let deadline = Instant::now() + Duration::from_secs(5);
        let package_baseline = loop {
            match observe_package() {
                Ok(observed)
                    if Some(&observed.executable)
                        == package_expected.trusted_child_executable.as_ref() =>
                {
                    break observed;
                }
                other => assert!(
                    Instant::now() < deadline,
                    "bound native Codex child did not settle on the trusted executable: {other:?}"
                ),
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        std::fs::write(&tool_marker, b"go").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while tmux::observe_session_effect_identity(&package_target)
            .is_ok_and(|effect| effect.foreground_pid == package_baseline.pid)
        {
            assert!(
                Instant::now() < deadline,
                "ordinary tool child did not become foreground"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        // The ordinary tool child must not change the attested harness identity.
        // Poll to a deadline rather than sampling once: under load the
        // observation can transiently fail to read before it settles back on the
        // baseline.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match observe_package() {
                Ok(observed) if observed == package_baseline => break,
                Ok(observed) => {
                    panic!(
                        "ordinary tool child changed the attested harness identity: {observed:?}"
                    )
                }
                Err(error) => assert!(
                    Instant::now() < deadline,
                    "attested harness identity was not observable after the tool child: {error:?}"
                ),
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        tmux::retire_managed_runtime(&package_target, &package_owner).unwrap();
        let target_triple = package_native
            .parent()
            .and_then(std::path::Path::parent)
            .and_then(std::path::Path::file_name)
            .unwrap();
        let bundled_native = package_script
            .parent()
            .and_then(std::path::Path::parent)
            .unwrap()
            .join("vendor")
            .join(target_triple)
            .join("bin/codex");
        std::fs::remove_file(&package_native).unwrap();
        std::fs::create_dir_all(bundled_native.parent().unwrap()).unwrap();
        std::fs::copy(&executable, &bundled_native).unwrap();
        let bundled_expected = crate::harness::resolve_expected_harness_launch_provenance(
            &package_command,
            Harness::Codex,
        )
        .unwrap();
        assert_eq!(
            bundled_expected
                .trusted_child_executable
                .as_ref()
                .unwrap()
                .path,
            bundled_native.display().to_string()
        );

        let script_dir = fixture_dir.join("node-provider");
        std::fs::create_dir_all(&script_dir).unwrap();
        let script = script_dir.join("codex");
        std::fs::write(
            &script,
            "#!/usr/bin/env node\nsetInterval(() => {}, 1000);\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script, permissions).unwrap();
        let script_command = script.display().to_string();
        let script_expected = crate::harness::resolve_expected_harness_launch_provenance(
            &script_command,
            Harness::Codex,
        )
        .unwrap();
        let pane = crate::commands::pane_command(None, Some(&script_command));
        let terminal_id = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
        let target = tmux_target(&terminal_id);
        let launch = tmux::prepare_managed_runtime_launch().unwrap();
        let owner = tmux::new_prepared_managed_session_with_env(
            &target,
            fixture_dir.to_str().unwrap(),
            pane.as_deref(),
            &[(crate::identity::SESSION_TOKEN_ENV.into(), token.clone())],
            &launch,
        )
        .unwrap();
        let observe_script = || {
            crate::harness::observe_scoped_harness_process(
                &target,
                Harness::Codex,
                &script_expected,
                &identity_id,
                &token,
                &owner.cgroup_path,
                owner.tmux.pane_start_ticks,
                Instant::now() + Duration::from_secs(2),
            )
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        let observed = loop {
            match observe_script() {
                Ok(observed) => break observed,
                Err(error) => assert!(
                    Instant::now() < deadline,
                    "Node provider was not observed: {error:?}"
                ),
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(observed.executable, script_expected.executable);

        let foreign_script_dir = fixture_dir.join("node-provider-foreign-child");
        std::fs::create_dir_all(&foreign_script_dir).unwrap();
        let foreign_script = foreign_script_dir.join("codex");
        let foreign_marker = foreign_script_dir.join("wrapper-ready");
        let child_marker = foreign_script_dir.join("child-start");
        let foreign_source = format!(
                "#!/usr/bin/env node\nconst fs = require('fs');\nconst {{ spawn }} = require('child_process');\nfs.writeFileSync({}, 'ready');\nspawn({}, ['foreign-first', {}, {}], {{ stdio: 'inherit' }});\nsetInterval(() => {{}}, 1000);\n",
                serde_json::to_string(&foreign_marker).unwrap(),
                serde_json::to_string(&executable).unwrap(),
                serde_json::to_string(&child_marker).unwrap(),
                serde_json::to_string(&fake).unwrap(),
            );
        std::fs::write(&foreign_script, foreign_source).unwrap();
        let mut permissions = std::fs::metadata(&foreign_script).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&foreign_script, permissions).unwrap();
        let foreign_script_command = foreign_script.display().to_string();
        let foreign_script_expected = crate::harness::resolve_expected_harness_launch_provenance(
            &foreign_script_command,
            Harness::Codex,
        )
        .unwrap();
        let pane = crate::commands::pane_command(None, Some(&foreign_script_command));
        let terminal_id = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
        let foreign_target = tmux_target(&terminal_id);
        let launch = tmux::prepare_managed_runtime_launch().unwrap();
        let foreign_owner = tmux::new_prepared_managed_session_with_env(
            &foreign_target,
            fixture_dir.to_str().unwrap(),
            pane.as_deref(),
            &[(crate::identity::SESSION_TOKEN_ENV.into(), token.clone())],
            &launch,
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !foreign_marker.exists() {
            assert!(Instant::now() < deadline, "Node wrapper did not start");
            std::thread::sleep(Duration::from_millis(20));
        }
        loop {
            match crate::harness::observe_scoped_harness_process(
                &foreign_target,
                Harness::Codex,
                &foreign_script_expected,
                &identity_id,
                &token,
                &foreign_owner.cgroup_path,
                foreign_owner.tmux.pane_start_ticks,
                Instant::now() + Duration::from_secs(2),
            ) {
                Err(crate::harness::LaunchAttestationError::ExpectedProvenanceMismatch) => break,
                Err(_) => {}
                Ok(observed) => panic!(
                    "foreign same-provider child beneath Node wrapper was trusted: {observed:?}"
                ),
            }
            assert!(
                Instant::now() < deadline,
                "foreign Node child never reached a stable rejected state"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let effect = tmux::observe_session_effect_identity(&foreign_target).unwrap();
        use std::os::unix::fs::MetadataExt;
        let foreground_exe =
            std::fs::metadata(format!("/proc/{}/exe", effect.foreground_pid)).unwrap();
        let fake_exe = std::fs::metadata(&fake).unwrap();
        assert_eq!(foreground_exe.dev(), fake_exe.dev());
        assert_eq!(foreground_exe.ino(), fake_exe.ino());
        let environment =
            std::fs::read(format!("/proc/{}/environ", effect.foreground_pid)).unwrap();
        assert!(environment.split(|byte| *byte == 0).any(|entry| {
            entry == format!("{}={token}", crate::identity::SESSION_TOKEN_ENV).as_bytes()
        }));
        let stat =
            std::fs::read_to_string(format!("/proc/{}/stat", effect.foreground_pid)).unwrap();
        let parent_pid = stat
            .rsplit_once(") ")
            .unwrap()
            .1
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse::<u32>()
            .unwrap();
        let wrapper_exe = std::fs::metadata(format!("/proc/{parent_pid}/exe")).unwrap();
        assert_eq!(wrapper_exe.dev(), foreign_script_expected.executable.device);
        assert_eq!(wrapper_exe.ino(), foreign_script_expected.executable.inode);
        tmux::retire_managed_runtime(&foreign_target, &foreign_owner).unwrap();

        let replacement = script_dir.join("replacement");
        std::fs::write(
            &replacement,
            "#!/usr/bin/env node\nsetInterval(() => {}, 2000);\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&replacement).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&replacement, permissions).unwrap();
        std::fs::rename(&replacement, &script).unwrap();
        // Substituting the on-disk launcher must be rejected as a provenance
        // mismatch. Poll to a deadline instead of asserting on a single
        // observation: under CI parallel load an observation can transiently
        // surface a different error (e.g. an unreadable/ancestry read) before it
        // settles on the mismatch, and demanding the exact variant on the first
        // try is what let this assertion flake. A trusted Ok is still a hard
        // failure.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match observe_script() {
                Err(crate::harness::LaunchAttestationError::ExpectedProvenanceMismatch) => break,
                Ok(observed) => panic!("substituted launcher was trusted: {observed:?}"),
                Err(error) => assert!(
                    Instant::now() < deadline,
                    "substituted launcher never reached a stable provenance mismatch: {error:?}"
                ),
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        tmux::retire_managed_runtime(&target, &owner).unwrap();
    }

    std::fs::remove_dir_all(fixture_dir).ok();
}

#[cfg(unix)]
#[test]
fn prepared_launch_rejects_foreign_same_name_provider_on_first_observation() {
    if tmux::managed_runtime_preflight().is_err() {
        return;
    }
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let mut ctx = test_ctx("cortana-first-provider-provenance").with_apply_sink(sink);
    ctx.addr = "127.0.0.1:4257".into();
    ctx.tab_registry().replace(vec![TabRecord {
        id: CAPTAIN_WORKSPACE_ID.into(),
        name: CAPTAIN_WORKSPACE_NAME.into(),
        tile_ids: Vec::new(),
    }]);
    let home = std::env::temp_dir().join(format!(
        "t-hub-cortana-first-provider-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&home).unwrap();
    let (expected_dir, expected_command) = test_harness_command("codex");
    let (foreign_dir, foreign_command) = test_harness_command("codex");

    let error = dispatch(
        &ctx,
        "reconcile_cortana",
        &json!({
            "operationId": "cortana-first-provider-operation",
            "testOrchestratorHome": home,
            "testStartupCommand": expected_command,
            "testEffectStartupCommand": foreign_command,
        }),
    )
    .unwrap_err();
    assert!(!error.trim().is_empty());
    assert!(!matches!(
        ctx.captains.cortana_identity().recovery,
        crate::cortana_reconcile::CortanaRecoveryState::Healthy { .. }
    ));
    assert!(!ctx
        .captains
        .snapshot()
        .captains
        .iter()
        .any(|captain| captain.role == FleetRole::Cortana));

    std::fs::remove_dir_all(expected_dir).unwrap();
    std::fs::remove_dir_all(foreign_dir).unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[cfg(unix)]
fn delayed_node_wrapper_attestation_case(trusted_child: bool) {
    if tmux::managed_runtime_preflight().is_err()
        || !std::process::Command::new("node")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
    {
        return;
    }
    let Some((fixture_dir, executable)) = compile_scoped_attestation_harness() else {
        eprintln!("prepared Node launch provenance: cc is unavailable - skipping");
        return;
    };
    use std::os::unix::fs::PermissionsExt;

    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let case = if trusted_child { "trusted" } else { "foreign" };
    let mut context = test_ctx(&format!("cortana-delayed-node-child-{case}")).with_apply_sink(sink);
    context.addr = "127.0.0.1:4259".into();
    let ctx = Arc::new(context);
    ctx.tab_registry().replace(vec![TabRecord {
        id: CAPTAIN_WORKSPACE_ID.into(),
        name: CAPTAIN_WORKSPACE_NAME.into(),
        tile_ids: Vec::new(),
    }]);
    let home = fixture_dir.join("home");
    let node_modules = fixture_dir.join("node_modules");
    let Some((script, trusted_native)) = test_codex_package_paths(&node_modules) else {
        return;
    };
    let script_dir = script.parent().unwrap();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(script_dir).unwrap();
    std::fs::create_dir_all(trusted_native.parent().unwrap()).unwrap();
    std::fs::copy(&executable, &trusted_native).unwrap();
    let fake = script_dir.join("foreign-codex");
    std::fs::copy(&executable, &fake).unwrap();
    let wrapper_marker = script_dir.join("wrapper-start");
    let spawn_gate = script_dir.join("spawn-foreign-child");
    let child_marker = script_dir.join("child-start");
    let child_spawn = if trusted_child {
        format!(
            "spawn({}, ['same-group', {}, {}], {{ stdio: 'inherit' }});",
            serde_json::to_string(&executable).unwrap(),
            serde_json::to_string(&child_marker).unwrap(),
            serde_json::to_string(&trusted_native).unwrap(),
        )
    } else {
        format!(
            "spawn({}, ['foreign-first', {}, {}], {{ stdio: 'inherit' }});",
            serde_json::to_string(&executable).unwrap(),
            serde_json::to_string(&child_marker).unwrap(),
            serde_json::to_string(&fake).unwrap(),
        )
    };
    let source = format!(
            "#!/usr/bin/env node\nconst fs = require('fs');\nconst {{ spawn }} = require('child_process');\nfs.writeFileSync({}, 'ready');\nconst timer = setInterval(() => {{\n  if (!fs.existsSync({})) return;\n  clearInterval(timer);\n  {}\n}}, 10);\n",
            serde_json::to_string(&wrapper_marker).unwrap(),
            serde_json::to_string(&spawn_gate).unwrap(),
            child_spawn,
        );
    std::fs::write(&script, source).unwrap();
    let mut permissions = std::fs::metadata(&script).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&script, permissions).unwrap();
    let launcher = node_modules.join(".bin/codex");
    std::fs::create_dir_all(launcher.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&script, &launcher).unwrap();

    let operation_id = format!("cortana-delayed-node-child-{case}-operation");
    let terminal_id = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    let identity = ctx.identity.mint(crate::identity::Role::Cortana).unwrap();
    ctx.identity.bind_tile(&identity.id, &terminal_id).unwrap();
    ctx.captains.begin_cortana_recovery(&operation_id).unwrap();
    let command = launcher.display().to_string();
    let expected =
        crate::harness::resolve_expected_harness_launch_provenance(&command, Harness::Codex)
            .unwrap();
    assert!(expected.trusted_child_executable.is_some());
    let launch = tmux::prepare_managed_runtime_launch().unwrap();
    ctx.captains
        .prepare_cortana_managed_launch(
            &operation_id,
            &terminal_id,
            &identity.id,
            1,
            "codex",
            &launch,
            expected.clone(),
        )
        .unwrap();
    let spawn_args = json!({
        "cwd": home,
        "name": "Cortana",
        "startupCommand": command,
        "tabId": CAPTAIN_WORKSPACE_ID,
    });
    let mut elevation = elevation_env(&ctx, &spawn_args);
    elevation.push((
        crate::identity::SESSION_TOKEN_ENV.to_string(),
        identity.secret.clone(),
    ));
    elevation.push((CORTANA_GENERATION_ENV.to_string(), "1".into()));
    elevation.push((
        PROVIDER_SESSION_ENV.to_string(),
        pending_provider_marker("codex"),
    ));
    let pane = crate::commands::pane_command(None, Some(&command));
    let (_, target, owner) = spawn_managed_tmux_terminal_with_id(
        &terminal_id,
        home.to_str().unwrap(),
        pane.as_deref(),
        &elevation,
        &launch,
    )
    .unwrap();
    let deadline = Instant::now() + TEST_ASYNC_FIXTURE_TIMEOUT;
    while !wrapper_marker.exists() {
        assert!(
            Instant::now() < deadline,
            "trusted Node wrapper did not start"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    loop {
        match crate::harness::observe_scoped_harness_process(
            &target,
            Harness::Codex,
            &expected,
            &identity.id,
            &identity.secret,
            &owner.cgroup_path,
            owner.tmux.pane_start_ticks,
            Instant::now() + Duration::from_secs(2),
        ) {
            Ok(observed) => {
                assert_eq!(observed.executable, expected.executable);
                break;
            }
            Err(_) => {
                assert!(
                    Instant::now() < deadline,
                    "trusted Node wrapper never became observable"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }

    let worker_ctx = Arc::clone(&ctx);
    let worker_home = home.clone();
    let worker_command = command.clone();
    let worker_operation_id = operation_id.clone();
    let worker = std::thread::spawn(move || {
        dispatch(
            &worker_ctx,
            "reconcile_cortana",
            &json!({
                "operationId": worker_operation_id,
                "testOrchestratorHome": worker_home,
                "testStartupCommand": worker_command,
            }),
        )
    });
    std::thread::sleep(Duration::from_millis(350));
    assert!(
        !worker.is_finished(),
        "delayed Node wrapper became terminal before its native child"
    );
    let assert_unpublished = || {
        let durable = ctx.captains.cortana_identity();
        assert!(durable
            .managed_launch
            .as_ref()
            .is_some_and(|launch| launch.harness_process.is_none()));
        assert!(!matches!(
            durable.recovery,
            crate::cortana_reconcile::CortanaRecoveryState::Healthy { .. }
        ));
        assert!(!ctx
            .captains
            .snapshot()
            .captains
            .iter()
            .any(|captain| captain.role == FleetRole::Cortana));
    };
    assert_unpublished();

    let (reached_tx, reached_rx) = mpsc::sync_channel(1);
    let (resume_tx, resume_rx) = mpsc::sync_channel(1);
    ctx.captains.set_dispatch_barrier(Some(DispatchBarrier {
        boundary: "cortana_harness_stability_observed",
        reached: reached_tx,
        resume: resume_rx,
    }));
    let reached = reached_rx.recv_timeout(TEST_ASYNC_FIXTURE_TIMEOUT);
    if reached.is_err() && worker.is_finished() {
        panic!(
            "Cortana exited before its startup-stability boundary: {:?}",
            worker.join().unwrap()
        );
    }
    assert_eq!(
        reached.expect("Cortana did not reach its startup-stability boundary"),
        "cortana_harness_stability_observed"
    );
    assert_unpublished();

    std::fs::write(&spawn_gate, "go").unwrap();
    while !child_marker.exists() {
        assert!(
            Instant::now() < deadline,
            "delayed provider child did not start"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    use std::os::unix::fs::MetadataExt;
    let child_executable = if trusted_child {
        &trusted_native
    } else {
        &fake
    };
    let child_exe = std::fs::metadata(child_executable).unwrap();
    if trusted_child {
        let effect = tmux::observe_session_effect_identity(&target).unwrap();
        let foreground_exe =
            std::fs::metadata(format!("/proc/{}/exe", effect.foreground_pid)).unwrap();
        assert_eq!(foreground_exe.dev(), expected.executable.device);
        assert_eq!(foreground_exe.ino(), expected.executable.inode);
        assert_ne!(
            (foreground_exe.dev(), foreground_exe.ino()),
            (child_exe.dev(), child_exe.ino())
        );
    } else {
        loop {
            let effect = tmux::observe_session_effect_identity(&target).unwrap();
            let foreground_exe =
                std::fs::metadata(format!("/proc/{}/exe", effect.foreground_pid)).unwrap();
            if foreground_exe.dev() == child_exe.dev() && foreground_exe.ino() == child_exe.ino() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "delayed provider child never became the foreground generation"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    let child_observation = crate::harness::observe_scoped_harness_process(
        &target,
        Harness::Codex,
        &expected,
        &identity.id,
        &identity.secret,
        &owner.cgroup_path,
        owner.tmux.pane_start_ticks,
        Instant::now() + Duration::from_secs(2),
    );
    if trusted_child {
        assert_eq!(
            child_observation.unwrap().executable,
            expected.trusted_child_executable.clone().unwrap()
        );
    } else {
        assert_eq!(
            child_observation.unwrap_err(),
            crate::harness::LaunchAttestationError::ExpectedProvenanceMismatch
        );
    }

    resume_tx.send(()).unwrap();
    let result = worker.join().unwrap();
    let durable = ctx.captains.cortana_identity();
    if trusted_child {
        result.unwrap();
        assert!(matches!(
            durable.recovery,
            crate::cortana_reconcile::CortanaRecoveryState::Healthy { .. }
        ));
        assert_eq!(
            durable
                .active_harness_attestation
                .as_ref()
                .map(|attestation| &attestation.process.executable),
            expected.trusted_child_executable.as_ref()
        );
        assert!(ctx
            .captains
            .snapshot()
            .captains
            .iter()
            .any(|captain| captain.role == FleetRole::Cortana));

        let restart_path = fixture_dir.join("captains-restart.json");
        std::fs::write(
            &restart_path,
            serde_json::to_vec_pretty(&ctx.captains.snapshot()).unwrap(),
        )
        .unwrap();
        let restarted_registry = Arc::new(CaptainsRegistry::load(restart_path));
        let restarted = test_ctx("cortana-active-attestation-restart")
            .with_captains_registry(Arc::clone(&restarted_registry))
            .with_identity_store(Arc::clone(&ctx.identity));
        assert_eq!(
            restarted_registry
                .cortana_identity()
                .active_harness_attestation,
            durable.active_harness_attestation
        );
        let authorized = resolve_identity(&restarted, &identity.secret).unwrap();
        assert_eq!(authorized.fleet_role, Some(FleetRole::Cortana));
        assert!(control_lease_authority(&restarted, &authorized).is_ok());

        let legacy_path = fixture_dir.join("captains-schema28-restart.json");
        let mut legacy_document = serde_json::to_value(restarted_registry.snapshot()).unwrap();
        legacy_document["schemaVersion"] = json!(28);
        let legacy_cortana = legacy_document["cortana"].as_object_mut().unwrap();
        legacy_cortana.remove("activeHarnessAttestation");
        legacy_cortana.remove("activeHarnessAttestationRecovery");
        std::fs::write(
            &legacy_path,
            serde_json::to_vec_pretty(&legacy_document).unwrap(),
        )
        .unwrap();
        let legacy_registry = Arc::new(CaptainsRegistry::load(legacy_path));
        let legacy = Arc::new(
            test_ctx("cortana-schema28-live-upgrade")
                .with_captains_registry(Arc::clone(&legacy_registry))
                .with_identity_store(Arc::clone(&ctx.identity)),
        );
        assert!(matches!(
            legacy_registry.cortana_identity().recovery,
            crate::cortana_reconcile::CortanaRecoveryState::Degraded { .. }
        ));
        let (reached_tx, reached_rx) = mpsc::sync_channel(1);
        let (resume_tx, resume_rx) = mpsc::sync_channel(1);
        legacy_registry.set_dispatch_barrier(Some(DispatchBarrier {
            boundary: "cortana_active_attestation_recovery_prepared",
            reached: reached_tx,
            resume: resume_rx,
        }));
        let upgrading = {
            let legacy = Arc::clone(&legacy);
            let home = home.clone();
            let command = command.clone();
            std::thread::spawn(move || {
                dispatch(
                    &legacy,
                    "reconcile_cortana",
                    &json!({
                        "operationId": "schema28-live-upgrade",
                        "testOrchestratorHome": home,
                        "testStartupCommand": command,
                    }),
                )
            })
        };
        assert_eq!(
            reached_rx.recv_timeout(TEST_ASYNC_FIXTURE_TIMEOUT).unwrap(),
            "cortana_active_attestation_recovery_prepared"
        );
        let staged = legacy_registry.cortana_identity();
        assert!(staged.active_harness_attestation.is_none());
        assert!(staged.active_harness_attestation_recovery.is_some());
        let crash_path = fixture_dir.join("captains-schema30-staged-restart.json");
        std::fs::write(
            &crash_path,
            serde_json::to_vec_pretty(&legacy_registry.snapshot()).unwrap(),
        )
        .unwrap();
        resume_tx.send(()).unwrap();
        upgrading.join().unwrap().unwrap();

        let crash_registry = Arc::new(CaptainsRegistry::load(crash_path));
        let legacy = Arc::new(
            test_ctx("cortana-schema30-staged-restart")
                .with_captains_registry(Arc::clone(&crash_registry))
                .with_identity_store(Arc::clone(&ctx.identity)),
        );
        dispatch(
            &legacy,
            "reconcile_cortana",
            &json!({
                "operationId": "ignored-after-staged-restart",
                "testOrchestratorHome": home,
                "testStartupCommand": command,
            }),
        )
        .unwrap();
        let upgraded = crash_registry.cortana_identity();
        assert!(matches!(
            upgraded.recovery,
            crate::cortana_reconcile::CortanaRecoveryState::Healthy { .. }
        ));
        assert!(upgraded.active_harness_attestation.is_some());
        assert!(upgraded.active_harness_attestation_recovery.is_none());
        assert!(upgraded.managed_launch.is_none());
        std::thread::scope(|scope| {
            let mut workers = Vec::new();
            for _ in 0..8 {
                let legacy = Arc::clone(&legacy);
                let home = home.clone();
                let command = command.clone();
                workers.push(scope.spawn(move || {
                    dispatch(
                        &legacy,
                        "reconcile_cortana",
                        &json!({
                            "operationId": "schema30-concurrent-keep",
                            "testOrchestratorHome": home,
                            "testStartupCommand": command,
                        }),
                    )
                }));
            }
            for worker in workers {
                worker.join().unwrap().unwrap();
            }
        });
        assert_eq!(
            crash_registry
                .snapshot()
                .captains
                .iter()
                .filter(|claim| claim.role == FleetRole::Cortana)
                .count(),
            1
        );

        let process_pid = durable
            .active_harness_attestation
            .as_ref()
            .unwrap()
            .process
            .pid;
        let killed = std::process::Command::new("/bin/kill")
            .args(["-KILL", &process_pid.to_string()])
            .output()
            .unwrap();
        assert!(killed.status.success(), "{killed:?}");
        while std::path::Path::new(&format!("/proc/{process_pid}")).exists() {
            assert!(
                Instant::now() < deadline,
                "attested child did not exit after substitution"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(control_lease_authority(&restarted, &authorized).is_err());
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    let revoked = resolve_identity(&restarted, &identity.secret).unwrap();
                    assert_eq!(revoked.fleet_role, None);
                    assert_eq!(revoked.mint_role, crate::identity::Role::Unknown);
                });
            }
        });
    } else {
        assert!(!result.unwrap_err().trim().is_empty());
        assert!(durable
            .managed_launch
            .as_ref()
            .is_some_and(|launch| launch.harness_process.is_none()));
        assert!(!matches!(
            durable.recovery,
            crate::cortana_reconcile::CortanaRecoveryState::Healthy { .. }
        ));
        assert!(!ctx
            .captains
            .snapshot()
            .captains
            .iter()
            .any(|captain| captain.role == FleetRole::Cortana));
    }

    tmux::retire_managed_runtime(&target, &owner).unwrap();
    std::fs::remove_dir_all(fixture_dir).unwrap();
}

#[cfg(unix)]
#[test]
fn delayed_node_wrapper_waits_for_exact_trusted_native_child() {
    delayed_node_wrapper_attestation_case(true);
}

#[cfg(unix)]
#[test]
fn delayed_node_wrapper_rejects_foreign_same_provider_child() {
    delayed_node_wrapper_attestation_case(false);
}

#[cfg(unix)]
#[test]
fn post_claim_process_change_retains_wal_and_claim_without_publishing_healthy() {
    if tmux::managed_runtime_preflight().is_err() {
        return;
    }
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let mut context = test_ctx("cortana-post-claim-revalidation").with_apply_sink(sink);
    context.addr = "127.0.0.1:4258".into();
    let ctx = Arc::new(context);
    let _runtime_cleanup = ManagedCortanaTestCleanup::new(Arc::clone(&ctx));
    ctx.tab_registry().replace(vec![TabRecord {
        id: CAPTAIN_WORKSPACE_ID.into(),
        name: CAPTAIN_WORKSPACE_NAME.into(),
        tile_ids: Vec::new(),
    }]);
    let home = std::env::temp_dir().join(format!(
        "t-hub-cortana-post-claim-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&home).unwrap();
    let (harness_dir, command) = test_harness_command("codex");
    let (reached_tx, reached_rx) = mpsc::sync_channel(1);
    let (resume_tx, resume_rx) = mpsc::sync_channel(1);
    ctx.captains.set_dispatch_barrier(Some(DispatchBarrier {
        boundary: "cortana_after_claim",
        reached: reached_tx,
        resume: resume_rx,
    }));
    let worker_ctx = Arc::clone(&ctx);
    let worker_home = home.clone();
    let worker_command = command.clone();
    let worker = std::thread::spawn(move || {
        dispatch(
            &worker_ctx,
            "reconcile_cortana",
            &json!({
                "operationId": "cortana-post-claim-operation",
                "testOrchestratorHome": worker_home,
                "testStartupCommand": worker_command,
            }),
        )
    });
    let reached = reached_rx.recv_timeout(TEST_ASYNC_FIXTURE_TIMEOUT);
    if reached.is_err() && worker.is_finished() {
        panic!(
            "Cortana exited before its post-claim boundary: {:?}",
            worker.join().unwrap()
        );
    }
    assert_eq!(
        reached.expect("Cortana did not reach its post-claim boundary"),
        "cortana_after_claim"
    );
    let claimed = ctx.captains.cortana_identity();
    assert!(claimed.managed_launch.is_some());
    assert!(ctx
        .captains
        .snapshot()
        .captains
        .iter()
        .any(|captain| captain.role == FleetRole::Cortana));
    let harness_pid = claimed
        .managed_launch
        .as_ref()
        .and_then(|launch| launch.harness_process.as_ref())
        .map(|process| process.pid)
        .unwrap();
    let kill = std::process::Command::new("/bin/kill")
        .arg("-KILL")
        .arg(harness_pid.to_string())
        .output()
        .unwrap();
    assert!(kill.status.success(), "{kill:?}");
    let deadline = Instant::now() + Duration::from_secs(5);
    while std::path::Path::new(&format!("/proc/{harness_pid}")).exists() {
        assert!(
            Instant::now() < deadline,
            "provider process did not exit after claim"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    resume_tx.send(()).unwrap();
    let error = worker.join().unwrap().unwrap_err();
    assert!(error.contains("WAL and Fleet claim retained"), "{error}");
    let retained = ctx.captains.cortana_identity();
    assert!(retained.managed_launch.is_some());
    assert!(!matches!(
        retained.recovery,
        crate::cortana_reconcile::CortanaRecoveryState::Healthy { .. }
    ));
    assert!(ctx
        .captains
        .snapshot()
        .captains
        .iter()
        .any(|captain| captain.role == FleetRole::Cortana));
    let launch = retained.managed_launch.as_ref().unwrap();
    let owner = retained.owner.as_ref().unwrap();
    tmux::retire_managed_runtime(&launch.tmux_target, &tmux_cortana_owner(owner)).unwrap();
    std::fs::remove_dir_all(harness_dir).unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[cfg(unix)]
fn managed_owner_generation_mutation_case(
    boundary: &'static str,
    expect_retained_claim: bool,
    case: &str,
) {
    if tmux::managed_runtime_preflight().is_err() {
        return;
    }
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let mut context = test_ctx(&format!("cortana-owner-generation-{case}")).with_apply_sink(sink);
    context.addr = "127.0.0.1:4261".into();
    let ctx = Arc::new(context);
    let _runtime_cleanup = ManagedCortanaTestCleanup::new(Arc::clone(&ctx));
    ctx.tab_registry().replace(vec![TabRecord {
        id: CAPTAIN_WORKSPACE_ID.into(),
        name: CAPTAIN_WORKSPACE_NAME.into(),
        tile_ids: Vec::new(),
    }]);
    let home = std::env::temp_dir().join(format!(
        "t-hub-cortana-owner-generation-{case}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&home).unwrap();
    let (harness_dir, command) = test_harness_command("codex");
    let operation_id = format!("cortana-owner-generation-{case}");
    let (reached_tx, reached_rx) = mpsc::sync_channel(1);
    let (resume_tx, resume_rx) = mpsc::sync_channel(1);
    ctx.captains.set_dispatch_barrier(Some(DispatchBarrier {
        boundary,
        reached: reached_tx,
        resume: resume_rx,
    }));
    let worker_ctx = Arc::clone(&ctx);
    let worker_home = home.clone();
    let worker_command = command.clone();
    let worker_operation_id = operation_id.clone();
    let worker = std::thread::spawn(move || {
        dispatch(
            &worker_ctx,
            "reconcile_cortana",
            &json!({
                "operationId": worker_operation_id,
                "testOrchestratorHome": worker_home,
                "testStartupCommand": worker_command,
            }),
        )
    });
    assert_eq!(
        reached_rx
            .recv_timeout(TEST_ASYNC_FIXTURE_TIMEOUT)
            .expect("Cortana did not reach full owner revalidation"),
        boundary
    );
    let observed = ctx.captains.cortana_identity();
    let launch = observed.managed_launch.clone().unwrap();
    assert_eq!(
        launch.phase == crate::cortana_reconcile::CortanaManagedLaunchPhase::Claimed,
        expect_retained_claim
    );
    let old_owner = observed.owner.clone().unwrap();
    let identity_secret = ctx.identity.get(&launch.identity_id).unwrap().secret;
    let bootstrap = dispatch_authenticated(
        &ctx,
        req_session(
            &ctx.read_token,
            &identity_secret,
            "cortana_bootstrap",
            json!({}),
        ),
    );
    assert!(bootstrap.ok, "{:?}", bootstrap.error);
    assert_eq!(bootstrap.result.unwrap()["cortana"]["state"], "inFlight");
    let mut reused_owner = old_owner.clone();
    reused_owner.invocation_id = if old_owner.invocation_id.starts_with('f') {
        format!("e{}", &old_owner.invocation_id[1..])
    } else {
        format!("f{}", &old_owner.invocation_id[1..])
    };
    assert_eq!(reused_owner.cgroup_path, old_owner.cgroup_path);
    assert_eq!(reused_owner.cgroup_inode, old_owner.cgroup_inode);
    assert_eq!(reused_owner.launcher_pid, old_owner.launcher_pid);
    assert_eq!(
        reused_owner.launcher_start_ticks,
        old_owner.launcher_start_ticks
    );
    assert_eq!(reused_owner.tmux, old_owner.tmux);
    ctx.captains
        .replace_cortana_runtime_owner_for_test(&old_owner, reused_owner.clone())
        .unwrap();
    assert!(tmux::revalidate_managed_runtime_owner(
        &launch.tmux_target,
        &tmux_cortana_owner(&reused_owner)
    )
    .is_err());
    resume_tx.send(()).unwrap();

    let error = worker.join().unwrap().unwrap_err();
    assert!(
        error.contains("managed launch changed after outside-lock observation"),
        "{error}"
    );
    let retained = ctx.captains.cortana_identity();
    assert!(retained.managed_launch.is_some());
    assert!(!matches!(
        retained.recovery,
        crate::cortana_reconcile::CortanaRecoveryState::Healthy { .. }
    ));
    assert_eq!(
        ctx.captains
            .snapshot()
            .captains
            .iter()
            .filter(|captain| captain.role == FleetRole::Cortana)
            .count(),
        usize::from(expect_retained_claim)
    );
    let denied = resolve_identity(&ctx, &identity_secret).unwrap();
    assert_eq!(denied.fleet_role, None);
    assert!(control_lease_authority(&ctx, &denied).is_err());
    tmux::revalidate_managed_runtime_owner(&launch.tmux_target, &tmux_cortana_owner(&old_owner))
        .unwrap();
    assert_eq!(
        tmux::session_liveness(&launch.tmux_target),
        tmux::SessionLiveness::Alive
    );

    let retry_error = dispatch(
        &ctx,
        "reconcile_cortana",
        &json!({
            "operationId": operation_id.clone(),
            "testOrchestratorHome": home.clone(),
            "testStartupCommand": command.clone(),
        }),
    )
    .unwrap_err();
    assert!(
        retry_error.contains("managed launch owner changed"),
        "{retry_error}"
    );
    tmux::revalidate_managed_runtime_owner(&launch.tmux_target, &tmux_cortana_owner(&old_owner))
        .unwrap();
    ctx.captains
        .replace_cortana_runtime_owner_for_test(&reused_owner, old_owner.clone())
        .unwrap();
    let recovered = dispatch(
        &ctx,
        "reconcile_cortana",
        &json!({
            "operationId": operation_id,
            "testOrchestratorHome": home.clone(),
            "testStartupCommand": command.clone(),
        }),
    )
    .unwrap();
    assert_eq!(recovered["healthy"], true);
    assert_eq!(
        ctx.captains
            .snapshot()
            .captains
            .iter()
            .filter(|captain| captain.role == FleetRole::Cortana)
            .count(),
        1
    );

    tmux::retire_managed_runtime(&launch.tmux_target, &tmux_cortana_owner(&old_owner)).unwrap();
    std::fs::remove_dir_all(harness_dir).unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[cfg(unix)]
#[test]
fn managed_owner_observed_launch_can_bootstrap_before_process_wal_commit() {
    managed_owner_generation_mutation_case(
        "cortana_before_owner_revalidation_owner_observed",
        false,
        "owner-observed",
    );
}

#[cfg(unix)]
#[test]
fn managed_owner_generation_mutation_before_claim_fails_closed() {
    managed_owner_generation_mutation_case(
        "cortana_before_owner_revalidation_observed",
        false,
        "before-claim",
    );
}

#[cfg(unix)]
#[test]
fn managed_owner_generation_mutation_before_healthy_retains_non_authoritative_claim() {
    managed_owner_generation_mutation_case(
        "cortana_before_owner_revalidation_claimed",
        true,
        "before-healthy",
    );
}

#[cfg(unix)]
#[test]
fn cortana_ancestry_observation_uses_one_deadline_outside_admission() {
    if tmux::managed_runtime_preflight().is_err() {
        return;
    }
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let mut context = test_ctx("cortana-ancestry-deadline")
        .with_live_sessions(|| Ok(Vec::new()))
        .with_apply_sink(sink);
    context.addr = "127.0.0.1:4260".into();
    context.tab_registry().replace(vec![TabRecord {
        id: CAPTAIN_WORKSPACE_ID.into(),
        name: CAPTAIN_WORKSPACE_NAME.into(),
        tile_ids: Vec::new(),
    }]);
    let home = std::env::temp_dir().join(format!(
        "t-hub-cortana-ancestry-deadline-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&home).unwrap();
    let (harness_dir, command) = test_harness_command("codex");
    dispatch(
        &context,
        "reconcile_cortana",
        &json!({
            "operationId": "cortana-ancestry-deadline-setup",
            "testOrchestratorHome": home,
            "testStartupCommand": command,
        }),
    )
    .unwrap();
    let active = context.captains.cortana_identity();
    assert!(matches!(
        active.recovery,
        crate::cortana_reconcile::CortanaRecoveryState::Healthy { .. }
    ));
    let terminal_id = active.terminal_id.clone().unwrap();
    let identity_id = active.identity_id.clone().unwrap();
    let generation = active.generation;
    let quarantine_ledger = active.quarantine_ledger.clone();
    let owner = active.owner.clone().unwrap();
    let sessions_before = tmux::list_sessions().unwrap();

    // Keep this retry observation-only after an uncertain result. That
    // makes the elapsed bound measure the shared observation deadline,
    // without allowing a replacement spawn to enter the result path.
    context.apply_sink = None;
    let ctx = Arc::new(context);
    let (reached_tx, reached_rx) = mpsc::channel();
    let worker_ctx = Arc::clone(&ctx);
    let worker_home = home.clone();
    let worker_command = command.clone();
    let worker = std::thread::spawn(move || {
        crate::harness::stall_next_scoped_ancestry_batch_for_current_thread(reached_tx);
        let started = Instant::now();
        let result = dispatch(
            &worker_ctx,
            "reconcile_cortana",
            &json!({
                "operationId": "cortana-ancestry-deadline-revalidate",
                "testOrchestratorHome": worker_home,
                "testStartupCommand": worker_command,
            }),
        );
        (result, started.elapsed())
    });
    reached_rx
        .recv_timeout(Duration::from_secs(3))
        .expect("Cortana did not reach the late ancestry observation boundary");
    let admission = ctx
        .dispatch_admission
        .try_lock()
        .expect("late ancestry observation held dispatch admission");
    drop(admission);

    let (result, elapsed) = worker.join().unwrap();
    let error = result.unwrap_err();
    assert!(
        is_retryable_error(&error),
        "an inconclusive ancestry observation must be retryable: {error:?}"
    );
    assert!(
        elapsed < Duration::from_secs(3),
        "one-second aggregate observation deadline took {elapsed:?}"
    );
    assert!(
        ctx.dispatch_admission.try_lock().is_ok(),
        "dispatch admission remained unavailable after observation timeout"
    );
    let retained = ctx.captains.cortana_identity();
    assert_eq!(retained.identity_id.as_deref(), Some(identity_id.as_str()));
    assert_eq!(retained.terminal_id.as_deref(), Some(terminal_id.as_str()));
    assert_eq!(retained.generation, generation);
    assert_eq!(retained.quarantine_ledger, quarantine_ledger);
    assert!(ctx.captains.snapshot().captains.iter().any(|captain| {
        captain.role == FleetRole::Cortana
            && captain.state == ClaimState::Active
            && captain.terminal_id.as_deref() == Some(terminal_id.as_str())
    }));
    let retained_identity = ctx.identity.get(&identity_id).unwrap();
    assert_eq!(
        retained_identity.session_tile.as_deref(),
        Some(terminal_id.as_str())
    );
    assert_eq!(
        tmux::session_liveness(&tmux_target(&terminal_id)),
        tmux::SessionLiveness::Alive
    );
    assert_eq!(
        tmux::list_sessions().unwrap(),
        sessions_before,
        "a transient observation must not spawn a replacement Cortana"
    );

    tmux::retire_managed_runtime(&tmux_target(&terminal_id), &tmux_cortana_owner(&owner)).unwrap();
    std::fs::remove_dir_all(harness_dir).unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn captured_packaged_observed_launch_requires_its_wal_before_generic_planning() {
    let fixture: Value = serde_json::from_str(PACKAGED_SCHEMA_25_OBSERVED_LAUNCH_FIXTURE).unwrap();
    let registry_path = captains_tmp("captured-observed-planning");
    std::fs::write(
        &registry_path,
        serde_json::to_vec(&fixture["captainsSnapshot"]).unwrap(),
    )
    .unwrap();
    let snapshot = CaptainsRegistry::read_snapshot(&registry_path).unwrap();
    std::fs::remove_file(registry_path).unwrap();
    CaptainsRegistry::validate_snapshot(&snapshot).unwrap();
    let durable = snapshot.cortana;
    let launch = durable.managed_launch.as_ref().unwrap();
    let owner = durable.owner.as_ref().unwrap();
    let candidate = crate::cortana_reconcile::CortanaRuntimeCandidate {
        terminal_id: launch.terminal_id.clone(),
        identity_id: Some(launch.identity_id.clone()),
        generation: launch.generation,
        harness: launch.harness.clone(),
        provider_session_id: None,
        terminal: crate::cortana_reconcile::RuntimeEvidence::Alive,
        harness_process: crate::cortana_reconcile::RuntimeEvidence::Alive,
        identity_bound_to_terminal: true,
        canonical_control_file: true,
        rotating_control_env_scrubbed: true,
        stale_legacy_control_env: false,
        unresolved_session_bearer: false,
        effect_identity: Some(owner.tmux),
        current_control_capability: true,
        trusted_cortana_identity: true,
    };

    let generic = crate::cortana_reconcile::plan_reconciliation(
        &durable,
        &launch.operation_id,
        std::slice::from_ref(&candidate),
    );
    assert_eq!(
        generic.action,
        crate::cortana_reconcile::CortanaReconcileAction::Degraded
    );
    assert!(
        generic
            .degraded_reason
            .as_deref()
            .unwrap()
            .contains("different Cortana identity than the durable singleton"),
        "{:?}",
        generic.degraded_reason
    );
    assert!(observed_launch_matches_recovery(&durable, launch));
    assert!(
        exact_observed_cortana_candidate(std::slice::from_ref(&candidate), launch, owner).is_some()
    );

    let mut foreground_changed = candidate.clone();
    let effect = foreground_changed.effect_identity.as_mut().unwrap();
    effect.foreground_pid = effect.foreground_pid.saturating_add(100);
    effect.foreground_start_ticks = effect.foreground_start_ticks.saturating_add(100);
    effect.foreground_process_group_id = effect.foreground_pid;
    assert!(exact_observed_cortana_candidate(
        std::slice::from_ref(&foreground_changed),
        launch,
        owner
    )
    .is_some());

    let mut reused_pane = candidate.clone();
    reused_pane
        .effect_identity
        .as_mut()
        .unwrap()
        .pane_start_ticks += 1;
    assert!(
        exact_observed_cortana_candidate(std::slice::from_ref(&reused_pane), launch, owner)
            .is_none()
    );
    let mut wrong_generation = candidate.clone();
    wrong_generation.generation += 1;
    assert!(exact_observed_cortana_candidate(
        std::slice::from_ref(&wrong_generation),
        launch,
        owner
    )
    .is_none());
    assert!(exact_observed_cortana_candidate(
        &[candidate.clone(), wrong_generation],
        launch,
        owner
    )
    .is_none());
    let mut stale_control = candidate;
    stale_control.current_control_capability = false;
    assert!(
        exact_observed_cortana_candidate(std::slice::from_ref(&stale_control), launch, owner)
            .is_none()
    );

    let exact_claim: CaptainRecord = serde_json::from_value(json!({
        "shipSlug": CORTANA_SLUG,
        "assignmentId": assignment_id_for(None, CORTANA_SLUG),
        "displayName": "Cortana",
        "role": "cortana",
        "provider": "codex",
        "terminalId": launch.terminal_id,
        "harness": "codex",
        "workspaceTabIds": [CAPTAIN_WORKSPACE_ID],
        "crew": [],
        "state": {"kind": "active"}
    }))
    .unwrap();
    assert!(exact_observed_cortana_claim(&exact_claim, launch));
    let mut foreign_claim = exact_claim;
    foreign_claim.terminal_id = Some("foreign1".into());
    assert!(!exact_observed_cortana_claim(&foreign_claim, launch));

    let mut owner_disagreement = durable.clone();
    owner_disagreement.owner.as_mut().unwrap().unit_name =
        format!("t-hub-{}.scope", "f".repeat(32));
    let ctx = test_ctx("captured-owner-disagreement");
    let error = finalize_observed_cortana_launch(
        &ctx,
        &launch.operation_id,
        &owner_disagreement,
        &[],
        None,
    )
    .unwrap_err();
    assert!(error.contains("observed launch and managed owner disagree"));
    assert!(ctx.captains.snapshot().captains.is_empty());
}

#[cfg(unix)]
#[test]
fn captured_observed_launch_reload_and_duplicate_reconcile_finalize_once() {
    if tmux::managed_runtime_preflight().is_err() || !tmux_process_tests_available() {
        return;
    }
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let fixture: Value = serde_json::from_str(PACKAGED_SCHEMA_25_OBSERVED_LAUNCH_FIXTURE).unwrap();
    let registry_path = captains_tmp("captured-observed-live");
    let identity_path = captains_tmp("captured-observed-live-identities");
    let _ = std::fs::remove_file(&registry_path);
    let _ = std::fs::remove_file(&identity_path);
    let identities = Arc::new(crate::identity::IdentityStore::load(identity_path.clone()));
    let legacy_identity = identities.mint(crate::identity::Role::Cortana).unwrap();
    identities.revoke(&legacy_identity.id).unwrap();
    let replacement_identity = identities.mint(crate::identity::Role::Cortana).unwrap();
    let legacy_terminal = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    let replacement_terminal = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    identities
        .bind_tile(&replacement_identity.id, &replacement_terminal)
        .unwrap();
    let home = std::env::temp_dir().join(format!(
        "t-hub-captured-observed-live-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&home).unwrap();
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    let control_token = "captured-observed-control-token";
    let mut spawn_ctx = test_ctx(control_token).with_identity_store(identities.clone());
    spawn_ctx.addr = "127.0.0.1:4242".into();
    let legacy_target = tmux_target(&legacy_terminal);
    create_test_tmux_session_with_env(
        &legacy_target,
        home.to_str().unwrap(),
        Some(&harness_command),
        &[
            (
                crate::identity::SESSION_TOKEN_ENV.into(),
                "captured-unresolved-legacy-token".into(),
            ),
            (CORTANA_GENERATION_ENV.into(), "1".into()),
            ("T_HUB_CONTROL_ADDR".into(), "127.0.0.1:31337".into()),
            ("T_HUB_CONTROL_TOKEN".into(), "retired-control-token".into()),
        ],
    )
    .unwrap();
    wait_for_harness_started(&legacy_terminal, "codex").unwrap();
    let legacy_effect = durable_cortana_effect_identity(
        tmux::observe_session_effect_identity(&legacy_target).unwrap(),
    );

    let operation_id = format!("captured-observed-{}", uuid::Uuid::new_v4());
    let launch = tmux::prepare_managed_runtime_launch().unwrap();
    let spawn_args = json!({
        "cwd": home,
        "name": "Cortana",
        "startupCommand": harness_command,
        "tabId": CAPTAIN_WORKSPACE_ID,
    });
    let mut elevation = elevation_env(&spawn_ctx, &spawn_args);
    elevation.push((
        crate::identity::SESSION_TOKEN_ENV.into(),
        replacement_identity.secret.clone(),
    ));
    elevation.push((CORTANA_GENERATION_ENV.into(), "2".into()));
    elevation.push((
        PROVIDER_SESSION_ENV.into(),
        pending_provider_marker("codex"),
    ));
    let pane = crate::commands::pane_command(None, Some(&harness_command));
    let (_, replacement_target, owner) = spawn_managed_tmux_terminal_with_id(
        &replacement_terminal,
        home.to_str().unwrap(),
        pane.as_deref(),
        &elevation,
        &launch,
    )
    .unwrap();
    wait_for_harness_started(&replacement_terminal, "codex").unwrap();
    let durable_owner = durable_cortana_owner(owner.clone());
    let durable_launch = crate::cortana_reconcile::CortanaManagedLaunchIntent {
        version: 1,
        operation_id: operation_id.clone(),
        terminal_id: replacement_terminal.clone(),
        tmux_target: replacement_target.clone(),
        identity_id: replacement_identity.id.clone(),
        generation: 2,
        harness: "codex".into(),
        unit_name: launch.unit_name.clone(),
        launch_nonce: launch.launch_nonce.clone(),
        tools: durable_cortana_tools(&launch.tools),
        phase: crate::cortana_reconcile::CortanaManagedLaunchPhase::Observed,
        expected_harness_launch_provenance: None,
        harness_process: None,
    };
    let mut snapshot = fixture["captainsSnapshot"].clone();
    let cortana = snapshot.get_mut("cortana").unwrap();
    cortana["identityId"] = json!(legacy_identity.id);
    cortana["generation"] = json!(1);
    cortana["terminalId"] = json!(replacement_terminal);
    cortana["harness"] = json!("codex");
    cortana["owner"] = serde_json::to_value(&durable_owner).unwrap();
    cortana["managedLaunch"] = serde_json::to_value(&durable_launch).unwrap();
    cortana["legacyQuarantine"] = json!({
        "terminalId": legacy_terminal,
        "identityId": legacy_identity.id,
        "generation": 1,
        "harness": "codex",
        "tmux": legacy_effect,
        "authorityRevoked": true,
        "quarantinedAt": now_ms().max(1),
    });
    cortana["recovery"] = json!({
        "kind": "legacyUnownedQuarantined",
        "operation_id": operation_id,
        "quarantined_at": now_ms().max(1),
        "legacy_terminal_id": legacy_terminal,
        "legacy_generation": 1,
        "replacement_identity_id": replacement_identity.id,
    });
    std::fs::write(
        &registry_path,
        serde_json::to_vec_pretty(&snapshot).unwrap(),
    )
    .unwrap();

    let captains = Arc::new(CaptainsRegistry::load(registry_path.clone()));
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let mut ctx = test_ctx(control_token)
        .with_captains_registry(captains.clone())
        .with_identity_store(identities.clone())
        .with_apply_sink(sink.clone());
    ctx.addr = spawn_ctx.addr.clone();
    ctx.tab_registry().replace(vec![TabRecord {
        id: CAPTAIN_WORKSPACE_ID.into(),
        name: CAPTAIN_WORKSPACE_NAME.into(),
        tile_ids: vec![],
    }]);
    let ctx = Arc::new(ctx);
    let changed_registry_path = captains_tmp("captured-observed-changed-provider");
    std::fs::copy(&registry_path, &changed_registry_path).unwrap();
    let changed_captains = Arc::new(CaptainsRegistry::load(changed_registry_path.clone()));
    let changed_sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let mut changed_ctx = test_ctx("captured-observed-changed-provider")
        .with_captains_registry(changed_captains.clone())
        .with_identity_store(identities.clone())
        .with_apply_sink(changed_sink);
    changed_ctx.addr = spawn_ctx.addr.clone();
    changed_ctx.tab_registry().replace(vec![TabRecord {
        id: CAPTAIN_WORKSPACE_ID.into(),
        name: CAPTAIN_WORKSPACE_NAME.into(),
        tile_ids: vec![],
    }]);
    let (changed_harness_dir, changed_harness_command) = test_harness_command("codex");
    let changed_error = dispatch(
        &changed_ctx,
        "reconcile_cortana",
        &json!({
            "operationId": "rotated-request-must-not-replace-captured-operation",
            "testOrchestratorHome": home,
            "testStartupCommand": changed_harness_command,
        }),
    )
    .unwrap_err();
    assert!(
        changed_error.contains("managed Harness process attestation failed"),
        "{changed_error}"
    );
    let changed_durable = changed_captains.cortana_identity();
    assert!(changed_durable.managed_launch.is_some());
    assert!(!matches!(
        changed_durable.recovery,
        crate::cortana_reconcile::CortanaRecoveryState::Healthy { .. }
    ));
    assert!(!changed_captains
        .snapshot()
        .captains
        .iter()
        .any(|captain| captain.role == FleetRole::Cortana));
    std::fs::remove_dir_all(changed_harness_dir).unwrap();
    std::fs::remove_file(changed_registry_path).unwrap();
    let captured = captains.cortana_identity();
    assert_eq!(captured.managed_launch.as_ref().unwrap().version, 1);
    assert!(captured
        .managed_launch
        .as_ref()
        .unwrap()
        .expected_harness_launch_provenance
        .is_none());
    assert!(captured
        .managed_launch
        .as_ref()
        .unwrap()
        .harness_process
        .is_none());
    let expected = crate::harness::resolve_expected_harness_launch_provenance(
        &harness_command,
        Harness::Codex,
    )
    .unwrap();
    let provenance_enriched = captains
        .record_cortana_expected_harness_launch_provenance(
            &operation_id,
            &replacement_terminal,
            &replacement_identity.id,
            2,
            expected.clone(),
        )
        .unwrap();
    assert_eq!(
        provenance_enriched
            .managed_launch
            .as_ref()
            .and_then(|launch| launch.expected_harness_launch_provenance.as_ref()),
        Some(&expected)
    );
    assert_eq!(
        CaptainsRegistry::load(registry_path.clone()).cortana_identity(),
        provenance_enriched
    );
    let enriched = attest_cortana_managed_harness(&ctx, &provenance_enriched).unwrap();
    let enriched_launch = enriched.managed_launch.as_ref().unwrap();
    assert_eq!(enriched_launch.version, 4);
    assert_eq!(
        enriched_launch.phase,
        crate::cortana_reconcile::CortanaManagedLaunchPhase::Observed
    );
    assert!(enriched_launch.harness_process.is_some());
    assert_eq!(
        CaptainsRegistry::load(registry_path.clone()).cortana_identity(),
        enriched
    );
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let results = std::thread::scope(|scope| {
        let first = {
            let ctx = ctx.clone();
            let barrier = barrier.clone();
            let home = home.clone();
            let harness_command = harness_command.clone();
            scope.spawn(move || {
                barrier.wait();
                dispatch(
                    &ctx,
                    "reconcile_cortana",
                    &json!({
                        "operationId": "duplicate-observed-a",
                        "testOrchestratorHome": home,
                        "testStartupCommand": harness_command,
                    }),
                )
            })
        };
        let second = {
            let ctx = ctx.clone();
            let barrier = barrier.clone();
            let home = home.clone();
            let harness_command = harness_command.clone();
            scope.spawn(move || {
                barrier.wait();
                dispatch(
                    &ctx,
                    "reconcile_cortana",
                    &json!({
                        "operationId": "duplicate-observed-b",
                        "testOrchestratorHome": home,
                        "testStartupCommand": harness_command,
                    }),
                )
            })
        };
        barrier.wait();
        [first.join().unwrap(), second.join().unwrap()]
    });
    for result in results {
        let result = result.unwrap();
        assert_eq!(result["healthy"], true);
        assert_eq!(result["terminalId"], replacement_terminal);
        assert_eq!(result["identityId"], replacement_identity.id);
        assert_eq!(result["generation"], 2);
    }
    let durable = captains.cortana_identity();
    assert!(durable.managed_launch.is_none());
    assert!(matches!(
        durable.recovery,
        crate::cortana_reconcile::CortanaRecoveryState::Healthy { .. }
    ));
    assert_eq!(
        durable.identity_id.as_deref(),
        Some(replacement_identity.id.as_str())
    );
    let claims = captains
        .snapshot()
        .captains
        .into_iter()
        .filter(|claim| claim.role == FleetRole::Cortana && claim.state == ClaimState::Active)
        .collect::<Vec<_>>();
    assert_eq!(claims.len(), 1);
    assert_eq!(
        claims[0].terminal_id.as_deref(),
        Some(replacement_terminal.as_str())
    );
    assert_eq!(
        tmux::session_liveness(&legacy_target),
        tmux::SessionLiveness::Alive
    );
    assert_eq!(
        tmux::session_liveness(&replacement_target),
        tmux::SessionLiveness::Alive
    );
    assert!(sink
        .calls
        .lock()
        .unwrap()
        .iter()
        .all(|(command, _)| command != "spawn_terminal"));

    dispatch(
        &ctx,
        "close_terminal",
        &json!({"sessionId": replacement_terminal}),
    )
    .unwrap();
    reap_test_tmux_session_and_assert_absent(&legacy_target);
    std::fs::remove_dir_all(harness_bin_dir).ok();
    std::fs::remove_dir_all(home).ok();
    std::fs::remove_file(registry_path).ok();
    std::fs::remove_file(identity_path).ok();
}

#[cfg(unix)]
#[test]
fn cortana_managed_launch_wal_recovers_live_effect_before_healthy_commit() {
    if tmux::managed_runtime_preflight().is_err() {
        return;
    }
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let path = captains_tmp("cortana-managed-launch-live");
    let _ = std::fs::remove_file(&path);
    let registry = powder_lifecycle_registry(Some(path.clone()));
    registry.begin_cortana_recovery("wal-live").unwrap();
    let launch = tmux::prepare_managed_runtime_launch().unwrap();
    registry
        .prepare_cortana_managed_launch(
            "wal-live",
            "wal00002",
            "identity-wal-live",
            1,
            "codex",
            &launch,
            synthetic_cortana_expected_harness_launch("codex"),
        )
        .unwrap();
    let target = tmux_target("wal00002");
    let owner = tmux::new_prepared_managed_session_with_env(
        &target,
        "/tmp",
        Some("sleep 60"),
        &[],
        &launch,
    )
    .unwrap();

    let restarted = powder_lifecycle_registry(Some(path.clone()));
    let prepared = restarted.cortana_identity();
    assert_eq!(
        prepared.managed_launch.as_ref().unwrap().phase,
        crate::cortana_reconcile::CortanaManagedLaunchPhase::Prepared
    );
    let recovered_owner = tmux::observe_prepared_managed_runtime_owner(&target, &launch)
        .expect("restart must recover the exact live prepared effect");
    assert_eq!(recovered_owner, owner);
    let observed = restarted
        .record_cortana_runtime_owner(
            "wal-live",
            "wal00002",
            durable_cortana_owner(recovered_owner),
        )
        .unwrap();
    assert_eq!(observed.terminal_id.as_deref(), Some("wal00002"));
    assert!(observed.owner.is_some());
    assert_eq!(
        observed.managed_launch.as_ref().unwrap().phase,
        crate::cortana_reconcile::CortanaManagedLaunchPhase::OwnerObserved
    );
    assert_eq!(
        powder_lifecycle_registry(Some(path.clone())).cortana_identity(),
        observed
    );

    tmux::kill_session(&target).unwrap();
    tmux::retire_managed_runtime(&target, &owner).unwrap();
    restarted
        .clear_prepared_cortana_managed_launch(observed.managed_launch.as_ref().unwrap())
        .unwrap();
    let cleaned = restarted.cortana_identity();
    assert!(cleaned.managed_launch.is_none());
    assert!(cleaned.owner.is_none());
    assert!(cleaned.terminal_id.is_none());
    let _ = std::fs::remove_file(path);
}

#[cfg(unix)]
#[test]
fn prepared_cortana_cleanup_preserves_wal_and_active_unknown_generation() {
    if tmux::managed_runtime_preflight().is_err() {
        return;
    }
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let path = captains_tmp("cortana-prepared-cleanup-active");
    let _ = std::fs::remove_file(&path);
    let registry = powder_lifecycle_registry(Some(path.clone()));
    registry.begin_cortana_recovery("wal-active").unwrap();
    let launch = tmux::prepare_managed_runtime_launch().unwrap();
    let prepared = registry
        .prepare_cortana_managed_launch(
            "wal-active",
            "wal00003",
            "identity-wal-active",
            1,
            "codex",
            &launch,
            synthetic_cortana_expected_harness_launch("codex"),
        )
        .unwrap();
    let durable_launch = prepared.managed_launch.unwrap();
    let target = tmux_target("wal00003");
    let owner = tmux::new_prepared_managed_session_with_env(
        &target,
        "/tmp",
        Some("sleep 60"),
        &[],
        &launch,
    )
    .unwrap();
    let mut sibling = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .unwrap();
    let sibling_pid = sibling.id();
    let ctx = test_ctx("wal-active-control").with_captains_registry(registry.clone());

    let error = cleanup_cortana_managed_launch(&ctx, &durable_launch, None).unwrap_err();

    assert!(error.contains("populated, reused, or unverifiable"));
    assert_eq!(
        registry.cortana_identity().managed_launch.as_ref(),
        Some(&durable_launch)
    );
    tmux::revalidate_managed_runtime_owner(&target, &owner).unwrap();
    assert!(std::path::Path::new(&format!("/proc/{sibling_pid}")).exists());

    tmux::retire_managed_runtime(&target, &owner).unwrap();
    cleanup_cortana_managed_launch(&ctx, &durable_launch, None).unwrap();
    assert!(registry.cortana_identity().managed_launch.is_none());
    let _ = sibling.kill();
    let _ = sibling.wait();
    let _ = std::fs::remove_file(path);
}

#[cfg(unix)]
#[test]
fn retained_managed_launch_operation_survives_reload_and_rotated_dispatch_ids() {
    if tmux::managed_runtime_preflight().is_err() {
        return;
    }
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();

    for observed_before_error in [false, true] {
        let tag = if observed_before_error {
            "observed"
        } else {
            "prepared"
        };
        let registry_path = captains_tmp(&format!("cortana-wal-operation-{tag}"));
        let identity_path = captains_tmp(&format!("cortana-wal-operation-{tag}-identities"));
        let _ = std::fs::remove_file(&registry_path);
        let _ = std::fs::remove_file(&identity_path);
        let captains = powder_lifecycle_registry(Some(registry_path.clone()));
        let identities = Arc::new(crate::identity::IdentityStore::load(identity_path.clone()));
        let sink = Arc::new(RecordingSink {
            calls: StdMutex::new(Vec::new()),
        });
        let mut ctx = test_ctx(&format!("cortana-wal-operation-{tag}"))
            .with_captains_registry(captains.clone())
            .with_identity_store(identities.clone())
            .with_apply_sink(sink);
        ctx.addr = "127.0.0.1:4242".into();
        ctx.tab_registry().replace(vec![TabRecord {
            id: CAPTAIN_WORKSPACE_ID.into(),
            name: CAPTAIN_WORKSPACE_NAME.into(),
            tile_ids: vec![],
        }]);

        let old_operation = format!("retained-{tag}-operation");
        let rotated_operation = format!("rotated-{tag}-request");
        let fresh_operation = format!("fresh-{tag}-request");
        let terminal_id = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
        let identity = identities.mint(crate::identity::Role::Cortana).unwrap();
        identities.bind_tile(&identity.id, &terminal_id).unwrap();
        captains.begin_cortana_recovery(&old_operation).unwrap();
        let (harness_bin_dir, harness_command) = test_harness_command("codex");
        let expected = crate::harness::resolve_expected_harness_launch_provenance(
            &harness_command,
            Harness::Codex,
        )
        .unwrap();
        let launch = tmux::prepare_managed_runtime_launch().unwrap();
        captains
            .prepare_cortana_managed_launch(
                &old_operation,
                &terminal_id,
                &identity.id,
                1,
                "codex",
                &launch,
                expected,
            )
            .unwrap();

        let home = std::env::temp_dir().join(format!(
            "t-hub-cortana-wal-operation-{tag}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&home).unwrap();
        let spawn_args = json!({
            "cwd": home,
            "name": "Cortana",
            "startupCommand": harness_command,
            "tabId": CAPTAIN_WORKSPACE_ID,
        });
        let mut elevation = elevation_env(&ctx, &spawn_args);
        elevation.push((
            crate::identity::SESSION_TOKEN_ENV.to_string(),
            identity.secret.clone(),
        ));
        elevation.push((CORTANA_GENERATION_ENV.to_string(), "1".into()));
        elevation.push((
            PROVIDER_SESSION_ENV.to_string(),
            pending_provider_marker("codex"),
        ));
        let pane = crate::commands::pane_command(None, Some(&harness_command));
        let (_, target, owner) = spawn_managed_tmux_terminal_with_id(
            &terminal_id,
            home.to_str().unwrap(),
            pane.as_deref(),
            &elevation,
            &launch,
        )
        .unwrap();
        wait_for_harness_started(&terminal_id, "codex").unwrap();
        if observed_before_error {
            captains
                .record_cortana_runtime_owner(
                    &old_operation,
                    &terminal_id,
                    durable_cortana_owner(owner),
                )
                .unwrap();
        }

        let application_error = dispatch(
            &ctx,
            "reconcile_cortana",
            &json!({
                "operationId": old_operation,
                "testOrchestratorHome": "relative-home-is-invalid",
                "testStartupCommand": harness_command,
            }),
        )
        .unwrap_err();
        assert!(application_error.contains("orchestrator home must be an absolute POSIX path"));
        let retained = captains.cortana_identity();
        assert_eq!(
            retained
                .managed_launch
                .as_ref()
                .map(|launch| launch.operation_id.as_str()),
            Some(old_operation.as_str())
        );
        assert!(matches!(
            retained.recovery,
            crate::cortana_reconcile::CortanaRecoveryState::Recovering {
                ref operation_id,
                ..
            } if operation_id == &old_operation
        ));
        drop(ctx);
        drop(captains);

        let restarted = Arc::new(CaptainsRegistry::load(registry_path.clone()));
        let sink = Arc::new(RecordingSink {
            calls: StdMutex::new(Vec::new()),
        });
        let mut restarted_ctx = test_ctx(&format!("cortana-wal-operation-{tag}-restart"))
            .with_captains_registry(restarted.clone())
            .with_identity_store(identities.clone())
            .with_apply_sink(sink);
        restarted_ctx.addr = "127.0.0.1:4242".into();
        restarted_ctx.tab_registry().replace(vec![TabRecord {
            id: CAPTAIN_WORKSPACE_ID.into(),
            name: CAPTAIN_WORKSPACE_NAME.into(),
            tile_ids: vec![],
        }]);
        let recovered = dispatch(
            &restarted_ctx,
            "reconcile_cortana",
            &json!({
                "operationId": rotated_operation,
                "testOrchestratorHome": home,
                "testStartupCommand": harness_command,
            }),
        )
        .unwrap();
        assert_eq!(recovered["operationId"], old_operation);
        let durable = restarted.cortana_identity();
        assert!(durable.managed_launch.is_none());
        assert!(matches!(
            durable.recovery,
            crate::cortana_reconcile::CortanaRecoveryState::Healthy {
                ref operation_id,
                ..
            } if operation_id == &old_operation
        ));

        let fresh = dispatch(
            &restarted_ctx,
            "reconcile_cortana",
            &json!({
                "operationId": fresh_operation,
                "testOrchestratorHome": home,
                "testStartupCommand": harness_command,
            }),
        )
        .unwrap();
        assert_eq!(fresh["operationId"], fresh_operation);
        assert!(restarted.cortana_identity().managed_launch.is_none());
        let recovered_terminal = fresh["terminalId"].as_str().unwrap();
        dispatch(
            &restarted_ctx,
            "close_terminal",
            &json!({"sessionId": recovered_terminal}),
        )
        .unwrap();
        assert_eq!(tmux::session_liveness(&target), tmux::SessionLiveness::Gone);
        std::fs::remove_dir_all(harness_bin_dir).ok();
        std::fs::remove_dir_all(home).ok();
        std::fs::remove_file(registry_path).ok();
        std::fs::remove_file(identity_path).ok();
    }
}

#[test]
fn generation_zero_preowner_cortana_is_preserved_without_authority_or_signal() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let audit_dir = std::env::temp_dir().join(format!(
        "t-hub-cortana-generation-audit-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let ctx = test_ctx("cortana-generation")
        .with_apply_sink(sink)
        .with_audit(Arc::new(AuditLog::new(audit_dir.clone())));
    ctx.tab_registry().replace(vec![TabRecord {
        id: CAPTAIN_WORKSPACE_ID.into(),
        name: CAPTAIN_WORKSPACE_NAME.into(),
        tile_ids: vec![],
    }]);
    let home = std::env::temp_dir().join(format!(
        "t-hub-cortana-generation-home-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&home).unwrap();
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    let identity = ctx.identity.mint(crate::identity::Role::Cortana).unwrap();
    let terminal_id = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    ctx.identity.bind_tile(&identity.id, &terminal_id).unwrap();
    let target = exact_cortana_tmux_target(&terminal_id).unwrap();
    create_test_tmux_session_with_env(
        &target,
        home.to_str().unwrap(),
        Some(&harness_command),
        &[
            ("T_HUB_CONTROL_FILE".into(), discovery_file_for_spawn()),
            ("T_HUB_CONTROL_ADDR".into(), String::new()),
            ("T_HUB_CONTROL_TOKEN".into(), String::new()),
            (
                crate::identity::SESSION_TOKEN_ENV.into(),
                identity.secret.clone(),
            ),
            (CORTANA_GENERATION_ENV.into(), "0".into()),
        ],
    )
    .unwrap();
    wait_for_harness_started(&terminal_id, "codex").unwrap();

    let error = dispatch(
        &ctx,
        "reconcile_cortana",
        &json!({
            "operationId": "cortana-generation-1",
            "testOrchestratorHome": home,
        }),
    )
    .unwrap_err();
    assert!(error.contains("predates managed ownership"), "{error}");
    assert_eq!(
        tmux::session_environment(&target, CORTANA_GENERATION_ENV).unwrap(),
        Some("0".into())
    );
    assert_eq!(
        tmux::session_liveness(&target),
        tmux::SessionLiveness::Alive
    );
    assert!(!ctx
        .captains
        .snapshot()
        .captains
        .iter()
        .any(|captain| captain.role == FleetRole::Cortana));
    reap_test_tmux_session_and_assert_absent(&target);
    let _ = std::fs::remove_dir_all(harness_bin_dir);
    let _ = std::fs::remove_dir_all(home);
    let _ = std::fs::remove_dir_all(audit_dir);
}

#[test]
fn generation_zero_update_hook_is_never_used_for_preowner_runtime() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let audit_dir = std::env::temp_dir().join(format!(
        "t-hub-cortana-generation-failure-audit-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let ctx = test_ctx("cortana-generation-failure")
        .with_apply_sink(sink)
        .with_audit(Arc::new(AuditLog::new(audit_dir.clone())));
    ctx.tab_registry().replace(vec![TabRecord {
        id: CAPTAIN_WORKSPACE_ID.into(),
        name: CAPTAIN_WORKSPACE_NAME.into(),
        tile_ids: vec![],
    }]);
    let home = std::env::temp_dir().join(format!(
        "t-hub-cortana-generation-failure-home-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&home).unwrap();
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    let identity = ctx.identity.mint(crate::identity::Role::Cortana).unwrap();
    let terminal_id = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    ctx.identity.bind_tile(&identity.id, &terminal_id).unwrap();
    let target = exact_cortana_tmux_target(&terminal_id).unwrap();
    create_test_tmux_session_with_env(
        &target,
        home.to_str().unwrap(),
        Some(&harness_command),
        &[
            ("T_HUB_CONTROL_FILE".into(), discovery_file_for_spawn()),
            ("T_HUB_CONTROL_ADDR".into(), String::new()),
            ("T_HUB_CONTROL_TOKEN".into(), String::new()),
            (
                crate::identity::SESSION_TOKEN_ENV.into(),
                identity.secret.clone(),
            ),
            (CORTANA_GENERATION_ENV.into(), "0".into()),
        ],
    )
    .unwrap();
    wait_for_harness_started(&terminal_id, "codex").unwrap();
    tmux::fail_next_session_environment_set_for(&target);

    let error = dispatch(
        &ctx,
        "reconcile_cortana",
        &json!({
            "operationId": "cortana-generation-failure-1",
            "testOrchestratorHome": home,
        }),
    )
    .unwrap_err();
    assert!(error.contains("predates managed ownership"), "{error}");
    assert_eq!(
        tmux::session_environment(&target, CORTANA_GENERATION_ENV).unwrap(),
        Some("0".into())
    );
    let durable = ctx.captains.cortana_identity();
    assert!(durable.identity_id.is_none());
    assert_eq!(durable.generation, 0);
    assert!(!ctx
        .captains
        .snapshot()
        .captains
        .iter()
        .any(|captain| captain.role == FleetRole::Cortana));
    reap_test_tmux_session_and_assert_absent(&target);
    let _ = std::fs::remove_dir_all(harness_bin_dir);
    let _ = std::fs::remove_dir_all(home);
    let _ = std::fs::remove_dir_all(audit_dir);
}
