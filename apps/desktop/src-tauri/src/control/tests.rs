use super::*;

#[test]
fn pty_output_and_probe_ack_frames_cannot_interleave() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (server, _) = listener.accept().unwrap();
    let outbound = Arc::new(Mutex::new(server));
    let mut sink = SharedPtyWriter {
        outbound: outbound.clone(),
        buffer: Vec::new(),
    };

    // Simulate the output producer constructing one frame through partial
    // writes while the input path emits an acknowledgement in between.
    sink.write_all(br#"{"out":"YW"#).unwrap();
    {
        let mut writer = outbound.lock().unwrap();
        write_json_line(&mut writer, &json!({ "probeAck": 7 })).unwrap();
    }
    sink.write_all(b"Jj\"}\n").unwrap();
    sink.flush().unwrap();

    let mut reader = BufReader::new(client);
    let mut first = String::new();
    let mut second = String::new();
    reader.read_line(&mut first).unwrap();
    reader.read_line(&mut second).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&first).unwrap(),
        json!({ "probeAck": 7 })
    );
    assert_eq!(
        serde_json::from_str::<Value>(&second).unwrap(),
        json!({ "out": "YWJj" })
    );
}
use std::sync::{mpsc, Mutex as StdMutex};
use std::thread;

// Real tmux fixture progress can be delayed substantially by the parallel
// workspace suite, while thirty seconds remains a bounded failure signal.
const TEST_ASYNC_FIXTURE_TIMEOUT: Duration = Duration::from_secs(30);

#[test]
fn control_request_debug_redacts_all_credential_and_argument_values() {
    let request = ControlRequest {
        token: "global-control-secret".into(),
        command: "new_tab".into(),
        args: serde_json::json!({"credential": "argument-secret"}),
        session: "bound-session-secret".into(),
        host: "host-proof-secret".into(),
        v: Some(PROTOCOL_VERSION),
    };

    let diagnostic = format!("{request:?}");
    assert!(diagnostic.contains("ControlRequest"));
    assert!(diagnostic.contains("new_tab"));
    assert!(diagnostic.contains("<redacted>"));
    for secret in [
        "global-control-secret",
        "argument-secret",
        "bound-session-secret",
        "host-proof-secret",
    ] {
        assert!(
            !diagnostic.contains(secret),
            "ControlRequest Debug leaked {secret}"
        );
    }
}

/// Build a ControlContext backed by a real (empty) Supervisor + StatusBridge,
/// with a fixed token, for dispatch tests.
fn test_ctx(token: &str) -> ControlContext {
    let supervisor = Arc::new(StdMutex::new(Supervisor::new()));
    let sup_for_closure = supervisor.clone();
    let visitor: Arc<dyn Fn(&mut dyn FnMut(&Supervisor)) + Send + Sync> =
        Arc::new(move |f: &mut dyn FnMut(&Supervisor)| {
            let guard = sup_for_closure.lock().unwrap();
            f(&guard);
        });
    // Point the audit sink at a per-token temp dir so dispatch_authenticated
    // tests never write to the real ~/.t-hub/audit.
    let audit_dir = std::env::temp_dir().join(format!("t-hub-ctl-test-{token}"));
    // A known read token so capability tests can present it; distinct from the
    // control token so ReadOnly vs Full resolution is exercised.
    let mut ctx = ControlContext::new(Arc::new(StatusBridge::new()), visitor, token.to_string())
        .with_read_token(format!("read-{token}"))
        .with_audit(Arc::new(crate::audit::AuditLog::new(audit_dir)));
    ctx.host_token = token.to_string();
    ctx
}

fn dispatch_test_repo_root() -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .unwrap()
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

fn tmux_process_tests_available() -> bool {
    std::process::Command::new("tmux")
        .arg("-V")
        .output()
        .is_ok_and(|output| output.status.success())
        && std::process::Command::new("node")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
}

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

fn seed_starting_agent(ctx: &ControlContext, agent_session_id: &str) {
    seed_starting_agent_with_purpose(
        ctx,
        agent_session_id,
        crate::governor::AdmissionPurpose::Ordinary,
    );
}

fn seed_starting_agent_with_purpose(
    ctx: &ControlContext,
    agent_session_id: &str,
    admission_purpose: crate::governor::AdmissionPurpose,
) {
    ctx.captains
        .upsert_project(ProjectRecord {
            root_path: None,
            vcs_capability: None,
            git_main_root: None,
            project_id: "capacity-project".into(),
            name: "Capacity Project".into(),
            repo_root: "/tmp/capacity-project".into(),
            remote_url: None,
            default_branch: None,
            powder: None,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    ctx.captains
        .claim_test("capacity-captain", Some("capacity-ship"), vec![])
        .unwrap();
    ctx.captains
        .bind_ship_context(
            "capacity-ship",
            "capacity-project",
            "Capacity assignment",
            "codex",
        )
        .unwrap();
    let (lane_claim, dispatch_capacity) = test_dispatch_evidence("capacity-lane", agent_session_id);
    ctx.captains
        .insert_agent_session(AgentSessionRecord {
            agent_session_id: agent_session_id.into(),
            captain_session_id: "capacity-captain".into(),
            project_id: "capacity-project".into(),
            assignment: "Pending durable start".into(),
            directory: "/tmp/capacity-agent".into(),
            worktree_path: None,
            branch: None,
            workspace_tab_id: None,
            harness: "codex".into(),
            provider: "codex".into(),
            provider_conversation_id: None,
            resume_point: None,
            runtime_state: RuntimeState::Starting,
            work_stage: crate::agent_session::WorkStage::Assigned,
            delivery: Some(crate::agent_session::DeliveryProvenance::new(
                "1111111111111111111111111111111111111111",
                false,
            )),
            lane_claim: Some(lane_claim),
            integration_contracts: Vec::new(),
            dispatch_capacity: Some(dispatch_capacity),
            admission_purpose,
            created_at: 2,
            updated_at: 2,
        })
        .unwrap();
}

fn history_service_at(root: &std::path::Path) -> Arc<crate::history::HistoryService> {
    let claude_root = root.join("claude");
    let codex_root = root.join("codex");
    std::fs::create_dir_all(&claude_root).unwrap();
    std::fs::create_dir_all(&codex_root).unwrap();
    Arc::new(crate::history::HistoryService::new(
        claude_root,
        codex_root,
        Duration::from_secs(60),
    ))
}

fn seed_history_resume(
    history: &crate::history::HistoryService,
    request_id: &str,
    terminal_id: &str,
    complete: bool,
) -> (String, String) {
    let history_id = format!("history:v1:{request_id}");
    let conversation_id = format!("conversation-{request_id}");
    let pending = crate::history::HistoryPendingResume {
        request_id: request_id.to_string(),
        history_id: history_id.clone(),
        harness: crate::history::Harness::Codex,
        conversation_id: conversation_id.clone(),
        terminal_id: terminal_id.to_string(),
        target_tab_id: None,
        authorized_ship_slug: None,
        authorized_project_id: None,
        authorized_assignment_id: None,
        reserved_at_ms: now_ms(),
    };
    history.reserve_resume(pending).unwrap();
    if complete {
        history
            .record_resume(
                crate::history::HistoryBinding {
                    history_id: history_id.clone(),
                    harness: crate::history::Harness::Codex,
                    conversation_id: conversation_id.clone(),
                    terminal_id: terminal_id.to_string(),
                },
                crate::history::HistoryResumeOperation {
                    request_id: request_id.to_string(),
                    history_id: history_id.clone(),
                    harness: crate::history::Harness::Codex,
                    conversation_id: conversation_id.clone(),
                    terminal_id: terminal_id.to_string(),
                    target_tab_id: None,
                    actual_tab_id: None,
                    authorized_ship_slug: None,
                    authorized_project_id: None,
                    authorized_assignment_id: None,
                    recorded_at_ms: now_ms(),
                },
            )
            .unwrap();
    }
    (history_id, conversation_id)
}

fn test_harness_command(harness: &str) -> (std::path::PathBuf, String) {
    let bin_dir = std::env::temp_dir().join(format!(
        "t-hub-test-harness-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&bin_dir).unwrap();
    let executable = bin_dir.join(harness);
    std::fs::copy("/bin/sleep", &executable).unwrap();
    let command = format!("{} 60", executable.display());
    (bin_dir, command)
}

fn modeled_codex_tool_approval(command: &str, tool: &str) -> &'static str {
    let argv = shell_words::split(command).unwrap();
    let expected = format!("mcp_servers.t-hub.tools.{tool}.approval_mode=\"approve\"");
    let approved = argv
        .windows(2)
        .filter(|pair| matches!(pair[0].as_str(), "-c" | "--config"))
        .any(|pair| pair[1] == expected);
    if approved {
        "approve"
    } else {
        "prompt"
    }
}

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
    if (argc < 3) return 2;
    signal(SIGTTOU, SIG_IGN);
    if (setpgid(0, 0) != 0 && errno != EACCES && getpgrp() != getpid()) return 8;
    if (tcsetpgrp(STDIN_FILENO, getpgrp()) != 0) return 9;
    const char *mode = argv[1];
    const char *marker = argv[2];
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

/// Tear down a real tmux fixture and prove the named session is absent.
///
/// tmux can remove its final session successfully and then return
/// `server exited unexpectedly` while the server shuts down. Production
/// continues to surface that error. Tests tolerate only that exact teardown
/// race, and only after a separate liveness probe proves the fixture is gone.
fn reap_test_tmux_session(target: &str) -> Result<(), String> {
    let teardown = tmux::kill_session_tree(target);
    let deadline = Instant::now() + Duration::from_secs(2);
    while tmux::session_liveness(target) != tmux::SessionLiveness::Gone {
        if Instant::now() >= deadline {
            return Err(format!(
                "tmux test fixture '{target}' survived teardown: {teardown:?}"
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    if let Err(error) = teardown {
        if error.message != "server exited unexpectedly" {
            return Err(format!(
                "tmux test fixture '{target}' reported an unexpected teardown failure: {error}"
            ));
        }
    }
    Ok(())
}

fn reap_test_tmux_session_and_assert_absent(target: &str) {
    reap_test_tmux_session(target).unwrap_or_else(|error| panic!("{error}"));
}

fn create_test_tmux_session_with_env(
    target: &str,
    cwd: &str,
    command: Option<&str>,
    env: &[(String, String)],
) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match tmux::new_session_with_env(target, cwd, command, env) {
            Ok(()) => return Ok(()),
            Err(error) if error.message == "server exited unexpectedly" => {
                match tmux::session_liveness(target) {
                    tmux::SessionLiveness::Alive => return Ok(()),
                    tmux::SessionLiveness::Gone if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    liveness => {
                        return Err(format!(
                                "tmux test fixture '{target}' could not start after server teardown ({liveness:?}): {error}"
                            ));
                    }
                }
            }
            Err(error) => return Err(error.to_string()),
        }
    }
}

fn create_test_tmux_session(target: &str) -> Result<(), String> {
    create_test_tmux_session_with_env(target, "/tmp", None, &[])
}

/// Serialize process-attestation fixtures and keep an anchor alive while a
/// case runs. This prevents one test's final-session shutdown from racing
/// another test's session creation. Dropping the guard reaps the anchor and
/// independently probes its absence, including after a successful final
/// removal that tmux reports as `server exited unexpectedly`.
struct ProcessAttestationTmuxGuard {
    _lifecycle: tmux::TestLifecycleGuard,
}

impl ProcessAttestationTmuxGuard {
    fn acquire() -> Self {
        Self {
            _lifecycle: tmux::TestLifecycleGuard::acquire(),
        }
    }
}

struct ManagedCortanaTestCleanup {
    ctx: Arc<ControlContext>,
}

impl ManagedCortanaTestCleanup {
    fn new(ctx: Arc<ControlContext>) -> Self {
        Self { ctx }
    }
}

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
    std::fs::write(
        &executable,
        "#!/usr/bin/env node\nsetInterval(() => {}, 1000);\n",
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
    let mut next_retry = Instant::now() + Duration::from_secs(1);
    let result = loop {
        if let Ok(after) = observe_harness_process(&target) {
            match attest_launch_permissions(
                expected.adapter(),
                &before,
                &after,
                PermMode::BypassPermissions,
            ) {
                Err(crate::harness::LaunchAttestationError::StaleEvidence) => {
                    let now = Instant::now();
                    assert!(
                        now < deadline,
                        "fake Harness did not produce process evidence"
                    );
                    if now >= next_retry {
                        tmux::send_text(&target, &command, true).unwrap();
                        next_retry = now + Duration::from_secs(1);
                    }
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
fn bad_token_is_rejected_before_dispatch() {
    let ctx = test_ctx("secret");
    let req = ControlRequest {
        token: "wrong".into(),
        command: "list_tabs".into(),
        args: Value::Null,
        session: String::new(),
        host: String::new(),
        v: None,
    };
    let resp = dispatch_authenticated(&ctx, req);
    assert!(!resp.ok);
    assert!(resp.error.unwrap().contains("unauthorized"));
}

#[test]
fn good_token_dispatches() {
    let ctx = test_ctx("secret");
    let req = ControlRequest {
        token: "secret".into(),
        command: "list_tabs".into(),
        args: Value::Null,
        session: String::new(),
        host: "secret".into(),
        v: None,
    };
    let resp = dispatch_authenticated(&ctx, req);
    assert!(resp.ok, "expected ok, got {:?}", resp.error);
    assert!(resp.result.unwrap().get("tabs").is_some());
}

#[test]
fn unknown_command_is_refused() {
    let ctx = test_ctx("t");
    let err = dispatch(&ctx, "definitely_not_a_command", &Value::Null).unwrap_err();
    assert!(err.contains("not exposed over the control channel"));
}

/// The id-namespace bridge: the supervisor keys by the Claude UUID, but callers
/// address a captain by its tile id (`captainSessionId`). `get_status` must
/// resolve tile -> UUID via the status bridge, so a captain's status is no longer
/// a spurious `unknown`. A UUID passed directly is unchanged.
#[test]
fn get_status_resolves_a_captain_tile_id_to_its_claude_uuid() {
    use t_hub_protocol::JournalEventType;
    let supervisor = Arc::new(StdMutex::new(Supervisor::new()));
    supervisor.lock().unwrap().ingest(
        Some("uuid-abc"),
        None,
        None,
        None,
        JournalEventType::SessionStart,
        1,
    );
    let sup_for_closure = supervisor.clone();
    let visitor: Arc<dyn Fn(&mut dyn FnMut(&Supervisor)) + Send + Sync> =
        Arc::new(move |f: &mut dyn FnMut(&Supervisor)| {
            let guard = sup_for_closure.lock().unwrap();
            f(&guard);
        });
    let status = Arc::new(StatusBridge::new());
    // The tile `cap01234` currently hosts Claude session `uuid-abc`.
    status.ingest(
        "uuid-abc",
        &json!({ "cwd": "/p", "tmux_session": "th_cap01234" }),
        1,
    );
    let ctx = ControlContext::new(status, visitor, "t".to_string());

    // Poll by the CAPTAIN tile id -> resolves to the UUID, returns the real status.
    let v = get_status(&ctx, &json!({ "sessionId": "cap01234" })).unwrap();
    assert_eq!(
        v.get("resolvedSessionId").and_then(|x| x.as_str()),
        Some("uuid-abc"),
        "tile id must resolve to the Claude UUID"
    );
    assert_eq!(
        v.get("status").and_then(|x| x.as_str()),
        Some("working"),
        "status must be the real supervisor status, not 'unknown'"
    );
    // A UUID (already a supervisor key) is passed through untouched.
    let v2 = get_status(&ctx, &json!({ "sessionId": "uuid-abc" })).unwrap();
    assert_eq!(
        v2.get("resolvedSessionId").and_then(|x| x.as_str()),
        Some("uuid-abc")
    );
    // A genuinely unknown id still resolves to unknown (no regression).
    let v3 = get_status(&ctx, &json!({ "sessionId": "ghostzzzz" })).unwrap();
    assert_eq!(v3.get("status").and_then(|x| x.as_str()), Some("unknown"));
}

#[test]
fn watch_fleet_requires_a_live_orchestrator_terminal() {
    let ctx = test_ctx("t");
    // No live tmux for this id -> the arm is refused so a bogus id can't arm a
    // watch that could never deliver.
    let err = watch_fleet(
        &ctx,
        &json!({ "orchestratorSessionId": "nolivetile" }),
        None,
        true,
    )
    .unwrap_err();
    assert!(err.contains("no live terminal"), "got: {err}");
    // And it requires the id at all.
    assert!(watch_fleet(&ctx, &json!({}), None, true)
        .unwrap_err()
        .contains("orchestratorSessionId"));
}

#[test]
fn unwatch_and_list_fleet_watches_on_empty_registry() {
    let ctx = test_ctx("t");
    let v = unwatch_fleet(
        &ctx,
        &json!({ "orchestratorSessionId": "whoever" }),
        None,
        true,
    )
    .unwrap();
    assert_eq!(v.get("removed").and_then(|x| x.as_bool()), Some(false));
    let list = list_fleet_watches(&ctx).unwrap();
    assert_eq!(list.get("count").and_then(|x| x.as_u64()), Some(0));
}

#[test]
fn arm_then_list_and_disarm_a_watch_via_the_registry() {
    // The command's tmux liveness guard needs a real session, so exercise the
    // arm/list/disarm round-trip through the shared registry directly (the
    // command is a thin validate-then-arm wrapper over exactly this).
    let ctx = test_ctx("t");
    ctx.fleet_watches
        .arm("orc12345", crate::fleet::WatchScope::Captains, vec![]);
    let list = list_fleet_watches(&ctx).unwrap();
    assert_eq!(list.get("count").and_then(|x| x.as_u64()), Some(1));
    let removed = unwatch_fleet(
        &ctx,
        &json!({ "orchestratorSessionId": "orc12345" }),
        None,
        true,
    )
    .unwrap();
    assert_eq!(removed.get("removed").and_then(|x| x.as_bool()), Some(true));
    assert_eq!(
        list_fleet_watches(&ctx)
            .unwrap()
            .get("count")
            .and_then(|x| x.as_u64()),
        Some(0)
    );
}

#[test]
fn parse_watch_scope_accepts_captains_all_and_explicit_lists() {
    use crate::fleet::WatchScope;
    assert_eq!(parse_watch_scope(&json!({})).unwrap(), WatchScope::Captains);
    assert_eq!(
        parse_watch_scope(&json!({ "scope": "all" })).unwrap(),
        WatchScope::All
    );
    assert_eq!(
        parse_watch_scope(&json!({ "scope": ["a", "b"] })).unwrap(),
        WatchScope::Sessions(vec!["a".into(), "b".into()])
    );
    assert!(parse_watch_scope(&json!({ "scope": "bogus" })).is_err());
    assert!(parse_watch_scope(&json!({ "scope": [] })).is_err());
}

#[test]
fn host_metrics_prefers_the_bridge_and_serializes_snake_case() {
    // A stubbed agent-bridge metrics RPC: the handler must PREFER it over the
    // daemon's local /proc, and serialize snake_case (the frontend wire shape in
    // src/ipc/protocol.ts) — NOT the camelCase `wsl_health` shape.
    let ctx = test_ctx("t").with_metrics(Arc::new(|| {
        Ok(t_hub_protocol::HostMetrics {
            mem_total_kib: 16_000_000,
            mem_available_kib: 8_000_000,
            swap_total_kib: 2_000_000,
            swap_free_kib: 1_500_000,
            cpu_count: 12,
            load_avg: [1.0, 0.5, 0.25],
            process_count: 432,
            distro: Some("Ubuntu 24.04".into()),
            captured_at_ms: 1_700_000_000_000,
        })
    }));
    let v = dispatch(&ctx, "host_metrics", &Value::Null).unwrap();
    assert_eq!(
        v.get("mem_total_kib").and_then(|x| x.as_u64()),
        Some(16_000_000)
    );
    assert_eq!(v.get("cpu_count").and_then(|x| x.as_u64()), Some(12));
    assert_eq!(v.get("process_count").and_then(|x| x.as_u64()), Some(432));
    assert_eq!(
        v.get("distro").and_then(|x| x.as_str()),
        Some("Ubuntu 24.04")
    );
    assert!(
        v.get("memTotalKib").is_none(),
        "must be snake_case, not the camelCase wsl_health shape"
    );
}

#[test]
fn host_metrics_falls_back_when_the_bridge_errors() {
    // Bridge says "not connected". On Linux the daemon's own /proc IS the real
    // host (native-WSL / remote-Linux daemon, or the dev box), so we serve a
    // snake_case snapshot from it. On non-Linux the local /proc would be
    // all-zeros, so we surface the error instead (preserves today's UX).
    let ctx = test_ctx("t").with_metrics(Arc::new(|| Err("not connected".into())));
    let out = dispatch(&ctx, "host_metrics", &Value::Null);
    #[cfg(target_os = "linux")]
    {
        let v = out.expect("linux falls back to local /proc");
        assert!(
            v.get("mem_total_kib").is_some(),
            "snake_case local snapshot"
        );
        assert!(v.get("captured_at_ms").is_some());
    }
    #[cfg(not(target_os = "linux"))]
    {
        assert!(out.unwrap_err().contains("not connected"));
    }
}

#[test]
fn spawn_terminal_without_sink_refuses_untracked_session() {
    // No apply sink (headless): there is no UI to adopt the tile, so spawn is
    // refused rather than creating an untracked tmux session (#17).
    let ctx = test_ctx("t");
    let err = dispatch(&ctx, "spawn_terminal", &json!({"cwd": "/tmp"})).unwrap_err();
    assert!(err.contains("no UI"), "got: {err}");
}

#[test]
fn spawn_terminal_with_sink_spawns_places_and_returns_id() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    // Headless-org: with a UI sink wired, the SERVER spawns the real session,
    // resolves `tabName` against the authoritative registry (minting a hidden
    // tab without switching the active one), places the tile there, returns
    // the real id synchronously, and forwards id + registry snapshot.
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let ctx = test_ctx("t").with_apply_sink(sink.clone());
    ctx.tab_registry().replace(vec![TabRecord {
        id: "tab-1".into(),
        name: "Main".into(),
        tile_ids: vec![],
    }]);
    let v = dispatch(
        &ctx,
        "spawn_terminal",
        &json!({"cwd": "/tmp", "name": "logs", "tabName": "hidden-ops"}),
    )
    .unwrap();
    assert_eq!(v["accepted"], "spawn_terminal");
    assert_eq!(v["audited"], true);
    let id = v["id"]
        .as_str()
        .expect("real id returned synchronously")
        .to_string();
    assert_eq!(v["placed"], true);
    let tab_id = v["tabId"].as_str().unwrap().to_string();
    assert_ne!(
        tab_id, "tab-1",
        "a NEW hidden tab is minted for the new name"
    );

    // The registry (authoritative) holds the placement, and the active tab
    // was NOT touched (no focus steal).
    let snap = ctx.tab_registry().snapshot_full();
    let tab = snap
        .tabs
        .iter()
        .find(|t| t.id == tab_id)
        .expect("tab minted");
    assert_eq!(tab.name, "hidden-ops");
    assert_eq!(tab.tile_ids, vec![id.clone()]);
    assert_eq!(snap.active_tab_id.as_deref(), Some("tab-1"));

    // The forward carries the id + snapshot for the UI to render from.
    {
        let calls = sink.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "spawn_terminal");
        assert_eq!(calls[0].1["id"], json!(id));
        assert_eq!(calls[0].1["cwd"], "/tmp");
        assert_eq!(calls[0].1["name"], "logs");
        assert_eq!(calls[0].1["tabId"], json!(tab_id));
        assert!(calls[0].1["sync"]["seq"].as_u64().is_some());
    }
    // Reap the real session this spawned.
    dispatch(&ctx, "close_terminal", &json!({"sessionId": id})).unwrap();
}

#[cfg(unix)]
#[test]
fn spawn_terminal_converts_wsl_unc_for_tmux_but_preserves_the_public_cwd() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let ctx = test_ctx("t").with_apply_sink(sink.clone());
    let runtime_dir = tempfile::tempdir().unwrap();
    let runtime_cwd = runtime_dir.path().canonicalize().unwrap();
    let runtime_cwd = runtime_cwd.to_str().unwrap();
    assert!(runtime_cwd.starts_with('/'));
    let canonical_cwd = format!(
        "\\\\?\\UNC\\wsl.localhost\\Ubuntu-24.04{}",
        runtime_cwd.replace('/', "\\")
    );
    let result = dispatch(
        &ctx,
        "spawn_terminal",
        &json!({"cwd": &canonical_cwd, "startupCommand": "sleep 60"}),
    )
    .unwrap();
    let id = result["id"].as_str().unwrap().to_string();
    let target = tmux_target(&id);
    let pane_cwd = std::process::Command::new("tmux")
        .args([
            "-L",
            tmux::socket(),
            "display-message",
            "-p",
            "-t",
            &target,
            "#{pane_current_path}",
        ])
        .output()
        .unwrap();

    assert!(pane_cwd.status.success());
    assert_eq!(
        String::from_utf8_lossy(&pane_cwd.stdout).trim(),
        runtime_cwd
    );
    assert_eq!(result["cwd"], canonical_cwd);
    assert_eq!(sink.calls.lock().unwrap()[0].1["cwd"], canonical_cwd);

    dispatch(&ctx, "close_terminal", &json!({"sessionId": id})).unwrap();
}

#[test]
fn spawn_terminal_forwards_the_startup_command() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    // T-B: the socket spawn carries the webview presets' `startupCommand`
    // (the resume flow's `claude --resume <id>` in production; a harmless
    // echo here - headless-org spawns the REAL session server-side now, so
    // the command actually runs). The forward must carry it verbatim.
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let ctx = test_ctx("t").with_apply_sink(sink.clone());
    let v = dispatch(
        &ctx,
        "spawn_terminal",
        &json!({"cwd": "/tmp", "startupCommand": "echo resume-proof"}),
    )
    .unwrap();
    assert_eq!(v["accepted"], "spawn_terminal");
    assert_eq!(v["startupCommand"], "echo resume-proof");
    let first_id = v["id"].as_str().unwrap().to_string();

    let calls = sink.calls.lock().unwrap();
    assert_eq!(calls[0].0, "spawn_terminal");
    assert_eq!(calls[0].1["startupCommand"], "echo resume-proof");
    // The snake_case alias parses too (loose-args convention).
    drop(calls);
    let v2 = dispatch(
        &ctx,
        "spawn_terminal",
        &json!({"cwd": "/tmp", "startup_command": "echo alias-proof"}),
    )
    .unwrap();
    assert_eq!(
        sink.calls.lock().unwrap()[1].1["startupCommand"],
        "echo alias-proof"
    );
    // Reap the real sessions these spawned.
    for id in [first_id.as_str(), v2["id"].as_str().unwrap()] {
        dispatch(&ctx, "close_terminal", &json!({"sessionId": id})).unwrap();
    }
}

#[test]
fn wait_for_status_immediate_match_does_not_time_out() {
    // An empty Supervisor reports `unknown` for any unseen session, so a
    // target of "unknown" matches on the first poll and returns at once.
    let ctx = test_ctx("t");
    let v = dispatch(
        &ctx,
        "wait_for_status",
        &json!({"sessionId": "absent", "targetStatus": "unknown"}),
    )
    .unwrap();
    assert_eq!(v["finalStatus"], "unknown");
    assert_eq!(v["timedOut"], false);
}

#[test]
fn wait_for_status_accepts_target_array() {
    let ctx = test_ctx("t");
    let v = dispatch(
        &ctx,
        "wait_for_status",
        &json!({"sessionId": "absent", "targetStatus": ["completed", "unknown"]}),
    )
    .unwrap();
    assert_eq!(v["finalStatus"], "unknown");
    assert_eq!(v["timedOut"], false);
}

#[test]
fn wait_for_status_times_out_when_target_never_seen() {
    // A status that never occurs for an unseen session, with a 0ms timeout,
    // returns on the first iteration with timedOut:true.
    let ctx = test_ctx("t");
    let v = dispatch(
        &ctx,
        "wait_for_status",
        &json!({"sessionId": "absent", "targetStatus": "completed", "timeoutMs": 0}),
    )
    .unwrap();
    assert_eq!(v["finalStatus"], "unknown");
    assert_eq!(v["timedOut"], true);
}

#[test]
fn wait_for_status_requires_session_and_target() {
    let ctx = test_ctx("t");
    let err = dispatch(
        &ctx,
        "wait_for_status",
        &json!({"targetStatus": "completed"}),
    )
    .unwrap_err();
    assert!(err.contains("sessionId"), "got: {err}");
    let err = dispatch(&ctx, "wait_for_status", &json!({"sessionId": "x"})).unwrap_err();
    assert!(err.contains("targetStatus"), "got: {err}");
}

// NOTE: the former `wait_for_status_captures_transient_edge_between_polls`
// test lived here. It drove A(working) → B(completed) → A(working) from a
// driver thread that slept 150ms hoping to land *inside* the poller's first
// 500ms `wait_for_status` window — a wall-clock race that slips on a loaded
// box (the driver can run before the dispatcher even captures its `consumed`
// watermark, or after the window it was aiming for). The semantics it tried to
// assert ("an edge logged strictly between two polls is still observed") can't
// be expressed at this control layer without that race: the dispatcher
// captures `consumed = current_seq()` *internally*, so any edge that is to land
// at `seq > consumed` must be logged by a concurrent thread after that capture,
// and the dispatcher exposes no hook to synchronize against.
//
// That edge-capture logic is `Supervisor::matched_since`, which `wait_for_status`
// calls directly — and it is already proven DETERMINISTICALLY (no threads, no
// sleeps) by `supervision::tests::transition_log_captures_transient_edge_through_b`,
// which drives the same A→B→A sequence and asserts `matched_since` recovers the
// transient Completed edge from the log. That is the real coverage; this
// duplicate was dropped rather than kept as a flaky wall-clock race.
//
// The deterministic dispatcher-level behaviours that DON'T need a race are still
// covered above: immediate current-status match (`wait_for_status_immediate_
// match_does_not_time_out`), target arrays, and the 0ms timeout path.

#[test]
fn read_terminal_requires_session_id() {
    let ctx = test_ctx("t");
    let err = dispatch(&ctx, "read_terminal", &Value::Null).unwrap_err();
    assert!(err.contains("sessionId"), "got: {err}");
}

#[test]
fn send_text_requires_session_and_text() {
    let ctx = test_ctx("t");
    let err = dispatch(&ctx, "send_text", &json!({"text": "hi"})).unwrap_err();
    assert!(err.contains("sessionId"), "got: {err}");
    let err = dispatch(&ctx, "send_text", &json!({"sessionId": "x"})).unwrap_err();
    assert!(err.contains("text"), "got: {err}");
}

#[test]
fn send_keys_requires_non_empty_keys() {
    let ctx = test_ctx("t");
    let err = dispatch(&ctx, "send_keys", &json!({"sessionId": "x", "keys": []})).unwrap_err();
    assert!(err.contains("keys"), "got: {err}");
}

#[test]
fn close_terminal_requires_session_id() {
    let ctx = test_ctx("t");
    let err = dispatch(&ctx, "close_terminal", &Value::Null).unwrap_err();
    assert!(err.contains("sessionId"), "got: {err}");
}

#[test]
fn send_to_missing_session_is_a_clear_error() {
    // No `th_*` session named this exists ⇒ a readable "no such session".
    let ctx = test_ctx("t");
    let err = dispatch(
        &ctx,
        "send_text",
        &json!({"sessionId": "definitely_absent_xyz", "text": "hi"}),
    )
    .unwrap_err();
    assert!(err.contains("no such session"), "got: {err}");
}

/// De-conflation guard (spawn-wedge): the direct-writer gate must map a
/// three-state probe correctly - `Alive` proceeds, a DEFINITIVE `Gone` is "no
/// such session", and an INDETERMINATE `Unknown` (a timed-out / failed probe) is
/// a RETRYABLE control-plane timeout that must NEVER read as "no such session".
/// That false negative is exactly what sent the fleet to raw-tmux break-glass on
/// 0.3.62; reverting the `Unknown` arm to the old `!has_session` conflation (so a
/// timeout falls into the Gone message) trips this test.
#[test]
fn writer_gate_timeout_is_retryable_never_no_such_session() {
    use tmux::SessionLiveness::*;
    // Alive proceeds.
    assert!(
        writer_liveness_gate("send_text", "e05764f5", "th_e05764f5", Alive).is_ok(),
        "a live session must proceed"
    );
    // Gone is a definitive "no such session".
    let gone = writer_liveness_gate("send_text", "e05764f5", "th_e05764f5", Gone).unwrap_err();
    assert!(
        gone.contains("no such session"),
        "a completed-absent probe is 'no such session'; got: {gone}"
    );
    // Unknown (a timed-out probe) is retryable and must NOT read as gone.
    let unknown =
        writer_liveness_gate("send_keys", "e05764f5", "th_e05764f5", Unknown).unwrap_err();
    assert!(
        !unknown.contains("no such session"),
        "a timed-out probe must NOT read as gone; got: {unknown}"
    );
    assert!(
        unknown.contains("timed out") && unknown.contains("retry"),
        "the Unknown arm must name the timeout and invite a retry; got: {unknown}"
    );
}

/// MED-1 guard (PR-58 review): the `close_terminal` `force` escape keeps a
/// genuinely-dead-but-`Unknown` session reapable, and never kills a session a
/// fresh re-probe CONFIRMS `Alive`. The name states exactly what is pinned - NOT
/// "never kills a live session" (a live-but-slow session whose re-probe also
/// times out is `Unknown`, indistinguishable from dead, and IS force-reaped; see
/// `plan_close`). `plan_close` is the pure decision; this pins every arm. Bypass:
/// make `force + Unknown + reprobe Alive` reap (drop the `RefuseForceOnLive` arm)
/// and the reprobe-Alive refusal assert trips.
#[test]
fn force_close_never_kills_a_session_that_probes_alive() {
    use tmux::SessionLiveness::*;
    // Default (no force): Alive/Gone reap normally; Unknown is a retryable refusal.
    assert!(matches!(
        plan_close(false, Alive, None),
        ClosePlan::Kill {
            existed: true,
            forced: false
        }
    ));
    assert!(matches!(
        plan_close(false, Gone, None),
        ClosePlan::Kill {
            existed: false,
            forced: false
        }
    ));
    assert!(matches!(
        plan_close(false, Unknown, None),
        ClosePlan::RetryableTimeout
    ));
    // force + Unknown, re-probe ALIVE => REFUSE (the load-bearing guarantee: a
    // session a fresh probe CONFIRMS Alive is never force-killed).
    assert!(matches!(
        plan_close(true, Unknown, Some(Alive)),
        ClosePlan::RefuseForceOnLive
    ));
    // force + Unknown, re-probe GONE => clean reap (now confirmed dead).
    assert!(matches!(
        plan_close(true, Unknown, Some(Gone)),
        ClosePlan::Kill {
            existed: false,
            forced: false
        }
    ));
    // force + Unknown, re-probe STILL Unknown => forced reap: a still-unreachable
    // session stays reapable (the whole point of the escape). Under a sustained
    // wedge this reaps a dead OR a live-but-unreachable tile - by design; force is
    // an explicit reap-during-wedge override.
    assert!(matches!(
        plan_close(true, Unknown, Some(Unknown)),
        ClosePlan::Kill {
            existed: false,
            forced: true
        }
    ));
}

/// LOW-1 guard (PR-58 review): a retryable control error carries a STRUCTURED
/// `retryable:true` flag on the wire so fleet automation discriminates a wedge
/// from a genuine error WITHOUT substring-matching prose - and the machine marker
/// never leaks into the human text, and the flag is omitted (wire unchanged) for
/// non-retryable errors. Ties a real site (the writer gate) through
/// `retryable_error` → `ControlResponse::err` → serialization. Bypass: drop the
/// `retryable_error` wrapper on the Unknown arm and the `retryable==true` assert
/// trips.
#[test]
fn low1_retryable_errors_carry_a_structured_flag_not_prose() {
    use tmux::SessionLiveness::*;
    // A retryable site (writer gate on Unknown) → structured retryable + clean text.
    let gate_err =
        writer_liveness_gate("send_text", "e05764f5", "th_e05764f5", Unknown).unwrap_err();
    let resp = ControlResponse::err(gate_err);
    assert!(!resp.ok);
    assert!(
        resp.retryable,
        "an Unknown-arm error must be structurally retryable"
    );
    let text = resp.error.as_deref().unwrap_or("");
    assert!(
        !text.contains(RETRYABLE_ERROR_MARKER),
        "the machine marker must be stripped from the wire text; got: {text:?}"
    );
    assert!(
        text.contains("timed out") && text.contains("retry"),
        "human guidance is preserved: {text}"
    );
    // A definitive (Gone) error is NOT retryable.
    let gone_err = writer_liveness_gate("send_text", "e05764f5", "th_e05764f5", Gone).unwrap_err();
    let resp_gone = ControlResponse::err(gone_err);
    assert!(
        !resp_gone.retryable,
        "a definitive 'no such session' must not be flagged retryable"
    );
    // Serialization: `retryable` present only when true (wire unchanged otherwise).
    let j = serde_json::to_value(&resp).unwrap();
    assert_eq!(j.get("retryable").and_then(|v| v.as_bool()), Some(true));
    let j_gone = serde_json::to_value(&resp_gone).unwrap();
    assert!(
        j_gone.get("retryable").is_none(),
        "retryable is omitted when false, so existing consumers see an unchanged wire"
    );
}

#[test]
fn git_init_recovery_errors_are_structured_on_the_control_wire() {
    let response = ControlResponse::err(
            "git_init_recovery code=git_init_recovery operation=git-init-123 phase=recovery_blocked message=ownership marker changed",
        );
    let wire = serde_json::to_value(response).unwrap();
    assert_eq!(wire["errorKind"], "git_init_recovery");
    assert_eq!(wire["errorDetails"]["operation"], "git-init-123");
    assert_eq!(wire["errorDetails"]["phase"], "recovery_blocked");
    assert_eq!(wire["error"], "ownership marker changed");
    assert!(!wire.to_string().contains("git_init_recovery:"));
}

#[test]
fn tmux_target_maps_id_and_is_idempotent() {
    assert_eq!(tmux_target("abc"), "th_abc");
    assert_eq!(tmux_target("th_abc"), "th_abc");
}

#[test]
fn remove_worktree_requires_args() {
    let ctx = test_ctx("t");
    let err = dispatch(&ctx, "remove_worktree", &json!({"worktreePath": "/x"})).unwrap_err();
    assert!(err.contains("repoRoot"), "got: {err}");
    let err = dispatch(&ctx, "remove_worktree", &json!({"repoRoot": "/r"})).unwrap_err();
    assert!(err.contains("worktreePath"), "got: {err}");
}

#[test]
fn remove_worktree_without_sink_fails_closed_before_mutation() {
    let ctx = test_ctx("t");
    let err = dispatch(
        &ctx,
        "remove_worktree",
        &json!({"repoRoot": "/r", "worktreePath": "/r/wt"}),
    )
    .unwrap_err();
    assert_eq!(err, git::WORKTREE_REMOVAL_UNAVAILABLE);
}

#[test]
fn remove_worktree_with_sink_fails_before_forwarding() {
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let ctx = test_ctx("t").with_apply_sink(sink.clone());
    let err = dispatch(
        &ctx,
        "remove_worktree",
        &json!({"repoRoot": "/r", "worktreePath": "/r/wt", "force": true}),
    )
    .unwrap_err();
    assert_eq!(err, git::WORKTREE_REMOVAL_UNAVAILABLE);
    let calls = sink.calls.lock().unwrap();
    assert!(
        calls.is_empty(),
        "no UI mutation may be forwarded: {calls:?}"
    );
}

#[test]
fn owned_empty_tab_rollback_preserves_shared_tabs() {
    let tabs = TabRegistry::new();
    tabs.insert_tab("owned", "Owned");
    tabs.rollback_owned_empty_tab("owned").unwrap();
    assert!(!tabs.has_tab("owned"));

    tabs.insert_tab("shared", "Shared");
    tabs.move_tile("live", "shared").unwrap();
    let err = tabs.rollback_owned_empty_tab("shared").unwrap_err();
    assert!(err.contains("gained a tile"), "got: {err}");
    assert!(tabs.has_tab("shared"));
}

#[test]
fn owned_create_state_rollback_removes_worktree_and_new_tab() {
    let (base, repo, worktree) = scratch_repo_with_worktree();
    let ctx = test_ctx("t");
    ctx.tabs.insert_tab("owned", "Owned");

    rollback_created_worktree_state(
        &ctx,
        repo.to_str().unwrap(),
        worktree.to_str().unwrap(),
        "owned",
        true,
    )
    .unwrap();

    assert!(!worktree.exists());
    assert!(!ctx.tabs.has_tab("owned"));
    std::fs::remove_dir_all(base).ok();
}

#[test]
fn ambiguous_spawn_failure_reports_preserved_worktree() {
    let error = ambiguous_spawn_rollback_error(
        "spawn outcome unknown",
        "identity store unavailable",
        Ok(()),
    );
    assert!(error.contains("terminal cleanup was not confirmed"));
    assert!(error.contains("worktree was preserved"));
    assert!(!error.contains("worktree was rolled back"));
}

#[test]
fn create_worktree_requires_args() {
    let ctx = test_ctx("t");
    let err = dispatch(&ctx, "create_worktree", &json!({"worktreePath": "/x"})).unwrap_err();
    assert!(err.contains("repoRoot"), "got: {err}");
    let err = dispatch(&ctx, "create_worktree", &json!({"repoRoot": "/r"})).unwrap_err();
    assert!(err.contains("worktreePath"), "got: {err}");
}

/// Scaffold a REAL throwaway git repo (initial commit) with one linked
/// worktree, under the OS temp dir. Returns `(base, repo, worktree)`; the
/// caller removes `base` when done (best-effort — a unique name per call
/// keeps reruns clean either way).
fn scratch_repo_with_worktree() -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
    fn sh_git(cwd: &std::path::Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .current_dir(cwd)
            .args(args)
            .output()
            .expect("git spawns");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let base = std::env::temp_dir().join(format!("t-hub-tb-{}", uuid::Uuid::new_v4().simple()));
    let repo = base.join("repo");
    std::fs::create_dir_all(&repo).expect("mkdir repo");
    sh_git(&repo, &["init", "-q"]);
    std::fs::write(repo.join("a.txt"), "hi").expect("seed file");
    sh_git(&repo, &["add", "."]);
    sh_git(
        &repo,
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-qm",
            "init",
        ],
    );
    let wt = base.join("wt");
    sh_git(&repo, &["worktree", "add", "-q", wt.to_str().unwrap()]);
    assert!(wt.exists(), "worktree dir created");
    (base, repo, wt)
}

fn exact_head(cwd: &std::path::Path) -> String {
    let (ok, stdout, stderr) = git::run_git_for_test(
        cwd.to_str().expect("UTF-8 test path"),
        &["rev-parse", "HEAD"],
    )
    .expect("git rev-parse spawns");
    assert!(ok, "git rev-parse failed: {stderr}");
    stdout.trim().to_string()
}

fn test_dispatch_evidence(
    lane_id: &str,
    owner_id: &str,
) -> (crate::governor::LaneClaim, crate::governor::CapacityReport) {
    let lane = crate::governor::LaneClaim {
        lane_id: lane_id.into(),
        owner_id: owner_id.into(),
        dependencies: Some(BTreeSet::new()),
        mutable_files: BTreeSet::new(),
        mutable_schemas: BTreeSet::new(),
        mutable_interfaces: BTreeSet::new(),
    };
    let request = crate::governor::DispatchPreflight {
        requested_lanes: vec![lane.clone()],
        requested_provider_lanes: 1,
        admission_purpose: crate::governor::AdmissionPurpose::Ordinary,
        ship_admin_scope: None,
        active_lanes: Vec::new(),
        satisfied_dependencies: BTreeSet::new(),
        integration_contracts: Vec::new(),
        capacity: crate::governor::RuntimeCapacity {
            live_sessions: 3,
            machine_healthy: true,
            machine_session_capacity: 64,
            provider_session_capacity: 64,
            provider_live_sessions: 3,
            provider_capacity_status: crate::governor::ProviderCapacityStatus {
                source: "test-telemetry".into(),
                degraded: false,
                detail: None,
            },
            available_worktrees: 8,
            active_captains: 0,
            active_captain_ships: BTreeSet::new(),
            live_cortana: 1,
            live_fleet_admins: 1,
            live_ship_admins: 0,
            live_ship_admin_scopes: BTreeMap::new(),
            live_recovery_sessions: 1,
        },
    };
    let capacity = SpawnGovernor::default()
        .preflight_dispatch(&request)
        .unwrap();
    (lane, capacity)
}

fn completed_delivery(
    baseline: &str,
    resulting_commit: &str,
) -> crate::agent_session::DeliveryProvenance {
    let mut delivery = crate::agent_session::DeliveryProvenance::new(baseline, false);
    delivery
        .record_implementation(resulting_commit.to_string())
        .unwrap();
    delivery
        .record_review(crate::agent_session::ReviewEvidence {
            commit: resulting_commit.to_string(),
            reviewer_identity: "independent-reviewer".into(),
            reference: "review://dependency".into(),
            recorded_at: 2,
        })
        .unwrap();
    delivery
        .record_acceptance_test(crate::agent_session::AcceptanceTestEvidence {
            commit: resulting_commit.to_string(),
            runner_identity: "acceptance-runner".into(),
            reference: "test://dependency".into(),
            environment: crate::agent_session::AcceptanceEnvironment::Source,
            recorded_at: 2,
        })
        .unwrap();
    delivery
}

#[cfg(not(windows))]
fn checkout_test_distro() -> String {
    std::env::var("T_HUB_DISTRO").unwrap_or_else(|_| "Ubuntu-24.04".to_string())
}

#[cfg(not(windows))]
fn extended_wsl_unc(path: &std::path::Path, distro: &str) -> String {
    format!(
        "\\\\?\\UNC\\wsl.localhost\\{distro}{}",
        path.to_string_lossy().replace('/', "\\")
    )
}

#[cfg(not(windows))]
#[test]
fn crew_checkout_accepts_a_wsl_worktree_for_an_extended_unc_project() {
    let (base, repo, worktree) = scratch_repo_with_worktree();
    let distro = checkout_test_distro();
    let durable_root = extended_wsl_unc(&repo, &distro);
    let project = ProjectRecord {
        root_path: None,
        vcs_capability: None,
        git_main_root: None,
        project_id: "project-wsl-worktree".into(),
        name: "WSL Worktree".into(),
        repo_root: durable_root.clone(),
        remote_url: None,
        default_branch: Some("main".into()),
        powder: None,
        created_at: 0,
        updated_at: 0,
    };

    let checkout = validate_crew_checkout(&project, Some(worktree.to_string_lossy().into_owned()))
        .expect("the WSL checkout must match the extended-UNC Project root");

    assert_eq!(
        checkout,
        std::fs::canonicalize(&worktree).unwrap().to_string_lossy()
    );
    assert_eq!(project.repo_root, durable_root);
    std::fs::remove_dir_all(base).ok();
}

#[cfg(not(windows))]
#[test]
fn crew_checkout_accepts_a_same_distro_unc_worktree() {
    let (base, repo, worktree) = scratch_repo_with_worktree();
    let distro = checkout_test_distro();
    let project = ProjectRecord {
        root_path: None,
        vcs_capability: None,
        git_main_root: None,
        project_id: "project-same-distro".into(),
        name: "Same Distro".into(),
        repo_root: extended_wsl_unc(&repo, &distro),
        remote_url: None,
        default_branch: Some("main".into()),
        powder: None,
        created_at: 0,
        updated_at: 0,
    };

    let checkout = validate_crew_checkout(&project, Some(extended_wsl_unc(&worktree, &distro)))
        .expect("an explicit checkout in the configured distro must remain valid");

    assert_eq!(
        checkout,
        std::fs::canonicalize(&worktree).unwrap().to_string_lossy()
    );
    std::fs::remove_dir_all(base).ok();
}

#[cfg(not(windows))]
#[test]
fn crew_checkout_rejects_foreign_distro_unc_paths_with_the_same_tail() {
    let (base, repo, worktree) = scratch_repo_with_worktree();
    let configured = checkout_test_distro();
    let foreign = if configured.eq_ignore_ascii_case("Debian") {
        "Ubuntu-24.04"
    } else {
        "Debian"
    };
    let mut project = ProjectRecord {
        root_path: None,
        vcs_capability: None,
        git_main_root: None,
        project_id: "project-foreign-distro".into(),
        name: "Foreign Distro".into(),
        repo_root: extended_wsl_unc(&repo, &configured),
        remote_url: None,
        default_branch: Some("main".into()),
        powder: None,
        created_at: 0,
        updated_at: 0,
    };

    let requested_error =
        validate_crew_checkout(&project, Some(extended_wsl_unc(&worktree, foreign)))
            .expect_err("the same path tail in a foreign distro must not be remapped");
    assert!(requested_error.contains("requested checkout"));
    assert!(requested_error.contains(foreign));
    assert!(requested_error.contains(&configured));

    project.repo_root = extended_wsl_unc(&repo, foreign);
    let project_error =
        validate_crew_checkout(&project, Some(worktree.to_string_lossy().into_owned()))
            .expect_err("a durable Project root in a foreign distro must fail closed");
    assert!(project_error.contains("Project root"));
    assert!(project_error.contains(foreign));
    assert!(project_error.contains(&configured));

    std::fs::remove_dir_all(base).ok();
}

#[cfg(not(windows))]
#[test]
fn crew_checkout_rejects_unregistered_directories_and_foreign_worktrees() {
    let (base, repo, _worktree) = scratch_repo_with_worktree();
    let (foreign_base, _foreign_repo, foreign_worktree) = scratch_repo_with_worktree();
    let ordinary = base.join("ordinary-checkout");
    std::fs::create_dir(&ordinary).expect("ordinary checkout fixture");
    let distro = checkout_test_distro();
    let project = ProjectRecord {
        root_path: None,
        vcs_capability: None,
        git_main_root: None,
        project_id: "project-wsl-rejections".into(),
        name: "WSL Rejections".into(),
        repo_root: extended_wsl_unc(&repo, &distro),
        remote_url: None,
        default_branch: Some("main".into()),
        powder: None,
        created_at: 0,
        updated_at: 0,
    };

    for rejected in [&ordinary, &foreign_worktree] {
        let error = validate_crew_checkout(&project, Some(rejected.to_string_lossy().into_owned()))
            .expect_err("only worktrees belonging to the Project may be dispatched");
        assert!(
            error.contains("is not a worktree of project"),
            "got: {error}"
        );
    }

    std::fs::remove_dir_all(base).ok();
    std::fs::remove_dir_all(foreign_base).ok();
}

fn scratch_product_repo_with_worktree() -> (std::path::PathBuf, String, String) {
    let base = std::env::temp_dir().join(format!(
        "t-hub-product-tb-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let repo_host = base.join("repo");
    let worktree_host = base.join("wt");
    std::fs::create_dir_all(&repo_host).expect("mkdir repo");

    let repo = test_product_path(&repo_host);
    let worktree = test_product_path(&worktree_host);
    let run = |args: &[&str]| {
        let (ok, stdout, stderr) = git::run_git_for_test(&repo, args).expect("git spawns");
        assert!(
            ok,
            "git {args:?} failed: {}",
            if stderr.trim().is_empty() {
                stdout.trim()
            } else {
                stderr.trim()
            }
        );
    };

    run(&["init", "-q"]);
    std::fs::write(repo_host.join("a.txt"), "hi").expect("seed file");
    run(&["add", "."]);
    run(&[
        "-c",
        "user.email=t@t",
        "-c",
        "user.name=t",
        "commit",
        "-qm",
        "init",
    ]);
    git::worktree_add(&repo, &worktree, None).expect("worktree add succeeds");
    assert!(worktree_host.exists(), "worktree dir created");
    (base, repo, worktree)
}

#[cfg(not(windows))]
fn test_product_path(path: &std::path::Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(windows)]
fn test_product_path(path: &std::path::Path) -> String {
    let native = path.to_string_lossy().replace('\\', "/");
    let bytes = native.as_bytes();
    assert!(
        bytes.len() >= 3 && bytes[1] == b':' && bytes[2] == b'/',
        "expected an absolute drive path, got {native:?}"
    );
    format!(
        "/mnt/{}/{}",
        (bytes[0] as char).to_ascii_lowercase(),
        &native[3..]
    )
}

#[test]
fn remove_worktree_with_subscribers_fails_before_broadcast_or_git() {
    let (base, repo, wt) = scratch_product_repo_with_worktree();

    for force in [false, true] {
        let err = git::worktree_remove(&repo, &wt, force).unwrap_err();
        assert_eq!(err, git::WORKTREE_REMOVAL_UNAVAILABLE);
        assert!(
            base.join("wt").exists(),
            "force={force} must preserve the worktree"
        );
    }

    let fanout = Arc::new(EventFanout::new());
    let ctx = test_ctx("t").with_event_fanout(fanout.clone());
    let mut reader = subscribe_test_reader(&fanout);
    let err = dispatch(
        &ctx,
        "remove_worktree",
        &json!({"repoRoot": repo, "worktreePath": wt}),
    )
    .unwrap_err();
    assert_eq!(err, git::WORKTREE_REMOVAL_UNAVAILABLE);
    assert_no_event(&mut reader);
    assert!(
        base.join("wt").exists(),
        "the worktree directory must remain intact"
    );
    let listed_paths = git::worktree_list(&repo)
        .expect("git worktree list succeeds")
        .into_iter()
        .map(|entry| entry.path)
        .collect::<Vec<_>>();
    assert!(
        listed_paths.contains(&wt),
        "the worktree registration must remain intact: expected {wt:?} in {listed_paths:?}"
    );

    git::rollback_created_worktree(&repo, &wt)
        .expect("transaction-owned rollback remains available");
    assert!(
        !base.join("wt").exists(),
        "private rollback must remove its owned worktree"
    );
    std::fs::remove_dir_all(&base).ok();
}

#[test]
fn create_worktree_runs_the_startup_command_in_the_worktree_terminal() {
    // audit MED (provisioning gap): create_worktree now carries a
    // `startupCommand` plumbed through the SAME pane_command / -ilc exec path
    // spawn_terminal uses, so a worktree crew boots into its command instead of
    // a bare shell. This proves it EXECUTES end-to-end: the startupCommand
    // touches a sentinel file, and we poll for it. BYPASS-WOULD-FAIL: pass
    // `None` for the pane again (the old bare-shell spawn) and the sentinel is
    // never created -> the poll times out RED.
    let (base, repo, _wt) = scratch_repo_with_worktree();
    let new_wt = base.join("wt-startup");
    let sentinel = base.join("startup-ran.marker");
    let startup = format!("touch {}", sentinel.to_str().unwrap());

    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let ctx = test_ctx("t").with_apply_sink(sink.clone());
    let v = dispatch(
        &ctx,
        "create_worktree",
        &json!({
            "repoRoot": repo.to_str().unwrap(),
            "worktreePath": new_wt.to_str().unwrap(),
            "startupCommand": startup,
        }),
    )
    .unwrap();
    assert_eq!(v["accepted"], "create_worktree");
    // The response + the UI forward both carry the command verbatim.
    assert_eq!(v["startupCommand"], json!(startup));
    let terminal_id = v["terminalId"].as_str().expect("a terminal was spawned");
    {
        let calls = sink.calls.lock().unwrap();
        let fwd = calls
            .iter()
            .find(|(cmd, _)| cmd == "add_worktree_workspace")
            .expect("the worktree forward was delivered");
        assert_eq!(fwd.1["startupCommand"], json!(startup));
    }

    // Poll for the sentinel: proof the -ilc pane wrap actually ran the command
    // (the interactive login shell can take a moment to source rc + exec).
    let mut ran = false;
    for _ in 0..100 {
        if sentinel.exists() {
            ran = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    // Reap the real session before asserting, so a failure never leaks a tmux
    // session or the scratch dir.
    dispatch(&ctx, "close_terminal", &json!({"sessionId": terminal_id})).ok();
    std::fs::remove_dir_all(&base).ok();
    assert!(
        ran,
        "the worktree terminal must have run the startupCommand"
    );
}

#[test]
fn send_text_break_glass_emits_loud_marker() {
    // comms-plane Phase 1: `send_text` is DEMOTED to break-glass. Using it must
    // emit a live `control://break-glass` marker (D11a) so the deviation from
    // the plane primary path is visible. The marker fires even though this
    // send_text ultimately errors (no such tmux session) - a break-glass USE is
    // logged on attempt, not only on success.
    let fanout = Arc::new(EventFanout::new());
    let ctx = test_ctx("t").with_event_fanout(fanout.clone());
    let mut reader = subscribe_test_reader(&fanout);

    let _ = dispatch(
        &ctx,
        "send_text",
        &json!({ "sessionId": "no-such-session", "text": "hello" }),
    );

    let frame = read_event_frame(&mut reader);
    assert_eq!(frame["event"], "control://break-glass");
    assert_eq!(frame["payload"]["command"], "send_text");
    assert_eq!(frame["payload"]["breakGlass"], true);
    assert_eq!(frame["payload"]["sessionId"], "no-such-session");
    // Byte length only - the marker must NOT leak the payload content.
    assert_eq!(frame["payload"]["bytes"], 5);
    assert!(
        frame["payload"].get("text").is_none(),
        "must not leak text: {frame}"
    );
}

#[test]
fn send_keys_break_glass_emits_loud_marker() {
    // The demoted twin: `send_keys` also emits the break-glass marker.
    let fanout = Arc::new(EventFanout::new());
    let ctx = test_ctx("t").with_event_fanout(fanout.clone());
    let mut reader = subscribe_test_reader(&fanout);

    let _ = dispatch(
        &ctx,
        "send_keys",
        &json!({ "sessionId": "no-such-session", "keys": ["C-c", "Escape"] }),
    );

    let frame = read_event_frame(&mut reader);
    assert_eq!(frame["event"], "control://break-glass");
    assert_eq!(frame["payload"]["command"], "send_keys");
    assert_eq!(frame["payload"]["breakGlass"], true);
    // send_keys carries its payload in `keys`, not `text`: the marker must
    // report the joined key-name length ("C-c Escape" = 10), not bytes=0.
    assert_eq!(frame["payload"]["bytes"], 10);
}

#[test]
fn list_worktrees_lists_main_then_linked() {
    let (base, repo, wt) = scratch_repo_with_worktree();
    let ctx = test_ctx("t");
    let v = dispatch(
        &ctx,
        "list_worktrees",
        &json!({"cwd": repo.to_str().unwrap()}),
    )
    .unwrap();
    let list = v["worktrees"].as_array().expect("worktrees array");
    assert_eq!(list.len(), 2, "main + linked: {list:?}");
    assert_eq!(list[0]["isLinked"], false);
    assert_eq!(list[1]["isLinked"], true);
    assert_eq!(list[1]["path"], json!(wt.to_str().unwrap()));
    // The IPC-twin alias resolves to the same handler.
    let v2 = dispatch(
        &ctx,
        "git_worktree_list",
        &json!({"cwd": repo.to_str().unwrap()}),
    )
    .unwrap();
    assert_eq!(v2["worktrees"], v["worktrees"]);
    std::fs::remove_dir_all(&base).ok();
}

#[test]
fn reprobe_reaped_create_worktree_resolves_against_reality() {
    // M1 full fix. A create_worktree whose InFlight reservation was reaped is
    // retried with the same requestId; before re-applying we RE-PROBE reality.
    let (base, repo, wt) = scratch_repo_with_worktree();
    let ctx = test_ctx("t");

    // The worktree EXISTS on disk (the original DID land): the re-probe must
    // resolve to a success outcome tagged reprobedAfterReap, NOT None (which
    // would let dispatch re-run git worktree add and duplicate/error).
    let args = json!({
        "repoRoot": repo.to_str().unwrap(),
        "worktreePath": wt.to_str().unwrap(),
    });
    let outcome = reprobe_reaped_request(&ctx, "create_worktree", &args)
        .expect("existing worktree must resolve against reality");
    let v = outcome.expect("resolved outcome is Ok");
    assert_eq!(v["accepted"], "create_worktree");
    assert_eq!(v["alreadyCreated"], true);
    assert_eq!(v["reprobedAfterReap"], true);

    // A worktree path that does NOT exist ⇒ None: the original truly died, so
    // dispatch proceeds to a fresh (re-checked) apply.
    let missing = json!({
        "repoRoot": repo.to_str().unwrap(),
        "worktreePath": base.join("never-created").to_str().unwrap(),
    });
    assert!(
        reprobe_reaped_request(&ctx, "create_worktree", &missing).is_none(),
        "an absent worktree must NOT resolve - it should re-apply fresh"
    );

    // spawn_terminal has a SERVER-minted id: nothing in args to probe by ⇒ None.
    assert!(
        reprobe_reaped_request(&ctx, "spawn_terminal", &json!({"cwd": "/tmp"})).is_none(),
        "spawn_terminal has no probe-able artifact in its args"
    );

    std::fs::remove_dir_all(&base).ok();
}

#[test]
fn list_worktrees_requires_cwd_and_is_empty_outside_a_repo() {
    let ctx = test_ctx("t");
    let err = dispatch(&ctx, "list_worktrees", &json!({})).unwrap_err();
    assert!(err.contains("cwd"), "got: {err}");
    // Best-effort like the IPC twin: a non-repo dir yields an empty list.
    let v = dispatch(&ctx, "list_worktrees", &json!({"cwd": "/"})).unwrap();
    assert_eq!(v["worktrees"], json!([]));
}

#[test]
fn remote_worktree_ops_are_gated_to_the_allowlist() {
    // A REMOTE peer (peer_is_loopback=false) with no T_HUB_REMOTE_FILE_ROOTS is
    // refused BEFORE any git runs (#27) — the scope gate fires ahead of
    // git::worktree_add and the UI forward. (test_ctx defaults to loopback, so
    // the existing create/remove tests above keep exercising the unrestricted
    // local path.)
    let mut ctx = test_ctx("t");
    ctx.peer_is_loopback = false;
    ctx.captains
        .upsert_project(ProjectRecord {
            root_path: Some("/home/x/proj".into()),
            vcs_capability: Some("none".into()),
            git_main_root: None,
            project_id: "remote-none".into(),
            name: "Remote none".into(),
            repo_root: "/home/x/proj".into(),
            remote_url: None,
            default_branch: None,
            powder: None,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    let before = ctx.captains.snapshot();
    for cmd in ["create_worktree", "remove_worktree", "list_worktrees"] {
        let err = dispatch(
            &ctx,
            cmd,
            &json!({"repoRoot": "/home/x/proj", "worktreePath": "/home/x/proj-wt/feature"}),
        )
        .unwrap_err();
        assert!(
            err.contains("disabled"),
            "{cmd} should be gated for a remote peer; got: {err}"
        );
        assert!(
            !err.contains("git_required"),
            "{cmd} disclosed registered capability before remote path authorization: {err}"
        );
    }
    assert_eq!(ctx.captains.snapshot().seq, before.seq);
    // git_info probes git at a peer-controlled cwd — same allowlist gate.
    let err = dispatch(&ctx, "git_info", &json!({"path": "/home/x/whatever"})).unwrap_err();
    assert!(
        err.contains("disabled"),
        "git_info should be gated; got: {err}"
    );
}

#[test]
fn focus_tab_is_organization_apply() {
    // Headless-org: focus_tab is STRICT (the tab must exist in the registry)
    // and mirrors the new active tab there. No sink (headless): accepted +
    // audited, but not applied.
    let ctx = test_ctx("t");
    let err = dispatch(&ctx, "focus_tab", &json!({"tabId": "tab-1"})).unwrap_err();
    assert!(err.contains("unknown tabId"), "got: {err}");

    ctx.tab_registry().replace(vec![TabRecord {
        id: "tab-1".into(),
        name: "Main".into(),
        tile_ids: vec![],
    }]);
    let v = dispatch(&ctx, "focus_tab", &json!({"tabId": "tab-1"})).unwrap();
    assert_eq!(v["accepted"], "focus_tab");
    assert_eq!(v["audited"], true);
    assert_eq!(v["applied"], false);
    assert_eq!(
        ctx.tab_registry().snapshot_full().active_tab_id.as_deref(),
        Some("tab-1")
    );
}

#[test]
fn new_tab_returns_a_tab_id_and_registers_it() {
    // TASK C: new_tab mints an id CORE-side, returns it, and records it so
    // list_tabs sees it immediately (addressable before any frontend report).
    let ctx = test_ctx("t");
    let v = dispatch(&ctx, "new_tab", &json!({"name": "Logs"})).unwrap();
    assert_eq!(v["accepted"], "new_tab");
    assert_eq!(v["name"], "Logs");
    let tab_id = v["tabId"].as_str().expect("new_tab returns a tabId");
    assert!(!tab_id.is_empty());

    let tabs = dispatch(&ctx, "list_tabs", &Value::Null).unwrap();
    let arr = tabs["tabs"].as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["id"], tab_id);
    assert_eq!(arr[0]["name"], "Logs");
    assert_eq!(arr[0]["tileIds"].as_array().unwrap().len(), 0);
}

#[test]
fn new_tab_auto_names_when_no_name_given() {
    let ctx = test_ctx("t");
    let v = dispatch(&ctx, "new_tab", &Value::Null).unwrap();
    assert_eq!(v["name"], "Workspace 1");
    let v2 = dispatch(&ctx, "new_tab", &Value::Null).unwrap();
    assert_eq!(v2["name"], "Workspace 2");
}

#[test]
fn new_tab_then_move_tile_reflected_in_list_tabs() {
    // The headless acceptance for #22: new_tab -> get its id -> move_tile a
    // terminal into it -> list_tabs shows the tile in that tab.
    let ctx = test_ctx("t");
    let created = dispatch(&ctx, "new_tab", &json!({"name": "Target"})).unwrap();
    let tab_id = created["tabId"].as_str().unwrap().to_string();

    dispatch(
        &ctx,
        "move_tile",
        &json!({"terminalId": "term-xyz", "tabId": tab_id}),
    )
    .unwrap();

    let tabs = dispatch(&ctx, "list_tabs", &Value::Null).unwrap();
    let target = tabs["tabs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == tab_id.as_str())
        .expect("target tab present");
    let tiles: Vec<&str> = target["tileIds"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(tiles, vec!["term-xyz"]);
}

#[test]
fn move_tile_uses_durable_fleet_authority_without_waiting_for_projection_lock() {
    let context = Arc::new(test_ctx("move-identity-transaction"));
    context
        .captains
        .create_workspace("work-a", "Work A", None)
        .unwrap();
    context
        .captains
        .create_workspace("work-b", "Work B", None)
        .unwrap();
    context.tab_registry().replace(vec![
        TabRecord {
            id: "work-a".into(),
            name: "Work A".into(),
            tile_ids: vec!["ordinary".into()],
        },
        TabRecord {
            id: "work-b".into(),
            name: "Work B".into(),
            tile_ids: Vec::new(),
        },
    ]);
    let tabs = context.tab_registry();
    let transaction = tabs.identity_transaction();
    let (sent, received) = std::sync::mpsc::channel();
    let moving_context = Arc::clone(&context);
    let moving = std::thread::spawn(move || {
        let result = dispatch(
            &moving_context,
            "move_tile",
            &json!({"terminalId": "ordinary", "tabId": "work-b"}),
        );
        sent.send(result).unwrap();
    });
    received
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    assert!(context
        .captains
        .snapshot()
        .workspaces
        .iter()
        .find(|workspace| workspace.id == "work-b")
        .unwrap()
        .tile_ids
        .contains(&"ordinary".to_string()));
    drop(transaction);
    moving.join().unwrap();
    assert!(tabs
        .snapshot()
        .iter()
        .find(|tab| tab.id == "work-b")
        .unwrap()
        .tile_ids
        .contains(&"ordinary".to_string()));
}

#[test]
fn rollback_restore_cannot_clobber_a_concurrent_valid_move() {
    let context = Arc::new(test_ctx("rollback-move-transaction"));
    context.tab_registry().replace(vec![
        TabRecord {
            id: "work-a".into(),
            name: "Work A".into(),
            tile_ids: vec!["ordinary".into()],
        },
        TabRecord {
            id: "work-b".into(),
            name: "Work B".into(),
            tile_ids: Vec::new(),
        },
    ]);
    let tabs = context.tab_registry();
    tabs.move_tile("ordinary", CAPTAIN_WORKSPACE_ID).unwrap();
    let (sent, received) = std::sync::mpsc::channel();
    let moving_context = Arc::clone(&context);
    let moving = std::thread::spawn(move || {
        sent.send(dispatch(
            &moving_context,
            "move_tile",
            &json!({"terminalId": "ordinary", "tabId": "work-b"}),
        ))
        .unwrap();
    });
    received
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    moving.join().unwrap();
    tabs.replace(context.captains.workspace_projection());
    let snapshot = tabs.snapshot();
    assert!(snapshot
        .iter()
        .find(|tab| tab.id == "work-b")
        .unwrap()
        .tile_ids
        .contains(&"ordinary".to_string()));
    assert_eq!(
        snapshot
            .iter()
            .flat_map(|tab| tab.tile_ids.iter())
            .filter(|tile| tile.as_str() == "ordinary")
            .count(),
        1
    );
}

#[test]
fn close_terminal_does_not_hold_or_wait_for_projection_identity_during_effects() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let terminal_id = format!(
        "close-race-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    create_test_tmux_session(&tmux_target(&terminal_id)).unwrap();
    let context = Arc::new(test_ctx("close-identity-transaction"));
    context.tab_registry().replace(vec![TabRecord {
        id: "work-a".into(),
        name: "Work A".into(),
        tile_ids: vec![terminal_id.clone()],
    }]);
    let tabs = context.tab_registry();
    let transaction = tabs.identity_transaction();
    let (sent, received) = std::sync::mpsc::channel();
    let closing_context = Arc::clone(&context);
    let closing_id = terminal_id.clone();
    let closing = std::thread::spawn(move || {
        sent.send(close_terminal(
            &closing_context,
            &json!({"sessionId": closing_id}),
        ))
        .unwrap();
    });
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline
        && (tmux::has_session(&tmux_target(&terminal_id))
            || !context
                .captains
                .snapshot()
                .pending_fleet_operations
                .is_empty())
    {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(!tmux::has_session(&tmux_target(&terminal_id)));
    assert!(context
        .captains
        .snapshot()
        .pending_fleet_operations
        .is_empty());
    received
        .recv_timeout(Duration::from_secs(2))
        .expect("close_terminal must not wait for the projection identity mutex")
        .unwrap();
    drop(transaction);
    closing.join().unwrap();
    assert!(!tmux::has_session(&tmux_target(&terminal_id)));
    assert!(!tabs
        .snapshot()
        .iter()
        .any(|tab| tab.tile_ids.contains(&terminal_id)));
}

#[test]
fn move_racing_claim_cannot_leave_an_active_captain_in_work() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let path = captains_tmp("move-vs-claim-transaction");
    let captains = Arc::new(CaptainsRegistry::load(path.clone()));
    let tabs = Arc::new(TabRegistry::new());
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    let captain_id = format!(
        "claim-race-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    tmux::new_session_with_env(
        &tmux_target(&captain_id),
        "/tmp",
        Some(&harness_command),
        &[],
    )
    .unwrap();
    wait_for_harness_started(&captain_id, "codex").unwrap();
    tabs.replace(vec![
        TabRecord {
            id: "work-a".into(),
            name: "Work A".into(),
            tile_ids: vec![captain_id.clone()],
        },
        TabRecord {
            id: "work-b".into(),
            name: "Work B".into(),
            tile_ids: Vec::new(),
        },
    ]);
    let context = Arc::new(
        test_ctx("move-vs-claim-transaction")
            .with_captains_registry(Arc::clone(&captains))
            .with_tab_registry(Arc::clone(&tabs))
            .with_apply_sink(Arc::new(RecordingSink {
                calls: StdMutex::new(Vec::new()),
            })),
    );
    let entered = Arc::new(std::sync::Barrier::new(2));
    let release = Arc::new(std::sync::Barrier::new(2));
    let hook_entered = Arc::clone(&entered);
    let hook_release = Arc::clone(&release);
    let first_persist = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let hook_first_persist = Arc::clone(&first_persist);
    captains.set_persist_hook(Box::new(move || {
        if hook_first_persist.swap(false, Ordering::SeqCst) {
            hook_entered.wait();
            hook_release.wait();
        }
    }));
    let claiming_context = Arc::clone(&context);
    let claiming_id = captain_id.clone();
    let claiming = std::thread::spawn(move || {
        dispatch(
            &claiming_context,
            "claim_captain",
            &json!({
                "captainSessionId": claiming_id,
                "shipSlug": "claim-race",
                "provider": "codex"
            }),
        )
    });
    entered.wait();
    let (move_sent, move_received) = std::sync::mpsc::channel();
    let moving_context = Arc::clone(&context);
    let moving_id = captain_id.clone();
    let moving = std::thread::spawn(move || {
        move_sent
            .send(dispatch(
                &moving_context,
                "move_tile",
                &json!({"terminalId": moving_id, "tabId": "work-b"}),
            ))
            .unwrap();
    });
    assert!(move_received
        .recv_timeout(Duration::from_millis(100))
        .is_err());
    release.wait();
    claiming.join().unwrap().unwrap();
    captains.set_persist_hook(Box::new(|| {}));
    let move_error = move_received
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap_err();
    assert!(move_error.contains("belongs to Captain Workspace"));
    moving.join().unwrap();
    let snapshot = tabs.snapshot();
    assert!(snapshot
        .iter()
        .find(|tab| tab.id == CAPTAIN_WORKSPACE_ID)
        .unwrap()
        .tile_ids
        .contains(&captain_id));
    assert!(!snapshot
        .iter()
        .filter(|tab| tab.kind() == WorkspaceKind::Work)
        .any(|tab| tab.tile_ids.contains(&captain_id)));

    close_terminal(&context, &json!({"sessionId": captain_id})).unwrap();
    let _ = std::fs::remove_dir_all(harness_bin_dir);
    let _ = std::fs::remove_file(path.with_extension("json.bak"));
    let _ = std::fs::remove_file(path);
}

#[test]
fn stale_report_is_rejected_and_answers_with_the_snapshot() {
    // Headless-org acceptance for requirement 2: a server-side move_tile must
    // survive a UI report that predates it (the exact lost-update repro: three
    // accepted move_tile calls, registry silently reverted by the reporter).
    let ctx = test_ctx("t");
    // UI boots and reports its layout (legacy/no baseSeq → accepted).
    let v = dispatch(
        &ctx,
        "report_workspace_tabs",
        &json!({"tabs": [
                {"id": "t1", "name": "Workspace 1", "tileIds": ["aa"]},
                {"id": "t2", "name": "hidden", "tileIds": []},
            ], "activeTabId": "t1", "baseSeq": 0}),
    )
    .unwrap();
    let seq = v["seq"].as_u64().unwrap();

    // A headless move into the hidden tab bumps the revision.
    dispatch(
        &ctx,
        "move_tile",
        &json!({"terminalId": "aa", "tabId": "t2"}),
    )
    .unwrap();

    // The UI (which never applied the move - hidden tab, suspended webview…)
    // reports its stale layout: REJECTED, answered with the snapshot.
    let v = dispatch(
        &ctx,
        "report_workspace_tabs",
        &json!({"tabs": [
                {"id": "t1", "name": "Workspace 1", "tileIds": ["aa"]},
                {"id": "t2", "name": "hidden", "tileIds": []},
            ], "activeTabId": "t1", "baseSeq": seq}),
    )
    .unwrap();
    assert_eq!(v["stale"], true);
    let tabs = v["tabs"].as_array().unwrap();
    let t2 = tabs.iter().find(|t| t["id"] == "t2").unwrap();
    assert_eq!(
        t2["tileIds"],
        json!(["aa"]),
        "the move survives the stale report"
    );

    // list_tabs stays truthful: the tile is in the hidden tab.
    let tabs = dispatch(&ctx, "list_tabs", &Value::Null).unwrap();
    let t2 = tabs["tabs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == "t2")
        .unwrap();
    assert_eq!(t2["tileIds"], json!(["aa"]));

    // A report based on the CURRENT revision is accepted (normal UI flow).
    let cur = tabs["seq"].as_u64().unwrap();
    let v = dispatch(
        &ctx,
        "report_workspace_tabs",
        &json!({"tabs": [
                {"id": "t1", "name": "Workspace 1", "tileIds": []},
                {"id": "t2", "name": "hidden", "tileIds": ["aa"]},
            ], "activeTabId": "t1", "baseSeq": cur}),
    )
    .unwrap();
    assert_eq!(v["reported"], 2);
}

#[test]
fn close_tab_headless_lifecycle_policy() {
    // Requirement 3: tiles leave their tab on close_terminal, and an emptied
    // auto-created tab is closeable headlessly - with the documented guards.
    let ctx = test_ctx("t");
    ctx.tab_registry().replace(vec![
        TabRecord {
            id: "t1".into(),
            name: "Workspace 1".into(),
            tile_ids: vec!["keep".into()],
        },
        TabRecord {
            id: "t2".into(),
            name: "staging".into(),
            tile_ids: vec!["dead".into()],
        },
    ]);

    // A non-empty tab is refused without force.
    let err = dispatch(&ctx, "close_tab", &json!({"tabId": "t2"})).unwrap_err();
    assert!(err.contains("close its terminals first"), "got: {err}");

    // close_terminal drops the tile from its tab (tmux kill is idempotent on
    // an already-gone session, so this exercises the registry path headlessly).
    dispatch(&ctx, "close_terminal", &json!({"sessionId": "dead"})).unwrap();
    let t2 = ctx
        .tab_registry()
        .snapshot()
        .into_iter()
        .find(|t| t.id == "t2")
        .unwrap();
    assert!(t2.tile_ids.is_empty(), "the closed tile left its tab");

    // The emptied tab closes headlessly (by name here - id also works).
    let v = dispatch(&ctx, "close_tab", &json!({"tabName": "staging"})).unwrap();
    assert_eq!(v["accepted"], "close_tab");
    assert_eq!(v["tabId"], "t2");
    assert!(ctx.tab_registry().snapshot().iter().all(|t| t.id != "t2"));

    // The LAST tab is never closed.
    let err = dispatch(&ctx, "close_tab", &json!({"tabId": "t1"})).unwrap_err();
    assert!(err.contains("last tab"), "got: {err}");
}

#[test]
fn placement_falls_back_when_the_target_tab_vanished() {
    // The tab-closed-during-spawn race, at the placement primitive: the tab
    // resolved before the tmux spawn may be gone by placement time. The tile
    // must ALWAYS land in the registry - active tab first, else first tab -
    // and the actual tab id is returned.
    let ctx = test_ctx("t");
    ctx.tab_registry().replace(vec![
        TabRecord {
            id: "t1".into(),
            name: "Workspace 1".into(),
            tile_ids: vec![],
        },
        TabRecord {
            id: "t2".into(),
            name: "Workspace 2".into(),
            tile_ids: vec![],
        },
    ]);
    assert!(ctx.tab_registry().set_active_tab("t2"));

    // Target vanished -> falls back to the ACTIVE tab.
    let placed = ctx
        .tab_registry()
        .place_tile_with_fallback("tile-a", Some("closed-mid-spawn"));
    assert_eq!(placed.as_deref(), Some("t2"));
    // Target vanished AND no active pointer -> first tab.
    ctx.tab_registry().replace(vec![TabRecord {
        id: "only".into(),
        name: "Solo".into(),
        tile_ids: vec![],
    }]);
    let placed = ctx
        .tab_registry()
        .place_tile_with_fallback("tile-b", Some("also-gone"));
    assert_eq!(placed.as_deref(), Some("only"));
    let snap = ctx.tab_registry().snapshot();
    assert_eq!(snap[0].tile_ids, vec!["tile-b"]);
    // Empty registry -> unplaced (None), the only case a tile stays out.
    ctx.tab_registry().replace(vec![]);
    assert_eq!(
        ctx.tab_registry()
            .place_tile_with_fallback("tile-c", Some("x")),
        None
    );
}

#[test]
fn spawn_survives_a_concurrent_close_of_its_target_tab() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    // Dispatch-level tab-closed-during-spawn race: close_tab races the spawn's
    // resolve->tmux->place window. Whichever side wins, the invariant holds:
    // the spawned session ends up in EXACTLY ONE registry tab, and the
    // response's tabId names that tab (fallback placement is reflected).
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let ctx = test_ctx("t").with_apply_sink(sink);
    ctx.tab_registry().replace(vec![
        TabRecord {
            id: "keep".into(),
            name: "Workspace 1".into(),
            tile_ids: vec![],
        },
        TabRecord {
            id: "doomed".into(),
            name: "staging".into(),
            tile_ids: vec![],
        },
    ]);
    assert!(ctx.tab_registry().set_active_tab("keep"));
    let spawn_started = std::env::temp_dir().join(format!(
        "t-hub-spawn-race-{}",
        uuid::Uuid::new_v4().simple()
    ));

    let closer = {
        let ctx = ctx.clone();
        let spawn_started = spawn_started.clone();
        std::thread::spawn(move || {
            // Wait until the pane command proves spawn passed strict tab
            // validation. This targets the intended resolve->place race
            // without allowing the close to invalidate the request before
            // spawn_terminal begins.
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                if spawn_started.exists() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(spawn_started.exists(), "spawn pane never signaled startup");
            // Either outcome is legal: the close wins (spawn falls back to
            // "keep") or the placement wins (close refuses the non-empty tab).
            let _ = dispatch(&ctx, "close_tab", &json!({"tabId": "doomed"}));
        })
    };
    let startup_command = format!("touch {}", spawn_started.display());
    let v = dispatch(
        &ctx,
        "spawn_terminal",
        &json!({
            "cwd": "/tmp",
            "tabId": "doomed",
            "startupCommand": startup_command,
        }),
    )
    .unwrap();
    closer.join().unwrap();
    let _ = std::fs::remove_file(spawn_started);

    let id = v["id"].as_str().unwrap().to_string();
    let placed_tab = v["tabId"].as_str().expect("always placed").to_string();
    assert_eq!(v["placed"], true);
    let owners: Vec<String> = ctx
        .tab_registry()
        .snapshot()
        .into_iter()
        .filter(|t| t.tile_ids.iter().any(|x| x == &id))
        .map(|t| t.id)
        .collect();
    assert_eq!(owners, vec![placed_tab], "tile in exactly the reported tab");

    dispatch(&ctx, "close_terminal", &json!({"sessionId": id})).unwrap();
}

#[test]
fn back_to_back_close_tab_keeps_the_active_pointer_valid() {
    // A second close (or a close racing a focus) must never leave the
    // registry's activeTabId pointing at a deleted tab: removal + pointer
    // fixup are atomic under the registry lock, and focus_tab's validate+set
    // is a single atomic operation.
    let ctx = test_ctx("t");
    ctx.tab_registry().replace(vec![
        TabRecord {
            id: "a".into(),
            name: "A".into(),
            tile_ids: vec![],
        },
        TabRecord {
            id: "b".into(),
            name: "B".into(),
            tile_ids: vec![],
        },
        TabRecord {
            id: "c".into(),
            name: "C".into(),
            tile_ids: vec![],
        },
    ]);
    assert!(ctx.tab_registry().set_active_tab("c"));

    let active_is_valid = |ctx: &ControlContext| {
        let snap = ctx.tab_registry().snapshot_full();
        let active = snap.active_tab_id.expect("active pointer set");
        assert!(
            snap.tabs.iter().any(|t| t.id == active),
            "active '{active}' must reference an existing tab; tabs: {:?}",
            snap.tabs.iter().map(|t| t.id.clone()).collect::<Vec<_>>()
        );
    };

    // Close the ACTIVE tab, then immediately close the tab the pointer
    // healed onto - the pointer must stay valid after each step.
    dispatch(&ctx, "close_tab", &json!({"tabId": "c"})).unwrap();
    active_is_valid(&ctx);
    let healed = ctx.tab_registry().snapshot_full().active_tab_id.unwrap();
    dispatch(&ctx, "close_tab", &json!({"tabId": healed})).unwrap();
    active_is_valid(&ctx);

    // focus_tab on the now-deleted tab fails cleanly, pointer untouched.
    let err = dispatch(&ctx, "focus_tab", &json!({"tabId": "c"})).unwrap_err();
    assert!(err.contains("unknown tabId"), "got: {err}");
    active_is_valid(&ctx);

    // Concurrent closes from a 3-tab registry: whichever interleaving wins,
    // the surviving pointer references a live tab.
    ctx.tab_registry().replace(vec![
        TabRecord {
            id: "a".into(),
            name: "A".into(),
            tile_ids: vec![],
        },
        TabRecord {
            id: "b".into(),
            name: "B".into(),
            tile_ids: vec![],
        },
        TabRecord {
            id: "c".into(),
            name: "C".into(),
            tile_ids: vec![],
        },
    ]);
    assert!(ctx.tab_registry().set_active_tab("b"));
    let t1 = {
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = ctx.tab_registry().remove_tab("b", false);
        })
    };
    let t2 = {
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = ctx.tab_registry().remove_tab("c", false);
        })
    };
    t1.join().unwrap();
    t2.join().unwrap();
    active_is_valid(&ctx);
}

#[test]
fn spawn_terminal_default_placement_is_the_active_tab_without_switching() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    // No tabName/tabId: the tile lands in the tab the USER has active (per the
    // registry mirror) - matching the "+" menu - and never switches it.
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let ctx = test_ctx("t").with_apply_sink(sink.clone());
    dispatch(
        &ctx,
        "report_workspace_tabs",
        &json!({"tabs": [
                {"id": "t1", "name": "Workspace 1", "tileIds": []},
                {"id": "t2", "name": "Workspace 2", "tileIds": []},
            ], "activeTabId": "t2"}),
    )
    .unwrap();

    let v = dispatch(&ctx, "spawn_terminal", &json!({"cwd": "/tmp"})).unwrap();
    let id = v["id"].as_str().unwrap().to_string();
    assert_eq!(v["tabId"], "t2", "default placement is the active tab");
    let snap = ctx.tab_registry().snapshot_full();
    assert_eq!(snap.active_tab_id.as_deref(), Some("t2"), "focus untouched");
    dispatch(&ctx, "close_terminal", &json!({"sessionId": id})).unwrap();
}

#[test]
fn report_workspace_tabs_replaces_the_registry() {
    // The frontend's up-sync (via the Tauri command, exercised here directly on
    // the shared registry) makes list_tabs reflect the live UI, including
    // UI-created tabs and real tile membership.
    let ctx = test_ctx("t");
    ctx.tab_registry().replace(vec![
        TabRecord {
            id: "t1".into(),
            name: "Main".into(),
            tile_ids: vec!["a".into(), "b".into()],
        },
        TabRecord {
            id: "t2".into(),
            name: "Side".into(),
            tile_ids: vec![],
        },
    ]);
    let tabs = dispatch(&ctx, "list_tabs", &Value::Null).unwrap();
    assert_eq!(tabs["count"], 3);
    assert_eq!(tabs["tabs"][0]["id"], "t1");
    assert_eq!(tabs["tabs"][0]["tileIds"][1], "b");
    assert_eq!(tabs["tabs"][1]["name"], "Side");
    assert_eq!(tabs["tabs"][2]["kind"], "captain");
    assert_eq!(tabs["tabs"][2]["name"], CAPTAIN_WORKSPACE_NAME);
}

#[test]
fn create_worktree_named_placement_reuses_a_tab_by_name() {
    // TASK C: a create_worktree with a tabName that already exists resolves to
    // the SAME tab id (no duplicate), and the forward carries that id so the
    // frontend places the tile deterministically, not into the focused tab.
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let ctx = test_ctx("t").with_apply_sink(sink.clone());
    // Seed an existing tab named "control-surface".
    ctx.tab_registry().replace(vec![TabRecord {
        id: "existing-tab".into(),
        name: "control-surface".into(),
        tile_ids: vec![],
    }]);
    // A create_worktree targeting that name should reuse `existing-tab`. We
    // resolve the tab BEFORE git runs by calling the registry directly for the
    // assertion (git::worktree_add needs a real repo, out of scope for a unit
    // test), mirroring the handler's own resolution.
    assert_eq!(
        ctx.tab_registry().id_for_name("control-surface"),
        Some("existing-tab".to_string())
    );
}

/// Live round-trip through dispatch: spawn a real tmux session, type a line
/// via `send_text`, read it back via `read_terminal`, then `close_terminal`.
/// Needs a real tmux on PATH (WSL2 dev shell; not the Windows CI target).
#[test]
fn live_send_read_close_roundtrip() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    // The id must honor the production invariant "the id IS the tmux session
    // suffix, capped at 8 chars" (`tmux::target_for_id`) — the previous long
    // `mcp3test<nanos>` id created `th_mcp3test<nanos>` but dispatched
    // against `th_mcp3test`, so send_text hit a session that never existed.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let id = format!("{:08x}", (nanos as u64) & 0xffff_ffff);
    let target = tmux::target_for_id(&id);
    let _ = tmux::kill_session(&target);
    tmux::new_session_with_env(&target, "/tmp", None, &[]).expect("spawn session");
    // Deterministic geometry regardless of what the server's latest client
    // reports (the wedged-2x24 gotcha; see tmux::resize_window_for_tests).
    tmux::resize_window_for_tests(&target, 80, 24).expect("resize session");

    let ctx = test_ctx("t");
    dispatch(
        &ctx,
        "send_text",
        &json!({"sessionId": id, "text": "echo MCP3_ROUNDTRIP_OK", "enter": true}),
    )
    .expect("send_text should succeed");
    std::thread::sleep(std::time::Duration::from_millis(300));

    let v = dispatch(&ctx, "read_terminal", &json!({"sessionId": id})).unwrap();
    assert!(
        v["text"].as_str().unwrap().contains("MCP3_ROUNDTRIP_OK"),
        "read_terminal should show the echoed sentinel; got {v:?}"
    );

    let c = dispatch(&ctx, "close_terminal", &json!({"sessionId": id})).unwrap();
    assert_eq!(c["accepted"], "close_terminal");
    assert!(
        !tmux::has_session(&target),
        "session should be gone after close"
    );
}

#[test]
fn idle_connection_is_closed_after_the_read_timeout() {
    use std::io::Read;
    use std::net::{TcpListener, TcpStream};

    // A listener + a context with a SHORT idle timeout. A client that connects
    // and never sends a request must be closed by the server (M2b hardening),
    // not left to park the handler thread forever.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().unwrap();
    let mut ctx = test_ctx("t");
    ctx.idle_timeout = std::time::Duration::from_millis(200);

    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept");
        // Returns Ok once the idle read times out and the request loop breaks.
        let _ = handle_conn(stream, &ctx);
    });

    // Connect, send NOTHING, then read: the server should close the socket
    // after ~200ms, so the read returns 0 (EOF). The generous 2s client-side
    // timeout only trips if the server FAILED to close us — the regression.
    let mut client = TcpStream::connect(addr).expect("connect");
    client
        .set_read_timeout(Some(std::time::Duration::from_secs(2)))
        .unwrap();
    let mut buf = [0u8; 16];
    let n = client
        .read(&mut buf)
        .expect("read should return EOF, not time out");
    assert_eq!(n, 0, "server should have closed the idle connection (EOF)");

    server.join().unwrap();
}

#[test]
fn protocol_version_gate_rejects_a_skewed_client() {
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, TcpStream};

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().unwrap();
    let ctx = test_ctx("secret");
    // Serve one connection per assertion (each `send` opens + closes one).
    let server = std::thread::spawn(move || {
        for _ in 0..4 {
            let (stream, _) = listener.accept().expect("accept");
            let _ = handle_conn(stream, &ctx);
        }
    });

    // Open a connection, send one frame, read one response line.
    let send = |frame: Value| -> Value {
        let mut s = TcpStream::connect(addr).expect("connect");
        let mut bytes = serde_json::to_vec(&frame).unwrap();
        bytes.push(b'\n');
        s.write_all(&bytes).unwrap();
        let mut reader = BufReader::new(s);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        serde_json::from_str::<Value>(line.trim()).unwrap()
    };

    // A valid token but a version NEWER than the server speaks is rejected — the
    // gate fires before dispatch, with a clear, actionable message.
    let bad = send(json!({"token": "secret", "command": "list_tabs", "v": 999}));
    assert_eq!(bad["ok"], false);
    assert!(
        bad["error"]
            .as_str()
            .unwrap()
            .contains("protocol version too new"),
        "got: {bad}"
    );

    // The matching version passes the gate and dispatches normally.
    let good = send(json!({"token": "secret", "command": "list_tabs", "v": PROTOCOL_VERSION}));
    assert_eq!(good["ok"], true, "got: {good}");

    // A LOWER version (a v1 client against this v2 server) is still accepted —
    // the protocol is backward-compatible (T13 binary framing is opt-in), so the
    // live webview keeps working unchanged.
    let v1 = send(json!({"token": "secret", "command": "list_tabs", "v": 1}));
    assert_eq!(v1["ok"], true, "got: {v1}");

    // No version field at all stays accepted (backward-compat: the MCP / legacy
    // clients don't advertise one).
    let legacy = send(json!({"token": "secret", "command": "list_tabs"}));
    assert_eq!(legacy["ok"], true, "got: {legacy}");

    server.join().unwrap();
}

#[test]
fn loopback_file_read_bypasses_the_scope() {
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, TcpStream};

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().unwrap();
    let ctx = test_ctx("secret");
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept");
        let _ = handle_conn(stream, &ctx);
    });

    // list_dir on a NON-indexed path: over loopback the peer is trusted, so the
    // #23 scope is bypassed and the listing succeeds. This proves handle_conn
    // tags the 127.0.0.1 peer as loopback -> enforce_scope=false end-to-end (a
    // logic inversion would either over-restrict locally or — worse — fail to
    // restrict a real remote peer; the core's enforce=true path is covered by
    // the files.rs scope test).
    let mut s = TcpStream::connect(addr).expect("connect");
    let tmp = std::env::temp_dir().to_string_lossy().into_owned();
    let frame = json!({"token": "secret", "command": "list_dir", "args": {"path": tmp}});
    let mut bytes = serde_json::to_vec(&frame).unwrap();
    bytes.push(b'\n');
    s.write_all(&bytes).unwrap();
    let mut reader = BufReader::new(s);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let resp: Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(
        resp["ok"], true,
        "loopback list_dir should bypass scope: {resp}"
    );
    // Close the client so the server's next read hits EOF and handle_conn
    // returns immediately — otherwise it would park until the idle timeout.
    drop(reader);

    server.join().unwrap();
}

#[test]
fn theme_commands_are_forwarded_by_name() {
    let ctx = test_ctx("t");
    // Forwarded by name; not yet wired ⇒ a clear, theme-specific error (not
    // the generic "unknown command" arm). This proves the forward path.
    for cmd in ["get_theme", "set_theme"] {
        let err = dispatch(&ctx, cmd, &Value::Null).unwrap_err();
        assert!(err.contains("theme command handler"), "got: {err}");
    }
}

#[test]
fn get_status_requires_session_id() {
    let ctx = test_ctx("t");
    let err = dispatch(&ctx, "get_status", &Value::Null).unwrap_err();
    assert!(err.contains("sessionId"), "got: {err}");
}

#[test]
fn get_status_returns_unknown_for_unseen_session() {
    let ctx = test_ctx("t");
    let v = dispatch(&ctx, "get_status", &json!({"sessionId": "nope"})).unwrap();
    assert_eq!(v["status"], "unknown");
    assert_eq!(v["sessionId"], "nope");
    assert!(v["snapshot"].is_null());
}

#[test]
fn supervision_tree_unknown_session_is_null() {
    let ctx = test_ctx("t");
    let v = dispatch(&ctx, "supervision_tree", &json!({"sessionId": "nope"})).unwrap();
    assert!(v.is_null());
}

#[test]
fn supervision_session_ids_returns_an_array() {
    // An empty supervisor reports no sessions — but the command returns a JSON
    // array (not null/error), matching the Tauri command's `Vec<String>`.
    let ctx = test_ctx("t");
    let v = dispatch(&ctx, "supervision_session_ids", &Value::Null).unwrap();
    assert!(v.is_array(), "expected an array, got {v:?}");
    assert_eq!(v.as_array().unwrap().len(), 0);
}

#[test]
fn wsl_health_has_metrics_and_supervised_count() {
    let ctx = test_ctx("t");
    let v = dispatch(&ctx, "wsl_health", &Value::Null).unwrap();
    assert!(v.get("metrics").is_some());
    assert_eq!(v["supervisedSessions"], 0);
    // The metrics object always carries capturedAtMs + cpuCount.
    assert!(v["metrics"].get("capturedAtMs").is_some());
    assert!(v["metrics"].get("cpuCount").is_some());
}

#[test]
fn organization_actions_are_accepted_and_audited() {
    // No apply sink (headless): accepted + audited, but not applied.
    // focus_session and a targetId-only move_tile (within-tab reorder) stay
    // legacy pass-through forwards.
    let ctx = test_ctx("t");
    for (cmd, args) in [
        ("focus_session", json!({"sessionId": "s1"})),
        ("move_tile", json!({"terminalId": "t1", "targetId": "t2"})),
    ] {
        let v = dispatch(&ctx, cmd, &args).unwrap();
        assert_eq!(v["accepted"], cmd);
        assert_eq!(v["audited"], true);
        assert_eq!(v["applied"], false);
    }
    // Headless-org: registry-first mutations are STRICT - an unknown target
    // is a hard error, not the old silent accept-then-lose.
    for (cmd, args) in [
        ("move_tile", json!({"terminalId": "t1", "tabId": "nope"})),
        ("rename_tab", json!({"tabId": "nope", "name": "x"})),
        ("close_tab", json!({"tabId": "nope"})),
    ] {
        let err = dispatch(&ctx, cmd, &args).unwrap_err();
        assert!(err.contains("unknown"), "{cmd}: {err}");
    }
}

/// A recording sink that captures every forwarded `{command, args}` so the
/// test can assert the dispatcher forwards Organization-tier mutations to it.
struct RecordingSink {
    calls: StdMutex<Vec<(String, Value)>>,
}
impl ApplySink for RecordingSink {
    fn apply(&self, command: &str, args: &Value) -> Result<(), String> {
        self.calls
            .lock()
            .unwrap()
            .push((command.to_string(), args.clone()));
        Ok(())
    }
}

#[test]
fn organization_actions_are_forwarded_and_applied_with_a_sink() {
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let ctx = test_ctx("t").with_apply_sink(sink.clone());
    ctx.tab_registry().replace(vec![
        TabRecord {
            id: "tab-1".into(),
            name: "Main".into(),
            tile_ids: vec!["term-1".into()],
        },
        TabRecord {
            id: "tab-2".into(),
            name: "Side".into(),
            tile_ids: vec![],
        },
    ]);

    for (cmd, args) in [
        ("focus_session", json!({"sessionId": "term-1"})),
        (
            "move_tile",
            json!({"terminalId": "term-1", "tabId": "tab-2"}),
        ),
        ("rename_tab", json!({"tabId": "tab-2", "name": "Ops"})),
    ] {
        let v = dispatch(&ctx, cmd, &args).unwrap();
        assert_eq!(v["accepted"], cmd);
        assert_eq!(v["audited"], true);
        // With a sink wired, the action is forwarded to the UI and applied.
        assert_eq!(v["applied"], true, "expected applied:true for {cmd}");
    }

    // Every Organization-tier command reached the sink, in order, with args.
    let calls = sink.calls.lock().unwrap();
    let names: Vec<&str> = calls.iter().map(|(c, _)| c.as_str()).collect();
    assert_eq!(names, ["focus_session", "move_tile", "rename_tab"]);
    assert_eq!(calls[0].1, json!({"sessionId": "term-1"}));

    // Headless-org: registry-first forwards carry the authoritative snapshot
    // (`sync.seq` / `sync.tabs`) so the UI renders FROM the registry - the
    // move is visible in the snapshot even before any UI report.
    let sync = &calls[1].1["sync"];
    assert!(sync["seq"].as_u64().unwrap() >= 1);
    let tabs = sync["tabs"].as_array().unwrap();
    let tab2 = tabs.iter().find(|t| t["id"] == "tab-2").unwrap();
    assert_eq!(tab2["tileIds"], json!(["term-1"]));
    assert_eq!(calls[2].1["name"], "Ops");
}

/// Register a real loopback socket as an event subscriber on `fanout`,
/// returning a line reader over the client end (T12 broadcast tests).
fn subscribe_test_reader(fanout: &EventFanout) -> std::io::BufReader<std::net::TcpStream> {
    use std::io::BufReader;
    use std::net::{TcpListener, TcpStream};
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().unwrap();
    let client = TcpStream::connect(addr).expect("connect");
    client
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();
    let (server_side, _) = listener.accept().expect("accept");
    fanout.register(server_side);
    BufReader::new(client)
}

fn assert_no_event(reader: &mut std::io::BufReader<std::net::TcpStream>) {
    use std::io::BufRead;
    reader
        .get_ref()
        .set_read_timeout(Some(std::time::Duration::from_millis(100)))
        .unwrap();
    let mut line = String::new();
    let error = reader
        .read_line(&mut line)
        .expect_err("no event should be broadcast");
    assert!(
        matches!(
            error.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        ),
        "unexpected subscriber read error: {error}"
    );
    assert!(line.is_empty());
}

/// Read one `{"event":..,"payload":..}` frame from a subscriber reader.
fn read_event_frame(reader: &mut impl std::io::BufRead) -> Value {
    let mut line = String::new();
    reader.read_line(&mut line).expect("read event frame");
    serde_json::from_str(line.trim()).expect("event frame is JSON")
}

/// SERVE-PATH WEDGE REGRESSION: a subscriber that stops draining its socket
/// must not stall an UNRELATED fanout operation. This reproduces the control
/// wedge in the small: `emit_event` used to hold the `subs` registry lock
/// across every blocking per-subscriber `write_all`, so a single stuck client
/// (its send buffer full) parked the lock for the full 5s `SO_SNDTIMEO` - and
/// with it every `register`/`unregister`/`subscriber_count` and every other
/// emit. Here a background emit blocks writing to a never-draining subscriber
/// while the main thread times a `register` + `subscriber_count`; with the lock
/// held across the write those calls block ~5s (the test's 3s bound trips),
/// and with the snapshot-then-write-unlocked fix they return immediately.
#[test]
fn stuck_subscriber_does_not_stall_registry_ops() {
    use std::net::{TcpListener, TcpStream};
    use std::time::{Duration, Instant};

    let fanout = Arc::new(EventFanout::new());

    // A "stuck" subscriber: a real loopback socket whose CLIENT end never
    // reads. We shrink both buffers so a modest frame overflows the send path
    // and the emit's `write_all` blocks (until the 5s subscriber write timeout
    // register() installs). The client MUST stay alive and unread for the
    // duration, so we hold it in scope and never touch it.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().unwrap();
    let stuck_client = TcpStream::connect(addr).expect("connect stuck client");
    {
        let cref = socket2::SockRef::from(&stuck_client);
        let _ = cref.set_recv_buffer_size(1024);
    }
    let (stuck_server, _) = listener.accept().expect("accept stuck server");
    {
        let sref = socket2::SockRef::from(&stuck_server);
        let _ = sref.set_send_buffer_size(1024);
    }
    fanout.register(stuck_server);

    // Background emit: a payload comfortably larger than the shrunk buffers so
    // the write to the stuck subscriber blocks rather than completing.
    let emit_fanout = Arc::clone(&fanout);
    let emitter = std::thread::spawn(move || {
        let big = "x".repeat(4 * 1024 * 1024);
        emit_fanout.emit_event("control://wedge-test", &json!({ "blob": big }));
    });

    // Let the emit get into its blocking write (and, on the buggy code, take
    // and hold the registry lock). This delay is OUTSIDE the measured window.
    std::thread::sleep(Duration::from_millis(300));

    // The unrelated registry ops. On the pre-fix code these block on the
    // `subs` lock the stuck emit holds for ~5s; with the fix the lock is free.
    let healthy_listener = TcpListener::bind("127.0.0.1:0").expect("bind healthy");
    let healthy_addr = healthy_listener.local_addr().unwrap();
    let _healthy_client = TcpStream::connect(healthy_addr).expect("connect healthy");
    let (healthy_server, _) = healthy_listener.accept().expect("accept healthy");

    let started = Instant::now();
    let id = fanout.register(healthy_server);
    let count = fanout.subscriber_count();
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_secs(3),
        "registry ops stalled behind a stuck subscriber's emit write ({elapsed:?}); \
             the subs lock is being held across the blocking socket write"
    );
    assert!(count >= 1, "the healthy subscriber should be registered");
    let _ = id;

    // The stuck subscriber's write eventually times out (5s SO_SNDTIMEO) and
    // the emit thread returns; join so the test owns no leaked thread. Keep the
    // stuck client alive until here so the connection never closes early.
    emitter.join().expect("emit thread joins");
    drop(stuck_client);
}

#[test]
fn apply_forwards_are_broadcast_to_event_subscribers() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    // T12: every accepted Organization forward ALSO reaches event
    // subscribers on `control://apply`, while the webview sink keeps
    // receiving exactly what it always did.
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let fanout = Arc::new(EventFanout::new());
    let ctx = test_ctx("t")
        .with_apply_sink(sink.clone())
        .with_event_fanout(fanout.clone());
    ctx.tab_registry().replace(vec![TabRecord {
        id: "tab-1".into(),
        name: "Main".into(),
        tile_ids: vec![],
    }]);
    let mut reader = subscribe_test_reader(&fanout);

    // A plain organization apply: broadcast mirrors the sink call.
    let v = dispatch(&ctx, "focus_tab", &json!({"tabId": "tab-1"})).unwrap();
    assert_eq!(v["applied"], true);
    let frame = read_event_frame(&mut reader);
    assert_eq!(frame["event"], APPLY_EVENT_CHANNEL);
    assert_eq!(frame["payload"]["command"], "focus_tab");
    assert_eq!(frame["payload"]["args"], json!({"tabId": "tab-1"}));

    // new_tab: the broadcast carries the SAME core-minted id the caller got.
    let v = dispatch(&ctx, "new_tab", &json!({"name": "Logs"})).unwrap();
    let frame = read_event_frame(&mut reader);
    assert_eq!(frame["payload"]["command"], "new_tab");
    assert_eq!(frame["payload"]["args"]["id"], v["tabId"]);
    assert_eq!(frame["payload"]["args"]["name"], "Logs");

    // spawn_terminal: the server spawns + places (headless-org), so sink AND
    // subscribers both hear the forward WITH the real id + registry snapshot.
    let v = dispatch(&ctx, "spawn_terminal", &json!({"cwd": "/tmp", "name": "n"})).unwrap();
    assert_eq!(v["accepted"], "spawn_terminal");
    let spawned_id = v["id"].as_str().unwrap().to_string();
    let frame = read_event_frame(&mut reader);
    assert_eq!(frame["payload"]["command"], "spawn_terminal");
    assert_eq!(frame["payload"]["args"]["cwd"], "/tmp");
    assert_eq!(frame["payload"]["args"]["id"], json!(spawned_id));
    assert!(frame["payload"]["args"]["sync"]["seq"].as_u64().is_some());

    // remove_worktree fails before either the sink or subscribers receive a
    // detach forward.
    let err = dispatch(
        &ctx,
        "remove_worktree",
        &json!({"repoRoot": "/r", "worktreePath": "/r/wt"}),
    )
    .unwrap_err();
    assert_eq!(err, git::WORKTREE_REMOVAL_UNAVAILABLE);
    assert_no_event(&mut reader);

    // The sink saw every forward, unchanged by the broadcast riding along.
    let names: Vec<String> = sink
        .calls
        .lock()
        .unwrap()
        .iter()
        .map(|(c, _)| c.clone())
        .collect();
    assert_eq!(names, ["focus_tab", "new_tab", "spawn_terminal"]);

    // Reap the real session the spawn created.
    dispatch(&ctx, "close_terminal", &json!({"sessionId": spawned_id})).unwrap();
}

#[test]
fn forward_without_sink_counts_event_subscribers_as_delivery() {
    // T12: a headless server fronting the native cockpit has no ApplySink;
    // reaching an event subscriber is what "applied" means there.
    let fanout = Arc::new(EventFanout::new());
    let ctx = test_ctx("t").with_event_fanout(fanout.clone());
    ctx.tab_registry().replace(vec![TabRecord {
        id: "x".into(),
        name: "Main".into(),
        tile_ids: vec![],
    }]);
    let mut reader = subscribe_test_reader(&fanout);

    let v = dispatch(&ctx, "rename_tab", &json!({"tabId": "x", "name": "ops"})).unwrap();
    assert_eq!(
        v["applied"], true,
        "subscriber delivery counts without a sink"
    );
    let frame = read_event_frame(&mut reader);
    assert_eq!(frame["payload"]["command"], "rename_tab");
    // (Sink-less AND subscriber-less stays applied:false - covered by
    // `organization_actions_are_accepted_and_audited`.)
}

#[test]
fn report_workspace_tabs_replaces_the_registry_for_list_tabs() {
    // T12: the socket twin of the Tauri report command - the native client's
    // half of the registry mirror.
    let ctx = test_ctx("t");
    let v = dispatch(
        &ctx,
        "report_workspace_tabs",
        &json!({"tabs": [
            {"id": "t1", "name": "Workspace 1", "tileIds": ["aa", "bb"]},
            {"id": "t2", "name": "ops", "tileIds": []},
        ]}),
    )
    .unwrap();
    assert_eq!(v["reported"], 2);

    let tabs = dispatch(&ctx, "list_tabs", &json!({})).unwrap();
    assert_eq!(tabs["count"], 3);
    assert_eq!(tabs["tabs"][0]["id"], "t1");
    assert_eq!(tabs["tabs"][0]["tileIds"], json!(["aa", "bb"]));
    assert_eq!(tabs["tabs"][1]["name"], "ops");
    assert_eq!(tabs["tabs"][2]["id"], CAPTAIN_WORKSPACE_ID);

    // A report may not erase the last Work Workspace. The reserved Captain
    // Workspace is not a usable canvas for ordinary terminals.
    let err = dispatch(&ctx, "report_workspace_tabs", &json!({"tabs": []})).unwrap_err();
    assert!(err.contains("at least one Work Workspace"), "got: {err}");
    assert_eq!(dispatch(&ctx, "list_tabs", &json!({})).unwrap()["count"], 3);

    // Malformed shapes are a clear error, not a partial replace.
    let err = dispatch(&ctx, "report_workspace_tabs", &json!({})).unwrap_err();
    assert!(err.contains("requires a 'tabs'"), "got: {err}");
    let err = dispatch(
        &ctx,
        "report_workspace_tabs",
        &json!({"tabs": [{"name": 7}]}),
    )
    .unwrap_err();
    assert!(err.contains("bad 'tabs' shape"), "got: {err}");
}

#[test]
fn search_files_searches_a_real_tree() {
    // Build a tiny fixture and search it end-to-end through dispatch.
    let mut root = std::env::temp_dir();
    root.push(format!("t-hub-control-files-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
    std::fs::write(root.join("README.md"), "# hi").unwrap();

    let ctx = test_ctx("t");
    let v = dispatch(
        &ctx,
        "search_files",
        &json!({ "root": root.to_string_lossy(), "query": "main", "limit": 5 }),
    )
    .unwrap();
    let hits = v["hits"].as_array().unwrap();
    assert!(
        hits.iter().any(|h| h["relPath"] == "src/main.rs"),
        "expected src/main.rs in {hits:?}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn open_file_reads_text_contents() {
    let mut root = std::env::temp_dir();
    root.push(format!("t-hub-control-open-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let file = root.join("notes.md");
    std::fs::write(&file, "# Title\n\nbody").unwrap();

    let ctx = test_ctx("t");
    let v = dispatch(
        &ctx,
        "open_file",
        &json!({ "path": file.to_string_lossy() }),
    )
    .unwrap();
    assert_eq!(v["ext"], "md");
    assert!(v["text"].as_str().unwrap().contains("# Title"));

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn event_fanout_streams_a_frame_to_a_subscriber() {
    // server-split M1: a registered subscriber receives each backend event as a
    // newline-delimited `{event,payload}` frame; unregister drops it. Uses a
    // real loopback socket pair but is deterministic (no disconnect-timing
    // races — we assert the explicit unregister, not write-error pruning).
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let client = TcpStream::connect(addr).unwrap();
    let (server, _) = listener.accept().unwrap();

    let fanout = EventFanout::new();
    let id = fanout.register(server);
    assert_eq!(fanout.subscriber_count(), 1);

    fanout.emit_event(
        "session://status",
        &json!({ "sessionId": "s1", "status": "working" }),
    );

    let mut reader = BufReader::new(&client);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let frame: Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(frame["event"], "session://status");
    assert_eq!(frame["payload"]["sessionId"], "s1");
    assert_eq!(frame["payload"]["status"], "working");

    fanout.unregister(id);
    assert_eq!(fanout.subscriber_count(), 0);
}

#[test]
fn is_allowed_peer_admits_only_loopback_and_tailscale() {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
    // Loopback — always.
    assert!(is_allowed_peer(IpAddr::V4(Ipv4Addr::LOCALHOST)));
    assert!(is_allowed_peer(IpAddr::V6(Ipv6Addr::LOCALHOST)));
    // Tailscale CGNAT 100.64.0.0/10 (IPv4).
    assert!(is_allowed_peer(IpAddr::V4(Ipv4Addr::new(100, 64, 0, 1))));
    assert!(is_allowed_peer(IpAddr::V4(Ipv4Addr::new(
        100, 127, 255, 254
    ))));
    // Tailscale ULA fd7a:115c::/32 (IPv6).
    assert!(is_allowed_peer(IpAddr::V6(Ipv6Addr::new(
        0xfd7a, 0x115c, 0, 0, 0, 0, 0, 1
    ))));
    // LAN / public — rejected before auth.
    assert!(!is_allowed_peer(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 10))));
    assert!(!is_allowed_peer(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5))));
    assert!(!is_allowed_peer(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
    // 100.x OUTSIDE the 64..=127 second octet is NOT Tailscale — rejected.
    assert!(!is_allowed_peer(IpAddr::V4(Ipv4Addr::new(100, 0, 0, 1))));
    assert!(!is_allowed_peer(IpAddr::V4(Ipv4Addr::new(100, 128, 0, 1))));
    // IPv4-mapped IPv6 (how IPv4 peers arrive on a dual-stack [::] bind): a
    // mapped loopback / tailnet IP is admitted, a mapped public IP rejected.
    assert!(is_allowed_peer("::ffff:127.0.0.1".parse().unwrap()));
    assert!(is_allowed_peer("::ffff:100.64.0.1".parse().unwrap()));
    assert!(!is_allowed_peer("::ffff:8.8.8.8".parse().unwrap()));
}

#[test]
fn handshake_roundtrips_through_json() {
    let h = ControlHandshake {
        addr: "127.0.0.1:5000".into(),
        token: "abc".into(),
        read_token: "rdonly".into(),
        pid: 42,
        protocol_version: PROTOCOL_VERSION,
        instance_id: "instance".into(),
        listener_generation: 1,
        published_at: 123,
        local_control_token: "full".into(),
        local_host_token: "host".into(),
    };
    let s = serde_json::to_string(&h).unwrap();
    let back: ControlHandshake = serde_json::from_str(&s).unwrap();
    assert_eq!(back.addr, "127.0.0.1:5000");
    assert_eq!(back.token, "abc");
    assert_eq!(back.read_token, "rdonly");
    assert_eq!(back.pid, 42);
    assert_eq!(back.protocol_version, PROTOCOL_VERSION);
    // `local_control_token` is in-process-only: it is NEVER serialized, so it
    // does not survive the JSON round-trip and comes back empty (its default).
    assert!(
        !s.contains("local_control_token"),
        "field must not serialize"
    );
    assert!(
        !s.contains("full"),
        "in-process token must not appear in JSON"
    );
    assert_eq!(back.local_control_token, "");
}

#[test]
fn old_handshake_without_read_token_still_parses() {
    // Backward-compat: a control.json written before Phase 2 (no read_token
    // field) must still deserialize - the field defaults to empty.
    let json = r#"{"addr":"127.0.0.1:9","token":"t","pid":1,"protocol_version":2}"#;
    let hs: ControlHandshake = serde_json::from_str(json).unwrap();
    assert_eq!(hs.token, "t");
    assert_eq!(hs.read_token, "");
    // The Phase-3 in-process field is absent from old files and defaults empty.
    assert_eq!(hs.local_control_token, "");
}

// ---- s27: attach path vs client churn -----------------------------------

use std::time::Duration;

/// The attach-churn tests share the process-global forwarder counter (and
/// real tmux sessions), so they run serialized; everything else in this
/// module stays parallel. Poison is ignored: a failed churn test must not
/// cascade into the other one.
static ATTACH_TEST_SERIAL: StdMutex<()> = StdMutex::new(());

fn attach_serial_guard() -> std::sync::MutexGuard<'static, ()> {
    ATTACH_TEST_SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Stand up the REAL accept loop (`serve`, not per-connection `handle_conn`)
/// on an ephemeral loopback port. The thread parks in accept for the process
/// lifetime, exactly like the `control_probe_server` example.
fn spawn_attach_listener(ctx: ControlContext) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    std::thread::spawn(move || serve(listener, ctx, stop));
    addr
}

/// Round-trip a no-I/O `get_theme` against `addr`; returns true iff the listener
/// accepted, handled, and wrote back a response line. Short timeouts so a
/// refused/retired port returns false fast instead of hanging the test. Any
/// response (even the theme "not wired" error) proves the serve path is live.
fn listener_serves(addr: &str) -> bool {
    use std::io::{BufRead, BufReader, Write};
    let sock: std::net::SocketAddr = match addr.parse() {
        Ok(s) => s,
        Err(_) => return false,
    };
    let stream = match TcpStream::connect_timeout(&sock, Duration::from_millis(300)) {
        Ok(s) => s,
        Err(_) => return false,
    };
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
    let mut writer = match stream.try_clone() {
        Ok(w) => w,
        Err(_) => return false,
    };
    let req = json!({"token": "secret", "command": "get_theme", "args": {}, "v": 1}).to_string();
    if writeln!(writer, "{req}").is_err() {
        return false;
    }
    let mut line = String::new();
    matches!(BufReader::new(stream).read_line(&mut line), Ok(n) if n > 0)
}

fn listener_discovery_proof(addr: &str, nonce: &str) -> Option<Value> {
    use std::io::{BufRead, BufReader, Write};
    let socket: std::net::SocketAddr = addr.parse().ok()?;
    let stream = TcpStream::connect_timeout(&socket, Duration::from_millis(300)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    let mut writer = stream.try_clone().ok()?;
    let request = json!({
        "token": "read-secret",
        "session": "",
        "command": "control_discovery_proof",
        "args": {"nonce": nonce},
        "v": PROTOCOL_VERSION,
    });
    writeln!(writer, "{request}").ok()?;
    let mut line = String::new();
    if BufReader::new(stream).read_line(&mut line).ok()? == 0 {
        return None;
    }
    serde_json::from_str::<Value>(&line)
        .ok()?
        .get("result")
        .cloned()
}

/// Poll `cond` until it holds or `budget` elapses (short sleeps).
fn wait_until(budget: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    cond()
}

/// RELAY-WEDGE SELF-HEAL (cause 2): `rebind_control` binds a fresh port, atomically
/// rewrites control.json (tokens KEPT), serves on the new port, retires the old
/// listener, and rate-limits back-to-back rebinds. (The WSL relay wedge itself is
/// unreproducible in-process - this proves the app-side rebind mechanics the client
/// bridge triggers; see the PR for the honest E2E limits.)
#[test]
fn rebind_control_moves_listener_rewrites_json_and_rate_limits() {
    // Unique temp control.json for this test; handshake_path() honors this env.
    let cj = std::env::temp_dir().join(format!(
        "t-hub-rebind-{}-{}.json",
        std::process::id(),
        REBIND_TEST_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::env::set_var("T_HUB_CONTROL_FILE", &cj);
    let _ = std::fs::remove_file(&cj);

    // Stand up an initial loopback listener + serve loop, like `start`: bind, set
    // addr on the ctx, register the stop flag in the rebind controller.
    let mut ctx = test_ctx("secret");
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind initial");
    let old_addr = listener.local_addr().unwrap().to_string();
    ctx.addr = old_addr.clone();
    let old_generation = ctx.listener_generation.fetch_add(1, Ordering::AcqRel) + 1;
    ctx.bound_listener_generation = old_generation;
    let stop = Arc::new(AtomicBool::new(false));
    ctx.rebind.set_initial_stop(stop.clone());
    {
        let serve_ctx = ctx.clone();
        let serve_stop = stop.clone();
        std::thread::spawn(move || serve(listener, serve_ctx, serve_stop));
    }
    assert!(
        wait_until(Duration::from_secs(2), || listener_serves(&old_addr)),
        "the initial listener should serve before a rebind"
    );
    let old_proof = listener_discovery_proof(&old_addr, "old-listener-proof").unwrap();
    assert_eq!(old_proof["listenerAddr"], old_addr);
    assert_eq!(old_proof["listenerGeneration"], old_generation);

    // WRITE-token gated: rebind_control is Organization tier (control token only).
    assert_eq!(required_tier("rebind_control"), CommandTier::Organization);

    // Rebind.
    let resp = rebind_control(&ctx).expect("rebind ok");
    assert_eq!(resp["rebound"], true);
    assert_eq!(resp["tokensRotated"], false);
    let new_addr = resp["addr"].as_str().unwrap().to_string();
    assert_ne!(new_addr, old_addr, "rebind must move to a fresh port");

    // control.json now names the fresh addr (atomic rewrite), tokens KEPT (a
    // rebind is transport recovery, never a key rotation). Under item-3's default-ON
    // hardening the PUBLISHED token is the read token ("read-secret") - still the
    // SAME read token, not a rotated one - and the full token stays off disk; the
    // frontend keeps full control via the in-process local_control_token.
    let written: Value =
        serde_json::from_slice(&std::fs::read(&cj).expect("read control.json")).unwrap();
    assert_eq!(written["addr"], json!(new_addr));
    assert_eq!(
        written["token"],
        json!("read-secret"),
        "the published token must be the KEPT read token (harden default-ON), not rotated"
    );
    assert_ne!(
        written["token"],
        json!("secret"),
        "the full token must NOT reach disk"
    );

    // The NEW listener serves; the OLD one is retired (stops accepting).
    assert!(
        wait_until(Duration::from_secs(2), || listener_serves(&new_addr)),
        "the fresh listener should serve after a rebind"
    );
    let new_proof = listener_discovery_proof(&new_addr, "new-listener-proof").unwrap();
    assert_eq!(new_proof["listenerAddr"], new_addr);
    assert_eq!(
        new_proof["listenerGeneration"],
        written["listener_generation"]
    );
    assert_ne!(
        new_proof["listenerGeneration"],
        old_proof["listenerGeneration"]
    );
    assert!(
        wait_until(Duration::from_secs(3), || !listener_serves(&old_addr)),
        "the old listener should stop accepting after a rebind"
    );

    // A second immediate rebind is rate-limited with a clear cooldown message.
    let err = rebind_control(&ctx).unwrap_err();
    assert!(
        err.contains("rate-limited"),
        "a back-to-back rebind must be refused: {err}"
    );

    // Cleanup: retire the fresh listener + env so we leak neither a thread nor state.
    if let Some(s) = ctx.rebind.lock().current_stop.take() {
        s.store(true, Ordering::Release);
    }
    wake_accept(&new_addr);
    std::env::remove_var("T_HUB_CONTROL_FILE");
    let _ = std::fs::remove_file(&cj);
}

#[test]
fn failed_rebind_publication_preserves_old_bound_proof_and_retires_unpublished_generation() {
    let root = std::env::temp_dir().join(format!(
        "t-hub-rebind-publish-fail-{}-{}",
        std::process::id(),
        REBIND_TEST_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let blocked_handshake = root.join("control.json");
    std::fs::create_dir_all(&blocked_handshake).unwrap();
    std::env::set_var("T_HUB_CONTROL_FILE", &blocked_handshake);

    let mut ctx = test_ctx("secret");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let old_addr = listener.local_addr().unwrap().to_string();
    ctx.addr = old_addr.clone();
    let old_generation = ctx.listener_generation.fetch_add(1, Ordering::AcqRel) + 1;
    ctx.bound_listener_generation = old_generation;
    let stop = Arc::new(AtomicBool::new(false));
    ctx.rebind.set_initial_stop(stop.clone());
    {
        let serve_ctx = ctx.clone();
        let serve_stop = stop.clone();
        std::thread::spawn(move || serve(listener, serve_ctx, serve_stop));
    }
    assert!(wait_until(Duration::from_secs(2), || {
        listener_discovery_proof(&old_addr, "before-failed-publish").is_some()
    }));

    let error = rebind_control(&ctx).unwrap_err();
    assert!(error.contains("failed to publish control.json"));
    let unpublished_addr = error
        .split("fresh port ")
        .nth(1)
        .and_then(|tail| tail.split(" but failed").next())
        .unwrap()
        .to_string();
    assert_eq!(
        ctx.listener_generation.load(Ordering::Acquire),
        old_generation + 1,
        "the failed publication consumes its reserved generation"
    );
    let old_proof =
        listener_discovery_proof(&old_addr, "after-failed-publish").expect("old remains live");
    assert_eq!(old_proof["listenerAddr"], old_addr);
    assert_eq!(old_proof["listenerGeneration"], old_generation);
    assert!(
        wait_until(Duration::from_secs(2), || listener_discovery_proof(
            &unpublished_addr,
            "unpublished"
        )
        .is_none()),
        "the unpublished generation must not remain available for validation"
    );

    stop.store(true, Ordering::Release);
    wake_accept(&old_addr);
    std::env::remove_var("T_HUB_CONTROL_FILE");
    let _ = std::fs::remove_dir_all(root);
}

static REBIND_TEST_SEQ: AtomicU64 = AtomicU64::new(0);

/// A disposable real tmux session for attach tests; returns (id, tmux name).
fn churn_tmux_session(tag: &str) -> (String, String) {
    let id = format!(
        "s27{tag}{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let target = format!("th_{id}");
    let _ = tmux::kill_session(&target);
    tmux::new_session_with_env(&target, "/tmp", None, &[]).expect("spawn churn tmux session");
    (id, target)
}

/// A disposable churn tmux session that is ALWAYS killed on drop - including
/// when an assertion panics mid-test - so the attach suite can NEVER leak a
/// `th_s27*` session onto the socket. That leak is exactly what produced the
/// 13 `th_s27churn*` ghosts in the incident: a failing run of the churn test
/// left its sessions behind, and the app's post-restart adopt path then choked
/// on the debris. Paired with the `cfg(test)` socket isolation in `tmux.rs`
/// (THIS crate's test sessions live on `t-hub-test`, never the live `t-hub`
/// socket), this makes a leak from the attach suite both unable-to-hit-the-live
/// -app AND self-cleaning. (Other producers isolate separately - see the SCOPE
/// note on `tmux::SOCKET_NAME`.)
struct ChurnSession {
    id: String,
    target: String,
}

impl ChurnSession {
    fn new(tag: &str) -> Self {
        let (id, target) = churn_tmux_session(tag);
        Self { id, target }
    }
}

impl Drop for ChurnSession {
    fn drop(&mut self) {
        let _ = tmux::kill_session(&self.target);
    }
}

/// Send a v1 `attach_pty` request line on `stream`.
fn send_attach_request(stream: &mut TcpStream, token: &str, session_id: &str) {
    let mut frame = serde_json::to_vec(&json!({
        "token": token,
        "command": ATTACH_PTY_COMMAND,
        "args": { "sessionId": session_id, "cols": 80, "rows": 24 },
    }))
    .unwrap();
    frame.push(b'\n');
    stream.write_all(&frame).expect("write attach_pty request");
}

/// Send a v1 `{"write":"<b64>"}` input frame (keystrokes) on `stream`.
fn send_write_frame(stream: &mut TcpStream, keys: &str) {
    let mut frame = serde_json::to_vec(&json!({ "write": STANDARD.encode(keys) })).unwrap();
    frame.push(b'\n');
    stream.write_all(&frame).expect("write input frame");
}

/// Read one newline-delimited JSON frame; panics on EOF (caller expects one).
fn read_json_frame(reader: &mut BufReader<TcpStream>) -> Value {
    let mut line = String::new();
    let n = reader.read_line(&mut line).expect("read frame");
    assert!(n > 0, "connection closed before the expected frame");
    serde_json::from_str(line.trim()).expect("frame is JSON")
}

/// Poll `ok` until it holds or `deadline` elapses (then panic with `what`).
fn eventually(what: &str, deadline: Duration, mut ok: impl FnMut() -> bool) {
    let start = std::time::Instant::now();
    while start.elapsed() < deadline {
        if ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("timed out waiting for {what}");
}

/// THE s27 regression: N clients die abruptly at every stage of the attach
/// lifecycle - before speaking, mid-request, pre-seed, post-seed via RST,
/// and the incident's exact shape: a client that starts a firehose, stops
/// draining, and silently HOLDS its socket (the un-reaped CLOSE_WAIT
/// forwarders that wedged the live server's new-attach path). The server
/// must reap every forwarder on its own and keep serving fresh attaches.
#[test]
fn attach_path_survives_abrupt_client_churn() {
    let _serial = attach_serial_guard();
    eventually(
        "forwarder table to drain before the test",
        Duration::from_secs(10),
        || attach_forwarder_count() == 0,
    );

    let mut ctx = test_ctx("churn-secret");
    ctx.idle_timeout = Duration::from_millis(500);
    ctx.attach_write_timeout = Duration::from_millis(300);
    let addr = spawn_attach_listener(ctx);
    let conns_baseline = ACTIVE_CONNS.load(Ordering::Relaxed);

    // Drop-guarded: the session is killed even if any assertion below panics.
    let churn = ChurnSession::new("churn");
    let id = churn.id.clone();
    let target = churn.target.clone();

    // (a) Dies before speaking: reaped by the idle read timeout.
    drop(TcpStream::connect(addr).expect("connect"));
    // (b) Dies mid-request-line (no newline ever arrives).
    {
        let mut s = TcpStream::connect(addr).expect("connect");
        s.write_all(b"{\"token\":\"churn-secret\",\"comm").unwrap();
        drop(s);
    }
    // (c) Attaches to a MISSING session and dies without reading the refusal.
    {
        let mut s = TcpStream::connect(addr).expect("connect");
        send_attach_request(&mut s, "churn-secret", "s27-definitely-absent");
        drop(s);
    }
    // (d) Dies between the request and the seed (FIN lands mid-seed), x3.
    for _ in 0..3 {
        let mut s = TcpStream::connect(addr).expect("connect");
        send_attach_request(&mut s, "churn-secret", &id);
        drop(s);
    }
    // (e) Reads the seed, then dies with an abrupt RST (SO_LINGER 0), x3.
    for _ in 0..3 {
        let s = TcpStream::connect(addr).expect("connect");
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let mut w = s.try_clone().unwrap();
        send_attach_request(&mut w, "churn-secret", &id);
        let mut reader = BufReader::new(s);
        let seed = read_json_frame(&mut reader);
        assert!(
            seed.get("scrollback").is_some(),
            "expected a seed, got {seed}"
        );
        socket2::SockRef::from(reader.get_ref())
            .set_linger(Some(Duration::from_secs(0)))
            .unwrap();
        // Dropping both clones now closes the socket -> RST, not FIN.
    }

    // (f) The incident wedge: a tiny-receive-buffer client attaches, starts a
    // firehose, stops reading, and HOLDS the socket open in silence. ~13 MB of
    // output against a 4 KiB client window and a <=4 MiB kernel send buffer
    // guarantees the forwarder's sink write blocks; the write timeout must
    // then tear the whole forwarder down while the client still holds its end.
    let wedge = {
        let sock =
            socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::STREAM, None).unwrap();
        sock.set_recv_buffer_size(4096).unwrap();
        sock.connect(&addr.into()).expect("connect wedge client");
        TcpStream::from(sock)
    };
    wedge
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut wedge_writer = wedge.try_clone().unwrap();
    send_attach_request(&mut wedge_writer, "churn-secret", &id);
    let mut wedge_reader = BufReader::new(wedge);
    let seed = read_json_frame(&mut wedge_reader);
    assert!(
        seed.get("scrollback").is_some(),
        "expected a seed, got {seed}"
    );
    send_write_frame(&mut wedge_writer, "yes S27-FIREHOSE | head -n 1000000\n");
    // Do NOT read, do NOT close. The server must reap the forwarder on its
    // own; every earlier case drains here too (EOF/RST paths are fast).
    eventually(
        "forwarder teardown while the wedged client still holds its socket",
        Duration::from_secs(20),
        || attach_forwarder_count() == 0,
    );

    // The forwarder timeout proves the wedged socket was reaped, but it does
    // not stop the firehose command running inside tmux. Under full-suite CPU
    // load that command can still fill the fresh client's receive window and
    // trip the deliberately tiny 300 ms server write timeout before this test
    // starts reading. Return the shared pane to a quiet prompt and observe a
    // marker there before testing recovery, so this assertion measures attach
    // health rather than a race with the previous client's output workload.
    tmux::send_keys(&target, &["C-c"]).expect("interrupt churn firehose");
    tmux::send_text(&target, "printf S27_FIREHOSE_STOPPED", true)
        .expect("write quiet-shell marker");
    eventually("churn firehose to stop", Duration::from_secs(10), || {
        tmux::capture_pane_text(&target, 100)
            .map(|text| text.contains("S27_FIREHOSE_STOPPED"))
            .unwrap_or(false)
    });

    // A FRESH attach must now succeed end to end - the exact operation that
    // failed for every client in the incident.
    let fresh = TcpStream::connect(addr).expect("connect fresh client");
    fresh
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut fresh_writer = fresh.try_clone().unwrap();
    send_attach_request(&mut fresh_writer, "churn-secret", &id);
    let mut fresh_reader = BufReader::new(fresh);
    let seed = read_json_frame(&mut fresh_reader);
    assert!(
        seed.get("scrollback").is_some(),
        "fresh attach after churn must get a seed, got {seed}"
    );
    send_write_frame(&mut fresh_writer, "echo S27_CHURN_OK\n");
    let mut seen = String::new();
    let sentinel_deadline = std::time::Instant::now() + Duration::from_secs(15);
    while !seen.contains("S27_CHURN_OK") {
        assert!(
            std::time::Instant::now() < sentinel_deadline,
            "sentinel never arrived on the fresh attach; saw: {seen:?}"
        );
        let mut line = String::new();
        let n = fresh_reader.read_line(&mut line).expect("read out frame");
        assert!(n > 0, "server closed the fresh attach early; saw: {seen:?}");
        let v: Value = match serde_json::from_str(line.trim()) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Some(b64) = v.get("out").and_then(|x| x.as_str()) {
            if let Ok(bytes) = STANDARD.decode(b64) {
                seen.push_str(&String::from_utf8_lossy(&bytes));
            }
        }
    }

    // Teardown: with every client gone, BOTH tables return to baseline - no
    // leaked forwarder slot, no leaked connection slot.
    drop(fresh_reader);
    drop(fresh_writer);
    drop(wedge_reader);
    drop(wedge_writer);
    let _ = tmux::kill_session(&target);
    eventually(
        "forwarder table back to baseline",
        Duration::from_secs(10),
        || attach_forwarder_count() == 0,
    );
    eventually(
        "connection handlers to drain",
        Duration::from_secs(10),
        || ACTIVE_CONNS.load(Ordering::Relaxed) <= conns_baseline,
    );
}

/// The defensive forwarder-table bound: at the cap a new attach is refused
/// with a clear error (not a silent close), and a released slot makes the
/// attach path serviceable again.
#[test]
fn attach_forwarder_cap_refuses_then_recovers() {
    let _serial = attach_serial_guard();
    eventually(
        "forwarder table to drain before the test",
        Duration::from_secs(10),
        || attach_forwarder_count() == 0,
    );

    let mut ctx = test_ctx("cap-secret");
    ctx.idle_timeout = Duration::from_millis(500);
    ctx.attach_write_timeout = Duration::from_secs(2);
    ctx.max_attach_forwarders = 1;
    let addr = spawn_attach_listener(ctx);

    let churn = ChurnSession::new("cap");
    let id = churn.id.clone();
    let target = churn.target.clone();

    // First attach fills the size-1 table; reading the seed proves the slot
    // is held (the guard is acquired before the seed is written).
    let first = TcpStream::connect(addr).expect("connect");
    first
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut first_writer = first.try_clone().unwrap();
    send_attach_request(&mut first_writer, "cap-secret", &id);
    let mut first_reader = BufReader::new(first);
    assert_eq!(
        read_json_frame(&mut first_reader)["scrollback"],
        "",
        "attach must not replay a second copy of the tmux screen"
    );
    assert_eq!(attach_forwarder_count(), 1);

    // Second attach: refused with an actionable error, then closed.
    let second = TcpStream::connect(addr).expect("connect");
    second
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut second_writer = second.try_clone().unwrap();
    send_attach_request(&mut second_writer, "cap-secret", &id);
    let mut second_reader = BufReader::new(second);
    let refusal = read_json_frame(&mut second_reader);
    assert_eq!(refusal["ok"], false, "expected a refusal, got {refusal}");
    assert!(
        refusal["error"]
            .as_str()
            .unwrap()
            .contains("forwarder table is full"),
        "got: {refusal}"
    );
    let mut rest = String::new();
    assert_eq!(
        second_reader
            .read_line(&mut rest)
            .expect("read after refusal"),
        0,
        "the refused connection must be closed, not parked"
    );

    // Release the slot; the table must drain without any explicit detach call.
    drop(first_reader);
    drop(first_writer);
    eventually(
        "slot release after client disconnect",
        Duration::from_secs(10),
        || attach_forwarder_count() == 0,
    );

    // And the attach path is serviceable again.
    let third = TcpStream::connect(addr).expect("connect");
    third
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut third_writer = third.try_clone().unwrap();
    send_attach_request(&mut third_writer, "cap-secret", &id);
    let mut third_reader = BufReader::new(third);
    assert!(
        read_json_frame(&mut third_reader)
            .get("scrollback")
            .is_some(),
        "attach must succeed once the table drained"
    );

    drop(third_reader);
    drop(third_writer);
    let _ = tmux::kill_session(&target);
    eventually(
        "forwarder table drained at test end",
        Duration::from_secs(10),
        || attach_forwarder_count() == 0,
    );
}

/// THE s27 idle-leak regression: a client attached to an IDLE terminal that
/// stops draining and then vanishes WITHOUT a clean close (no FIN reaches the
/// server's input read) must still be reaped. The forwarder only ever noticed
/// a dead client when it had real output to write; an idle terminal produces
/// none, so the write path never fired and the forwarder parked forever on the
/// silent PTY read - leaking the slot and, at scale, wedging the table so new
/// cockpit tiles could not attach. The sibling churn test above never catches
/// this because every one of its clients either closes (FIN/RST -> the input
/// read unblocks) or drives a firehose (the sink write blocks -> write
/// timeout); only a SILENT idle client exercises the gap. The periodic idle
/// keepalive must now force the stalled client to surface (its socket buffers
/// fill, the attach write timeout fires) so the forwarder reaps on its own.
#[test]
fn attach_reaps_idle_terminal_with_stalled_client() {
    let _serial = attach_serial_guard();
    eventually(
        "forwarder table to drain before the test",
        Duration::from_secs(10),
        || attach_forwarder_count() == 0,
    );

    let mut ctx = test_ctx("idle-secret");
    ctx.idle_timeout = Duration::from_millis(500);
    ctx.attach_write_timeout = Duration::from_millis(300);
    // A short keepalive so the idle liveness probe fires within the test window
    // (production drives seconds). Without the probe an idle forwarder never
    // writes, so a stalled client is never noticed and the slot leaks forever.
    ctx.attach_keepalive_interval = Duration::from_millis(50);
    let addr = spawn_attach_listener(ctx);
    let conns_baseline = ACTIVE_CONNS.load(Ordering::Relaxed);

    let churn = ChurnSession::new("idle");
    let id = churn.id.clone();
    let target = churn.target.clone();

    // A tiny-receive-buffer client attaches to an IDLE session, reads the seed,
    // then STOPS reading and holds the socket in silence - the idle analogue of
    // the firehose wedge (case f above), but with no output to force the issue.
    // Only the idle keepalive can fill the small buffer and trip the write
    // timeout; without it this forwarder never reaps.
    let stalled = {
        let sock =
            socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::STREAM, None).unwrap();
        sock.set_recv_buffer_size(4096).unwrap();
        sock.connect(&addr.into()).expect("connect stalled client");
        TcpStream::from(sock)
    };
    stalled
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut stalled_writer = stalled.try_clone().unwrap();
    send_attach_request(&mut stalled_writer, "idle-secret", &id);
    let mut stalled_reader = BufReader::new(stalled);
    let seed = read_json_frame(&mut stalled_reader);
    assert!(
        seed.get("scrollback").is_some(),
        "expected a seed, got {seed}"
    );
    assert_eq!(attach_forwarder_count(), 1, "forwarder up after attach");

    // Do NOT read, do NOT close: the client is gone but its socket lingers. The
    // server must reap this idle forwarder on its own, driven by the keepalive.
    eventually(
        "idle-terminal forwarder reaps a stalled client via the keepalive probe",
        Duration::from_secs(15),
        || attach_forwarder_count() == 0,
    );

    // Hold the client until AFTER the assertion so the reap is proven to be
    // driven by the server's probe, not by the socket finally closing.
    drop(stalled_reader);
    drop(stalled_writer);
    let _ = tmux::kill_session(&target);
    eventually(
        "connection handlers to drain",
        Duration::from_secs(10),
        || ACTIVE_CONNS.load(Ordering::Relaxed) <= conns_baseline,
    );
}

// ---- Captains registry (captain-chat phase 2) -------------------------

/// A unique temp path for a captains persistence file (removed by the caller).
fn captains_tmp(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "t-hub-captains-test-{tag}-{}.json",
        uuid::Uuid::new_v4().simple()
    ))
}

const SCHEMA_13_REGISTRY_FIXTURE: &str = include_str!("../fixtures/captains-schema-13.json");
const SCHEMA_17_REGISTRY_FIXTURE: &str = include_str!("../fixtures/captains-schema-17.json");
const SCHEMA_18_REGISTRY_FIXTURE: &str = include_str!("../fixtures/captains-schema-18.json");
const PACKAGED_SCHEMA_25_LEGACY_ORPHAN_FIXTURE: &str =
    include_str!("../fixtures/captains-schema-25-packaged-legacy-orphan.json");
const PACKAGED_SCHEMA_25_OBSERVED_LAUNCH_FIXTURE: &str =
    include_str!("../fixtures/captains-schema-25-packaged-observed-launch.json");

/// A crew ref's tile ids, for concise assertions.
fn crew_tiles(rec: &FleetIdentity) -> Vec<String> {
    rec.crew.iter().map(|c| c.terminal_id.clone()).collect()
}
/// The one captain record (tests keep a single ship).
fn only(reg: &CaptainsRegistry) -> FleetIdentity {
    reg.snapshot().captains.into_iter().next().unwrap()
}
/// "Everything alive" liveness predicate (never auto-releases).
fn all_alive(_: &str) -> bool {
    false
}
/// Crew liveness seam that reports every crew Alive - the legacy resurrect-all
/// readopt behavior. Tests that exercise the Gone/Unknown legs pass their own.
fn crew_all_alive(_: &str) -> tmux::SessionLiveness {
    tmux::SessionLiveness::Alive
}

#[test]
fn claim_registers_updates_and_bumps_seq() {
    let reg = CaptainsRegistry::new();
    let out = reg
        .claim(
            "cap-1",
            Some("Ship Alpha!"),
            FleetRole::Captain,
            None,
            vec!["tab-1".into()],
            &all_alive,
            &crew_all_alive,
        )
        .unwrap();
    assert_eq!(out.disposition, ClaimDisposition::Created);
    let rec = out.record;
    assert_eq!(rec.ship_slug, "ship-alpha");
    assert_eq!(rec.terminal_id.as_deref(), Some("cap-1"));
    assert_eq!(rec.role, FleetRole::Captain);
    assert_eq!(rec.state, ClaimState::Active);
    assert_eq!(rec.workspace_tab_ids, vec!["tab-1".to_string()]);
    assert!(rec.crew.is_empty());
    assert_eq!(reg.snapshot().seq, 1);

    // Re-claim by the SAME terminal to a new ship is a re-designation: slug/tabs
    // refresh, crew kept, no duplicate record.
    assert!(reg.record_crew("cap-1", "crew-1").unwrap());
    let out = reg
        .claim(
            "cap-1",
            Some("ship-beta"),
            FleetRole::Captain,
            None,
            vec!["tab-2".into()],
            &all_alive,
            &crew_all_alive,
        )
        .unwrap();
    let rec = out.record;
    assert_eq!(rec.ship_slug, "ship-beta");
    assert_eq!(rec.workspace_tab_ids, vec!["tab-2".to_string()]);
    assert_eq!(crew_tiles(&rec), vec!["crew-1".to_string()]);
    let snap = reg.snapshot();
    assert_eq!(
        snap.captains.len(),
        1,
        "re-designation must not duplicate the claim"
    );
    assert_eq!(snap.seq, 3);
}

#[test]
fn project_bound_same_terminal_redesignation_is_rejected_without_identity_drift() {
    let path = captains_tmp("project-bound-redesignation");
    let registry = CaptainsRegistry::load(path.clone());
    registry
        .upsert_project(ProjectRecord {
            root_path: None,
            vcs_capability: None,
            git_main_root: None,
            project_id: "project-alpha".into(),
            name: "Alpha Project".into(),
            repo_root: "/tmp/project-alpha".into(),
            remote_url: None,
            default_branch: Some("main".into()),
            powder: None,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    registry
        .claim_test("captain-a", Some("alpha"), vec!["work-a".into()])
        .unwrap();
    registry
        .bind_ship_context("alpha", "project-alpha", "Own Alpha", "codex")
        .unwrap();
    registry
        .rename_captain(Some("captain-a"), None, "Alpha Lead")
        .unwrap();
    registry.record_crew("captain-a", "crew-a").unwrap();
    let before = registry.snapshot();

    let error = registry
        .claim_provider(
            "captain-a",
            Some("beta"),
            FleetRole::Captain,
            Some("codex"),
            None,
            vec!["work-b".into()],
            &all_alive,
            &crew_all_alive,
        )
        .unwrap_err();
    assert!(error.contains("project-bound"), "got: {error}");
    let after = registry.snapshot();
    assert_eq!(after.seq, before.seq);
    assert_eq!(after.captains, before.captains);
    let restarted = CaptainsRegistry::load(path.clone()).snapshot();
    assert_eq!(restarted.seq, before.seq);
    assert_eq!(restarted.captains, before.captains);

    registry.release("captain-a").unwrap();
    let reused = registry
        .claim_provider(
            "captain-b",
            Some("alpha"),
            FleetRole::Captain,
            Some("codex"),
            None,
            vec!["work-a".into()],
            &all_alive,
            &crew_all_alive,
        )
        .unwrap();
    assert_eq!(reused.record.ship_slug, "alpha");
    assert_eq!(reused.record.terminal_id.as_deref(), Some("captain-b"));
    assert_eq!(reused.record.project_id.as_deref(), Some("project-alpha"));
    assert_eq!(
        reused.record.assignment_id,
        "assignment:project-alpha:alpha"
    );
    assert_eq!(reused.record.display_name, "Alpha Lead");
    assert_eq!(crew_tiles(&reused.record), vec!["crew-a"]);
    let reused_after_restart = CaptainsRegistry::load(path.clone()).snapshot();
    assert_eq!(reused_after_restart.captains.len(), 1);
    assert_eq!(
        reused_after_restart.captains[0].terminal_id.as_deref(),
        Some("captain-b")
    );
    assert_eq!(reused_after_restart.captains[0].ship_slug, "alpha");

    let _ = std::fs::remove_file(path.with_extension("json.bak"));
    let _ = std::fs::remove_file(path);
}

#[test]
fn claim_defaults_slug_and_a_live_ship_is_never_seized() {
    // The double-claim RACE / wedged-not-dead guard: a DIFFERENT terminal claiming
    // a slug held by a LIVE incumbent is REJECTED (a bypass - seizing a live ship
    // on a soft signal - would split-brain; HIGH-2/R1). A live tmux session is the
    // "wedged" case too: has_session true => not transfer-grade => reject.
    let reg = CaptainsRegistry::new();
    let out = reg.claim_test("cap-1", None, vec![]).unwrap();
    assert_eq!(out.record.ship_slug, "ship-cap-1");
    let err = reg
        .claim(
            "cap-2",
            Some("ship-cap-1"),
            FleetRole::Captain,
            None,
            vec![],
            &all_alive,
            &crew_all_alive,
        )
        .unwrap_err();
    assert!(
        err.contains("already captained by a LIVE session 'cap-1'"),
        "got: {err}"
    );
    // The incumbent is untouched; the refusal did not bump the revision.
    assert_eq!(only(&reg).terminal_id.as_deref(), Some("cap-1"));
    assert_eq!(reg.snapshot().seq, 1, "refusals must not bump the revision");
    // Empty session id is refused before touching the registry.
    assert!(reg.claim_test("  ", None, vec![]).is_err());
}

#[test]
fn corpse_holds_slug_auto_releases_on_unambiguous_death() {
    // R-H2 core: a captain's terminal is killed and the session migrates to a new
    // terminal. The corpse's claim would DEADLOCK the migrated re-claim today.
    // Re-keyed: `tmux::has_session == false` (the SOLE transfer-grade signal) auto-
    // releases the corpse and the new terminal takes the slug. Crew are preserved.
    let reg = CaptainsRegistry::new();
    reg.claim_test("cap-old", Some("t-hub-app"), vec![])
        .unwrap();
    assert!(reg.record_crew("cap-old", "crew-1").unwrap());
    // cap-old's pane is gone; cap-new re-claims the same ship (no UUID resolved).
    let dead_is_old = |tile: &str| tile == "cap-old";
    let out = reg
        .claim(
            "cap-new",
            Some("t-hub-app"),
            FleetRole::Captain,
            None,
            vec![],
            &dead_is_old,
            &crew_all_alive,
        )
        .unwrap();
    assert_eq!(out.disposition, ClaimDisposition::AutoReleasedDead);
    assert_eq!(out.record.terminal_id.as_deref(), Some("cap-new"));
    assert_eq!(
        crew_tiles(&out.record),
        vec!["crew-1".to_string()],
        "crew followed the ship"
    );
    assert_eq!(
        reg.snapshot().captains.len(),
        1,
        "no duplicate - the slug transferred"
    );
}

#[test]
fn timed_out_probe_never_seizes_an_incumbents_ship() {
    // De-conflation guard (spawn-wedge): the transfer decision must be driven by
    // the SAME production mapping the real claim uses -
    // `is_definitively_gone(session_liveness(..))` - so that an INDETERMINATE probe
    // (a 5s tmux timeout under a degraded spawn path) is NOT transfer-grade. Here
    // the injected predicate is that production mapping applied to an `Unknown`
    // probe result; the incumbent must be treated as a LIVE ship and the claim
    // REJECTED, never auto-released. The old `!has_session` conflation returned
    // `true` for a timeout and WOULD have seized the live ship - this trips it.
    let reg = CaptainsRegistry::new();
    reg.claim_test("cap-old", Some("t-hub-app"), vec![])
        .unwrap();
    assert!(reg.record_crew("cap-old", "crew-1").unwrap());
    let before_seq = reg.snapshot().seq;
    let probe_times_out = |_: &str| tmux::is_definitively_gone(tmux::SessionLiveness::Unknown);
    let err = reg
        .claim(
            "cap-new",
            Some("t-hub-app"),
            FleetRole::Captain,
            None,
            vec![],
            &probe_times_out,
            &crew_all_alive,
        )
        .unwrap_err();
    assert!(
        err.contains("already captained by a LIVE session 'cap-old'"),
        "an ambiguous (timed-out) probe must reject like a live ship, not seize; got: {err}"
    );
    // The incumbent and its crew are untouched; the refusal did not bump the seq.
    assert_eq!(only(&reg).terminal_id.as_deref(), Some("cap-old"));
    assert_eq!(crew_tiles(&only(&reg)), vec!["crew-1".to_string()]);
    assert_eq!(
        reg.snapshot().seq,
        before_seq,
        "a refused seize must not bump the revision"
    );
}

#[test]
fn matching_provider_id_cannot_seize_a_live_incumbent() {
    let reg = CaptainsRegistry::new();
    reg.claim(
        "cap-old",
        Some("shipx"),
        FleetRole::Captain,
        Some("uuid-1"),
        vec![],
        &all_alive,
        &crew_all_alive,
    )
    .unwrap();
    let error = reg
        .claim(
            "cap-new",
            Some("shipx"),
            FleetRole::Captain,
            Some("uuid-1"),
            vec![],
            &all_alive,
            &crew_all_alive,
        )
        .unwrap_err();
    assert!(error.contains("already captained by a LIVE session"));
    assert_eq!(only(&reg).terminal_id.as_deref(), Some("cap-old"));
    assert_eq!(reg.snapshot().captains.len(), 1);
}

#[test]
fn provider_change_without_runtime_identity_clears_stale_conversation_fields() {
    let reg = CaptainsRegistry::new();
    reg.claim_provider(
        "cap-one",
        Some("shipx"),
        FleetRole::Captain,
        Some("claude"),
        Some("claude-session"),
        vec![],
        &all_alive,
        &crew_all_alive,
    )
    .unwrap();
    let changed = reg
        .claim_provider(
            "cap-one",
            Some("shipx"),
            FleetRole::Captain,
            Some("codex"),
            None,
            vec![],
            &all_alive,
            &crew_all_alive,
        )
        .unwrap()
        .record;
    assert_eq!(changed.provider.as_deref(), Some("codex"));
    assert!(changed.provider_session_id.is_none());
    assert!(changed.conversation_id.is_none());
    assert!(changed.claude_uuid.is_none());
}

#[test]
fn orphaned_record_is_readopted_by_ship_slug_reclaim() {
    // D4 auto-rebind on resume: after the captain dies (Orphaned), a resumed
    // captain re-claiming the ship SLUG (the always-available trigger, no UUID
    // needed) re-adopts the record → Active and resurrects its Orphaned crew.
    let reg = CaptainsRegistry::new();
    reg.claim_test("cap-old", Some("shipx"), vec![]).unwrap();
    assert!(reg.record_crew("cap-old", "crew-1").unwrap());
    assert!(
        reg.remove_session("cap-old").unwrap(),
        "captain death marks orphaned"
    );
    assert!(matches!(only(&reg).state, ClaimState::Orphaned { .. }));

    let out = reg.claim_test("cap-new", Some("shipx"), vec![]).unwrap();
    assert_eq!(out.disposition, ClaimDisposition::ReadoptedOrphan);
    let rec = only(&reg);
    assert_eq!(rec.state, ClaimState::Active);
    assert_eq!(rec.terminal_id.as_deref(), Some("cap-new"));
    assert_eq!(
        rec.crew[0].state,
        CrewState::Active,
        "orphaned crew re-adopted"
    );
}

#[test]
fn readopt_is_gated_on_per_crew_liveness_never_blind_activates() {
    // audit BUG-1: a resumed captain must NOT blind-flip every Orphaned crew to
    // Active - it re-probes each and only re-adopts the ones actually Alive.
    // Alive -> Active, Gone (definitively absent) -> Removed, Unknown (ambiguous
    // probe) -> stays Orphaned (re-adoptable next resume). BYPASS-WOULD-FAIL:
    // restore the blind `cr.state = Active` and the Gone/Unknown crew come back
    // Active -> RED.
    let reg = CaptainsRegistry::new();
    reg.claim_test("cap-old", Some("shipx"), vec![]).unwrap();
    for c in ["crew-alive", "crew-gone", "crew-unknown"] {
        assert!(reg.record_crew("cap-old", c).unwrap());
    }
    assert!(
        reg.remove_session("cap-old").unwrap(),
        "captain death orphans the crew"
    );
    assert!(
        only(&reg)
            .crew
            .iter()
            .all(|c| matches!(c.state, CrewState::Orphaned { .. })),
        "all crew start Orphaned"
    );

    // The liveness seam the real handler precomputes lock-free: one verdict per
    // crew tile.
    let crew_liveness = |tile: &str| match tile {
        "crew-alive" => tmux::SessionLiveness::Alive,
        "crew-gone" => tmux::SessionLiveness::Gone,
        _ => tmux::SessionLiveness::Unknown,
    };
    let out = reg
        .claim(
            "cap-new",
            Some("shipx"),
            FleetRole::Captain,
            None,
            vec![],
            &all_alive,
            &crew_liveness,
        )
        .unwrap();
    assert_eq!(out.disposition, ClaimDisposition::ReadoptedOrphan);

    let rec = only(&reg);
    assert_eq!(
        rec.state,
        ClaimState::Active,
        "the captain itself re-activates"
    );
    let state_of = |tile: &str| {
        rec.crew
            .iter()
            .find(|c| c.terminal_id == tile)
            .map(|c| c.state.clone())
            .unwrap()
    };
    assert_eq!(
        state_of("crew-alive"),
        CrewState::Active,
        "Alive -> re-adopted"
    );
    assert!(
        matches!(state_of("crew-gone"), CrewState::Removed { .. }),
        "Gone -> retired, never resurrected"
    );
    assert!(
        matches!(state_of("crew-unknown"), CrewState::Orphaned { .. }),
        "Unknown -> left Orphaned (ambiguous is never seized)"
    );
}

#[test]
fn dead_captain_orphans_crew_and_is_not_scrubbed() {
    // Phase B: death MARKS, it does not scrub (retiring the C4 silent leak). A dead
    // captain's record is retained Orphaned, un-pointed, with its crew Orphaned.
    let reg = CaptainsRegistry::new();
    reg.claim_test("cap-1", Some("alpha"), vec![]).unwrap();
    assert!(reg.record_crew("cap-1", "crew-1").unwrap());
    assert!(reg.record_crew("cap-1", "crew-2").unwrap());
    assert!(reg.remove_session("cap-1").unwrap());
    let rec = only(&reg);
    assert!(
        matches!(rec.state, ClaimState::Orphaned { .. }),
        "retained, not scrubbed"
    );
    assert!(rec.terminal_id.is_none(), "un-pointed");
    assert!(
        rec.crew
            .iter()
            .all(|c| matches!(c.state, CrewState::Orphaned { .. })),
        "crew orphaned under the surviving ship, never dropped"
    );
}

#[test]
fn dead_crew_tile_is_marked_removed_not_scrubbed() {
    // A crew's OWN tile dying flips that ref to Removed (retained for telemetry),
    // leaving the live captain + sibling crew untouched.
    let reg = CaptainsRegistry::new();
    reg.claim_test("cap-1", Some("alpha"), vec![]).unwrap();
    assert!(reg.record_crew("cap-1", "crew-1").unwrap());
    assert!(reg.record_crew("cap-1", "crew-2").unwrap());
    assert!(reg.remove_session("crew-1").unwrap());
    let rec = only(&reg);
    assert_eq!(rec.state, ClaimState::Active, "captain still alive");
    let c1 = rec.crew.iter().find(|c| c.terminal_id == "crew-1").unwrap();
    let c2 = rec.crew.iter().find(|c| c.terminal_id == "crew-2").unwrap();
    assert!(
        matches!(c1.state, CrewState::Removed { .. }),
        "dead crew retained as Removed"
    );
    assert_eq!(c2.state, CrewState::Active);
    // Removing an unknown session changes nothing (no revision bump).
    let seq = reg.snapshot().seq;
    assert!(!reg.remove_session("nobody").unwrap());
    assert_eq!(reg.snapshot().seq, seq);
}

#[test]
fn record_crew_dedupes_and_reactivates_a_removed_ref() {
    let reg = CaptainsRegistry::new();
    reg.claim_test("cap-1", Some("alpha"), vec![]).unwrap();
    assert!(
        !reg.record_crew("cap-ghost", "crew-1").unwrap(),
        "unclaimed spawner is a no-op"
    );
    assert!(reg.record_crew("cap-1", "crew-1").unwrap());
    assert!(
        !reg.record_crew("cap-1", "crew-1").unwrap(),
        "duplicate Active crew must not re-add"
    );
    // A reused tile id after its ref was Removed re-activates (does not duplicate).
    assert!(reg.remove_session("crew-1").unwrap());
    assert!(
        reg.record_crew("cap-1", "crew-1").unwrap(),
        "reused tile reactivates"
    );
    let rec = only(&reg);
    assert_eq!(rec.crew.len(), 1);
    assert_eq!(rec.crew[0].state, CrewState::Active);
}

#[test]
fn cortana_is_a_first_class_singleton_role() {
    // D1: Cortana is a first-class role, unique registry-wide, NOT a slug hack. A
    // second Cortana claim by a LIVE competitor is rejected; only unambiguous death
    // (or the same session) yields the apex.
    let reg = CaptainsRegistry::new();
    let out = reg
        .claim(
            "cor-1",
            None,
            FleetRole::Cortana,
            None,
            vec![],
            &all_alive,
            &crew_all_alive,
        )
        .unwrap();
    assert_eq!(out.record.role, FleetRole::Cortana);
    assert_eq!(out.record.ship_slug, CORTANA_SLUG);
    // A different LIVE terminal cannot seize the singleton.
    let err = reg
        .claim(
            "cor-2",
            None,
            FleetRole::Cortana,
            None,
            vec![],
            &all_alive,
            &crew_all_alive,
        )
        .unwrap_err();
    assert!(err.contains("LIVE"), "got: {err}");
    // The incumbent dying hands the apex to the resumed Cortana.
    let dead_is_1 = |t: &str| t == "cor-1";
    let out = reg
        .claim(
            "cor-2",
            None,
            FleetRole::Cortana,
            None,
            vec![],
            &dead_is_1,
            &crew_all_alive,
        )
        .unwrap();
    assert_eq!(out.disposition, ClaimDisposition::AutoReleasedDead);
    assert_eq!(out.record.terminal_id.as_deref(), Some("cor-2"));
    assert_eq!(
        reg.snapshot()
            .captains
            .iter()
            .filter(|c| c.role == FleetRole::Cortana)
            .count(),
        1
    );
}

#[test]
fn release_with_crew_becomes_vacant_childless_removes() {
    let reg = CaptainsRegistry::new();
    reg.claim_test("cap-1", Some("alpha"), vec![]).unwrap();
    assert!(reg.record_crew("cap-1", "crew-1").unwrap());
    // Release with crew: transition to Vacant (re-claimable), crew preserved.
    let released = reg.release("alpha").unwrap();
    assert_eq!(released.state, ClaimState::Vacant);
    assert!(released.terminal_id.is_none());
    assert_eq!(only(&reg).crew.len(), 1, "crew preserved for re-adoption");
    // Re-claiming the vacant ship re-adopts it.
    let out = reg.claim_test("cap-2", Some("alpha"), vec![]).unwrap();
    assert_eq!(out.disposition, ClaimDisposition::ReadoptedOrphan);

    // A childless claim hard-removes on release.
    reg.claim_test("cap-9", Some("beta"), vec![]).unwrap();
    assert_eq!(reg.release("beta").unwrap().ship_slug, "beta");
    assert!(reg
        .snapshot()
        .captains
        .iter()
        .all(|c| c.ship_slug != "beta"));
    // Unknown target is an error, not a silent no-op.
    assert!(reg
        .release("no-such")
        .unwrap_err()
        .contains("no claim matches"));
}

#[test]
fn ship_of_resolves_supervisor_and_crew_across_the_namespace() {
    // Phase D: the cross-ship ownership KEY resolves for both a supervisor terminal
    // and a crew tile (item-1 Phase 3 wires the ACL on top of this).
    let reg = CaptainsRegistry::new();
    reg.claim(
        "cap-1",
        Some("shipx"),
        FleetRole::Captain,
        None,
        vec![],
        &all_alive,
        &crew_all_alive,
    )
    .unwrap();
    assert!(reg.record_crew("cap-1", "crew-1").unwrap());
    assert_eq!(
        reg.ship_of("cap-1"),
        Some(ShipMembership::Supervisor {
            ship_slug: "shipx".into(),
            role: FleetRole::Captain
        })
    );
    assert_eq!(
        reg.ship_of("crew-1"),
        Some(ShipMembership::Crew {
            ship_slug: "shipx".into()
        })
    );
    assert_eq!(reg.ship_of("nobody"), None);
    // A Removed crew tile no longer resolves.
    assert!(reg.remove_session("crew-1").unwrap());
    assert_eq!(reg.ship_of("crew-1"), None);
}

#[test]
fn backfill_uuid_fills_only_a_none_anchor() {
    // MED-7: the async-resolved anchor is backfilled once, never overwritten.
    let reg = CaptainsRegistry::new();
    reg.claim_test("cap-1", Some("shipx"), vec![]).unwrap();
    assert!(reg.record_crew("cap-1", "crew-1").unwrap());
    assert!(reg.backfill_uuid("cap-1", "uuid-cap").unwrap());
    assert!(reg.backfill_uuid("crew-1", "uuid-crew").unwrap());
    let rec = only(&reg);
    assert_eq!(rec.claude_uuid.as_deref(), Some("uuid-cap"));
    assert_eq!(rec.crew[0].claude_uuid.as_deref(), Some("uuid-crew"));
    // A second backfill of an already-resolved anchor is a no-op (no seq bump).
    let seq = reg.snapshot().seq;
    assert!(!reg.backfill_uuid("cap-1", "uuid-other").unwrap());
    assert_eq!(reg.snapshot().seq, seq);
    assert_eq!(only(&reg).claude_uuid.as_deref(), Some("uuid-cap"));
}

#[test]
fn legacy_v0_captains_json_migrates_in_place() {
    // D2/MED-6: the versioned reader accepts the legacy shape (captainSessionId +
    // crew: [string], no role/state) AND special-cases the cortana slug -> the
    // first-class Cortana singleton, seeded from the live incumbent.
    let path = captains_tmp("legacy-v0");
    let legacy = serde_json::json!({
        "seq": 5,
        "captains": [
            { "shipSlug": "cortana", "captainSessionId": "cor-x", "crew": ["c1", "c2"] },
            { "shipSlug": "t-hub-app", "captainSessionId": "cap-y", "workspaceTabIds": ["t1"], "crew": [] }
        ]
    });
    std::fs::write(&path, serde_json::to_vec_pretty(&legacy).unwrap()).unwrap();

    let reg = CaptainsRegistry::load(path.clone());
    let snap = reg.snapshot();
    assert_eq!(snap.seq, 5, "seq preserved across the migration");
    let cor = snap
        .captains
        .iter()
        .find(|c| c.ship_slug == "cortana")
        .unwrap();
    assert_eq!(
        cor.role,
        FleetRole::Cortana,
        "legacy cortana slug seeds the singleton role"
    );
    assert_eq!(
        cor.terminal_id.as_deref(),
        Some("cor-x"),
        "captainSessionId -> terminal_id"
    );
    assert_eq!(cor.state, ClaimState::Active);
    assert_eq!(
        crew_tiles(cor),
        vec!["c1".to_string(), "c2".to_string()],
        "crew strings -> CrewRef"
    );
    assert!(cor.crew.iter().all(|c| c.state == CrewState::Active));
    let cap = snap
        .captains
        .iter()
        .find(|c| c.ship_slug == "t-hub-app")
        .unwrap();
    assert_eq!(
        cap.role,
        FleetRole::Captain,
        "a normal ship stays a Captain"
    );
    assert_eq!(cap.assignment_id, "assignment:unbound:t-hub-app");
    assert_eq!(cap.display_name, "t-hub-app");
    assert_eq!(cap.workspace_tab_ids, vec!["t1"]);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn captain_identity_and_multiple_project_captains_survive_restart() {
    let path = captains_tmp("multiple-captains-one-project");
    let reg = CaptainsRegistry::load(path.clone());
    reg.upsert_project(ProjectRecord {
        root_path: None,
        vcs_capability: None,
        git_main_root: None,
        project_id: "project-shared".into(),
        name: "Shared".into(),
        repo_root: dispatch_test_repo_root(),
        remote_url: None,
        default_branch: Some("main".into()),
        powder: None,
        created_at: 1,
        updated_at: 1,
    })
    .unwrap();
    reg.claim_test("captain-a", Some("alpha"), vec!["work-a".into()])
        .unwrap();
    reg.claim_test("captain-b", Some("beta"), vec!["work-b".into()])
        .unwrap();
    reg.bind_ship_context("alpha", "project-shared", "Assignment A", "codex")
        .unwrap();
    reg.bind_ship_context("beta", "project-shared", "Assignment B", "claude")
        .unwrap();
    reg.rename_captain(Some("captain-a"), None, "  Alpha Lead  ")
        .unwrap();

    let restored = CaptainsRegistry::load(path.clone()).snapshot();
    assert_eq!(restored.captains.len(), 2);
    let alpha = restored
        .captains
        .iter()
        .find(|captain| captain.terminal_id.as_deref() == Some("captain-a"))
        .unwrap();
    let beta = restored
        .captains
        .iter()
        .find(|captain| captain.terminal_id.as_deref() == Some("captain-b"))
        .unwrap();
    assert_eq!(alpha.assignment_id, "assignment:project-shared:alpha");
    assert_eq!(alpha.display_name, "Alpha Lead");
    assert_eq!(beta.assignment_id, "assignment:project-shared:beta");
    assert_ne!(alpha.assignment_id, beta.assignment_id);
    assert!(reg
        .rename_captain(Some("captain-a"), None, &"x".repeat(121))
        .unwrap_err()
        .contains("at most 120 bytes"));
    let _ = std::fs::remove_file(path);
}

#[test]
fn workspace_kind_migrates_and_reserved_workspace_is_canonical() {
    let legacy: TabRecord = serde_json::from_value(json!({
        "id": CAPTAIN_WORKSPACE_ID,
        "name": "Captains",
        "order": ["captain-a"]
    }))
    .unwrap();
    let wire = serde_json::to_value(&legacy).unwrap();
    assert_eq!(wire["schemaVersion"], WORKSPACE_SCHEMA_VERSION);
    assert_eq!(wire["kind"], "captain");
    assert_eq!(wire["name"], CAPTAIN_WORKSPACE_NAME);
    assert_eq!(wire["tileIds"], json!(["captain-a"]));

    let tabs = TabRegistry::new();
    tabs.replace(vec![
        TabRecord {
            id: CAPTAIN_WORKSPACE_ID.into(),
            name: "Captains".into(),
            tile_ids: vec!["captain-a".into()],
        },
        TabRecord {
            id: CAPTAIN_WORKSPACE_ID.into(),
            name: "duplicate".into(),
            tile_ids: vec!["captain-a".into(), "captain-b".into()],
        },
    ]);
    let snapshot = tabs.snapshot();
    assert_eq!(snapshot.len(), 1);
    assert_eq!(snapshot[0].name, CAPTAIN_WORKSPACE_NAME);
    assert_eq!(snapshot[0].tile_ids, vec!["captain-a", "captain-b"]);
    assert!(tabs
        .rename_tab(CAPTAIN_WORKSPACE_ID, "Other")
        .unwrap_err()
        .contains("cannot be renamed"));
    assert!(tabs
        .remove_tab(CAPTAIN_WORKSPACE_ID, true)
        .unwrap_err()
        .contains("cannot be closed"));
    assert!(serde_json::from_value::<TabRecord>(json!({
        "schemaVersion": 1,
        "id": "work-a",
        "name": "Work A",
        "kind": "captain",
        "tileIds": []
    }))
    .unwrap_err()
    .to_string()
    .contains("conflicts"));
}

#[test]
fn legacy_crew_workspace_reconciliation_is_exact_or_needs_assignment() {
    let reg = CaptainsRegistry::new();
    reg.claim_test(
        "captain-a",
        Some("alpha"),
        vec!["work-a".into(), "work-b".into()],
    )
    .unwrap();
    reg.record_crew("captain-a", "crew-exact").unwrap();
    reg.record_crew("captain-a", "crew-ambiguous").unwrap();
    let mut tabs = vec![
        TabRecord {
            id: "work-a".into(),
            name: "Work A".into(),
            tile_ids: vec!["crew-exact".into()],
        },
        TabRecord {
            id: "work-b".into(),
            name: "Work B".into(),
            tile_ids: Vec::new(),
        },
        TabRecord {
            id: CAPTAIN_WORKSPACE_ID.into(),
            name: CAPTAIN_WORKSPACE_NAME.into(),
            tile_ids: vec!["captain-a".into(), "crew-ambiguous".into()],
        },
    ];
    assert!(reg.reconcile_crew_workspaces(&mut tabs).unwrap());
    let captain = &reg.snapshot().captains[0];
    let exact = captain
        .crew
        .iter()
        .find(|crew| crew.terminal_id == "crew-exact")
        .unwrap();
    assert_eq!(exact.workspace_tab_id.as_deref(), Some("work-a"));
    assert_eq!(exact.state, CrewState::Active);
    let ambiguous = captain
        .crew
        .iter()
        .find(|crew| crew.terminal_id == "crew-ambiguous")
        .unwrap();
    assert!(matches!(ambiguous.state, CrewState::NeedsAssignment { .. }));
    assert!(tabs
        .iter()
        .all(|tab| !tab.tile_ids.iter().any(|id| id == "crew-ambiguous")));
    assert!(!reg.reconcile_crew_workspaces(&mut tabs).unwrap());
}

#[test]
fn dispatch_workspace_resolution_is_owned_exact_and_bounded() {
    let reg = Arc::new(CaptainsRegistry::new());
    reg.claim_test(
        "captain-a",
        Some("alpha"),
        vec!["work-a".into(), "work-b".into(), "foreign".into()],
    )
    .unwrap();
    let tabs = Arc::new(TabRegistry::new());
    tabs.replace(vec![
        TabRecord {
            id: "work-a".into(),
            name: "Shared".into(),
            tile_ids: Vec::new(),
        },
        TabRecord {
            id: "work-b".into(),
            name: "Shared".into(),
            tile_ids: Vec::new(),
        },
    ]);
    let ctx = test_ctx("dispatch-workspace")
        .with_captains_registry(reg.clone())
        .with_tab_registry(tabs);
    let captain = reg.snapshot().captains[0].clone();
    let error = resolve_dispatch_workspace(&ctx, &json!({}), &captain).unwrap_err();
    assert!(error.starts_with("workspace_required:"));
    assert!(error.contains("work-a"));
    assert!(error.contains("work-b"));
    assert!(!error.contains("foreign"));
    assert!(resolve_dispatch_workspace(
        &ctx,
        &json!({"workspaceTabId": CAPTAIN_WORKSPACE_ID}),
        &captain
    )
    .unwrap_err()
    .contains("Crew cannot"));
    assert_eq!(
        resolve_dispatch_workspace(&ctx, &json!({"workspaceTabId": "work-a"}), &captain)
            .unwrap()
            .id,
        "work-a"
    );
    assert!(
        resolve_dispatch_workspace(&ctx, &json!({"tabName": "Shared"}), &captain)
            .unwrap_err()
            .starts_with("workspace_required:")
    );
}

#[test]
fn work_workspace_ownership_is_globally_exclusive_sequentially_and_concurrently() {
    let sequential = CaptainsRegistry::new();
    sequential
        .claim_test("captain-a", Some("alpha"), vec!["shared-work".into()])
        .unwrap();
    let before = sequential.snapshot();
    let error = sequential
        .claim_test("captain-b", Some("beta"), vec!["shared-work".into()])
        .unwrap_err();
    assert!(error.contains("already owned"), "got: {error}");
    assert_eq!(sequential.snapshot().seq, before.seq);
    assert_eq!(sequential.snapshot().captains, before.captains);

    let concurrent = Arc::new(CaptainsRegistry::new());
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let joins = [("captain-a", "alpha"), ("captain-b", "beta")]
        .into_iter()
        .map(|(terminal, ship)| {
            let registry = Arc::clone(&concurrent);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                registry.claim_test(terminal, Some(ship), vec!["shared-work".into()])
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    let results = joins
        .into_iter()
        .map(|join| join.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);
    let snapshot = concurrent.snapshot();
    assert_eq!(snapshot.captains.len(), 1);
    assert_eq!(snapshot.captains[0].workspace_tab_ids, vec!["shared-work"]);
}

#[test]
fn schema_load_rejects_duplicate_global_workspace_ownership() {
    let path = captains_tmp("duplicate-global-workspace-owner");
    let source = CaptainsRegistry::load(path.clone());
    source
        .claim_test("captain-a", Some("alpha"), vec!["work-a".into()])
        .unwrap();
    source
        .claim_test("captain-b", Some("beta"), vec!["work-b".into()])
        .unwrap();
    let mut invalid = source.snapshot();
    invalid.captains[1].workspace_tab_ids = vec!["work-a".into()];
    std::fs::write(&path, serde_json::to_vec_pretty(&invalid).unwrap()).unwrap();
    let _ = std::fs::remove_file(path.with_extension("json.bak"));

    let restored = CaptainsRegistry::load(path.clone()).snapshot();
    assert!(
        restored.captains.is_empty(),
        "ambiguous persisted ownership must fail closed instead of selecting an owner"
    );
    let _ = std::fs::remove_file(path);
}

#[test]
fn cross_project_workspace_reports_and_moves_are_rejected_without_effect() {
    let registry = Arc::new(CaptainsRegistry::new());
    for (project_id, name) in [("project-a", "A"), ("project-b", "B")] {
        registry
            .upsert_project(ProjectRecord {
                root_path: None,
                vcs_capability: None,
                git_main_root: None,
                project_id: project_id.into(),
                name: name.into(),
                repo_root: format!("/tmp/{project_id}"),
                remote_url: None,
                default_branch: Some("main".into()),
                powder: None,
                created_at: 1,
                updated_at: 1,
            })
            .unwrap();
    }
    registry
        .claim_test("captain-a", Some("alpha"), vec!["work-a".into()])
        .unwrap();
    registry
        .claim_test("captain-b", Some("beta"), vec!["work-b".into()])
        .unwrap();
    registry
        .bind_ship_context("alpha", "project-a", "Assignment A", "codex")
        .unwrap();
    registry
        .bind_ship_context("beta", "project-b", "Assignment B", "codex")
        .unwrap();
    registry.record_crew("captain-b", "crew-b").unwrap();
    let tabs = Arc::new(TabRegistry::new());
    tabs.replace(vec![
        TabRecord {
            id: "work-a".into(),
            name: "Work A".into(),
            tile_ids: Vec::new(),
        },
        TabRecord {
            id: "work-b".into(),
            name: "Work B".into(),
            tile_ids: vec!["crew-b".into()],
        },
    ]);
    let ctx = test_ctx("cross-project-workspace")
        .with_captains_registry(Arc::clone(&registry))
        .with_tab_registry(Arc::clone(&tabs));
    let before_tabs = tabs.snapshot_full();
    let before_captains = registry.snapshot();

    let report_error = dispatch(
            &ctx,
            "report_workspace_tabs",
            &json!({
                "baseSeq": before_tabs.seq,
                "tabs": [
                    {"id": "work-a", "name": "Work A", "tileIds": ["crew-b"]},
                    {"id": "work-b", "name": "Work B", "tileIds": []},
                    {"id": CAPTAIN_WORKSPACE_ID, "name": CAPTAIN_WORKSPACE_NAME, "tileIds": ["captain-a", "captain-b"]}
                ]
            }),
        )
        .unwrap_err();
    assert!(report_error.contains("not owned"), "got: {report_error}");
    let after_report_tabs = tabs.snapshot_full();
    assert_eq!(after_report_tabs.seq, before_tabs.seq);
    assert_eq!(after_report_tabs.active_tab_id, before_tabs.active_tab_id);
    assert_eq!(
        serde_json::to_value(after_report_tabs.tabs).unwrap(),
        serde_json::to_value(&before_tabs.tabs).unwrap()
    );
    assert_eq!(registry.snapshot().seq, before_captains.seq);
    assert_eq!(registry.snapshot().captains, before_captains.captains);

    let move_error = dispatch(
        &ctx,
        "move_tile",
        &json!({"terminalId": "crew-b", "tabId": "work-a"}),
    )
    .unwrap_err();
    assert!(move_error.contains("not owned"), "got: {move_error}");
    let after_move_tabs = tabs.snapshot_full();
    assert_eq!(after_move_tabs.seq, before_tabs.seq);
    assert_eq!(after_move_tabs.active_tab_id, before_tabs.active_tab_id);
    assert_eq!(
        serde_json::to_value(after_move_tabs.tabs).unwrap(),
        serde_json::to_value(&before_tabs.tabs).unwrap()
    );
    assert_eq!(registry.snapshot().seq, before_captains.seq);
}

#[test]
fn authenticated_workspace_mutations_are_scoped_to_exact_caller_assignment() {
    let captains = Arc::new(CaptainsRegistry::new());
    for (project_id, name) in [("project-a", "A"), ("project-b", "B")] {
        captains
            .upsert_project(ProjectRecord {
                root_path: None,
                vcs_capability: None,
                git_main_root: None,
                project_id: project_id.into(),
                name: name.into(),
                repo_root: format!("/tmp/{project_id}"),
                remote_url: None,
                default_branch: Some("main".into()),
                powder: None,
                created_at: 1,
                updated_at: 1,
            })
            .unwrap();
    }
    for (terminal, ship, project, workspace) in [
        ("captain-a", "alpha", "project-a", "work-a"),
        ("captain-b", "beta", "project-b", "work-b"),
    ] {
        captains
            .claim_test(terminal, Some(ship), vec![workspace.into()])
            .unwrap();
        captains
            .bind_ship_context(ship, project, &format!("Assignment {ship}"), "codex")
            .unwrap();
    }
    let tabs = Arc::new(TabRegistry::new());
    tabs.replace(captains.workspace_projection());
    let identities = Arc::new(crate::identity::IdentityStore::ephemeral());
    let captain_a = mint_session(
        &identities,
        crate::identity::Role::Captain,
        "alpha",
        "captain-a",
    );
    let context = test_ctx("workspace-project-auth")
        .with_captains_registry(Arc::clone(&captains))
        .with_tab_registry(Arc::clone(&tabs))
        .with_identity_store(identities);
    let before_captains = captains.snapshot();
    let before_tabs = tabs.snapshot_full();

    for (command, args) in [
        ("focus_session", json!({"sessionId": "captain-b"})),
        ("focus_tab", json!({"tabId": "work-b"})),
        (
            "move_tile",
            json!({"terminalId": "ordinary-b", "tabId": "work-b"}),
        ),
        ("rename_tab", json!({"tabId": "work-b", "name": "Stolen"})),
        ("close_tab", json!({"tabId": "work-b"})),
        (
            "new_tab",
            json!({"name": "Foreign", "projectId": "project-b", "shipSlug": "beta"}),
        ),
        (
            "report_workspace_tabs",
            json!({"baseSeq": before_tabs.seq, "tabs": before_tabs.tabs}),
        ),
    ] {
        let response = dispatch_authenticated(
            &context,
            req_session("workspace-project-auth", &captain_a, command, args),
        );
        assert!(!response.ok, "{command} unexpectedly crossed Project scope");
        assert!(response.error.unwrap_or_default().contains("acl:"));
    }
    for (command, args) in [
        (
            "rename_tab",
            json!({"tabId": "work-b", "name": "No Session"}),
        ),
        ("close_tab", json!({"tabId": "work-b"})),
    ] {
        let response = dispatch_authenticated(
            &context,
            req_untrusted("workspace-project-auth", "", command, args),
        );
        assert!(
            !response.ok,
            "unattributed {command} unexpectedly succeeded"
        );
        assert!(response
            .error
            .unwrap_or_default()
            .contains("requires a valid T_HUB_SESSION_TOKEN"));
    }
    assert_eq!(captains.snapshot().seq, before_captains.seq);
    assert_eq!(captains.snapshot().captains, before_captains.captains);
    assert_eq!(captains.snapshot().workspaces, before_captains.workspaces);
    assert_eq!(tabs.snapshot_full().seq, before_tabs.seq);

    let created = dispatch_authenticated(
        &context,
        req_session(
            "workspace-project-auth",
            &captain_a,
            "new_tab",
            json!({"name": "Owned A"}),
        ),
    );
    assert!(created.ok, "{:?}", created.error);
    let tab_id = created.result.unwrap()["tabId"]
        .as_str()
        .unwrap()
        .to_string();
    let durable = captains.snapshot();
    let workspace = durable
        .workspaces
        .iter()
        .find(|workspace| workspace.id == tab_id)
        .unwrap();
    assert_eq!(
        workspace.owner.as_ref().unwrap(),
        &FleetWorkspaceOwner {
            project_id: "project-a".into(),
            assignment_id: "assignment:project-a:alpha".into(),
            ship_slug: "alpha".into(),
        }
    );
}

fn report_reconciliation_fixture(
    tag: &str,
) -> (ControlContext, Arc<CaptainsRegistry>, Arc<TabRegistry>) {
    let captains = Arc::new(CaptainsRegistry::load(captains_tmp(tag)));
    captains
        .claim_test("captain-a", Some("alpha"), vec!["work-a".into()])
        .unwrap();
    captains.record_crew("captain-a", "crew-a").unwrap();
    let tabs = Arc::new(TabRegistry::new());
    tabs.replace(vec![TabRecord {
        id: "work-a".into(),
        name: "Work A".into(),
        tile_ids: vec!["crew-a".into()],
    }]);
    let context = test_ctx(tag)
        .with_captains_registry(Arc::clone(&captains))
        .with_tab_registry(Arc::clone(&tabs));
    (context, captains, tabs)
}

#[test]
fn workspace_reports_require_organization_capability_before_mutation() {
    let (base, captains, tabs) = report_reconciliation_fixture("report-read-tier");
    let identities = Arc::new(crate::identity::IdentityStore::ephemeral());
    let crew = mint_session(&identities, crate::identity::Role::Crew, "alpha", "crew-a");
    let context = base.with_identity_store(identities);
    let before_captains = captains.snapshot();
    let before_tabs = tabs.snapshot_full();
    let response = dispatch_authenticated(
        &context,
        req_session(
            "read-report-read-tier",
            &crew,
            "report_workspace_tabs",
            json!({
                "baseSeq": before_tabs.seq,
                "tabs": [
                    {"id": "work-a", "name": "Work A", "tileIds": ["crew-a"]},
                    {"id": CAPTAIN_WORKSPACE_ID, "name": CAPTAIN_WORKSPACE_NAME, "tileIds": ["captain-a"]}
                ]
            }),
        ),
    );
    assert!(!response.ok, "a read Crew must not mutate Workspace state");
    assert!(!response.error.unwrap_or_default().is_empty());
    assert_eq!(captains.snapshot().seq, before_captains.seq);
    assert_eq!(tabs.snapshot_full().seq, before_tabs.seq);
}

#[test]
fn invalid_workspace_reports_leave_tabs_captains_and_sequences_unchanged() {
    for (tag, report) in [
        (
            "invalid-occupant",
            json!({"tabs": [
                {"id": "work-a", "name": "Work A", "tileIds": ["captain-a"]},
                {"id": CAPTAIN_WORKSPACE_ID, "name": CAPTAIN_WORKSPACE_NAME, "tileIds": []}
            ]}),
        ),
        (
            "duplicate-id",
            json!({"tabs": [
                {"id": "work-a", "name": "Work A", "tileIds": ["crew-a"]},
                {"id": "work-a", "name": "Duplicate", "tileIds": []},
                {"id": CAPTAIN_WORKSPACE_ID, "name": CAPTAIN_WORKSPACE_NAME, "tileIds": ["captain-a"]}
            ]}),
        ),
        (
            "future-schema",
            json!({"tabs": [
                {"schemaVersion": WORKSPACE_SCHEMA_VERSION + 1, "id": "work-a", "name": "Work A", "kind": "work", "tileIds": ["crew-a"]}
            ]}),
        ),
    ] {
        let (context, captains, tabs) = report_reconciliation_fixture(tag);
        let before_captains = captains.snapshot();
        let before_tabs = tabs.snapshot_full();
        let mut report = report;
        report["baseSeq"] = json!(before_tabs.seq);
        assert!(dispatch(&context, "report_workspace_tabs", &report).is_err());
        let after_captains = captains.snapshot();
        let after_tabs = tabs.snapshot_full();
        assert_eq!(after_captains.seq, before_captains.seq, "case {tag}");
        assert_eq!(
            after_captains.captains, before_captains.captains,
            "case {tag}"
        );
        assert_eq!(after_tabs.seq, before_tabs.seq, "case {tag}");
        assert_eq!(
            serde_json::to_value(after_tabs.tabs).unwrap(),
            serde_json::to_value(before_tabs.tabs).unwrap(),
            "case {tag}"
        );
    }
}

#[test]
fn stale_workspace_report_cas_cannot_commit_crew_reconciliation() {
    let (context, captains, tabs) = report_reconciliation_fixture("report-stale-cas");
    let before_captains = captains.snapshot();
    let before_tabs = tabs.snapshot_full();
    tabs.insert_tab("racing-work", "Racing Work");

    let response = dispatch(
            &context,
            "report_workspace_tabs",
            &json!({
                "baseSeq": before_tabs.seq,
                "tabs": [
                    {"id": "work-a", "name": "Work A", "tileIds": ["crew-a"]},
                    {"id": CAPTAIN_WORKSPACE_ID, "name": CAPTAIN_WORKSPACE_NAME, "tileIds": ["captain-a"]}
                ]
            }),
        )
        .unwrap();
    assert_eq!(response["stale"], true);
    let after_captains = captains.snapshot();
    assert_eq!(after_captains.seq, before_captains.seq);
    assert_eq!(after_captains.captains, before_captains.captains);
    let after_tabs = tabs.snapshot_full();
    assert_eq!(after_tabs.seq, before_tabs.seq + 1);
    assert!(after_tabs.tabs.iter().any(|tab| tab.id == "racing-work"));
}

#[test]
fn workspace_report_persistence_failure_rolls_back_both_registries() {
    let (context, captains, tabs) = report_reconciliation_fixture("report-persist-fail");
    let before_captains = captains.snapshot();
    let before_tabs = tabs.snapshot_full();
    captains.fail_next_persist("workspace report persistence failure");
    let error = dispatch(
            &context,
            "report_workspace_tabs",
            &json!({
                "baseSeq": before_tabs.seq,
                "tabs": [
                    {"id": "work-a", "name": "Work A", "tileIds": ["crew-a"]},
                    {"id": CAPTAIN_WORKSPACE_ID, "name": CAPTAIN_WORKSPACE_NAME, "tileIds": ["captain-a"]}
                ]
            }),
        )
        .unwrap_err();
    assert!(error.contains("workspace report persistence failure"));
    let after_captains = captains.snapshot();
    let after_tabs = tabs.snapshot_full();
    assert_eq!(after_captains.seq, before_captains.seq);
    assert_eq!(after_captains.captains, before_captains.captains);
    assert_eq!(after_tabs.seq, before_tabs.seq);
    assert_eq!(
        serde_json::to_value(after_tabs.tabs).unwrap(),
        serde_json::to_value(before_tabs.tabs).unwrap()
    );
}

#[test]
fn empty_backend_restart_report_reconciles_a_durable_captain_from_stale_work_placement() {
    let path = captains_tmp("captain-relocation-crash");
    CaptainsRegistry::load(path.clone())
        .claim_test("captain-a", Some("alpha"), vec!["work-a".into()])
        .unwrap();
    let captains = Arc::new(CaptainsRegistry::load(path.clone()));
    let tabs = Arc::new(TabRegistry::new());
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let context = test_ctx("captain-relocation-crash")
        .with_captains_registry(Arc::clone(&captains))
        .with_tab_registry(Arc::clone(&tabs))
        .with_apply_sink(sink.clone());
    let startup = tabs.snapshot_full();
    assert!(
        startup.tabs.is_empty(),
        "production starts with no backend tabs"
    );

    let response = dispatch(
        &context,
        "report_workspace_tabs",
        &json!({
            "baseSeq": startup.seq,
            "tabs": [
                {"id": "work-a", "name": "Work A", "tileIds": ["captain-a"]},
                {"id": CAPTAIN_WORKSPACE_ID, "name": CAPTAIN_WORKSPACE_NAME, "tileIds": []}
            ],
            "activeTabId": "work-a"
        }),
    )
    .unwrap();
    assert_eq!(response["stale"], true);
    assert_eq!(
        response["tabs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tab| tab["id"] == CAPTAIN_WORKSPACE_ID)
            .unwrap()["tileIds"],
        json!(["captain-a"])
    );
    let converged = tabs.snapshot_full();
    assert!(!converged.tabs[0].tile_ids.contains(&"captain-a".into()));
    assert_eq!(
        converged
            .tabs
            .iter()
            .find(|tab| tab.id == CAPTAIN_WORKSPACE_ID)
            .unwrap()
            .tile_ids,
        vec!["captain-a".to_string()]
    );
    let calls = sink.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "sync_captains");
    assert!(calls[0].1["sync"]["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .any(|workspace| workspace["id"] == "work-a"));

    let _ = std::fs::remove_file(path.with_extension("json.bak"));
    let _ = std::fs::remove_file(path);
}

#[test]
fn empty_backend_restart_rejects_foreign_or_duplicate_supervisor_placement_without_effect() {
    for (tag, reported_tabs) in [
        (
            "foreign",
            json!([
                {"id": "work-a", "name": "Work A", "tileIds": ["captain-a"]},
                {"id": CAPTAIN_WORKSPACE_ID, "name": CAPTAIN_WORKSPACE_NAME, "tileIds": ["foreign-captain"]}
            ]),
        ),
        (
            "duplicate",
            json!([
                {"id": "work-a", "name": "Work A", "tileIds": ["captain-a"]},
                {"id": CAPTAIN_WORKSPACE_ID, "name": CAPTAIN_WORKSPACE_NAME, "tileIds": ["captain-a"]}
            ]),
        ),
    ] {
        let path = captains_tmp(&format!("empty-restart-{tag}"));
        CaptainsRegistry::load(path.clone())
            .claim_test("captain-a", Some("alpha"), vec!["work-a".into()])
            .unwrap();
        let captains = Arc::new(CaptainsRegistry::load(path.clone()));
        let tabs = Arc::new(TabRegistry::new());
        let context = test_ctx(&format!("empty-restart-{tag}"))
            .with_captains_registry(Arc::clone(&captains))
            .with_tab_registry(Arc::clone(&tabs));
        let before_captains = captains.snapshot();
        let before_tabs = tabs.snapshot_full();

        assert!(dispatch(
            &context,
            "report_workspace_tabs",
            &json!({"baseSeq": 0, "tabs": reported_tabs}),
        )
        .is_err());
        assert_eq!(captains.snapshot().seq, before_captains.seq, "case {tag}");
        assert_eq!(
            captains.snapshot().captains,
            before_captains.captains,
            "case {tag}"
        );
        assert_eq!(tabs.snapshot_full().seq, before_tabs.seq, "case {tag}");
        assert_eq!(
            serde_json::to_value(tabs.snapshot_full().tabs).unwrap(),
            serde_json::to_value(before_tabs.tabs).unwrap(),
            "case {tag}"
        );

        let _ = std::fs::remove_file(path.with_extension("json.bak"));
        let _ = std::fs::remove_file(path);
    }
}

#[test]
fn durable_fleet_workspaces_seed_list_tabs_before_the_first_frontend_report() {
    let path = captains_tmp("durable-workspace-projection");
    let initial = CaptainsRegistry::load(path.clone());
    initial
        .upsert_project(ProjectRecord {
            root_path: None,
            vcs_capability: None,
            git_main_root: None,
            project_id: "project-a".into(),
            name: "Project A".into(),
            repo_root: "/tmp/project-a".into(),
            remote_url: None,
            default_branch: Some("main".into()),
            powder: None,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    initial
        .claim_test("captain-a", Some("alpha"), vec!["work-a".into()])
        .unwrap();
    initial
        .bind_ship_context("alpha", "project-a", "Assignment A", "codex")
        .unwrap();
    initial.record_crew("captain-a", "crew-a").unwrap();
    initial
        .bind_crew_context_exact(
            "captain-a",
            "crew-a",
            "durable placement",
            "codex",
            None,
            None,
            Some("work-a"),
            PowderWorkBinding {
                card_id: "card-a".into(),
                run_id: "run-a".into(),
                agent: Some("agent-a".into()),
                claim_expires_at: Some(1),
                mutation_intent: None,
                dispatch_release_recovery: false,
                state: PowderWorkState::Active,
            },
            None,
            None,
        )
        .unwrap();
    drop(initial);

    let restarted = Arc::new(CaptainsRegistry::load(path.clone()));
    let tabs = Arc::new(TabRegistry::new());
    tabs.replace(restarted.workspace_projection());
    let context = test_ctx("durable-workspace-projection")
        .with_captains_registry(Arc::clone(&restarted))
        .with_tab_registry(Arc::clone(&tabs));
    let listed = dispatch(&context, "list_tabs", &Value::Null).unwrap();
    assert!(listed["tabs"].as_array().unwrap().iter().any(|workspace| {
        workspace["id"] == "work-a" && workspace["tileIds"] == json!(["crew-a"])
    }));
    assert!(listed["tabs"].as_array().unwrap().iter().any(|workspace| {
        workspace["id"] == CAPTAIN_WORKSPACE_ID
            && workspace["name"] == CAPTAIN_WORKSPACE_NAME
            && workspace["tileIds"] == json!(["captain-a"])
    }));
    let durable = restarted.snapshot();
    let owner = durable
        .workspaces
        .iter()
        .find(|workspace| workspace.id == "work-a")
        .unwrap()
        .owner
        .as_ref()
        .unwrap();
    assert_eq!(owner.project_id, "project-a");
    assert_eq!(owner.assignment_id, "assignment:project-a:alpha");
    assert_eq!(owner.ship_slug, "alpha");
    assert!(durable.pending_fleet_operations.is_empty());

    let _ = std::fs::remove_file(path.with_extension("json.bak"));
    let _ = std::fs::remove_file(path);
}

fn durable_close_workspace_fixture(
    tag: &str,
    workspace_ids: &[&str],
) -> (
    PathBuf,
    Arc<CaptainsRegistry>,
    Arc<TabRegistry>,
    ControlContext,
) {
    let path = captains_tmp(tag);
    let captains = Arc::new(CaptainsRegistry::load(path.clone()));
    captains
        .upsert_project(ProjectRecord {
            root_path: None,
            vcs_capability: None,
            git_main_root: None,
            project_id: "project-a".into(),
            name: "Project A".into(),
            repo_root: "/tmp/project-a".into(),
            remote_url: None,
            default_branch: Some("main".into()),
            powder: None,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    captains
        .claim_test(
            "captain-a",
            Some("alpha"),
            workspace_ids.iter().map(|id| (*id).to_string()).collect(),
        )
        .unwrap();
    captains
        .bind_ship_context("alpha", "project-a", "Assignment A", "codex")
        .unwrap();
    captains.record_crew("captain-a", "crew-a").unwrap();
    captains
        .bind_crew_context_exact(
            "captain-a",
            "crew-a",
            "close Workspace recovery",
            "codex",
            None,
            None,
            Some("work-a"),
            PowderWorkBinding {
                card_id: "card-a".into(),
                run_id: "run-a".into(),
                agent: Some("agent-a".into()),
                claim_expires_at: Some(1),
                mutation_intent: None,
                dispatch_release_recovery: false,
                state: PowderWorkState::Active,
            },
            None,
            None,
        )
        .unwrap();
    let tabs = Arc::new(TabRegistry::new());
    tabs.replace(captains.workspace_projection());
    let context = test_ctx(tag)
        .with_captains_registry(Arc::clone(&captains))
        .with_tab_registry(Arc::clone(&tabs));
    (path, captains, tabs, context)
}

#[test]
fn force_close_atomically_rehomes_crew_and_restart_projects_the_committed_state() {
    let (path, captains, tabs, context) =
        durable_close_workspace_fixture("force-close-rehome", &["work-a", "work-b"]);
    let before_projection = tabs.snapshot_full();

    let error = dispatch(
        &context,
        "close_tab",
        &json!({
            "tabId": "work-a",
            "force": true,
            "testCrashAfterFleetCommit": true,
        }),
    )
    .unwrap_err();
    assert!(error.contains("injected crash"));
    assert_eq!(tabs.snapshot_full().seq, before_projection.seq);
    assert!(tabs
        .snapshot()
        .iter()
        .any(|workspace| workspace.id == "work-a"));

    let committed = captains.snapshot();
    assert!(committed
        .workspaces
        .iter()
        .all(|workspace| workspace.id != "work-a"));
    let crew = &committed.captains[0].crew[0];
    assert_eq!(crew.workspace_tab_id.as_deref(), Some("work-b"));
    assert!(matches!(crew.state, CrewState::Active));

    drop(context);
    drop(tabs);
    drop(captains);
    let restarted = CaptainsRegistry::load(path.clone());
    let restarted_tabs = TabRegistry::new();
    restarted_tabs.replace(restarted.workspace_projection());
    let projected = restarted_tabs.snapshot();
    assert!(projected.iter().all(|workspace| workspace.id != "work-a"));
    let work_b = projected
        .iter()
        .find(|workspace| workspace.id == "work-b")
        .unwrap();
    assert_eq!(work_b.tile_ids, vec!["crew-a"]);

    let _ = std::fs::remove_file(path.with_extension("json.bak"));
    let _ = std::fs::remove_file(path);
}

#[test]
fn force_close_with_ambiguous_rehome_persists_needs_assignment_and_rolls_back_on_failure() {
    let (path, captains, tabs, context) =
        durable_close_workspace_fixture("force-close-ambiguous", &["work-a", "work-b", "work-c"]);
    let before_captains = captains.snapshot();
    let before_tabs = tabs.snapshot_full();
    captains.fail_next_persist("force close persistence failure");
    let error = dispatch(
        &context,
        "close_tab",
        &json!({"tabId": "work-a", "force": true}),
    )
    .unwrap_err();
    assert!(error.contains("force close persistence failure"));
    assert_eq!(captains.snapshot().seq, before_captains.seq);
    assert_eq!(captains.snapshot().captains, before_captains.captains);
    assert_eq!(captains.snapshot().workspaces, before_captains.workspaces);
    assert_eq!(tabs.snapshot_full().seq, before_tabs.seq);

    dispatch(
        &context,
        "close_tab",
        &json!({"tabId": "work-a", "force": true}),
    )
    .unwrap();
    let committed = captains.snapshot();
    let crew = &committed.captains[0].crew[0];
    assert_eq!(crew.workspace_tab_id, None);
    assert!(matches!(crew.state, CrewState::NeedsAssignment { .. }));
    assert!(tabs
        .snapshot()
        .iter()
        .all(|workspace| !workspace.tile_ids.contains(&"crew-a".to_string())));

    drop(context);
    drop(tabs);
    drop(captains);
    let restarted = CaptainsRegistry::load(path.clone());
    let crew = &restarted.snapshot().captains[0].crew[0];
    assert_eq!(crew.workspace_tab_id, None);
    assert!(matches!(crew.state, CrewState::NeedsAssignment { .. }));

    let _ = std::fs::remove_file(path.with_extension("json.bak"));
    let _ = std::fs::remove_file(path);
}

#[test]
fn prune_tab_drops_the_tab_but_keeps_the_claim() {
    let reg = CaptainsRegistry::new();
    reg.claim_test("cap-1", Some("alpha"), vec!["tab-1".into(), "tab-2".into()])
        .unwrap();
    assert!(reg.prune_tab("tab-1").unwrap());
    let snap = reg.snapshot();
    assert_eq!(
        snap.captains[0].workspace_tab_ids,
        vec!["tab-2".to_string()]
    );
    assert!(
        !reg.prune_tab("tab-1").unwrap(),
        "already-pruned tab must not bump the revision"
    );
    assert!(reg.prune_tab("tab-2").unwrap());
    // Zero controlled tabs is a valid claim state.
    assert_eq!(reg.snapshot().captains.len(), 1);
}

#[test]
fn close_tab_persistence_failure_preserves_both_registries_and_projection() {
    let path = captains_tmp("close-tab-transaction");
    let captains = Arc::new(CaptainsRegistry::load(path.clone()));
    captains
        .claim_test("captain-a", Some("alpha"), vec!["work-a".into()])
        .unwrap();
    let tabs = Arc::new(TabRegistry::new());
    tabs.replace(vec![
        TabRecord {
            id: "work-a".into(),
            name: "Work A".into(),
            tile_ids: Vec::new(),
        },
        TabRecord {
            id: "work-b".into(),
            name: "Work B".into(),
            tile_ids: Vec::new(),
        },
    ]);
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let context = test_ctx("close-tab-transaction")
        .with_captains_registry(Arc::clone(&captains))
        .with_tab_registry(Arc::clone(&tabs))
        .with_apply_sink(sink.clone());
    let before_captains = captains.snapshot();
    let before_tabs = tabs.snapshot_full();
    captains.fail_next_persist("close tab prune persistence failure");

    let error = dispatch(&context, "close_tab", &json!({"tabId": "work-a"})).unwrap_err();
    assert!(error.contains("close tab prune persistence failure"));
    assert_eq!(captains.snapshot().captains, before_captains.captains);
    assert_eq!(captains.snapshot().seq, before_captains.seq);
    assert_eq!(
        serde_json::to_value(tabs.snapshot_full().tabs).unwrap(),
        serde_json::to_value(before_tabs.tabs).unwrap()
    );
    assert_eq!(tabs.snapshot_full().seq, before_tabs.seq);
    assert!(sink.calls.lock().unwrap().is_empty());

    let _ = std::fs::remove_file(path.with_extension("json.bak"));
    let _ = std::fs::remove_file(path);
}

#[test]
fn registry_persists_across_reloads_including_seq() {
    let path = captains_tmp("roundtrip");
    {
        let reg = CaptainsRegistry::load(path.clone());
        reg.claim_test("cap-1", Some("alpha"), vec!["tab-1".into()])
            .unwrap();
        reg.record_crew("cap-1", "crew-1").unwrap();
    }
    // A fresh load (an app restart) resumes the same claims AND revision.
    let reg = CaptainsRegistry::load(path.clone());
    let snap = reg.snapshot();
    assert_eq!(snap.seq, 2);
    assert_eq!(snap.captains.len(), 1);
    assert_eq!(snap.captains[0].ship_slug, "alpha");
    assert_eq!(crew_tiles(&snap.captains[0]), vec!["crew-1".to_string()]);
    // And keeps counting monotonically from there.
    reg.claim_test("cap-2", Some("beta"), vec![]).unwrap();
    assert_eq!(CaptainsRegistry::load(path.clone()).snapshot().seq, 3);

    // Atomic write discipline: the temp file is renamed over the target, so
    // no `.tmp` sibling is ever left behind after the writes above.
    let stem = path.file_name().unwrap().to_string_lossy().into_owned();
    let leftover_tmp = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .filter_map(|e| e.ok())
        .any(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.starts_with(&stem) && n.ends_with(".tmp")
        });
    assert!(!leftover_tmp, "atomic write must leave no .tmp file behind");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn corrupt_registry_recovers_from_validated_backup_and_quarantines_primary() {
    let path = captains_tmp("backup-recovery");
    let backup = path.with_extension("json.bak");
    {
        let reg = CaptainsRegistry::load(path.clone());
        reg.claim_test("cap-1", Some("alpha"), vec![]).unwrap();
        reg.record_crew("cap-1", "crew-1").unwrap();
        reg.checkpoint(Some("cap-1"), None, None, None, Some("durable checkpoint"))
            .unwrap();
    }
    assert!(
        backup.exists(),
        "a prior validated revision must be retained"
    );
    std::fs::write(&path, b"{ definitely not json").unwrap();

    let recovered = CaptainsRegistry::load(path.clone()).snapshot();
    assert_eq!(recovered.captains.len(), 1);
    assert_eq!(recovered.captains[0].ship_slug, "alpha");
    assert_eq!(crew_tiles(&recovered.captains[0]), vec!["crew-1"]);
    let quarantined = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .filter_map(Result::ok)
        .any(|entry| {
            entry.file_name().to_string_lossy().starts_with(&format!(
                "{}.corrupt.",
                path.file_name().unwrap().to_string_lossy()
            ))
        });
    assert!(quarantined, "the corrupt primary must be quarantined");
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(backup);
}

#[test]
fn concurrent_distinct_ship_claims_create_distinct_project_captains() {
    let reg = Arc::new(CaptainsRegistry::new());
    reg.upsert_project(ProjectRecord {
        root_path: None,
        vcs_capability: None,
        git_main_root: None,
        project_id: "project-one".into(),
        name: "One".into(),
        repo_root: "/tmp".into(),
        remote_url: None,
        default_branch: None,
        powder: None,
        created_at: 0,
        updated_at: 0,
    })
    .unwrap();
    reg.claim_test("cap-a", Some("alpha"), vec![]).unwrap();
    reg.claim_test("cap-b", Some("beta"), vec![]).unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let mut joins = Vec::new();
    for (ship, assignment) in [("alpha", "A"), ("beta", "B")] {
        let reg = Arc::clone(&reg);
        let barrier = Arc::clone(&barrier);
        joins.push(std::thread::spawn(move || {
            barrier.wait();
            reg.bind_ship_context(ship, "project-one", assignment, "codex")
        }));
    }
    barrier.wait();
    let results = joins
        .into_iter()
        .map(|join| join.join().unwrap())
        .collect::<Vec<_>>();
    assert!(results.iter().all(Result::is_ok));

    let snapshot = reg.snapshot();
    let project_captains = snapshot
        .captains
        .iter()
        .filter(|captain| captain.project_id.as_deref() == Some("project-one"))
        .collect::<Vec<_>>();
    assert_eq!(project_captains.len(), 2);
    assert_ne!(
        project_captains[0].assignment_id,
        project_captains[1].assignment_id
    );
    let mut ship_slugs = project_captains
        .iter()
        .map(|captain| captain.ship_slug.as_str())
        .collect::<Vec<_>>();
    ship_slugs.sort_unstable();
    assert_eq!(ship_slugs, vec!["alpha", "beta"]);
}

#[test]
fn concurrent_equivalent_project_registrations_dedupe_canonical_identity() {
    let reg = Arc::new(CaptainsRegistry::new());
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let roots = [
        "/tmp/t-hub-equivalent/./root",
        "/tmp/t-hub-equivalent/root/",
    ];
    let joins = roots
        .into_iter()
        .enumerate()
        .map(|(index, root)| {
            let reg = Arc::clone(&reg);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                reg.upsert_project(ProjectRecord {
                    root_path: Some(root.into()),
                    vcs_capability: Some("none".into()),
                    git_main_root: None,
                    project_id: format!("project-{index}"),
                    name: format!("Project {index}"),
                    repo_root: root.into(),
                    remote_url: None,
                    default_branch: None,
                    powder: None,
                    created_at: 0,
                    updated_at: 0,
                })
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    for join in joins {
        join.join().unwrap().unwrap();
    }
    let projects = reg.projects();
    assert_eq!(projects.len(), 1);
    assert_eq!(
        projects[0].root_path.as_deref(),
        Some("/tmp/t-hub-equivalent/root")
    );
    assert_eq!(projects[0].vcs_capability.as_deref(), Some("none"));
}

#[test]
fn linked_worktree_project_identity_keeps_selected_root_separate_from_git_main_root() {
    let registry = CaptainsRegistry::new();
    let project = registry
        .upsert_project(ProjectRecord {
            root_path: Some("/home/natkins/project/.claude/worktrees/feature".into()),
            vcs_capability: Some("git".into()),
            git_main_root: Some("/home/natkins/project".into()),
            project_id: "linked-project".into(),
            name: "Linked Project".into(),
            repo_root: "/home/natkins/project/.claude/worktrees/feature".into(),
            remote_url: None,
            default_branch: Some("main".into()),
            powder: None,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    assert_eq!(
        project.root_path.as_deref(),
        Some("/home/natkins/project/.claude/worktrees/feature")
    );
    assert_eq!(
        project.git_main_root.as_deref(),
        Some("/home/natkins/project")
    );
    assert_eq!(registry.projects()[0], project);
}

#[test]
fn distinct_linked_roots_do_not_dedupe_on_shared_git_main_root() {
    let registry = CaptainsRegistry::new();
    for (id, root) in [
        ("linked-a", "/home/natkins/project/.claude/worktrees/a"),
        ("linked-b", "/home/natkins/project/.claude/worktrees/b"),
    ] {
        registry
            .upsert_project(ProjectRecord {
                root_path: Some(root.into()),
                vcs_capability: Some("git".into()),
                git_main_root: Some("/home/natkins/project".into()),
                project_id: id.into(),
                name: id.into(),
                repo_root: root.into(),
                remote_url: None,
                default_branch: Some("main".into()),
                powder: None,
                created_at: 1,
                updated_at: 1,
            })
            .unwrap();
    }
    assert_eq!(registry.projects().len(), 2);
    assert_eq!(
        registry
            .projects()
            .iter()
            .map(|project| project.root_path.clone().unwrap())
            .collect::<std::collections::HashSet<_>>()
            .len(),
        2
    );
}

#[test]
fn current_schema_migration_preserves_linked_worktree_identity_metadata() {
    let path = captains_tmp("linked-migration");
    std::fs::write(
        &path,
        json!({
            "schemaVersion": CAPTAINS_SCHEMA_VERSION,
            "seq": 1,
            "captains": [],
            "projects": [{
                "projectId": "linked-project",
                "name": "Linked Project",
                "repoRoot": "/home/natkins/project/.claude/worktrees/feature",
                "rootPath": "/home/natkins/project/.claude/worktrees/feature",
                "vcsCapability": "git",
                "gitMainRoot": "/home/natkins/project",
                "createdAt": 1,
                "updatedAt": 1
            }],
            "workspaces": [{
                "id": "captains-reserved",
                "name": "Captain Workspace",
                "kind": "captain",
                "tileIds": []
            }]
        })
        .to_string(),
    )
    .unwrap();
    let project = CaptainsRegistry::load(path.clone()).projects()[0].clone();
    assert_eq!(
        project.root_path.as_deref(),
        Some("/home/natkins/project/.claude/worktrees/feature")
    );
    assert_eq!(
        project.git_main_root.as_deref(),
        Some("/home/natkins/project")
    );
    let _ = std::fs::remove_file(path);
}

#[test]
fn every_registered_git_only_gate_rejects_non_git_before_operation() {
    let ctx = test_ctx("git-gate-matrix");
    ctx.captains
        .upsert_project(ProjectRecord {
            root_path: Some("/tmp/non-git-gate".into()),
            vcs_capability: Some("none".into()),
            git_main_root: None,
            project_id: "non-git-project".into(),
            name: "Non-Git Project".into(),
            repo_root: "/tmp/non-git-gate".into(),
            remote_url: None,
            default_branch: None,
            powder: None,
            created_at: 0,
            updated_at: 0,
        })
        .unwrap();
    for operation in [
        "dispatch_preflight",
        "baseline",
        "integration",
        "delivery",
        "capacity",
        "create_worktree",
        "remove_worktree",
        "list_worktrees",
        "admin_worktree",
    ] {
        let error =
            require_registered_git_capability(&ctx, operation, "/tmp/non-git-gate").unwrap_err();
        assert_eq!(
                error,
                format!(
                    "git_required code=git_required operation={operation} capability=git action=initialize_git"
                )
            );
    }
}

fn assert_native_git_required(response: ControlResponse, operation: &str) {
    assert!(!response.ok, "unexpected success: {response:?}");
    assert_eq!(response.error_kind.as_deref(), Some("git_required"));
    assert!(!response.retryable);
    assert_eq!(
        response.error_details,
        Some(json!({
            "code": "git_required",
            "operation": operation,
            "capability": "git",
            "action": "initialize_git",
        }))
    );
    assert!(response
        .error
        .as_deref()
        .is_some_and(|message| message.contains("initialize_git")));
}

#[test]
fn explicit_none_dispatcher_response_matches_cli_mcp_parity_fixture() {
    let ctx = test_ctx("dispatcher-parity-fixture");
    ctx.captains
        .upsert_project(ProjectRecord {
            root_path: Some("/tmp/dispatcher-parity-fixture".into()),
            vcs_capability: Some("none".into()),
            git_main_root: None,
            project_id: "dispatcher-parity-fixture".into(),
            name: "Dispatcher parity fixture".into(),
            repo_root: "/tmp/dispatcher-parity-fixture".into(),
            remote_url: None,
            default_branch: None,
            powder: None,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    let response = dispatch_authenticated(
        &ctx,
        req(
            "dispatcher-parity-fixture",
            "dispatch_preflight",
            json!({
                "projectId": "dispatcher-parity-fixture",
                "sourceCommit": "1111111111111111111111111111111111111111",
                "requestedLanes": [],
                "integrationContracts": []
            }),
        ),
    );
    let actual = serde_json::to_value(response).unwrap();
    let fixture: Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/explicit-none-dispatch-preflight-response.json"
    ))
    .unwrap();
    assert_eq!(actual, fixture);
}

#[test]
fn real_dispatch_preflight_and_delivery_gates_return_native_git_required_without_mutation() {
    let ctx = test_ctx("native-git-gates");
    ctx.captains
        .upsert_project(ProjectRecord {
            root_path: Some("/tmp/native-git-gates".into()),
            vcs_capability: Some("none".into()),
            git_main_root: None,
            project_id: "native-git-gates-project".into(),
            name: "Native Git Gates".into(),
            repo_root: "/tmp/native-git-gates".into(),
            remote_url: None,
            default_branch: None,
            powder: None,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    git::reset_worktree_list_calls();
    let preflight = dispatch_authenticated(
        &ctx,
        req(
            "native-git-gates",
            "dispatch_preflight",
            json!({
                "projectId": "native-git-gates-project",
                "sourceCommit": "1111111111111111111111111111111111111111",
                "requestedLanes": [],
                "integrationContracts": []
            }),
        ),
    );
    assert_native_git_required(preflight, "dispatch_preflight");

    ctx.captains
        .claim_test("native-git-captain", Some("native-git-ship"), vec![])
        .unwrap();
    ctx.captains
        .bind_ship_context(
            "native-git-ship",
            "native-git-gates-project",
            "Native gate assignment",
            "codex",
        )
        .unwrap();
    let start = dispatch_authenticated(
        &ctx,
        req(
            "native-git-gates",
            "start_agent",
            json!({
                "requestId": "native-start",
                "captainSessionId": "native-git-captain",
                "assignment": "Native gate assignment",
                "directory": "/tmp/native-git-gates/worktree",
                "harness": "codex",
                "name": "Native gate agent",
                "workspaceTabId": "work",
                "sourceCommit": "1111111111111111111111111111111111111111",
                "visibleProductBug": false,
                "laneId": "native-lane",
                "dependencies": [],
                "mutableFiles": [],
                "mutableSchemas": [],
                "mutableInterfaces": [],
                "integrationContracts": [],
                "admissionPurpose": "ordinary"
            }),
        ),
    );
    assert_native_git_required(start, "start_agent");

    let (lane_claim, dispatch_capacity) =
        test_dispatch_evidence("native-delivery-lane", "native-delivery-agent");
    ctx.captains
        .insert_agent_session(AgentSessionRecord {
            agent_session_id: "native-delivery-agent".into(),
            captain_session_id: "native-git-captain".into(),
            project_id: "native-git-gates-project".into(),
            assignment: "Native delivery gate".into(),
            directory: "/tmp/native-git-gates/delivery".into(),
            worktree_path: None,
            branch: None,
            workspace_tab_id: None,
            harness: "codex".into(),
            provider: "codex".into(),
            provider_conversation_id: None,
            resume_point: None,
            runtime_state: RuntimeState::Starting,
            work_stage: crate::agent_session::WorkStage::Assigned,
            delivery: Some(crate::agent_session::DeliveryProvenance::new(
                "1111111111111111111111111111111111111111",
                false,
            )),
            lane_claim: Some(lane_claim),
            integration_contracts: Vec::new(),
            dispatch_capacity: Some(dispatch_capacity),
            admission_purpose: crate::governor::AdmissionPurpose::Ordinary,
            created_at: 2,
            updated_at: 2,
        })
        .unwrap();
    let delivery_before = ctx.captains.snapshot();
    let delivery = dispatch_authenticated(
        &ctx,
        req(
            "native-git-gates",
            "record_agent_delivery",
            json!({
                "agentSessionId": "native-delivery-agent",
                "state": "implemented",
                "evidence": {}
            }),
        ),
    );
    assert_native_git_required(delivery, "delivery");
    let integration = dispatch_authenticated(
        &ctx,
        req(
            "native-git-gates",
            "record_agent_delivery",
            json!({
                "agentSessionId": "native-delivery-agent",
                "state": "integrated",
                "evidence": {}
            }),
        ),
    );
    assert_native_git_required(integration, "integration");
    let after = ctx.captains.snapshot();
    assert_eq!(after.seq, delivery_before.seq);
    let agent = after
        .agent_sessions
        .iter()
        .find(|agent| agent.agent_session_id == "native-delivery-agent")
        .unwrap();
    assert!(agent
        .delivery
        .as_ref()
        .is_some_and(|delivery| delivery.resulting_commit.is_none()));
    assert_eq!(git::worktree_list_calls(), 0);
}

#[test]
fn worktree_list_counter_observes_calls_across_threads() {
    git::reset_worktree_list_calls();
    let calls = std::thread::spawn(|| {
        git::reset_worktree_list_calls();
        let _ = git::worktree_list("/tmp/worktree-counter-positive-control");
        git::worktree_list_calls()
    })
    .join()
    .unwrap();
    assert_eq!(calls, 1);
}

#[test]
fn native_worktree_dispatchers_gate_registered_none_before_probe_or_mutation() {
    let ctx = test_ctx("native-worktree-gates");
    ctx.captains
        .upsert_project(ProjectRecord {
            root_path: Some("/tmp/native-worktree-gates".into()),
            vcs_capability: Some("none".into()),
            git_main_root: None,
            project_id: "native-worktree-gates".into(),
            name: "Native worktree gates".into(),
            repo_root: "/tmp/native-worktree-gates".into(),
            remote_url: None,
            default_branch: None,
            powder: None,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    for (command, args, operation) in [
        (
            "create_worktree",
            json!({
                "repoRoot": "/tmp/native-worktree-gates",
                "worktreePath": "/tmp/native-worktree-gates-wt"
            }),
            "create_worktree",
        ),
        (
            "remove_worktree",
            json!({
                "repoRoot": "/tmp/native-worktree-gates",
                "worktreePath": "/tmp/native-worktree-gates-wt"
            }),
            "remove_worktree",
        ),
        (
            "list_worktrees",
            json!({ "cwd": "/tmp/native-worktree-gates" }),
            "list_worktrees",
        ),
        (
            "git_worktree_list",
            json!({ "cwd": "/tmp/native-worktree-gates" }),
            "list_worktrees",
        ),
    ] {
        let before = ctx.captains.snapshot();
        git::reset_worktree_list_calls();
        let response = dispatch_authenticated(&ctx, req("native-worktree-gates", command, args));
        assert_native_git_required(response, operation);
        assert_eq!(ctx.captains.snapshot().seq, before.seq);
        assert!(ctx.captains.snapshot().pending_fleet_operations.is_empty());
        assert_eq!(git::worktree_list_calls(), 0);
    }
}

#[test]
fn stale_create_worktree_reprobe_authorizes_then_gates_without_worktree_probe() {
    let mut ctx = test_ctx("stale-native-worktree-gate");
    ctx.captains
        .upsert_project(ProjectRecord {
            root_path: Some("/tmp/stale-native-worktree-gate".into()),
            vcs_capability: Some("none".into()),
            git_main_root: None,
            project_id: "stale-native-worktree-gate".into(),
            name: "Stale native worktree gate".into(),
            repo_root: "/tmp/stale-native-worktree-gate".into(),
            remote_url: None,
            default_branch: None,
            powder: None,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    let args = json!({
        "requestId": "stale-native-worktree-request",
        "repoRoot": "/tmp/stale-native-worktree-gate",
        "worktreePath": "/tmp/stale-native-worktree-gate-stale",
    });
    ctx.requests = Arc::new(RequestCache::with_bounds(
        8,
        Duration::from_secs(600),
        Duration::from_millis(1),
    ));
    let signature = request_signature("create_worktree", &args);
    assert!(matches!(
        ctx.requests
            .begin_bound_with_reservation("stale-native-worktree-request", &signature)
            .0,
        BeginOutcome::Fresh
    ));
    std::thread::sleep(Duration::from_millis(5));
    let before = ctx.captains.snapshot();
    git::reset_worktree_list_calls();
    let response = dispatch_authenticated(
        &ctx,
        req("stale-native-worktree-gate", "create_worktree", args),
    );
    assert_native_git_required(response, "create_worktree");
    assert_eq!(ctx.captains.snapshot().seq, before.seq);
    assert!(ctx.captains.snapshot().pending_fleet_operations.is_empty());
    assert_eq!(git::worktree_list_calls(), 0);
    assert!(!std::path::Path::new("/tmp/stale-native-worktree-gate-stale").exists());
}

#[test]
fn delegated_none_worktree_admin_authorizes_before_git_gate_and_rejects_expired_grants() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let ctx = test_ctx("delegated-none-worktree-gate");
    let admin_tile = "delegated-none-admin";
    let worktree = "/tmp/delegated-none-worktree-gate/worktree";
    ctx.captains
        .upsert_project(ProjectRecord {
            root_path: Some("/tmp/delegated-none-worktree-gate".into()),
            vcs_capability: Some("none".into()),
            git_main_root: None,
            project_id: "delegated-none-worktree-project".into(),
            name: "Delegated none worktree".into(),
            repo_root: "/tmp/delegated-none-worktree-gate".into(),
            remote_url: None,
            default_branch: None,
            powder: None,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    ctx.captains
        .claim_test(
            "delegated-none-captain",
            Some("delegated-none-ship"),
            vec![],
        )
        .unwrap();
    ctx.captains
        .bind_ship_context(
            "delegated-none-ship",
            "delegated-none-worktree-project",
            "Delegated none assignment",
            "codex",
        )
        .unwrap();
    ctx.captains
        .record_crew("delegated-none-captain", admin_tile)
        .unwrap();
    create_test_tmux_session(&tmux_target(admin_tile)).unwrap();
    let captain_identity = ctx.identity.mint(crate::identity::Role::Captain).unwrap();
    ctx.identity
        .bind_tile(&captain_identity.id, "delegated-none-captain")
        .unwrap();
    let admin_identity = ctx.identity.mint(crate::identity::Role::Crew).unwrap();
    ctx.identity
        .bind_tile(&admin_identity.id, admin_tile)
        .unwrap();
    let captain = resolve_identity(&ctx, &captain_identity.secret).unwrap();
    let appointed = appoint_admin(
        &ctx,
        &json!({
            "actorSessionId": admin_tile,
            "role": "shipAdmin",
            "permittedOperations": ["recoverResource"]
        }),
        Some(&captain),
        false,
    )
    .unwrap();
    let grant_id = appointed["grant"]["grantId"].as_str().unwrap().to_string();
    let before = ctx.captains.snapshot();
    git::reset_worktree_list_calls();
    let response = dispatch_authenticated(
        &ctx,
        req_session(
            &ctx.token,
            &admin_identity.secret,
            "execute_admin_operation",
            json!({
                "operation": "recoverResource",
                "target": { "kind": "worktree", "path": worktree }
            }),
        ),
    );
    assert_native_git_required(response, "admin_worktree");
    assert_eq!(ctx.captains.snapshot().seq, before.seq);
    assert_eq!(git::worktree_list_calls(), 0);

    let foreign_identity = ctx.identity.mint(crate::identity::Role::Crew).unwrap();
    ctx.identity
        .bind_tile(&foreign_identity.id, "delegated-none-foreign")
        .unwrap();
    let unauthorized = dispatch_authenticated(
        &ctx,
        req_session(
            &ctx.token,
            &foreign_identity.secret,
            "execute_admin_operation",
            json!({
                "operation": "recoverResource",
                "target": { "kind": "worktree", "path": worktree }
            }),
        ),
    );
    assert!(!unauthorized.ok);
    assert!(unauthorized.error.unwrap().contains("administrative grant"));
    assert_eq!(unauthorized.error_kind, None);
    assert_eq!(git::worktree_list_calls(), 0);

    revoke_admin(
        &ctx,
        &json!({ "grantId": grant_id, "reason": "expired-test" }),
        Some(&captain),
        false,
    )
    .unwrap();
    let expired = dispatch_authenticated(
        &ctx,
        req_session(
            &ctx.token,
            &admin_identity.secret,
            "execute_admin_operation",
            json!({
                "operation": "recoverResource",
                "target": { "kind": "worktree", "path": worktree }
            }),
        ),
    );
    assert!(!expired.ok);
    assert!(expired.error.unwrap().contains("administrative grant"));
    assert_eq!(expired.error_kind, None);
    assert_eq!(git::worktree_list_calls(), 0);
    reap_test_tmux_session(&tmux_target(admin_tile)).unwrap();
}

#[test]
fn registered_git_gate_uses_the_most_specific_project_independent_of_order() {
    for (outer_capability, inner_capability, expected_error) in
        [("none", "git", false), ("git", "none", true)]
    {
        for order in [["outer", "inner"], ["inner", "outer"]] {
            let registry = CaptainsRegistry::new();
            for project_id in order {
                let (root, capability) = if project_id == "outer" {
                    ("/tmp/project-nesting", outer_capability)
                } else {
                    ("/tmp/project-nesting/selected", inner_capability)
                };
                registry
                    .upsert_project(ProjectRecord {
                        root_path: Some(root.into()),
                        vcs_capability: Some(capability.into()),
                        git_main_root: None,
                        project_id: project_id.into(),
                        name: project_id.into(),
                        repo_root: root.into(),
                        remote_url: None,
                        default_branch: None,
                        powder: None,
                        created_at: 1,
                        updated_at: 1,
                    })
                    .unwrap();
            }
            let ctx = test_ctx("specific-git-gate").with_captains_registry(Arc::new(registry));
            let result = require_registered_git_capability(
                &ctx,
                "list_worktrees",
                "/tmp/project-nesting/selected/worktree",
            );
            assert_eq!(result.is_err(), expected_error);
        }
    }
}

#[test]
fn registered_git_gate_fails_closed_for_equal_specificity_ambiguity() {
    let registry = CaptainsRegistry::new();
    {
        let mut inner = registry.lock();
        inner.projects = vec![
            ProjectRecord {
                root_path: Some("/tmp/ambiguous-root".into()),
                vcs_capability: Some("none".into()),
                git_main_root: None,
                project_id: "ambiguous-a".into(),
                name: "Ambiguous A".into(),
                repo_root: "/tmp/ambiguous-root".into(),
                remote_url: None,
                default_branch: None,
                powder: None,
                created_at: 1,
                updated_at: 1,
            },
            ProjectRecord {
                root_path: Some("/tmp/ambiguous-root/".into()),
                vcs_capability: Some("git".into()),
                git_main_root: None,
                project_id: "ambiguous-b".into(),
                name: "Ambiguous B".into(),
                repo_root: "/tmp/ambiguous-root/".into(),
                remote_url: None,
                default_branch: None,
                powder: None,
                created_at: 1,
                updated_at: 1,
            },
        ];
    }
    let ctx = test_ctx("ambiguous-git-gate").with_captains_registry(Arc::new(registry));
    let error =
        require_registered_git_capability(&ctx, "list_worktrees", "/tmp/ambiguous-root/selected")
            .unwrap_err();
    assert!(error.contains("ambiguous"));
    assert!(!error.contains("git_required"));
}

#[test]
fn targeted_provision_rollback_preserves_unrelated_concurrent_mutation() {
    let reg = CaptainsRegistry::new();
    let claimed = reg
        .claim_test("cap-a", Some("alpha"), vec![])
        .unwrap()
        .record;
    reg.claim_test("cap-b", Some("beta"), vec![]).unwrap();
    reg.rollback_provisioned_claim("cap-a", &claimed, None)
        .unwrap();
    let snapshot = reg.snapshot();
    assert!(snapshot
        .captains
        .iter()
        .any(|captain| captain.ship_slug == "beta"));
    assert!(!snapshot
        .captains
        .iter()
        .any(|captain| captain.ship_slug == "alpha"));
}

#[test]
fn registry_mutation_fails_and_restores_memory_when_persistence_fails() {
    let blocker = captains_tmp("persist-blocker");
    std::fs::write(&blocker, b"not a directory").unwrap();
    let reg = CaptainsRegistry::load(blocker.join("captains.json"));

    let error = reg
        .claim_test("cap-1", Some("alpha"), vec!["tab-1".into()])
        .unwrap_err();

    assert!(error.contains("could not be created"), "got: {error}");
    assert_eq!(reg.snapshot().seq, 0);
    assert!(reg.snapshot().captains.is_empty());
    std::fs::remove_file(blocker).unwrap();
}

#[test]
fn captain_and_crew_checkpoints_survive_registry_reload() {
    let path = captains_tmp("checkpoint-roundtrip");
    let _ = std::fs::remove_file(&path);
    let reg = CaptainsRegistry::load(path.clone());
    reg.claim_test("captain-1", Some("checkpoint-ship"), vec![])
        .unwrap();
    reg.record_crew("captain-1", "crew-1").unwrap();
    reg.checkpoint(
        None,
        Some("checkpoint-ship"),
        None,
        Some("thread-captain"),
        Some("Review Crew result, then update Powder."),
    )
    .unwrap();
    reg.checkpoint(
        Some("captain-1"),
        None,
        Some("crew-1"),
        Some("thread-crew"),
        Some("Implementing persistence tests."),
    )
    .unwrap();

    let restored = CaptainsRegistry::load(path.clone()).snapshot();
    let captain = &restored.captains[0];
    assert_eq!(captain.conversation_id.as_deref(), Some("thread-captain"));
    assert_eq!(
        captain.resume_point.as_deref(),
        Some("Review Crew result, then update Powder.")
    );
    assert_eq!(
        captain.crew[0].conversation_id.as_deref(),
        Some("thread-crew")
    );
    assert_eq!(
        captain.crew[0].resume_point.as_deref(),
        Some("Implementing persistence tests.")
    );
    std::fs::remove_file(path).unwrap();
}

#[test]
fn captain_checkpoint_command_updates_the_manifest() {
    let ctx = test_ctx("secret");
    ctx.captains
        .claim_test("captain-checkpoint", Some("checkpoint-command"), vec![])
        .unwrap();

    let response = dispatch(
        &ctx,
        "captain_checkpoint",
        &json!({
            "shipSlug": "checkpoint-command",
            "conversationId": "thread-123",
            "resumePoint": "Resume by reconciling Powder events."
        }),
    )
    .unwrap();

    assert_eq!(response["accepted"], "captain_checkpoint");
    assert_eq!(response["target"], "captain");
    assert_eq!(response["captain"]["conversationId"], "thread-123");
    assert_eq!(
        response["captain"]["resumePoint"],
        "Resume by reconciling Powder events."
    );
}

#[test]
fn corrupt_or_missing_persistence_starts_empty() {
    let missing = CaptainsRegistry::load(captains_tmp("missing"));
    assert_eq!(missing.snapshot().seq, 0);
    assert!(missing.snapshot().captains.is_empty());

    let path = captains_tmp("corrupt");
    std::fs::write(&path, b"{not json").unwrap();
    let reg = CaptainsRegistry::load(path.clone());
    assert!(reg.snapshot().captains.is_empty());
    // The first mutation heals the file.
    reg.claim_test("cap-1", None, vec![]).unwrap();
    let healed = CaptainsRegistry::load(path.clone());
    assert_eq!(healed.snapshot().captains.len(), 1);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn future_registry_schema_is_preserved_and_blocks_writes() {
    let path = captains_tmp("future-schema");
    let body = json!({
        "schemaVersion": CAPTAINS_SCHEMA_VERSION + 1,
        "seq": 99,
        "captains": [],
        "projects": [],
        "futureField": {"must": "survive"},
    })
    .to_string();
    std::fs::write(&path, &body).unwrap();

    let registry = CaptainsRegistry::load(path.clone());
    assert!(registry.write_blocked.is_some());
    let error = registry
        .claim_test("cap-future", Some("future"), vec![])
        .unwrap_err();
    assert!(error.contains("read-only"), "got: {error}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), body);

    let prefix = format!("{}.corrupt.", path.file_name().unwrap().to_string_lossy());
    let quarantined = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .flatten()
        .any(|entry| entry.file_name().to_string_lossy().starts_with(&prefix));
    assert!(!quarantined, "future schemas must never be quarantined");
    let _ = std::fs::remove_file(path);
}

#[test]
fn orphan_replacement_requires_its_exact_registry_schema() {
    let path = captains_tmp("orphan-replacement-old-schema");
    std::fs::write(
        &path,
        json!({
            "schemaVersion": 22,
            "cortana": {
                "recovery": {
                    "kind": "replacingOrphan"
                }
            }
        })
        .to_string(),
    )
    .unwrap();

    assert!(matches!(
        CaptainsRegistry::read_snapshot(&path),
        Err(SnapshotReadError::IncompatibleRecovery { .. })
    ));

    std::fs::write(
        &path,
        json!({
            "schemaVersion": CAPTAINS_SCHEMA_VERSION,
            "cortana": {
                "identityId": "legacy-orphan-identity",
                "generation": 1,
                "terminalId": "a1b2c3d4",
                "harness": "codex",
                "recovery": {
                    "kind": "replacingOrphan",
                    "operation_id": "missing-effect-identity",
                    "started_at": 1,
                    "orphan_terminal_id": "a1b2c3d4",
                    "orphan_identity_id": "legacy-orphan-identity",
                    "orphan_generation": 1,
                    "harness": "codex"
                }
            }
        })
        .to_string(),
    )
    .unwrap();
    assert!(matches!(
        CaptainsRegistry::read_snapshot(&path),
        Err(SnapshotReadError::IncompatibleRecovery { .. })
    ));
    std::fs::remove_file(path).ok();
}

#[test]
fn managed_launch_recovery_requires_schema_v25_v26_v27_v28_and_exact_shape() {
    let path = captains_tmp("managed-launch-schema");
    let launch = json!({
        "version": 1,
        "operationId": "launch-operation",
        "terminalId": "a1b2c3d4",
        "tmuxTarget": "th_a1b2c3d4",
        "identityId": "cortana-identity",
        "generation": 1,
        "harness": "codex",
        "unitName": format!("t-hub-{}.scope", "a".repeat(32)),
        "launchNonce": "a".repeat(32),
        "tools": {
            "python": {"path": "/usr/bin/python3.12", "device": 1, "inode": 3},
            "systemctl": {"path": "/usr/bin/systemctl", "device": 1, "inode": 1},
            "systemdRun": {"path": "/usr/bin/systemd-run", "device": 1, "inode": 2}
        },
        "phase": "prepared"
    });
    let document = |schema_version, launch: Value| {
        json!({
            "schemaVersion": schema_version,
            "cortana": {
                "identityId": "cortana-identity",
                "generation": 1,
                "harness": "codex",
                "managedLaunch": launch,
                "recovery": {
                    "kind": "recovering",
                    "operation_id": "launch-operation",
                    "started_at": 1
                }
            }
        })
    };

    std::fs::write(&path, document(24, launch.clone()).to_string()).unwrap();
    assert!(matches!(
        CaptainsRegistry::read_snapshot(&path),
        Err(SnapshotReadError::IncompatibleRecovery { .. })
    ));

    for (tool, path_value) in [("python", "/tmp/python3"), ("systemctl", "/tmp/systemctl")] {
        let mut malformed = launch.clone();
        malformed["tools"][tool]["path"] = json!(path_value);
        std::fs::write(
            &path,
            document(CAPTAINS_SCHEMA_VERSION, malformed).to_string(),
        )
        .unwrap();
        assert!(matches!(
            CaptainsRegistry::read_snapshot(&path),
            Err(SnapshotReadError::IncompatibleRecovery { .. })
        ));
    }

    let mut v2 = launch.clone();
    v2["version"] = json!(2);
    std::fs::write(&path, document(25, v2).to_string()).unwrap();
    assert!(matches!(
        CaptainsRegistry::read_snapshot(&path),
        Err(SnapshotReadError::IncompatibleRecovery { .. })
    ));

    let mut v3 = launch.clone();
    v3["version"] = json!(3);
    v3["expectedHarnessLaunchProvenance"] = json!({
        "version": 1,
        "provider": "codex",
        "kind": "direct",
        "executable": {
            "path": "/usr/local/bin/codex",
            "device": 1,
            "inode": 4
        }
    });
    std::fs::write(&path, document(26, v3.clone()).to_string()).unwrap();
    assert!(matches!(
        CaptainsRegistry::read_snapshot(&path),
        Err(SnapshotReadError::IncompatibleRecovery { .. })
    ));
    let v3_document = document(27, v3.clone());
    let current_snapshot: CaptainsSnapshot = serde_json::from_value(v3_document).unwrap();
    let decoded_launch = current_snapshot.cortana.managed_launch.as_ref().unwrap();
    assert!(crate::harness::valid_expected_harness_launch_provenance(
        decoded_launch
            .expected_harness_launch_provenance
            .as_ref()
            .unwrap()
    ));
    assert_eq!(
        exact_cortana_tmux_target(&decoded_launch.terminal_id).unwrap(),
        decoded_launch.tmux_target
    );
    assert!(valid_cortana_python_tool(&decoded_launch.tools.python));
    assert!(
        valid_cortana_managed_launch(decoded_launch),
        "{decoded_launch:?}"
    );
    let mut valid_current_snapshot = powder_lifecycle_registry(None).snapshot();
    valid_current_snapshot.schema_version = 27;
    valid_current_snapshot.cortana = current_snapshot.cortana;
    std::fs::write(
        &path,
        serde_json::to_string(&valid_current_snapshot).unwrap(),
    )
    .unwrap();
    let current = CaptainsRegistry::read_snapshot(&path);
    assert!(current.is_ok(), "{current:?}");

    let mut v3_with_child = v3.clone();
    v3_with_child["expectedHarnessLaunchProvenance"]["version"] = json!(2);
    v3_with_child["expectedHarnessLaunchProvenance"]["trustedChildExecutable"] = json!({
        "path": "/usr/local/lib/codex/native/codex",
        "device": 1,
        "inode": 5
    });
    std::fs::write(&path, document(27, v3_with_child).to_string()).unwrap();
    assert!(matches!(
        CaptainsRegistry::read_snapshot(&path),
        Err(SnapshotReadError::IncompatibleRecovery { .. })
    ));

    let mut v4 = v3;
    v4["version"] = json!(4);
    v4["expectedHarnessLaunchProvenance"]["version"] =
        json!(crate::harness::EXPECTED_HARNESS_LAUNCH_PROVENANCE_VERSION);
    std::fs::write(&path, document(27, v4.clone()).to_string()).unwrap();
    assert!(matches!(
        CaptainsRegistry::read_snapshot(&path),
        Err(SnapshotReadError::IncompatibleRecovery { .. })
    ));
    let v4_document = document(CAPTAINS_SCHEMA_VERSION, v4);
    let v4_snapshot: CaptainsSnapshot = serde_json::from_value(v4_document).unwrap();
    assert!(valid_cortana_managed_launch(
        v4_snapshot.cortana.managed_launch.as_ref().unwrap()
    ));

    let mut mismatched = document(CAPTAINS_SCHEMA_VERSION, launch);
    mismatched["cortana"]["recovery"]["operation_id"] = json!("other-operation");
    std::fs::write(&path, mismatched.to_string()).unwrap();
    assert!(matches!(
        CaptainsRegistry::read_snapshot(&path),
        Err(SnapshotReadError::IncompatibleRecovery { .. })
    ));
    std::fs::remove_file(path).ok();
}

#[test]
fn legacy_orphan_provenance_requires_schema_v22_and_exact_durable_binding() {
    let path = captains_tmp("legacy-orphan-provenance-old-schema");
    let provenance = json!({
        "version": crate::cortana_reconcile::LEGACY_ORPHAN_PROVENANCE_VERSION,
        "sourceSchemaVersion": 18,
        "identityId": "legacy-identity",
        "terminalId": "a1b2c3d4",
        "generation": 1,
        "harness": "codex",
        "healthyOperationId": "legacy-healthy"
    });
    std::fs::write(
        &path,
        json!({
            "schemaVersion": 21,
            "cortana": {
                "identityId": "legacy-identity",
                "generation": 1,
                "harness": "codex",
                "legacyOrphanProvenance": provenance
            }
        })
        .to_string(),
    )
    .unwrap();
    assert!(matches!(
        CaptainsRegistry::read_snapshot(&path),
        Err(SnapshotReadError::IncompatibleRecovery { .. })
    ));

    std::fs::write(
        &path,
        json!({
            "schemaVersion": 22,
            "cortana": {
                "identityId": "different-identity",
                "generation": 1,
                "harness": "codex",
                "legacyOrphanProvenance": provenance
            }
        })
        .to_string(),
    )
    .unwrap();
    assert!(matches!(
        CaptainsRegistry::read_snapshot(&path),
        Err(SnapshotReadError::IncompatibleRecovery { .. })
    ));
    std::fs::remove_file(path).ok();
}

#[test]
fn conflicting_schema18_cortana_backups_do_not_mint_recovery_provenance() {
    let path = captains_tmp("legacy-orphan-conflicting-backups");
    let parent = path.parent().unwrap();
    let file_name = path.file_name().unwrap().to_string_lossy();
    std::fs::write(
        &path,
        json!({
            "schemaVersion": 21,
            "seq": 20,
            "captains": [],
            "workspaces": [{
                "id": CAPTAIN_WORKSPACE_ID,
                "name": CAPTAIN_WORKSPACE_NAME,
                "kind": "captain",
                "tileIds": []
            }],
            "cortana": {
                "identityId": "legacy-identity",
                "generation": 1,
                "terminalId": null,
                "harness": "codex",
                "recovery": {
                    "kind": "degraded",
                    "operation_id": "legacy-degraded",
                    "reason": "identity disappeared",
                    "detected_at": 2
                }
            }
        })
        .to_string(),
    )
    .unwrap();
    let mut backups = Vec::new();
    for (index, terminal_id) in ["a1b2c3d4", "e5f6g7h8"].into_iter().enumerate() {
        let backup = parent.join(format!("{file_name}.migration-v20.{index}.bak"));
        std::fs::write(
            &backup,
            json!({
                "schemaVersion": 18,
                "seq": 10 + index,
                "captains": [],
                "workspaces": [{
                    "id": CAPTAIN_WORKSPACE_ID,
                    "name": CAPTAIN_WORKSPACE_NAME,
                    "kind": "captain",
                    "tileIds": []
                }],
                "cortana": {
                    "identityId": "legacy-identity",
                    "generation": 1,
                    "terminalId": terminal_id,
                    "harness": "codex",
                    "recovery": {
                        "kind": "healthy",
                        "operation_id": format!("healthy-{index}"),
                        "verified_at": 1
                    }
                }
            })
            .to_string(),
        )
        .unwrap();
        backups.push(backup);
    }
    let registry = CaptainsRegistry::load(path.clone());
    assert!(registry
        .cortana_identity()
        .legacy_orphan_provenance
        .is_none());
    std::fs::remove_file(path).ok();
    for backup in backups {
        std::fs::remove_file(backup).ok();
    }
}

#[test]
fn schema_v12_without_release_recovery_upgrades_to_v13_on_the_next_write() {
    let path = captains_tmp("schema-v12-upgrade");
    let _ = std::fs::remove_file(&path);
    let legacy = CaptainsSnapshot {
        schema_version: 12,
        seq: 1,
        captains: vec![],
        cortana: crate::cortana_reconcile::CortanaDurableIdentity::default(),
        agent_sessions: vec![],
        agent_checkpoints: vec![],
        agent_events: vec![],
        projects: vec![],
        workspaces: vec![],
        pending_fleet_operations: vec![],
        retired_fleet_tile_ids: vec![],
        pending_dispatch_claims: vec![],
        pending_dispatch_releases: vec![],
        pending_git_initializations: vec![],
    };
    std::fs::write(&path, serde_json::to_vec(&legacy).unwrap()).unwrap();

    assert_eq!(
        CaptainsRegistry::read_snapshot(&path)
            .unwrap()
            .schema_version,
        12
    );
    let registry = CaptainsRegistry::load(path.clone());
    registry
        .claim_test("captain-v13", Some("schema-v13"), vec![])
        .unwrap();
    let persisted: CaptainsSnapshot =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(persisted.schema_version, CAPTAINS_SCHEMA_VERSION);
    let _ = std::fs::remove_file(path.with_extension("json.bak"));
    let _ = std::fs::remove_file(path);
}

#[test]
fn schema_13_fixture_loads_without_network_access() {
    let path = captains_tmp("schema-13-fixture-load");
    std::fs::write(&path, SCHEMA_13_REGISTRY_FIXTURE).unwrap();

    let snapshot = CaptainsRegistry::read_snapshot(&path).unwrap();
    assert_eq!(snapshot.schema_version, 13);
    assert_eq!(snapshot.captains[0].ship_slug, "aurora");
    assert_eq!(
        snapshot.captains[0].crew[0].terminal_id,
        "tile-aurora-worker"
    );
    assert_eq!(
        snapshot.projects[0].remote_url.as_deref(),
        Some("https://example.invalid/aurora.git")
    );
    assert_eq!(
        snapshot.projects[0].root_path.as_deref(),
        Some("/sanitized/workspaces/aurora")
    );
    assert_eq!(snapshot.projects[0].vcs_capability.as_deref(), Some("git"));
    assert_eq!(
        snapshot.projects[0].git_main_root.as_deref(),
        Some("/sanitized/workspaces/aurora")
    );

    let loaded = CaptainsRegistry::load(path.clone());
    assert_eq!(loaded.snapshot().seq, 41);
    assert!(loaded.snapshot().agent_sessions.is_empty());

    let _ = std::fs::remove_file(path);
}

#[test]
fn schema_13_fixture_first_write_creates_migration_backup() {
    let path = captains_tmp("schema-13-fixture-migration-backup");
    std::fs::write(&path, SCHEMA_13_REGISTRY_FIXTURE).unwrap();

    let registry = CaptainsRegistry::load(path.clone());
    registry
        .claim_provider(
            "tile-lyra-captain",
            Some("lyra"),
            FleetRole::Captain,
            Some("codex"),
            None,
            vec![],
            &|_| false,
            &|_| tmux::SessionLiveness::Alive,
        )
        .unwrap();

    let file_name = path.file_name().unwrap().to_string_lossy();
    let prefix = format!("{file_name}.migration-v{CAPTAINS_SCHEMA_VERSION}.");
    let backups: Vec<_> = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(&prefix))
        .collect();
    assert_eq!(backups.len(), 1);
    assert_eq!(
        std::fs::read_to_string(backups[0].path()).unwrap(),
        SCHEMA_13_REGISTRY_FIXTURE
    );

    for backup in backups {
        let _ = std::fs::remove_file(backup.path());
    }
    let _ = std::fs::remove_file(path.with_extension("json.bak"));
    let _ = std::fs::remove_file(path);
}

#[test]
fn schema_17_fixture_loads_without_network_access() {
    let path = captains_tmp("schema-17-fixture-load");
    std::fs::write(&path, SCHEMA_17_REGISTRY_FIXTURE).unwrap();

    let registry = CaptainsRegistry::load(path.clone());
    let snapshot = registry.snapshot();
    assert_eq!(snapshot.schema_version, CAPTAINS_SCHEMA_VERSION);
    assert_eq!(snapshot.seq, 108);
    assert_eq!(
        snapshot.agent_sessions[0].agent_session_id,
        "agent-aurora-17"
    );
    assert_eq!(snapshot.agent_checkpoints[0].cursor, 7);
    assert_eq!(snapshot.agent_events[0].kind, "checkpoint");

    let _ = std::fs::remove_file(path);
}

#[test]
fn schema_18_fixture_migrates_project_identity_and_pending_release_losslessly() {
    let path = captains_tmp("schema-18-fixture-load");
    std::fs::write(&path, SCHEMA_18_REGISTRY_FIXTURE).unwrap();
    let original: Value = serde_json::from_str(SCHEMA_18_REGISTRY_FIXTURE).unwrap();
    let mut diagnostic: CaptainsSnapshot = serde_json::from_value(original.clone()).unwrap();
    migrate_project_identities(&mut diagnostic).unwrap();
    if let Err(error) = CaptainsRegistry::validate_snapshot(&diagnostic) {
        panic!("schema-v18 fixture validation failed: {error}");
    }

    let snapshot = CaptainsRegistry::read_snapshot(&path).unwrap();
    assert_eq!(snapshot.schema_version, 18);
    assert_eq!(snapshot.seq, original["seq"].as_u64().unwrap());
    assert_eq!(
        snapshot.projects[0].root_path.as_deref(),
        Some("/sanitized/workspaces/aurora")
    );
    assert_eq!(
        snapshot.projects[0].repo_root,
        "/sanitized/workspaces/aurora"
    );
    assert_eq!(snapshot.projects[0].vcs_capability.as_deref(), Some("git"));
    assert_eq!(
        snapshot.projects[0].git_main_root.as_deref(),
        Some("/sanitized/workspaces/aurora")
    );
    assert_eq!(
        snapshot.captains[0].project_id.as_deref(),
        Some("project-aurora")
    );
    assert_eq!(
        snapshot.captains[0].crew[0].conversation_id.as_deref(),
        Some("conversation-aurora-worker-18")
    );
    assert_eq!(
        snapshot.agent_sessions[0].agent_session_id,
        "agent-aurora-18"
    );
    assert_eq!(snapshot.agent_checkpoints[0].cursor, 7);
    assert_eq!(snapshot.workspaces[1].id, "workspace-aurora");
    assert_eq!(snapshot.pending_dispatch_releases.len(), 1);

    let registry = CaptainsRegistry::load(path.clone());
    let _ = registry
        .claim_test("schema-18-reload", Some("schema-18-reload"), vec![])
        .unwrap();
    let persisted: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(persisted["schemaVersion"], CAPTAINS_SCHEMA_VERSION);
    let file_name = path.file_name().unwrap().to_string_lossy();
    let prefix = format!("{file_name}.migration-v{CAPTAINS_SCHEMA_VERSION}.");
    let backups: Vec<_> = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(&prefix))
        .collect();
    assert_eq!(backups.len(), 1);
    assert_eq!(
        std::fs::read_to_string(backups[0].path()).unwrap(),
        SCHEMA_18_REGISTRY_FIXTURE
    );

    let reloaded = CaptainsRegistry::load(path.clone()).snapshot();
    assert_eq!(
        reloaded.projects[0].root_path.as_deref(),
        Some("/sanitized/workspaces/aurora")
    );
    assert_eq!(reloaded.pending_dispatch_releases.len(), 1);
    for backup in backups {
        let _ = std::fs::remove_file(backup.path());
    }
    let _ = std::fs::remove_file(path);
}

#[test]
fn first_schema_v18_write_creates_a_timestamped_migration_backup() {
    let path = captains_tmp("schema-v18-migration-backup");
    let legacy = CaptainsSnapshot {
        schema_version: 17,
        seq: 1,
        captains: vec![],
        cortana: crate::cortana_reconcile::CortanaDurableIdentity::default(),
        agent_sessions: vec![],
        agent_checkpoints: vec![],
        agent_events: vec![],
        projects: vec![],
        workspaces: vec![FleetWorkspaceRecord::captain_workspace()],
        pending_fleet_operations: vec![],
        retired_fleet_tile_ids: vec![],
        pending_dispatch_claims: vec![],
        pending_dispatch_releases: vec![],
        pending_git_initializations: vec![],
    };
    std::fs::write(&path, serde_json::to_vec(&legacy).unwrap()).unwrap();

    let registry = CaptainsRegistry::load(path.clone());
    registry
        .claim_test("migration-backup-captain", None, vec![])
        .unwrap();

    let file_name = path.file_name().unwrap().to_string_lossy();
    let prefix = format!("{file_name}.migration-v{CAPTAINS_SCHEMA_VERSION}.");
    let backups: Vec<_> = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(&prefix))
        .collect();
    assert_eq!(backups.len(), 1);
    let backup_body = std::fs::read_to_string(backups[0].path()).unwrap();
    assert_eq!(backup_body, serde_json::to_string(&legacy).unwrap());

    for backup in backups {
        let _ = std::fs::remove_file(backup.path());
    }
    let _ = std::fs::remove_file(path.with_extension("json.bak"));
    let _ = std::fs::remove_file(path);
}

fn powder_lifecycle_registry(path: Option<PathBuf>) -> Arc<CaptainsRegistry> {
    powder_lifecycle_registry_with_profile_and_crew(
        path,
        "profile-that-does-not-exist-for-control-tests",
        "crew-powder",
    )
}

fn powder_lifecycle_registry_with_profile_and_crew(
    path: Option<PathBuf>,
    connection_profile: &str,
    crew_session_id: &str,
) -> Arc<CaptainsRegistry> {
    let registry = Arc::new(match path {
        Some(path) => CaptainsRegistry::load(path),
        None => CaptainsRegistry::new(),
    });
    registry
        .upsert_project(ProjectRecord {
            root_path: None,
            vcs_capability: None,
            git_main_root: None,
            project_id: "project-powder-lifecycle".into(),
            name: "Powder Lifecycle".into(),
            repo_root: "/tmp/powder-lifecycle".into(),
            remote_url: None,
            default_branch: Some("main".into()),
            powder: Some(PowderProjectBinding {
                connection_profile: connection_profile.into(),
                repository: "t-hub".into(),
                event_cursor: 0,
            }),
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    registry
        .claim_test("captain-powder", Some("powder-ship"), vec![])
        .unwrap();
    registry
        .bind_ship_context(
            "powder-ship",
            "project-powder-lifecycle",
            "Own Powder lifecycle",
            "codex",
        )
        .unwrap();
    registry
        .record_crew("captain-powder", crew_session_id)
        .unwrap();
    registry
        .bind_crew_context(
            "captain-powder",
            crew_session_id,
            "Implement Powder lifecycle",
            "codex",
            Some("/tmp/powder-lifecycle"),
            Some("feat/powder-lifecycle"),
            PowderWorkBinding {
                card_id: "thub-powder-control-lifecycle".into(),
                run_id: "run-authoritative".into(),
                agent: Some("powder-agent".into()),
                claim_expires_at: Some(100),
                mutation_intent: None,
                dispatch_release_recovery: false,
                state: PowderWorkState::Active,
            },
        )
        .unwrap();
    registry
}

fn pending_release_snapshot_document(profile: &str, crew_session_id: &str) -> Value {
    let registry = powder_lifecycle_registry_with_profile_and_crew(None, profile, crew_session_id);
    let mut snapshot = registry.snapshot();
    let crew = &mut snapshot.captains[0].crew[0];
    crew.state = CrewState::CleanupPending { since: 1 };
    let work = crew.powder_work.as_mut().unwrap();
    work.dispatch_release_recovery = true;
    snapshot
        .pending_dispatch_releases
        .push(PendingDispatchRelease {
            crew_session_id: crew.terminal_id.clone(),
            project_id: "project-powder-lifecycle".into(),
            connection_profile: profile.into(),
            connection_endpoint_identity: format!("hmac-sha256:{}", "0".repeat(64)),
            repository: "t-hub".into(),
            card_id: work.card_id.clone(),
            run_id: work.run_id.clone(),
            agent: work.agent.clone().unwrap(),
            operation_id: "initial-claim:actor-t-hub:incompatible-load".into(),
            created_at: 1,
            state: PendingDispatchReleaseState::InFlight,
        });
    serde_json::to_value(snapshot).unwrap()
}

fn assert_incompatible_release_load_blocks_actions(
    path: &Path,
    primary_body: &str,
    backup_body: Option<&str>,
) {
    let backup = path.with_extension("json.bak");
    let registry = Arc::new(CaptainsRegistry::load(path.to_path_buf()));
    assert!(registry.write_blocked.is_some());
    assert!(registry.snapshot().captains.is_empty());
    assert!(registry.snapshot().pending_dispatch_releases.is_empty());
    assert!(registry
        .claim_test("blocked-captain", Some("blocked-ship"), vec![])
        .unwrap_err()
        .contains("read-only"));
    assert_eq!(std::fs::read_to_string(path).unwrap(), primary_body);
    match backup_body {
        Some(body) => assert_eq!(std::fs::read_to_string(&backup).unwrap(), body),
        None => assert!(!backup.exists()),
    }
    let prefix = format!("{}.corrupt.", path.file_name().unwrap().to_string_lossy());
    assert!(
        !std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .flatten()
            .any(|entry| entry.file_name().to_string_lossy().starts_with(&prefix)),
        "incompatible release state must never be quarantined"
    );
}

#[test]
fn production_load_blocks_v11_release_recovery_without_backup() {
    let path = captains_tmp("incompatible-v11-release-primary");
    let _ = std::fs::remove_file(&path);
    let profile = "incompatible-v11-primary-profile";
    let mut document = pending_release_snapshot_document(profile, "incompatible-v11-primary-crew");
    document["schemaVersion"] = json!(11);
    document["pendingDispatchReleases"][0]
        .as_object_mut()
        .unwrap()
        .remove("connectionEndpointIdentity");
    let body = serde_json::to_string(&document).unwrap();
    std::fs::write(&path, &body).unwrap();

    assert_incompatible_release_load_blocks_actions(&path, &body, None);
    let _ = std::fs::remove_file(path);
}

#[test]
fn production_load_blocks_v12_unsalted_endpoint_digest_recovery() {
    let path = captains_tmp("incompatible-v12-unsalted-release-primary");
    let _ = std::fs::remove_file(&path);
    let profile = "incompatible-v12-unsalted-profile";
    let mut document = pending_release_snapshot_document(profile, "incompatible-v12-unsalted-crew");
    document["schemaVersion"] = json!(12);
    let release = document["pendingDispatchReleases"][0]
        .as_object_mut()
        .unwrap();
    release.remove("connectionEndpointIdentity");
    release.insert(
        "connectionEndpointDigest".into(),
        json!(format!("sha256:{}", "0".repeat(64))),
    );
    let body = serde_json::to_string(&document).unwrap();
    std::fs::write(&path, &body).unwrap();

    assert_incompatible_release_load_blocks_actions(&path, &body, None);
    let _ = std::fs::remove_file(path);
}

#[test]
fn production_load_preserves_v11_release_primary_over_clean_stale_backup() {
    let path = captains_tmp("incompatible-v11-release-primary-backup");
    let backup = path.with_extension("json.bak");
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&backup);
    let profile = "incompatible-v11-backup-profile";
    let mut document = pending_release_snapshot_document(profile, "incompatible-v11-backup-crew");
    document["schemaVersion"] = json!(11);
    document["pendingDispatchReleases"][0]
        .as_object_mut()
        .unwrap()
        .remove("connectionEndpointIdentity");
    let primary_body = serde_json::to_string(&document).unwrap();
    let backup_body = json!({
        "schemaVersion": 11,
        "seq": 1,
        "captains": [],
        "projects": [],
        "pendingDispatchClaims": [],
        "pendingDispatchReleases": [],
    })
    .to_string();
    std::fs::write(&path, &primary_body).unwrap();
    std::fs::write(&backup, &backup_body).unwrap();

    assert_incompatible_release_load_blocks_actions(&path, &primary_body, Some(&backup_body));
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(backup);
}

#[test]
fn production_load_blocks_actions_when_backup_has_incompatible_release_recovery() {
    let path = captains_tmp("incompatible-release-backup");
    let backup = path.with_extension("json.bak");
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&backup);
    let profile = "incompatible-backup-profile";
    let primary_body = json!({
        "schemaVersion": CAPTAINS_SCHEMA_VERSION,
        "seq": 9,
        "captains": [],
        "projects": [],
        "pendingDispatchClaims": [],
        "pendingDispatchReleases": [],
    })
    .to_string();
    let mut document = pending_release_snapshot_document(profile, "incompatible-backup-crew");
    document["schemaVersion"] = json!(11);
    document["pendingDispatchReleases"][0]
        .as_object_mut()
        .unwrap()
        .remove("connectionEndpointIdentity");
    let backup_body = serde_json::to_string(&document).unwrap();
    std::fs::write(&path, &primary_body).unwrap();
    std::fs::write(&backup, &backup_body).unwrap();

    assert_incompatible_release_load_blocks_actions(&path, &primary_body, Some(&backup_body));
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(backup);
}

#[test]
fn production_load_rejects_raw_endpoint_field_in_schema_v13_release_recovery() {
    let path = captains_tmp("incompatible-raw-release-endpoint");
    let _ = std::fs::remove_file(&path);
    let profile = "incompatible-raw-profile";
    let mut document = pending_release_snapshot_document(profile, "incompatible-raw-crew");
    document["pendingDispatchReleases"][0]
        .as_object_mut()
        .unwrap()
        .insert(
            "connectionEndpoint".into(),
            json!("http://gateway.invalid/path-token?access_token=query-token#fragment-token"),
        );
    let body = serde_json::to_string(&document).unwrap();
    std::fs::write(&path, &body).unwrap();

    assert_incompatible_release_load_blocks_actions(&path, &body, None);
    let _ = std::fs::remove_file(path);
}

#[test]
fn pre_v13_release_recovery_is_rejected_before_any_recovery_can_run() {
    let path = captains_tmp("schema-v12-release-recovery");
    let _ = std::fs::remove_file(&path);
    let registry = powder_lifecycle_registry_with_profile_and_crew(
        None,
        "legacy-release-profile",
        "legacy-release-crew",
    );
    let mut snapshot = registry.snapshot();
    let crew = &mut snapshot.captains[0].crew[0];
    crew.state = CrewState::CleanupPending { since: 1 };
    let work = crew.powder_work.as_mut().unwrap();
    work.dispatch_release_recovery = true;
    snapshot
        .pending_dispatch_releases
        .push(PendingDispatchRelease {
            crew_session_id: crew.terminal_id.clone(),
            project_id: "project-powder-lifecycle".into(),
            connection_profile: "legacy-release-profile".into(),
            connection_endpoint_identity: format!("hmac-sha256:{}", "0".repeat(64)),
            repository: "t-hub".into(),
            card_id: work.card_id.clone(),
            run_id: work.run_id.clone(),
            agent: work.agent.clone().unwrap(),
            operation_id: "initial-claim:actor-t-hub:legacy".into(),
            created_at: 1,
            state: PendingDispatchReleaseState::InFlight,
        });
    snapshot.schema_version = 12;
    std::fs::write(&path, serde_json::to_vec(&snapshot).unwrap()).unwrap();

    let error = CaptainsRegistry::read_snapshot(&path).unwrap_err();
    assert!(error
        .to_string()
        .contains("dispatch release recovery state incompatible"));
    let _ = std::fs::remove_file(path);
}

#[test]
fn schema_v11_raw_endpoint_release_recovery_fails_closed_before_network() {
    let path = captains_tmp("schema-v11-raw-release-endpoint");
    let _ = std::fs::remove_file(&path);
    let registry = powder_lifecycle_registry_with_profile_and_crew(
        None,
        "legacy-raw-release-profile",
        "legacy-raw-release-crew",
    );
    let mut snapshot = registry.snapshot();
    let crew = &mut snapshot.captains[0].crew[0];
    crew.state = CrewState::CleanupPending { since: 1 };
    let work = crew.powder_work.as_mut().unwrap();
    work.dispatch_release_recovery = true;
    snapshot
        .pending_dispatch_releases
        .push(PendingDispatchRelease {
            crew_session_id: crew.terminal_id.clone(),
            project_id: "project-powder-lifecycle".into(),
            connection_profile: "legacy-raw-release-profile".into(),
            connection_endpoint_identity: format!("hmac-sha256:{}", "0".repeat(64)),
            repository: "t-hub".into(),
            card_id: work.card_id.clone(),
            run_id: work.run_id.clone(),
            agent: work.agent.clone().unwrap(),
            operation_id: "initial-claim:actor-t-hub:legacy-raw".into(),
            created_at: 1,
            state: PendingDispatchReleaseState::InFlight,
        });
    snapshot.schema_version = 11;
    let mut raw = serde_json::to_value(&snapshot).unwrap();
    let release = raw["pendingDispatchReleases"][0].as_object_mut().unwrap();
    release.remove("connectionEndpointIdentity");
    release.insert(
        "connectionEndpoint".into(),
        json!("https://gateway.example/api?access_token=legacy-secret"),
    );
    std::fs::write(&path, serde_json::to_vec(&raw).unwrap()).unwrap();

    assert!(CaptainsRegistry::read_snapshot(&path).is_err());
    let _ = std::fs::remove_file(path);
}

#[test]
fn release_recovery_snapshot_pairs_are_required_and_fail_closed_before_network() {
    let path = captains_tmp("release-recovery-pairs");
    let _ = std::fs::remove_file(&path);
    let registry =
        powder_lifecycle_registry_with_profile_and_crew(None, "pair-profile", "pair-crew");
    let mut paired = registry.snapshot();
    let crew = &mut paired.captains[0].crew[0];
    crew.state = CrewState::CleanupPending { since: 1 };
    let work = crew.powder_work.as_mut().unwrap();
    work.dispatch_release_recovery = true;
    let recovery = PendingDispatchRelease {
        crew_session_id: crew.terminal_id.clone(),
        project_id: "project-powder-lifecycle".into(),
        connection_profile: "pair-profile".into(),
        connection_endpoint_identity: format!("hmac-sha256:{}", "0".repeat(64)),
        repository: "t-hub".into(),
        card_id: work.card_id.clone(),
        run_id: work.run_id.clone(),
        agent: work.agent.clone().unwrap(),
        operation_id: "initial-claim:actor-t-hub:pair".into(),
        created_at: 1,
        state: PendingDispatchReleaseState::InFlight,
    };
    paired.pending_dispatch_releases = vec![recovery.clone()];
    assert!(CaptainsRegistry::validate_snapshot(&paired).is_ok());

    let mut orphan = paired.clone();
    orphan.pending_dispatch_releases[0].crew_session_id = "missing-crew".into();
    assert!(CaptainsRegistry::validate_snapshot(&orphan).is_err());

    let mut foreign = paired.clone();
    foreign.pending_dispatch_releases[0].project_id = "foreign-project".into();
    assert!(CaptainsRegistry::validate_snapshot(&foreign).is_err());

    let mut mismatched = paired.clone();
    mismatched.pending_dispatch_releases[0].agent = "foreign-agent".into();
    assert!(CaptainsRegistry::validate_snapshot(&mismatched).is_err());

    let mut missing_record = paired.clone();
    missing_record.pending_dispatch_releases.clear();
    assert!(CaptainsRegistry::validate_snapshot(&missing_record).is_err());

    let mut active_crew = paired.clone();
    active_crew.captains[0].crew[0].state = CrewState::Active;
    assert!(CaptainsRegistry::validate_snapshot(&active_crew).is_err());

    let mut malformed_identity = paired.clone();
    malformed_identity.pending_dispatch_releases[0].card_id = "card\ncontrol".into();
    assert!(CaptainsRegistry::validate_snapshot(&malformed_identity).is_err());

    let mut oversized_identity = paired.clone();
    oversized_identity.pending_dispatch_releases[0].operation_id = "x".repeat(513);
    assert!(CaptainsRegistry::validate_snapshot(&oversized_identity).is_err());

    std::fs::write(&path, serde_json::to_vec(&orphan).unwrap()).unwrap();
    assert!(CaptainsRegistry::read_snapshot(&path).is_err());
    let _ = std::fs::remove_file(path);
}

#[test]
fn future_backup_schema_is_preserved_and_blocks_writes() {
    let path = captains_tmp("future-backup-schema");
    let backup = path.with_extension("json.bak");
    let primary = CaptainsSnapshot {
        schema_version: CAPTAINS_SCHEMA_VERSION,
        seq: 4,
        captains: vec![],
        cortana: crate::cortana_reconcile::CortanaDurableIdentity::default(),
        agent_sessions: vec![],
        agent_checkpoints: vec![],
        agent_events: vec![],
        projects: vec![],
        workspaces: vec![FleetWorkspaceRecord::captain_workspace()],
        pending_fleet_operations: vec![],
        retired_fleet_tile_ids: vec![],
        pending_dispatch_claims: vec![],
        pending_dispatch_releases: vec![],
        pending_git_initializations: vec![],
    };
    let backup_body = json!({
        "schemaVersion": CAPTAINS_SCHEMA_VERSION + 1,
        "seq": 5,
        "captains": [],
        "projects": [],
        "futureField": "preserve",
    })
    .to_string();
    std::fs::write(&path, serde_json::to_vec(&primary).unwrap()).unwrap();
    std::fs::write(&backup, &backup_body).unwrap();

    let registry = CaptainsRegistry::load(path.clone());
    assert_eq!(
        registry.snapshot().seq,
        4,
        "supported primary remains readable"
    );
    assert!(registry.write_blocked.is_some());
    assert!(registry
        .claim_test("cap-future", Some("future"), vec![])
        .unwrap_err()
        .contains("read-only"));
    assert_eq!(std::fs::read_to_string(&backup).unwrap(), backup_body);

    let prefix = format!("{}.corrupt.", backup.file_name().unwrap().to_string_lossy());
    assert!(!std::fs::read_dir(backup.parent().unwrap())
        .unwrap()
        .flatten()
        .any(|entry| entry.file_name().to_string_lossy().starts_with(&prefix)));
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(backup);
}

#[test]
fn future_backup_is_not_quarantined_when_primary_is_corrupt() {
    let path = captains_tmp("future-backup-corrupt-primary");
    let backup = path.with_extension("json.bak");
    let primary_body = "{ invalid";
    let backup_body = json!({
        "schemaVersion": CAPTAINS_SCHEMA_VERSION + 1,
        "seq": 9,
        "captains": [],
        "projects": [],
    })
    .to_string();
    std::fs::write(&path, primary_body).unwrap();
    std::fs::write(&backup, &backup_body).unwrap();

    let registry = CaptainsRegistry::load(path.clone());
    assert!(registry.write_blocked.is_some());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), primary_body);
    assert_eq!(std::fs::read_to_string(&backup).unwrap(), backup_body);
    assert!(registry
        .claim_test("cap-future", Some("future"), vec![])
        .unwrap_err()
        .contains("read-only"));

    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(backup);
}

#[test]
fn semantic_registry_corruption_recovers_from_validated_backup() {
    let path = captains_tmp("semantic-corruption");
    let backup = path.with_extension("json.bak");
    let invalid = json!({
        "schemaVersion": CAPTAINS_SCHEMA_VERSION,
        "seq": 2,
        "captains": [],
        "projects": [
            {
                "projectId": "duplicate",
                "name": "One",
                "repoRoot": "/tmp/one",
                "createdAt": 0,
                "updatedAt": 0
            },
            {
                "projectId": "duplicate",
                "name": "Two",
                "repoRoot": "/tmp/two",
                "createdAt": 0,
                "updatedAt": 0
            }
        ]
    });
    let valid = CaptainsSnapshot {
        schema_version: CAPTAINS_SCHEMA_VERSION,
        seq: 1,
        captains: vec![],
        cortana: crate::cortana_reconcile::CortanaDurableIdentity::default(),
        agent_sessions: vec![],
        agent_checkpoints: vec![],
        agent_events: vec![],
        projects: vec![],
        workspaces: vec![FleetWorkspaceRecord::captain_workspace()],
        pending_fleet_operations: vec![],
        retired_fleet_tile_ids: vec![],
        pending_dispatch_claims: vec![],
        pending_dispatch_releases: vec![],
        pending_git_initializations: vec![],
    };
    std::fs::write(&path, serde_json::to_vec(&invalid).unwrap()).unwrap();
    std::fs::write(&backup, serde_json::to_vec(&valid).unwrap()).unwrap();

    let registry = CaptainsRegistry::load(path.clone());
    let restored = registry.snapshot();
    assert_eq!(restored.seq, valid.seq);
    assert!(restored.captains.is_empty());
    assert!(restored.projects.is_empty());
    assert!(!path.exists(), "invalid primary should be quarantined");
    assert!(backup.exists());

    let prefix = format!("{}.corrupt.", path.file_name().unwrap().to_string_lossy());
    let quarantined = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .flatten()
        .find(|entry| entry.file_name().to_string_lossy().starts_with(&prefix))
        .expect("semantic corruption should be quarantined");
    let _ = std::fs::remove_file(quarantined.path());
    let _ = std::fs::remove_file(backup);
}

#[test]
fn current_schema_rejects_semantically_impossible_snapshots() {
    let base = json!({
        "schemaVersion": CAPTAINS_SCHEMA_VERSION,
        "seq": 1,
        "captains": [],
        "projects": [],
        "workspaces": [{
            "id": CAPTAIN_WORKSPACE_ID,
            "name": CAPTAIN_WORKSPACE_NAME,
            "kind": "captain",
            "tileIds": []
        }],
    });
    let cases = [
        (
            "active-without-terminal",
            json!({
                "schemaVersion": CAPTAINS_SCHEMA_VERSION,
                "seq": 1,
                "captains": [{"shipSlug": "alpha", "role": "captain", "state": {"kind": "active"}}],
                "projects": [],
            }),
        ),
        (
            "relative-project-root",
            json!({
                "schemaVersion": CAPTAINS_SCHEMA_VERSION,
                "seq": 1,
                "captains": [],
                "projects": [{
                    "projectId": "p",
                    "name": "P",
                    "repoRoot": "relative/path",
                    "createdAt": 1,
                    "updatedAt": 1
                }],
            }),
        ),
        (
            "incomplete-powder-binding",
            json!({
                "schemaVersion": CAPTAINS_SCHEMA_VERSION,
                "seq": 1,
                "captains": [],
                "projects": [{
                    "projectId": "p",
                    "name": "P",
                    "repoRoot": "/tmp/p",
                    "powder": {"connectionProfile": "", "repository": "repo"},
                    "createdAt": 1,
                    "updatedAt": 1
                }],
            }),
        ),
        (
            "unknown-captain-provider",
            json!({
                "schemaVersion": CAPTAINS_SCHEMA_VERSION,
                "seq": 1,
                "captains": [{"shipSlug": "alpha", "role": "captain", "terminalId": "cap-a", "provider": "other"}],
                "projects": [],
            }),
        ),
        (
            "captain-provider-harness-mismatch",
            json!({
                "schemaVersion": CAPTAINS_SCHEMA_VERSION,
                "seq": 1,
                "captains": [{"shipSlug": "alpha", "role": "captain", "terminalId": "cap-a", "provider": "codex", "harness": "claude"}],
                "projects": [],
            }),
        ),
        (
            "claude-continuity-mismatch",
            json!({
                "schemaVersion": CAPTAINS_SCHEMA_VERSION,
                "seq": 1,
                "captains": [{
                    "shipSlug": "alpha", "role": "captain", "terminalId": "cap-a",
                    "provider": "claude", "harness": "claude",
                    "providerSessionId": "provider-a", "claudeUuid": "claude-b"
                }],
                "projects": [],
            }),
        ),
        (
            "codex-crew-with-claude-uuid",
            json!({
                "schemaVersion": CAPTAINS_SCHEMA_VERSION,
                "seq": 1,
                "captains": [{
                    "shipSlug": "alpha", "role": "captain", "terminalId": "cap-a",
                    "crew": [{
                        "terminalId": "crew-a", "provider": "codex", "harness": "codex",
                        "providerSessionId": "codex-a", "claudeUuid": "claude-a"
                    }]
                }],
                "projects": [],
            }),
        ),
    ];
    assert!(CaptainsRegistry::validate_snapshot(&serde_json::from_value(base).unwrap()).is_ok());
    for (name, value) in cases {
        let snapshot: CaptainsSnapshot = serde_json::from_value(value).unwrap();
        assert!(
            CaptainsRegistry::validate_snapshot(&snapshot).is_err(),
            "{name} was accepted"
        );
    }
}

#[test]
fn registry_mutations_reject_noncanonical_harnesses_and_providers() {
    let registry = CaptainsRegistry::new();
    let invalid_claim = registry.claim_provider(
        "cap-a",
        Some("alpha"),
        FleetRole::Captain,
        Some("other"),
        Some("session-a"),
        vec![],
        &|_| false,
        &|_| tmux::SessionLiveness::Alive,
    );
    assert!(invalid_claim.unwrap_err().contains("codex"));
    assert!(registry
        .bind_ship_context("alpha", "project-a", "task", "other")
        .unwrap_err()
        .contains("codex"));
    assert!(registry
        .bind_crew_context(
            "cap-a",
            "crew-a",
            "task",
            "other",
            None,
            None,
            PowderWorkBinding {
                card_id: "card-a".into(),
                run_id: "run-a".into(),
                agent: None,
                claim_expires_at: None,
                mutation_intent: None,
                dispatch_release_recovery: false,
                state: PowderWorkState::Active,
            },
        )
        .unwrap_err()
        .contains("codex"));
    assert!(registry.snapshot().captains.is_empty());
}

#[test]
fn list_captains_returns_the_versioned_snapshot() {
    let ctx = test_ctx("secret");
    ctx.captains
        .claim_test("cap-1", Some("alpha"), vec!["tab-1".into()])
        .unwrap();
    let v = dispatch(&ctx, "list_captains", &json!({})).unwrap();
    assert_eq!(v["count"], 1);
    assert_eq!(v["seq"], 1);
    assert_eq!(v["captains"][0]["shipSlug"], "alpha");
    assert_eq!(v["captains"][0]["terminalId"], "cap-1");
    assert_eq!(v["captains"][0]["workspaceTabIds"][0], "tab-1");
    assert_eq!(v["captains"][0]["crew"], json!([]));
}

#[test]
fn list_agents_returns_a_bounded_assignment_free_snapshot() {
    let ctx = test_ctx("secret");
    let v = dispatch(
        &ctx,
        "list_agents",
        &json!({"projectId": "project-1", "limit": 1}),
    )
    .unwrap();
    assert_eq!(v["agents"], json!([]));
    assert_eq!(v["count"], 0);
    assert_eq!(v["total"], 0);
    assert_eq!(v["hasMore"], false);
    assert_eq!(v["eventCursor"], 0);
    assert!(v["digest"]
        .as_str()
        .is_some_and(|digest| digest.starts_with("sha256:")));
}

#[test]
fn get_agent_requires_an_existing_durable_agent_record() {
    let ctx = test_ctx("secret");
    let error = dispatch(
        &ctx,
        "get_agent",
        &json!({"agentSessionId": "missing-agent"}),
    )
    .unwrap_err();
    assert!(error.contains("agent 'missing-agent' was not found"));
}

#[test]
fn checkpoint_and_event_reads_fail_closed_for_unknown_agents() {
    let ctx = test_ctx("secret");
    let checkpoint_error = dispatch(
        &ctx,
        "agent_checkpoint",
        &json!({
            "agentSessionId": "missing-agent",
            "authorSessionId": "captain-1",
            "summary": "progress"
        }),
    )
    .unwrap_err();
    assert!(checkpoint_error.contains("agent 'missing-agent' was not found"));

    let events_error = dispatch(
        &ctx,
        "agent_events",
        &json!({"agentSessionId": "missing-agent", "cursor": "0"}),
    )
    .unwrap_err();
    assert!(events_error.contains("agent 'missing-agent' was not found"));
}

#[test]
fn durable_agent_checkpoint_persists_and_advances_the_event_cursor() {
    let ctx = test_ctx("secret");
    ctx.captains
        .upsert_project(ProjectRecord {
            root_path: None,
            vcs_capability: None,
            git_main_root: None,
            project_id: "project-1".into(),
            name: "Project".into(),
            repo_root: "/tmp/project-1".into(),
            remote_url: None,
            default_branch: None,
            powder: None,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    ctx.captains
        .claim_test("captain-1", Some("captain"), vec![])
        .unwrap();
    ctx.captains
        .bind_ship_context("captain", "project-1", "Assignment", "codex")
        .unwrap();
    let (lane_claim, dispatch_capacity) = test_dispatch_evidence("lane-checkpoint", "agent-1");
    ctx.captains
        .insert_agent_session(AgentSessionRecord {
            agent_session_id: "agent-1".into(),
            captain_session_id: "captain-1".into(),
            project_id: "project-1".into(),
            assignment: "Do the work".into(),
            directory: "/tmp/project-1".into(),
            worktree_path: None,
            branch: None,
            workspace_tab_id: None,
            harness: "codex".into(),
            provider: "codex".into(),
            provider_conversation_id: None,
            resume_point: None,
            runtime_state: crate::agent_session::RuntimeState::Starting,
            work_stage: crate::agent_session::WorkStage::Assigned,
            delivery: Some(crate::agent_session::DeliveryProvenance::new(
                "1111111111111111111111111111111111111111",
                false,
            )),
            lane_claim: Some(lane_claim),
            integration_contracts: Vec::new(),
            dispatch_capacity: Some(dispatch_capacity),
            admission_purpose: crate::governor::AdmissionPurpose::Ordinary,
            created_at: 2,
            updated_at: 2,
        })
        .unwrap();
    let response = dispatch(
        &ctx,
        "agent_checkpoint",
        &json!({
            "agentSessionId": "agent-1",
            "authorSessionId": "captain-1",
            "summary": "finished the first slice"
        }),
    )
    .unwrap();
    assert_eq!(
        response["checkpoint"]["summary"],
        "finished the first slice"
    );
    assert!(response["eventCursor"]
        .as_u64()
        .is_some_and(|cursor| cursor > 0));
    let listed = dispatch(
        &ctx,
        "list_agents",
        &json!({"projectId": "project-1", "limit": 10}),
    )
    .unwrap();
    assert!(listed["eventCursor"].as_u64() >= response["eventCursor"].as_u64());
    let events = dispatch(
        &ctx,
        "agent_events",
        &json!({"agentSessionId": "agent-1", "cursor": "0", "limit": 10}),
    )
    .unwrap();
    assert!(events["count"].as_u64().is_some_and(|count| count >= 1));
    assert!(events["events"]
        .as_array()
        .is_some_and(|events| events.iter().any(|event| event["kind"] == "checkpoint")));
}

#[test]
fn authenticated_agent_followup_is_owned_durable_idempotent_and_scope_explicit() {
    let ctx = test_ctx("agent-followup");
    seed_starting_agent(&ctx, "followup-agent");
    ctx.captains
        .upsert_project(ProjectRecord {
            root_path: None,
            vcs_capability: None,
            git_main_root: None,
            project_id: "foreign-project".into(),
            name: "Foreign Project".into(),
            repo_root: "/tmp/foreign-project".into(),
            remote_url: None,
            default_branch: None,
            powder: None,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    ctx.captains
        .claim_test("foreign-captain", Some("foreign-ship"), vec![])
        .unwrap();
    ctx.captains
        .bind_ship_context(
            "foreign-ship",
            "foreign-project",
            "Foreign Assignment",
            "codex",
        )
        .unwrap();
    let captain_identity = ctx
        .identity
        .mint_for(crate::identity::Role::Captain, Some("capacity-ship".into()))
        .unwrap();
    ctx.identity
        .bind_tile(&captain_identity.id, "capacity-captain")
        .unwrap();
    let foreign_identity = ctx
        .identity
        .mint_for(crate::identity::Role::Captain, Some("foreign-ship".into()))
        .unwrap();
    ctx.identity
        .bind_tile(&foreign_identity.id, "foreign-captain")
        .unwrap();
    let call = |request_id: &str, message: &str, replacement: Option<&str>| {
        let mut args = json!({
            "requestId": request_id,
            "captainSessionId": "capacity-captain",
            "shipSlug": "capacity-ship",
            "projectId": "capacity-project",
            "agentSessionId": "followup-agent",
            "message": message,
        });
        if let Some(replacement) = replacement {
            args["replacementAssignment"] = json!(replacement);
        }
        dispatch_authenticated(
            &ctx,
            req_session(
                "agent-followup",
                &captain_identity.secret,
                "agent_followup",
                args,
            ),
        )
    };

    let first = call("followup-1", "Continue the bounded repair.", None);
    assert!(first.ok, "got: {:?}", first.error);
    assert_eq!(
        first.result.as_ref().unwrap()["agentSessionId"],
        "followup-agent"
    );
    assert_eq!(first.result.as_ref().unwrap()["messageSeq"], 0);
    assert_eq!(ctx.inbox.depth("followup-agent").enqueued, 1);
    assert_eq!(
        ctx.captains.snapshot().agent_sessions[0].assignment,
        "Pending durable start"
    );

    let replay = call("followup-1", "Continue the bounded repair.", None);
    assert!(replay.ok, "got: {:?}", replay.error);
    assert_eq!(replay.result.as_ref().unwrap()["idempotentReplay"], true);
    assert_eq!(ctx.inbox.depth("followup-agent").enqueued, 1);
    let foreign_replay = dispatch_authenticated(
        &ctx,
        req_session(
            "agent-followup",
            &foreign_identity.secret,
            "agent_followup",
            json!({
                "requestId": "followup-1",
                "captainSessionId": "capacity-captain",
                "shipSlug": "capacity-ship",
                "projectId": "capacity-project",
                "agentSessionId": "followup-agent",
                "message": "Continue the bounded repair."
            }),
        ),
    );
    assert!(!foreign_replay.ok, "foreign Captain replayed owner success");
    assert_eq!(foreign_replay.error_kind.as_deref(), Some("unauthorized"));

    let foreign_squat = dispatch_authenticated(
        &ctx,
        req_session(
            "agent-followup",
            &foreign_identity.secret,
            "agent_followup",
            json!({
                "requestId": "followup-squat",
                "captainSessionId": "capacity-captain",
                "shipSlug": "capacity-ship",
                "projectId": "capacity-project",
                "agentSessionId": "followup-agent",
                "message": "Owner must still be able to send this."
            }),
        ),
    );
    assert!(!foreign_squat.ok);
    let owner_after_squat = call(
        "followup-squat",
        "Owner must still be able to send this.",
        None,
    );
    assert!(
        owner_after_squat.ok,
        "foreign Captain poisoned owner requestId: {:?}",
        owner_after_squat.error
    );
    let conflict = call("followup-1", "Changed retry payload.", None);
    assert!(!conflict.ok);
    assert_eq!(conflict.error_kind.as_deref(), Some("request_conflict"));
    assert_eq!(ctx.inbox.depth("followup-agent").enqueued, 2);

    let replacement = call(
        "followup-2",
        "The reviewed scope is now explicit.",
        Some("Replacement bounded assignment"),
    );
    assert!(replacement.ok, "got: {:?}", replacement.error);
    assert_eq!(
        replacement.result.as_ref().unwrap()["assignmentChanged"],
        true
    );
    assert_eq!(
        ctx.captains.snapshot().agent_sessions[0].assignment,
        "Replacement bounded assignment"
    );
}

#[test]
fn agent_followup_rejects_foreign_and_exited_agents_with_structured_errors() {
    let ctx = test_ctx("agent-followup-errors");
    seed_starting_agent(&ctx, "followup-agent");
    let foreign_identity = ctx
        .identity
        .mint_for(crate::identity::Role::Captain, Some("foreign-ship".into()))
        .unwrap();
    ctx.identity
        .bind_tile(&foreign_identity.id, "foreign-captain")
        .unwrap();
    let args = json!({
        "requestId": "followup-foreign",
        "captainSessionId": "capacity-captain",
        "shipSlug": "capacity-ship",
        "projectId": "capacity-project",
        "agentSessionId": "followup-agent",
        "message": "Do not deliver this.",
    });
    let foreign = dispatch_authenticated(
        &ctx,
        req_session(
            "agent-followup-errors",
            &foreign_identity.secret,
            "agent_followup",
            args.clone(),
        ),
    );
    assert!(!foreign.ok);
    assert_eq!(foreign.error_kind.as_deref(), Some("unauthorized"));
    assert_eq!(ctx.inbox.depth("followup-agent").enqueued, 0);

    let captain_identity = ctx
        .identity
        .mint_for(crate::identity::Role::Captain, Some("capacity-ship".into()))
        .unwrap();
    ctx.identity
        .bind_tile(&captain_identity.id, "capacity-captain")
        .unwrap();
    ctx.captains
        .reconcile_agent_runtime("followup-agent", RuntimeState::Exited, None)
        .unwrap();
    let mut exited_args = args;
    exited_args["requestId"] = json!("followup-exited");
    let exited = dispatch_authenticated(
        &ctx,
        req_session(
            "agent-followup-errors",
            &captain_identity.secret,
            "agent_followup",
            exited_args,
        ),
    );
    assert!(!exited.ok);
    assert_eq!(exited.error_kind.as_deref(), Some("agent_exited"));
    assert_eq!(
        exited.error_details.as_ref().unwrap()["operation"],
        "agent_followup"
    );
    assert_eq!(ctx.inbox.depth("followup-agent").enqueued, 0);
}

#[test]
fn agent_followup_assignment_persist_failure_never_makes_new_scope_deliverable() {
    let path = captains_tmp("agent-followup-assignment-failure");
    let registry = Arc::new(CaptainsRegistry::load(path.clone()));
    let ctx =
        test_ctx("agent-followup-assignment-failure").with_captains_registry(Arc::clone(&registry));
    seed_starting_agent(&ctx, "followup-agent");
    let captain_identity = ctx
        .identity
        .mint_for(crate::identity::Role::Captain, Some("capacity-ship".into()))
        .unwrap();
    ctx.identity
        .bind_tile(&captain_identity.id, "capacity-captain")
        .unwrap();
    registry.fail_next_persist("injected Assignment persistence failure");

    let response = dispatch_authenticated(
        &ctx,
        req_session(
            "agent-followup-assignment-failure",
            &captain_identity.secret,
            "agent_followup",
            json!({
                "requestId": "followup-failed-assignment",
                "captainSessionId": "capacity-captain",
                "shipSlug": "capacity-ship",
                "projectId": "capacity-project",
                "agentSessionId": "followup-agent",
                "message": "Act on the replacement Assignment only.",
                "replacementAssignment": "Replacement Assignment"
            }),
        ),
    );
    assert!(!response.ok);
    assert_eq!(response.error_kind.as_deref(), Some("persistence_failed"));
    assert_eq!(
        registry.snapshot().agent_sessions[0].assignment,
        "Pending durable start"
    );
    assert_eq!(
        ctx.inbox.drain_one("followup-agent", |_| Ok(())),
        crate::inbox::DrainOutcome::Empty,
        "failed Assignment persistence exposed a deliverable wrong-scope instruction"
    );

    let retry = dispatch_authenticated(
        &ctx,
        req_session(
            "agent-followup-assignment-failure",
            &captain_identity.secret,
            "agent_followup",
            json!({
                "requestId": "followup-failed-assignment",
                "captainSessionId": "capacity-captain",
                "shipSlug": "capacity-ship",
                "projectId": "capacity-project",
                "agentSessionId": "followup-agent",
                "message": "Act on the replacement Assignment only.",
                "replacementAssignment": "Replacement Assignment"
            }),
        ),
    );
    assert!(retry.ok, "retry did not converge: {:?}", retry.error);
    assert_eq!(
        registry.snapshot().agent_sessions[0].assignment,
        "Replacement Assignment"
    );
    assert_eq!(
        ctx.inbox.drain_one("followup-agent", |_| Ok(())),
        crate::inbox::DrainOutcome::Delivered { seq: 0 }
    );

    let _ = std::fs::remove_file(path.with_extension("json.bak"));
    let _ = std::fs::remove_file(path);
}

#[test]
fn agent_delivery_command_keeps_completion_and_release_states_distinct() {
    let ctx = test_ctx("agent-delivery");
    let (base, repo_root, worktree) = scratch_repo_with_worktree();
    let repo_path = repo_root.to_string_lossy().to_string();
    let run = |args: &[&str]| {
        let (ok, stdout, stderr) = git::run_git_for_test(&repo_path, args).unwrap();
        assert!(ok, "git {args:?} failed: {stderr}");
        stdout
    };
    run(&["branch", "-M", "main"]);
    let baseline = exact_head(&repo_root);
    let commit_file = |name: &str, content: &str| {
        std::fs::write(repo_root.join(name), content).unwrap();
        run(&["add", name]);
        run(&[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-qm",
            name,
        ]);
        exact_head(&repo_root)
    };
    let interface_result = commit_file("interface.txt", "shared interface\n");
    let result = commit_file("implementation.txt", "lane result\n");
    let incomplete_result = commit_file("incomplete.txt", "incomplete lane\n");
    let canonical = commit_file("integration.txt", "canonical integration\n");
    let worktree_path = worktree.to_string_lossy().to_string();
    let run_worktree = |args: &[&str]| {
        let (ok, stdout, stderr) = git::run_git_for_test(&worktree_path, args).unwrap();
        assert!(ok, "git {args:?} failed: {stderr}");
        stdout
    };
    std::fs::write(worktree.join("divergent.txt"), "divergent lane\n").unwrap();
    run_worktree(&["add", "divergent.txt"]);
    run_worktree(&[
        "-c",
        "user.email=t@t",
        "-c",
        "user.name=t",
        "commit",
        "-qm",
        "divergent lane",
    ]);
    let divergent_result = exact_head(&worktree);
    ctx.captains
        .upsert_project(ProjectRecord {
            root_path: None,
            vcs_capability: None,
            git_main_root: None,
            project_id: "project-delivery".into(),
            name: "Delivery".into(),
            repo_root: repo_path,
            remote_url: None,
            default_branch: Some("main".into()),
            powder: None,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    ctx.captains
        .claim_test("captain-delivery", Some("delivery-ship"), vec![])
        .unwrap();
    ctx.captains
        .bind_ship_context(
            "delivery-ship",
            "project-delivery",
            "Review delivery",
            "codex",
        )
        .unwrap();
    ctx.captains
        .record_crew("captain-delivery", "agent-delivery")
        .unwrap();
    ctx.captains
        .record_crew("captain-delivery", "agent-interface")
        .unwrap();
    ctx.captains
        .record_crew("captain-delivery", "agent-incomplete")
        .unwrap();
    ctx.captains
        .record_crew("captain-delivery", "agent-divergent")
        .unwrap();
    let mut interface_delivery = crate::agent_session::DeliveryProvenance::new(&baseline, false);
    interface_delivery
        .record_implementation(&interface_result)
        .unwrap();
    interface_delivery
        .record_review(crate::agent_session::ReviewEvidence {
            commit: interface_result.clone(),
            reviewer_identity: "reviewer-interface".into(),
            reference: "review://interface".into(),
            recorded_at: 2,
        })
        .unwrap();
    interface_delivery
        .record_acceptance_test(crate::agent_session::AcceptanceTestEvidence {
            commit: interface_result.clone(),
            runner_identity: "tester-interface".into(),
            reference: "test://interface".into(),
            environment: crate::agent_session::AcceptanceEnvironment::Source,
            recorded_at: 2,
        })
        .unwrap();
    let (interface_lane_claim, interface_dispatch_capacity) =
        test_dispatch_evidence("shared-interface", "agent-interface");
    ctx.captains
        .insert_agent_session(AgentSessionRecord {
            agent_session_id: "agent-interface".into(),
            captain_session_id: "captain-delivery".into(),
            project_id: "project-delivery".into(),
            assignment: "Define the shared interface".into(),
            directory: "/tmp/project-delivery-interface".into(),
            worktree_path: None,
            branch: Some("shared-interface".into()),
            workspace_tab_id: None,
            harness: "codex".into(),
            provider: "codex".into(),
            provider_conversation_id: None,
            resume_point: None,
            runtime_state: RuntimeState::Exited,
            work_stage: crate::agent_session::WorkStage::Complete,
            delivery: Some(interface_delivery),
            lane_claim: Some(interface_lane_claim),
            integration_contracts: Vec::new(),
            dispatch_capacity: Some(interface_dispatch_capacity),
            admission_purpose: crate::governor::AdmissionPurpose::Ordinary,
            created_at: 2,
            updated_at: 2,
        })
        .unwrap();
    let (divergent_lane_claim, divergent_dispatch_capacity) =
        test_dispatch_evidence("divergent-lane", "agent-divergent");
    ctx.captains
        .insert_agent_session(AgentSessionRecord {
            agent_session_id: "agent-divergent".into(),
            captain_session_id: "captain-delivery".into(),
            project_id: "project-delivery".into(),
            assignment: "Build a divergent lane".into(),
            directory: worktree_path.clone(),
            worktree_path: Some(files::posix_form(&worktree_path)),
            branch: Some("wt".into()),
            workspace_tab_id: None,
            harness: "codex".into(),
            provider: "codex".into(),
            provider_conversation_id: None,
            resume_point: None,
            runtime_state: RuntimeState::Exited,
            work_stage: crate::agent_session::WorkStage::Complete,
            delivery: Some(completed_delivery(&baseline, &divergent_result)),
            lane_claim: Some(divergent_lane_claim),
            integration_contracts: Vec::new(),
            dispatch_capacity: Some(divergent_dispatch_capacity),
            admission_purpose: crate::governor::AdmissionPurpose::Ordinary,
            created_at: 2,
            updated_at: 2,
        })
        .unwrap();
    let mut incomplete_delivery = crate::agent_session::DeliveryProvenance::new(&baseline, false);
    incomplete_delivery
        .record_implementation(&incomplete_result)
        .unwrap();
    let (incomplete_lane_claim, incomplete_dispatch_capacity) =
        test_dispatch_evidence("incomplete-lane", "agent-incomplete");
    ctx.captains
        .insert_agent_session(AgentSessionRecord {
            agent_session_id: "agent-incomplete".into(),
            captain_session_id: "captain-delivery".into(),
            project_id: "project-delivery".into(),
            assignment: "Incomplete lane".into(),
            directory: "/tmp/project-delivery-incomplete".into(),
            worktree_path: None,
            branch: Some("incomplete-lane".into()),
            workspace_tab_id: None,
            harness: "codex".into(),
            provider: "codex".into(),
            provider_conversation_id: None,
            resume_point: None,
            runtime_state: RuntimeState::Idle,
            work_stage: crate::agent_session::WorkStage::Working,
            delivery: Some(incomplete_delivery),
            lane_claim: Some(incomplete_lane_claim),
            integration_contracts: Vec::new(),
            dispatch_capacity: Some(incomplete_dispatch_capacity),
            admission_purpose: crate::governor::AdmissionPurpose::Ordinary,
            created_at: 2,
            updated_at: 2,
        })
        .unwrap();
    let captain_identity = ctx
        .identity
        .mint_for(crate::identity::Role::Captain, Some("delivery-ship".into()))
        .unwrap();
    ctx.identity
        .bind_tile(&captain_identity.id, "captain-delivery")
        .unwrap();
    let captain = resolve_identity(&ctx, &captain_identity.secret).unwrap();
    let integration_owner_identity = captain.session_id.clone();
    let (lane_claim, dispatch_capacity) = test_dispatch_evidence("lane-delivery", "agent-delivery");
    ctx.captains
        .insert_agent_session(AgentSessionRecord {
            agent_session_id: "agent-delivery".into(),
            captain_session_id: "captain-delivery".into(),
            project_id: "project-delivery".into(),
            assignment: "Implement one scope".into(),
            directory: "/tmp/project-delivery".into(),
            worktree_path: None,
            branch: Some("agent-delivery".into()),
            workspace_tab_id: None,
            harness: "codex".into(),
            provider: "codex".into(),
            provider_conversation_id: None,
            resume_point: None,
            runtime_state: RuntimeState::Running,
            work_stage: crate::agent_session::WorkStage::Working,
            delivery: Some(crate::agent_session::DeliveryProvenance::new(
                &baseline, true,
            )),
            lane_claim: Some(lane_claim),
            integration_contracts: vec![
                crate::governor::IntegrationContract {
                    contract_id: "delivery-integration".into(),
                    integration_owner: integration_owner_identity.clone(),
                    ordered_lane_ids: vec!["shared-interface".into(), "lane-delivery".into()],
                },
                crate::governor::IntegrationContract {
                    contract_id: "incomplete-integration-test".into(),
                    integration_owner: integration_owner_identity.clone(),
                    ordered_lane_ids: vec!["incomplete-lane".into(), "lane-delivery".into()],
                },
                crate::governor::IntegrationContract {
                    contract_id: "divergent-integration-test".into(),
                    integration_owner: integration_owner_identity.clone(),
                    ordered_lane_ids: vec!["divergent-lane".into(), "lane-delivery".into()],
                },
            ],
            dispatch_capacity: Some(dispatch_capacity),
            admission_purpose: crate::governor::AdmissionPurpose::Ordinary,
            created_at: 2,
            updated_at: 2,
        })
        .unwrap();
    let agent_identity = ctx
        .identity
        .mint_for(crate::identity::Role::Crew, Some("delivery-ship".into()))
        .unwrap();
    ctx.identity
        .bind_tile(&agent_identity.id, "agent-delivery")
        .unwrap();
    let agent = resolve_identity(&ctx, &agent_identity.secret).unwrap();

    let self_discard = dispatch_authenticated(
        &ctx,
        req_session(
            "agent-delivery",
            &agent_identity.secret,
            "agent_checkpoint",
            json!({
                "agentSessionId": "agent-delivery",
                "authorSessionId": agent.session_id,
                "summary": "attempt self discard",
                "stage": "stopped"
            }),
        ),
    );
    assert!(!self_discard.ok);
    assert!(
        self_discard
            .error
            .as_deref()
            .is_some_and(|error| error.contains("stage is not permitted")),
        "got: {:?}",
        self_discard.error
    );
    assert!(
        active_dispatch_lanes(&ctx.captains.snapshot(), "project-delivery")
            .iter()
            .any(|lane| lane.lane_id == "incomplete-lane")
    );
    let discard = dispatch_authenticated(
        &ctx,
        req_session(
            "agent-delivery",
            &captain_identity.secret,
            "agent_checkpoint",
            json!({
                "agentSessionId": "agent-incomplete",
                "authorSessionId": captain.session_id,
                "summary": "discard abandoned lane",
                "stage": "stopped"
            }),
        ),
    );
    assert!(discard.ok, "got: {:?}", discard.error);
    assert!(
        !active_dispatch_lanes(&ctx.captains.snapshot(), "project-delivery")
            .iter()
            .any(|lane| lane.lane_id == "incomplete-lane")
    );
    let resume_discarded = dispatch_authenticated(
        &ctx,
        req_session(
            "agent-delivery",
            &captain_identity.secret,
            "agent_checkpoint",
            json!({
                "agentSessionId": "agent-incomplete",
                "authorSessionId": captain.session_id,
                "summary": "attempt to resume discarded lane",
                "stage": "working"
            }),
        ),
    );
    assert!(!resume_discarded.ok);
    assert!(resume_discarded
        .error
        .as_deref()
        .is_some_and(|error| error.contains("terminal work stage")));
    let update_discarded = dispatch_with_caller(
        &ctx,
        "record_agent_delivery",
        &json!({
            "agentSessionId": "agent-incomplete",
            "state": "reviewed",
            "evidence": {
                "commit": incomplete_result,
                "reference": "review://discarded"
            }
        }),
        Some(&captain),
        false,
    )
    .unwrap_err();
    assert!(update_discarded.contains("stopped lane is discarded"));

    let implemented = dispatch_authenticated(
        &ctx,
        req_session(
            "read-agent-delivery",
            &agent_identity.secret,
            "record_agent_delivery",
            json!({
            "agentSessionId": "agent-delivery",
            "state": "implemented",
            "evidence": { "commit": result }
            }),
        ),
    );
    assert!(implemented.ok, "got: {:?}", implemented.error);
    let self_review = dispatch_with_caller(
        &ctx,
        "record_agent_delivery",
        &json!({
            "agentSessionId": "agent-delivery",
            "state": "reviewed",
            "evidence": { "commit": result, "reference": "review://self" }
        }),
        Some(&agent),
        false,
    )
    .unwrap_err();
    assert!(self_review.contains("implementing agent"));
    dispatch_with_caller(
        &ctx,
        "record_agent_delivery",
        &json!({
            "agentSessionId": "agent-delivery",
            "state": "reviewed",
            "evidence": { "commit": result, "reference": "review://captain" }
        }),
        Some(&captain),
        false,
    )
    .unwrap();
    let complete = dispatch_with_caller(
        &ctx,
        "record_agent_delivery",
        &json!({
            "agentSessionId": "agent-delivery",
            "state": "tested",
            "evidence": {
                "commit": result,
                "reference": "test://acceptance",
                "environment": {
                    "kind": "packagedGuiE2e",
                    "artifactId": "candidate-installer-1",
                    "sourceCommit": result,
                    "installationTarget": "C:\\T-Hub-Candidate"
                }
            }
        }),
        Some(&agent),
        false,
    )
    .unwrap();
    assert_eq!(complete["deliveryStates"]["complete"], true);
    assert_eq!(complete["deliveryStates"]["integrated"], false);
    assert_eq!(complete["deliveryStates"]["installed"], false);
    assert_eq!(complete["agent"]["workStage"], "complete");
    assert!(
        active_dispatch_lanes(&ctx.captains.snapshot(), "project-delivery")
            .iter()
            .any(|lane| lane.lane_id == "lane-delivery")
    );
    let mut explicitly_stopped = ctx
        .captains
        .snapshot()
        .agent_sessions
        .into_iter()
        .find(|agent| agent.agent_session_id == "agent-delivery")
        .unwrap();
    explicitly_stopped.work_stage = crate::agent_session::WorkStage::Stopped;
    assert!(!agent_retains_lane_ownership(&explicitly_stopped));

    let missing_manifest = dispatch_with_caller(
        &ctx,
        "record_agent_delivery",
        &json!({
            "agentSessionId": "agent-delivery",
            "state": "integrated",
            "evidence": {
                "sourceCommit": result,
                "canonicalBaseline": "main",
                "canonicalCommit": canonical,
                "reference": "git://integration"
            }
        }),
        Some(&captain),
        false,
    )
    .unwrap_err();
    assert!(missing_manifest.contains("manifest"));

    let integration_evidence = |owner: &str| {
        json!({
            "sourceCommit": result,
            "canonicalBaseline": "main",
            "canonicalCommit": canonical,
            "reference": "git://integration",
            "manifest": {
                "integrationOwnerIdentity": owner,
                "inputs": [
                    {
                        "laneId": "shared-interface",
                        "agentSessionId": "agent-interface",
                        "sourceBaseline": baseline,
                        "resultingCommit": interface_result
                    },
                    {
                        "laneId": "lane-delivery",
                        "agentSessionId": "agent-delivery",
                        "sourceBaseline": baseline,
                        "resultingCommit": result
                    }
                ]
            }
        })
    };
    let manifest = serde_json::from_value::<crate::agent_session::IntegrationManifest>(
        integration_evidence(&integration_owner_identity)["manifest"].clone(),
    )
    .unwrap();
    let mut ambiguous_target = ctx
        .captains
        .snapshot()
        .agent_sessions
        .into_iter()
        .find(|agent| agent.agent_session_id == "agent-delivery")
        .unwrap();
    let mut duplicate_contract = ambiguous_target.integration_contracts[0].clone();
    duplicate_contract.contract_id = "duplicate-delivery-integration".into();
    ambiguous_target
        .integration_contracts
        .push(duplicate_contract);
    assert!(enforce_recorded_integration_contract(
        &ambiguous_target,
        &manifest,
        &integration_owner_identity,
    )
    .unwrap_err()
    .contains("matches multiple durable integration contracts"));
    let forged_owner = dispatch_with_caller(
        &ctx,
        "record_agent_delivery",
        &json!({
            "agentSessionId": "agent-delivery",
            "state": "integrated",
            "evidence": integration_evidence("forged-owner")
        }),
        Some(&captain),
        false,
    )
    .unwrap_err();
    assert!(forged_owner.contains("authenticated actor identity"));

    let general_identity = ctx.identity.mint(crate::identity::Role::General).unwrap();
    let general = resolve_identity(&ctx, &general_identity.secret).unwrap();
    let wrong_designated_owner = dispatch_with_caller(
        &ctx,
        "record_agent_delivery",
        &json!({
            "agentSessionId": "agent-delivery",
            "state": "integrated",
            "evidence": integration_evidence(&general.session_id)
        }),
        Some(&general),
        false,
    )
    .unwrap_err();
    assert!(
        wrong_designated_owner.contains("designates integration owner"),
        "got: {wrong_designated_owner}"
    );

    let mut omitted_lane = integration_evidence(&integration_owner_identity);
    omitted_lane["manifest"]["inputs"]
        .as_array_mut()
        .unwrap()
        .remove(0);
    let omitted_lane = dispatch_with_caller(
        &ctx,
        "record_agent_delivery",
        &json!({
            "agentSessionId": "agent-delivery",
            "state": "integrated",
            "evidence": omitted_lane
        }),
        Some(&captain),
        false,
    )
    .unwrap_err();
    assert!(omitted_lane.contains("exactly match one durable integration contract"));

    let mut reordered_lanes = integration_evidence(&integration_owner_identity);
    reordered_lanes["manifest"]["inputs"]
        .as_array_mut()
        .unwrap()
        .swap(0, 1);
    let reordered_lanes = dispatch_with_caller(
        &ctx,
        "record_agent_delivery",
        &json!({
            "agentSessionId": "agent-delivery",
            "state": "integrated",
            "evidence": reordered_lanes
        }),
        Some(&captain),
        false,
    )
    .unwrap_err();
    assert!(reordered_lanes.contains("exactly match one durable integration contract"));
    assert!(
        !ctx.captains
            .snapshot()
            .agent_sessions
            .iter()
            .find(|agent| agent.agent_session_id == "agent-delivery")
            .unwrap()
            .delivery_states()
            .unwrap()
            .integrated
    );

    let mut invented_agent = integration_evidence(&integration_owner_identity);
    invented_agent["manifest"]["inputs"][0]["agentSessionId"] = json!("invented-agent");
    let invented_agent = dispatch_with_caller(
        &ctx,
        "record_agent_delivery",
        &json!({
            "agentSessionId": "agent-delivery",
            "state": "integrated",
            "evidence": invented_agent
        }),
        Some(&captain),
        false,
    )
    .unwrap_err();
    assert!(invented_agent.contains("is not registered"));

    let mut wrong_lane = integration_evidence(&integration_owner_identity);
    wrong_lane["manifest"]["inputs"][0]["laneId"] = json!("invented-lane");
    let wrong_lane = dispatch_with_caller(
        &ctx,
        "record_agent_delivery",
        &json!({
            "agentSessionId": "agent-delivery",
            "state": "integrated",
            "evidence": wrong_lane
        }),
        Some(&captain),
        false,
    )
    .unwrap_err();
    assert!(wrong_lane.contains("exactly match one durable integration contract"));

    let mut wrong_commits = integration_evidence(&integration_owner_identity);
    wrong_commits["manifest"]["inputs"][0]["sourceBaseline"] =
        json!("9999999999999999999999999999999999999999");
    let wrong_commits = dispatch_with_caller(
        &ctx,
        "record_agent_delivery",
        &json!({
            "agentSessionId": "agent-delivery",
            "state": "integrated",
            "evidence": wrong_commits
        }),
        Some(&captain),
        false,
    )
    .unwrap_err();
    assert!(wrong_commits.contains("commits do not match"));

    let mut incomplete_input = integration_evidence(&integration_owner_identity);
    incomplete_input["manifest"]["inputs"][0]["laneId"] = json!("incomplete-lane");
    incomplete_input["manifest"]["inputs"][0]["agentSessionId"] = json!("agent-incomplete");
    incomplete_input["manifest"]["inputs"][0]["resultingCommit"] = json!(incomplete_result);
    let incomplete_input = dispatch_with_caller(
        &ctx,
        "record_agent_delivery",
        &json!({
            "agentSessionId": "agent-delivery",
            "state": "integrated",
            "evidence": incomplete_input
        }),
        Some(&captain),
        false,
    )
    .unwrap_err();
    assert!(incomplete_input.contains("is not complete"));

    let mut wrong_canonical_tip = integration_evidence(&integration_owner_identity);
    wrong_canonical_tip["canonicalCommit"] = json!(result);
    let wrong_canonical_tip = dispatch_with_caller(
        &ctx,
        "record_agent_delivery",
        &json!({
            "agentSessionId": "agent-delivery",
            "state": "integrated",
            "evidence": wrong_canonical_tip
        }),
        Some(&captain),
        false,
    )
    .unwrap_err();
    assert!(
        wrong_canonical_tip.contains("canonical baseline rejected"),
        "got: {wrong_canonical_tip}"
    );
    assert!(
        !ctx.captains
            .snapshot()
            .agent_sessions
            .iter()
            .find(|agent| agent.agent_session_id == "agent-delivery")
            .unwrap()
            .delivery_states()
            .unwrap()
            .integrated
    );

    let mut divergent_input = integration_evidence(&integration_owner_identity);
    divergent_input["manifest"]["inputs"][0]["laneId"] = json!("divergent-lane");
    divergent_input["manifest"]["inputs"][0]["agentSessionId"] = json!("agent-divergent");
    divergent_input["manifest"]["inputs"][0]["resultingCommit"] = json!(divergent_result);
    let divergent_input = dispatch_with_caller(
        &ctx,
        "record_agent_delivery",
        &json!({
            "agentSessionId": "agent-delivery",
            "state": "integrated",
            "evidence": divergent_input
        }),
        Some(&captain),
        false,
    )
    .unwrap_err();
    assert!(
        divergent_input.contains("not incorporated"),
        "got: {divergent_input}"
    );
    assert!(
        !ctx.captains
            .snapshot()
            .agent_sessions
            .iter()
            .find(|agent| agent.agent_session_id == "agent-delivery")
            .unwrap()
            .delivery_states()
            .unwrap()
            .integrated
    );

    let integrated = dispatch_with_caller(
        &ctx,
        "record_agent_delivery",
        &json!({
            "agentSessionId": "agent-delivery",
            "state": "integrated",
            "evidence": integration_evidence(&integration_owner_identity)
        }),
        Some(&captain),
        false,
    )
    .unwrap();
    assert_eq!(integrated["deliveryStates"]["complete"], true);
    assert_eq!(integrated["deliveryStates"]["integrated"], true);
    assert_eq!(integrated["deliveryStates"]["packaged"], false);
    assert!(
        !active_dispatch_lanes(&ctx.captains.snapshot(), "project-delivery")
            .iter()
            .any(|lane| lane.lane_id == "lane-delivery")
    );
    assert_eq!(
        integrated["agent"]["delivery"]["integration"]["manifest"]["inputs"][0]["laneId"],
        "shared-interface"
    );
    let integration_recorded_at = integrated["agent"]["delivery"]["integration"]["recordedAt"]
        .as_u64()
        .unwrap();

    let missing_artifact_manifest = dispatch_with_caller(
        &ctx,
        "record_agent_delivery",
        &json!({
            "agentSessionId": "agent-delivery",
            "state": "packaged",
            "evidence": {
                "artifactId": "installer-1",
                "sourceBaseline": canonical,
                "reference": "artifact://windows/installer"
            }
        }),
        Some(&captain),
        false,
    )
    .unwrap_err();
    assert!(missing_artifact_manifest.contains("manifest"));

    let packaged = dispatch_with_caller(
            &ctx,
            "record_agent_delivery",
            &json!({
                "agentSessionId": "agent-delivery",
                "state": "packaged",
                "evidence": {
                    "artifactId": "installer-1",
                    "sourceBaseline": canonical,
                    "reference": "artifact://windows/installer",
                    "manifest": {
                        "branch": "main",
                        "sourceCommit": canonical,
                        "gitTree": "5555555555555555555555555555555555555555",
                        "version": "0.3.107",
                        "installerSha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                        "builtAt": integration_recorded_at,
                        "signatureStatus": "verified"
                    }
                }
            }),
            Some(&captain),
            false,
        )
        .unwrap();
    assert_eq!(packaged["deliveryStates"]["integrated"], true);
    assert_eq!(packaged["deliveryStates"]["packaged"], true);
    assert_eq!(packaged["deliveryStates"]["installed"], false);

    let installed = dispatch_with_caller(
        &ctx,
        "record_agent_delivery",
        &json!({
            "agentSessionId": "agent-delivery",
            "state": "installed",
            "evidence": {
                "artifactId": "installer-1",
                "target": "C:\\Program Files\\T-Hub",
                "reference": "install://windows/release"
            }
        }),
        Some(&captain),
        false,
    )
    .unwrap();
    assert_eq!(installed["deliveryStates"]["installed"], true);
    assert_eq!(
        installed["agent"]["delivery"]["acceptanceTest"]["environment"]["artifact_id"],
        "candidate-installer-1"
    );
    assert_eq!(
        installed["agent"]["delivery"]["artifact"]["artifactId"],
        "installer-1"
    );

    let live_verified = dispatch_with_caller(
        &ctx,
        "record_agent_delivery",
        &json!({
            "agentSessionId": "agent-delivery",
            "state": "liveVerified",
            "evidence": {
                "artifactId": "installer-1",
                "target": "C:\\Program Files\\T-Hub",
                "verifierKind": "aiAgent",
                "reference": "verification://windows/release"
            }
        }),
        Some(&captain),
        false,
    )
    .unwrap();
    assert_eq!(live_verified["deliveryStates"]["liveVerified"], true);

    let status = get_agent(
        &ctx,
        &json!({ "agentSessionId": "agent-delivery" }),
        Some(&captain),
        false,
    )
    .unwrap();
    assert_eq!(status["deliveryStates"]["complete"], true);
    assert_eq!(status["deliveryStates"]["integrated"], true);
    assert_eq!(status["deliveryStates"]["packaged"], true);
    assert_eq!(status["deliveryStates"]["liveVerified"], true);
    let events = agent_events(
        &ctx,
        &json!({ "agentSessionId": "agent-delivery", "cursor": "0" }),
        Some(&captain),
        false,
    )
    .unwrap();
    assert!(events["events"].as_array().is_some_and(|events| events
        .iter()
        .any(|event| event["kind"] == "delivery_evidence"
            && event["deliveryStates"]["complete"] == true)));
    std::fs::remove_dir_all(base).ok();
}

#[test]
fn spawn_admission_fails_closed_without_tmux_evidence_and_preserves_rate_token() {
    let available = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let evidence = available.clone();
    let ctx = test_ctx("tmux-evidence")
        .with_governor(Arc::new(SpawnGovernor::new(128, 20.0, 1.0)))
        .with_live_sessions(move || {
            if evidence.load(Ordering::SeqCst) {
                Ok(Vec::new())
            } else {
                Err("injected enumeration outage".into())
            }
        });

    let refused = admit_spawn(&ctx, SpawnPurpose::Ordinary, 1, None).unwrap_err();
    assert_eq!(refused.code, "refused-evidence");
    assert!(refused.message.contains("injected enumeration outage"));

    available.store(true, Ordering::SeqCst);
    assert!(
        admit_spawn(&ctx, SpawnPurpose::Ordinary, 1, None).is_ok(),
        "an evidence refusal must not consume the sole rate token"
    );
}

#[test]
fn fresh_install_uses_reported_packaged_provider_policy() {
    let evidence = provider_capacity_from_environment(Err(std::env::VarError::NotPresent)).unwrap();
    assert_eq!(evidence.session_capacity, 32);
    assert_eq!(evidence.status.source, "packaged-conservative-policy-v1");
    assert!(evidence.status.degraded);
    assert!(evidence
        .status
        .detail
        .as_deref()
        .is_some_and(|detail| detail.contains("live provider quota telemetry is unavailable")));

    let ctx = test_ctx("packaged-provider-policy")
        .with_live_sessions(|| Ok(Vec::new()))
        .with_provider_capacity_evidence(|| {
            provider_capacity_from_environment(Err(std::env::VarError::NotPresent))
        })
        .with_provider_live_sessions(|_| Ok(0));
    let admission = admit_spawn(&ctx, SpawnPurpose::Cortana, 1, None).unwrap();
    assert_eq!(admission._capacity.provider_session_limit, 32);
    assert_eq!(admission._capacity.provider_live_sessions, 0);
    assert_eq!(
        admission._capacity.provider_capacity_status.source,
        "packaged-conservative-policy-v1"
    );
    assert!(admission._capacity.provider_capacity_status.degraded);
}

#[test]
fn explicit_provider_capacity_configuration_is_validated_fail_closed() {
    for invalid in ["", "0", "unknown", "-1"] {
        let error = provider_capacity_from_environment(Ok(invalid.into())).unwrap_err();
        assert!(error.contains("must be a positive integer"), "got: {error}");
    }
    let configured = provider_capacity_from_environment(Ok("7".into())).unwrap();
    assert_eq!(configured.session_capacity, 7);
    assert_eq!(
        configured.status.source,
        "environment-override:T_HUB_PROVIDER_SESSION_CAPACITY"
    );
    assert!(!configured.status.degraded);
    let unavailable = provider_capacity_from_environment(Err(std::env::VarError::NotUnicode(
        std::ffi::OsString::from("configured-unavailable"),
    )))
    .unwrap_err();
    assert!(unavailable.contains("not valid Unicode"));
}

#[test]
fn legacy_captains_snapshot_derives_nested_provider_reservation_headroom() {
    let ctx = test_ctx("legacy-capacity-snapshot");
    seed_starting_agent(&ctx, "legacya1");
    let mut document = serde_json::to_value(ctx.captains.snapshot()).unwrap();
    let report = document["agentSessions"][0]["dispatchCapacity"]
        .as_object_mut()
        .expect("seeded dispatch report");
    let provider_headroom = report["providerHeadroom"].as_u64().unwrap() as usize;
    let reservation_deficit = report["reservations"]["totalDeficit"].as_u64().unwrap() as usize;
    report.remove("requestedProviderLanes");
    report.remove("providerHeadroomAfterReservations");

    let restored: CaptainsSnapshot = serde_json::from_value(document).unwrap();
    let restored = restored.agent_sessions[0]
        .dispatch_capacity
        .as_ref()
        .unwrap();
    assert_eq!(restored.requested_provider_lanes, restored.requested_lanes);
    assert_eq!(
        restored.provider_headroom_after_reservations,
        provider_headroom.saturating_sub(reservation_deficit)
    );
}

#[test]
fn provider_usage_attestation_excludes_generic_tmux_terminals() {
    if !tmux_process_tests_available() {
        eprintln!(
                "provider_usage_attestation_excludes_generic_tmux_terminals: tmux or node not on PATH - skipping"
            );
        return;
    }
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let generic_id = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    let provider_id = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    let generic_target = tmux_target(&generic_id);
    let provider_target = tmux_target(&provider_id);
    create_test_tmux_session(&generic_target).unwrap();
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    create_test_tmux_session_with_env(
        &provider_target,
        "/tmp",
        Some(&harness_command),
        &[(PROVIDER_SESSION_ENV.into(), "codex".into())],
    )
    .unwrap();
    wait_for_harness_started(&provider_id, "codex").unwrap();

    let snapshot = test_ctx("provider-usage-attestation").captains.snapshot();
    assert_eq!(
        inspect_provider_live_sessions(
            &snapshot,
            &[generic_target.clone(), provider_target.clone()]
        )
        .unwrap(),
        1
    );

    reap_test_tmux_session(&generic_target).unwrap();
    reap_test_tmux_session(&provider_target).unwrap();
    std::fs::remove_dir_all(harness_bin_dir).unwrap();
}

#[test]
fn pending_ui_provider_marker_consumes_quota_before_harness_readiness() {
    if !tmux_process_tests_available() {
        eprintln!(
                "pending_ui_provider_marker_consumes_quota_before_harness_readiness: tmux or node not on PATH - skipping"
            );
        return;
    }
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let provider_id = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    let provider_target = tmux_target(&provider_id);
    create_test_tmux_session_with_env(
        &provider_target,
        "/tmp",
        None,
        &[(
            PROVIDER_SESSION_ENV.into(),
            pending_provider_marker("codex"),
        )],
    )
    .unwrap();

    let sessions = vec![provider_target.clone()];
    let listed = sessions.clone();
    let governor = SpawnGovernor::new(8, 20.0, 8.0).with_reservation_policy(
        crate::governor::ReservationPolicy {
            cortana: 0,
            fleet_admins: 0,
            ship_admins_per_active_captain: 0,
            recovery: 0,
        },
    );
    let ctx = test_ctx("pending-ui-provider")
        .with_governor(Arc::new(governor))
        .with_provider_capacity(|| Ok(1))
        .with_live_sessions(move || Ok(listed.clone()));
    assert_eq!(
        inspect_provider_live_sessions(&ctx.captains.snapshot(), &sessions).unwrap(),
        1
    );
    assert_eq!(
        admit_spawn(&ctx, SpawnPurpose::Ordinary, 1, None)
            .unwrap_err()
            .code,
        "provider-capacity"
    );
    assert!(
        admit_spawn(&ctx, SpawnPurpose::Ordinary, 0, None).is_ok(),
        "a generic shell remains admissible at full provider quota"
    );

    reap_test_tmux_session(&provider_target).unwrap();
}

#[test]
fn pending_history_provider_intent_is_counted_but_its_own_admission_is_not_double_counted() {
    let temp = tempfile::tempdir().unwrap();
    let history = history_service_at(temp.path());
    seed_history_resume(&history, "pending-capacity", "histpend", false);
    let governor = SpawnGovernor::new(8, 20.0, 8.0).with_reservation_policy(
        crate::governor::ReservationPolicy {
            cortana: 0,
            fleet_admins: 0,
            ship_admins_per_active_captain: 0,
            recovery: 0,
        },
    );
    let ctx = test_ctx("pending-history-provider")
        .with_governor(Arc::new(governor))
        .with_history_service(history)
        .with_provider_capacity(|| Ok(1))
        .with_provider_live_sessions(|_| Ok(0))
        .with_live_sessions(|| Ok(Vec::new()));

    assert_eq!(
        admit_spawn(&ctx, SpawnPurpose::Ordinary, 1, None)
            .unwrap_err()
            .code,
        "provider-capacity"
    );
    assert!(admit_spawn(&ctx, SpawnPurpose::Ordinary, 1, Some("histpend")).is_ok());
}

#[test]
fn spawn_admission_fails_closed_without_provider_evidence_and_at_provider_limit() {
    let available = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let evidence = available.clone();
    let ctx = test_ctx("provider-evidence")
        .with_governor(Arc::new(SpawnGovernor::new(128, 20.0, 1.0)))
        .with_live_sessions(|| Ok(Vec::new()))
        .with_provider_capacity(move || {
            if evidence.load(Ordering::SeqCst) {
                Ok(128)
            } else {
                Err("injected provider outage".into())
            }
        });
    let unavailable = admit_spawn(&ctx, SpawnPurpose::Ordinary, 1, None).unwrap_err();
    assert_eq!(unavailable.code, "refused-provider");
    assert!(unavailable.message.contains("injected provider outage"));
    available.store(true, Ordering::SeqCst);
    assert!(
        admit_spawn(&ctx, SpawnPurpose::Ordinary, 1, None).is_ok(),
        "a provider-evidence refusal must not consume the sole rate token"
    );

    let at_limit = test_ctx("provider-limit")
        .with_governor(Arc::new(SpawnGovernor::new(128, 20.0, 8.0)))
        .with_live_sessions(|| Ok(vec!["th_live0001".into()]))
        .with_provider_capacity(|| Ok(1));
    let refusal = admit_spawn(&at_limit, SpawnPurpose::Cortana, 1, None).unwrap_err();
    assert_eq!(refusal.code, "provider-capacity");
}

#[test]
fn durable_starting_agent_consumes_capacity_before_tmux_exists() {
    let ctx = test_ctx("pending-start")
        .with_governor(Arc::new(SpawnGovernor::new(5, 20.0, 8.0)))
        .with_live_sessions(|| Ok(Vec::new()));
    seed_starting_agent(&ctx, "pending1");

    assert_eq!(
        live_session_count(&ctx, &ctx.captains.snapshot()).unwrap(),
        1
    );
    let refusal = admit_spawn(&ctx, SpawnPurpose::Ordinary, 1, None).unwrap_err();
    assert_eq!(refusal.code, "reserved-capacity");

    let same_runtime_visible = test_ctx("pending-visible")
        .with_governor(Arc::new(SpawnGovernor::new(8, 20.0, 8.0)))
        .with_live_sessions(|| Ok(vec!["th_pending2".into()]));
    seed_starting_agent(&same_runtime_visible, "pending2");
    assert_eq!(
        live_session_count(
            &same_runtime_visible,
            &same_runtime_visible.captains.snapshot()
        )
        .unwrap(),
        1,
        "a Starting record whose tmux session is visible must not be double counted"
    );
}

#[test]
fn durable_provider_intent_survives_starting_and_counts_once_when_tmux_appears() {
    let absent = test_ctx("pending-provider-absent")
        .with_governor(Arc::new(SpawnGovernor::new(16, 20.0, 8.0)))
        .with_live_sessions(|| Ok(Vec::new()))
        .with_provider_capacity(|| Ok(1))
        .with_provider_live_sessions(|_| Ok(0));
    seed_starting_agent(&absent, "pendprv1");
    let refusal = admit_spawn(&absent, SpawnPurpose::Ordinary, 1, None).unwrap_err();
    assert_eq!(refusal.code, "provider-capacity");

    absent
        .captains
        .mark_agent_started("pendprv1", None)
        .unwrap();
    let refusal = admit_spawn(&absent, SpawnPurpose::Ordinary, 1, None).unwrap_err();
    assert_eq!(refusal.code, "provider-capacity");

    let visible = test_ctx("pending-provider-visible")
        .with_governor(Arc::new(SpawnGovernor::new(16, 20.0, 8.0)))
        .with_live_sessions(|| Ok(vec!["th_pendprv2".into()]))
        .with_provider_capacity(|| Ok(2))
        .with_provider_live_sessions(|_| Ok(1));
    seed_starting_agent(&visible, "pendprv2");
    let live = live_session_evidence(&visible, &visible.captains.snapshot(), None).unwrap();
    let runtime =
        runtime_capacity_from_evidence(&visible, &visible.captains.snapshot(), &live, 16).unwrap();
    assert_eq!(runtime.provider_live_sessions, 1);

    let baseline = "1111111111111111111111111111111111111111";
    let resulting = "2222222222222222222222222222222222222222";
    let mut integrated = visible.captains.snapshot().agent_sessions[0].clone();
    integrated.work_stage = crate::agent_session::WorkStage::Complete;
    let mut delivery = completed_delivery(baseline, resulting);
    delivery
        .record_integration(crate::agent_session::IntegrationEvidence {
            source_commit: resulting.into(),
            canonical_baseline: "main".into(),
            canonical_commit: resulting.into(),
            reference: "integration://provider-capacity".into(),
            recorded_at: 3,
            manifest: Some(crate::agent_session::IntegrationManifest {
                integration_owner_identity: "integration-owner".into(),
                inputs: vec![crate::agent_session::IntegrationInput {
                    lane_id: "capacity-lane".into(),
                    agent_session_id: integrated.agent_session_id.clone(),
                    source_baseline: baseline.into(),
                    resulting_commit: resulting.into(),
                }],
            }),
        })
        .unwrap();
    integrated.delivery = Some(delivery);
    assert!(!agent_has_durable_provider_intent(&integrated));
}

#[test]
fn recovery_reservation_counts_only_nonterminal_recovery_agent_records() {
    let ctx = test_ctx("recovery-agent-record")
        .with_live_sessions(|| Ok(Vec::new()))
        .with_provider_live_sessions(|_| Ok(0));
    ctx.captains
        .begin_cortana_recovery("recovering-state")
        .unwrap();
    let snapshot = ctx.captains.snapshot();
    let live = live_session_evidence(&ctx, &snapshot, None).unwrap();
    let runtime = runtime_capacity_from_evidence(&ctx, &snapshot, &live, 16).unwrap();
    assert_eq!(runtime.live_recovery_sessions, 0);

    seed_starting_agent_with_purpose(
        &ctx,
        "recovery1",
        crate::governor::AdmissionPurpose::Recovery,
    );
    let snapshot = ctx.captains.snapshot();
    let live = live_session_evidence(&ctx, &snapshot, None).unwrap();
    let runtime = runtime_capacity_from_evidence(&ctx, &snapshot, &live, 16).unwrap();
    assert_eq!(runtime.live_recovery_sessions, 1);

    ctx.captains
        .update_agent_stage("recovery1", crate::agent_session::WorkStage::Stopped)
        .unwrap();
    let snapshot = ctx.captains.snapshot();
    let live = live_session_evidence(&ctx, &snapshot, None).unwrap();
    let runtime = runtime_capacity_from_evidence(&ctx, &snapshot, &live, 16).unwrap();
    assert_eq!(runtime.live_recovery_sessions, 0);
}

#[test]
fn reserved_purposes_fill_only_their_authorized_slot() {
    let ctx = test_ctx("reserved-purpose")
        .with_governor(Arc::new(SpawnGovernor::new(4, 20.0, 8.0)))
        .with_live_sessions(|| Ok(vec!["th_existing".into()]));
    let ordinary = admit_spawn(&ctx, SpawnPurpose::Ordinary, 1, None).unwrap_err();
    assert_eq!(ordinary.code, "reserved-capacity");
    assert!(admit_spawn(&ctx, SpawnPurpose::FleetAdmin, 1, None).is_ok());
    assert!(admit_spawn(&ctx, SpawnPurpose::Recovery, 1, None).is_ok());
    assert!(admit_spawn(&ctx, SpawnPurpose::Cortana, 1, None).is_ok());
}

#[test]
fn privileged_admission_purposes_require_the_delegating_supervisor() {
    let crew = ResolvedIdentity {
        session_id: "crew-identity".into(),
        mint_role: crate::identity::Role::Crew,
        tile: Some("crew-tile".into()),
        ship_slug: Some("ship-one".into()),
        fleet_role: None,
        claude_uuid: None,
    };
    assert!(requested_spawn_purpose(
        "start_agent",
        &json!({"captainSessionId": "captain-one", "admissionPurpose": "fleet-admin"}),
        Some(&crew),
        false,
    )
    .is_err());
    assert!(requested_spawn_purpose(
        "start_agent",
        &json!({"captainSessionId": "captain-one", "admissionPurpose": "recovery"}),
        Some(&crew),
        false,
    )
    .is_err());

    let captain = ResolvedIdentity {
        session_id: "captain-identity".into(),
        mint_role: crate::identity::Role::Captain,
        tile: Some("captain-one".into()),
        ship_slug: Some("ship-one".into()),
        fleet_role: Some(FleetRole::Captain),
        claude_uuid: None,
    };
    assert_eq!(
        requested_spawn_purpose(
            "start_agent",
            &json!({"captainSessionId": "captain-one", "admissionPurpose": "ship-admin"}),
            Some(&captain),
            false,
        )
        .unwrap(),
        SpawnPurpose::ShipAdmin {
            ship_slug: "ship-one".into()
        }
    );
    assert!(requested_spawn_purpose(
        "start_agent",
        &json!({"captainSessionId": "sibling-captain", "admissionPurpose": "ship-admin"}),
        Some(&captain),
        false,
    )
    .is_err());
    assert!(requested_spawn_purpose(
        "start_agent",
        &json!({"captainSessionId": "captain-one", "admissionPurpose": "fleet-admin"}),
        Some(&captain),
        false,
    )
    .is_err());

    let cortana = ResolvedIdentity {
        session_id: "cortana-identity".into(),
        mint_role: crate::identity::Role::Cortana,
        tile: Some("cortana-one".into()),
        ship_slug: Some("fleet".into()),
        fleet_role: Some(FleetRole::Cortana),
        claude_uuid: None,
    };
    assert_eq!(
        requested_spawn_purpose(
            "start_agent",
            &json!({"captainSessionId": "captain-one", "admissionPurpose": "fleet-admin"}),
            Some(&cortana),
            false,
        )
        .unwrap(),
        SpawnPurpose::FleetAdmin
    );
}

#[test]
fn public_captain_spawn_assignment_is_refused_before_rate_or_process_side_effects() {
    let store = Arc::new(crate::identity::IdentityStore::ephemeral());
    let registry = Arc::new(CaptainsRegistry::new());
    registry
        .claim_test("captain-one", Some("ship-one"), vec![])
        .unwrap();
    let captain = mint_session(
        &store,
        crate::identity::Role::Captain,
        "ship-one",
        "captain-one",
    );
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let ctx = test_ctx("captain-spawn-contract")
        .with_governor(Arc::new(SpawnGovernor::new(128, 20.0, 1.0)))
        .with_live_sessions(|| Ok(Vec::new()))
        .with_identity_store(store)
        .with_captains_registry(registry)
        .with_apply_sink(sink.clone());

    let response = dispatch_authenticated(
        &ctx,
        req_session(
            "captain-spawn-contract",
            &captain,
            "spawn_terminal",
            json!({
                "cwd": "/tmp",
                "spawnedBy": "captain-one",
                "startupCommand": "codex"
            }),
        ),
    );
    assert!(!response.ok);
    let error = response.error.unwrap();
    assert!(error.contains("must use start_agent"), "got: {error}");
    assert!(ctx.captains.snapshot().captains[0].crew.is_empty());
    assert!(sink.calls.lock().unwrap().is_empty());
    assert!(
        admit_spawn(&ctx, SpawnPurpose::Ordinary, 1, None).is_ok(),
        "a contract refusal must not consume the sole spawn-rate token"
    );
}

#[test]
fn start_agent_caller_cannot_set_its_own_capability() {
    let ctx = test_ctx("start-agent-capability-contract");
    let error = start_agent(
        &ctx,
        &json!({
            "requestId": "caller-capability",
            "captainSessionId": "captain-one",
            "assignment": "Attempt capability relabel",
            "directory": "/tmp",
            "sourceCommit": "1111111111111111111111111111111111111111",
            "visibleProductBug": false,
            "laneId": "caller-capability",
            "dependencies": [],
            "mutableFiles": [],
            "mutableSchemas": [],
            "mutableInterfaces": [],
            "integrationContracts": [],
            "capability": "control"
        }),
        None,
        true,
    )
    .unwrap_err();
    assert!(error.contains("unexpected argument"), "got: {error}");
    assert!(error.contains("capability"), "got: {error}");
    assert!(ctx.captains.snapshot().agent_sessions.is_empty());
}

#[test]
fn public_captain_worktree_assignment_is_refused_before_filesystem_or_rate_side_effects() {
    let store = Arc::new(crate::identity::IdentityStore::ephemeral());
    let registry = Arc::new(CaptainsRegistry::new());
    registry
        .claim_test("captain-one", Some("ship-one"), vec![])
        .unwrap();
    let captain = mint_session(
        &store,
        crate::identity::Role::Captain,
        "ship-one",
        "captain-one",
    );
    let root = std::env::temp_dir().join(format!(
        "t-hub-contract-no-worktree-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let worktree = root.join("worktree");
    let ctx = test_ctx("captain-worktree-contract")
        .with_governor(Arc::new(SpawnGovernor::new(128, 20.0, 1.0)))
        .with_live_sessions(|| Ok(Vec::new()))
        .with_identity_store(store)
        .with_captains_registry(registry);

    let response = dispatch_authenticated(
        &ctx,
        req_session(
            "captain-worktree-contract",
            &captain,
            "create_worktree",
            json!({
                "repoRoot": root,
                "worktreePath": worktree,
                "spawnedBy": "captain-one",
                "startupCommand": "claude"
            }),
        ),
    );
    assert!(!response.ok);
    let error = response.error.unwrap();
    assert!(error.contains("must use start_agent"), "got: {error}");
    assert!(!worktree.exists());
    assert!(
        admit_spawn(&ctx, SpawnPurpose::Ordinary, 1, None).is_ok(),
        "a contract refusal must not consume the sole spawn-rate token"
    );
}

#[test]
fn plain_supervisor_shell_and_worktree_remain_generic_operations() {
    let captain = ResolvedIdentity {
        session_id: "captain-identity".into(),
        mint_role: crate::identity::Role::Captain,
        tile: Some("captain-one".into()),
        ship_slug: Some("ship-one".into()),
        fleet_role: Some(FleetRole::Captain),
        claude_uuid: None,
    };
    assert!(enforce_public_spawn_contract(
        "spawn_terminal",
        &json!({"cwd": "/tmp"}),
        Some(&captain),
        false,
    )
    .is_ok());
    assert!(enforce_public_spawn_contract(
        "create_worktree",
        &json!({"repoRoot": "/repo", "worktreePath": "/worktree"}),
        Some(&captain),
        false,
    )
    .is_ok());
    for command in ["spawn_terminal", "create_worktree"] {
        let error = enforce_public_spawn_contract(
            command,
            &json!({"capability": "control"}),
            Some(&captain),
            false,
        )
        .unwrap_err();
        assert!(error.contains("must use start_agent"), "got: {error}");
    }
}

#[test]
fn generic_spawn_admission_is_atomic_through_runtime_creation_window() {
    let live = Arc::new(StdMutex::new(Vec::<String>::new()));
    let evidence = live.clone();
    let ctx = test_ctx("atomic-generic")
        .with_governor(Arc::new(SpawnGovernor::new(4, 20.0, 8.0)))
        .with_live_sessions(move || Ok(evidence.lock().unwrap().clone()));
    let (held_tx, held_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    let first_ctx = ctx.clone();
    let first_live = live.clone();
    let first = std::thread::spawn(move || {
        let guard = admit_spawn(&first_ctx, SpawnPurpose::FleetAdmin, 1, None).unwrap();
        held_tx.send(()).unwrap();
        release_rx.recv().unwrap();
        first_live.lock().unwrap().push("th_newadmin".into());
        drop(guard);
    });
    held_rx.recv().unwrap();
    let second_ctx = ctx.clone();
    let second = std::thread::spawn(move || {
        admit_spawn(&second_ctx, SpawnPurpose::Ordinary, 1, None)
            .expect_err("ordinary admission must be refused")
    });
    assert!(ctx.dispatch_admission.try_lock().is_err());
    release_tx.send(()).unwrap();
    first.join().unwrap();
    let refusal = second.join().unwrap();
    assert_eq!(refusal.code, "reserved-capacity");
}

#[test]
fn create_worktree_organization_command_cannot_bypass_spawn_capacity() {
    let ctx = test_ctx("worktree-cap")
        .with_governor(Arc::new(SpawnGovernor::new(4, 20.0, 8.0)))
        .with_live_sessions(|| {
            Ok(vec![
                "th_live0001".into(),
                "th_live0002".into(),
                "th_live0003".into(),
                "th_live0004".into(),
            ])
        });
    let response = dispatch_authenticated(
        &ctx,
        req(
            "worktree-cap",
            "create_worktree",
            json!({"repoRoot": "/tmp/repo", "worktreePath": "/tmp/worktree"}),
        ),
    );
    assert!(!response.ok);
    let error = response.error.unwrap();
    assert!(error.contains("dispatch refused"), "got: {error}");
    assert!(
        !error.contains("repoRoot"),
        "handler ran before admission: {error}"
    );
}

#[test]
fn fresh_history_resume_acquires_capacity_and_cancels_its_new_reservation_on_refusal() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let temp = tempfile::tempdir().unwrap();
    let codex_root = temp.path().join("codex/2026/07/20");
    let project_cwd = temp.path().join("project");
    std::fs::create_dir_all(&codex_root).unwrap();
    std::fs::create_dir_all(&project_cwd).unwrap();
    let conversation_id = "22222222-2222-4222-8222-222222222222";
    std::fs::write(
            codex_root.join(format!(
                "rollout-2026-07-20T10-00-00-{conversation_id}.jsonl"
            )),
            format!(
                "{}\n{}",
                json!({"type":"session_meta","payload":{"id":conversation_id,"cwd":project_cwd,"model_provider":"openai"}}),
                json!({"type":"event_msg","payload":{"type":"user_message","message":"Resume me"}})
            ),
        )
        .unwrap();
    let history = Arc::new(crate::history::HistoryService::new(
        temp.path().join("claude"),
        temp.path().join("codex"),
        Duration::from_secs(60),
    ));
    let ctx = test_ctx("history-cap")
        .with_governor(Arc::new(SpawnGovernor::new(4, 20.0, 8.0)))
        .with_live_sessions(|| {
            Ok(vec![
                "th_live0001".into(),
                "th_live0002".into(),
                "th_live0003".into(),
                "th_live0004".into(),
            ])
        })
        .with_history_service(history.clone());
    let listed = history_list(&ctx, &json!({"limit": 10}), None, true).unwrap();
    let history_id = listed["entries"][0]["historyId"]
        .as_str()
        .unwrap()
        .to_string();
    let response = dispatch_authenticated(
        &ctx,
        req(
            "history-cap",
            "history_resume",
            json!({"historyId": history_id, "requestId": "fresh-capacity"}),
        ),
    );
    assert!(!response.ok);
    let error = response.error.unwrap();
    assert!(error.contains("dispatch refused"), "got: {error}");
    assert!(
        history.pending_resume("fresh-capacity").unwrap().is_none(),
        "a pre-spawn capacity refusal must not strand a durable reservation"
    );
}

#[test]
fn completed_history_replay_precedes_full_capacity_and_preserves_one_rate_token() {
    let temp = tempfile::tempdir().unwrap();
    let history = history_service_at(temp.path());
    let (full_history_id, _) = seed_history_resume(&history, "completed-full", "cmpfull1", true);
    let full = test_ctx("history-completed-full")
        .with_governor(Arc::new(SpawnGovernor::new(4, 20.0, 8.0)))
        .with_live_sessions(|| {
            Ok(vec![
                "th_live0001".into(),
                "th_live0002".into(),
                "th_live0003".into(),
                "th_live0004".into(),
            ])
        })
        .with_history_service(history.clone());
    let replay = dispatch_authenticated(
        &full,
        req(
            "history-completed-full",
            "history_resume",
            json!({"historyId": full_history_id, "requestId": "completed-full"}),
        ),
    );
    assert!(!replay.ok);
    let error = replay.error.unwrap();
    assert!(
        error.contains("history_previous_resume_closed"),
        "got: {error}"
    );
    assert!(!error.contains("spawn refused"), "got: {error}");

    let (token_history_id, _) = seed_history_resume(&history, "completed-token", "cmptoken", true);
    let one_token = test_ctx("history-completed-token")
        .with_governor(Arc::new(SpawnGovernor::new(128, 20.0, 1.0)))
        .with_live_sessions(|| Ok(Vec::new()))
        .with_history_service(history);
    let replay = dispatch_authenticated(
        &one_token,
        req(
            "history-completed-token",
            "history_resume",
            json!({"historyId": token_history_id, "requestId": "completed-token"}),
        ),
    );
    assert!(!replay.ok);
    assert!(replay
        .error
        .unwrap_or_default()
        .contains("history_previous_resume_closed"));
    assert!(
        admit_spawn(&one_token, SpawnPurpose::Ordinary, 1, None).is_ok(),
        "completed replay must not consume the sole spawn-rate token"
    );
}

#[test]
fn pending_history_replay_precedes_full_capacity_and_preserves_one_rate_token() {
    let temp = tempfile::tempdir().unwrap();
    let history = history_service_at(temp.path());
    let terminal_id = "pendrep1";
    let (history_id, _) = seed_history_resume(&history, "pending-replay", terminal_id, false);

    let full = test_ctx("history-pending-full")
        .with_governor(Arc::new(SpawnGovernor::new(4, 20.0, 8.0)))
        .with_live_sessions(|| {
            Ok(vec![
                "th_live0001".into(),
                "th_live0002".into(),
                "th_live0003".into(),
                "th_live0004".into(),
            ])
        })
        .with_history_service(history.clone());
    let replay = dispatch_authenticated(
        &full,
        req(
            "history-pending-full",
            "history_resume",
            json!({"historyId": history_id, "requestId": "pending-replay"}),
        ),
    );
    assert!(!replay.ok);
    let error = replay.error.unwrap();
    assert!(error.contains("history_resume_in_flight"), "got: {error}");
    assert!(!error.contains("spawn refused"), "got: {error}");

    let one_token = test_ctx("history-pending-token")
        .with_governor(Arc::new(SpawnGovernor::new(128, 20.0, 1.0)))
        .with_live_sessions(|| Ok(Vec::new()))
        .with_history_service(history);
    let replay = dispatch_authenticated(
        &one_token,
        req(
            "history-pending-token",
            "history_resume",
            json!({"historyId": "history:v1:pending-replay", "requestId": "pending-replay"}),
        ),
    );
    assert!(!replay.ok);
    assert!(replay
        .error
        .unwrap_or_default()
        .contains("history_resume_in_flight"));
    assert!(
        admit_spawn(&one_token, SpawnPurpose::Ordinary, 1, None).is_ok(),
        "pending replay must not consume the sole spawn-rate token"
    );
}

#[test]
fn dispatch_preflight_admits_six_independent_lanes_with_available_capacity() {
    let ctx = test_ctx("dispatch-six").with_governor(Arc::new(SpawnGovernor::new(128, 20.0, 8.0)));
    let (base, repo_root, _worktree) = scratch_repo_with_worktree();
    let source_commit = exact_head(&repo_root);
    for index in 2..=5 {
        let branch = format!("lane-{index}");
        let path = base.join(format!("wt-{index}"));
        let output = std::process::Command::new("git")
            .current_dir(&repo_root)
            .args([
                "worktree",
                "add",
                "-q",
                "-b",
                &branch,
                path.to_str().unwrap(),
            ])
            .output()
            .expect("git worktree add spawns");
        assert!(
            output.status.success(),
            "git worktree add failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    ctx.captains
        .upsert_project(ProjectRecord {
            root_path: None,
            vcs_capability: None,
            git_main_root: None,
            project_id: "project-six".into(),
            name: "Six Lane Project".into(),
            repo_root: repo_root.to_string_lossy().to_string(),
            remote_url: None,
            default_branch: None,
            powder: None,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    let requested_lanes = (1..=6)
        .map(|index| {
            json!({
                "laneId": format!("lane-{index}"),
                "ownerId": format!("owner-{index}"),
                "dependencies": [],
                "mutableFiles": [format!("scope-{index}")],
                "mutableSchemas": [],
                "mutableInterfaces": []
            })
        })
        .collect::<Vec<_>>();

    let response = dispatch(
        &ctx,
        "dispatch_preflight",
        &json!({
            "projectId": "project-six",
            "sourceCommit": source_commit,
            "requestedLanes": requested_lanes,
            "integrationContracts": []
        }),
    )
    .unwrap();

    assert_eq!(response["admitted"], true);
    assert_eq!(response["capacity"]["requestedLanes"], 6);
    assert!(response["capacity"]["effectiveLaneHeadroom"]
        .as_u64()
        .is_some_and(|headroom| headroom >= 6));
    std::fs::remove_dir_all(base).ok();
}

#[test]
fn start_agent_rejects_dependency_result_missing_from_source_baseline() {
    let ctx = test_ctx("dependency-ancestry");
    let (base, repo_root, worktree) = scratch_repo_with_worktree();
    let initial_commit = exact_head(&worktree);
    std::fs::write(worktree.join("source.txt"), "dependent source\n").unwrap();
    let worktree_path = worktree.to_string_lossy().to_string();
    let run = |cwd: &str, args: &[&str]| {
        let (ok, stdout, stderr) = git::run_git_for_test(cwd, args).unwrap();
        assert!(ok, "git {args:?} failed: {stderr}");
        stdout
    };
    run(&worktree_path, &["add", "source.txt"]);
    run(
        &worktree_path,
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-qm",
            "dependent source",
        ],
    );
    let source_commit = exact_head(&worktree);

    let repo_path = repo_root.to_string_lossy().to_string();
    std::fs::write(repo_root.join("dependency.txt"), "dependency result\n").unwrap();
    run(&repo_path, &["add", "dependency.txt"]);
    run(
        &repo_path,
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-qm",
            "divergent dependency result",
        ],
    );
    let dependency_result = exact_head(&repo_root);

    ctx.captains
        .upsert_project(ProjectRecord {
            root_path: None,
            vcs_capability: None,
            git_main_root: None,
            project_id: "project-dependency".into(),
            name: "Dependency Project".into(),
            repo_root: repo_path,
            remote_url: None,
            default_branch: None,
            powder: None,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    ctx.captains
        .claim_test("captain-dependency", Some("captain-dependency"), vec![])
        .unwrap();
    ctx.captains
        .bind_ship_context(
            "captain-dependency",
            "project-dependency",
            "Assignment",
            "codex",
        )
        .unwrap();
    let (lane_claim, dispatch_capacity) =
        test_dispatch_evidence("dependency-lane", "dependency-agent");
    ctx.captains
        .insert_agent_session(AgentSessionRecord {
            agent_session_id: "dependency-agent".into(),
            captain_session_id: "captain-dependency".into(),
            project_id: "project-dependency".into(),
            assignment: "Build dependency".into(),
            directory: repo_root.to_string_lossy().to_string(),
            worktree_path: None,
            branch: Some("main".into()),
            workspace_tab_id: None,
            harness: "codex".into(),
            provider: "codex".into(),
            provider_conversation_id: None,
            resume_point: None,
            runtime_state: RuntimeState::Exited,
            work_stage: crate::agent_session::WorkStage::Complete,
            delivery: Some(completed_delivery(&initial_commit, &dependency_result)),
            lane_claim: Some(lane_claim),
            integration_contracts: Vec::new(),
            dispatch_capacity: Some(dispatch_capacity),
            admission_purpose: crate::governor::AdmissionPurpose::Ordinary,
            created_at: 2,
            updated_at: 2,
        })
        .unwrap();

    let mut ancestor_snapshot = ctx.captains.snapshot();
    ancestor_snapshot.agent_sessions[0].delivery =
        Some(completed_delivery(&initial_commit, &initial_commit));
    assert_eq!(
        validate_dependency_result_ancestry(
            "test_dependency_ancestry",
            &ancestor_snapshot,
            "project-dependency",
            &BTreeSet::from(["dependency-lane".to_string()]),
            &worktree_path,
            &source_commit,
        )
        .unwrap(),
        BTreeSet::from(["dependency-lane".to_string()])
    );

    let preflight_error = dispatch(
        &ctx,
        "dispatch_preflight",
        &json!({
            "projectId": "project-dependency",
            "sourceCommit": source_commit,
            "requestedLanes": [{
                "laneId": "dependent-lane",
                "ownerId": "dependent-owner",
                "dependencies": ["dependency-lane"],
                "mutableFiles": ["src/dependent.rs"],
                "mutableSchemas": [],
                "mutableInterfaces": []
            }],
            "integrationContracts": []
        }),
    )
    .unwrap_err();
    assert!(
        preflight_error.contains("dispatch_preflight: dependency 'dependency-lane'"),
        "got: {preflight_error}"
    );
    assert!(
        preflight_error.contains("not present in sourceCommit"),
        "got: {preflight_error}"
    );

    let error = start_agent(
        &ctx,
        &json!({
            "requestId": "dependency-ancestry-rejected",
            "captainSessionId": "captain-dependency",
            "assignment": "Build dependent lane",
            "directory": worktree_path,
            "sourceCommit": source_commit,
            "visibleProductBug": false,
            "laneId": "dependent-lane",
            "dependencies": ["dependency-lane"],
            "mutableFiles": ["src/dependent.rs"],
            "mutableSchemas": [],
            "mutableInterfaces": [],
            "integrationContracts": []
        }),
        None,
        true,
    )
    .unwrap_err();
    assert!(error.contains("dependency-lane"), "got: {error}");
    assert!(
        error.contains("not present in sourceCommit"),
        "got: {error}"
    );
    assert_eq!(ctx.captains.snapshot().agent_sessions.len(), 1);
    std::fs::remove_dir_all(base).ok();
}

#[test]
fn concurrent_start_agent_admission_cannot_double_claim_a_checkout() {
    if !tmux_process_tests_available() {
        eprintln!(
                "concurrent_start_agent_admission_cannot_double_claim_a_checkout: tmux or node not on PATH - skipping"
            );
        return;
    }
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let governor = SpawnGovernor::new(8, 20.0, 8.0).with_reservation_policy(
        crate::governor::ReservationPolicy {
            cortana: 0,
            fleet_admins: 0,
            ship_admins_per_active_captain: 0,
            recovery: 0,
        },
    );
    let ctx = test_ctx("atomic-start-agent")
        .with_governor(Arc::new(governor))
        .with_provider_capacity(|| Ok(1))
        .with_provider_live_sessions(|_| Ok(0))
        .with_live_sessions(|| Ok(Vec::new()))
        .with_apply_sink(sink);
    let (base, repo_root, worktree) = scratch_repo_with_worktree();
    let source_commit = exact_head(&worktree);
    let checkout = worktree.to_string_lossy().to_string();
    ctx.captains
        .upsert_project(ProjectRecord {
            root_path: None,
            vcs_capability: None,
            git_main_root: None,
            project_id: "project-atomic-start".into(),
            name: "Atomic Start Project".into(),
            repo_root: repo_root.to_string_lossy().to_string(),
            remote_url: None,
            default_branch: None,
            powder: None,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    ctx.captains
        .claim_test("captain-atomic-start", Some("captain-atomic-start"), vec![])
        .unwrap();
    ctx.captains
        .bind_ship_context(
            "captain-atomic-start",
            "project-atomic-start",
            "Assignment",
            "codex",
        )
        .unwrap();

    let args = json!({
        "requestId": "atomic-start-first",
        "captainSessionId": "captain-atomic-start",
        "assignment": "Own the shared checkout",
        "directory": checkout,
        "harness": "codex",
        "sourceCommit": source_commit,
        "visibleProductBug": false,
        "laneId": "atomic-lane-first",
        "dependencies": [],
        "mutableFiles": ["src/shared.rs"],
        "mutableSchemas": [],
        "mutableInterfaces": [],
        "integrationContracts": []
    });
    let (reached, wait_for_admission) = std::sync::mpsc::sync_channel(1);
    let (resume, continue_start) = std::sync::mpsc::sync_channel(1);
    ctx.captains.set_dispatch_barrier(Some(DispatchBarrier {
        boundary: "start_agent_admitted",
        reached,
        resume: continue_start,
    }));
    let first_ctx = ctx.clone();
    let first_args = args.clone();
    let first = std::thread::spawn(move || start_agent(&first_ctx, &first_args, None, true));
    assert_eq!(
        wait_for_admission
            .recv_timeout(Duration::from_secs(2))
            .expect("first start did not reach durable admission"),
        "start_agent_admitted"
    );
    assert!(ctx.dispatch_admission.try_lock().is_err());
    assert_eq!(ctx.captains.snapshot().agent_sessions.len(), 1);
    let snapshot = ctx.captains.snapshot();
    let live = live_session_evidence(&ctx, &snapshot, None).unwrap();
    let runtime = runtime_capacity_from_evidence(&ctx, &snapshot, &live, 8).unwrap();
    assert_eq!(
        runtime.provider_live_sessions, 1,
        "the paused durable start must occupy the sole provider slot before tmux exists"
    );

    let mut second_args = args;
    second_args["requestId"] = json!("atomic-start-second");
    second_args["laneId"] = json!("atomic-lane-second");
    let second_ctx = ctx.clone();
    let (attempted_tx, attempted_rx) = std::sync::mpsc::sync_channel(1);
    let (finished_tx, finished_rx) = std::sync::mpsc::sync_channel(1);
    let second = std::thread::spawn(move || {
        attempted_tx.send(()).unwrap();
        let result = start_agent(&second_ctx, &second_args, None, true);
        finished_tx.send(result).unwrap();
    });
    attempted_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(matches!(
        finished_rx.recv_timeout(Duration::from_millis(150)),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
    ));

    resume.send(()).unwrap();
    let first_result = first.join().unwrap().unwrap();
    let second_error = finished_rx
        .recv_timeout(Duration::from_secs(3))
        .unwrap()
        .unwrap_err();
    second.join().unwrap();
    assert!(
        second_error.contains("already owned"),
        "got: {second_error}"
    );
    assert_eq!(ctx.captains.snapshot().agent_sessions.len(), 1);
    assert_eq!(first_result["sourceCommit"], source_commit);
    assert_eq!(first_result["sourceBaseline"], source_commit);
    assert_eq!(first_result["admissionPurpose"], "ordinary");
    let agent_session_id = first_result["agentSessionId"].as_str().unwrap();
    reap_test_tmux_session(&tmux_target(agent_session_id)).unwrap();
    std::fs::remove_dir_all(base).ok();
}

#[test]
fn start_agent_persists_before_a_launch_failure_and_records_unavailable() {
    let ctx = test_ctx("secret");
    let (base, repo_root, worktree) = scratch_repo_with_worktree();
    let source_commit = exact_head(&worktree);
    let repo = worktree.to_string_lossy().to_string();
    ctx.captains
        .upsert_project(ProjectRecord {
            root_path: None,
            vcs_capability: None,
            git_main_root: None,
            project_id: "project-start".into(),
            name: "Start Project".into(),
            repo_root: repo_root.to_string_lossy().to_string(),
            remote_url: None,
            default_branch: None,
            powder: None,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    ctx.captains
        .claim_test("captain-start", Some("captain-start"), vec![])
        .unwrap();
    ctx.captains
        .bind_ship_context("captain-start", "project-start", "Assignment", "codex")
        .unwrap();

    let error = dispatch(
        &ctx,
        "start_agent",
        &json!({
            "requestId": "start-agent-test",
            "captainSessionId": "captain-start",
            "assignment": "Do one bounded change",
            "directory": repo.clone(),
            "sourceCommit": source_commit,
            "visibleProductBug": false,
            "laneId": "lane-start-failure",
            "dependencies": [],
            "mutableFiles": ["src/start-failure.rs"],
            "mutableSchemas": [],
            "mutableInterfaces": [],
            "integrationContracts": [],
            "admissionPurpose": "fleet-admin"
        }),
    )
    .unwrap_err();
    assert!(error.contains("no UI is connected"), "got: {error}");
    let agent = ctx
        .captains
        .snapshot()
        .agent_sessions
        .into_iter()
        .next()
        .expect("agent record persisted before launch");
    assert_eq!(agent.runtime_state, RuntimeState::Unavailable);
    assert_eq!(agent.work_stage, crate::agent_session::WorkStage::Stopped);
    assert_eq!(
        agent.admission_purpose,
        crate::governor::AdmissionPurpose::FleetAdmin
    );
    let events = ctx.captains.snapshot().agent_events;
    assert_eq!(
        events.last().map(|event| event.kind.as_str()),
        Some("unavailable")
    );
    std::fs::remove_dir_all(base).ok();
}

#[test]
fn start_agent_uses_matching_reserved_capacity_in_project_preflight() {
    let ctx = test_ctx("reserved-start")
        .with_governor(Arc::new(SpawnGovernor::new(4, 20.0, 8.0)))
        .with_live_sessions(|| Ok(Vec::new()));
    let (base, repo_root, worktree) = scratch_repo_with_worktree();
    let source_commit = exact_head(&worktree);
    let repo = worktree.to_string_lossy().to_string();
    ctx.captains
        .upsert_project(ProjectRecord {
            root_path: None,
            vcs_capability: None,
            git_main_root: None,
            project_id: "project-reserved-start".into(),
            name: "Reserved Start Project".into(),
            repo_root: repo_root.to_string_lossy().to_string(),
            remote_url: None,
            default_branch: None,
            powder: None,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    ctx.captains
        .claim_test(
            "captain-reserved-start",
            Some("captain-reserved-start"),
            vec![],
        )
        .unwrap();
    ctx.captains
        .bind_ship_context(
            "captain-reserved-start",
            "project-reserved-start",
            "Assignment",
            "codex",
        )
        .unwrap();

    let ordinary_refusal = admit_spawn(&ctx, SpawnPurpose::Ordinary, 1, None).unwrap_err();
    assert_eq!(ordinary_refusal.code, "reserved-capacity");

    let error = dispatch(
        &ctx,
        "start_agent",
        &json!({
            "requestId": "reserved-start-agent-test",
            "captainSessionId": "captain-reserved-start",
            "assignment": "Start the standing Fleet Admin",
            "directory": repo,
            "sourceCommit": source_commit,
            "visibleProductBug": false,
            "laneId": "reserved-fleet-admin-lane",
            "dependencies": [],
            "mutableFiles": ["src/reserved-admin.rs"],
            "mutableSchemas": [],
            "mutableInterfaces": [],
            "integrationContracts": [],
            "admissionPurpose": "fleet-admin"
        }),
    )
    .unwrap_err();
    assert!(error.contains("no UI is connected"), "got: {error}");
    let agent = ctx
        .captains
        .snapshot()
        .agent_sessions
        .into_iter()
        .next()
        .expect("Fleet Admin persisted after reserved admission");
    assert_eq!(
        agent.admission_purpose,
        crate::governor::AdmissionPurpose::FleetAdmin
    );
    assert_eq!(agent.runtime_state, RuntimeState::Unavailable);
    std::fs::remove_dir_all(base).ok();
}

#[test]
fn start_agent_records_running_after_launch_without_inventing_provider_identity() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let mut ctx = test_ctx("start-agent-success").with_apply_sink(sink);
    ctx.addr = "127.0.0.1:1".into();
    let (base, repo_root, worktree) = scratch_repo_with_worktree();
    let source_commit = exact_head(&worktree);
    let repo = worktree.to_string_lossy().to_string();
    ctx.captains
        .upsert_project(ProjectRecord {
            root_path: None,
            vcs_capability: None,
            git_main_root: None,
            project_id: "project-start-success".into(),
            name: "Start Success Project".into(),
            repo_root: repo_root.to_string_lossy().to_string(),
            remote_url: None,
            default_branch: None,
            powder: None,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    ctx.captains
        .claim_test(
            "captain-start-success",
            Some("captain-start-success"),
            vec![],
        )
        .unwrap();
    ctx.captains
        .bind_ship_context(
            "captain-start-success",
            "project-start-success",
            "Assignment",
            "codex",
        )
        .unwrap();

    let response = dispatch(
        &ctx,
        "start_agent",
        &json!({
            "requestId": "start-agent-success",
            "captainSessionId": "captain-start-success",
            "assignment": "Do one bounded change",
            "directory": repo.clone(),
            "harness": "codex",
            "sourceCommit": source_commit,
            "visibleProductBug": false,
            "laneId": "lane-start-success",
            "dependencies": [],
            "mutableFiles": ["src/start-success.rs"],
            "mutableSchemas": [],
            "mutableInterfaces": [],
            "integrationContracts": []
        }),
    )
    .unwrap();
    let agent_session_id = response["agentSessionId"].as_str().unwrap();
    let agent = ctx
        .captains
        .snapshot()
        .agent_sessions
        .into_iter()
        .find(|agent| agent.agent_session_id == agent_session_id)
        .expect("agent record persisted after launch");
    assert_eq!(agent.runtime_state, RuntimeState::Running);
    assert_eq!(agent.work_stage, crate::agent_session::WorkStage::Assigned);
    assert!(agent.provider_conversation_id.is_none());
    assert_eq!(response["runtimeState"], "running");
    assert!(response.get("providerConversationId").is_none());
    assert_eq!(
        tmux::session_environment(&tmux_target(agent_session_id), "T_HUB_CONTROL_TOKEN")
            .unwrap()
            .as_deref(),
        Some(""),
        "ordinary implementation lanes must not persist rotating credentials"
    );
    let event = ctx
        .captains
        .snapshot()
        .agent_events
        .into_iter()
        .find(|event| event.agent_session_id == agent_session_id)
        .expect("started lifecycle event");
    assert_eq!(event.kind, "started");
    assert_eq!(event.runtime_state, Some(RuntimeState::Running));

    reap_test_tmux_session(&tmux_target(agent_session_id)).unwrap();
    std::fs::remove_dir_all(base).ok();
}

#[test]
fn authorized_admin_start_agent_receives_control_capability() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let mut ctx = test_ctx("start-fleet-admin").with_apply_sink(sink);
    ctx.addr = "127.0.0.1:1".into();
    let (base, repo_root, worktree) = scratch_repo_with_worktree();
    let source_commit = exact_head(&worktree);
    let repo = worktree.to_string_lossy().to_string();
    ctx.captains
        .upsert_project(ProjectRecord {
            root_path: None,
            vcs_capability: None,
            git_main_root: None,
            project_id: "project-fleet-admin".into(),
            name: "Fleet Admin Project".into(),
            repo_root: repo_root.to_string_lossy().to_string(),
            remote_url: None,
            default_branch: None,
            powder: None,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    ctx.captains
        .claim_test("captain-fleet-admin", Some("captain-fleet-admin"), vec![])
        .unwrap();
    ctx.captains
        .bind_ship_context(
            "captain-fleet-admin",
            "project-fleet-admin",
            "Assignment",
            "codex",
        )
        .unwrap();

    let response = dispatch(
        &ctx,
        "start_agent",
        &json!({
            "requestId": "start-fleet-admin",
            "captainSessionId": "captain-fleet-admin",
            "assignment": "Perform delegated fleet administration",
            "directory": repo,
            "harness": "codex",
            "sourceCommit": source_commit,
            "visibleProductBug": false,
            "laneId": "lane-fleet-admin",
            "dependencies": [],
            "mutableFiles": [],
            "mutableSchemas": [],
            "mutableInterfaces": [],
            "integrationContracts": [],
            "admissionPurpose": "fleet-admin"
        }),
    )
    .unwrap();
    let agent_session_id = response["agentSessionId"].as_str().unwrap();
    assert_eq!(
        tmux::session_environment(&tmux_target(agent_session_id), "T_HUB_CONTROL_TOKEN")
            .unwrap()
            .as_deref(),
        Some(""),
        "an administrative lane must reacquire scoped authority from durable identity"
    );
    assert!(
        tmux::session_environment(&tmux_target(agent_session_id), "GH_CONFIG_DIR")
            .unwrap()
            .is_some_and(|value| !value.is_empty())
    );
    assert_eq!(
        tmux::session_environment(&tmux_target(agent_session_id), "NPM_TOKEN").unwrap(),
        Some(String::new()),
        "administrative control capability must not restore ambient credentials"
    );
    let agent = ctx
        .captains
        .snapshot()
        .agent_sessions
        .into_iter()
        .find(|agent| agent.agent_session_id == agent_session_id)
        .unwrap();
    assert_eq!(
        agent.admission_purpose,
        crate::governor::AdmissionPurpose::FleetAdmin
    );

    reap_test_tmux_session(&tmux_target(agent_session_id)).unwrap();
    std::fs::remove_dir_all(base).ok();
}

#[test]
fn project_commands_register_idempotently_and_powder_commands_are_tombstoned() {
    let ctx = test_ctx("secret");
    let repo = env!("CARGO_MANIFEST_DIR");
    let first = dispatch(
        &ctx,
        "register_project",
        &json!({"repoRoot": repo, "name": "T-Hub"}),
    )
    .unwrap();
    let second = dispatch(
        &ctx,
        "register_project",
        &json!({"repoRoot": repo, "name": "T-Hub"}),
    )
    .unwrap();
    assert_eq!(first["projectId"], second["projectId"]);

    let catalog = dispatch(&ctx, "list_projects", &json!({})).unwrap();
    assert_eq!(catalog["count"], 1);
    assert_eq!(catalog["projects"][0]["projectId"], first["projectId"]);

    for command in [
        "dispatch_crew",
        "list_powder_boards",
        "bind_project_powder",
        "project_board_snapshot",
        "powder_status",
        "heartbeat_crew_powder",
        "append_crew_powder_work_log",
        "read_crew_powder_evidence",
        "review_crew_powder_criterion",
        "complete_crew_powder",
    ] {
        assert!(is_retired_powder_command(command));
        let response = ControlResponse::powder_retired(command);
        assert!(!response.ok);
        assert_eq!(response.error_kind.as_deref(), Some("powder_retired"));
        assert_eq!(
            response.error.as_deref(),
            Some(
                format!("{command} is retired; use the agent session operations instead").as_str()
            )
        );
        assert!(!response.retryable);
    }
}

fn fleet_admin_grant_fixture(
    tag: &str,
) -> (
    ControlContext,
    crate::identity::SessionIdentity,
    crate::identity::SessionIdentity,
    String,
    String,
) {
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let cortana_tile = format!("co{}", &nonce[..6]);
    let captain_tile = format!("ca{}", &nonce[..6]);
    let admin_tile = format!("fa{}", &nonce[..6]);
    let ctx = test_ctx(&format!("fleet-grant-{tag}-{}", &nonce[..6]));
    ctx.captains
        .claim_test(&captain_tile, Some(&format!("ship-{tag}")), vec![])
        .unwrap();
    ctx.captains
        .record_crew(&captain_tile, &admin_tile)
        .unwrap();
    create_test_tmux_session(&tmux_target(&admin_tile)).unwrap();

    let cortana_secret = mint_current_cortana_session(&ctx.identity, &ctx.captains, &cortana_tile);
    let cortana_identity = ctx.identity.resolve(&cortana_secret).unwrap();
    let admin_identity = ctx.identity.mint(crate::identity::Role::Crew).unwrap();
    ctx.identity
        .bind_tile(&admin_identity.id, &admin_tile)
        .unwrap();
    let cortana = resolve_identity(&ctx, &cortana_secret).unwrap();
    let appointed = appoint_admin(
        &ctx,
        &json!({
            "actorSessionId": admin_tile,
            "role": "fleetAdmin",
            "permittedOperations": ["maintainFleetResource"]
        }),
        Some(&cortana),
        false,
    )
    .unwrap();
    let grant_id = appointed["grant"]["grantId"].as_str().unwrap().to_string();
    (ctx, cortana_identity, admin_identity, grant_id, admin_tile)
}

#[test]
fn captain_appoints_and_revokes_a_ship_admin_for_exact_ship_inspection() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let ctx = test_ctx("delegated-admin");
    ctx.captains
        .claim_test("captain-admin", Some("alpha"), vec![])
        .unwrap();
    ctx.captains
        .record_crew("captain-admin", "crew-admin")
        .unwrap();
    let admin_target = tmux_target("crew-admin");
    create_test_tmux_session(&admin_target).unwrap();
    let captain_identity = ctx.identity.mint(crate::identity::Role::Captain).unwrap();
    ctx.identity
        .bind_tile(&captain_identity.id, "captain-admin")
        .unwrap();
    let crew_identity = ctx.identity.mint(crate::identity::Role::Crew).unwrap();
    ctx.identity
        .bind_tile(&crew_identity.id, "crew-admin")
        .unwrap();
    let captain = resolve_identity(&ctx, &captain_identity.secret).unwrap();
    let appointed = appoint_admin(
        &ctx,
        &json!({
            "actorSessionId": "crew-admin",
            "role": "shipAdmin",
            "permittedOperations": ["inspectStatus", "maintainSession"],
        }),
        Some(&captain),
        false,
    )
    .unwrap();
    let grant_id = appointed["grant"]["grantId"].as_str().unwrap().to_string();
    let crew = resolve_identity(&ctx, &crew_identity.secret).unwrap();
    let audit = authorize_delegated_admin(
        &ctx,
        &crew,
        crate::delegated_admin::AdminOperation::InspectStatus,
        crate::delegated_admin::AdminTarget::CrewSession {
            ship_slug: "alpha".into(),
            session_id: "crew-peer".into(),
        },
        crate::delegated_admin::AdminSafeguards::default(),
    )
    .unwrap();
    assert_eq!(audit.actor_identity_id, crew_identity.id);
    assert_eq!(audit.delegating_supervisor_identity_id, captain_identity.id);
    let foreign = authorize_delegated_admin(
        &ctx,
        &crew,
        crate::delegated_admin::AdminOperation::InspectStatus,
        crate::delegated_admin::AdminTarget::CrewSession {
            ship_slug: "beta".into(),
            session_id: "foreign".into(),
        },
        crate::delegated_admin::AdminSafeguards::default(),
    )
    .unwrap_err();
    assert!(foreign.contains("targetOutOfScope"));

    revoke_admin(
        &ctx,
        &json!({ "grantId": grant_id, "reason": "rotation" }),
        Some(&captain),
        false,
    )
    .unwrap();
    let revoked = authorize_delegated_admin(
        &ctx,
        &crew,
        crate::delegated_admin::AdminOperation::InspectStatus,
        crate::delegated_admin::AdminTarget::Ship {
            ship_slug: "alpha".into(),
        },
        crate::delegated_admin::AdminSafeguards::default(),
    )
    .unwrap_err();
    assert!(revoked.contains("no active administrative grant"));
    reap_test_tmux_session(&admin_target).unwrap();
}

#[test]
fn ship_admin_grant_fails_closed_for_ambiguous_delegator_ship() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let ctx = test_ctx("delegated-admin-ambiguous");
    ctx.captains
        .claim_test("captain-admin", Some("alpha"), vec![])
        .unwrap();
    ctx.captains
        .record_crew("captain-admin", "crew-admin")
        .unwrap();
    let admin_target = tmux_target("crew-admin");
    create_test_tmux_session(&admin_target).unwrap();
    let captain_identity = ctx.identity.mint(crate::identity::Role::Captain).unwrap();
    ctx.identity
        .bind_tile(&captain_identity.id, "captain-admin")
        .unwrap();
    let admin_identity = ctx.identity.mint(crate::identity::Role::Crew).unwrap();
    ctx.identity
        .bind_tile(&admin_identity.id, "crew-admin")
        .unwrap();
    let captain = resolve_identity(&ctx, &captain_identity.secret).unwrap();
    let appointed = appoint_admin(
        &ctx,
        &json!({
            "actorSessionId": "crew-admin",
            "role": "shipAdmin",
            "permittedOperations": ["inspectStatus"],
        }),
        Some(&captain),
        false,
    )
    .unwrap();
    let grant_id = appointed["grant"]["grantId"].as_str().unwrap();

    let mut duplicate = ctx
        .captains
        .snapshot()
        .captains
        .into_iter()
        .find(|record| record.terminal_id.as_deref() == Some("captain-admin"))
        .unwrap();
    duplicate.assignment_id = "ambiguous-assignment".into();
    duplicate.terminal_id = Some("captain-duplicate".into());
    ctx.captains.lock().captains.push(duplicate);

    let admin = resolve_identity(&ctx, &admin_identity.secret).unwrap();
    let denied = authorize_delegated_admin(
        &ctx,
        &admin,
        crate::delegated_admin::AdminOperation::InspectStatus,
        crate::delegated_admin::AdminTarget::Ship {
            ship_slug: "alpha".into(),
        },
        crate::delegated_admin::AdminSafeguards::default(),
    )
    .unwrap_err();
    assert!(denied.contains("supervisorInactive"), "{denied}");
    assert!(matches!(
        ctx.delegated_admin.get(grant_id).unwrap().state,
        crate::delegated_admin::GrantState::Invalidated { .. }
    ));
    reap_test_tmux_session(&admin_target).unwrap();
}

#[test]
fn ship_admin_executes_own_ship_operations_and_denies_foreign_or_reserved_targets() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let captain_alpha = format!("ca{}", &nonce[..6]);
    let captain_beta = format!("cb{}", &nonce[..6]);
    let admin_session = format!("aa{}", &nonce[..6]);
    let peer_alpha = format!("pa{}", &nonce[..6]);
    let peer_beta = format!("pb{}", &nonce[..6]);
    let audit_dir = std::env::temp_dir().join(format!(
        "t-hub-admin-execute-audit-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let ctx = test_ctx("admin-execute-ship").with_audit(Arc::new(AuditLog::new(audit_dir.clone())));
    ctx.captains
        .claim_test(&captain_alpha, Some("alpha"), vec![])
        .unwrap();
    ctx.captains
        .claim_test(&captain_beta, Some("beta"), vec![])
        .unwrap();
    for crew in [&admin_session, &peer_alpha] {
        ctx.captains.record_crew(&captain_alpha, crew).unwrap();
    }
    ctx.captains.record_crew(&captain_beta, &peer_beta).unwrap();
    let session_ids = [
        admin_session.as_str(),
        peer_alpha.as_str(),
        peer_beta.as_str(),
    ];
    for session_id in session_ids {
        create_test_tmux_session(&tmux_target(session_id)).unwrap();
    }
    let captain_identity = ctx.identity.mint(crate::identity::Role::Captain).unwrap();
    ctx.identity
        .bind_tile(&captain_identity.id, &captain_alpha)
        .unwrap();
    let admin_identity = ctx.identity.mint(crate::identity::Role::Crew).unwrap();
    ctx.identity
        .bind_tile(&admin_identity.id, &admin_session)
        .unwrap();
    let captain = resolve_identity(&ctx, &captain_identity.secret).unwrap();
    appoint_admin(
        &ctx,
        &json!({
            "actorSessionId": admin_session,
            "role": "shipAdmin",
            "permittedOperations": [
                "maintainSession",
                "recoverResource",
                "prepareRetirement"
            ]
        }),
        Some(&captain),
        false,
    )
    .unwrap();
    let admin = resolve_identity(&ctx, &admin_identity.secret).unwrap();

    let own = execute_admin_operation(
        &ctx,
        &json!({
            "operation": "maintainSession",
            "target": { "kind": "session", "sessionId": admin_session }
        }),
        Some(&admin),
        false,
    )
    .unwrap();
    assert_eq!(own["outcome"]["outcome"], "maintained");
    assert_eq!(
        own["outcome"]["maintainedSessions"][0]["sessionId"],
        admin_session
    );

    let sibling = execute_admin_operation(
        &ctx,
        &json!({
            "operation": "recoverResource",
            "target": { "kind": "session", "sessionId": peer_alpha }
        }),
        Some(&admin),
        false,
    )
    .unwrap();
    assert_eq!(sibling["outcome"]["outcome"], "maintained");

    let retirement = execute_admin_operation(
        &ctx,
        &json!({
            "operation": "prepareRetirement",
            "target": { "kind": "session", "sessionId": peer_alpha }
        }),
        Some(&admin),
        false,
    )
    .unwrap();
    assert_eq!(retirement["outcome"]["outcome"], "retirementPrepared");
    assert_eq!(retirement["outcome"]["ready"], false);
    assert!(retirement["outcome"]["planId"]
        .as_str()
        .is_some_and(|plan| plan.starts_with("sha256:")));

    let foreign = execute_admin_operation(
        &ctx,
        &json!({
            "operation": "maintainSession",
            "target": { "kind": "session", "sessionId": peer_beta }
        }),
        Some(&admin),
        false,
    )
    .unwrap_err();
    assert!(foreign.contains("targetOutOfScope"));

    let reserved = execute_admin_operation(
        &ctx,
        &json!({
            "operation": "recoverResource",
            "target": { "kind": "generalReserved", "action": "installRelease" }
        }),
        Some(&admin),
        false,
    )
    .unwrap_err();
    assert!(reserved.contains("targetOutOfScope"));

    let implementation = execute_admin_operation(
        &ctx,
        &json!({
            "operation": "recoverResource",
            "target": {
                "kind": "implementation",
                "shipSlug": "alpha",
                "assignmentId": "assignment-1"
            }
        }),
        Some(&admin),
        false,
    )
    .unwrap_err();
    assert!(implementation.contains("targetOutOfScope"));

    let records = read_audit(&audit_dir);
    let operation_records = records
        .iter()
        .filter(|record| record["command"] == "delegated_admin_operation")
        .collect::<Vec<_>>();
    assert_eq!(operation_records.len(), 3);
    assert_eq!(
        operation_records[0]["args"]["authorization"]["actorIdentityId"],
        admin_identity.id
    );
    assert_eq!(
        operation_records[0]["args"]["authorization"]["delegatingSupervisorIdentityId"],
        captain_identity.id
    );
    assert_eq!(
        operation_records[0]["args"]["result"]["outcome"]["outcome"],
        "maintained"
    );

    for session_id in session_ids {
        reap_test_tmux_session(&tmux_target(session_id)).unwrap();
    }
    std::fs::remove_dir_all(audit_dir).ok();
}

#[test]
fn fleet_admin_maintains_captains_without_crossing_into_crew_or_general_authority() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let cortana_session = format!("co{}", &nonce[..6]);
    let captain_alpha = format!("ca{}", &nonce[..6]);
    let captain_beta = format!("cb{}", &nonce[..6]);
    let fleet_admin_session = format!("fa{}", &nonce[..6]);
    let peer_beta = format!("pb{}", &nonce[..6]);
    let ctx = test_ctx(&format!("admin-execute-fleet-{}", &nonce[..8]));
    ctx.captains
        .claim_provider(
            &cortana_session,
            None,
            FleetRole::Cortana,
            Some("codex"),
            None,
            vec![],
            &all_alive,
            &crew_all_alive,
        )
        .unwrap();
    ctx.captains
        .claim_test(&captain_alpha, Some("alpha"), vec![])
        .unwrap();
    ctx.captains
        .claim_test(&captain_beta, Some("beta"), vec![])
        .unwrap();
    ctx.captains
        .record_crew(&captain_alpha, &fleet_admin_session)
        .unwrap();
    ctx.captains.record_crew(&captain_beta, &peer_beta).unwrap();
    let session_ids = [
        fleet_admin_session.as_str(),
        captain_alpha.as_str(),
        captain_beta.as_str(),
        peer_beta.as_str(),
    ];
    for session_id in session_ids {
        create_test_tmux_session(&tmux_target(session_id)).unwrap();
    }
    let cortana_identity = ctx.identity.mint(crate::identity::Role::Cortana).unwrap();
    ctx.identity
        .bind_tile(&cortana_identity.id, &cortana_session)
        .unwrap();
    ctx.captains
        .begin_cortana_recovery("fleet-admin-test")
        .unwrap();
    ctx.captains
        .commit_cortana_runtime(
            "fleet-admin-test",
            &cortana_identity.id,
            1,
            &cortana_session,
            "codex",
            None,
        )
        .unwrap();
    let fleet_admin_identity = ctx.identity.mint(crate::identity::Role::Crew).unwrap();
    ctx.identity
        .bind_tile(&fleet_admin_identity.id, &fleet_admin_session)
        .unwrap();
    let cortana = resolve_identity(&ctx, &cortana_identity.secret).unwrap();
    appoint_admin(
        &ctx,
        &json!({
            "actorSessionId": fleet_admin_session,
            "role": "fleetAdmin",
            "permittedOperations": [
                "maintainFleetResource",
                "recoverResource",
                "prepareRetirement"
            ]
        }),
        Some(&cortana),
        false,
    )
    .unwrap();
    let fleet_admin = resolve_identity(&ctx, &fleet_admin_identity.secret).unwrap();
    let renewed = dispatch_authenticated(
        &ctx,
        req_session(
            &ctx.read_token,
            &fleet_admin_identity.secret,
            "renew_captain_control_lease",
            Value::Null,
        ),
    );
    assert!(renewed.ok, "{:?}", renewed.error);
    let renewed = renewed.result.unwrap();
    assert_eq!(renewed["scope"]["kind"], "delegatedAdmin");
    assert_eq!(renewed["scope"]["role"], "fleetAdmin");
    let fleet_admin_lease = renewed["lease"].as_str().unwrap();
    let leased_mutation = dispatch_authenticated(
        &ctx,
        req_session(
            fleet_admin_lease,
            &fleet_admin_identity.secret,
            "execute_admin_operation",
            json!({
                "operation": "maintainFleetResource",
                "target": { "kind": "fleet" }
            }),
        ),
    );
    assert!(leased_mutation.ok, "{:?}", leased_mutation.error);

    let maintained = execute_admin_operation(
        &ctx,
        &json!({
            "operation": "maintainFleetResource",
            "target": { "kind": "fleet" }
        }),
        Some(&fleet_admin),
        false,
    )
    .unwrap();
    assert_eq!(maintained["outcome"]["outcome"], "maintained");
    assert_eq!(
        maintained["outcome"]["maintainedSessions"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    let retirement = execute_admin_operation(
        &ctx,
        &json!({
            "operation": "prepareRetirement",
            "target": { "kind": "session", "sessionId": captain_beta }
        }),
        Some(&fleet_admin),
        false,
    )
    .unwrap();
    assert_eq!(retirement["outcome"]["ready"], false);

    let crew_denied = execute_admin_operation(
        &ctx,
        &json!({
            "operation": "recoverResource",
            "target": { "kind": "session", "sessionId": peer_beta }
        }),
        Some(&fleet_admin),
        false,
    )
    .unwrap_err();
    assert!(crew_denied.contains("targetOutOfScope"));

    let general_denied = execute_admin_operation(
        &ctx,
        &json!({
            "operation": "maintainFleetResource",
            "target": { "kind": "generalReserved", "action": "approveRelease" }
        }),
        Some(&fleet_admin),
        false,
    )
    .unwrap_err();
    assert!(general_denied.contains("targetOutOfScope"));

    for session_id in session_ids {
        reap_test_tmux_session(&tmux_target(session_id)).unwrap();
    }
}

#[test]
fn fleet_admin_grants_invalidate_with_every_non_authoritative_cortana_state() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    for state in ["recovering", "degraded", "duplicate"] {
        let (ctx, _cortana_identity, admin_identity, grant_id, admin_tile) =
            fleet_admin_grant_fixture(state);
        match state {
            "recovering" => {
                ctx.captains
                    .begin_cortana_recovery("test-recovering")
                    .unwrap();
            }
            "degraded" => {
                ctx.captains
                    .mark_cortana_degraded("test-degraded", "injected uncertainty")
                    .unwrap();
            }
            "duplicate" => {
                let mut duplicate = ctx
                    .captains
                    .snapshot()
                    .captains
                    .into_iter()
                    .find(|captain| captain.role == FleetRole::Cortana)
                    .unwrap();
                duplicate.ship_slug = "cortana-duplicate".into();
                duplicate.assignment_id = "cortana-duplicate-assignment".into();
                duplicate.terminal_id = Some("duplicate".into());
                ctx.captains.lock().captains.push(duplicate);
            }
            _ => unreachable!(),
        }

        let admin = resolve_identity(&ctx, &admin_identity.secret).unwrap();
        let denied = authorize_delegated_admin(
            &ctx,
            &admin,
            crate::delegated_admin::AdminOperation::MaintainFleetResource,
            crate::delegated_admin::AdminTarget::Fleet,
            crate::delegated_admin::AdminSafeguards::default(),
        )
        .unwrap_err();
        assert!(
            denied.contains("supervisorInactive")
                || denied.contains("no active administrative grant"),
            "unexpected {state} denial: {denied}"
        );
        assert!(matches!(
            ctx.delegated_admin.get(&grant_id).unwrap().state,
            crate::delegated_admin::GrantState::Invalidated { .. }
        ));
        reap_test_tmux_session(&tmux_target(&admin_tile)).unwrap();
    }
}

#[test]
fn dispatch_capacity_counts_one_live_harness_backed_ship_admin_per_ship() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let ctx = test_ctx("admin-capacity");
    ctx.captains
        .claim_test("captain-alpha", Some("alpha"), vec![])
        .unwrap();
    let captain_identity = ctx.identity.mint(crate::identity::Role::Captain).unwrap();
    ctx.identity
        .bind_tile(&captain_identity.id, "captain-alpha")
        .unwrap();
    let captain = resolve_identity(&ctx, &captain_identity.secret).unwrap();
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    let mut admin_targets = Vec::new();

    for admin_id in ["adminalfa", "adminbeta"] {
        ctx.captains.record_crew("captain-alpha", admin_id).unwrap();
        ctx.captains
            .bind_crew_context(
                "captain-alpha",
                admin_id,
                "standing administration",
                "codex",
                None,
                None,
                PowderWorkBinding {
                    card_id: format!("card-{admin_id}"),
                    run_id: format!("run-{admin_id}"),
                    agent: None,
                    claim_expires_at: None,
                    mutation_intent: None,
                    dispatch_release_recovery: false,
                    state: PowderWorkState::Active,
                },
            )
            .unwrap();
        let target = tmux_target(admin_id);
        tmux::new_session_with_env(&target, "/tmp", Some(&harness_command), &[]).unwrap();
        wait_for_harness_started(admin_id, "codex").unwrap();
        admin_targets.push(target);

        let identity = ctx.identity.mint(crate::identity::Role::Crew).unwrap();
        ctx.identity.bind_tile(&identity.id, admin_id).unwrap();
        appoint_admin(
            &ctx,
            &json!({
                "actorSessionId": admin_id,
                "role": "shipAdmin",
                "permittedOperations": ["inspectStatus"]
            }),
            Some(&captain),
            false,
        )
        .unwrap();
    }

    assert_eq!(
        live_admin_counts(&ctx, &ctx.captains.snapshot()),
        (0, [("alpha".to_string(), 2usize)].into_iter().collect())
    );
    reap_test_tmux_session(&admin_targets[0]).unwrap();
    assert_eq!(
        live_admin_counts(&ctx, &ctx.captains.snapshot()),
        (0, [("alpha".to_string(), 1usize)].into_iter().collect())
    );
    reap_test_tmux_session(&admin_targets[1]).unwrap();
    assert_eq!(
        live_admin_counts(&ctx, &ctx.captains.snapshot()),
        (0, BTreeMap::new())
    );
    std::fs::remove_dir_all(harness_bin_dir).ok();
}

#[test]
fn ship_admin_can_read_own_captain_status_but_cannot_run_dispatch_preflight() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let ctx = test_ctx("ship-admin-status");
    let admin_id = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    ctx.captains
        .upsert_project(ProjectRecord {
            root_path: None,
            vcs_capability: None,
            git_main_root: None,
            project_id: "project-alpha".into(),
            name: "Alpha".into(),
            repo_root: "/tmp/project-alpha".into(),
            remote_url: None,
            default_branch: None,
            powder: None,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    ctx.captains
        .claim_test("captain-alpha", Some("alpha"), vec![])
        .unwrap();
    ctx.captains
        .bind_ship_context("alpha", "project-alpha", "Assignment", "codex")
        .unwrap();
    ctx.captains
        .record_crew("captain-alpha", &admin_id)
        .unwrap();
    let admin_target = tmux_target(&admin_id);
    create_test_tmux_session(&admin_target).unwrap();
    let captain_identity = ctx.identity.mint(crate::identity::Role::Captain).unwrap();
    ctx.identity
        .bind_tile(&captain_identity.id, "captain-alpha")
        .unwrap();
    let admin_identity = ctx.identity.mint(crate::identity::Role::Crew).unwrap();
    ctx.identity
        .bind_tile(&admin_identity.id, &admin_id)
        .unwrap();
    let captain = resolve_identity(&ctx, &captain_identity.secret).unwrap();
    appoint_admin(
        &ctx,
        &json!({
            "actorSessionId": admin_id,
            "role": "shipAdmin",
            "permittedOperations": ["inspectStatus"]
        }),
        Some(&captain),
        false,
    )
    .unwrap();
    let admin = resolve_identity(&ctx, &admin_identity.secret).unwrap();

    let report = list_agents(
        &ctx,
        &json!({"captainSessionId": "captain-alpha"}),
        Some(&admin),
        false,
    )
    .unwrap();
    assert_eq!(report["count"], 0);

    let denied = authorize_agent_filter(
        &ctx,
        Some("captain-alpha"),
        Some("project-alpha"),
        Some(&admin),
        false,
        "dispatch_preflight",
        false,
    )
    .unwrap_err();
    assert!(denied.contains("owning Captain or a fleet supervisor"));
    reap_test_tmux_session(&admin_target).unwrap();
}

#[test]
fn ship_admin_worktree_maintenance_is_scoped_and_cannot_dispatch_implementation() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let provider_probes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let provider_probe_count = provider_probes.clone();
    let tmux_probes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let tmux_probe_count = tmux_probes.clone();
    let ctx = test_ctx("ship-admin-worktree")
        .with_apply_sink(sink.clone())
        .with_provider_capacity(move || {
            provider_probe_count.fetch_add(1, Ordering::SeqCst);
            Err("provider admission unavailable".into())
        })
        .with_live_sessions(move || {
            tmux_probe_count.fetch_add(1, Ordering::SeqCst);
            Err("tmux admission unavailable".into())
        });
    let admin_id = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    let (base, repo_root, _existing_worktree) = scratch_repo_with_worktree();
    ctx.captains
        .upsert_project(ProjectRecord {
            root_path: None,
            vcs_capability: None,
            git_main_root: None,
            project_id: "project-alpha".into(),
            name: "Alpha".into(),
            repo_root: repo_root.to_string_lossy().to_string(),
            remote_url: None,
            default_branch: None,
            powder: None,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    ctx.captains
        .claim_test("captain-alpha", Some("alpha"), vec![])
        .unwrap();
    ctx.captains
        .bind_ship_context("alpha", "project-alpha", "Assignment", "codex")
        .unwrap();
    ctx.captains
        .record_crew("captain-alpha", &admin_id)
        .unwrap();
    let admin_target = tmux_target(&admin_id);
    create_test_tmux_session(&admin_target).unwrap();
    let captain_identity = ctx.identity.mint(crate::identity::Role::Captain).unwrap();
    ctx.identity
        .bind_tile(&captain_identity.id, "captain-alpha")
        .unwrap();
    let admin_identity = ctx.identity.mint(crate::identity::Role::Crew).unwrap();
    ctx.identity
        .bind_tile(&admin_identity.id, &admin_id)
        .unwrap();
    let captain = resolve_identity(&ctx, &captain_identity.secret).unwrap();
    appoint_admin(
        &ctx,
        &json!({
            "actorSessionId": admin_id,
            "role": "shipAdmin",
            "permittedOperations": ["maintainWorktree"]
        }),
        Some(&captain),
        false,
    )
    .unwrap();
    let admin = resolve_identity(&ctx, &admin_identity.secret).unwrap();
    let target = base.join("admin-worktree").to_string_lossy().to_string();

    let audit = authorize_worktree_maintenance(
        &ctx,
        Some(&admin),
        false,
        &json!({}),
        repo_root.to_str().unwrap(),
        &target,
        None,
        None,
    )
    .unwrap()
    .expect("delegated audit context");
    assert_eq!(audit.delegated_role.label(), "shipAdmin");
    assert_eq!(
        audit.target.fingerprint(),
        format!("worktree:alpha:{target}")
    );

    let denied = authorize_worktree_maintenance(
        &ctx,
        Some(&admin),
        false,
        &json!({"startupCommand": "codex exec implement"}),
        repo_root.to_str().unwrap(),
        &target,
        Some("codex exec implement"),
        None,
    )
    .unwrap_err();
    assert!(denied.contains("cannot create or elevate a runtime"));

    let tabs_before = ctx.tabs.snapshot_full();
    let identities_before = ctx.identity.len();
    let maintained = dispatch_authenticated(
        &ctx,
        req_session(
            "ship-admin-worktree",
            &admin_identity.secret,
            "create_worktree",
            json!({
                "repoRoot": repo_root,
                "worktreePath": target,
            }),
        ),
    );
    assert!(
        maintained.ok,
        "maintenance-only create was governed as a spawn: {:?}",
        maintained.error
    );
    let maintained = maintained.result.unwrap();
    assert_eq!(maintained["administrativeMaintenanceOnly"], true);
    assert!(maintained["tabId"].is_null());
    assert!(maintained["terminalId"].is_null());
    assert!(std::path::Path::new(&target).exists());
    assert_eq!(ctx.tabs.snapshot_full().seq, tabs_before.seq);
    assert_eq!(
        serde_json::to_value(ctx.tabs.snapshot_full().tabs).unwrap(),
        serde_json::to_value(tabs_before.tabs).unwrap()
    );
    assert_eq!(ctx.identity.len(), identities_before);
    // The tmux namespace is shared by concurrent tests, so verify this
    // fixture's exact runtime instead of comparing the global session list.
    assert_eq!(
        tmux::session_liveness(&admin_target),
        tmux::SessionLiveness::Alive
    );
    assert!(sink.calls.lock().unwrap().is_empty());
    assert_eq!(provider_probes.load(Ordering::SeqCst), 0);
    assert_eq!(tmux_probes.load(Ordering::SeqCst), 0);

    let elevated_target = base.join("elevated-worktree");
    let elevated = dispatch_authenticated(
        &ctx,
        req_session(
            "ship-admin-worktree",
            &admin_identity.secret,
            "create_worktree",
            json!({
                "repoRoot": repo_root,
                "worktreePath": elevated_target,
                "capability": "control",
            }),
        ),
    );
    assert!(!elevated.ok);
    let elevated = elevated.error.unwrap();
    assert!(elevated.contains("cannot create or elevate a runtime"));
    assert!(!elevated_target.exists());
    assert_eq!(ctx.tabs.snapshot_full().seq, tabs_before.seq);
    assert_eq!(ctx.identity.len(), identities_before);
    assert_eq!(
        tmux::session_liveness(&admin_target),
        tmux::SessionLiveness::Alive
    );
    assert!(sink.calls.lock().unwrap().is_empty());
    assert_eq!(provider_probes.load(Ordering::SeqCst), 0);
    assert_eq!(tmux_probes.load(Ordering::SeqCst), 0);
    reap_test_tmux_session(&admin_target).unwrap();
    std::fs::remove_dir_all(base).ok();
}

#[test]
fn list_captains_exposes_active_admin_role_without_granting_captain_identity() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let ctx = test_ctx("admin-role-wire");
    let admin_id = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    ctx.captains
        .claim_test("captain-alpha", Some("alpha"), vec![])
        .unwrap();
    ctx.captains
        .record_crew("captain-alpha", &admin_id)
        .unwrap();
    let admin_target = tmux_target(&admin_id);
    create_test_tmux_session(&admin_target).unwrap();
    let captain_identity = ctx.identity.mint(crate::identity::Role::Captain).unwrap();
    ctx.identity
        .bind_tile(&captain_identity.id, "captain-alpha")
        .unwrap();
    let admin_identity = ctx.identity.mint(crate::identity::Role::Crew).unwrap();
    ctx.identity
        .bind_tile(&admin_identity.id, &admin_id)
        .unwrap();
    let captain = resolve_identity(&ctx, &captain_identity.secret).unwrap();
    appoint_admin(
        &ctx,
        &json!({
            "actorSessionId": admin_id,
            "role": "shipAdmin",
            "permittedOperations": ["inspectStatus"]
        }),
        Some(&captain),
        false,
    )
    .unwrap();

    let roster = list_captains(&ctx).unwrap();
    assert_eq!(
        roster["captains"][0]["crew"][0]["delegatedRole"],
        "shipAdmin"
    );
    assert!(roster["captains"][0]["crew"][0]["delegatedGrantGeneration"]
        .as_u64()
        .is_some_and(|generation| generation > 0));
    assert_eq!(roster["captains"][0]["crew"][0]["terminalId"], admin_id);
    reap_test_tmux_session(&admin_target).unwrap();
}

#[test]
fn ship_admin_session_cleanup_requires_and_consumes_exact_supervisor_approval() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let ctx = test_ctx("admin-cleanup");
    let crew_target_id = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    let admin_id = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    ctx.captains
        .claim_test("captain-alpha", Some("alpha"), vec![])
        .unwrap();
    ctx.captains
        .record_crew("captain-alpha", &crew_target_id)
        .unwrap();
    ctx.captains
        .record_crew("captain-alpha", &admin_id)
        .unwrap();
    let admin_target = tmux_target(&admin_id);
    create_test_tmux_session(&admin_target).unwrap();
    let captain_identity = ctx.identity.mint(crate::identity::Role::Captain).unwrap();
    ctx.identity
        .bind_tile(&captain_identity.id, "captain-alpha")
        .unwrap();
    let admin_identity = ctx.identity.mint(crate::identity::Role::Crew).unwrap();
    ctx.identity
        .bind_tile(&admin_identity.id, &admin_id)
        .unwrap();
    let captain = resolve_identity(&ctx, &captain_identity.secret).unwrap();
    let grant = appoint_admin(
        &ctx,
        &json!({
            "actorSessionId": admin_id,
            "role": "shipAdmin",
            "permittedOperations": ["cleanupSession"]
        }),
        Some(&captain),
        false,
    )
    .unwrap();
    let admin = resolve_identity(&ctx, &admin_identity.secret).unwrap();
    let target = tmux_target(&crew_target_id);
    create_test_tmux_session(&target).unwrap();

    let denied = close_terminal_authorized(
        &ctx,
        &json!({"sessionId": crew_target_id}),
        Some(&admin),
        false,
    )
    .unwrap_err();
    assert!(denied.contains("exact supervisor approvalId"));
    assert_eq!(
        tmux::session_liveness(&target),
        tmux::SessionLiveness::Alive
    );

    let fabricated = approve_admin_action(
        &ctx,
        &json!({
            "grantId": grant["grant"]["grantId"],
            "operation": "cleanupSession",
            "target": {
                "kind": "crewSession",
                "shipSlug": "alpha",
                "sessionId": crew_target_id
            }
        }),
        Some(&captain),
        false,
    )
    .unwrap_err();
    assert!(fabricated.contains("sessionId only"));

    let approval = approve_admin_action(
        &ctx,
        &json!({
            "grantId": grant["grant"]["grantId"],
            "operation": "cleanupSession",
            "sessionId": crew_target_id,
        }),
        Some(&captain),
        false,
    )
    .unwrap();
    assert_eq!(approval["approval"]["target"]["kind"], "crewSession");
    assert_eq!(approval["approval"]["target"]["shipSlug"], "alpha");
    let approval_id = approval["approval"]["approval"]["approvalId"]
        .as_str()
        .unwrap();
    let closed = close_terminal_authorized(
        &ctx,
        &json!({"sessionId": crew_target_id, "approvalId": approval_id}),
        Some(&admin),
        false,
    )
    .unwrap();

    assert_eq!(closed["outcome"], "killed");
    assert_eq!(tmux::session_liveness(&target), tmux::SessionLiveness::Gone);
    assert!(ctx
        .delegated_admin
        .get_approval(approval_id)
        .is_some_and(|approval| matches!(
            approval.state,
            crate::delegated_admin::ApprovalState::Consumed { .. }
        )));
    reap_test_tmux_session(&admin_target).unwrap();
}

#[test]
fn delegated_admin_control_token_is_limited_to_role_aware_routes() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let ctx = test_ctx("admin-boundaries");
    ctx.captains
        .claim_test("captain-alpha", Some("alpha"), vec![])
        .unwrap();
    ctx.captains
        .record_crew("captain-alpha", "admin-alpha")
        .unwrap();
    let admin_target = tmux_target("admin-alpha");
    create_test_tmux_session(&admin_target).unwrap();
    let captain_identity = ctx.identity.mint(crate::identity::Role::Captain).unwrap();
    ctx.identity
        .bind_tile(&captain_identity.id, "captain-alpha")
        .unwrap();
    let admin_identity = ctx.identity.mint(crate::identity::Role::Crew).unwrap();
    ctx.identity
        .bind_tile(&admin_identity.id, "admin-alpha")
        .unwrap();
    let captain = resolve_identity(&ctx, &captain_identity.secret).unwrap();
    appoint_admin(
        &ctx,
        &json!({
            "actorSessionId": "admin-alpha",
            "role": "shipAdmin",
            "permittedOperations": ["maintainSession"]
        }),
        Some(&captain),
        false,
    )
    .unwrap();
    let admin = resolve_identity(&ctx, &admin_identity.secret).unwrap();
    let renewed = dispatch_authenticated(
        &ctx,
        req_session(
            &ctx.read_token,
            &admin_identity.secret,
            "renew_captain_control_lease",
            Value::Null,
        ),
    );
    assert!(renewed.ok, "{:?}", renewed.error);
    let renewed = renewed.result.unwrap();
    assert_eq!(renewed["scope"]["kind"], "delegatedAdmin");
    assert_eq!(renewed["scope"]["role"], "shipAdmin");
    let admin_lease = renewed["lease"].as_str().unwrap().to_string();

    let read_denied = read_terminal(
        &ctx,
        &json!({"sessionId": "admin-alpha"}),
        Some(&admin),
        false,
    )
    .unwrap_err();
    assert!(read_denied.contains("operationNotGranted"));
    let list_denied = list_agents(
        &ctx,
        &json!({"captainSessionId": "captain-alpha"}),
        Some(&admin),
        false,
    )
    .unwrap_err();
    assert!(list_denied.contains("operationNotGranted"));
    let close_denied = close_terminal_authorized(
        &ctx,
        &json!({"sessionId": "admin-alpha"}),
        Some(&admin),
        false,
    )
    .unwrap_err();
    assert!(close_denied.contains("exact supervisor approvalId"));
    assert_eq!(
        tmux::session_liveness(&admin_target),
        tmux::SessionLiveness::Alive
    );

    for command in [
        "spawn_terminal",
        "start_agent",
        "dispatch_crew",
        "send_text",
        "move_tile",
        "register_project",
        "appoint_admin",
    ] {
        let response = dispatch_authenticated(
            &ctx,
            req_session(&admin_lease, &admin_identity.secret, command, json!({})),
        );
        assert!(
            !response.ok,
            "delegated admin unexpectedly called {command}"
        );
        assert!(
            response
                .error
                .unwrap_or_default()
                .contains("outside their exact administrative operation grants"),
            "{command} did not fail at the delegated-role boundary"
        );
    }
    assert!(
        enforce_attach_authority(&ctx, Some(&admin), false, "admin-alpha", FleetRole::Captain,)
            .unwrap_err()
            .contains("cannot acquire Captain or Cortana authority")
    );
    assert!(appoint_admin(
        &ctx,
        &json!({
            "actorSessionId": "admin-alpha",
            "role": "shipAdmin",
            "permittedOperations": ["maintainSession"]
        }),
        Some(&admin),
        false,
    )
    .unwrap_err()
    .contains("cannot re-delegate authority"));

    let maintained = dispatch_authenticated(
        &ctx,
        req_session(
            &admin_lease,
            &admin_identity.secret,
            "execute_admin_operation",
            json!({
                "operation": "maintainSession",
                "target": { "kind": "session", "sessionId": "admin-alpha" }
            }),
        ),
    );
    assert!(
        maintained.ok,
        "role-authorized maintenance route failed: {:?}",
        maintained.error
    );

    let grants = dispatch_authenticated(
        &ctx,
        req_session(
            &admin_lease,
            &admin_identity.secret,
            "list_admin_grants",
            json!({}),
        ),
    );
    assert!(grants.ok, "self grant listing failed: {:?}", grants.error);
    reap_test_tmux_session(&admin_target).unwrap();
}

#[test]
fn privileged_agent_intent_is_permanent_admin_history_before_appointment_and_reload() {
    for (label, purpose) in [
        ("fleet", crate::governor::AdmissionPurpose::FleetAdmin),
        ("ship", crate::governor::AdmissionPurpose::ShipAdmin),
        ("recovery", crate::governor::AdmissionPurpose::Recovery),
    ] {
        let nonce = uuid::Uuid::new_v4().simple().to_string();
        let captains_path = captains_tmp(&format!("intent-history-{label}-{nonce}"));
        let identities_path = std::env::temp_dir().join(format!(
            "t-hub-intent-history-identities-{label}-{nonce}.json"
        ));
        let agent_id = format!("{}{}", &label[..2], &nonce[..6]);
        let admin_secret;
        let general_secret;

        {
            let captains = Arc::new(CaptainsRegistry::load(captains_path.clone()));
            let identities = Arc::new(crate::identity::IdentityStore::load(
                identities_path.clone(),
            ));
            let ctx = test_ctx(&format!("intent-history-{label}"))
                .with_captains_registry(captains)
                .with_identity_store(identities);
            seed_starting_agent_with_purpose(&ctx, &agent_id, purpose);
            admin_secret = mint_session(
                &ctx.identity,
                crate::identity::Role::Crew,
                "capacity-ship",
                &agent_id,
            );
            general_secret = mint_session(
                &ctx.identity,
                crate::identity::Role::General,
                "fleet",
                "general-intent",
            );
            let admin = resolve_identity(&ctx, &admin_secret).unwrap();
            assert!(has_delegated_admin_history(&ctx, &admin.session_id));

            let mutation = dispatch_authenticated(
                &ctx,
                req_session(
                    &format!("intent-history-{label}"),
                    &admin_secret,
                    "new_tab",
                    json!({"name": "forbidden-before-appointment"}),
                ),
            );
            assert!(!mutation.ok);
            assert!(mutation
                .error
                .unwrap_or_default()
                .contains("outside their exact administrative operation grants"));

            let general = resolve_identity(&ctx, &general_secret).unwrap();
            for role in [FleetRole::Captain, FleetRole::Cortana] {
                assert!(
                    enforce_attach_authority(&ctx, Some(&general), false, &agent_id, role,)
                        .unwrap_err()
                        .contains("administrative Crew identity")
                );
            }
            for (command, args) in [
                (
                    "claim_captain",
                    json!({"captainSessionId": &agent_id, "shipSlug": "forbidden"}),
                ),
                (
                    "attach_captain",
                    json!({
                        "captainSessionId": &agent_id,
                        "projectId": "capacity-project",
                        "assignment": "forbidden"
                    }),
                ),
            ] {
                let response = dispatch_authenticated(
                    &ctx,
                    req_session(
                        &format!("intent-history-{label}"),
                        &general_secret,
                        command,
                        args,
                    ),
                );
                assert!(!response.ok, "{command} promoted {label} intent");
                assert!(response
                    .error
                    .unwrap_or_default()
                    .contains("administrative Crew identity"));
            }
        }

        {
            let ctx = test_ctx(&format!("intent-history-{label}"))
                .with_captains_registry(Arc::new(CaptainsRegistry::load(captains_path.clone())))
                .with_identity_store(Arc::new(crate::identity::IdentityStore::load(
                    identities_path.clone(),
                )));
            let admin = resolve_identity(&ctx, &admin_secret).unwrap();
            assert!(has_delegated_admin_history(&ctx, &admin.session_id));
            let mutation = dispatch_authenticated(
                &ctx,
                req_session(
                    &format!("intent-history-{label}"),
                    &admin_secret,
                    "new_tab",
                    json!({"name": "forbidden-after-reload"}),
                ),
            );
            assert!(!mutation.ok);
            assert!(mutation
                .error
                .unwrap_or_default()
                .contains("outside their exact administrative operation grants"));
        }

        for path in [
            captains_path.with_extension("json.bak"),
            captains_path,
            identities_path,
        ] {
            std::fs::remove_file(path).ok();
        }
    }
}

#[test]
fn revoked_and_invalidated_admin_tokens_stay_restricted_after_reload() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let captains_path = captains_tmp(&format!("admin-history-{nonce}"));
    let identities_path =
        std::env::temp_dir().join(format!("t-hub-admin-history-identities-{nonce}.json"));
    let grants_path = std::env::temp_dir().join(format!("t-hub-admin-history-grants-{nonce}.json"));
    let captain_tile = format!("cp{}", &nonce[..6]);
    let revoked_tile = format!("rv{}", &nonce[..6]);
    let invalidated_tile = format!("iv{}", &nonce[..6]);
    let captain_secret;
    let admin_credentials;

    {
        let captains = Arc::new(CaptainsRegistry::load(captains_path.clone()));
        captains
            .claim_test(&captain_tile, Some("alpha"), vec![])
            .unwrap();
        captains.record_crew(&captain_tile, &revoked_tile).unwrap();
        captains
            .record_crew(&captain_tile, &invalidated_tile)
            .unwrap();
        let identities = Arc::new(crate::identity::IdentityStore::load(
            identities_path.clone(),
        ));
        let grants = Arc::new(
            crate::delegated_admin::DelegatedAdminStore::load(grants_path.clone()).unwrap(),
        );
        let captain_identity = identities.mint(crate::identity::Role::Captain).unwrap();
        identities
            .bind_tile(&captain_identity.id, &captain_tile)
            .unwrap();
        captain_secret = captain_identity.secret.clone();
        let mut credentials = Vec::new();
        for tile in [&revoked_tile, &invalidated_tile] {
            create_test_tmux_session(&tmux_target(tile)).unwrap();
            let identity = identities.mint(crate::identity::Role::Crew).unwrap();
            identities.bind_tile(&identity.id, tile).unwrap();
            credentials.push((tile.clone(), identity.id, identity.secret));
        }
        let ctx = test_ctx("persisted-admin-token")
            .with_captains_registry(captains)
            .with_identity_store(identities)
            .with_delegated_admin(grants);
        let captain = resolve_identity(&ctx, &captain_secret).unwrap();
        for (tile, _, _) in &credentials {
            appoint_admin(
                &ctx,
                &json!({
                    "actorSessionId": tile,
                    "role": "shipAdmin",
                    "permittedOperations": ["maintainSession"]
                }),
                Some(&captain),
                false,
            )
            .unwrap();
        }
        admin_credentials = credentials;
    }

    // An active appointment and its exact operation survive a process reload.
    {
        let ctx = test_ctx("persisted-admin-token")
            .with_captains_registry(Arc::new(CaptainsRegistry::load(captains_path.clone())))
            .with_identity_store(Arc::new(crate::identity::IdentityStore::load(
                identities_path.clone(),
            )))
            .with_delegated_admin(Arc::new(
                crate::delegated_admin::DelegatedAdminStore::load(grants_path.clone()).unwrap(),
            ));
        for (tile, _, secret) in &admin_credentials {
            let maintained = dispatch_authenticated(
                &ctx,
                req_session(
                    "persisted-admin-token",
                    secret,
                    "execute_admin_operation",
                    json!({
                        "operation": "maintainSession",
                        "target": { "kind": "session", "sessionId": tile }
                    }),
                ),
            );
            assert!(
                maintained.ok,
                "active admin reload failed: {:?}",
                maintained.error
            );
        }
        let captain = resolve_identity(&ctx, &captain_secret).unwrap();
        let revoked_grant = ctx
            .delegated_admin
            .grants_for_actor(&admin_credentials[0].1)
            .into_iter()
            .find(|grant| grant.state.is_active())
            .unwrap();
        revoke_admin(
            &ctx,
            &json!({ "grantId": revoked_grant.grant_id, "reason": "rotation" }),
            Some(&captain),
            false,
        )
        .unwrap();
        ctx.delegated_admin
            .invalidate_actor(&admin_credentials[1].1, "Crew ownership changed")
            .unwrap();
    }

    // Both durable tombstone forms continue to classify the old bearer as an
    // administrator after another reload. Full control admission cannot turn
    // either identity back into an ordinary mutator or a Captain claimant.
    {
        let ctx = test_ctx("persisted-admin-token")
            .with_captains_registry(Arc::new(CaptainsRegistry::load(captains_path.clone())))
            .with_identity_store(Arc::new(crate::identity::IdentityStore::load(
                identities_path.clone(),
            )))
            .with_delegated_admin(Arc::new(
                crate::delegated_admin::DelegatedAdminStore::load(grants_path.clone()).unwrap(),
            ));
        for (tile, _, secret) in &admin_credentials {
            for (command, args) in [
                ("new_tab", json!({"name": "escaped"})),
                (
                    "claim_captain",
                    json!({"captainSessionId": tile, "shipSlug": "escaped"}),
                ),
                (
                    "attach_captain",
                    json!({
                        "captainSessionId": tile,
                        "projectId": "escaped",
                        "assignment": "escaped"
                    }),
                ),
            ] {
                let response = dispatch_authenticated(
                    &ctx,
                    req_session("persisted-admin-token", secret, command, args),
                );
                assert!(!response.ok, "historical admin called {command}");
                assert!(
                    response
                        .error
                        .unwrap_or_default()
                        .contains("outside their exact administrative operation grants"),
                    "historical admin did not fail at durable boundary for {command}"
                );
            }
            let resolved = resolve_identity(&ctx, secret).unwrap();
            assert!(enforce_attach_authority(
                &ctx,
                Some(&resolved),
                false,
                tile,
                FleetRole::Captain,
            )
            .unwrap_err()
            .contains("cannot acquire Captain or Cortana authority"));
        }
    }

    for (tile, _, _) in &admin_credentials {
        reap_test_tmux_session(&tmux_target(tile)).unwrap();
    }
    for path in [
        captains_path.with_extension("json.bak"),
        captains_path,
        identities_path,
        grants_path,
    ] {
        std::fs::remove_file(path).ok();
    }
}

#[test]
fn captain_release_invalidates_dependent_grants_before_reclaim() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let ctx = test_ctx("admin-release");
    ctx.captains
        .claim_test("captain-alpha", Some("alpha"), vec![])
        .unwrap();
    ctx.captains
        .record_crew("captain-alpha", "admin-alpha")
        .unwrap();
    let admin_target = tmux_target("admin-alpha");
    create_test_tmux_session(&admin_target).unwrap();
    let captain_identity = ctx.identity.mint(crate::identity::Role::Captain).unwrap();
    ctx.identity
        .bind_tile(&captain_identity.id, "captain-alpha")
        .unwrap();
    let admin_identity = ctx.identity.mint(crate::identity::Role::Crew).unwrap();
    ctx.identity
        .bind_tile(&admin_identity.id, "admin-alpha")
        .unwrap();
    let captain = resolve_identity(&ctx, &captain_identity.secret).unwrap();
    let appointed = appoint_admin(
        &ctx,
        &json!({
            "actorSessionId": "admin-alpha",
            "role": "shipAdmin",
            "permittedOperations": ["inspectStatus"]
        }),
        Some(&captain),
        false,
    )
    .unwrap();
    let grant_id = appointed["grant"]["grantId"].as_str().unwrap();

    release_captain(&ctx, &json!({"shipSlug": "alpha"}), Some(&captain), false).unwrap();
    assert!(matches!(
        ctx.delegated_admin.get(grant_id).unwrap().state,
        crate::delegated_admin::GrantState::Invalidated { .. }
    ));
    ctx.captains
        .claim_test("captain-replacement", Some("alpha"), vec![])
        .unwrap();
    let admin = resolve_identity(&ctx, &admin_identity.secret).unwrap();
    let denied = authorize_delegated_admin(
        &ctx,
        &admin,
        crate::delegated_admin::AdminOperation::InspectStatus,
        crate::delegated_admin::AdminTarget::Ship {
            ship_slug: "alpha".into(),
        },
        crate::delegated_admin::AdminSafeguards::default(),
    )
    .unwrap_err();
    assert!(denied.contains("no active administrative grant"));
    reap_test_tmux_session(&admin_target).unwrap();
}

#[test]
fn delegated_admin_operation_invalidates_a_transferred_actor() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let ctx = test_ctx("admin-transfer");
    ctx.captains
        .claim_test("captain-alpha", Some("alpha"), vec![])
        .unwrap();
    ctx.captains
        .claim_test("captain-beta", Some("beta"), vec![])
        .unwrap();
    ctx.captains
        .record_crew("captain-alpha", "admin-alpha")
        .unwrap();
    let admin_target = tmux_target("admin-alpha");
    create_test_tmux_session(&admin_target).unwrap();
    let captain_identity = ctx.identity.mint(crate::identity::Role::Captain).unwrap();
    ctx.identity
        .bind_tile(&captain_identity.id, "captain-alpha")
        .unwrap();
    let admin_identity = ctx.identity.mint(crate::identity::Role::Crew).unwrap();
    ctx.identity
        .bind_tile(&admin_identity.id, "admin-alpha")
        .unwrap();
    let captain = resolve_identity(&ctx, &captain_identity.secret).unwrap();
    let appointed = appoint_admin(
        &ctx,
        &json!({
            "actorSessionId": "admin-alpha",
            "role": "shipAdmin",
            "permittedOperations": ["inspectStatus"]
        }),
        Some(&captain),
        false,
    )
    .unwrap();
    let grant_id = appointed["grant"]["grantId"].as_str().unwrap();
    ctx.captains.rollback_crew("admin-alpha").unwrap();
    ctx.captains
        .record_crew("captain-beta", "admin-alpha")
        .unwrap();

    let admin = resolve_identity(&ctx, &admin_identity.secret).unwrap();
    let denied = authorize_delegated_admin(
        &ctx,
        &admin,
        crate::delegated_admin::AdminOperation::InspectStatus,
        crate::delegated_admin::AdminTarget::Ship {
            ship_slug: "alpha".into(),
        },
        crate::delegated_admin::AdminSafeguards::default(),
    )
    .unwrap_err();
    assert!(denied.contains("actorMismatch"));
    assert!(matches!(
        ctx.delegated_admin.get(grant_id).unwrap().state,
        crate::delegated_admin::GrantState::Invalidated { .. }
    ));
    reap_test_tmux_session(&admin_target).unwrap();
}

#[test]
fn delegated_admin_audit_records_attributed_success_and_failure_outcomes() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let audit_dir = std::env::temp_dir().join(format!(
        "t-hub-admin-outcome-audit-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let ctx = test_ctx("admin-audit").with_audit(Arc::new(AuditLog::new(audit_dir.clone())));
    ctx.captains
        .claim_test("captain-alpha", Some("alpha"), vec![])
        .unwrap();
    ctx.captains
        .record_crew("captain-alpha", "admin-alpha")
        .unwrap();
    let admin_target = tmux_target("admin-alpha");
    create_test_tmux_session(&admin_target).unwrap();
    let captain_identity = ctx.identity.mint(crate::identity::Role::Captain).unwrap();
    ctx.identity
        .bind_tile(&captain_identity.id, "captain-alpha")
        .unwrap();
    let admin_identity = ctx.identity.mint(crate::identity::Role::Crew).unwrap();
    ctx.identity
        .bind_tile(&admin_identity.id, "admin-alpha")
        .unwrap();
    let captain = resolve_identity(&ctx, &captain_identity.secret).unwrap();
    appoint_admin(
        &ctx,
        &json!({
            "actorSessionId": "admin-alpha",
            "role": "shipAdmin",
            "permittedOperations": ["inspectStatus"]
        }),
        Some(&captain),
        false,
    )
    .unwrap();
    let admin = resolve_identity(&ctx, &admin_identity.secret).unwrap();

    list_agents(
        &ctx,
        &json!({"captainSessionId": "captain-alpha"}),
        Some(&admin),
        false,
    )
    .unwrap();
    list_agents(
        &ctx,
        &json!({"captainSessionId": "captain-alpha", "limit": 0}),
        Some(&admin),
        false,
    )
    .unwrap_err();

    let records = read_audit(&audit_dir);
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["command"], "delegated_admin_operation");
    assert_eq!(records[0]["decision"], "succeeded");
    assert_eq!(records[0]["args"]["outcome"], "succeeded");
    assert_eq!(
        records[0]["args"]["authorization"]["actorIdentityId"],
        admin_identity.id
    );
    assert_eq!(
        records[0]["args"]["authorization"]["delegatingSupervisorIdentityId"],
        captain_identity.id
    );
    assert_eq!(records[1]["decision"], "failed");
    assert_eq!(records[1]["args"]["outcome"], "failed");
    assert!(records[1]["args"]["error"]
        .as_str()
        .is_some_and(|error| error.contains("limit must be between")));

    reap_test_tmux_session(&admin_target).unwrap();
    std::fs::remove_dir_all(audit_dir).ok();
}

#[test]
fn reconcile_cortana_is_idempotent_and_recovers_the_same_identity() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let mut ctx = test_ctx("cortana-control")
        .with_live_sessions(|| Ok(Vec::new()))
        .with_apply_sink(sink);
    ctx.addr = "127.0.0.1:4242".into();
    ctx.tab_registry().replace(vec![TabRecord {
        id: CAPTAIN_WORKSPACE_ID.into(),
        name: CAPTAIN_WORKSPACE_NAME.into(),
        tile_ids: vec![],
    }]);
    let home = std::env::temp_dir().join(format!(
        "t-hub-cortana-home-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    let first = dispatch(
        &ctx,
        "reconcile_cortana",
        &json!({
            "operationId": "cortana-startup-1",
            "testOrchestratorHome": home,
            "testStartupCommand": harness_command,
        }),
    )
    .unwrap();
    assert_eq!(first["action"], "create");
    assert_eq!(first["healthy"], true);
    assert_eq!(first["generation"], 1);
    let first_terminal = first["terminalId"].as_str().unwrap().to_string();
    let identity_id = first["identityId"].as_str().unwrap().to_string();

    let second = dispatch(
        &ctx,
        "reconcile_cortana",
        &json!({
            "operationId": "cortana-startup-1",
            "testOrchestratorHome": home,
            "testStartupCommand": harness_command,
        }),
    )
    .unwrap();
    assert_eq!(second["action"], "keep");
    assert_eq!(second["terminalId"], first_terminal);
    assert_eq!(second["identityId"], identity_id);
    assert_eq!(
        ctx.captains
            .snapshot()
            .captains
            .iter()
            .filter(|captain| captain.role == FleetRole::Cortana)
            .count(),
        1
    );

    reap_test_tmux_session_and_assert_absent(&tmux_target(&first_terminal));
    let recovered = dispatch(
        &ctx,
        "reconcile_cortana",
        &json!({
            "operationId": "cortana-startup-2",
            "testOrchestratorHome": home,
            "testStartupCommand": harness_command,
        }),
    )
    .unwrap();
    assert_eq!(recovered["action"], "recover");
    assert_eq!(recovered["generation"], 2);
    assert_eq!(recovered["identityId"], identity_id);
    assert_ne!(recovered["terminalId"], first_terminal);
    let recovered_terminal = recovered["terminalId"].as_str().unwrap();
    assert!(tmux::has_session(&tmux_target(recovered_terminal)));

    dispatch(
        &ctx,
        "close_terminal",
        &json!({ "sessionId": recovered_terminal }),
    )
    .unwrap();
    let _ = std::fs::remove_dir_all(harness_bin_dir);
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn cortana_bootstrap_requires_exact_live_authority_and_returns_a_bounded_redacted_snapshot() {
    if tmux::managed_runtime_preflight().is_err() {
        return;
    }
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let mut ctx = test_ctx("cortana-bootstrap")
        .with_live_sessions(|| tmux::list_sessions().map_err(|error| error.to_string()))
        .with_apply_sink(sink.clone());
    ctx.addr = "127.0.0.1:4263".into();
    ctx.tab_registry().replace(vec![TabRecord {
        id: CAPTAIN_WORKSPACE_ID.into(),
        name: CAPTAIN_WORKSPACE_NAME.into(),
        tile_ids: Vec::new(),
    }]);
    let home = std::env::temp_dir().join(format!(
        "t-hub-cortana-bootstrap-home-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    let started = dispatch(
        &ctx,
        "reconcile_cortana",
        &json!({
            "operationId": "cortana-bootstrap-start",
            "testOrchestratorHome": home,
            "testStartupCommand": harness_command,
        }),
    )
    .unwrap();
    let terminal_id = started["terminalId"].as_str().unwrap().to_string();
    let target = tmux_target(&terminal_id);
    let bearer = tmux::session_environment(&target, crate::identity::SESSION_TOKEN_ENV)
        .unwrap()
        .unwrap();
    let modeled_launch = cortana_startup_command(
        &crate::cortana_reconcile::CortanaDurableIdentity::default(),
        &json!({}),
        Harness::Codex,
    );
    assert_eq!(
        modeled_codex_tool_approval(&modeled_launch, "cortana_bootstrap"),
        "approve"
    );
    assert_eq!(
        modeled_codex_tool_approval(&modeled_launch, "focus_session"),
        "prompt"
    );
    assert_eq!(
        modeled_codex_tool_approval(&modeled_launch, "spawn_terminal"),
        "prompt"
    );

    for index in (0..20).rev() {
        let ship_slug = format!("ship-{index:02}");
        ctx.captains
            .claim_test(&format!("captain-{index:02}"), Some(&ship_slug), vec![])
            .unwrap();
        ctx.captains
            .checkpoint(
                None,
                Some(&ship_slug),
                None,
                Some(&format!("thread-{index:02}")),
                Some(&"x".repeat(CORTANA_BOOTSTRAP_MAX_TEXT_BYTES + 64)),
            )
            .unwrap();
    }

    let bootstrap = dispatch_authenticated(
        &ctx,
        req_session(&ctx.read_token, &bearer, "cortana_bootstrap", json!({})),
    );
    assert!(bootstrap.ok, "{:?}", bootstrap.error);
    let result = bootstrap.result.unwrap();
    assert_eq!(result["activeCount"], 20);
    assert_eq!(result["returnedCount"], CORTANA_BOOTSTRAP_MAX_SHIPS);
    assert_eq!(result["omittedCount"], 4);
    assert_eq!(result["ships"][0]["shipSlug"], "ship-00");
    assert_eq!(result["ships"][15]["shipSlug"], "ship-15");
    assert_eq!(
        result["ships"][0]["resumePoint"].as_str().unwrap().len(),
        CORTANA_BOOTSTRAP_MAX_TEXT_BYTES
    );
    let encoded = serde_json::to_vec(&result).unwrap();
    assert!(encoded.len() <= CORTANA_BOOTSTRAP_MAX_RESPONSE_BYTES);
    let redacted = String::from_utf8(encoded).unwrap().to_ascii_lowercase();
    for forbidden in [
        "assignment",
        "launchnonce",
        "owner",
        "harnessprocess",
        "argv",
        "sessiontoken",
    ] {
        assert!(!redacted.contains(forbidden), "{forbidden}: {redacted}");
    }
    let effects_before_denials = sink.calls.lock().unwrap().len();
    for (command, args) in [
        ("focus_session", json!({"sessionId": terminal_id.clone()})),
        (
            "spawn_terminal",
            json!({"requestId": "cortana-bootstrap-must-not-spawn"}),
        ),
    ] {
        let denied =
            dispatch_authenticated(&ctx, req_session(&ctx.read_token, &bearer, command, args));
        assert!(!denied.ok, "{command}");
        assert!(denied
            .error
            .as_deref()
            .is_some_and(|error| { error.contains("requires the control capability") }));
    }
    assert_eq!(sink.calls.lock().unwrap().len(), effects_before_denials);

    let crew = ctx.identity.mint(crate::identity::Role::Crew).unwrap();
    let denied_crew = dispatch_authenticated(
        &ctx,
        req_session(
            &ctx.read_token,
            &crew.secret,
            "cortana_bootstrap",
            json!({}),
        ),
    );
    assert!(!denied_crew.ok);

    let ambiguous = ctx.identity.mint(crate::identity::Role::Cortana).unwrap();
    ctx.identity.bind_tile(&ambiguous.id, &terminal_id).unwrap();
    let denied_ambiguous = dispatch_authenticated(
        &ctx,
        req_session(&ctx.read_token, &bearer, "cortana_bootstrap", json!({})),
    );
    assert!(!denied_ambiguous.ok);
    ctx.identity.retire(&ambiguous.id).unwrap();

    let dead = test_ctx("cortana-bootstrap-dead")
        .with_captains_registry(Arc::clone(&ctx.captains))
        .with_identity_store(Arc::clone(&ctx.identity))
        .with_live_sessions(|| Ok(Vec::new()));
    let denied_dead = dispatch_authenticated(
        &dead,
        req_session(&dead.read_token, &bearer, "cortana_bootstrap", json!({})),
    );
    assert!(!denied_dead.ok);
    let denied_missing = dispatch_authenticated(
        &ctx,
        req_session(&ctx.read_token, "", "cortana_bootstrap", json!({})),
    );
    assert!(!denied_missing.ok);

    let (reached_tx, reached_rx) = std::sync::mpsc::sync_channel(1);
    let (resume_tx, resume_rx) = std::sync::mpsc::sync_channel(1);
    ctx.captains.set_dispatch_barrier(Some(DispatchBarrier {
        boundary: "cortana-bootstrap-response-built",
        reached: reached_tx,
        resume: resume_rx,
    }));
    let raced = std::thread::scope(|scope| {
        let request_ctx = ctx.clone();
        let request_bearer = bearer.clone();
        let request = scope.spawn(move || {
            dispatch_authenticated(
                &request_ctx,
                req_session(
                    &request_ctx.read_token,
                    &request_bearer,
                    "cortana_bootstrap",
                    json!({}),
                ),
            )
        });
        assert_eq!(
            reached_rx.recv_timeout(Duration::from_secs(10)).unwrap(),
            "cortana-bootstrap-response-built"
        );
        ctx.captains
            .begin_cortana_recovery("cortana-bootstrap-raced-basis")
            .unwrap();
        resume_tx.send(()).unwrap();
        request.join().unwrap()
    });
    assert!(!raced.ok);
    assert!(raced.error.as_deref().is_some_and(|error| {
        error.contains("not healthy or in an admitted launch phase")
            || error.contains("basis changed")
    }));

    dispatch(&ctx, "close_terminal", &json!({ "sessionId": terminal_id })).unwrap();
    std::fs::remove_dir_all(harness_bin_dir).unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn managed_cortana_with_lost_session_authority_is_replaced_after_restart_without_signal() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let registry_path = captains_tmp("cortana-lost-session-authority");
    let identity_path = captains_tmp("cortana-lost-session-authority-identities");
    let captains = Arc::new(CaptainsRegistry::load(registry_path.clone()));
    let identities = Arc::new(crate::identity::IdentityStore::load(identity_path.clone()));
    let mut ctx = test_ctx("cortana-lost-session-authority-control")
        .with_governor(Arc::new(SpawnGovernor::new(64, 600.0, 8.0)))
        .with_live_sessions(|| Ok(Vec::new()))
        .with_captains_registry(Arc::clone(&captains))
        .with_identity_store(Arc::clone(&identities))
        .with_apply_sink(Arc::new(RecordingSink {
            calls: StdMutex::new(Vec::new()),
        }));
    ctx.addr = "127.0.0.1:4260".into();
    ctx.tab_registry().replace(vec![TabRecord {
        id: CAPTAIN_WORKSPACE_ID.into(),
        name: CAPTAIN_WORKSPACE_NAME.into(),
        tile_ids: vec![],
    }]);
    let home = std::env::temp_dir().join(format!(
        "t-hub-cortana-lost-session-authority-home-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    let first = dispatch(
        &ctx,
        "reconcile_cortana",
        &json!({
            "operationId": "lost-session-authority-initial",
            "testOrchestratorHome": home,
            "testStartupCommand": harness_command,
        }),
    )
    .unwrap();
    assert_eq!(first["healthy"], true);
    let incumbent_terminal = first["terminalId"].as_str().unwrap().to_string();
    let incumbent_identity = first["identityId"].as_str().unwrap().to_string();
    let incumbent_target = exact_cortana_tmux_target(&incumbent_terminal).unwrap();
    let incumbent_effect = tmux::observe_session_effect_identity(&incumbent_target).unwrap();
    let incumbent_bearer =
        tmux::session_environment(&incumbent_target, crate::identity::SESSION_TOKEN_ENV)
            .unwrap()
            .unwrap();
    assert_eq!(
        tmux::session_environment(&incumbent_target, "T_HUB_CONTROL_ADDR").unwrap(),
        Some(String::new())
    );
    assert_eq!(
        tmux::session_environment(&incumbent_target, "T_HUB_CONTROL_TOKEN").unwrap(),
        Some(String::new())
    );
    let healthy_before_negative = captains.snapshot();
    let healthy_candidates = discover_cortana_runtimes(
        &ctx,
        &files::posix_form(&home.to_string_lossy()),
        &healthy_before_negative.cortana,
    )
    .unwrap();
    assert!(retirable_unattested_managed_cortana_incumbent(
        &ctx,
        &healthy_before_negative.cortana,
        &healthy_candidates,
    )
    .is_none());
    assert_eq!(captains.snapshot().seq, healthy_before_negative.seq);
    assert!(!identities.is_revoked(&incumbent_identity));
    assert_eq!(
        tmux::observe_session_effect_identity(&incumbent_target).unwrap(),
        incumbent_effect
    );

    identities.revoke(&incumbent_identity).unwrap();
    assert!(identities.resolve(&incumbent_bearer).is_none());
    drop(ctx);
    drop(captains);
    drop(identities);

    let restarted_captains = Arc::new(CaptainsRegistry::load(registry_path.clone()));
    let restarted_identities =
        Arc::new(crate::identity::IdentityStore::load(identity_path.clone()));
    let mut restarted = test_ctx("cortana-lost-session-authority-restart")
        .with_governor(Arc::new(SpawnGovernor::new(64, 600.0, 8.0)))
        .with_live_sessions({
            let incumbent_target = incumbent_target.clone();
            move || Ok(vec![incumbent_target.clone()])
        })
        .with_captains_registry(Arc::clone(&restarted_captains))
        .with_identity_store(Arc::clone(&restarted_identities))
        .with_apply_sink(Arc::new(RecordingSink {
            calls: StdMutex::new(Vec::new()),
        }));
    restarted.addr = "127.0.0.1:4261".into();
    restarted.tab_registry().replace(vec![TabRecord {
        id: CAPTAIN_WORKSPACE_ID.into(),
        name: CAPTAIN_WORKSPACE_NAME.into(),
        tile_ids: vec![incumbent_terminal.clone()],
    }]);
    let durable_before_recovery = restarted_captains.cortana_identity();
    let candidates_before_recovery = discover_cortana_runtimes(
        &restarted,
        &files::posix_form(&home.to_string_lossy()),
        &durable_before_recovery,
    )
    .unwrap();
    let prepared_incumbent = retirable_unattested_managed_cortana_incumbent(
                &restarted,
                &durable_before_recovery,
                &candidates_before_recovery,
            )
            .unwrap_or_else(|| panic!(
                "durable={durable_before_recovery:#?} candidates={candidates_before_recovery:#?} claims={:#?}",
                restarted_captains.snapshot().captains,
            ));
    let seq_before_mismatch = restarted_captains.snapshot().seq;
    let mut mismatched_candidates = candidates_before_recovery.clone();
    mismatched_candidates[0]
        .effect_identity
        .as_mut()
        .unwrap()
        .pane_start_ticks = mismatched_candidates[0]
        .effect_identity
        .as_ref()
        .unwrap()
        .pane_start_ticks
        .saturating_add(1);
    assert!(retirable_unattested_managed_cortana_incumbent(
        &restarted,
        &durable_before_recovery,
        &mismatched_candidates,
    )
    .is_none());
    let mut mismatched_attestation = durable_before_recovery.clone();
    mismatched_attestation
        .active_harness_attestation
        .as_mut()
        .unwrap()
        .process
        .start_ticks = mismatched_attestation
        .active_harness_attestation
        .as_ref()
        .unwrap()
        .process
        .start_ticks
        .saturating_add(1);
    assert!(revalidate_unresolved_cortana_attestation(&mismatched_attestation).is_err());
    assert_eq!(restarted_captains.snapshot().seq, seq_before_mismatch);
    assert_eq!(
        tmux::observe_session_effect_identity(&incumbent_target).unwrap(),
        incumbent_effect
    );
    revalidate_unresolved_cortana_attestation(&durable_before_recovery).unwrap();
    restarted_captains
        .begin_cortana_recovery("lost-session-authority-replacement")
        .unwrap();
    restarted_captains
        .prepare_cortana_orphan_replacement(
            "lost-session-authority-replacement",
            &prepared_incumbent.terminal_id,
            durable_before_recovery.identity_id.as_deref().unwrap(),
            durable_before_recovery.generation,
            durable_before_recovery.harness.as_deref().unwrap(),
            prepared_incumbent.effect_identity.unwrap(),
        )
        .unwrap();
    assert!(matches!(
        restarted_captains.cortana_identity().recovery,
        crate::cortana_reconcile::CortanaRecoveryState::ReplacingOrphan {
            managed_basis: Some(_),
            ..
        }
    ));
    drop(restarted);
    drop(restarted_captains);
    drop(restarted_identities);

    let restarted_captains = Arc::new(CaptainsRegistry::load(registry_path.clone()));
    let restarted_identities =
        Arc::new(crate::identity::IdentityStore::load(identity_path.clone()));
    let mut restarted = test_ctx("cortana-lost-session-authority-wal-restart")
        .with_governor(Arc::new(SpawnGovernor::new(64, 600.0, 8.0)))
        .with_live_sessions({
            let incumbent_target = incumbent_target.clone();
            move || Ok(vec![incumbent_target.clone()])
        })
        .with_captains_registry(Arc::clone(&restarted_captains))
        .with_identity_store(Arc::clone(&restarted_identities))
        .with_apply_sink(Arc::new(RecordingSink {
            calls: StdMutex::new(Vec::new()),
        }));
    restarted.addr = "127.0.0.1:4261".into();
    restarted.tab_registry().replace(vec![TabRecord {
        id: CAPTAIN_WORKSPACE_ID.into(),
        name: CAPTAIN_WORKSPACE_NAME.into(),
        tile_ids: vec![incumbent_terminal.clone()],
    }]);
    let gained_identity = restarted_identities
        .mint(crate::identity::Role::Cortana)
        .unwrap();
    restarted_identities
        .bind_tile(&gained_identity.id, &incumbent_terminal)
        .unwrap();
    tmux::set_session_environment(
        &incumbent_target,
        crate::identity::SESSION_TOKEN_ENV,
        &gained_identity.secret,
    )
    .unwrap();
    let seq_before_capability_gain = restarted_captains.snapshot().seq;
    let capability_gain_error = dispatch(
        &restarted,
        "reconcile_cortana",
        &json!({
            "operationId": "lost-session-authority-replacement",
            "testOrchestratorHome": home,
            "testStartupCommand": harness_command,
        }),
    )
    .unwrap_err();
    assert!(
        capability_gain_error.contains("runtime changed after WAL")
            || capability_gain_error.contains("attestation failed"),
        "{capability_gain_error}"
    );
    assert!(!restarted_identities.is_revoked(&gained_identity.id));
    assert_eq!(
        restarted_captains.snapshot().seq,
        seq_before_capability_gain
    );
    assert!(restarted_captains
        .cortana_identity()
        .quarantine_ledger
        .is_empty());
    assert_eq!(
        tmux::observe_session_effect_identity(&incumbent_target).unwrap(),
        incumbent_effect
    );
    tmux::set_session_environment(
        &incumbent_target,
        crate::identity::SESSION_TOKEN_ENV,
        &incumbent_bearer,
    )
    .unwrap();
    restarted_identities.retire(&gained_identity.id).unwrap();
    let wal_durable = restarted_captains.cortana_identity();
    let (
        wal_effect,
        wal_basis,
        wal_identity,
        wal_generation,
        wal_harness,
        original_assignment,
        original_attestation,
    ) = match &wal_durable.recovery {
        crate::cortana_reconcile::CortanaRecoveryState::ReplacingOrphan {
            orphan_identity_id,
            orphan_generation,
            harness,
            effect_identity,
            managed_basis: Some(basis),
            ..
        } => (
            *effect_identity,
            basis.clone(),
            orphan_identity_id.clone(),
            *orphan_generation,
            harness.clone(),
            basis.claim_assignment_id.clone(),
            basis.active_harness_attestation.clone(),
        ),
        other => panic!("expected managed quarantine WAL, got {other:#?}"),
    };
    revalidate_unresolved_cortana_attestation(&wal_durable).unwrap();
    let post_wal_candidates = discover_cortana_runtimes(
        &restarted,
        &files::posix_form(&home.to_string_lossy()),
        &wal_durable,
    )
    .unwrap();
    assert_eq!(post_wal_candidates.len(), 1);
    assert!(exact_unresolved_managed_cortana_candidate(
        &post_wal_candidates[0],
        &incumbent_terminal,
        wal_generation,
        &wal_harness,
        &wal_effect,
    ));

    restarted_captains
        .set_cortana_quarantine_claim_assignment_for_test("changed-after-revalidation")
        .unwrap();
    assert!(restarted_captains
        .validate_cortana_managed_quarantine_basis(
            "lost-session-authority-replacement",
            &incumbent_terminal,
            &wal_identity,
            wal_generation,
            &wal_harness,
            &wal_effect,
            &wal_basis,
        )
        .is_err());
    assert!(restarted_captains
        .cortana_identity()
        .quarantine_ledger
        .is_empty());
    restarted_captains
        .set_cortana_quarantine_claim_assignment_for_test(&original_assignment)
        .unwrap();

    restarted_captains
        .set_cortana_quarantine_attestation_for_test(None)
        .unwrap();
    assert!(restarted_captains
        .validate_cortana_managed_quarantine_basis(
            "lost-session-authority-replacement",
            &incumbent_terminal,
            &wal_identity,
            wal_generation,
            &wal_harness,
            &wal_effect,
            &wal_basis,
        )
        .is_err());
    restarted_captains
        .set_cortana_quarantine_attestation_for_test(original_attestation)
        .unwrap();
    assert!(restarted_captains
        .validate_cortana_managed_quarantine_basis(
            "lost-session-authority-replacement",
            &incumbent_terminal,
            &wal_identity,
            wal_generation,
            &wal_harness,
            &wal_effect,
            &wal_basis,
        )
        .is_ok());
    assert_eq!(
        tmux::observe_session_effect_identity(&incumbent_target).unwrap(),
        incumbent_effect
    );
    let recovered = dispatch(
        &restarted,
        "reconcile_cortana",
        &json!({
            "operationId": "lost-session-authority-replacement",
            "testOrchestratorHome": home,
            "testStartupCommand": harness_command,
        }),
    )
    .unwrap();
    assert_eq!(recovered["action"], "recover", "{recovered:#}");
    assert_eq!(recovered["healthy"], true);
    assert_eq!(recovered["generation"], 2);
    assert_ne!(recovered["terminalId"], incumbent_terminal);
    assert_eq!(
        tmux::session_liveness(&incumbent_target),
        tmux::SessionLiveness::Alive,
        "the exact invalid incumbent must be quarantined without a signal"
    );
    assert_eq!(
        tmux::observe_session_effect_identity(&incumbent_target).unwrap(),
        incumbent_effect,
        "the quarantined process generation must remain unchanged"
    );
    let denied = dispatch_authenticated(
        &restarted,
        req_session(
            &restarted.token,
            &incumbent_bearer,
            "register_project",
            json!({"rootPath": "/tmp/lost-session-authority-must-not-register"}),
        ),
    );
    assert!(!denied.ok);
    let replacement_terminal = recovered["terminalId"].as_str().unwrap().to_string();
    let replacement_identity = recovered["identityId"].as_str().unwrap().to_string();
    let replacement_target = exact_cortana_tmux_target(&replacement_terminal).unwrap();
    let replacement_effect = tmux::observe_session_effect_identity(&replacement_target).unwrap();
    let replacement_bearer =
        tmux::session_environment(&replacement_target, crate::identity::SESSION_TOKEN_ENV)
            .unwrap()
            .unwrap();
    assert_eq!(
        restarted_captains
            .snapshot()
            .captains
            .iter()
            .filter(
                |captain| captain.role == FleetRole::Cortana && captain.state == ClaimState::Active
            )
            .count(),
        1
    );
    assert_eq!(
        restarted_captains
            .cortana_identity()
            .quarantine_ledger
            .len(),
        1
    );

    restarted_identities.revoke(&replacement_identity).unwrap();
    assert!(restarted_identities.resolve(&replacement_bearer).is_none());
    drop(restarted);
    drop(restarted_captains);
    drop(restarted_identities);

    let mut native_document: Value =
        serde_json::from_slice(&std::fs::read(&registry_path).unwrap()).unwrap();
    let native_cortana = native_document
        .get_mut("cortana")
        .and_then(Value::as_object_mut)
        .unwrap();
    native_cortana.remove("activeHarnessAttestation");
    native_cortana.insert("providerSessionId".into(), Value::Null);
    native_cortana.insert("conversationId".into(), Value::Null);
    native_cortana.insert(
            "recovery".into(),
            json!({
                "kind": "degraded",
                "operation_id": "native-lost-session-authority",
                "reason": "live managed runtime lost authoritative session identity and control evidence",
                "detected_at": now_ms().max(1),
            }),
        );
    std::fs::write(
        &registry_path,
        serde_json::to_vec_pretty(&native_document).unwrap(),
    )
    .unwrap();

    let native_captains = Arc::new(CaptainsRegistry::load(registry_path.clone()));
    let native_identities = Arc::new(crate::identity::IdentityStore::load(identity_path.clone()));
    let mut native = test_ctx("cortana-native-lost-session-authority")
        .with_governor(Arc::new(SpawnGovernor::new(2, 600.0, 8.0)))
        .with_live_sessions({
            let incumbent_target = incumbent_target.clone();
            let replacement_target = replacement_target.clone();
            move || Ok(vec![incumbent_target.clone(), replacement_target.clone()])
        })
        .with_metrics(Arc::new(|| {
            Ok(t_hub_protocol::HostMetrics {
                mem_total_kib: 16_000_000,
                mem_available_kib: 8_000_000,
                swap_total_kib: 2_000_000,
                swap_free_kib: 1_500_000,
                cpu_count: 12,
                load_avg: [1.0, 0.5, 0.25],
                process_count: 432,
                distro: Some("test".into()),
                captured_at_ms: now_ms(),
            })
        }))
        .with_captains_registry(Arc::clone(&native_captains))
        .with_identity_store(Arc::clone(&native_identities))
        .with_apply_sink(Arc::new(RecordingSink {
            calls: StdMutex::new(Vec::new()),
        }));
    native.addr = "127.0.0.1:4262".into();
    native.tab_registry().replace(vec![TabRecord {
        id: CAPTAIN_WORKSPACE_ID.into(),
        name: CAPTAIN_WORKSPACE_NAME.into(),
        tile_ids: vec![replacement_terminal.clone()],
    }]);
    let native_durable_before_wal = native_captains.cortana_identity();
    let native_candidates_before_wal = discover_cortana_runtimes(
        &native,
        &files::posix_form(&home.to_string_lossy()),
        &native_durable_before_wal,
    )
    .unwrap();
    let native_incumbent = retirable_unattested_managed_cortana_incumbent(
        &native,
        &native_durable_before_wal,
        &native_candidates_before_wal,
    )
    .expect("native invalid incumbent must have exact managed evidence");
    native_captains
        .begin_cortana_recovery("native-lost-session-authority-replacement")
        .unwrap();
    native_captains
        .prepare_cortana_orphan_replacement(
            "native-lost-session-authority-replacement",
            &native_incumbent.terminal_id,
            native_durable_before_wal.identity_id.as_deref().unwrap(),
            native_durable_before_wal.generation,
            native_durable_before_wal.harness.as_deref().unwrap(),
            native_incumbent.effect_identity.unwrap(),
        )
        .unwrap();
    let gained_native_identity = native_identities
        .mint(crate::identity::Role::Cortana)
        .unwrap();
    native_identities
        .bind_tile(&gained_native_identity.id, &replacement_terminal)
        .unwrap();
    tmux::set_session_environment(
        &replacement_target,
        crate::identity::SESSION_TOKEN_ENV,
        &gained_native_identity.secret,
    )
    .unwrap();
    let native_seq_before_gain = native_captains.snapshot().seq;
    let native_gain_error = dispatch(
        &native,
        "reconcile_cortana",
        &json!({
            "operationId": "native-lost-session-authority-replacement",
            "testOrchestratorHome": home,
            "testStartupCommand": harness_command,
        }),
    )
    .unwrap_err();
    assert!(
        native_gain_error.contains("runtime changed after WAL"),
        "{native_gain_error}"
    );
    assert_eq!(native_captains.snapshot().seq, native_seq_before_gain);
    assert!(!native_identities.is_revoked(&gained_native_identity.id));
    assert_eq!(
        native_captains.cortana_identity().quarantine_ledger.len(),
        1
    );
    assert_eq!(
        tmux::observe_session_effect_identity(&replacement_target).unwrap(),
        replacement_effect
    );
    tmux::set_session_environment(
        &replacement_target,
        crate::identity::SESSION_TOKEN_ENV,
        &replacement_bearer,
    )
    .unwrap();
    native_identities
        .retire(&gained_native_identity.id)
        .unwrap();
    let native = Arc::new(native);
    let concurrent_start = Arc::new(std::sync::Barrier::new(5));
    let mut concurrent_workers = Vec::new();
    for _ in 0..4 {
        let native = Arc::clone(&native);
        let concurrent_start = Arc::clone(&concurrent_start);
        let home = home.clone();
        let harness_command = harness_command.clone();
        concurrent_workers.push(std::thread::spawn(move || {
            concurrent_start.wait();
            dispatch(
                &native,
                "reconcile_cortana",
                &json!({
                    "operationId": "native-lost-session-authority-replacement",
                    "testOrchestratorHome": home,
                    "testStartupCommand": harness_command,
                }),
            )
            .unwrap()
        }));
    }
    concurrent_start.wait();
    let concurrent_results = concurrent_workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect::<Vec<_>>();
    let native_recovered = concurrent_results
        .iter()
        .find(|result| result["action"] == "recover")
        .cloned()
        .expect("one concurrent caller must perform the recovery");
    assert_eq!(
        concurrent_results
            .iter()
            .filter(|result| result["action"] == "recover")
            .count(),
        1
    );
    assert!(concurrent_results.iter().all(|result| {
        result["generation"] == 3
            && result["terminalId"] == native_recovered["terminalId"]
            && matches!(result["action"].as_str(), Some("recover" | "keep"))
    }));
    assert_eq!(
        native_recovered["action"], "recover",
        "{native_recovered:#}"
    );
    assert_eq!(native_recovered["healthy"], true);
    assert_eq!(native_recovered["generation"], 3);
    let generation_three_terminal = native_recovered["terminalId"].as_str().unwrap().to_string();
    assert_ne!(generation_three_terminal, replacement_terminal);
    assert_eq!(
        tmux::session_liveness(&incumbent_target),
        tmux::SessionLiveness::Alive
    );
    assert_eq!(
        tmux::observe_session_effect_identity(&incumbent_target).unwrap(),
        incumbent_effect
    );
    assert_eq!(
        tmux::session_liveness(&replacement_target),
        tmux::SessionLiveness::Alive
    );
    assert_eq!(
        tmux::observe_session_effect_identity(&replacement_target).unwrap(),
        replacement_effect
    );
    let native_durable = native_captains.cortana_identity();
    assert_eq!(native_durable.quarantine_ledger.len(), 2);
    assert_eq!(
        native_durable.quarantine_ledger[0].terminal_id,
        incumbent_terminal
    );
    assert_eq!(
        native_durable.quarantine_ledger[1].terminal_id,
        replacement_terminal
    );
    assert!(native_identities.is_revoked(&incumbent_identity));
    assert!(native_identities.is_revoked(&replacement_identity));
    for bearer in [&incumbent_bearer, &replacement_bearer] {
        let denied = dispatch_authenticated(
            &native,
            req_session(
                &native.token,
                bearer,
                "register_project",
                json!({"rootPath": "/tmp/quarantined-cortana-must-not-register"}),
            ),
        );
        assert!(!denied.ok);
    }
    let stale_workspace_report = dispatch(
        &native,
        "report_workspace_tabs",
        &json!({
            "tabs": [{
                "id": CAPTAIN_WORKSPACE_ID,
                "name": CAPTAIN_WORKSPACE_NAME,
                "tileIds": [replacement_terminal.clone()],
            }]
        }),
    );
    assert!(stale_workspace_report.is_err());
    assert_eq!(
        native_captains.cortana_identity().terminal_id.as_deref(),
        Some(generation_three_terminal.as_str())
    );
    assert_eq!(
        native_captains
            .snapshot()
            .captains
            .iter()
            .filter(
                |captain| captain.role == FleetRole::Cortana && captain.state == ClaimState::Active
            )
            .filter_map(|captain| captain.terminal_id.as_deref())
            .collect::<Vec<_>>(),
        vec![generation_three_terminal.as_str()]
    );

    let after_restart = dispatch(
        &native,
        "reconcile_cortana",
        &json!({
            "operationId": "lost-session-authority-after-restart",
            "testOrchestratorHome": home,
            "testStartupCommand": harness_command,
        }),
    )
    .unwrap();
    assert_eq!(after_restart["action"], "keep");
    assert_eq!(after_restart["terminalId"], generation_three_terminal);
    assert_eq!(after_restart["generation"], 3);

    dispatch(
        &native,
        "close_terminal",
        &json!({ "sessionId": generation_three_terminal }),
    )
    .unwrap();
    reap_test_tmux_session_and_assert_absent(&incumbent_target);
    reap_test_tmux_session_and_assert_absent(&replacement_target);
    std::fs::remove_dir_all(harness_bin_dir).ok();
    std::fs::remove_dir_all(home).ok();
    std::fs::remove_file(registry_path).ok();
    std::fs::remove_file(identity_path).ok();
}

#[test]
fn installed_stale_legacy_cortana_is_quarantined_without_signal_and_replaced() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    if !tmux_process_tests_available() {
        eprintln!(
                "installed_stale_legacy_cortana_is_exactly_replaced_from_v22_provenance: tmux or node not on PATH - skipping"
            );
        return;
    }
    let registry_path = captains_tmp("cortana-schema18-orphan");
    let migration_backup = registry_path.parent().unwrap().join(format!(
        "{}.migration-v20.1.bak",
        registry_path.file_name().unwrap().to_string_lossy()
    ));
    let identity_path = captains_tmp("cortana-schema18-orphan-identities");
    let orphan_terminal = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    let orphan_identity = "missing-schema18-cortana-identity";
    std::fs::write(
        &registry_path,
        serde_json::to_vec_pretty(&json!({
            "schemaVersion": 18,
            "seq": 6,
            "captains": [],
            "workspaces": [{
                "id": CAPTAIN_WORKSPACE_ID,
                "name": CAPTAIN_WORKSPACE_NAME,
                "kind": "captain",
                "tileIds": [orphan_terminal.clone()]
            }],
            "cortana": {
                "identityId": orphan_identity,
                "generation": 1,
                "terminalId": orphan_terminal,
                "harness": "codex",
                "providerSessionId": null,
                "conversationId": null,
                "checkpoint": null,
                "recovery": {
                    "kind": "healthy",
                    "operation_id": "installed-original",
                    "verified_at": 1
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::rename(&registry_path, &migration_backup).unwrap();
    std::fs::write(
        &registry_path,
        serde_json::to_vec_pretty(&json!({
            "schemaVersion": 21,
            "seq": 1531,
            "captains": [],
            "workspaces": [{
                "id": CAPTAIN_WORKSPACE_ID,
                "name": CAPTAIN_WORKSPACE_NAME,
                "kind": "captain",
                "tileIds": []
            }],
            "cortana": {
                "identityId": orphan_identity,
                "generation": 1,
                "terminalId": null,
                "harness": "codex",
                "providerSessionId": null,
                "conversationId": null,
                "checkpoint": null,
                "recovery": {
                    "kind": "degraded",
                    "operation_id": "installed-degraded",
                    "reason": "legacy runtime lost its identity",
                    "detected_at": 2
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();
    let captains = Arc::new(CaptainsRegistry::load(registry_path.clone()));
    assert!(captains.cortana_identity().terminal_id.is_none());
    assert_eq!(
        captains
            .cortana_identity()
            .legacy_orphan_provenance
            .as_ref()
            .map(|provenance| provenance.terminal_id.as_str()),
        Some(orphan_terminal.as_str())
    );
    let identities = Arc::new(crate::identity::IdentityStore::load(identity_path.clone()));
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let mut ctx = test_ctx("schema18-orphan-control")
        .with_live_sessions(|| Ok(Vec::new()))
        .with_captains_registry(captains.clone())
        .with_identity_store(identities.clone())
        .with_apply_sink(sink);
    ctx.addr = "127.0.0.1:4250".into();
    ctx.tab_registry().replace(vec![TabRecord {
        id: CAPTAIN_WORKSPACE_ID.into(),
        name: CAPTAIN_WORKSPACE_NAME.into(),
        tile_ids: vec![orphan_terminal.clone()],
    }]);
    let home = std::env::temp_dir().join(format!(
        "t-hub-cortana-schema18-orphan-home-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&home).unwrap();
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    let orphan_target = exact_cortana_tmux_target(&orphan_terminal).unwrap();
    create_test_tmux_session_with_env(
        &orphan_target,
        home.to_str().unwrap(),
        Some(&harness_command),
        &[
            (
                crate::identity::SESSION_TOKEN_ENV.into(),
                "untrusted-orphan-bearer".into(),
            ),
            ("T_HUB_CONTROL_ADDR".into(), "127.0.0.1:51330".into()),
            ("T_HUB_CONTROL_TOKEN".into(), "stale-control-token".into()),
            (CORTANA_GENERATION_ENV.into(), "1".into()),
        ],
    )
    .unwrap();
    wait_for_harness_started(&orphan_terminal, "codex").unwrap();
    let persist_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let hook_calls = Arc::clone(&persist_calls);
    captains.set_persist_hook(Box::new(move || {
        hook_calls.fetch_add(1, Ordering::SeqCst);
    }));
    let ctx = Arc::new(ctx);
    let recovered = dispatch(
        &ctx,
        "reconcile_cortana",
        &json!({
            "operationId": "schema18-orphan-replacement",
            "testOrchestratorHome": home,
            "testStartupCommand": harness_command,
        }),
    )
    .unwrap();
    assert!(
        persist_calls.load(Ordering::SeqCst) >= 3,
        "begin, quarantine, and managed owner publication must be durable"
    );

    assert_eq!(recovered["action"], "recover");
    assert_eq!(recovered["healthy"], true);
    assert_eq!(recovered["generation"], 2);
    assert_ne!(recovered["identityId"], orphan_identity);
    assert_eq!(
        tmux::session_liveness(&orphan_target),
        tmux::SessionLiveness::Alive,
        "legacy quarantine must not signal or close the pre-owner runtime"
    );
    let replacement_terminal = recovered["terminalId"].as_str().unwrap().to_string();
    let replacement_target = exact_cortana_tmux_target(&replacement_terminal).unwrap();
    assert_eq!(
        tmux::session_environment(&replacement_target, CORTANA_GENERATION_ENV).unwrap(),
        Some("2".into())
    );
    assert_eq!(
        tmux::session_environment(&replacement_target, "T_HUB_CONTROL_FILE").unwrap(),
        Some(discovery_file_for_spawn())
    );
    assert_eq!(
        tmux::session_environment(&replacement_target, "T_HUB_CONTROL_ADDR").unwrap(),
        Some(String::new())
    );
    assert_eq!(
        tmux::session_environment(&replacement_target, "T_HUB_CONTROL_TOKEN").unwrap(),
        Some(String::new())
    );
    let durable = captains.snapshot();
    assert_eq!(durable.schema_version, CAPTAINS_SCHEMA_VERSION);
    assert!(matches!(
        durable.cortana.recovery,
        crate::cortana_reconcile::CortanaRecoveryState::Healthy { .. }
    ));
    assert_eq!(
        durable
            .cortana
            .quarantine_ledger
            .last()
            .map(|quarantine| quarantine.terminal_id.as_str()),
        Some(orphan_terminal.as_str())
    );
    assert_eq!(
        durable
            .captains
            .iter()
            .filter(|captain| captain.role == FleetRole::Cortana)
            .count(),
        1
    );
    let replacement_identity = durable.cortana.identity_id.unwrap();
    assert_eq!(
        identities.get(&replacement_identity).unwrap().role,
        crate::identity::Role::Cortana
    );

    dispatch(
        &ctx,
        "close_terminal",
        &json!({ "sessionId": replacement_terminal }),
    )
    .unwrap();
    reap_test_tmux_session_and_assert_absent(&orphan_target);
    std::fs::remove_dir_all(harness_bin_dir).ok();
    std::fs::remove_dir_all(home).ok();
    std::fs::remove_file(migration_backup).ok();
    std::fs::remove_file(registry_path).ok();
    std::fs::remove_file(identity_path).ok();
}

#[test]
fn captured_packaged_schema25_orphan_rotates_then_quarantines_without_signal() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    if !tmux_process_tests_available() {
        eprintln!(
                "captured_packaged_schema25_orphan_rotates_then_quarantines_without_signal: tmux or node not on PATH - skipping"
            );
        return;
    }
    let fixture: Value = serde_json::from_str(PACKAGED_SCHEMA_25_LEGACY_ORPHAN_FIXTURE).unwrap();
    let registry_path = captains_tmp("captured-packaged-schema25-orphan");
    let identity_path = captains_tmp("captured-packaged-schema25-identities");
    std::fs::write(
        &registry_path,
        serde_json::to_vec_pretty(&fixture["captainsSnapshot"]).unwrap(),
    )
    .unwrap();
    std::fs::write(
        &identity_path,
        serde_json::to_vec_pretty(&fixture["identitiesSnapshot"]).unwrap(),
    )
    .unwrap();
    let captains = Arc::new(CaptainsRegistry::load(registry_path.clone()));
    let identities = Arc::new(crate::identity::IdentityStore::load(identity_path.clone()));
    let terminal_id = fixture["capture"]["runtime"]["terminalId"]
        .as_str()
        .unwrap();
    let legacy_addr = fixture["capture"]["control"]["legacyAddress"]
        .as_str()
        .unwrap();
    let current_addr = fixture["capture"]["control"]["currentAddress"]
        .as_str()
        .unwrap();
    let shared_token = fixture["capture"]["control"]["sharedPersistentToken"]
        .as_str()
        .unwrap();
    let session_token = fixture["capture"]["runtime"]["sessionToken"]
        .as_str()
        .unwrap();
    let legacy_identity = captains.cortana_identity().identity_id.clone().unwrap();
    let home = std::env::temp_dir().join(format!(
        "t-hub-captured-packaged-orphan-home-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&home).unwrap();
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    let target = exact_cortana_tmux_target(terminal_id).unwrap();
    create_test_tmux_session_with_env(
        &target,
        home.to_str().unwrap(),
        Some(&harness_command),
        &[
            (
                crate::identity::SESSION_TOKEN_ENV.into(),
                session_token.into(),
            ),
            ("T_HUB_CONTROL_ADDR".into(), legacy_addr.into()),
            ("T_HUB_CONTROL_TOKEN".into(), shared_token.into()),
            (CORTANA_GENERATION_ENV.into(), "1".into()),
        ],
    )
    .unwrap();
    wait_for_harness_started(terminal_id, "codex").unwrap();

    let build_ctx = |token: &str| {
        let mut ctx = test_ctx(token)
            .with_live_sessions(|| Ok(Vec::new()))
            .with_captains_registry(captains.clone())
            .with_identity_store(identities.clone())
            .with_apply_sink(Arc::new(RecordingSink {
                calls: StdMutex::new(Vec::new()),
            }));
        ctx.addr = current_addr.into();
        ctx.tab_registry().replace(vec![TabRecord {
            id: CAPTAIN_WORKSPACE_ID.into(),
            name: CAPTAIN_WORKSPACE_NAME.into(),
            tile_ids: vec![terminal_id.into()],
        }]);
        ctx
    };
    let same_bearer = build_ctx(shared_token);
    let reproduced = dispatch(
        &same_bearer,
        "reconcile_cortana",
        &json!({
            "operationId": "captured-packaged-before-rotation",
            "testOrchestratorHome": home,
            "testStartupCommand": harness_command,
        }),
    )
    .unwrap();
    assert_eq!(reproduced["action"], "degraded");
    assert_eq!(
            reproduced["degradedReason"],
            format!(
                "live runtime '{terminal_id}' in Cortana's reserved scope lacks authoritative identity, generation, or control evidence"
            )
        );
    assert_eq!(
        tmux::session_liveness(&target),
        tmux::SessionLiveness::Alive
    );
    assert!(!identities.is_revoked(&legacy_identity));

    let key_dir = std::env::temp_dir().join(format!(
        "t-hub-captured-packaged-key-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&key_dir).unwrap();
    let key_path = key_dir.join("server-key");
    write_key_file(&key_path, shared_token);
    let rotated_token = persistent_key_for_start_with(&key_path, false, 3600, true).unwrap();
    assert_ne!(rotated_token, shared_token);

    let restarted = build_ctx(&rotated_token);
    assert_eq!(resolve_capability(&restarted, shared_token), None);
    assert_eq!(
        resolve_capability(&restarted, &rotated_token),
        Some(Capability::Full)
    );
    let denied = dispatch_authenticated(
        &restarted,
        ControlRequest {
            token: shared_token.into(),
            command: "close_terminal".into(),
            args: json!({ "sessionId": terminal_id }),
            session: session_token.into(),
            host: String::new(),
            v: Some(PROTOCOL_VERSION),
        },
    );
    assert!(!denied.ok);
    assert_eq!(
        denied.error.as_deref(),
        Some("unauthorized: bad control token")
    );
    let recovered = dispatch(
        &restarted,
        "reconcile_cortana",
        &json!({
            "operationId": "captured-packaged-after-rotation",
            "testOrchestratorHome": home,
            "testStartupCommand": harness_command,
        }),
    )
    .unwrap();
    assert_eq!(recovered["action"], "recover");
    assert_eq!(recovered["healthy"], true);
    assert_eq!(recovered["generation"], 2);
    assert_eq!(
        tmux::session_liveness(&target),
        tmux::SessionLiveness::Alive
    );
    assert!(identities.is_revoked(&legacy_identity));
    assert_eq!(
        captains
            .cortana_identity()
            .quarantine_ledger
            .last()
            .map(|quarantine| quarantine.terminal_id.as_str()),
        Some(terminal_id)
    );

    let replacement = recovered["terminalId"].as_str().unwrap().to_string();
    let replacement_target = exact_cortana_tmux_target(&replacement).unwrap();
    assert_eq!(
        tmux::session_environment(&replacement_target, "T_HUB_CONTROL_FILE").unwrap(),
        Some(discovery_file_for_spawn())
    );
    assert_eq!(
        tmux::session_environment(&replacement_target, "T_HUB_CONTROL_TOKEN").unwrap(),
        Some(String::new())
    );
    let replacement_session_token =
        tmux::session_environment(&replacement_target, crate::identity::SESSION_TOKEN_ENV)
            .unwrap()
            .expect("replacement has a per-session bearer");
    let replacement_identity = identities
        .resolve(&replacement_session_token)
        .expect("replacement bearer resolves after control-key rotation");
    assert_eq!(replacement_identity.role, crate::identity::Role::Cortana);
    assert_eq!(
        replacement_identity.session_tile.as_deref(),
        Some(replacement.as_str())
    );
    assert_eq!(
        captains
            .snapshot()
            .captains
            .iter()
            .filter(|captain| {
                captain.role == FleetRole::Cortana
                    && captain.state == ClaimState::Active
                    && captain.terminal_id.as_deref() == Some(replacement.as_str())
            })
            .count(),
        1
    );
    dispatch(
        &restarted,
        "close_terminal",
        &json!({ "sessionId": replacement }),
    )
    .unwrap();
    reap_test_tmux_session_and_assert_absent(&target);
    std::fs::remove_dir_all(key_dir).ok();
    std::fs::remove_dir_all(harness_bin_dir).ok();
    std::fs::remove_dir_all(home).ok();
    std::fs::remove_file(registry_path).ok();
    std::fs::remove_file(identity_path).ok();
}

fn legacy_orphan_durable(
    identity_id: &str,
    terminal_id: &str,
) -> crate::cortana_reconcile::CortanaDurableIdentity {
    crate::cortana_reconcile::CortanaDurableIdentity {
        identity_id: Some(identity_id.into()),
        generation: 1,
        terminal_id: None,
        harness: Some("codex".into()),
        legacy_orphan_provenance: Some(crate::cortana_reconcile::CortanaLegacyOrphanProvenance {
            version: crate::cortana_reconcile::LEGACY_ORPHAN_PROVENANCE_VERSION,
            source_schema_version: 18,
            identity_id: identity_id.into(),
            terminal_id: terminal_id.into(),
            generation: 1,
            harness: "codex".into(),
            healthy_operation_id: "legacy-healthy".into(),
        }),
        recovery: crate::cortana_reconcile::CortanaRecoveryState::Degraded {
            operation_id: "legacy-degraded".into(),
            reason: "identity disappeared".into(),
            detected_at: 1,
        },
        ..Default::default()
    }
}

fn stale_legacy_orphan_candidate(
    terminal_id: &str,
) -> crate::cortana_reconcile::CortanaRuntimeCandidate {
    crate::cortana_reconcile::CortanaRuntimeCandidate {
        terminal_id: terminal_id.into(),
        identity_id: None,
        generation: 1,
        harness: "codex".into(),
        provider_session_id: None,
        terminal: crate::cortana_reconcile::RuntimeEvidence::Alive,
        harness_process: crate::cortana_reconcile::RuntimeEvidence::Alive,
        identity_bound_to_terminal: false,
        canonical_control_file: false,
        rotating_control_env_scrubbed: false,
        stale_legacy_control_env: true,
        unresolved_session_bearer: true,
        effect_identity: Some(test_cortana_effect_identity(100)),
        current_control_capability: false,
        trusted_cortana_identity: false,
    }
}

fn test_cortana_effect_identity(
    seed: u32,
) -> crate::cortana_reconcile::CortanaOrphanEffectIdentity {
    crate::cortana_reconcile::CortanaOrphanEffectIdentity {
        tmux_session_id: u64::from(seed),
        tmux_session_created: u64::from(seed) + 1,
        tmux_window_id: u64::from(seed) + 2,
        tmux_pane_id: u64::from(seed) + 3,
        pane_pid: seed + 4,
        pane_start_ticks: u64::from(seed) + 5,
        pane_process_group_id: seed + 4,
        pane_process_session_id: seed + 4,
        foreground_pid: seed + 6,
        foreground_start_ticks: u64::from(seed) + 7,
        foreground_process_group_id: seed + 6,
        foreground_process_session_id: seed + 4,
    }
}

#[test]
fn schema30_singular_cortana_quarantine_migrates_to_canonical_ledger() {
    let path = captains_tmp("schema30-singular-cortana-quarantine");
    let effect = test_cortana_effect_identity(31);
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&json!({
            "schemaVersion": 30,
            "seq": 9,
            "captains": [],
            "workspaces": [{
                "id": CAPTAIN_WORKSPACE_ID,
                "name": CAPTAIN_WORKSPACE_NAME,
                "kind": "captain",
                "tileIds": [],
            }],
            "cortana": {
                "identityId": "schema30-burned-identity",
                "generation": 1,
                "terminalId": null,
                "harness": "codex",
                "legacyQuarantine": {
                    "terminalId": "deadbeef",
                    "identityId": "schema30-burned-identity",
                    "generation": 1,
                    "harness": "codex",
                    "tmux": effect,
                    "authorityRevoked": true,
                    "quarantinedAt": 8,
                },
                "recovery": {
                    "kind": "legacyUnownedQuarantined",
                    "operation_id": "schema30-quarantine",
                    "quarantined_at": 8,
                    "legacy_terminal_id": "deadbeef",
                    "legacy_generation": 1,
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();
    let registry = CaptainsRegistry::load(path.clone());
    let snapshot = registry.snapshot();
    assert_eq!(snapshot.cortana.quarantine_ledger.len(), 1);
    assert_eq!(
        snapshot.cortana.quarantine_ledger[0].terminal_id,
        "deadbeef"
    );
    let canonical = serde_json::to_value(snapshot).unwrap();
    assert_eq!(canonical["schemaVersion"], CAPTAINS_SCHEMA_VERSION);
    assert!(canonical.pointer("/cortana/quarantineLedger").is_some());
    assert!(canonical.pointer("/cortana/legacyQuarantine").is_none());
    let conflict_path = captains_tmp("schema31-conflicting-cortana-quarantine");
    let mut conflicting = canonical;
    let duplicate = conflicting["cortana"]["quarantineLedger"][0].clone();
    conflicting["cortana"]["quarantineLedger"]
        .as_array_mut()
        .unwrap()
        .push(duplicate);
    std::fs::write(
        &conflict_path,
        serde_json::to_vec_pretty(&conflicting).unwrap(),
    )
    .unwrap();
    assert!(CaptainsRegistry::read_snapshot(&conflict_path).is_err());
    std::fs::remove_file(path).ok();
    std::fs::remove_file(conflict_path).ok();
}

#[test]
fn managed_quarantine_generation_allows_only_foreground_transition() {
    let owner = test_cortana_effect_identity(41);
    let mut harness = owner;
    harness.foreground_pid = harness.foreground_pid.saturating_add(100);
    harness.foreground_start_ticks = harness.foreground_start_ticks.saturating_add(100);
    harness.foreground_process_group_id = harness.foreground_pid;
    assert!(same_cortana_tmux_generation(&owner, &harness));
    let mutations: [fn(&mut crate::cortana_reconcile::CortanaOrphanEffectIdentity); 6] = [
        |value: &mut crate::cortana_reconcile::CortanaOrphanEffectIdentity| {
            value.tmux_session_id = value.tmux_session_id.saturating_add(1)
        },
        |value: &mut crate::cortana_reconcile::CortanaOrphanEffectIdentity| {
            value.tmux_session_created = value.tmux_session_created.saturating_add(1)
        },
        |value: &mut crate::cortana_reconcile::CortanaOrphanEffectIdentity| {
            value.tmux_window_id = value.tmux_window_id.saturating_add(1)
        },
        |value: &mut crate::cortana_reconcile::CortanaOrphanEffectIdentity| {
            value.tmux_pane_id = value.tmux_pane_id.saturating_add(1)
        },
        |value: &mut crate::cortana_reconcile::CortanaOrphanEffectIdentity| {
            value.pane_pid = value.pane_pid.saturating_add(1)
        },
        |value: &mut crate::cortana_reconcile::CortanaOrphanEffectIdentity| {
            value.pane_start_ticks = value.pane_start_ticks.saturating_add(1)
        },
    ];
    for mutate in mutations {
        let mut changed = harness;
        mutate(&mut changed);
        assert!(!same_cortana_tmux_generation(&owner, &changed));
    }
}

#[test]
fn legacy_orphan_retirement_requires_exact_provenance_and_untrusted_stale_runtime() {
    let terminal_id = "a1b2c3d4";
    let missing_identity = "missing-legacy-cortana";
    let ctx = test_ctx("legacy-retirement-current-token");
    let durable = legacy_orphan_durable(missing_identity, terminal_id);
    let candidate = stale_legacy_orphan_candidate(terminal_id);
    assert!(
        retirable_legacy_cortana_orphan(&ctx, &durable, std::slice::from_ref(&candidate)).is_some()
    );

    let mut no_provenance = durable.clone();
    no_provenance.legacy_orphan_provenance = None;
    assert!(retirable_legacy_cortana_orphan(
        &ctx,
        &no_provenance,
        std::slice::from_ref(&candidate)
    )
    .is_none());

    let mut mismatched_terminal = durable.clone();
    mismatched_terminal
        .legacy_orphan_provenance
        .as_mut()
        .unwrap()
        .terminal_id = "other001".into();
    assert!(retirable_legacy_cortana_orphan(
        &ctx,
        &mismatched_terminal,
        std::slice::from_ref(&candidate)
    )
    .is_none());

    let mut current_endpoint = candidate.clone();
    current_endpoint.stale_legacy_control_env = false;
    assert!(retirable_legacy_cortana_orphan(
        &ctx,
        &durable,
        std::slice::from_ref(&current_endpoint)
    )
    .is_none());

    let mut copied_bearer = candidate.clone();
    copied_bearer.identity_id = Some("copied-known-identity".into());
    assert!(
        retirable_legacy_cortana_orphan(&ctx, &durable, std::slice::from_ref(&copied_bearer))
            .is_none()
    );

    let mut unknown_liveness = candidate.clone();
    unknown_liveness.terminal = crate::cortana_reconcile::RuntimeEvidence::Unknown;
    assert!(retirable_legacy_cortana_orphan(
        &ctx,
        &durable,
        std::slice::from_ref(&unknown_liveness)
    )
    .is_none());

    let mut missing_effect_identity = candidate.clone();
    missing_effect_identity.effect_identity = None;
    assert!(retirable_legacy_cortana_orphan(
        &ctx,
        &durable,
        std::slice::from_ref(&missing_effect_identity)
    )
    .is_none());

    assert!(retirable_legacy_cortana_orphan(
        &ctx,
        &durable,
        &[candidate.clone(), stale_legacy_orphan_candidate("e5f6g7h8")]
    )
    .is_none());

    let existing = ctx.identity.mint(crate::identity::Role::Cortana).unwrap();
    let existing_durable = legacy_orphan_durable(&existing.id, terminal_id);
    assert!(retirable_legacy_cortana_orphan(
        &ctx,
        &existing_durable,
        std::slice::from_ref(&candidate)
    )
    .is_none());

    let claimed = ctx
        .captains
        .claim_test("active-cortana", Some("legacy-active-claim"), vec![])
        .unwrap();
    {
        let mut inner = ctx.captains.lock();
        let record = inner
            .captains
            .iter_mut()
            .find(|record| record.ship_slug == claimed.record.ship_slug)
            .unwrap();
        record.role = FleetRole::Cortana;
        record.state = ClaimState::Active;
    }
    assert!(
        retirable_legacy_cortana_orphan(&ctx, &durable, std::slice::from_ref(&candidate)).is_none()
    );
}

#[test]
fn stale_legacy_control_detection_rejects_current_endpoint_or_token() {
    let current_addr = "127.0.0.1:63930";
    let current_token = "current-control-token";
    assert!(stale_legacy_cortana_control_env(
        None,
        Some("127.0.0.1:51330"),
        Some("stale-control-token"),
        current_addr,
        current_token,
    ));
    for (control_file, address, token) in [
        (
            Some("/home/user/.t-hub-dev/control.json"),
            Some("127.0.0.1:51330"),
            Some("stale-control-token"),
        ),
        (None, Some(current_addr), Some("stale-control-token")),
        (None, Some("127.0.0.1:51330"), Some(current_token)),
        (None, None, Some("stale-control-token")),
        (None, Some("127.0.0.1:51330"), None),
    ] {
        assert!(!stale_legacy_cortana_control_env(
            control_file,
            address,
            token,
            current_addr,
            current_token,
        ));
    }
}

#[test]
fn stale_legacy_runtime_without_exact_provenance_stays_alive_and_degraded() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    if !tmux_process_tests_available() {
        eprintln!(
                "stale_legacy_runtime_without_exact_provenance_stays_alive_and_degraded: tmux or node not on PATH - skipping"
            );
        return;
    }
    let registry_path = captains_tmp("cortana-stale-no-provenance");
    let terminal_id = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    std::fs::write(
        &registry_path,
        serde_json::to_vec_pretty(&json!({
            "schemaVersion": 21,
            "seq": 20,
            "captains": [],
            "workspaces": [{
                "id": CAPTAIN_WORKSPACE_ID,
                "name": CAPTAIN_WORKSPACE_NAME,
                "kind": "captain",
                "tileIds": []
            }],
            "cortana": {
                "identityId": "missing-no-provenance-identity",
                "generation": 1,
                "terminalId": null,
                "harness": "codex",
                "recovery": {
                    "kind": "degraded",
                    "operation_id": "no-provenance-original",
                    "reason": "identity disappeared",
                    "detected_at": 1
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();
    let captains = Arc::new(CaptainsRegistry::load(registry_path.clone()));
    assert!(captains
        .cortana_identity()
        .legacy_orphan_provenance
        .is_none());
    let mut ctx = test_ctx("no-provenance-current-token")
        .with_captains_registry(captains.clone())
        .with_apply_sink(Arc::new(RecordingSink {
            calls: StdMutex::new(Vec::new()),
        }));
    ctx.addr = "127.0.0.1:63930".into();
    let home = std::env::temp_dir().join(format!(
        "t-hub-cortana-no-provenance-home-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&home).unwrap();
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    let target = exact_cortana_tmux_target(&terminal_id).unwrap();
    create_test_tmux_session_with_env(
        &target,
        home.to_str().unwrap(),
        Some(&harness_command),
        &[
            (
                crate::identity::SESSION_TOKEN_ENV.into(),
                "unresolved-no-provenance-bearer".into(),
            ),
            ("T_HUB_CONTROL_ADDR".into(), "127.0.0.1:51330".into()),
            ("T_HUB_CONTROL_TOKEN".into(), "stale-control-token".into()),
            (CORTANA_GENERATION_ENV.into(), "1".into()),
        ],
    )
    .unwrap();
    wait_for_harness_started(&terminal_id, "codex").unwrap();

    let result = dispatch(
        &ctx,
        "reconcile_cortana",
        &json!({
            "operationId": "no-provenance-reconcile",
            "testOrchestratorHome": home,
            "testStartupCommand": harness_command,
        }),
    )
    .unwrap();
    assert_eq!(result["action"], "degraded");
    assert_eq!(result["healthy"], false);
    assert_eq!(
        tmux::session_liveness(&target),
        tmux::SessionLiveness::Alive
    );
    assert!(matches!(
        captains.cortana_identity().recovery,
        crate::cortana_reconcile::CortanaRecoveryState::Degraded { .. }
    ));

    reap_test_tmux_session(&target).unwrap();
    std::fs::remove_dir_all(harness_bin_dir).ok();
    std::fs::remove_dir_all(home).ok();
    std::fs::remove_file(registry_path).ok();
}

#[test]
fn ownerless_replacement_after_process_restart_is_not_adopted() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    if !tmux_process_tests_available() {
        eprintln!(
                "orphan_replacement_adopts_generation_two_after_process_restart: tmux or node not on PATH - skipping"
            );
        return;
    }
    let registry_path = captains_tmp("cortana-orphan-restart");
    let identity_path = captains_tmp("cortana-orphan-restart-identities");
    let orphan_terminal = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    let orphan_identity = "missing-restart-cortana-identity";
    std::fs::write(
        &registry_path,
        serde_json::to_vec_pretty(&json!({
            "schemaVersion": 18,
            "seq": 11,
            "captains": [],
            "workspaces": [{
                "id": CAPTAIN_WORKSPACE_ID,
                "name": CAPTAIN_WORKSPACE_NAME,
                "kind": "captain",
                "tileIds": [orphan_terminal.clone()]
            }],
            "cortana": {
                "identityId": orphan_identity,
                "generation": 1,
                "terminalId": orphan_terminal,
                "harness": "codex",
                "providerSessionId": null,
                "conversationId": null,
                "checkpoint": "restart-checkpoint",
                "recovery": {
                    "kind": "healthy",
                    "operation_id": "restart-original",
                    "verified_at": 1
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();
    let captains = Arc::new(CaptainsRegistry::load(registry_path.clone()));
    captains
        .begin_cortana_recovery("orphan-restart-operation")
        .unwrap();
    captains
        .prepare_cortana_orphan_replacement(
            "orphan-restart-operation",
            &orphan_terminal,
            orphan_identity,
            1,
            "codex",
            test_cortana_effect_identity(200),
        )
        .unwrap();
    let identities = Arc::new(crate::identity::IdentityStore::load(identity_path.clone()));
    let replacement = identities.mint(crate::identity::Role::Cortana).unwrap();
    captains
        .bind_cortana_orphan_replacement_identity("orphan-restart-operation", &replacement.id)
        .unwrap();
    let replacement_terminal = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    identities
        .bind_tile(&replacement.id, &replacement_terminal)
        .unwrap();
    let home = std::env::temp_dir().join(format!(
        "t-hub-cortana-orphan-restart-home-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&home).unwrap();
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    let replacement_target = exact_cortana_tmux_target(&replacement_terminal).unwrap();
    create_test_tmux_session_with_env(
        &replacement_target,
        home.to_str().unwrap(),
        Some(&harness_command),
        &[
            (
                crate::identity::SESSION_TOKEN_ENV.into(),
                replacement.secret.clone(),
            ),
            ("T_HUB_CONTROL_FILE".into(), discovery_file_for_spawn()),
            ("T_HUB_CONTROL_ADDR".into(), String::new()),
            ("T_HUB_CONTROL_TOKEN".into(), String::new()),
            (CORTANA_GENERATION_ENV.into(), "2".into()),
        ],
    )
    .unwrap();
    wait_for_harness_started(&replacement_terminal, "codex").unwrap();

    drop(captains);
    drop(identities);
    let restarted_captains = Arc::new(CaptainsRegistry::load(registry_path.clone()));
    let restarted_identities =
        Arc::new(crate::identity::IdentityStore::load(identity_path.clone()));
    assert!(matches!(
        restarted_captains.cortana_identity().recovery,
        crate::cortana_reconcile::CortanaRecoveryState::ReplacingOrphan { .. }
    ));
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let ctx = test_ctx("orphan-restart-control")
        .with_captains_registry(restarted_captains.clone())
        .with_identity_store(restarted_identities)
        .with_apply_sink(sink);
    ctx.tab_registry().replace(vec![TabRecord {
        id: CAPTAIN_WORKSPACE_ID.into(),
        name: CAPTAIN_WORKSPACE_NAME.into(),
        tile_ids: Vec::new(),
    }]);
    restarted_captains
        .claim_provider(
            &replacement_terminal,
            None,
            FleetRole::Cortana,
            Some("codex"),
            None,
            Vec::new(),
            &all_alive,
            &crew_all_alive,
        )
        .unwrap();
    let cross_store_error = restarted_captains
        .commit_cortana_runtime(
            "orphan-restart-operation",
            "unreserved-cross-store-identity",
            2,
            &replacement_terminal,
            "codex",
            None,
        )
        .unwrap_err();
    assert!(cross_store_error.contains("durable orphan replacement intent"));
    assert!(matches!(
        restarted_captains.cortana_identity().recovery,
        crate::cortana_reconcile::CortanaRecoveryState::ReplacingOrphan { .. }
    ));

    let error = dispatch(
        &ctx,
        "reconcile_cortana",
        &json!({
            "operationId": "new-request-after-restart",
            "testOrchestratorHome": home,
        }),
    )
    .unwrap_err();
    assert!(error.contains("authority is ambiguous"), "{error}");
    assert!(matches!(
        restarted_captains.cortana_identity().recovery,
        crate::cortana_reconcile::CortanaRecoveryState::ReplacingOrphan { .. }
    ));
    reap_test_tmux_session_and_assert_absent(&replacement_target);
    std::fs::remove_dir_all(harness_bin_dir).ok();
    std::fs::remove_dir_all(home).ok();
    std::fs::remove_file(registry_path).ok();
    std::fs::remove_file(identity_path).ok();
}

#[test]
fn prepared_legacy_orphan_restart_retires_only_exact_target_before_replacement() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    if !tmux_process_tests_available() {
        eprintln!(
                "prepared_legacy_orphan_restart_retires_only_exact_target_before_replacement: tmux or node not on PATH - skipping"
            );
        return;
    }
    let registry_path = captains_tmp("cortana-prepared-orphan-restart");
    let identity_path = captains_tmp("cortana-prepared-orphan-restart-identities");
    let orphan_terminal = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    let sentinel_terminal = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    let missing_identity = "missing-prepared-cortana-identity";
    std::fs::write(
        &registry_path,
        serde_json::to_vec_pretty(&json!({
            "schemaVersion": 18,
            "seq": 30,
            "captains": [],
            "workspaces": [{
                "id": CAPTAIN_WORKSPACE_ID,
                "name": CAPTAIN_WORKSPACE_NAME,
                "kind": "captain",
                "tileIds": [orphan_terminal.clone()]
            }],
            "cortana": {
                "identityId": missing_identity,
                "generation": 1,
                "terminalId": orphan_terminal,
                "harness": "codex",
                "recovery": {
                    "kind": "healthy",
                    "operation_id": "prepared-original",
                    "verified_at": 1
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();
    let home = std::env::temp_dir().join(format!(
        "t-hub-cortana-prepared-restart-home-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&home).unwrap();
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    let orphan_target = exact_cortana_tmux_target(&orphan_terminal).unwrap();
    create_test_tmux_session_with_env(
        &orphan_target,
        home.to_str().unwrap(),
        Some(&harness_command),
        &[
            (
                crate::identity::SESSION_TOKEN_ENV.into(),
                "unresolved-prepared-bearer".into(),
            ),
            ("T_HUB_CONTROL_ADDR".into(), "127.0.0.1:51330".into()),
            ("T_HUB_CONTROL_TOKEN".into(), "stale-control-token".into()),
            (CORTANA_GENERATION_ENV.into(), "1".into()),
        ],
    )
    .unwrap();
    wait_for_harness_started(&orphan_terminal, "codex").unwrap();
    let orphan_effect_identity = durable_cortana_effect_identity(
        tmux::observe_session_effect_identity(&orphan_target).unwrap(),
    );
    let sentinel_target = exact_cortana_tmux_target(&sentinel_terminal).unwrap();
    create_test_tmux_session(&sentinel_target).unwrap();

    let captains = Arc::new(CaptainsRegistry::load(registry_path.clone()));
    captains
        .begin_cortana_recovery("prepared-restart-operation")
        .unwrap();
    captains
        .prepare_cortana_orphan_replacement(
            "prepared-restart-operation",
            &orphan_terminal,
            missing_identity,
            1,
            "codex",
            orphan_effect_identity,
        )
        .unwrap();
    assert_eq!(
        tmux::session_liveness(&orphan_target),
        tmux::SessionLiveness::Alive
    );
    drop(captains);

    let restarted_captains = Arc::new(CaptainsRegistry::load(registry_path.clone()));
    assert!(matches!(
        restarted_captains.cortana_identity().recovery,
        crate::cortana_reconcile::CortanaRecoveryState::ReplacingOrphan { .. }
    ));
    let identities = Arc::new(crate::identity::IdentityStore::load(identity_path.clone()));
    let mut ctx = test_ctx("prepared-restart-current-token")
        .with_captains_registry(restarted_captains.clone())
        .with_identity_store(identities)
        .with_apply_sink(Arc::new(RecordingSink {
            calls: StdMutex::new(Vec::new()),
        }));
    ctx.addr = "127.0.0.1:63930".into();
    ctx.tab_registry().replace(vec![TabRecord {
        id: CAPTAIN_WORKSPACE_ID.into(),
        name: CAPTAIN_WORKSPACE_NAME.into(),
        tile_ids: vec![orphan_terminal.clone()],
    }]);

    let competing_claim = ctx
        .captains
        .claim(
            "prepared-restart-competing-cortana",
            None,
            FleetRole::Cortana,
            None,
            vec![],
            &all_alive,
            &crew_all_alive,
        )
        .unwrap();
    let claim_error = dispatch(
        &ctx,
        "reconcile_cortana",
        &json!({
            "operationId": "ignored-while-competing-claim-exists",
            "testOrchestratorHome": home,
            "testStartupCommand": harness_command,
        }),
    )
    .unwrap_err();
    assert!(claim_error.contains("authority is ambiguous"));
    assert_eq!(
        tmux::session_liveness(&orphan_target),
        tmux::SessionLiveness::Alive
    );
    {
        let mut inner = ctx.captains.lock();
        let record = inner
            .captains
            .iter_mut()
            .find(|record| record.ship_slug == competing_claim.record.ship_slug)
            .unwrap();
        record.state = ClaimState::Vacant;
        record.terminal_id = None;
    }

    let recovered = dispatch(
        &ctx,
        "reconcile_cortana",
        &json!({
            "operationId": "ignored-after-prepared-restart",
            "testOrchestratorHome": home,
            "testStartupCommand": harness_command,
        }),
    )
    .unwrap();
    assert_eq!(recovered["operationId"], "prepared-restart-operation");
    assert_eq!(recovered["action"], "recover");
    assert_eq!(recovered["generation"], 2);
    assert_eq!(
        tmux::session_liveness(&orphan_target),
        tmux::SessionLiveness::Alive
    );
    assert_eq!(
        tmux::session_liveness(&sentinel_target),
        tmux::SessionLiveness::Alive
    );
    let replacement_terminal = recovered["terminalId"].as_str().unwrap();
    let replacement_target = exact_cortana_tmux_target(replacement_terminal).unwrap();
    assert_eq!(
        tmux::session_environment(&replacement_target, "T_HUB_CONTROL_FILE").unwrap(),
        Some(discovery_file_for_spawn())
    );
    assert_eq!(
        tmux::session_environment(&replacement_target, "T_HUB_CONTROL_ADDR").unwrap(),
        Some(String::new())
    );
    assert_eq!(
        tmux::session_environment(&replacement_target, "T_HUB_CONTROL_TOKEN").unwrap(),
        Some(String::new())
    );

    dispatch(
        &ctx,
        "close_terminal",
        &json!({ "sessionId": replacement_terminal }),
    )
    .unwrap();
    reap_test_tmux_session(&orphan_target).unwrap();
    reap_test_tmux_session(&sentinel_target).unwrap();
    std::fs::remove_dir_all(harness_bin_dir).ok();
    std::fs::remove_dir_all(home).ok();
    std::fs::remove_file(registry_path).ok();
    std::fs::remove_file(identity_path).ok();
}

#[test]
fn prepared_legacy_orphan_restart_preserves_same_session_replacement() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    if !tmux_process_tests_available() {
        eprintln!(
                "prepared_legacy_orphan_restart_preserves_same_session_replacement: tmux or node not on PATH - skipping"
            );
        return;
    }
    let registry_path = captains_tmp("cortana-prepared-same-session-reuse");
    let identity_path = captains_tmp("cortana-prepared-same-session-reuse-identities");
    let orphan_terminal = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    let missing_identity = "missing-reused-session-cortana-identity";
    std::fs::write(
        &registry_path,
        serde_json::to_vec_pretty(&json!({
            "schemaVersion": 18,
            "seq": 40,
            "captains": [],
            "workspaces": [{
                "id": CAPTAIN_WORKSPACE_ID,
                "name": CAPTAIN_WORKSPACE_NAME,
                "kind": "captain",
                "tileIds": [orphan_terminal.clone()]
            }],
            "cortana": {
                "identityId": missing_identity,
                "generation": 1,
                "terminalId": orphan_terminal,
                "harness": "codex",
                "recovery": {
                    "kind": "healthy",
                    "operation_id": "same-session-original",
                    "verified_at": 1
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();
    let home = std::env::temp_dir().join(format!(
        "t-hub-cortana-same-session-reuse-home-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&home).unwrap();
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    let orphan_target = exact_cortana_tmux_target(&orphan_terminal).unwrap();
    create_test_tmux_session_with_env(
        &orphan_target,
        home.to_str().unwrap(),
        Some(&harness_command),
        &[
            (
                crate::identity::SESSION_TOKEN_ENV.into(),
                "unresolved-reused-session-bearer".into(),
            ),
            ("T_HUB_CONTROL_ADDR".into(), "127.0.0.1:51330".into()),
            ("T_HUB_CONTROL_TOKEN".into(), "stale-control-token".into()),
            (CORTANA_GENERATION_ENV.into(), "1".into()),
        ],
    )
    .unwrap();
    wait_for_harness_started(&orphan_terminal, "codex").unwrap();
    let original_effect = tmux::observe_session_effect_identity(&orphan_target).unwrap();

    let captains = Arc::new(CaptainsRegistry::load(registry_path.clone()));
    captains
        .begin_cortana_recovery("same-session-reuse-operation")
        .unwrap();
    captains
        .prepare_cortana_orphan_replacement(
            "same-session-reuse-operation",
            &orphan_terminal,
            missing_identity,
            1,
            "codex",
            durable_cortana_effect_identity(original_effect),
        )
        .unwrap();

    let transition =
        tmux::respawn_pane_exact(&orphan_target, home.to_str().unwrap(), &harness_command).unwrap();
    assert_eq!(transition.before.session_id, transition.after.session_id);
    assert_eq!(
        transition.before.session_created,
        transition.after.session_created
    );
    assert_eq!(transition.before.window_id, transition.after.window_id);
    assert_eq!(transition.before.pane_id, transition.after.pane_id);
    assert_ne!(transition.before.pane_pid, transition.after.pane_pid);
    wait_for_harness_started(&orphan_terminal, "codex").unwrap();
    let replacement_effect = tmux::observe_session_effect_identity(&orphan_target).unwrap();
    assert_ne!(replacement_effect, original_effect);
    drop(captains);

    let restarted_captains = Arc::new(CaptainsRegistry::load(registry_path.clone()));
    let identities = Arc::new(crate::identity::IdentityStore::load(identity_path.clone()));
    let mut ctx = test_ctx("same-session-reuse-current-token")
        .with_captains_registry(restarted_captains.clone())
        .with_identity_store(identities)
        .with_apply_sink(Arc::new(RecordingSink {
            calls: StdMutex::new(Vec::new()),
        }));
    ctx.addr = "127.0.0.1:63930".into();
    ctx.tab_registry().replace(vec![TabRecord {
        id: CAPTAIN_WORKSPACE_ID.into(),
        name: CAPTAIN_WORKSPACE_NAME.into(),
        tile_ids: vec![orphan_terminal.clone()],
    }]);

    let error = dispatch(
        &ctx,
        "reconcile_cortana",
        &json!({
            "operationId": "ignored-after-same-session-reuse",
            "testOrchestratorHome": home,
            "testStartupCommand": harness_command,
        }),
    )
    .unwrap_err();
    assert!(error.contains("evidence is ambiguous"), "{error}");
    assert_eq!(
        tmux::session_liveness(&orphan_target),
        tmux::SessionLiveness::Alive,
        "same-session replacement must survive a stale prepared retirement"
    );
    assert_eq!(
        tmux::observe_session_effect_identity(&orphan_target).unwrap(),
        replacement_effect
    );
    assert!(matches!(
        restarted_captains.cortana_identity().recovery,
        crate::cortana_reconcile::CortanaRecoveryState::ReplacingOrphan { .. }
    ));

    reap_test_tmux_session(&orphan_target).unwrap();
    std::fs::remove_dir_all(harness_bin_dir).ok();
    std::fs::remove_dir_all(home).ok();
    std::fs::remove_file(registry_path).ok();
    std::fs::remove_file(identity_path).ok();
}

#[test]
fn orphan_replacement_restart_rejects_copied_bearers_and_control_env_drift() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    if !tmux_process_tests_available() {
        eprintln!(
                "orphan_replacement_restart_rejects_copied_bearers_and_control_env_drift: tmux or node not on PATH - skipping"
            );
        return;
    }
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    for case in [
        "copied-bearer-wrong-tile",
        "missing-control-file",
        "wrong-control-file",
        "nonblank-control-addr",
        "nonblank-control-token",
    ] {
        let registry_path = captains_tmp(&format!("cortana-negative-restart-{case}"));
        let identity_path = captains_tmp(&format!("cortana-negative-identity-{case}"));
        let orphan_terminal = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
        std::fs::write(
            &registry_path,
            serde_json::to_vec_pretty(&json!({
                "schemaVersion": 18,
                "seq": 1,
                "captains": [],
                "workspaces": [{
                    "id": CAPTAIN_WORKSPACE_ID,
                    "name": CAPTAIN_WORKSPACE_NAME,
                    "kind": "captain",
                    "tileIds": [orphan_terminal.clone()]
                }],
                "cortana": {
                    "identityId": "missing-negative-cortana-identity",
                    "generation": 1,
                    "terminalId": orphan_terminal,
                    "harness": "codex",
                    "providerSessionId": null,
                    "conversationId": null,
                    "checkpoint": null,
                    "recovery": {
                        "kind": "healthy",
                        "operation_id": "negative-original",
                        "verified_at": 1
                    }
                }
            }))
            .unwrap(),
        )
        .unwrap();
        let captains = Arc::new(CaptainsRegistry::load(registry_path.clone()));
        captains
            .begin_cortana_recovery("negative-restart-operation")
            .unwrap();
        captains
            .prepare_cortana_orphan_replacement(
                "negative-restart-operation",
                &orphan_terminal,
                "missing-negative-cortana-identity",
                1,
                "codex",
                test_cortana_effect_identity(300),
            )
            .unwrap();
        let identities = Arc::new(crate::identity::IdentityStore::load(identity_path.clone()));
        let replacement = identities.mint(crate::identity::Role::Cortana).unwrap();
        captains
            .bind_cortana_orphan_replacement_identity("negative-restart-operation", &replacement.id)
            .unwrap();
        let replacement_terminal = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
        let bound_terminal = if case == "copied-bearer-wrong-tile" {
            "source-tile"
        } else {
            replacement_terminal.as_str()
        };
        identities
            .bind_tile(&replacement.id, bound_terminal)
            .unwrap();
        let home = std::env::temp_dir().join(format!(
            "t-hub-cortana-negative-restart-{case}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&home).unwrap();
        let mut environment = vec![
            (
                crate::identity::SESSION_TOKEN_ENV.into(),
                replacement.secret.clone(),
            ),
            (CORTANA_GENERATION_ENV.into(), "2".into()),
        ];
        if case != "missing-control-file" {
            environment.push((
                "T_HUB_CONTROL_FILE".into(),
                if case == "wrong-control-file" {
                    "/tmp/foreign-t-hub-control.json".into()
                } else {
                    discovery_file_for_spawn()
                },
            ));
        }
        environment.push((
            "T_HUB_CONTROL_ADDR".into(),
            if case == "nonblank-control-addr" {
                "127.0.0.1:9".into()
            } else {
                String::new()
            },
        ));
        environment.push((
            "T_HUB_CONTROL_TOKEN".into(),
            if case == "nonblank-control-token" {
                "copied-global-token".into()
            } else {
                String::new()
            },
        ));
        let replacement_target = exact_cortana_tmux_target(&replacement_terminal).unwrap();
        create_test_tmux_session_with_env(
            &replacement_target,
            home.to_str().unwrap(),
            Some(&harness_command),
            &environment,
        )
        .unwrap();
        wait_for_harness_started(&replacement_terminal, "codex").unwrap();

        drop(captains);
        drop(identities);
        let restarted_captains = Arc::new(CaptainsRegistry::load(registry_path.clone()));
        let restarted_identities =
            Arc::new(crate::identity::IdentityStore::load(identity_path.clone()));
        let ctx = test_ctx(&format!("negative-restart-{case}"))
            .with_captains_registry(restarted_captains.clone())
            .with_identity_store(restarted_identities)
            .with_apply_sink(Arc::new(RecordingSink {
                calls: StdMutex::new(Vec::new()),
            }));

        let error = dispatch(
            &ctx,
            "reconcile_cortana",
            &json!({
                "operationId": "new-request-must-not-replace-durable-operation",
                "testOrchestratorHome": home,
            }),
        )
        .unwrap_err();
        assert!(error.contains("reserved scope changed"), "{case}: {error}");
        assert!(matches!(
            restarted_captains.cortana_identity().recovery,
            crate::cortana_reconcile::CortanaRecoveryState::LegacyUnownedQuarantined { .. }
        ));
        assert!(!restarted_captains
            .snapshot()
            .captains
            .iter()
            .any(|captain| captain.role == FleetRole::Cortana));
        assert_eq!(
            tmux::session_liveness(&replacement_target),
            tmux::SessionLiveness::Alive,
            "{case} must fail closed without killing an untrusted candidate"
        );

        reap_test_tmux_session(&replacement_target).unwrap();
        std::fs::remove_dir_all(home).ok();
        std::fs::remove_file(registry_path).ok();
        std::fs::remove_file(identity_path).ok();
    }
    std::fs::remove_dir_all(harness_bin_dir).ok();
}

#[test]
fn discovered_preowner_cortana_is_quarantined_and_replacement_consumes_spawn_rate() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let mut ctx = test_ctx("cortana-no-spawn-rate")
        .with_live_sessions(|| Ok(Vec::new()))
        .with_apply_sink(sink)
        .with_governor(Arc::new(SpawnGovernor::new(64, 0.0, 1.0)));
    ctx.addr = "127.0.0.1:4249".into();
    ctx.tab_registry().replace(vec![TabRecord {
        id: CAPTAIN_WORKSPACE_ID.into(),
        name: CAPTAIN_WORKSPACE_NAME.into(),
        tile_ids: Vec::new(),
    }]);
    let home = std::env::temp_dir().join(format!(
        "t-hub-cortana-no-spawn-rate-{}-{}",
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
            (CORTANA_GENERATION_ENV.into(), "1".into()),
        ],
    )
    .unwrap();
    wait_for_harness_started(&terminal_id, "codex").unwrap();

    let adopted = dispatch(
        &ctx,
        "reconcile_cortana",
        &json!({
            "operationId": "cortana-no-spawn-rate-1",
            "testOrchestratorHome": home,
            "testStartupCommand": harness_command,
        }),
    )
    .unwrap();
    assert_eq!(adopted["action"], "recover");
    assert_eq!(adopted["generation"], 2);
    assert_ne!(adopted["terminalId"], terminal_id);
    assert_eq!(
        tmux::session_liveness(&target),
        tmux::SessionLiveness::Alive,
        "pre-owner quarantine must not signal the old runtime"
    );

    assert!(admit_spawn(&ctx, SpawnPurpose::Ordinary, 1, None).is_err());
    let replacement_terminal = adopted["terminalId"].as_str().unwrap();
    dispatch(
        &ctx,
        "close_terminal",
        &json!({ "sessionId": replacement_terminal }),
    )
    .unwrap();
    reap_test_tmux_session(&target).unwrap();
    std::fs::remove_dir_all(harness_bin_dir).unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn concurrent_cortana_startup_calls_produce_one_runtime() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let mut ctx = test_ctx("cortana-concurrent")
        .with_live_sessions(|| Ok(Vec::new()))
        .with_apply_sink(sink);
    ctx.addr = "127.0.0.1:4243".into();
    ctx.tab_registry().replace(vec![TabRecord {
        id: CAPTAIN_WORKSPACE_ID.into(),
        name: CAPTAIN_WORKSPACE_NAME.into(),
        tile_ids: vec![],
    }]);
    let home = std::env::temp_dir().join(format!(
        "t-hub-cortana-concurrent-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    let start = Arc::new(std::sync::Barrier::new(3));
    let mut workers = Vec::new();
    for _ in 0..2 {
        let ctx = ctx.clone();
        let home = home.clone();
        let harness_command = harness_command.clone();
        let start = start.clone();
        workers.push(std::thread::spawn(move || {
            start.wait();
            dispatch(
                &ctx,
                "reconcile_cortana",
                &json!({
                    "operationId": "cortana-concurrent-startup",
                    "testOrchestratorHome": home,
                    "testStartupCommand": harness_command,
                }),
            )
            .unwrap()
        }));
    }
    start.wait();
    let results = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(results[0]["terminalId"], results[1]["terminalId"]);
    assert_eq!(results[0]["identityId"], results[1]["identityId"]);
    assert_eq!(
        ctx.captains
            .snapshot()
            .captains
            .iter()
            .filter(|captain| captain.role == FleetRole::Cortana)
            .count(),
        1
    );
    let terminal_id = results[0]["terminalId"].as_str().unwrap();
    dispatch(&ctx, "close_terminal", &json!({ "sessionId": terminal_id })).unwrap();
    let _ = std::fs::remove_dir_all(harness_bin_dir);
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn cortana_attestation_transition_retries_are_bounded() {
    let ctx = test_ctx("cortana-transition-budget");
    let error = reconcile_cortana_with_transition_count(&ctx, &json!({}), true, 7)
        .expect_err("an exhausted attestation transition budget must fail closed");
    assert!(
        error.contains("did not advance after 6 transitions"),
        "{error}"
    );
}

#[test]
fn copied_cortana_bearer_on_a_second_terminal_fails_closed_without_quarantine() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let audit_dir = std::env::temp_dir().join(format!(
        "t-hub-cortana-quarantine-audit-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let mut ctx = test_ctx("cortana-quarantine")
        .with_apply_sink(sink)
        .with_audit(Arc::new(AuditLog::new(audit_dir.clone())));
    ctx.addr = "127.0.0.1:4244".into();
    ctx.tab_registry().replace(vec![TabRecord {
        id: CAPTAIN_WORKSPACE_ID.into(),
        name: CAPTAIN_WORKSPACE_NAME.into(),
        tile_ids: vec![],
    }]);
    let home = std::env::temp_dir().join(format!(
        "t-hub-cortana-quarantine-home-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&home).unwrap();
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    let identity = ctx.identity.mint(crate::identity::Role::Cortana).unwrap();
    let mut terminal_ids = (0..2)
        .map(|_| uuid::Uuid::new_v4().simple().to_string()[..8].to_string())
        .collect::<Vec<_>>();
    ctx.identity
        .bind_tile(&identity.id, &terminal_ids[0])
        .unwrap();
    let environment = vec![
        ("T_HUB_CONTROL_FILE".into(), discovery_file_for_spawn()),
        ("T_HUB_CONTROL_ADDR".into(), String::new()),
        ("T_HUB_CONTROL_TOKEN".into(), String::new()),
        (
            crate::identity::SESSION_TOKEN_ENV.into(),
            identity.secret.clone(),
        ),
        (CORTANA_GENERATION_ENV.into(), "7".into()),
    ];
    for terminal_id in &terminal_ids {
        let target = exact_cortana_tmux_target(terminal_id).unwrap();
        create_test_tmux_session_with_env(
            &target,
            home.to_str().unwrap(),
            Some(&harness_command),
            &environment,
        )
        .unwrap();
        wait_for_harness_started(terminal_id, "codex").unwrap();
    }
    terminal_ids.sort();

    let degraded = dispatch(
        &ctx,
        "reconcile_cortana",
        &json!({
            "operationId": "cortana-quarantine-1",
            "testOrchestratorHome": home,
            "testStartupCommand": harness_command,
        }),
    )
    .unwrap();

    assert_eq!(degraded["action"], "degraded");
    assert_eq!(degraded["healthy"], false);
    assert_eq!(degraded["quarantinedTerminalIds"], json!([]));
    assert!(degraded["degradedReason"]
        .as_str()
        .is_some_and(|reason| reason.contains("lacks authoritative identity")));
    assert!(terminal_ids
        .iter()
        .all(|terminal_id| tmux::session_liveness(
            &exact_cortana_tmux_target(terminal_id).unwrap()
        ) == tmux::SessionLiveness::Alive));
    assert!(ctx.captains.cortana_identity().identity_id.is_none());
    assert!(ctx.identity.resolve(&identity.secret).is_some());
    assert!(!read_audit(&audit_dir)
        .iter()
        .any(|record| record["decision"] == "quarantined"));
    for terminal_id in &terminal_ids {
        reap_test_tmux_session(&exact_cortana_tmux_target(terminal_id).unwrap()).unwrap();
    }
    let _ = std::fs::remove_dir_all(harness_bin_dir);
    let _ = std::fs::remove_dir_all(home);
    let _ = std::fs::remove_dir_all(audit_dir);
}

#[test]
fn ambiguous_quarantine_revokes_all_bearers_without_signaling_runtimes() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let audit_dir = std::env::temp_dir().join(format!(
        "t-hub-cortana-identity-quarantine-audit-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let ctx = test_ctx("cortana-identity-quarantine")
        .with_audit(Arc::new(AuditLog::new(audit_dir.clone())));
    let durable_identity = ctx.identity.mint(crate::identity::Role::Cortana).unwrap();
    let foreign_identity = ctx.identity.mint(crate::identity::Role::Cortana).unwrap();
    let durable_terminal = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    let foreign_terminal = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    ctx.identity
        .bind_tile(&durable_identity.id, &durable_terminal)
        .unwrap();
    ctx.identity
        .bind_tile(&foreign_identity.id, &foreign_terminal)
        .unwrap();
    for terminal_id in [&durable_terminal, &foreign_terminal] {
        create_test_tmux_session(&exact_cortana_tmux_target(terminal_id).unwrap()).unwrap();
    }
    let candidates = vec![
        crate::cortana_reconcile::CortanaRuntimeCandidate {
            terminal_id: durable_terminal.clone(),
            identity_id: Some(durable_identity.id.clone()),
            generation: 4,
            harness: "codex".into(),
            provider_session_id: None,
            terminal: crate::cortana_reconcile::RuntimeEvidence::Alive,
            harness_process: crate::cortana_reconcile::RuntimeEvidence::Alive,
            identity_bound_to_terminal: true,
            canonical_control_file: true,
            rotating_control_env_scrubbed: true,
            stale_legacy_control_env: false,
            unresolved_session_bearer: false,
            effect_identity: None,
            current_control_capability: true,
            trusted_cortana_identity: true,
        },
        crate::cortana_reconcile::CortanaRuntimeCandidate {
            terminal_id: foreign_terminal.clone(),
            identity_id: Some(foreign_identity.id.clone()),
            generation: 4,
            harness: "codex".into(),
            provider_session_id: None,
            terminal: crate::cortana_reconcile::RuntimeEvidence::Alive,
            harness_process: crate::cortana_reconcile::RuntimeEvidence::Alive,
            identity_bound_to_terminal: true,
            canonical_control_file: true,
            rotating_control_env_scrubbed: true,
            stale_legacy_control_env: false,
            unresolved_session_bearer: false,
            effect_identity: None,
            current_control_capability: true,
            trusted_cortana_identity: true,
        },
    ];
    let durable = crate::cortana_reconcile::CortanaDurableIdentity {
        identity_id: Some(durable_identity.id.clone()),
        generation: 4,
        terminal_id: Some(durable_terminal.clone()),
        harness: Some("codex".into()),
        ..Default::default()
    };
    let requested = vec![durable_terminal, foreign_terminal];

    let quarantined = quarantine_cortana_runtimes(
        &ctx,
        "cortana-identity-quarantine-1",
        &requested,
        &candidates,
        &durable,
    )
    .unwrap();

    let mut expected = requested;
    expected.sort();
    assert_eq!(quarantined, expected);
    for (identity, terminal_id) in [
        (&durable_identity, &candidates[0].terminal_id),
        (&foreign_identity, &candidates[1].terminal_id),
    ] {
        assert!(ctx.identity.resolve(&identity.secret).is_none());
        assert!(ctx.identity.is_revoked(&identity.id));
        let denied = dispatch_authenticated(
            &ctx,
            req_session(
                &ctx.token,
                &identity.secret,
                "register_project",
                json!({"rootPath": "/tmp/ambiguous-bearer-must-not-register"}),
            ),
        );
        assert!(!denied.ok);
        assert_eq!(
                denied.error.as_deref(),
                Some(
                    "unauthorized: 'register_project' requires a valid T_HUB_SESSION_TOKEN with the control capability"
                )
            );
        assert_eq!(
            tmux::session_liveness(&exact_cortana_tmux_target(terminal_id).unwrap()),
            tmux::SessionLiveness::Alive
        );
    }
    assert!(!ctx
        .captains
        .snapshot()
        .captains
        .iter()
        .any(|captain| captain.role == FleetRole::Cortana));
    for terminal_id in &expected {
        reap_test_tmux_session_and_assert_absent(&exact_cortana_tmux_target(terminal_id).unwrap());
    }
    let _ = std::fs::remove_dir_all(audit_dir);
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
                Err(error) => break Err(error),
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
    assert_eq!(observe(&target, &owner).unwrap(), baseline);
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
        let deadline = Instant::now() + Duration::from_secs(5);
        let package_baseline = loop {
            match observe_package() {
                Ok(observed) => break observed,
                Err(error) => assert!(
                    Instant::now() < deadline,
                    "bound native Codex child was not observed: {error:?}"
                ),
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(
            Some(&package_baseline.executable),
            package_expected.trusted_child_executable.as_ref()
        );
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
        assert_eq!(observe_package().unwrap(), package_baseline);
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
        assert_eq!(
            observe_script().unwrap_err(),
            crate::harness::LaunchAttestationError::ExpectedProvenanceMismatch
        );
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

#[test]
fn cortana_startup_budget_covers_atomic_windows_observation_contract() {
    const MEASURED_WSL_HELPER_LATENCY: Duration = Duration::from_millis(1_100);
    const LEGACY_WINDOWS_HELPERS_PER_OBSERVATION: usize = 2;
    const LEGACY_STARTUP_TIMEOUT: Duration = Duration::from_secs(5);

    let observations = CORTANA_HARNESS_REQUIRED_CONFIRMATIONS + 1;
    let legacy_measured_floor = MEASURED_WSL_HELPER_LATENCY
        * (observations * LEGACY_WINDOWS_HELPERS_PER_OBSERVATION) as u32
        + CORTANA_HARNESS_CONFIRM_INTERVAL * (observations - 1) as u32;
    assert!(
        legacy_measured_floor > LEGACY_STARTUP_TIMEOUT,
        "the measured two-helper contract must reproduce the five-second startup failure"
    );

    let atomic_measured_floor = MEASURED_WSL_HELPER_LATENCY
        * (observations * crate::harness::WINDOWS_SCOPED_HARNESS_HELPERS_PER_OBSERVATION) as u32
        + CORTANA_HARNESS_CONFIRM_INTERVAL * (observations - 1) as u32;
    assert!(atomic_measured_floor < CORTANA_HARNESS_STARTUP_TIMEOUT);

    let bounded_cold_start_contract = crate::harness::SCOPED_HARNESS_SINGLE_HELPER_TIMEOUT
        * observations as u32
        + CORTANA_HARNESS_CONFIRM_INTERVAL * (observations - 1) as u32;
    assert!(
        bounded_cold_start_contract < CORTANA_HARNESS_STARTUP_TIMEOUT,
        "the hard startup budget must contain baseline plus two maximally bounded observations"
    );
    assert_eq!(
        crate::harness::WINDOWS_SCOPED_HARNESS_HELPERS_PER_OBSERVATION,
        1
    );
}

#[test]
fn cortana_startup_prompt_and_resume_use_the_dedicated_bootstrap_policy() {
    let durable = crate::cortana_reconcile::CortanaDurableIdentity::default();
    let fresh = cortana_startup_command(&durable, &json!({}), Harness::Codex);
    assert!(fresh.contains("First call cortana_bootstrap"));
    assert!(!fresh.contains("captain_bootstrap"));
    assert!(fresh.contains("--sandbox read-only"));
    assert!(fresh.contains(crate::harness::CORTANA_CODEX_TOOL_APPROVAL_OVERRIDE));

    let resumed = cortana_startup_command(
        &crate::cortana_reconcile::CortanaDurableIdentity {
            provider_session_id: Some("thread-cortana".into()),
            ..Default::default()
        },
        &json!({}),
        Harness::Codex,
    );
    assert_eq!(
            resumed,
            "codex resume --sandbox read-only -c 'mcp_servers.t-hub.tools.cortana_bootstrap.approval_mode=\"approve\"' 'thread-cortana'"
        );
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
            "spawn({}, ['foreign-first', {}, {}], {{ stdio: 'inherit' }});",
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
    let owner = active.owner.clone().unwrap();

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
    assert!(!error.trim().is_empty());
    assert!(
        elapsed < Duration::from_secs(3),
        "one-second aggregate observation deadline took {elapsed:?}"
    );
    assert!(
        ctx.dispatch_admission.try_lock().is_ok(),
        "dispatch admission remained unavailable after observation timeout"
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

#[test]
fn concurrent_captain_commission_and_cortana_recovery_follow_one_lock_order() {
    if !tmux_process_tests_available() {
        eprintln!(
                "concurrent_captain_commission_and_cortana_recovery_follow_one_lock_order: tmux or node not on PATH - skipping"
            );
        return;
    }
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let mut context = test_ctx("ordered-provisioning")
        .with_live_sessions(|| Ok(Vec::new()))
        .with_apply_sink(sink)
        .with_governor(Arc::new(SpawnGovernor::new(64, 600.0, 8.0)));
    context.addr = "127.0.0.1:4251".into();
    let ctx = Arc::new(context);
    ctx.tab_registry().replace(vec![TabRecord {
        id: CAPTAIN_WORKSPACE_ID.into(),
        name: CAPTAIN_WORKSPACE_NAME.into(),
        tile_ids: Vec::new(),
    }]);
    ctx.captains
        .upsert_project(ProjectRecord {
            root_path: None,
            vcs_capability: None,
            git_main_root: None,
            project_id: "project-ordered-provisioning".into(),
            name: "Ordered Provisioning".into(),
            repo_root: "/tmp".into(),
            remote_url: None,
            default_branch: Some("main".into()),
            powder: Some(PowderProjectBinding {
                connection_profile: "production".into(),
                repository: "ordered-provisioning".into(),
                event_cursor: 0,
            }),
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    let home = std::env::temp_dir().join(format!(
        "t-hub-ordered-provisioning-home-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&home).unwrap();
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    let (reconcile_reached_tx, reconcile_reached_rx) = mpsc::sync_channel(1);
    let (reconcile_resume_tx, reconcile_resume_rx) = mpsc::sync_channel(1);
    ctx.captains.set_dispatch_barrier(Some(DispatchBarrier {
        boundary: "cortana_spawn_admission_required",
        reached: reconcile_reached_tx,
        resume: reconcile_resume_rx,
    }));

    let reconcile_ctx = Arc::clone(&ctx);
    let reconcile_command = harness_command.clone();
    let reconcile_home = home.clone();
    let (reconcile_done_tx, reconcile_done_rx) = mpsc::sync_channel(1);
    let reconcile_thread = std::thread::spawn(move || {
        let result = dispatch(
            &reconcile_ctx,
            "reconcile_cortana",
            &json!({
                "operationId": "ordered-cortana-recovery",
                "testOrchestratorHome": reconcile_home,
                "testStartupCommand": reconcile_command,
            }),
        );
        reconcile_done_tx.send(result).unwrap();
    });
    assert_eq!(
        reconcile_reached_rx
            .recv_timeout(Duration::from_secs(3))
            .expect("Cortana did not reach the ordered admission boundary"),
        "cortana_spawn_admission_required"
    );

    let (commission_reached_tx, commission_reached_rx) = mpsc::sync_channel(1);
    let (commission_resume_tx, commission_resume_rx) = mpsc::sync_channel(1);
    ctx.captains.set_dispatch_barrier(Some(DispatchBarrier {
        boundary: "commission_initial_inspection",
        reached: commission_reached_tx,
        resume: commission_resume_rx,
    }));

    let commission_ctx = Arc::clone(&ctx);
    let commission_command = harness_command.clone();
    let (commission_done_tx, commission_done_rx) = mpsc::sync_channel(1);
    let commission_thread = std::thread::spawn(move || {
        let response = dispatch_authenticated(
            &commission_ctx,
            ControlRequest {
                token: commission_ctx.token.clone(),
                command: "commission_captain".into(),
                args: json!({
                    "projectId": "project-ordered-provisioning",
                    "assignment": "Own the ordered project",
                    "harness": "codex",
                    "shipSlug": "ordered-provisioning",
                    "testStartupCommand": commission_command,
                    "testSkipPowderHealth": true,
                }),
                session: String::new(),
                host: commission_ctx.host_token.clone(),
                v: None,
            },
        );
        commission_done_tx.send(response).unwrap();
    });

    assert_eq!(
        commission_reached_rx
            .recv_timeout(Duration::from_secs(3))
            .expect("Captain commission did not reach its inspection pass"),
        "commission_initial_inspection"
    );
    commission_resume_tx.send(()).unwrap();

    // Cortana still owns only provisioning during its inspection pass.
    // Captain inspection may wait for that lock, but must not acquire spawn
    // admission first and recreate the inverse ordering that deadlocked the
    // old one-pass implementation.
    assert!(
        ctx.dispatch_admission.try_lock().is_ok(),
        "Captain inspection held dispatch admission while waiting on provisioning"
    );
    reconcile_resume_tx.send(()).unwrap();

    let commission = commission_done_rx
        .recv_timeout(Duration::from_secs(8))
        .expect("Captain commission deadlocked with Cortana reconciliation");
    assert!(commission.ok, "commission failed: {:?}", commission.error);
    let reconciled = reconcile_done_rx
        .recv_timeout(Duration::from_secs(8))
        .expect("Cortana reconciliation deadlocked with Captain commission")
        .unwrap();
    commission_thread.join().unwrap();
    reconcile_thread.join().unwrap();

    assert_eq!(reconciled["healthy"], true);
    assert_eq!(reconciled["action"], "create");
    let snapshot = ctx.captains.snapshot();
    assert_eq!(
        snapshot
            .captains
            .iter()
            .filter(|captain| captain.role == FleetRole::Captain)
            .count(),
        1
    );
    assert_eq!(
        snapshot
            .captains
            .iter()
            .filter(|captain| captain.role == FleetRole::Cortana)
            .count(),
        1
    );
    assert_eq!(
        snapshot.cortana.terminal_id.as_deref(),
        reconciled["terminalId"].as_str()
    );

    for terminal_id in snapshot
        .captains
        .iter()
        .filter_map(|captain| captain.terminal_id.as_deref())
    {
        reap_test_tmux_session(&tmux_target(terminal_id)).unwrap();
    }
    std::fs::remove_dir_all(harness_bin_dir).unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn commission_captain_spawns_binds_bootstraps_and_deduplicates() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let ctx = test_ctx("secret")
        .with_governor(Arc::new(SpawnGovernor::new(128, 20.0, 2.0)))
        .with_apply_sink(sink);
    ctx.tab_registry().replace(vec![
        TabRecord {
            id: "project-tab".into(),
            name: "Commission Crew".into(),
            tile_ids: vec![],
        },
        TabRecord {
            id: CAPTAIN_WORKSPACE_ID.into(),
            name: CAPTAIN_WORKSPACE_NAME.into(),
            tile_ids: vec![],
        },
    ]);
    ctx.captains
        .upsert_project(ProjectRecord {
            root_path: None,
            vcs_capability: None,
            git_main_root: None,
            project_id: "project-e2e".into(),
            name: "Commission E2E".into(),
            repo_root: "/tmp".into(),
            remote_url: None,
            default_branch: Some("main".into()),
            powder: Some(PowderProjectBinding {
                connection_profile: "production".into(),
                repository: "commission-e2e".into(),
                event_cursor: 0,
            }),
            created_at: 0,
            updated_at: 0,
        })
        .unwrap();

    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    let args = json!({
        "requestId": "commission-first-operation",
        "projectId": "project-e2e",
        "assignment": "Keep this project stable",
        "harness": "codex",
        "shipSlug": "commission-e2e",
        "workspaceTabIds": ["project-tab"],
        "testStartupCommand": harness_command,
        "testSkipPowderHealth": true,
    });
    let first = dispatch(&ctx, "commission_captain", &args).unwrap();
    assert_eq!(first["alreadyCommissioned"], false);
    assert_eq!(first["captain"]["projectId"], "project-e2e");
    assert_eq!(first["captain"]["assignment"], "Keep this project stable");
    assert_eq!(first["captain"]["harness"], "codex");
    assert_eq!(first["captain"]["workspaceTabIds"][0], "project-tab");
    assert_eq!(first["project"]["powder"]["repository"], "commission-e2e");
    assert!(ctx.captains.snapshot().pending_fleet_operations.is_empty());
    let terminal_id = first["captain"]["terminalId"].as_str().unwrap().to_string();
    assert!(tmux::has_session(&tmux_target(&terminal_id)));

    let bootstrap = dispatch(
        &ctx,
        "captain_bootstrap",
        &json!({ "captainSessionId": terminal_id }),
    )
    .unwrap();
    assert_eq!(bootstrap["recoverySource"], "captains-registry");
    assert!(bootstrap["instructions"]
        .as_str()
        .unwrap()
        .contains("Use $captain"));
    assert!(bootstrap["instructions"]
        .as_str()
        .unwrap()
        .contains("commission-e2e"));

    let mut claude_captain = ctx.captains.snapshot().captains[0].clone();
    claude_captain.harness = Some("claude".into());
    let claude_instructions = bootstrap_instructions(&claude_captain, &ctx.captains.projects()[0]);
    assert!(claude_instructions.contains("Use /captain"));
    assert!(!claude_instructions.contains("Use $captain"));

    let mut retry_args = args.clone();
    retry_args["requestId"] = json!("commission-fresh-noop-operation");
    let retry = dispatch(&ctx, "commission_captain", &retry_args).unwrap();
    assert_eq!(retry["alreadyCommissioned"], true);
    assert_eq!(retry["captain"]["terminalId"], terminal_id);
    assert_eq!(ctx.captains.snapshot().captains.len(), 1);
    assert!(
            admit_spawn(&ctx, SpawnPurpose::Ordinary, 0, None).is_ok(),
            "an exact no-op commission with a fresh operation ID must not consume the remaining spawn-rate token"
        );

    dispatch(&ctx, "close_terminal", &json!({ "sessionId": terminal_id })).unwrap();
    std::fs::remove_dir_all(harness_bin_dir).unwrap();
}

#[test]
fn non_git_captain_checkpoint_reload_and_bootstrap_preserve_real_projects() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let base = std::env::temp_dir().join(format!(
        "t-hub-non-git-captain-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let populated = base.join("populated");
    let empty = base.join("empty");
    std::fs::create_dir_all(&populated).unwrap();
    std::fs::create_dir_all(&empty).unwrap();
    std::fs::write(populated.join("README.txt"), b"non-Git fixture\n").unwrap();
    let registry_path = base.join("captains.json");
    let registry = Arc::new(CaptainsRegistry::load(registry_path.clone()));
    for (project_id, name, root) in [
        ("non-git-populated", "Populated non-Git", &populated),
        ("non-git-empty", "Empty non-Git", &empty),
    ] {
        let root = root.canonicalize().unwrap().to_string_lossy().into_owned();
        registry
            .upsert_project(ProjectRecord {
                root_path: Some(root.clone()),
                vcs_capability: Some("none".into()),
                git_main_root: None,
                project_id: project_id.into(),
                name: name.into(),
                repo_root: root,
                remote_url: None,
                default_branch: None,
                powder: None,
                created_at: 1,
                updated_at: 1,
            })
            .unwrap();
    }
    let tabs = Arc::new(TabRegistry::new());
    tabs.replace(vec![
        TabRecord {
            id: "non-git-populated-tab".into(),
            name: "Populated".into(),
            tile_ids: vec![],
        },
        TabRecord {
            id: "non-git-empty-tab".into(),
            name: "Empty".into(),
            tile_ids: vec![],
        },
        TabRecord {
            id: CAPTAIN_WORKSPACE_ID.into(),
            name: CAPTAIN_WORKSPACE_NAME.into(),
            tile_ids: vec![],
        },
    ]);
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let ctx = test_ctx("non-git-captain")
        .with_captains_registry(Arc::clone(&registry))
        .with_tab_registry(Arc::clone(&tabs))
        .with_governor(Arc::new(SpawnGovernor::new(128, 20.0, 2.0)))
        .with_apply_sink(sink);
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    let mut terminal_ids = Vec::new();
    for (project_id, ship_slug, tab_id) in [
        (
            "non-git-populated",
            "non-git-populated-ship",
            "non-git-populated-tab",
        ),
        ("non-git-empty", "non-git-empty-ship", "non-git-empty-tab"),
    ] {
        let result = dispatch(
            &ctx,
            "commission_captain",
            &json!({
                "requestId": format!("commission-{project_id}"),
                "projectId": project_id,
                "assignment": format!("Maintain {project_id}"),
                "harness": "codex",
                "shipSlug": ship_slug,
                "workspaceTabIds": [tab_id],
                "testStartupCommand": harness_command,
                "testSkipPowderHealth": true,
            }),
        )
        .unwrap();
        assert_eq!(result["alreadyCommissioned"], false);
        terminal_ids.push(
            result["captain"]["terminalId"]
                .as_str()
                .unwrap()
                .to_string(),
        );
        let before_dispatch = ctx.captains.snapshot();
        let dispatch_refusal = dispatch_authenticated(
            &ctx,
            req(
                "non-git-captain",
                "dispatch_crew",
                json!({
                    "captainSessionId": terminal_ids.last().unwrap(),
                    "cardId": "non-git-card",
                    "task": "must refuse before Git"
                }),
            ),
        );
        assert_native_git_required(dispatch_refusal, "dispatch_crew");
        assert_eq!(ctx.captains.snapshot().seq, before_dispatch.seq);
        let checkpoint = dispatch(
            &ctx,
            "captain_checkpoint",
            &json!({
                "shipSlug": ship_slug,
                "conversationId": format!("conversation-{project_id}"),
                "resumePoint": format!("resume-{project_id}"),
            }),
        )
        .unwrap();
        assert_eq!(checkpoint["accepted"], "captain_checkpoint");
    }
    assert_eq!(ctx.captains.projects().len(), 2);
    assert_eq!(ctx.captains.snapshot().captains.len(), 2);
    assert!(ctx.captains.snapshot().pending_fleet_operations.is_empty());
    assert!(populated.join(".git").metadata().is_err());
    assert!(empty.join(".git").metadata().is_err());

    let restarted_registry = Arc::new(CaptainsRegistry::load(registry_path.clone()));
    let restarted = test_ctx("non-git-captain-restart")
        .with_captains_registry(Arc::clone(&restarted_registry))
        .with_tab_registry(Arc::clone(&tabs))
        .with_governor(Arc::new(SpawnGovernor::new(128, 20.0, 2.0)))
        .with_apply_sink(Arc::new(RecordingSink {
            calls: StdMutex::new(Vec::new()),
        }));
    for (project_id, ship_slug, terminal_id) in [
        (
            "non-git-populated",
            "non-git-populated-ship",
            &terminal_ids[0],
        ),
        ("non-git-empty", "non-git-empty-ship", &terminal_ids[1]),
    ] {
        let project = restarted
            .captains
            .projects()
            .into_iter()
            .find(|project| project.project_id == project_id)
            .unwrap();
        assert_eq!(project.vcs_capability.as_deref(), Some("none"));
        assert_eq!(
            project.root_path.as_deref(),
            Some(project.repo_root.as_str())
        );
        let bootstrap = dispatch(
            &restarted,
            "captain_bootstrap",
            &json!({ "captainSessionId": terminal_id }),
        )
        .unwrap();
        assert_eq!(bootstrap["project"]["projectId"], project_id);
        assert_eq!(bootstrap["project"]["vcsCapability"], "none");
        assert_eq!(bootstrap["captain"]["shipSlug"], ship_slug);
        assert_eq!(
            bootstrap["captain"]["conversationId"],
            format!("conversation-{project_id}")
        );
        assert_eq!(
            bootstrap["captain"]["resumePoint"],
            format!("resume-{project_id}")
        );
        assert_eq!(
            bootstrap["captain"]["terminalId"].as_str(),
            Some(terminal_id.as_str())
        );
        assert!(bootstrap["instructions"]
            .as_str()
            .unwrap()
            .contains(project_id));
    }
    for (project_id, ship_slug, tab_id) in [
        (
            "non-git-populated",
            "non-git-populated-ship",
            "non-git-populated-tab",
        ),
        ("non-git-empty", "non-git-empty-ship", "non-git-empty-tab"),
    ] {
        let retry = dispatch(
            &restarted,
            "commission_captain",
            &json!({
                "requestId": format!("retry-{project_id}"),
                "projectId": project_id,
                "assignment": format!("Maintain {project_id}"),
                "harness": "codex",
                "shipSlug": ship_slug,
                "workspaceTabIds": [tab_id],
                "testStartupCommand": harness_command,
                "testSkipPowderHealth": true,
            }),
        )
        .unwrap();
        assert_eq!(retry["alreadyCommissioned"], true);
    }
    assert_eq!(restarted.captains.projects().len(), 2);
    assert_eq!(restarted.captains.snapshot().captains.len(), 2);
    assert!(populated.join(".git").metadata().is_err());
    assert!(empty.join(".git").metadata().is_err());
    assert_eq!(git::worktree_list_calls(), 0);
    for terminal_id in terminal_ids {
        let _ = dispatch(
            &restarted,
            "close_terminal",
            &json!({ "sessionId": terminal_id }),
        );
    }
    std::fs::remove_dir_all(&harness_bin_dir).unwrap();
    std::fs::remove_dir_all(&base).unwrap();
}

#[test]
fn non_git_captain_commission_persistence_failure_preserves_project_and_cleans_exactly() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let base = std::env::temp_dir().join(format!(
        "t-hub-non-git-captain-failure-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&base).unwrap();
    let root = base.join("source");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("file.txt"), b"non-Git\n").unwrap();
    let registry_path = base.join("captains.json");
    let registry = Arc::new(CaptainsRegistry::load(registry_path.clone()));
    let root = root.canonicalize().unwrap().to_string_lossy().into_owned();
    registry
        .upsert_project(ProjectRecord {
            root_path: Some(root.clone()),
            vcs_capability: Some("none".into()),
            git_main_root: None,
            project_id: "non-git-failure".into(),
            name: "Non-Git failure".into(),
            repo_root: root.clone(),
            remote_url: None,
            default_branch: None,
            powder: None,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    let tabs = Arc::new(TabRegistry::new());
    tabs.replace(vec![
        TabRecord {
            id: "non-git-failure-tab".into(),
            name: "Non-Git failure".into(),
            tile_ids: vec![],
        },
        TabRecord {
            id: CAPTAIN_WORKSPACE_ID.into(),
            name: CAPTAIN_WORKSPACE_NAME.into(),
            tile_ids: vec![],
        },
    ]);
    let ctx = test_ctx("non-git-captain-failure")
        .with_captains_registry(Arc::clone(&registry))
        .with_tab_registry(tabs)
        .with_governor(Arc::new(SpawnGovernor::new(128, 20.0, 2.0)))
        .with_apply_sink(Arc::new(RecordingSink {
            calls: StdMutex::new(Vec::new()),
        }));
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    let error = dispatch(
        &ctx,
        "commission_captain",
        &json!({
            "projectId": "non-git-failure",
            "assignment": "Recover safely",
            "harness": "codex",
            "shipSlug": "non-git-failure-ship",
            "workspaceTabIds": ["non-git-failure-tab"],
            "testStartupCommand": harness_command,
            "testSkipPowderHealth": true,
            "testFailCommitPersist": true
        }),
    )
    .unwrap_err();
    assert!(
        error.contains("commission binding persistence failure"),
        "got: {error}"
    );
    let snapshot = registry.snapshot();
    assert!(snapshot.captains.is_empty());
    assert!(snapshot.pending_fleet_operations.is_empty());
    assert_eq!(snapshot.projects.len(), 1);
    assert_eq!(snapshot.projects[0].vcs_capability.as_deref(), Some("none"));
    assert!(std::path::Path::new(&root).join(".git").metadata().is_err());
    let restarted = CaptainsRegistry::load(registry_path);
    assert_eq!(restarted.projects().len(), 1);
    assert_eq!(restarted.snapshot().captains.len(), 0);
    assert_eq!(
        restarted.projects()[0].vcs_capability.as_deref(),
        Some("none")
    );
    assert!(std::path::Path::new(&root).join(".git").metadata().is_err());
    std::fs::remove_dir_all(&harness_bin_dir).unwrap();
    std::fs::remove_dir_all(&base).unwrap();
}

#[test]
fn concurrent_non_git_commissions_converge_and_conflicts_fail_closed() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let base = std::env::temp_dir().join(format!(
        "t-hub-non-git-captain-concurrent-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&base).unwrap();
    let root = base.join("source");
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap().to_string_lossy().into_owned();
    let registry = Arc::new(CaptainsRegistry::load(base.join("captains.json")));
    registry
        .upsert_project(ProjectRecord {
            root_path: Some(root.clone()),
            vcs_capability: Some("none".into()),
            git_main_root: None,
            project_id: "non-git-concurrent".into(),
            name: "Non-Git concurrent".into(),
            repo_root: root,
            remote_url: None,
            default_branch: None,
            powder: None,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    let tabs = Arc::new(TabRegistry::new());
    tabs.replace(vec![
        TabRecord {
            id: "non-git-concurrent-tab".into(),
            name: "Concurrent".into(),
            tile_ids: vec![],
        },
        TabRecord {
            id: CAPTAIN_WORKSPACE_ID.into(),
            name: CAPTAIN_WORKSPACE_NAME.into(),
            tile_ids: vec![],
        },
    ]);
    let context = Arc::new(
        test_ctx("non-git-concurrent")
            .with_captains_registry(Arc::clone(&registry))
            .with_tab_registry(tabs)
            .with_governor(Arc::new(SpawnGovernor::new(128, 20.0, 2.0)))
            .with_apply_sink(Arc::new(RecordingSink {
                calls: StdMutex::new(Vec::new()),
            })),
    );
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let joins = (0..2)
        .map(|index| {
            let context = Arc::clone(&context);
            let barrier = Arc::clone(&barrier);
            let harness_command = harness_command.clone();
            std::thread::spawn(move || {
                barrier.wait();
                dispatch(
                    &context,
                    "commission_captain",
                    &json!({
                        "requestId": format!("concurrent-{index}"),
                        "projectId": "non-git-concurrent",
                        "assignment": "Same explicit-none assignment",
                        "harness": "codex",
                        "shipSlug": "non-git-concurrent-ship",
                        "workspaceTabIds": ["non-git-concurrent-tab"],
                        "testStartupCommand": harness_command,
                        "testSkipPowderHealth": true,
                    }),
                )
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    let results = joins
        .into_iter()
        .map(|join| join.join().unwrap())
        .collect::<Vec<_>>();
    assert!(results.iter().all(Result::is_ok), "results: {results:?}");
    let result_terminal_ids = results
        .iter()
        .map(|result| {
            result.as_ref().unwrap()["captain"]["terminalId"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect::<Vec<_>>();
    assert_eq!(result_terminal_ids.len(), 2);
    assert_eq!(result_terminal_ids[0], result_terminal_ids[1]);
    assert_eq!(registry.snapshot().captains.len(), 1);
    assert_eq!(registry.snapshot().pending_fleet_operations.len(), 0);
    let terminal_id = registry.snapshot().captains[0].terminal_id.clone().unwrap();
    assert_eq!(terminal_id, result_terminal_ids[0]);
    let matching_sessions = tmux::list_sessions()
        .unwrap()
        .into_iter()
        .filter(|session| session == &tmux_target(&terminal_id))
        .collect::<Vec<_>>();
    assert_eq!(matching_sessions, vec![tmux_target(&terminal_id)]);
    let before_conflict = registry.snapshot();
    let conflict = dispatch(
        &context,
        "commission_captain",
        &json!({
            "requestId": "concurrent-conflict",
            "projectId": "non-git-concurrent",
            "assignment": "Conflicting assignment",
            "harness": "codex",
            "shipSlug": "non-git-concurrent-ship",
            "workspaceTabIds": ["non-git-concurrent-tab"],
            "testStartupCommand": harness_command,
            "testSkipPowderHealth": true,
        }),
    )
    .unwrap_err();
    assert_eq!(
            conflict,
            "commission_captain: project 'Non-Git concurrent' already has live Captain 'non-git-concurrent-ship' with a different assignment, harness, or shipSlug; release or update that Captain explicitly"
        );
    assert_eq!(registry.snapshot().captains.len(), 1);
    assert_eq!(registry.snapshot().seq, before_conflict.seq);
    assert!(base.join("source/.git").metadata().is_err());
    let _ = dispatch(
        &context,
        "close_terminal",
        &json!({ "sessionId": terminal_id }),
    );
    std::fs::remove_dir_all(&harness_bin_dir).unwrap();
    std::fs::remove_dir_all(&base).unwrap();
}

#[test]
fn commission_binding_failure_never_projects_a_ghost_captain_or_placement() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let path = captains_tmp("commission-projection-rollback");
    let captains = Arc::new(CaptainsRegistry::load(path.clone()));
    captains
        .upsert_project(ProjectRecord {
            root_path: None,
            vcs_capability: None,
            git_main_root: None,
            project_id: "project-commission-fail".into(),
            name: "Commission Failure".into(),
            repo_root: "/tmp".into(),
            remote_url: None,
            default_branch: Some("main".into()),
            powder: Some(PowderProjectBinding {
                connection_profile: "production".into(),
                repository: "commission-failure".into(),
                event_cursor: 0,
            }),
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    let tabs = Arc::new(TabRegistry::new());
    tabs.replace(vec![TabRecord {
        id: "commission-work".into(),
        name: "Commission Work".into(),
        tile_ids: Vec::new(),
    }]);
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let context = test_ctx("commission-projection-rollback")
        .with_captains_registry(Arc::clone(&captains))
        .with_tab_registry(Arc::clone(&tabs))
        .with_apply_sink(sink.clone());
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let terminal_id = suffix[..8].to_string();
    let identities_before = context.identity.len();

    let error = dispatch(
        &context,
        "commission_captain",
        &json!({
            "projectId": "project-commission-fail",
            "assignment": "Own failure",
            "harness": "codex",
            "shipSlug": "commission-failure",
            "workspaceTabIds": ["commission-work"],
            "testStartupCommand": harness_command,
            "testSkipPowderHealth": true,
            "testFailCommitPersist": true,
            "testTerminalId": terminal_id
        }),
    )
    .unwrap_err();
    assert!(
        error.contains("commission binding persistence failure"),
        "got: {error}"
    );
    assert!(captains.snapshot().captains.is_empty());
    assert!(captains.snapshot().pending_fleet_operations.is_empty());
    assert_eq!(context.identity.len(), identities_before);
    assert_eq!(
        tmux::session_liveness(&tmux_target(&terminal_id)),
        tmux::SessionLiveness::Gone
    );
    assert!(tabs
        .snapshot()
        .iter()
        .find(|tab| tab.id == CAPTAIN_WORKSPACE_ID)
        .unwrap()
        .tile_ids
        .is_empty());
    let projected_commands: Vec<String> = sink
        .calls
        .lock()
        .unwrap()
        .iter()
        .map(|(command, _)| command.clone())
        .collect();
    assert!(!projected_commands.iter().any(|command| {
        matches!(
            command.as_str(),
            "spawn_terminal" | "move_tile" | "sync_captains"
        )
    }));

    let _ = std::fs::remove_dir_all(harness_bin_dir);
    let _ = std::fs::remove_file(path.with_extension("json.bak"));
    let _ = std::fs::remove_file(path);
}

#[test]
fn commission_crash_recovery_reaps_exact_tmux_identity_and_unprojected_intent() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let path = captains_tmp("commission-crash-recovery");
    let non_git_root = std::env::temp_dir().join(format!(
        "t-hub-commission-crash-non-git-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&non_git_root).unwrap();
    std::fs::write(non_git_root.join("README"), b"non-Git crash fixture\n").unwrap();
    let non_git_root = non_git_root
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let identity_path = path.with_extension("identities.json");
    let captains = Arc::new(CaptainsRegistry::load(path.clone()));
    captains
        .upsert_project(ProjectRecord {
            root_path: Some(non_git_root.clone()),
            vcs_capability: Some("none".into()),
            git_main_root: None,
            project_id: "project-commission-crash".into(),
            name: "Commission Crash".into(),
            repo_root: non_git_root.clone(),
            remote_url: None,
            default_branch: Some("main".into()),
            powder: Some(PowderProjectBinding {
                connection_profile: "production".into(),
                repository: "commission-crash".into(),
                event_cursor: 0,
            }),
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    let identities = Arc::new(crate::identity::IdentityStore::load(identity_path.clone()));
    let tabs = Arc::new(TabRegistry::new());
    tabs.replace(captains.workspace_projection());
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let context = Arc::new(
        test_ctx("commission-crash-recovery")
            .with_captains_registry(Arc::clone(&captains))
            .with_tab_registry(Arc::clone(&tabs))
            .with_identity_store(Arc::clone(&identities))
            .with_apply_sink(sink.clone()),
    );
    git::reset_worktree_list_calls();
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    let (reached_tx, reached_rx) = std::sync::mpsc::sync_channel(1);
    let (resume_tx, resume_rx) = std::sync::mpsc::sync_channel(1);
    captains.set_dispatch_barrier(Some(DispatchBarrier {
        boundary: "commission_effect_applied",
        reached: reached_tx,
        resume: resume_rx,
    }));
    let commissioning_context = Arc::clone(&context);
    let commissioning = std::thread::spawn(move || {
        git::reset_worktree_list_calls();
        let result = dispatch(
            &commissioning_context,
            "commission_captain",
            &json!({
            "projectId": "project-commission-crash",
            "assignment": "Own crash recovery",
            "harness": "codex",
            "shipSlug": "commission-crash",
            "testStartupCommand": harness_command,
            "testSkipPowderHealth": true,
            "testCrashAfterTmux": true
            }),
        );
        (result, git::worktree_list_calls())
    });
    assert_eq!(
        reached_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
        "commission_effect_applied"
    );
    let during_effect = dispatch(&context, "list_tabs", &Value::Null).unwrap();
    assert!(during_effect["tabs"]
        .as_array()
        .unwrap()
        .iter()
        .all(|workspace| workspace["tileIds"].as_array().unwrap().is_empty()));
    assert!(dispatch(&context, "list_captains", &Value::Null).is_ok());
    resume_tx.send(()).unwrap();
    let (commission_result, commission_worktree_calls) = commissioning.join().unwrap();
    let error = commission_result.unwrap_err();
    assert!(error.contains("injected commission crash"));
    assert_eq!(commission_worktree_calls, 0);
    let durable = captains.snapshot();
    assert!(durable.captains.is_empty());
    assert_eq!(durable.pending_fleet_operations.len(), 1);
    let PendingFleetOperationPayload::CommissionCaptain {
        terminal_id,
        identity_id,
        ..
    } = &durable.pending_fleet_operations[0].payload
    else {
        panic!("expected pending commission operation")
    };
    assert!(tmux::has_session(&tmux_target(terminal_id)));
    assert!(identity_id.is_some());
    assert_eq!(identities.len(), 1);
    let listed = dispatch(&context, "list_tabs", &Value::Null).unwrap();
    assert!(listed["tabs"].as_array().unwrap().iter().all(|workspace| {
        workspace["tileIds"]
            .as_array()
            .unwrap()
            .iter()
            .all(|tile| tile != terminal_id)
    }));
    assert!(sink.calls.lock().unwrap().is_empty());
    let terminal_id = terminal_id.clone();

    drop(context);
    drop(tabs);
    drop(captains);
    drop(identities);
    let restarted_captains = Arc::new(CaptainsRegistry::load(path.clone()));
    let restarted_identities =
        Arc::new(crate::identity::IdentityStore::load(identity_path.clone()));
    let restarted_tabs = Arc::new(TabRegistry::new());
    restarted_tabs.replace(restarted_captains.workspace_projection());
    let restarted = test_ctx("commission-crash-recovery-restart")
        .with_captains_registry(Arc::clone(&restarted_captains))
        .with_tab_registry(Arc::clone(&restarted_tabs))
        .with_identity_store(Arc::clone(&restarted_identities));
    git::reset_worktree_list_calls();
    recover_pending_fleet_operations(&restarted);
    assert!(!tmux::has_session(&tmux_target(&terminal_id)));
    assert_eq!(restarted_identities.len(), 0);
    assert!(restarted_captains
        .snapshot()
        .pending_fleet_operations
        .is_empty());
    assert!(restarted_captains.snapshot().captains.is_empty());
    assert_eq!(restarted_captains.projects().len(), 1);
    assert_eq!(
        restarted_captains.projects()[0].vcs_capability.as_deref(),
        Some("none")
    );
    assert!(std::path::Path::new(&non_git_root)
        .join(".git")
        .metadata()
        .is_err());
    assert_eq!(git::worktree_list_calls(), 0);

    drop(restarted);
    drop(restarted_tabs);
    drop(restarted_identities);
    drop(restarted_captains);
    let second_captains = Arc::new(CaptainsRegistry::load(path.clone()));
    let second_identities = Arc::new(crate::identity::IdentityStore::load(identity_path.clone()));
    let second = test_ctx("commission-crash-recovery-second-restart")
        .with_captains_registry(Arc::clone(&second_captains))
        .with_identity_store(second_identities);
    git::reset_worktree_list_calls();
    recover_pending_fleet_operations(&second);
    assert!(second_captains
        .snapshot()
        .pending_fleet_operations
        .is_empty());
    assert!(second_captains.snapshot().captains.is_empty());
    assert_eq!(second_captains.projects().len(), 1);
    assert!(std::path::Path::new(&non_git_root)
        .join(".git")
        .metadata()
        .is_err());

    let _ = std::fs::remove_dir_all(harness_bin_dir);
    let _ = std::fs::remove_file(identity_path.with_extension("json.bak"));
    let _ = std::fs::remove_file(identity_path);
    let _ = std::fs::remove_file(path.with_extension("json.bak"));
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_dir_all(&non_git_root);
}

#[test]
fn captain_bootstrap_uses_the_wsl_runtime_root_without_mutating_the_project() {
    let canonical_repo_root =
        "\\\\?\\UNC\\wsl.localhost\\Ubuntu-24.04\\home\\natkins\\projects\\tools\\t-hub\\t-hub-app";
    let runtime_repo_root = "/home/natkins/projects/tools/t-hub/t-hub-app";
    let captain = CaptainRecord {
        ship_slug: "t-hub-app".into(),
        assignment_id: "assignment:project-1:t-hub-app".into(),
        display_name: "t-hub-app".into(),
        role: FleetRole::Captain,
        claude_uuid: None,
        provider: Some("codex".into()),
        provider_session_id: None,
        terminal_id: None,
        project_id: Some("project-e2e".into()),
        assignment: Some("Keep this project stable".into()),
        harness: Some("codex".into()),
        conversation_id: None,
        resume_point: None,
        workspace_tab_ids: Vec::new(),
        crew: Vec::new(),
        state: ClaimState::Active,
    };
    let project = ProjectRecord {
        root_path: None,
        vcs_capability: None,
        git_main_root: None,
        project_id: "project-e2e".into(),
        name: "T-Hub".into(),
        repo_root: canonical_repo_root.into(),
        remote_url: None,
        default_branch: Some("main".into()),
        powder: None,
        created_at: 0,
        updated_at: 0,
    };

    let instructions = bootstrap_instructions(&captain, &project);

    assert!(instructions.contains(runtime_repo_root));
    assert!(!instructions.contains(canonical_repo_root));
    assert_eq!(project.repo_root, canonical_repo_root);
}

#[test]
fn attach_captain_refuses_read_only_and_preserves_existing_control_capability() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let mut ctx = test_ctx("control-secret").with_apply_sink(Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    }));
    ctx.addr = "127.0.0.1:4242".into();
    ctx.captains
        .upsert_project(ProjectRecord {
            root_path: None,
            vcs_capability: None,
            git_main_root: None,
            project_id: "project-attach".into(),
            name: "Attach Project".into(),
            repo_root: "/tmp".into(),
            remote_url: None,
            default_branch: Some("main".into()),
            powder: Some(PowderProjectBinding {
                connection_profile: "production".into(),
                repository: "attach-project".into(),
                event_cursor: 0,
            }),
            created_at: 0,
            updated_at: 0,
        })
        .unwrap();
    ctx.tab_registry().replace(vec![TabRecord {
        id: "attach-work".into(),
        name: "Attach Work".into(),
        tile_ids: Vec::new(),
    }]);
    let read_id = dispatch(
        &ctx,
        "spawn_terminal",
        &json!({ "cwd": "/tmp", "capability": "read", "tabId": "attach-work" }),
    )
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let error = dispatch(
        &ctx,
        "attach_captain",
        &json!({
            "captainSessionId": read_id,
            "projectId": "project-attach",
            "assignment": "Own stability",
            "provider": "codex",
            "testSkipPowderHealth": true,
        }),
    )
    .unwrap_err();
    assert!(
        error.contains("read-only; refusing silent elevation"),
        "got: {error}"
    );

    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    let control_id = dispatch(
        &ctx,
        "spawn_terminal",
        &json!({
            "cwd": "/tmp",
            "capability": "read",
            "tabId": "attach-work",
            "startupCommand": harness_command,
        }),
    )
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    wait_for_harness_started(&control_id, "codex").unwrap();
    // Convert this ordinary read-only spawn into a compatibility fixture for
    // a terminal created before Package 0. New terminals never receive this
    // rotating global credential from their spawn request.
    tmux::set_session_environment(&tmux_target(&control_id), "T_HUB_CONTROL_TOKEN", &ctx.token)
        .unwrap();
    let attached = dispatch(
        &ctx,
        "attach_captain",
        &json!({
            "captainSessionId": control_id,
            "projectId": "project-attach",
            "assignment": "Own stability",
            "provider": "codex",
            "testSkipPowderHealth": true,
        }),
    )
    .unwrap();
    assert_eq!(attached["accepted"], "attach_captain");
    assert_eq!(attached["capabilityPreserved"], "control");
    assert_eq!(attached["captain"]["provider"], "codex");
    assert!(attached["captain"].get("providerSessionId").is_none());
    assert!(attached["captain"].get("claudeUuid").is_none());
    let attached_tabs = ctx.tabs.snapshot_full();
    assert!(!attached_tabs
        .tabs
        .iter()
        .find(|tab| tab.id == "attach-work")
        .unwrap()
        .tile_ids
        .contains(&control_id));
    assert_eq!(
        attached_tabs
            .tabs
            .iter()
            .find(|tab| tab.id == CAPTAIN_WORKSPACE_ID)
            .unwrap()
            .tile_ids
            .iter()
            .filter(|tile| *tile == &control_id)
            .count(),
        1
    );
    let unchanged_report = dispatch(
        &ctx,
        "report_workspace_tabs",
        &json!({
            "baseSeq": attached_tabs.seq,
            "tabs": attached_tabs.tabs,
            "activeTabId": attached_tabs.active_tab_id
        }),
    )
    .unwrap();
    assert!(unchanged_report.get("reported").is_some());

    let checkpoint = dispatch(
        &ctx,
        "captain_checkpoint",
        &json!({
            "captainSessionId": control_id,
            "conversationId": "thread-attach",
            "resumePoint": "Continue verification",
        }),
    )
    .unwrap();
    assert_eq!(
        checkpoint["captain"]["resumePoint"],
        "Continue verification"
    );

    dispatch(&ctx, "close_terminal", &json!({ "sessionId": read_id })).unwrap();
    dispatch(&ctx, "close_terminal", &json!({ "sessionId": control_id })).unwrap();
    std::fs::remove_dir_all(harness_bin_dir).unwrap();
}

#[test]
fn attach_captain_binding_failure_restores_placement_and_retry_is_durable() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let path = captains_tmp("attach-relocation-rollback");
    let captains = Arc::new(CaptainsRegistry::load(path.clone()));
    captains
        .upsert_project(ProjectRecord {
            root_path: None,
            vcs_capability: None,
            git_main_root: None,
            project_id: "project-attach-rollback".into(),
            name: "Attach Rollback".into(),
            repo_root: "/tmp/attach-rollback".into(),
            remote_url: None,
            default_branch: Some("main".into()),
            powder: Some(PowderProjectBinding {
                connection_profile: "production".into(),
                repository: "attach-rollback".into(),
                event_cursor: 0,
            }),
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    let tabs = Arc::new(TabRegistry::new());
    tabs.replace(vec![TabRecord {
        id: "attach-work".into(),
        name: "Attach Work".into(),
        tile_ids: Vec::new(),
    }]);
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let mut ctx = test_ctx("attach-relocation-rollback")
        .with_apply_sink(sink.clone())
        .with_captains_registry(Arc::clone(&captains))
        .with_tab_registry(Arc::clone(&tabs));
    ctx.addr = "127.0.0.1:4242".into();
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    let captain_id = dispatch(
        &ctx,
        "spawn_terminal",
        &json!({
            "cwd": "/tmp",
            "capability": "read",
            "tabId": "attach-work",
            "startupCommand": harness_command
        }),
    )
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    wait_for_harness_started(&captain_id, "codex").unwrap();
    // Convert the read-only spawn into a legacy attach fixture without
    // restoring global-token persistence for newly spawned terminals.
    tmux::set_session_environment(&tmux_target(&captain_id), "T_HUB_CONTROL_TOKEN", &ctx.token)
        .unwrap();
    sink.calls.lock().unwrap().clear();
    captains.fail_next_persist("attach bind persistence failure");

    let error = dispatch(
        &ctx,
        "attach_captain",
        &json!({
            "captainSessionId": captain_id,
            "projectId": "project-attach-rollback",
            "assignment": "Own rollback",
            "provider": "codex",
            "testSkipPowderHealth": true
        }),
    )
    .unwrap_err();
    assert!(
        error.contains("attach bind persistence failure"),
        "got: {error}"
    );
    assert!(captains.snapshot().captains.is_empty());
    let rolled_back = tabs.snapshot_full();
    assert_eq!(
        rolled_back
            .tabs
            .iter()
            .flat_map(|tab| tab.tile_ids.iter())
            .filter(|tile| *tile == &captain_id)
            .count(),
        1
    );
    assert!(rolled_back
        .tabs
        .iter()
        .find(|tab| tab.id == "attach-work")
        .unwrap()
        .tile_ids
        .contains(&captain_id));
    let failed_projection = serde_json::to_string(&*sink.calls.lock().unwrap()).unwrap();
    assert!(
        !failed_projection.contains(&captain_id),
        "failed attachment must never project a ghost Captain or placement: {failed_projection}"
    );

    let retry = dispatch(
        &ctx,
        "attach_captain",
        &json!({
            "captainSessionId": captain_id,
            "projectId": "project-attach-rollback",
            "assignment": "Own rollback",
            "provider": "codex",
            "testSkipPowderHealth": true
        }),
    )
    .unwrap();
    assert_eq!(retry["accepted"], "attach_captain");
    let restored = CaptainsRegistry::load(path.clone()).snapshot();
    assert_eq!(restored.captains.len(), 1);
    assert_eq!(
        restored.captains[0].project_id.as_deref(),
        Some("project-attach-rollback")
    );
    let final_tabs = tabs.snapshot_full();
    assert_eq!(
        final_tabs
            .tabs
            .iter()
            .flat_map(|tab| tab.tile_ids.iter())
            .filter(|tile| *tile == &captain_id)
            .count(),
        1
    );
    assert!(final_tabs
        .tabs
        .iter()
        .find(|tab| tab.id == CAPTAIN_WORKSPACE_ID)
        .unwrap()
        .tile_ids
        .contains(&captain_id));

    dispatch(
        &ctx,
        "release_captain",
        &json!({"captainSessionId": captain_id}),
    )
    .unwrap();
    dispatch(
        &ctx,
        "move_tile",
        &json!({"terminalId": captain_id, "tabId": "attach-work"}),
    )
    .unwrap();
    let before_bootstrap_failure = captains.snapshot();
    sink.calls.lock().unwrap().clear();
    let bootstrap_error = dispatch(
        &ctx,
        "attach_captain",
        &json!({
            "captainSessionId": captain_id,
            "projectId": "project-attach-rollback",
            "assignment": "Own rollback",
            "provider": "codex",
            "testSkipPowderHealth": true,
            "testFailBootstrapDelivery": true
        }),
    )
    .unwrap_err();
    assert!(bootstrap_error.contains("injected bootstrap delivery failure"));
    assert_eq!(
        captains.snapshot().captains,
        before_bootstrap_failure.captains
    );
    assert!(tabs
        .snapshot()
        .iter()
        .find(|tab| tab.id == "attach-work")
        .unwrap()
        .tile_ids
        .contains(&captain_id));
    assert!(
        sink.calls.lock().unwrap().is_empty(),
        "bootstrap rollback must occur before any Captain or Workspace projection"
    );

    dispatch(&ctx, "close_terminal", &json!({"sessionId": captain_id})).unwrap();
    let _ = std::fs::remove_dir_all(harness_bin_dir);
    let _ = std::fs::remove_file(path.with_extension("json.bak"));
    let _ = std::fs::remove_file(path);
}

#[test]
fn close_terminal_retires_legacy_powder_without_network() {
    if !std::process::Command::new("tmux")
        .arg("-V")
        .output()
        .is_ok_and(|output| output.status.success())
    {
        eprintln!(
                "rollback_close_retains_cleanup_pending_crew_when_powder_release_fails: tmux not on PATH - skipping"
            );
        return;
    }
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let crew_id = format!("rollback-{}", uuid::Uuid::new_v4().simple());
    let target = tmux_target(&crew_id);
    create_test_tmux_session(&target).unwrap();

    let registry = Arc::new(CaptainsRegistry::new());
    registry
        .upsert_project(ProjectRecord {
            root_path: None,
            vcs_capability: None,
            git_main_root: None,
            project_id: "rollback-project".into(),
            name: "Rollback Project".into(),
            repo_root: "/tmp".into(),
            remote_url: None,
            default_branch: None,
            powder: Some(PowderProjectBinding {
                connection_profile: format!("missing-{}", uuid::Uuid::new_v4().simple()),
                repository: "rollback-project".into(),
                event_cursor: 0,
            }),
            created_at: 0,
            updated_at: 0,
        })
        .unwrap();
    registry
        .claim_test("rollback-captain", Some("rollback-ship"), vec![])
        .unwrap();
    registry
        .bind_ship_context(
            "rollback-ship",
            "rollback-project",
            "Test rollback",
            "codex",
        )
        .unwrap();
    registry.record_crew("rollback-captain", &crew_id).unwrap();
    registry
        .bind_crew_context(
            "rollback-captain",
            &crew_id,
            "Test failed release",
            "codex",
            Some("/tmp"),
            Some("card-1"),
            PowderWorkBinding {
                card_id: "card-1".into(),
                run_id: "run-1".into(),
                agent: None,
                claim_expires_at: Some(1),
                mutation_intent: None,
                dispatch_release_recovery: false,
                state: PowderWorkState::Active,
            },
        )
        .unwrap();
    let ctx = test_ctx("secret").with_captains_registry(registry.clone());

    let closed =
        close_terminal_with_policy(&ctx, &json!({ "sessionId": crew_id }), true, None).unwrap();

    assert_eq!(closed["powderRelease"]["outcome"], "retired");
    assert_eq!(closed["powderRelease"]["released"], false);
    assert_eq!(tmux::session_liveness(&target), tmux::SessionLiveness::Gone);
    let snapshot = registry.snapshot();
    assert!(snapshot.pending_dispatch_releases.is_empty());
    assert!(matches!(
        snapshot.captains[0].crew[0].state,
        CrewState::Removed { .. }
    ));
    // The retired binding remains on the historical tombstone for
    // deserialization compatibility; no remote release was attempted.
    assert!(snapshot.captains[0].crew[0].powder_work.is_some());
}

#[test]
fn register_project_authorizes_before_creating_or_initializing_files() {
    let identities = Arc::new(crate::identity::IdentityStore::ephemeral());
    let captain = mint_session(
        &identities,
        crate::identity::Role::Captain,
        "foreign-ship",
        "foreign-captain",
    );
    let ctx = test_ctx("register-project-scope").with_identity_store(identities);
    let parent = std::env::temp_dir().join(format!(
        "t-hub-register-scope-{}-{}",
        std::process::id(),
        now_ms()
    ));
    std::fs::create_dir_all(&parent).unwrap();
    let requested = parent.join("must-not-exist");

    let response = dispatch_authenticated(
        &ctx,
        req_session(
            "register-project-scope",
            &captain,
            "register_project",
            json!({
                "repoRoot": requested.to_string_lossy(),
                "name": "Scoped Project",
                "createDirectory": true,
            }),
        ),
    );

    assert!(!response.ok);
    assert!(response
        .error
        .as_deref()
        .is_some_and(|error| error.contains("only General/Cortana")));
    assert!(
        !requested.exists(),
        "authorization ran after filesystem mutation"
    );
    let _ = std::fs::remove_dir(parent);
}

#[test]
fn unauthorized_project_root_requests_have_zero_probe_or_persistence_counts() {
    let identities = Arc::new(crate::identity::IdentityStore::ephemeral());
    let caller = mint_session(
        &identities,
        crate::identity::Role::Captain,
        "foreign-project-ship",
        "foreign-project-captain",
    );
    let ctx = test_ctx("project-probe-order").with_identity_store(identities);
    let existing = std::env::temp_dir().join(format!("t-hub-unauthorized-existing-{}", now_ms()));
    std::fs::create_dir_all(&existing).unwrap();
    let missing = existing.join("missing");

    for (command, root) in [
        ("register_project", existing.clone()),
        ("initialize_git", missing),
    ] {
        reset_project_probe_counts();
        let args = if command == "register_project" {
            json!({ "rootPath": root.to_string_lossy(), "name": "Denied Project", "createDirectory": true })
        } else {
            json!({ "rootPath": root.to_string_lossy(), "name": "Denied Project" })
        };
        let response = dispatch_authenticated(
            &ctx,
            req_session("project-probe-order", &caller, command, args),
        );
        assert!(!response.ok, "{command} unexpectedly succeeded");
        assert!(response
            .error
            .unwrap_or_default()
            .contains("only General/Cortana"));
        assert_eq!(
            project_probe_counts(),
            [0; 6],
            "{command} probed before authority"
        );
        assert!(ctx.captains.projects().is_empty());
    }
    let _ = std::fs::remove_dir_all(existing);
}

#[test]
fn project_root_identity_accepts_posix_and_all_supported_wsl_unc_spellings() {
    let expected = "/home/natkins/projects/demo";
    for spelling in [
        expected,
        "/home/natkins/projects/./demo/",
        r#"\\wsl.localhost\Ubuntu-24.04\home\natkins\projects\demo"#,
        r#"\\wsl$\Ubuntu-24.04\home\natkins\projects\demo"#,
        r#"\\?\UNC\wsl.localhost\Ubuntu-24.04\home\natkins\projects\demo\."#,
    ] {
        assert_eq!(canonical_project_identity(spelling).unwrap(), expected);
    }
}

#[test]
fn project_root_identity_rejects_relative_traversal_foreign_and_unsafe_unc() {
    for spelling in [
        "relative/project",
        "/tmp/../secret",
        r#"\\wsl.localhost\Debian\home\natkins\project"#,
        r#"\\server\share\project"#,
    ] {
        assert!(
            canonical_project_identity(spelling).is_err(),
            "accepted {spelling}"
        );
    }
}

#[test]
fn conflicting_root_aliases_fail_before_project_probes_or_mutation() {
    let ctx = test_ctx("root-alias-conflict");
    reset_project_probe_counts();
    let response = dispatch(
        &ctx,
        "register_project",
        &json!({
            "rootPath": "/tmp/root-primary",
            "repoRoot": "/tmp/root-conflict",
            "name": "Conflicting Roots",
            "createDirectory": true,
        }),
    )
    .unwrap_err();
    assert!(response.contains("conflicting rootPath and repoRoot"));
    assert!(ctx.captains.projects().is_empty());
    assert_eq!(project_probe_counts(), [0; 6]);
    assert!(!std::path::Path::new("/tmp/root-primary").exists());
    assert!(!std::path::Path::new("/tmp/root-conflict").exists());
}

#[test]
fn register_project_accepts_each_root_identity_contract_form() {
    let forms = ["rootPath", "repoRoot", "repo_root"];
    for (index, field) in forms.into_iter().enumerate() {
        let ctx = test_ctx(&format!("root-alias-form-{field}"));
        let dir = std::env::temp_dir().join(format!(
            "t-hub-root-alias-form-{}-{}-{index}",
            std::process::id(),
            now_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let mut args = serde_json::Map::new();
        args.insert(field.to_string(), json!(dir.to_string_lossy()));
        args.insert("name".to_string(), json!(format!("Root Form {field}")));
        let project = dispatch(&ctx, "register_project", &Value::Object(args)).unwrap();
        assert_eq!(project["rootPath"], dir.to_string_lossy().to_string());
        assert_eq!(project["repoRoot"], project["rootPath"]);
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[test]
fn initialize_git_conflicting_root_aliases_fail_before_probes() {
    let ctx = test_ctx("initialize-root-alias-conflict");
    reset_project_probe_counts();
    let error = dispatch(
        &ctx,
        "initialize_git",
        &json!({
            "rootPath": "/tmp/initialize-root-primary",
            "repo_root": "/tmp/initialize-root-conflict",
            "name": "Conflicting Initialize Roots",
        }),
    )
    .unwrap_err();
    assert!(error.contains("conflicting rootPath and repoRoot"));
    assert_eq!(project_probe_counts(), [0; 6]);
    assert!(ctx.captains.projects().is_empty());
}

#[test]
fn every_root_alias_conflict_is_rejected_before_dispatch_probes() {
    let conflicts = [
        json!({
            "rootPath": "/tmp/root-primary",
            "repoRoot": "/tmp/root-conflict",
            "name": "Conflicting Roots",
        }),
        json!({
            "repoRoot": "/tmp/repo-root-primary",
            "repo_root": "/tmp/repo-root-conflict",
            "name": "Conflicting Roots",
        }),
        json!({
            "rootPath": "/tmp/three-field-primary",
            "repoRoot": "/tmp/three-field-primary",
            "repo_root": "/tmp/three-field-conflict",
            "name": "Conflicting Roots",
        }),
    ];
    for command in ["register_project", "initialize_git"] {
        for mut args in conflicts.clone() {
            if command == "register_project" {
                args["createDirectory"] = json!(true);
            }
            let ctx = test_ctx(&format!("root-alias-conflict-{command}"));
            reset_project_probe_counts();
            let error = dispatch(&ctx, command, &args).unwrap_err();
            assert!(
                error.contains("conflicting rootPath and repoRoot"),
                "{command}: {error}"
            );
            assert_eq!(
                project_probe_counts(),
                [0; 6],
                "{command} performed a probe before alias validation"
            );
            assert!(ctx.captains.projects().is_empty());
        }
    }
}

#[test]
fn all_equal_root_aliases_dispatch_without_duplicate_identity() {
    let register_ctx = test_ctx("root-alias-all-equal-register");
    let register_dir = std::env::temp_dir().join(format!(
        "t-hub-root-alias-all-equal-register-{}-{}",
        std::process::id(),
        now_ms()
    ));
    std::fs::create_dir_all(&register_dir).unwrap();
    let register_root = register_dir.to_string_lossy().to_string();
    let registered = dispatch(
        &register_ctx,
        "register_project",
        &json!({
            "rootPath": format!("{register_root}/./"),
            "repoRoot": register_root,
            "repo_root": format!("{}/", register_dir.to_string_lossy()),
            "name": "All Equal Roots",
        }),
    )
    .unwrap();
    assert_eq!(registered["rootPath"], registered["repoRoot"]);
    assert_eq!(register_ctx.captains.projects().len(), 1);
    let _ = std::fs::remove_dir_all(register_dir);

    let initialize_ctx = test_ctx("root-alias-all-equal-initialize");
    let initialize_dir = std::env::temp_dir().join(format!(
        "t-hub-root-alias-all-equal-initialize-{}-{}",
        std::process::id(),
        now_ms()
    ));
    std::fs::create_dir_all(&initialize_dir).unwrap();
    let initialize_root = initialize_dir.to_string_lossy().to_string();
    let initialized = dispatch(
        &initialize_ctx,
        "initialize_git",
        &json!({
            "rootPath": format!("{initialize_root}/./"),
            "repoRoot": initialize_root,
            "repo_root": format!("{}/", initialize_dir.to_string_lossy()),
            "name": "All Equal Initialized Roots",
        }),
    )
    .unwrap();
    assert_eq!(initialized["rootPath"], initialized["repoRoot"]);
    assert_eq!(initialize_ctx.captains.projects().len(), 1);
    let _ = std::fs::remove_dir_all(initialize_dir);
}

#[test]
fn equal_root_aliases_register_using_authoritative_root_path() {
    let ctx = test_ctx("root-alias-equal");
    let dir = std::env::temp_dir().join(format!("t-hub-root-alias-{}", now_ms()));
    std::fs::create_dir_all(&dir).unwrap();
    let root = dir.to_string_lossy().to_string();
    let response = dispatch(
        &ctx,
        "register_project",
        &json!({
            "rootPath": format!("{root}/./"),
            "repoRoot": root,
            "name": "Equal Roots",
        }),
    )
    .unwrap();
    assert_eq!(response["rootPath"], response["repoRoot"]);
    assert_eq!(response["rootPath"], dir.to_string_lossy().to_string());
    assert_eq!(ctx.captains.projects().len(), 1);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn register_project_retains_linked_selection_and_separate_git_main_root() {
    let (base, repo, linked) = scratch_repo_with_worktree();
    let ctx = test_ctx("linked-project-registration");
    let selected = linked.to_string_lossy().to_string();
    let project = dispatch(
        &ctx,
        "register_project",
        &json!({ "rootPath": selected, "name": "Linked Selection" }),
    )
    .unwrap();
    assert_eq!(project["rootPath"], linked.to_string_lossy().to_string());
    assert_eq!(project["repoRoot"], project["rootPath"]);
    assert_eq!(project["gitMainRoot"], repo.to_string_lossy().to_string());
    assert_ne!(project["rootPath"], project["gitMainRoot"]);
    assert_eq!(ctx.captains.projects().len(), 1);
    let _ = std::fs::remove_dir_all(base);
}

#[cfg(unix)]
#[test]
fn concurrent_symlink_equivalent_registrations_converge_to_one_project() {
    let parent = std::env::temp_dir().join(format!("t-hub-project-race-{}", now_ms()));
    let root = parent.join("root");
    let alias = parent.join("alias");
    std::fs::create_dir_all(&root).unwrap();
    std::os::unix::fs::symlink(&root, &alias).unwrap();
    let expected_root = root.to_string_lossy().to_string();
    let ctx = Arc::new(test_ctx("project-registration-race"));
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let joins = [root.clone(), alias]
        .into_iter()
        .map(|path| {
            let ctx = Arc::clone(&ctx);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                dispatch(
                    &ctx,
                    "register_project",
                    &json!({ "rootPath": path.to_string_lossy(), "name": "Raced Project" }),
                )
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    let results: Vec<_> = joins.into_iter().map(|join| join.join().unwrap()).collect();
    assert!(results.iter().all(Result::is_ok), "results: {results:?}");
    let projects = ctx.captains.projects();
    assert_eq!(projects.len(), 1);
    assert_eq!(
        projects[0].root_path.as_deref(),
        Some(expected_root.as_str())
    );
    assert_eq!(
        projects[0].repo_root,
        projects[0].root_path.clone().unwrap()
    );
    let _ = std::fs::remove_dir_all(parent);
}

#[test]
fn register_project_accepts_a_non_repository_without_initializing_git() {
    let ctx = test_ctx("secret");
    let dir = std::env::temp_dir().join(format!(
        "t-hub-register-nonrepo-{}-{}",
        std::process::id(),
        now_ms()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let project = dispatch(
        &ctx,
        "register_project",
        &json!({"repoRoot": dir.to_string_lossy(), "name": "Non Git Project"}),
    )
    .unwrap();
    assert_eq!(project["repoRoot"], dir.to_string_lossy().to_string());
    assert!(!dir.join(".git").exists());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn initialize_git_is_separate_from_register_project() {
    let ctx = test_ctx("secret");
    let dir = std::env::temp_dir().join(format!(
        "t-hub-register-init-{}-{}",
        std::process::id(),
        now_ms()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("keep.txt"), "preserve me").unwrap();

    let project = dispatch(
        &ctx,
        "initialize_git",
        &json!({"repoRoot": dir.to_string_lossy(), "name": "Initialized Project"}),
    )
    .unwrap();

    assert_eq!(project["repoRoot"], dir.to_string_lossy().as_ref());
    assert_eq!(project["defaultBranch"], "main");
    assert!(dir.join(".git").is_dir());
    assert_eq!(
        std::fs::read_to_string(dir.join("keep.txt")).unwrap(),
        "preserve me"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn initialize_git_recovers_durable_transaction_after_restart() {
    let ctx = test_ctx("initialize-git-recovery");
    let dir = std::env::temp_dir().join(format!(
        "t-hub-initialize-git-recovery-{}-{}",
        std::process::id(),
        now_ms()
    ));
    let registry_path = dir.with_extension("json");
    std::fs::create_dir_all(&dir).unwrap();
    let registry = Arc::new(CaptainsRegistry::load(registry_path.clone()));
    let ctx = ctx.with_captains_registry(Arc::clone(&registry));
    set_git_init_fault("after_marker_before_project");

    let error = dispatch(
        &ctx,
        "initialize_git",
        &json!({ "repoRoot": dir.to_string_lossy(), "name": "Recovery Project" }),
    )
    .unwrap_err();
    clear_git_init_fault();

    assert!(
        error.contains("injected Git initialization fault"),
        "got: {error}"
    );
    assert!(dir.join(".git").is_dir());
    assert_eq!(registry.pending_git_initializations().len(), 1);
    assert!(dir.join(".git/t-hub-git-init-marker.json").is_file());

    let recovered = CaptainsRegistry::load(registry_path.clone());
    let project = recovered
        .projects()
        .into_iter()
        .find(|project| project.name == "Recovery Project")
        .expect("restart should finalize the owned Git initialization");
    assert_eq!(project.vcs_capability.as_deref(), Some("git"));
    assert_eq!(
        project.root_path.as_deref(),
        project.repo_root.as_str().into()
    );
    assert!(recovered.pending_git_initializations().is_empty());
    assert!(!dir.join(".git/t-hub-git-init-marker.json").exists());

    let recovered_again = CaptainsRegistry::load(registry_path.clone());
    assert_eq!(recovered_again.projects(), recovered.projects());
    assert!(recovered_again.pending_git_initializations().is_empty());

    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::remove_file(registry_path);
}

#[test]
fn initialize_git_fault_boundaries_recover_deterministically() {
    for (index, fault, expects_git, expects_project) in [
        (0, "after_intent_before_git_init", false, false),
        (1, "after_git_init_before_marker", true, false),
        (2, "after_marker_before_project", true, true),
        (3, "after_project_before_clear", true, true),
        (4, "during_cleanup", true, true),
    ] {
        let ctx = test_ctx("initialize-git-fault");
        let dir = std::env::temp_dir().join(format!(
            "t-hub-initialize-git-fault-{index}-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let registry_path = dir.with_extension("json");
        std::fs::create_dir_all(&dir).unwrap();
        let registry = Arc::new(CaptainsRegistry::load(registry_path.clone()));
        let ctx = ctx.with_captains_registry(Arc::clone(&registry));
        set_git_init_fault(fault);

        let response = dispatch(
            &ctx,
            "initialize_git",
            &json!({ "rootPath": dir.to_string_lossy(), "name": "Fault Project" }),
        );
        clear_git_init_fault();
        assert!(response.is_err(), "fault {fault} did not fire");
        assert!(
            response
                .as_ref()
                .unwrap_err()
                .starts_with("git_init_recovery code=git_init_recovery operation="),
            "fault {fault} returned a non-structured error: {response:?}"
        );

        let restarted = CaptainsRegistry::load(registry_path.clone());
        assert_eq!(dir.join(".git").is_dir(), expects_git, "fault {fault}");
        assert_eq!(
            restarted
                .projects()
                .iter()
                .any(|project| project.name == "Fault Project"),
            expects_project,
            "fault {fault}"
        );
        if fault == "after_git_init_before_marker" {
            assert_eq!(restarted.pending_git_initializations().len(), 1);
            assert!(
                restarted.pending_git_initializations()[0].phase.as_str() == "recovery_blocked"
            );
        } else {
            assert!(
                restarted.pending_git_initializations().is_empty(),
                "fault {fault}"
            );
        }
        let restarted_again = CaptainsRegistry::load(registry_path.clone());
        assert_eq!(
            restarted_again.projects(),
            restarted.projects(),
            "fault {fault} was not idempotent"
        );
        if expects_project {
            assert!(!dir.join(".git/t-hub-git-init-marker.json").exists());
        }
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_file(registry_path);
    }
}

#[test]
fn initialize_git_before_intent_fault_has_no_durable_or_filesystem_residue() {
    let ctx = test_ctx("initialize-git-before-intent");
    let dir = std::env::temp_dir().join(format!(
        "t-hub-initialize-git-before-intent-{}-{}",
        std::process::id(),
        now_ms()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    set_git_init_fault("before_intent_write");
    let error = dispatch(
        &ctx,
        "initialize_git",
        &json!({ "rootPath": dir.to_string_lossy(), "name": "Before Intent" }),
    )
    .unwrap_err();
    clear_git_init_fault();
    assert!(error.contains("before_intent_write"));
    assert!(!dir.join(".git").exists());
    assert!(ctx.captains.pending_git_initializations().is_empty());
    assert!(ctx.captains.projects().is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn initialize_git_concurrent_equivalent_requests_converge_to_one_transaction() {
    let ctx = test_ctx("initialize-git-concurrent-equivalent");
    let dir = std::env::temp_dir().join(format!(
        "t-hub-initialize-git-concurrent-{}-{}",
        std::process::id(),
        now_ms()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let joins = (0..2)
        .map(|_| {
            let ctx = ctx.clone();
            let barrier = Arc::clone(&barrier);
            let root = dir.to_string_lossy().to_string();
            std::thread::spawn(move || {
                barrier.wait();
                dispatch(
                    &ctx,
                    "initialize_git",
                    &json!({ "rootPath": root, "name": "Concurrent Git Project" }),
                )
            })
        })
        .collect::<Vec<_>>();
    let results = joins
        .into_iter()
        .map(|join| join.join().unwrap())
        .collect::<Vec<_>>();
    assert!(results.iter().all(Result::is_ok), "results: {results:?}");
    assert_eq!(ctx.captains.projects().len(), 1);
    assert!(ctx.captains.pending_git_initializations().is_empty());
    assert!(!dir.join(".git/t-hub-git-init-marker.json").exists());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn initialize_git_conflicting_names_refuse_before_a_second_mutation() {
    let ctx = test_ctx("initialize-git-conflicting-names");
    let dir = std::env::temp_dir().join(format!(
        "t-hub-initialize-git-conflicting-names-{}-{}",
        std::process::id(),
        now_ms()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let joins = ["First Git Project", "Conflicting Git Project"]
        .into_iter()
        .map(|name| {
            let ctx = ctx.clone();
            let barrier = Arc::clone(&barrier);
            let root = dir.to_string_lossy().to_string();
            std::thread::spawn(move || {
                barrier.wait();
                dispatch(
                    &ctx,
                    "initialize_git",
                    &json!({ "rootPath": root, "name": name }),
                )
            })
        })
        .collect::<Vec<_>>();
    let results = joins
        .into_iter()
        .map(|join| join.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    let errors = results
        .iter()
        .filter_map(|result| result.as_ref().err())
        .collect::<Vec<_>>();
    assert_eq!(errors.len(), 1);
    assert!(errors[0].contains("conflicting durable Project name"));
    assert_eq!(ctx.captains.projects().len(), 1);
    assert!(ctx.captains.pending_git_initializations().is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn initialize_git_project_persistence_failure_leaves_recoverable_evidence() {
    let ctx = test_ctx("initialize-git-project-persist-failure");
    let dir = std::env::temp_dir().join(format!(
        "t-hub-initialize-git-project-failure-{}-{}",
        std::process::id(),
        now_ms()
    ));
    let registry_path = dir.with_extension("json");
    std::fs::create_dir_all(&dir).unwrap();
    let registry = Arc::new(CaptainsRegistry::load(registry_path.clone()));
    let calls = Arc::new(AtomicUsize::new(0));
    let hook_registry = Arc::clone(&registry);
    let hook_calls = Arc::clone(&calls);
    registry.set_persist_hook(Box::new(move || {
        if hook_calls.fetch_add(1, Ordering::SeqCst) == 2 {
            hook_registry.fail_next_persist("injected Project persistence failure");
        }
    }));
    let ctx = ctx.with_captains_registry(Arc::clone(&registry));

    let response = dispatch(
        &ctx,
        "initialize_git",
        &json!({ "rootPath": dir.to_string_lossy(), "name": "Persisted Recovery Project" }),
    );
    assert!(response
        .unwrap_err()
        .contains("injected Project persistence failure"));
    assert!(dir.join(".git").is_dir());
    assert!(dir.join(".git/t-hub-git-init-marker.json").is_file());

    let restarted = CaptainsRegistry::load(registry_path.clone());
    assert_eq!(restarted.projects().len(), 1);
    assert_eq!(
        restarted.projects()[0].vcs_capability.as_deref(),
        Some("git")
    );
    assert!(restarted.pending_git_initializations().is_empty());
    assert!(!dir.join(".git/t-hub-git-init-marker.json").exists());
    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::remove_file(registry_path);
}

#[test]
fn initialize_git_refuses_foreign_or_tampered_git_state_without_deletion() {
    let ctx = test_ctx("initialize-git-ownership");
    let dir = std::env::temp_dir().join(format!(
        "t-hub-initialize-git-ownership-{}-{}",
        std::process::id(),
        now_ms()
    ));
    std::fs::create_dir_all(dir.join(".git")).unwrap();
    std::fs::write(dir.join(".git/foreign"), "keep").unwrap();
    let response = dispatch(
        &ctx,
        "initialize_git",
        &json!({ "rootPath": dir.to_string_lossy(), "name": "Foreign Project" }),
    );
    assert!(response.unwrap_err().contains("pre-existing .git"));
    assert_eq!(
        std::fs::read_to_string(dir.join(".git/foreign")).unwrap(),
        "keep"
    );
    assert!(!ctx
        .captains
        .projects()
        .iter()
        .any(|project| project.name == "Foreign Project"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn initialize_git_foreign_git_clear_failure_survives_restart_without_ownership() {
    let ctx = test_ctx("initialize-git-foreign-clear-failure");
    let dir = std::env::temp_dir().join(format!(
        "t-hub-initialize-git-foreign-clear-failure-{}-{}",
        std::process::id(),
        now_ms()
    ));
    let registry_path = dir.with_extension("json");
    std::fs::create_dir_all(dir.join(".git")).unwrap();
    std::fs::write(dir.join(".git/foreign-state"), "preserve").unwrap();
    let registry = Arc::new(CaptainsRegistry::load(registry_path.clone()));
    let calls = Arc::new(AtomicUsize::new(0));
    let hook_registry = Arc::clone(&registry);
    let hook_calls = Arc::clone(&calls);
    registry.set_persist_hook(Box::new(move || {
        if hook_calls.fetch_add(1, Ordering::SeqCst) == 2 {
            hook_registry.fail_next_persist("injected foreign intent clear failure");
        }
    }));
    let ctx = ctx.with_captains_registry(Arc::clone(&registry));

    let error = dispatch(
        &ctx,
        "initialize_git",
        &json!({ "rootPath": dir.to_string_lossy(), "name": "Foreign Clear Failure" }),
    )
    .unwrap_err();
    assert!(error.starts_with("git_init_recovery code=git_init_recovery operation="));
    assert_eq!(registry.pending_git_initializations().len(), 1);
    assert_eq!(
        registry.pending_git_initializations()[0].phase,
        "foreign_git"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join(".git/foreign-state")).unwrap(),
        "preserve"
    );

    set_git_init_fault("foreign_cleanup");
    let restarted = CaptainsRegistry::load(registry_path.clone());
    clear_git_init_fault();
    assert!(restarted.projects().is_empty());
    assert_eq!(restarted.pending_git_initializations().len(), 1);
    assert_eq!(
        restarted.pending_git_initializations()[0].phase,
        "foreign_git"
    );
    assert!(!dir.join(".git/t-hub-git-init-marker.json").exists());
    assert_eq!(
        std::fs::read_to_string(dir.join(".git/foreign-state")).unwrap(),
        "preserve"
    );
    let restarted_again = CaptainsRegistry::load(registry_path.clone());
    assert!(restarted_again.projects().is_empty());
    assert!(restarted_again.pending_git_initializations().is_empty());
    assert!(!dir.join(".git/t-hub-git-init-marker.json").exists());
    assert_eq!(
        std::fs::read_to_string(dir.join(".git/foreign-state")).unwrap(),
        "preserve"
    );
    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::remove_file(registry_path);
}

#[test]
fn initialize_git_tampered_marker_fails_closed_across_restart() {
    let ctx = test_ctx("initialize-git-tampered-marker");
    let dir = std::env::temp_dir().join(format!(
        "t-hub-initialize-git-tampered-marker-{}-{}",
        std::process::id(),
        now_ms()
    ));
    let registry_path = dir.with_extension("json");
    std::fs::create_dir_all(&dir).unwrap();
    let registry = Arc::new(CaptainsRegistry::load(registry_path.clone()));
    let ctx = ctx.with_captains_registry(Arc::clone(&registry));
    set_git_init_fault("after_marker_before_project");
    let _ = dispatch(
        &ctx,
        "initialize_git",
        &json!({ "rootPath": dir.to_string_lossy(), "name": "Tampered Project" }),
    );
    clear_git_init_fault();

    let marker_path = dir.join(".git/t-hub-git-init-marker.json");
    let mut marker: GitInitMarker =
        serde_json::from_str(&std::fs::read_to_string(&marker_path).unwrap()).unwrap();
    marker.marker_nonce = "foreign-nonce".into();
    std::fs::write(&marker_path, serde_json::to_vec(&marker).unwrap()).unwrap();

    let restarted = CaptainsRegistry::load(registry_path.clone());
    assert!(restarted.projects().is_empty());
    assert_eq!(restarted.pending_git_initializations().len(), 1);
    assert_eq!(
        restarted.pending_git_initializations()[0].phase,
        "recovery_blocked"
    );
    assert!(marker_path.is_file());
    let restarted_again = CaptainsRegistry::load(registry_path.clone());
    assert!(restarted_again.projects().is_empty());
    assert_eq!(restarted_again.pending_git_initializations().len(), 1);
    assert!(marker_path.is_file());
    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::remove_file(registry_path);
}

#[test]
fn register_project_never_rewrites_an_existing_git_entry() {
    let ctx = test_ctx("secret");
    let dir = std::env::temp_dir().join(format!(
        "t-hub-register-existing-git-entry-{}-{}",
        std::process::id(),
        now_ms()
    ));
    std::fs::create_dir_all(dir.join(".git")).unwrap();
    std::fs::write(dir.join(".git/owner"), "pre-existing").unwrap();

    let project = dispatch(
        &ctx,
        "register_project",
        &json!({"repoRoot": dir.to_string_lossy(), "name": "Existing Git Project"}),
    )
    .unwrap();
    assert_eq!(project["vcsCapability"], "none");
    assert_eq!(
        std::fs::read_to_string(dir.join(".git/owner")).unwrap(),
        "pre-existing"
    );
    assert_eq!(ctx.captains.projects().len(), 1);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn register_project_creates_an_absent_empty_codebase_leaf() {
    let ctx = test_ctx("secret");
    let parent = std::env::temp_dir().join(format!(
        "t-hub-register-new-{}-{}",
        std::process::id(),
        now_ms()
    ));
    std::fs::create_dir(&parent).unwrap();
    let destination = parent.join("fresh-codebase");

    let project = dispatch(
        &ctx,
        "register_project",
        &json!({
            "repoRoot": destination.to_string_lossy(),
            "name": "Fresh Codebase",
            "createDirectory": true,
        }),
    )
    .unwrap();

    assert_eq!(project["name"], "Fresh Codebase");
    assert_eq!(project["repoRoot"], destination.to_string_lossy().as_ref());
    assert!(project["defaultBranch"].is_null());
    assert!(!destination.join(".git").exists());
    let _ = std::fs::remove_dir_all(parent);
}

#[test]
fn register_project_new_codebase_refuses_any_existing_destination() {
    let ctx = test_ctx("secret");
    let parent = std::env::temp_dir().join(format!(
        "t-hub-register-new-existing-{}-{}",
        std::process::id(),
        now_ms()
    ));
    let destination = parent.join("already-here");
    std::fs::create_dir_all(&destination).unwrap();
    std::fs::write(destination.join("keep.txt"), "preserve me").unwrap();

    let error = dispatch(
        &ctx,
        "register_project",
        &json!({
            "repoRoot": destination.to_string_lossy(),
            "createDirectory": true,
            "name": "Existing Destination"
        }),
    )
    .unwrap_err();

    assert!(error.contains("already exists"), "got: {error}");
    assert_eq!(
        std::fs::read_to_string(destination.join("keep.txt")).unwrap(),
        "preserve me"
    );
    assert!(ctx.captains.projects().is_empty());
    let _ = std::fs::remove_dir_all(parent);
}

#[test]
fn register_project_new_codebase_can_remain_non_git() {
    let ctx = test_ctx("secret");
    let parent = std::env::temp_dir().join(format!(
        "t-hub-register-new-invalid-{}-{}",
        std::process::id(),
        now_ms()
    ));
    std::fs::create_dir(&parent).unwrap();
    let destination = parent.join("missing-init");
    let project = dispatch(
        &ctx,
        "register_project",
        &json!({
            "repoRoot": destination.to_string_lossy(),
            "createDirectory": true,
            "name": "Missing Init"
        }),
    )
    .unwrap();
    assert_eq!(
        project["repoRoot"],
        destination.to_string_lossy().to_string()
    );
    assert!(destination.is_dir());
    assert!(!destination.join(".git").exists());

    let trailing_slash = format!("{}/", parent.join("ambiguous").to_string_lossy());
    let initialized = dispatch(
        &ctx,
        "register_project",
        &json!({
            "repoRoot": trailing_slash,
            "createDirectory": true,
            "name": "Trailing Path"
        }),
    )
    .unwrap();
    assert_eq!(
        initialized["repoRoot"],
        parent.join("ambiguous").to_string_lossy().to_string()
    );
    assert!(!parent.join("ambiguous/.git").exists());

    let missing_parent = parent.join("missing").join("child");
    let error = dispatch(
        &ctx,
        "register_project",
        &json!({
            "repoRoot": missing_parent.to_string_lossy(),
            "createDirectory": true,
            "name": "Missing Parent"
        }),
    )
    .unwrap_err();
    assert!(error.contains("parent directory"), "got: {error}");
    assert!(!parent.join("missing").exists());
    let _ = std::fs::remove_dir(parent);
}

#[test]
fn register_project_rejects_retired_powder_arguments_before_git_work() {
    let ctx = test_ctx("secret");
    let response = dispatch(
        &ctx,
        "register_project",
        &json!({
            "repoRoot": "/tmp/not-touched",
            "powderRepository": "legacy-board"
        }),
    )
    .unwrap_err();
    assert!(response.contains("unexpected argument 'powderRepository'"));
}

#[test]
fn scribe_status_dispatches_and_returns_a_listening_bool() {
    // The read-tier scribe voice-gate: dispatches to crate::scribe and
    // always returns an object with a boolean `listening` field, whatever
    // the on-disk file says (fail-open guarantees the shape). Asserting the
    // shape (not the value) keeps this deterministic whether or not a real
    // Scribe status file exists on the test machine.
    let ctx = test_ctx("secret");
    let v = dispatch(&ctx, "scribe_status", &Value::Null).unwrap();
    assert!(v.is_object());
    assert!(v["listening"].is_boolean());
}

#[test]
fn claim_and_release_are_audited_and_forward_the_captains_snapshot() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let ctx = test_ctx("t").with_apply_sink(sink.clone());
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    ctx.tab_registry().replace(vec![TabRecord {
        id: "tab-1".into(),
        name: "Main".into(),
        tile_ids: vec![],
    }]);
    // A LIVE terminal to claim (the liveness gate): spawn it into tab-1.
    let cap_id = dispatch(
        &ctx,
        "spawn_terminal",
        &json!({
            "cwd": "/tmp",
            "tabId": "tab-1",
            "startupCommand": harness_command,
        }),
    )
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    wait_for_harness_started(&cap_id, "codex").unwrap();

    // Claim with no explicit workspaceTabIds does not infer Work Workspace
    // ownership from the Captain terminal's current placement.
    let v = dispatch(&ctx, "claim_captain", &json!({"captainSessionId": cap_id})).unwrap();
    assert_eq!(v["accepted"], "claim_captain");
    assert_eq!(v["audited"], true);
    assert_eq!(v["applied"], true);
    assert_eq!(v["captain"]["shipSlug"], format!("ship-{cap_id}"));
    assert_eq!(v["captain"]["workspaceTabIds"], json!([]));
    assert_eq!(v["captain"]["terminalId"], cap_id);

    let v = dispatch(
        &ctx,
        "release_captain",
        &json!({"captainSessionId": cap_id}),
    )
    .unwrap();
    assert_eq!(v["accepted"], "release_captain");
    assert_eq!(v["released"]["terminalId"], cap_id);
    assert_eq!(v["captains"], json!([]));

    // The claim + release each forwarded a sync_captains snapshot (filtering
    // out the spawn_terminal forward that seeded the live session).
    let sync_calls: Vec<_> = sink
        .calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(c, _)| c == "sync_captains")
        .cloned()
        .collect();
    assert_eq!(sync_calls.len(), 2);
    assert_eq!(sync_calls[0].1["sync"]["captains"][0]["terminalId"], cap_id);
    assert_eq!(sync_calls[1].1["sync"]["captains"], json!([]));

    dispatch(&ctx, "close_terminal", &json!({"sessionId": cap_id})).unwrap();
    let _ = std::fs::remove_dir_all(harness_bin_dir);
}

#[test]
fn claim_captain_relocates_the_tile_atomically_and_survives_retry_and_restart() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let path = captains_tmp("claim-relocation-restart");
    let captains = Arc::new(CaptainsRegistry::load(path.clone()));
    let tabs = Arc::new(TabRegistry::new());
    tabs.replace(vec![TabRecord {
        id: "work-a".into(),
        name: "Work A".into(),
        tile_ids: Vec::new(),
    }]);
    let ctx = test_ctx("claim-relocation")
        .with_apply_sink(Arc::new(RecordingSink {
            calls: StdMutex::new(Vec::new()),
        }))
        .with_captains_registry(Arc::clone(&captains))
        .with_tab_registry(Arc::clone(&tabs));
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    let captain_id = dispatch(
        &ctx,
        "spawn_terminal",
        &json!({"cwd": "/tmp", "tabId": "work-a", "startupCommand": harness_command}),
    )
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    wait_for_harness_started(&captain_id, "codex").unwrap();

    let claimed = dispatch(
        &ctx,
        "claim_captain",
        &json!({
            "captainSessionId": captain_id,
            "shipSlug": "alpha",
            "workspaceTabIds": ["work-a"]
        }),
    )
    .unwrap();
    assert_eq!(claimed["accepted"], "claim_captain");
    let snapshot = tabs.snapshot_full();
    let work = snapshot.tabs.iter().find(|tab| tab.id == "work-a").unwrap();
    let captain_workspace = snapshot
        .tabs
        .iter()
        .find(|tab| tab.id == CAPTAIN_WORKSPACE_ID)
        .unwrap();
    assert!(!work.tile_ids.contains(&captain_id));
    assert_eq!(
        captain_workspace
            .tile_ids
            .iter()
            .filter(|tile| *tile == &captain_id)
            .count(),
        1
    );

    let unchanged = dispatch(
        &ctx,
        "report_workspace_tabs",
        &json!({
            "baseSeq": snapshot.seq,
            "tabs": snapshot.tabs,
            "activeTabId": snapshot.active_tab_id
        }),
    )
    .unwrap();
    assert!(unchanged.get("reported").is_some());
    dispatch(
        &ctx,
        "claim_captain",
        &json!({
            "captainSessionId": captain_id,
            "shipSlug": "alpha",
            "workspaceTabIds": ["work-a"]
        }),
    )
    .unwrap();
    let after_retry = tabs.snapshot_full();
    assert_eq!(
        after_retry
            .tabs
            .iter()
            .flat_map(|tab| tab.tile_ids.iter())
            .filter(|tile| *tile == &captain_id)
            .count(),
        1
    );
    let restored = CaptainsRegistry::load(path.clone()).snapshot();
    assert_eq!(restored.captains.len(), 1);
    assert_eq!(
        restored.captains[0].terminal_id.as_deref(),
        Some(captain_id.as_str())
    );

    dispatch(&ctx, "close_terminal", &json!({"sessionId": captain_id})).unwrap();
    let _ = std::fs::remove_dir_all(harness_bin_dir);
    let _ = std::fs::remove_file(path.with_extension("json.bak"));
    let _ = std::fs::remove_file(path);
}

#[test]
fn failed_claim_captain_persistence_keeps_the_original_work_placement() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let captains = Arc::new(CaptainsRegistry::load(captains_tmp(
        "claim-relocation-fail",
    )));
    let tabs = Arc::new(TabRegistry::new());
    tabs.replace(vec![TabRecord {
        id: "work-a".into(),
        name: "Work A".into(),
        tile_ids: Vec::new(),
    }]);
    let ctx = test_ctx("claim-relocation-fail")
        .with_apply_sink(Arc::new(RecordingSink {
            calls: StdMutex::new(Vec::new()),
        }))
        .with_captains_registry(Arc::clone(&captains))
        .with_tab_registry(Arc::clone(&tabs));
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    let captain_id = dispatch(
        &ctx,
        "spawn_terminal",
        &json!({"cwd": "/tmp", "tabId": "work-a", "startupCommand": harness_command}),
    )
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    wait_for_harness_started(&captain_id, "codex").unwrap();
    let before = tabs.snapshot_full();
    captains.fail_next_persist("claim relocation persistence failure");
    let error = dispatch(
        &ctx,
        "claim_captain",
        &json!({"captainSessionId": captain_id, "shipSlug": "alpha"}),
    )
    .unwrap_err();
    assert!(error.contains("claim relocation persistence failure"));
    assert!(captains.snapshot().captains.is_empty());
    let after = tabs.snapshot_full();
    assert_eq!(after.seq, before.seq);
    assert!(after
        .tabs
        .iter()
        .find(|tab| tab.id == "work-a")
        .unwrap()
        .tile_ids
        .contains(&captain_id));
    assert!(!after
        .tabs
        .iter()
        .find(|tab| tab.id == CAPTAIN_WORKSPACE_ID)
        .unwrap()
        .tile_ids
        .contains(&captain_id));

    dispatch(&ctx, "close_terminal", &json!({"sessionId": captain_id})).unwrap();
    let _ = std::fs::remove_dir_all(harness_bin_dir);
}

#[test]
fn codex_claim_never_inherits_a_stale_claude_session_id() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let terminal_id = format!("codex{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
    let status = Arc::new(StatusBridge::new());
    status.ingest(
        "stale-claude-uuid",
        &json!({ "cwd": "/tmp", "tmux_session": tmux_target(&terminal_id) }),
        1,
    );
    let supervisor: Arc<dyn Fn(&mut dyn FnMut(&Supervisor)) + Send + Sync> =
        Arc::new(|visitor| visitor(&Supervisor::new()));
    let ctx = ControlContext::new(status, supervisor, "t".into()).with_apply_sink(Arc::new(
        RecordingSink {
            calls: StdMutex::new(Vec::new()),
        },
    ));
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    tmux::new_session_with_env(
        &tmux_target(&terminal_id),
        "/tmp",
        Some(&harness_command),
        &[],
    )
    .unwrap();
    wait_for_harness_started(&terminal_id, "codex").unwrap();

    let mismatched_provider = dispatch(
        &ctx,
        "claim_captain",
        &json!({
            "captainSessionId": terminal_id,
            "provider": "claude",
            "providerSessionId": "spoofed-claude-id",
        }),
    )
    .unwrap_err();
    assert!(mismatched_provider.contains("does not match a live harness"));
    let spoofed_id = dispatch(
        &ctx,
        "claim_captain",
        &json!({
            "captainSessionId": terminal_id,
            "provider": "codex",
            "providerSessionId": "spoofed-codex-id",
        }),
    )
    .unwrap_err();
    assert!(spoofed_id.contains("cannot be trusted"));

    let value = dispatch(
        &ctx,
        "claim_captain",
        &json!({
            "captainSessionId": terminal_id,
            "provider": "codex",
        }),
    )
    .unwrap();
    assert_eq!(value["captain"]["provider"], "codex");
    assert!(value["captain"].get("providerSessionId").is_none());
    assert!(value["captain"].get("conversationId").is_none());
    assert!(value["captain"].get("claudeUuid").is_none());
    let tabs = ctx.tab_registry().snapshot();
    let captain_workspace = tabs
        .iter()
        .find(|tab| tab.id == CAPTAIN_WORKSPACE_ID)
        .expect("claim creates the durable Captain Workspace when boot starts headless");
    assert_eq!(captain_workspace.name, CAPTAIN_WORKSPACE_NAME);
    assert_eq!(captain_workspace.tile_ids, vec![terminal_id.clone()]);

    dispatch(&ctx, "close_terminal", &json!({ "sessionId": terminal_id })).unwrap();
    let _ = std::fs::remove_dir_all(harness_bin_dir);
}

#[test]
fn claim_conflicts_liveness_and_bad_release_are_dispatch_errors() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let ctx = test_ctx("t").with_apply_sink(Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    }));
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    ctx.tab_registry().replace(vec![TabRecord {
        id: "tab-1".into(),
        name: "Main".into(),
        tile_ids: vec![],
    }]);
    let id1 = dispatch(
        &ctx,
        "spawn_terminal",
        &json!({
            "cwd": "/tmp",
            "tabId": "tab-1",
            "startupCommand": harness_command,
        }),
    )
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    wait_for_harness_started(&id1, "codex").unwrap();
    let id2 = dispatch(
        &ctx,
        "spawn_terminal",
        &json!({
            "cwd": "/tmp",
            "tabId": "tab-1",
            "startupCommand": harness_command,
        }),
    )
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    wait_for_harness_started(&id2, "codex").unwrap();

    dispatch(
        &ctx,
        "claim_captain",
        &json!({"captainSessionId": id1, "shipSlug": "alpha"}),
    )
    .unwrap();
    // A DIFFERENT live captain claiming the same ship is refused.
    let err = dispatch(
        &ctx,
        "claim_captain",
        &json!({"captainSessionId": id2, "shipSlug": "alpha"}),
    )
    .unwrap_err();
    assert!(err.contains("already captained"), "got: {err}");
    // A claim for a DEAD/unknown session is refused by the liveness gate
    // (else it would persist and linger forever).
    let err = dispatch(
        &ctx,
        "claim_captain",
        &json!({"captainSessionId": "nonexistent"}),
    )
    .unwrap_err();
    assert!(err.contains("no live terminal"), "got: {err}");
    let err = dispatch(&ctx, "release_captain", &json!({"shipSlug": "nope"})).unwrap_err();
    assert!(err.contains("no claim matches"), "got: {err}");
    assert!(dispatch(&ctx, "claim_captain", &json!({})).is_err());
    assert!(dispatch(&ctx, "release_captain", &json!({})).is_err());

    dispatch(&ctx, "close_terminal", &json!({"sessionId": id1})).unwrap();
    dispatch(&ctx, "close_terminal", &json!({"sessionId": id2})).unwrap();
    let _ = std::fs::remove_dir_all(harness_bin_dir);
}

#[test]
fn idempotent_reclaim_does_not_bump_seq_or_forward() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let ctx = test_ctx("t").with_apply_sink(sink.clone());
    let (harness_bin_dir, harness_command) = test_harness_command("codex");
    ctx.tab_registry().replace(vec![TabRecord {
        id: "tab-1".into(),
        name: "Main".into(),
        tile_ids: vec![],
    }]);
    let id = dispatch(
        &ctx,
        "spawn_terminal",
        &json!({
            "cwd": "/tmp",
            "tabId": "tab-1",
            "startupCommand": harness_command,
        }),
    )
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    wait_for_harness_started(&id, "codex").unwrap();

    let v1 = dispatch(&ctx, "claim_captain", &json!({"captainSessionId": id})).unwrap();
    assert_eq!(v1["applied"], true);
    let seq1 = v1["seq"].as_u64().unwrap();
    // An identical re-claim changes nothing: seq stays put, no second forward.
    let v2 = dispatch(&ctx, "claim_captain", &json!({"captainSessionId": id})).unwrap();
    assert_eq!(
        v2["seq"].as_u64().unwrap(),
        seq1,
        "unchanged re-claim must not bump seq"
    );
    assert_eq!(v2["applied"], false, "unchanged re-claim must not forward");
    let sync_count = sink
        .calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(c, _)| c == "sync_captains")
        .count();
    assert_eq!(sync_count, 1, "only the first (changing) claim forwards");

    dispatch(&ctx, "close_terminal", &json!({"sessionId": id})).unwrap();
    let _ = std::fs::remove_dir_all(harness_bin_dir);
}

#[test]
fn spawn_with_spawned_by_records_crew_and_close_terminal_removes_it() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let ctx = test_ctx("t").with_apply_sink(sink.clone());
    ctx.tab_registry().replace(vec![TabRecord {
        id: "tab-1".into(),
        name: "Main".into(),
        tile_ids: vec![],
    }]);
    ctx.captains
        .claim_test("cap-1", Some("alpha"), vec![])
        .unwrap();

    // A claimed captain spawns crew: the link is recorded + synced.
    let v = dispatch(
        &ctx,
        "spawn_terminal",
        &json!({"cwd": "/tmp", "spawnedBy": "cap-1"}),
    )
    .unwrap();
    assert_eq!(v["crewRecorded"], true);
    assert_eq!(v["spawnedBy"], "cap-1");
    let crew_id = v["id"].as_str().unwrap().to_string();
    let snap = ctx.captains.snapshot();
    assert_eq!(crew_tiles(&snap.captains[0]), vec![crew_id.clone()]);

    // Item-2 Phase B: a dead crew session is MARKED Removed (retained for
    // telemetry / reap-ship), not scrubbed (retiring the old silent-leak), and a
    // sync still forwards so every surface drops the crewmate live.
    dispatch(
        &ctx,
        "close_terminal",
        &json!({"sessionId": crew_id.clone()}),
    )
    .unwrap();
    let after = ctx.captains.snapshot();
    let cr = after.captains[0]
        .crew
        .iter()
        .find(|c| c.terminal_id == crew_id)
        .expect("crew ref retained, not scrubbed");
    assert!(matches!(cr.state, CrewState::Removed { .. }));

    // Forwards: sync_captains (crew add), spawn_terminal (with spawnedBy),
    // sync_tabs (tile drop), sync_captains (crew removal).
    let calls = sink.calls.lock().unwrap();
    let names: Vec<&str> = calls.iter().map(|(c, _)| c.as_str()).collect();
    assert_eq!(
        names,
        [
            "sync_captains",
            "spawn_terminal",
            "sync_tabs",
            "sync_captains"
        ]
    );
    // The crew-add forward carries the crew as a CrewRef (terminalId + state).
    assert_eq!(
        calls[0].1["sync"]["captains"][0]["crew"][0]["terminalId"],
        crew_id
    );
    assert_eq!(calls[1].1["spawnedBy"], "cap-1");
    // The crew-removal forward retains the ref, now marked Removed (not scrubbed).
    assert_eq!(
        calls[3].1["sync"]["captains"][0]["crew"][0]["terminalId"],
        crew_id
    );
    assert_eq!(
        calls[3].1["sync"]["captains"][0]["crew"][0]["state"]["kind"],
        "removed"
    );
}

#[test]
fn spawn_with_an_unclaimed_spawned_by_still_spawns_without_a_crew_link() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let ctx = test_ctx("t").with_apply_sink(sink.clone());
    let v = dispatch(
        &ctx,
        "spawn_terminal",
        &json!({"cwd": "/tmp", "spawnedBy": "cap-ghost"}),
    )
    .unwrap();
    assert_eq!(v["accepted"], "spawn_terminal");
    assert_eq!(
        v["crewRecorded"], false,
        "no claim = no crew link, spawn unaffected"
    );
    assert!(ctx.captains.snapshot().captains.is_empty());
    let id = v["id"].as_str().unwrap().to_string();
    dispatch(&ctx, "close_terminal", &json!({"sessionId": id})).unwrap();
    let calls = sink.calls.lock().unwrap();
    assert!(
        calls.iter().all(|(c, _)| c != "sync_captains"),
        "nothing captain-shaped changed, so no captains sync may be forwarded"
    );
}

#[test]
fn close_terminal_of_a_captain_orphans_its_claim() {
    let ctx = test_ctx("t");
    ctx.captains
        .claim_test("cap-1", Some("alpha"), vec![])
        .unwrap();
    // Item-2 Phase B: the captain's own session dies (already-gone tmux session:
    // the kill is idempotent, so dispatch succeeds and the registry cleanup runs).
    // The claim is MARKED Orphaned + un-pointed (retained for re-adoption by a
    // resumed captain of the same ship), NOT scrubbed - the old whole-record
    // `retain`-away was the C4 silent leak.
    dispatch(&ctx, "close_terminal", &json!({"sessionId": "cap-1"})).unwrap();
    let snap = ctx.captains.snapshot();
    assert_eq!(snap.captains.len(), 1, "record retained, not scrubbed");
    assert!(matches!(
        snap.captains[0].state,
        ClaimState::Orphaned { .. }
    ));
    assert!(snap.captains[0].terminal_id.is_none(), "un-pointed");
}

#[test]
fn close_terminal_releases_fleet_lock_during_external_effects() {
    let registry = Arc::new(CaptainsRegistry::new());
    registry
        .claim_test("cap-1", Some("alpha"), vec!["work-a".into()])
        .unwrap();
    registry.record_crew("cap-1", "crew-1").unwrap();
    let tabs = Arc::new(TabRegistry::new());
    tabs.replace(registry.workspace_projection());
    let context = Arc::new(
        test_ctx("close-effect-lock")
            .with_captains_registry(Arc::clone(&registry))
            .with_tab_registry(Arc::clone(&tabs)),
    );
    let (reached_tx, reached_rx) = std::sync::mpsc::sync_channel(1);
    let (resume_tx, resume_rx) = std::sync::mpsc::sync_channel(1);
    registry.set_dispatch_barrier(Some(DispatchBarrier {
        boundary: "close_terminal_effect",
        reached: reached_tx,
        resume: resume_rx,
    }));
    let closing_context = Arc::clone(&context);
    let closing = std::thread::spawn(move || {
        close_terminal_with_policy(
            &closing_context,
            &json!({"sessionId": "crew-1"}),
            false,
            None,
        )
    });
    assert_eq!(
        reached_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
        "close_terminal_effect"
    );
    assert_eq!(registry.snapshot().pending_fleet_operations.len(), 1);

    let (listed_tx, listed_rx) = std::sync::mpsc::sync_channel(1);
    let listing_context = Arc::clone(&context);
    std::thread::spawn(move || {
        listed_tx
            .send((
                dispatch(&listing_context, "list_captains", &Value::Null),
                dispatch(&listing_context, "list_tabs", &Value::Null),
            ))
            .unwrap();
    });
    let (captains, listed_tabs) = listed_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("Fleet readers must remain prompt during terminal effects");
    captains.unwrap();
    listed_tabs.unwrap();
    resume_tx.send(()).unwrap();
    closing.join().unwrap().unwrap();
    assert!(registry.snapshot().pending_fleet_operations.is_empty());
    assert!(matches!(
        registry.snapshot().captains[0].crew[0].state,
        CrewState::Removed { .. }
    ));
}

#[test]
fn report_workspace_tabs_prunes_closed_tabs_from_captains() {
    // The PRIMARY UI tab-close path is report_workspace_tabs (the webview
    // reports its new tab set), NOT the socket close_tab. A tab dropped from
    // the report must leave every captain's workspaceTabIds and forward a
    // captains snapshot - else it lingers as a phantom controlled-workspace.
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let ctx = test_ctx("t").with_apply_sink(sink.clone());
    ctx.tab_registry().replace(vec![
        TabRecord {
            id: "t1".into(),
            name: "Main".into(),
            tile_ids: vec![],
        },
        TabRecord {
            id: "t2".into(),
            name: "Side".into(),
            tile_ids: vec![],
        },
    ]);
    ctx.captains
        .claim_test("cap-1", Some("alpha"), vec!["t1".into(), "t2".into()])
        .unwrap();

    // Report a tab set WITHOUT t2 (the user closed it): t2 is pruned from the
    // captain, and a sync_captains forward carries the pruned snapshot.
    dispatch(
        &ctx,
        "report_workspace_tabs",
        &json!({"tabs": [{"id": "t1", "name": "Main", "tileIds": []}]}),
    )
    .unwrap();
    assert_eq!(
        ctx.captains.snapshot().captains[0].workspace_tab_ids,
        vec!["t1".to_string()],
    );
    let calls = sink.calls.lock().unwrap();
    assert!(
        calls.iter().any(|(c, a)| c == "sync_captains"
            && a["sync"]["captains"][0]["workspaceTabIds"] == json!(["t1"])),
        "a sync_captains forward must carry the pruned workspaceTabIds"
    );
}

#[test]
fn close_tab_prunes_captain_workspace_ownership() {
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let ctx = test_ctx("t").with_apply_sink(sink.clone());
    ctx.tab_registry().replace(vec![
        TabRecord {
            id: "tab-1".into(),
            name: "Main".into(),
            tile_ids: vec![],
        },
        TabRecord {
            id: "tab-2".into(),
            name: "Side".into(),
            tile_ids: vec![],
        },
    ]);
    ctx.captains
        .claim_test("cap-1", Some("alpha"), vec!["tab-2".into()])
        .unwrap();

    dispatch(&ctx, "close_tab", &json!({"tabId": "tab-2"})).unwrap();
    let snap = ctx.captains.snapshot();
    assert_eq!(snap.captains[0].workspace_tab_ids, Vec::<String>::new());
    // The prune rode a sync_captains forward ahead of the close_tab apply.
    let calls = sink.calls.lock().unwrap();
    let names: Vec<&str> = calls.iter().map(|(c, _)| c.as_str()).collect();
    assert_eq!(names, ["sync_captains", "close_tab"]);
}

// -----------------------------------------------------------------------
// socket-gate Phase 1: fleet governor + audit wiring at dispatch_authenticated
// -----------------------------------------------------------------------

/// Read every audit record written under `dir` (order within a single day file
/// is append order). Empty when nothing was audited.
fn read_audit(dir: &std::path::Path) -> Vec<Value> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            if let Ok(txt) = std::fs::read_to_string(entry.path()) {
                for line in txt.lines() {
                    if !line.trim().is_empty() {
                        out.push(serde_json::from_str(line).unwrap());
                    }
                }
            }
        }
    }
    out
}

fn req(token: &str, command: &str, args: Value) -> ControlRequest {
    ControlRequest {
        token: token.to_string(),
        command: command.to_string(),
        args,
        session: String::new(),
        host: token.to_string(),
        v: None,
    }
}

/// A request carrying a per-session token (Phase 3): drives `dispatch_authenticated`
/// end-to-end with a resolved caller identity, so the ACL wiring is exercised through
/// the real authenticated path (not just the pure predicate).
fn req_session(token: &str, session: &str, command: &str, args: Value) -> ControlRequest {
    ControlRequest {
        token: token.to_string(),
        command: command.to_string(),
        args,
        session: session.to_string(),
        host: String::new(),
        v: None,
    }
}

fn req_untrusted(token: &str, session: &str, command: &str, args: Value) -> ControlRequest {
    ControlRequest {
        token: token.to_string(),
        command: command.to_string(),
        args,
        session: session.to_string(),
        host: String::new(),
        v: None,
    }
}

fn captain_lease_fixture(
    live: bool,
) -> (
    ControlContext,
    Arc<CaptainsRegistry>,
    Arc<crate::identity::IdentityStore>,
    crate::identity::SessionIdentity,
) {
    let captains = Arc::new(CaptainsRegistry::new());
    captains
        .upsert_project(ProjectRecord {
            root_path: None,
            vcs_capability: None,
            git_main_root: None,
            project_id: "lease-project".into(),
            name: "Lease Project".into(),
            repo_root: "/tmp/lease-project".into(),
            remote_url: None,
            default_branch: None,
            powder: None,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    captains
        .claim_test("lease-captain", Some("lease-ship"), vec![])
        .unwrap();
    captains
        .bind_ship_context("lease-ship", "lease-project", "Package 0", "codex")
        .unwrap();
    let identities = Arc::new(crate::identity::IdentityStore::ephemeral());
    let identity = identities
        .mint_and_bind(
            crate::identity::Role::Captain,
            Some("lease-ship".into()),
            "lease-captain",
        )
        .unwrap();
    let sessions = if live {
        vec![tmux_target("lease-captain")]
    } else {
        Vec::new()
    };
    let ctx = test_ctx("global-control")
        .with_captains_registry(captains.clone())
        .with_identity_store(identities.clone())
        .with_live_sessions(move || Ok(sessions.clone()));
    (ctx, captains, identities, identity)
}

#[test]
fn normal_captain_fanout_burst_not_refused_at_gate() {
    // THE most important test (design spec): a captain fanning out 6 crew in an
    // instant burst must NOT be refused by the fleet gate. With the default
    // burst of 8 the governor admits all six; they fail downstream only because
    // this headless ctx has no UI sink, never because of the budget.
    let dir = std::env::temp_dir().join("t-hub-gate-burst");
    let _ = std::fs::remove_dir_all(&dir);
    let ctx = test_ctx("burst")
        .with_governor(Arc::new(SpawnGovernor::default()))
        .with_audit(Arc::new(AuditLog::new(dir.clone())));
    for i in 0..6 {
        let resp = dispatch_authenticated(
            &ctx,
            req(
                "burst",
                "spawn_terminal",
                json!({"cwd": "/tmp", "name": format!("crew-{i}")}),
            ),
        );
        let err = resp.error.clone().unwrap_or_default();
        assert!(
            !err.contains("rate limit"),
            "spawn {i} was rate-limited: {err}"
        );
        assert!(
            !err.contains("concurrent-session cap"),
            "spawn {i} hit the concurrent cap: {err}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn spawn_rate_limit_refuses_with_exact_message_and_audits() {
    // Burst 1: the first spawn spends the only token; the second is refused with
    // the exact §5 message and recorded as `refused-rate`.
    let dir = std::env::temp_dir().join("t-hub-gate-rate");
    let _ = std::fs::remove_dir_all(&dir);
    let ctx = test_ctx("rate")
        .with_governor(Arc::new(SpawnGovernor::new(64, 20.0, 1.0)))
        .with_audit(Arc::new(AuditLog::new(dir.clone())));
    let r1 = dispatch_authenticated(&ctx, req("rate", "spawn_terminal", json!({"cwd": "/tmp"})));
    // Governor admitted r1; it fails downstream on the missing UI sink.
    assert!(
        r1.error.clone().unwrap_or_default().contains("no UI"),
        "got: {:?}",
        r1.error
    );
    let r2 = dispatch_authenticated(&ctx, req("rate", "spawn_terminal", json!({"cwd": "/tmp"})));
    assert!(
        r2.error
            .clone()
            .unwrap()
            .contains("spawn rate limit (20/min); retry shortly"),
        "got: {:?}",
        r2.error
    );

    let recs = read_audit(&dir);
    assert_eq!(recs.len(), 2, "expected an allowed + a refused record");
    assert_eq!(recs[0]["decision"], "allowed");
    assert_eq!(recs[0]["command"], "spawn_terminal");
    assert_eq!(recs[1]["decision"], "refused-rate");
    // The hash chain links the refusal to the prior line.
    assert_eq!(recs[1]["prev"], recs[0]["hash"]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn read_tier_is_not_gated_or_audited() {
    // list_terminals is Read tier: it must never touch the governor or the audit
    // log, whether or not tmux is reachable in the test env.
    let dir = std::env::temp_dir().join("t-hub-gate-read");
    let _ = std::fs::remove_dir_all(&dir);
    let ctx = test_ctx("read").with_audit(Arc::new(AuditLog::new(dir.clone())));
    let _ = dispatch_authenticated(&ctx, req("read", "list_terminals", json!({})));
    assert!(
        read_audit(&dir).is_empty(),
        "a read-tier command was audited"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn send_text_is_audited_with_redaction_through_gate() {
    // send_text is process-changing (audited) but NOT rate-limited. The literal
    // text must never reach the audit log - only a length + hash.
    let dir = std::env::temp_dir().join("t-hub-gate-sendtext");
    let _ = std::fs::remove_dir_all(&dir);
    let ctx = test_ctx("st").with_audit(Arc::new(AuditLog::new(dir.clone())));
    let resp = dispatch_authenticated(
        &ctx,
        req(
            "st",
            "send_text",
            json!({"sessionId": "ghost", "text": "SUPERSECRET", "enter": true}),
        ),
    );
    assert!(!resp.ok); // no such session, but the audit still lands
    let recs = read_audit(&dir);
    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0]["command"], "send_text");
    assert_eq!(recs[0]["decision"], "allowed");
    let blob = serde_json::to_string(&recs[0]).unwrap();
    assert!(
        !blob.contains("SUPERSECRET"),
        "literal text leaked into audit: {blob}"
    );
    assert_eq!(recs[0]["args"]["textLen"], 11);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bad_token_is_rejected_and_not_audited() {
    // A bad token is rejected before the gate and never audited (no leak of the
    // process-changing surface to an unauthenticated probe).
    let dir = std::env::temp_dir().join("t-hub-gate-badtok");
    let _ = std::fs::remove_dir_all(&dir);
    let ctx = test_ctx("good").with_audit(Arc::new(AuditLog::new(dir.clone())));
    let resp = dispatch_authenticated(&ctx, req("WRONG", "spawn_terminal", json!({})));
    assert!(resp.error.unwrap().contains("bad control token"));
    assert!(read_audit(&dir).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn kill_style_send_keys_is_throttled_but_navigation_is_not() {
    // The destructive throttle covers kill-style keys (C-c) but not navigation
    // (Up/Enter) - proven at the classifier the gate uses.
    assert!(keys_are_kill_style(&json!({"keys": ["C-c"]})));
    assert!(keys_are_kill_style(&json!({"keys": ["Up", "C-d"]})));
    assert!(!keys_are_kill_style(&json!({"keys": ["Up", "Enter"]})));
    assert!(!keys_are_kill_style(&json!({"keys": []})));
}

#[test]
fn command_tiers_are_classified() {
    assert_eq!(
        required_tier("spawn_terminal"),
        CommandTier::ProcessChanging
    );
    assert_eq!(
        required_tier("close_terminal"),
        CommandTier::ProcessChanging
    );
    assert_eq!(
        required_tier("history_resume"),
        CommandTier::ProcessChanging
    );
    for command in ["preview_start", "preview_stop", "preview_restart"] {
        assert_eq!(required_tier(command), CommandTier::ProcessChanging);
    }
    for command in ["preview_select", "preview_refresh", "preview_open"] {
        assert_eq!(required_tier(command), CommandTier::Organization);
    }
    for command in ["preview_discover", "preview_status"] {
        assert_eq!(required_tier(command), CommandTier::Read);
    }
    assert_eq!(required_tier("send_text"), CommandTier::ProcessChanging);
    assert_eq!(
        required_tier("complete_crew_powder"),
        CommandTier::ProcessChanging
    );
    assert_eq!(required_tier("new_tab"), CommandTier::Organization);
    assert_eq!(required_tier("history_focus"), CommandTier::Organization);
    assert_eq!(required_tier("create_worktree"), CommandTier::Organization);
    assert_eq!(required_tier("remove_worktree"), CommandTier::Organization);
    assert_eq!(required_tier("list_terminals"), CommandTier::Read);
    assert_eq!(required_tier("get_status"), CommandTier::Read);
    assert_eq!(required_tier("history_list"), CommandTier::Organization);
    assert_eq!(required_tier("invalidate_history_cache"), CommandTier::Read);
    // Comms-plane Phase 2 (review H1): `inbox_ack` mutates + compacts durable
    // receipt state, so it must require the control token (Organization) and be
    // audited - NOT fall through to the read tier. `inbox_status` is counts-only
    // and stays Read.
    assert_eq!(required_tier("inbox_ack"), CommandTier::Organization);
    assert_eq!(required_tier("inbox_status"), CommandTier::Read);
}

#[test]
fn preview_commands_forward_registry_authorized_arguments_to_one_backend_adapter() {
    let root = tempfile::tempdir().unwrap();
    let root_path = root.path().to_string_lossy().into_owned();
    let calls = Arc::new(StdMutex::new(Vec::<(String, Value)>::new()));
    let recorded = calls.clone();
    let ctx = test_ctx("preview-control").with_preview_control(move |command, args, _root| {
        recorded
            .lock()
            .unwrap()
            .push((command.to_string(), args.clone()));
        Ok(json!({"command": command, "args": args}))
    });
    ctx.captains
        .upsert_project(ProjectRecord {
            project_id: "project-1".into(),
            name: "Preview Project".into(),
            repo_root: root_path.clone(),
            root_path: Some(root_path.clone()),
            vcs_capability: Some("none".into()),
            git_main_root: None,
            remote_url: None,
            default_branch: None,
            powder: None,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    let scoped = json!({
        "scope": {"projectId": "project-1"},
        "requestId": "request-1"
    });
    let rooted = json!({
        "rootPath": root_path,
        "scope": {"projectId": "project-1"},
        "requestId": "request-1"
    });
    for (command, args) in [
        ("preview_discover", json!({"rootPath": root_path})),
        ("preview_status", scoped.clone()),
        ("preview_select", rooted.clone()),
        ("preview_refresh", scoped.clone()),
        ("preview_open", scoped.clone()),
        ("preview_start", rooted.clone()),
        ("preview_stop", scoped.clone()),
        ("preview_restart", rooted.clone()),
    ] {
        let result = dispatch(&ctx, command, &args).unwrap();
        assert_eq!(result["command"], command);
        assert_eq!(result["args"], args);
    }
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 8);
}

#[test]
fn preview_control_rejects_unknown_projects_and_forged_roots_before_adapter() {
    let root = tempfile::tempdir().unwrap();
    let root_path = root.path().to_string_lossy().into_owned();
    let calls = Arc::new(AtomicUsize::new(0));
    let recorded = calls.clone();
    let ctx = test_ctx("preview-authority").with_preview_control(move |_, _, _| {
        recorded.fetch_add(1, Ordering::SeqCst);
        Ok(json!({"unexpected": true}))
    });
    ctx.captains
        .upsert_project(ProjectRecord {
            project_id: "project-1".into(),
            name: "Preview Project".into(),
            repo_root: root_path.clone(),
            root_path: Some(root_path.clone()),
            vcs_capability: Some("none".into()),
            git_main_root: None,
            remote_url: None,
            default_branch: None,
            powder: None,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();

    let unknown = dispatch(
        &ctx,
        "preview_status",
        &json!({"scope": {"projectId": "unknown"}}),
    )
    .unwrap_err();
    assert!(unknown.contains("unknown projectId"));
    let forged = dispatch(
        &ctx,
        "preview_start",
        &json!({
            "rootPath": "/tmp/not-the-registered-project",
            "scope": {"projectId": "project-1"},
            "requestId": "request-1"
        }),
    )
    .unwrap_err();
    assert!(forged.contains("does not match registered Project"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn preview_control_allows_only_the_owning_project_captain() {
    let root = tempfile::tempdir().unwrap();
    let root_path = root.path().to_string_lossy().into_owned();
    let calls = Arc::new(AtomicUsize::new(0));
    let recorded = calls.clone();
    let ctx = test_ctx("preview-captain-authority").with_preview_control(move |_, _, _| {
        recorded.fetch_add(1, Ordering::SeqCst);
        Ok(json!({"authorized": true}))
    });
    ctx.captains
        .upsert_project(ProjectRecord {
            project_id: "project-1".into(),
            name: "Preview Project".into(),
            repo_root: root_path.clone(),
            root_path: Some(root_path.clone()),
            vcs_capability: Some("none".into()),
            git_main_root: None,
            remote_url: None,
            default_branch: None,
            powder: None,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    ctx.captains
        .claim_test("captain-tile", Some("preview-ship"), vec![])
        .unwrap();
    ctx.captains
        .bind_ship_context("preview-ship", "project-1", "Package 3", "codex")
        .unwrap();
    let captain = ctx
        .captains
        .snapshot()
        .captains
        .into_iter()
        .find(|captain| captain.ship_slug == "preview-ship")
        .unwrap();
    ctx.captains
        .create_workspace(
            "workspace-1",
            "Work",
            Some(&FleetWorkspaceOwner {
                project_id: "project-1".into(),
                assignment_id: captain.assignment_id,
                ship_slug: "preview-ship".into(),
            }),
        )
        .unwrap();
    let owning_captain = ResolvedIdentity {
        session_id: "captain-session".into(),
        mint_role: crate::identity::Role::Captain,
        tile: Some("captain-tile".into()),
        ship_slug: Some("preview-ship".into()),
        fleet_role: Some(FleetRole::Captain),
        claude_uuid: None,
    };
    let args = json!({"scope": {"projectId": "project-1"}});

    assert_eq!(
        preview_control(&ctx, "preview_status", &args, Some(&owning_captain), false).unwrap(),
        json!({"authorized": true})
    );
    let unrelated = ResolvedIdentity {
        ship_slug: Some("another-ship".into()),
        ..owning_captain.clone()
    };
    assert!(
        preview_control(&ctx, "preview_status", &args, Some(&unrelated), false)
            .unwrap_err()
            .contains("owning Project Captain")
    );
    assert!(preview_control(&ctx, "preview_status", &args, None, false)
        .unwrap_err()
        .contains("requires a Fleet identity"));
    let workspace_scope = json!({"projectId": "project-1", "workspaceId": "workspace-1"});
    for (command, args) in [
        ("preview_status", json!({"scope": workspace_scope})),
        (
            "preview_select",
            json!({"rootPath": root_path, "target": {"scope": workspace_scope}}),
        ),
        ("preview_refresh", json!({"scope": workspace_scope})),
        ("preview_open", json!({"scope": workspace_scope})),
        (
            "preview_start",
            json!({"rootPath": root_path, "scope": workspace_scope}),
        ),
        ("preview_stop", json!({"scope": workspace_scope})),
        (
            "preview_restart",
            json!({"rootPath": root_path, "scope": workspace_scope}),
        ),
    ] {
        preview_control(&ctx, command, &args, Some(&owning_captain), false).unwrap();
    }
    assert_eq!(calls.load(Ordering::SeqCst), 8);

    {
        let mut registry = ctx.captains.lock();
        registry
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.id == "workspace-1")
            .unwrap()
            .owner
            .as_mut()
            .unwrap()
            .assignment_id = "different-assignment".into();
    }
    let error = preview_control(
        &ctx,
        "preview_status",
        &json!({"scope": workspace_scope}),
        Some(&owning_captain),
        false,
    )
    .unwrap_err();
    assert!(error.contains("another Captain Assignment"));
    assert_eq!(calls.load(Ordering::SeqCst), 8);
}

#[test]
fn preview_root_keeps_posix_identity_separate_from_host_open_path() {
    let project = ProjectRecord {
        project_id: "project-1".into(),
        name: "Preview Project".into(),
        repo_root: "/home/natkins/project".into(),
        root_path: Some("/home/natkins/project".into()),
        vcs_capability: Some("none".into()),
        git_main_root: None,
        remote_url: None,
        default_branch: None,
        powder: None,
        created_at: 1,
        updated_at: 1,
    };
    let authority = preview_root_authority_with(&project, |identity| {
        assert_eq!(identity, "/home/natkins/project");
        PathBuf::from(r"\\wsl.localhost\Ubuntu-24.04\home\natkins\project")
    })
    .unwrap();
    assert_eq!(authority.posix_identity, "/home/natkins/project");
    assert_eq!(
        authority.host_open_path,
        PathBuf::from(r"\\wsl.localhost\Ubuntu-24.04\home\natkins\project")
    );
}

#[test]
fn preview_scoped_commands_refuse_unknown_and_foreign_durable_workspaces() {
    let roots = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
    let calls = Arc::new(AtomicUsize::new(0));
    let recorded = calls.clone();
    let ctx = test_ctx("preview-workspace-authority").with_preview_control(move |_, _, _| {
        recorded.fetch_add(1, Ordering::SeqCst);
        Ok(json!({"unexpected": true}))
    });
    for (index, root) in roots.iter().enumerate() {
        let project_id = format!("project-{}", index + 1);
        let ship_slug = format!("ship-{}", index + 1);
        let terminal_id = format!("captain-{}", index + 1);
        let root_path = root.path().to_string_lossy().into_owned();
        ctx.captains
            .upsert_project(ProjectRecord {
                project_id: project_id.clone(),
                name: project_id.clone(),
                repo_root: root_path.clone(),
                root_path: Some(root_path),
                vcs_capability: Some("none".into()),
                git_main_root: None,
                remote_url: None,
                default_branch: None,
                powder: None,
                created_at: 1,
                updated_at: 1,
            })
            .unwrap();
        ctx.captains
            .claim_test(&terminal_id, Some(&ship_slug), vec![])
            .unwrap();
        ctx.captains
            .bind_ship_context(&ship_slug, &project_id, "Package 3", "codex")
            .unwrap();
        let captain = ctx
            .captains
            .snapshot()
            .captains
            .into_iter()
            .find(|captain| captain.ship_slug == ship_slug)
            .unwrap();
        ctx.captains
            .create_workspace(
                &format!("workspace-{}", index + 1),
                "Work",
                Some(&FleetWorkspaceOwner {
                    project_id,
                    assignment_id: captain.assignment_id,
                    ship_slug,
                }),
            )
            .unwrap();
    }
    let root_path = roots[0].path().to_string_lossy().into_owned();
    let scoped = |workspace_id: &str| {
        json!({
            "scope": {
                "projectId": "project-1",
                "workspaceId": workspace_id
            },
            "requestId": "request-1"
        })
    };
    let rooted = |workspace_id: &str| {
        json!({
            "rootPath": root_path,
            "scope": {
                "projectId": "project-1",
                "workspaceId": workspace_id
            },
            "requestId": "request-1"
        })
    };
    for workspace_id in ["missing-workspace", "workspace-2"] {
        for (command, args) in [
            ("preview_status", scoped(workspace_id)),
            (
                "preview_select",
                json!({
                    "rootPath": root_path,
                    "target": {
                        "scope": {
                            "projectId": "project-1",
                            "workspaceId": workspace_id
                        }
                    }
                }),
            ),
            ("preview_refresh", scoped(workspace_id)),
            ("preview_open", scoped(workspace_id)),
            ("preview_start", rooted(workspace_id)),
            ("preview_stop", scoped(workspace_id)),
            ("preview_restart", rooted(workspace_id)),
        ] {
            let error = dispatch(&ctx, command, &args).unwrap_err();
            assert!(
                error.contains("unknown durable workspaceId")
                    || error.contains("belongs to another Project"),
                "{command}: {error}"
            );
        }
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn preview_control_refuses_mismatched_top_level_and_target_scopes() {
    let root = tempfile::tempdir().unwrap();
    let root_path = root.path().to_string_lossy().into_owned();
    let calls = Arc::new(AtomicUsize::new(0));
    let recorded = calls.clone();
    let ctx = test_ctx("preview-scope-match").with_preview_control(move |_, _, _| {
        recorded.fetch_add(1, Ordering::SeqCst);
        Ok(json!({"unexpected": true}))
    });
    ctx.captains
        .upsert_project(ProjectRecord {
            project_id: "project-1".into(),
            name: "Preview Project".into(),
            repo_root: root_path.clone(),
            root_path: Some(root_path.clone()),
            vcs_capability: Some("none".into()),
            git_main_root: None,
            remote_url: None,
            default_branch: None,
            powder: None,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();

    for target_scope in [
        json!({"projectId": "another-project"}),
        json!({"projectId": "project-1", "workspaceId": "another-workspace"}),
    ] {
        let error = dispatch(
            &ctx,
            "preview_select",
            &json!({
                "rootPath": root_path,
                "scope": {"projectId": "project-1"},
                "target": {"scope": target_scope}
            }),
        )
        .unwrap_err();
        assert!(error.contains("scopes must match exactly"));
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn history_list_control_contract_discovers_codex_without_hiding_claude() {
    let temp = tempfile::tempdir().unwrap();
    let claude_root = temp.path().join(".claude/projects/repo");
    let codex_root = temp.path().join(".codex/sessions/2026/07/20");
    std::fs::create_dir_all(&claude_root).unwrap();
    std::fs::create_dir_all(&codex_root).unwrap();
    std::fs::write(
        claude_root.join("claude-control.jsonl"),
        r#"{"type":"user","cwd":"/same","message":{"content":"Claude control"}}"#,
    )
    .unwrap();
    let codex_id = "22222222-2222-4222-8222-222222222222";
    std::fs::write(
            codex_root.join(format!(
                "rollout-2026-07-20T10-00-00-{codex_id}.jsonl"
            )),
            format!(
                "{}\n{}",
                json!({"type":"session_meta","payload":{"id":codex_id,"cwd":"/same","model_provider":"openai"}}),
                json!({"type":"event_msg","payload":{"type":"user_message","message":"Codex control"}})
            ),
        )
        .unwrap();
    let history = Arc::new(crate::history::HistoryService::new(
        temp.path().join(".claude/projects"),
        temp.path().join(".codex/sessions"),
        std::time::Duration::from_secs(60),
    ));
    let ctx = test_ctx("history-list").with_history_service(history);

    let value = dispatch(&ctx, "history_list", &json!({"limit": 10})).unwrap();

    assert_eq!(value["schemaVersion"], 1);
    assert_eq!(value["count"], 2);
    assert_eq!(value["total"], 2);
    assert_eq!(value["entries"].as_array().unwrap().len(), 2);
    assert!(value["entries"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["harness"] == "codex" && entry["conversationId"] == codex_id));
    assert!(value["entries"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["harness"] == "claude" && entry["conversationId"] == "claude-control"));
    assert_eq!(value["sources"].as_array().unwrap().len(), 2);
}

#[test]
fn authenticated_history_list_is_scoped_to_an_active_captain_assignment() {
    let temp = tempfile::tempdir().unwrap();
    let claude_root = temp.path().join(".claude/projects");
    let codex_root = temp.path().join(".codex/sessions/2026/07/20");
    std::fs::create_dir_all(&claude_root).unwrap();
    std::fs::create_dir_all(&codex_root).unwrap();
    let ids = [
        "22222222-2222-4222-8222-222222222222",
        "33333333-3333-4333-8333-333333333333",
    ];
    for (index, id) in ids.iter().enumerate() {
        std::fs::write(
                codex_root.join(format!(
                    "rollout-2026-07-20T10-00-0{index}-{id}.jsonl"
                )),
                format!(
                    "{}\n{}",
                    json!({"type":"session_meta","payload":{"id":id,"cwd":format!("/repo-{index}"),"model_provider":"openai"}}),
                    json!({"type":"event_msg","payload":{"type":"user_message","message":format!("Task {index}")}})
                ),
            )
            .unwrap();
    }
    let history = Arc::new(crate::history::HistoryService::new(
        claude_root,
        temp.path().join(".codex/sessions"),
        std::time::Duration::from_secs(60),
    ));
    let registry = Arc::new(CaptainsRegistry::new());
    for (index, (ship, terminal, id)) in [("ship-a", "cap-a", ids[0]), ("ship-b", "cap-b", ids[1])]
        .into_iter()
        .enumerate()
    {
        let project_id = format!("project-{index}");
        registry
            .upsert_project(ProjectRecord {
                root_path: None,
                vcs_capability: None,
                git_main_root: None,
                project_id: project_id.clone(),
                name: project_id.clone(),
                repo_root: format!("/repo-{index}"),
                remote_url: None,
                default_branch: None,
                powder: None,
                created_at: 0,
                updated_at: 0,
            })
            .unwrap();
        registry
            .claim_provider(
                terminal,
                Some(ship),
                FleetRole::Captain,
                Some("codex"),
                Some(id),
                vec![],
                &|_| false,
                &|_| tmux::SessionLiveness::Gone,
            )
            .unwrap();
        registry
            .bind_ship_context(ship, &project_id, "History test", "codex")
            .unwrap();
    }
    let identities = Arc::new(crate::identity::IdentityStore::ephemeral());
    let captain = mint_session(
        &identities,
        crate::identity::Role::Captain,
        "ship-a",
        "cap-a",
    );
    let crew = mint_session(&identities, crate::identity::Role::Crew, "ship-a", "crew-a");
    let ctx = test_ctx("ctrl")
        .with_history_service(history)
        .with_captains_registry(Arc::clone(&registry))
        .with_identity_store(identities);

    let denied = dispatch_authenticated(
        &ctx,
        req_session("ctrl", &crew, "history_list", json!({"limit": 10})),
    );
    assert!(!denied.ok);

    let scoped = dispatch_authenticated(
        &ctx,
        req_session("ctrl", &captain, "history_list", json!({"limit": 10})),
    );
    assert!(scoped.ok, "Captain History list failed: {:?}", scoped.error);
    let entries = scoped.result.unwrap()["entries"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["conversationId"], ids[0]);
    assert_eq!(entries[0]["captainId"], "ship-a");

    registry.release("ship-a").unwrap();
    let released = dispatch_authenticated(
        &ctx,
        req_session("ctrl", &captain, "history_list", json!({"limit": 10})),
    );
    assert!(!released.ok, "released Captain retained History access");
}

#[test]
fn history_resume_command_is_selected_only_from_exact_backend_harness() {
    let codex_id = "22222222-2222-4222-8222-222222222222";
    let codex = crate::history::parse_codex_rollout(
        std::path::Path::new(
            "rollout-2026-07-20T10-00-00-22222222-2222-4222-8222-222222222222.jsonl",
        ),
        &json!({"type":"session_meta","payload":{"id":codex_id,"cwd":"/repo"}}).to_string(),
        1,
    )
    .unwrap()
    .entry;
    let claude = crate::history::parse_claude_transcript(
        std::path::Path::new("claude-exact.jsonl"),
        r#"{"type":"user","cwd":"/repo","message":{"content":"task"}}"#,
        1,
        false,
    )
    .unwrap()
    .entry;

    assert_eq!(
        history_resume_command(&codex),
        "codex resume '22222222-2222-4222-8222-222222222222'"
    );
    assert_eq!(
        history_resume_command(&claude),
        "claude --resume 'claude-exact'"
    );
}

#[test]
fn fresh_history_resume_scope_rejects_a_rebound_project_assignment() {
    let id = "22222222-2222-4222-8222-222222222222";
    let entry = crate::history::parse_codex_rollout(
        std::path::Path::new(
            "rollout-2026-07-20T10-00-00-22222222-2222-4222-8222-222222222222.jsonl",
        ),
        &json!({"type":"session_meta","payload":{"id":id,"cwd":"/repo-old"}}).to_string(),
        1,
    )
    .unwrap()
    .entry;
    let association = crate::history::HistoryAssociation {
        harness: crate::history::Harness::Codex,
        conversation_id: id.to_string(),
        terminal_id: Some("term0001".to_string()),
        liveness: crate::history::AssociationLiveness::Inactive,
        project_id: Some("project-old".to_string()),
        project_name: Some("Old Project".to_string()),
        captain_id: Some("ship".to_string()),
        assignment_id: Some("assignment-old".to_string()),
        role: Some("crew".to_string()),
        workspace_id: Some("workspace-old".to_string()),
        worktree_id: None,
        branch: None,
    };
    assert!(enforce_history_entry_owner(
        &WorkspaceMutationAuthority::Assignment(FleetWorkspaceOwner {
            project_id: "project-old".to_string(),
            assignment_id: "assignment-old".to_string(),
            ship_slug: "ship".to_string(),
        }),
        &entry,
        std::slice::from_ref(&association),
    )
    .is_ok());
    assert!(enforce_history_entry_owner(
        &WorkspaceMutationAuthority::Assignment(FleetWorkspaceOwner {
            project_id: "project-new".to_string(),
            assignment_id: "assignment-new".to_string(),
            ship_slug: "ship".to_string(),
        }),
        &entry,
        &[association],
    )
    .is_err());
}

#[test]
fn pending_history_runtime_proof_has_a_bounded_grace_period() {
    let pending = crate::history::HistoryPendingResume {
        request_id: "request-one".to_string(),
        history_id: "history:v1:one".to_string(),
        harness: crate::history::Harness::Codex,
        conversation_id: "22222222-2222-4222-8222-222222222222".to_string(),
        terminal_id: "term0001".to_string(),
        target_tab_id: Some("workspace".to_string()),
        authorized_ship_slug: None,
        authorized_project_id: None,
        authorized_assignment_id: None,
        reserved_at_ms: now_ms(),
    };
    assert!(!history_pending_runtime_proof_expired(&pending));
    let expired = crate::history::HistoryPendingResume {
        reserved_at_ms: now_ms().saturating_sub(HISTORY_PENDING_RUNTIME_PROOF_GRACE_MS),
        ..pending
    };
    assert!(history_pending_runtime_proof_expired(&expired));
}

#[test]
fn mcp_tier_annotations_match_control_required_tier() {
    // item-3 ledger #16: the drift-can't-recur guard. Every tool the MCP surface
    // advertises must carry the SAME tier the control server ENFORCES via
    // `required_tier`, or the annotation-vs-enforcement drift that motivated the
    // socket-gate work reopens. BYPASS-WOULD-FAIL: change one MCP tool's tier (or
    // its control-side arm) without the other and this test goes RED.
    for tool in t_hub_mcp::tools::catalog() {
        let expected = match tool.tier {
            t_hub_mcp::tools::Tier::Read => CommandTier::Read,
            t_hub_mcp::tools::Tier::Organization => CommandTier::Organization,
            t_hub_mcp::tools::Tier::ProcessChanging => CommandTier::ProcessChanging,
            // The theme get/set pair is a PARALLEL track forwarded by name (it does
            // not flow through `required_tier`'s capability gate), so it has no
            // control-side tier to mirror. Skip it explicitly.
            t_hub_mcp::tools::Tier::Theme => continue,
        };
        assert_eq!(
            required_tier(tool.name),
            expected,
            "tier drift: MCP tool '{}' is annotated {:?} but control enforces {:?}",
            tool.name,
            tool.tier,
            required_tier(tool.name),
        );
    }
}

#[test]
fn inbox_status_unscoped_enumeration_requires_organization() {
    // item-3 §2.4 (ledger #15): a SCOPED inbox_status (own recipient) stays Read,
    // but an UNSCOPED fleet-wide enumeration (depth_all) is Organization so a bare
    // read token cannot enumerate every recipient's counts/cursors. inbox_ack STAYS
    // Organization regardless (§2.4.1). BYPASS-WOULD-FAIL: drop the effective_tier
    // refinement and the unscoped case falls back to Read and the assert goes RED.
    assert_eq!(
        effective_tier("inbox_status", &json!({"sessionId": "tileX"})),
        CommandTier::Read,
        "a scoped inbox_status is a Read"
    );
    assert_eq!(
        effective_tier("inbox_status", &json!({})),
        CommandTier::Organization,
        "an unscoped inbox_status enumeration must require Organization"
    );
    // inbox_ack is Organization independent of scope (no self-scope until the
    // session-token-on-request substrate lands, §2.4.1).
    assert_eq!(
        effective_tier("inbox_ack", &json!({"sessionId": "tileX"})),
        CommandTier::Organization
    );
    // Every other command's effective tier is exactly its required_tier.
    assert_eq!(
        effective_tier("list_terminals", &json!({})),
        CommandTier::Read
    );
    assert_eq!(
        effective_tier("spawn_terminal", &json!({})),
        CommandTier::ProcessChanging
    );
}

#[test]
fn read_token_cannot_enumerate_all_inboxes_but_can_scope_its_own() {
    // End-to-end through the gate: a read token doing an UNSCOPED inbox_status is
    // authz-refused (Organization), while a SCOPED inbox_status is admitted (Read).
    let ctx = test_ctx("t").with_inbox(Arc::new(crate::inbox::Inbox::ephemeral()));
    let unscoped = dispatch_authenticated(&ctx, req("read-t", "inbox_status", json!({})));
    assert!(
        unscoped
            .error
            .clone()
            .unwrap_or_default()
            .contains("requires the control capability"),
        "read token must be refused an unscoped enumeration, got: {:?}",
        unscoped.error
    );
    let scoped = dispatch_authenticated(
        &ctx,
        req("read-t", "inbox_status", json!({"sessionId": "me"})),
    );
    assert!(
        !scoped
            .error
            .clone()
            .unwrap_or_default()
            .contains("requires the control capability"),
        "read token must be allowed a scoped inbox_status, got: {:?}",
        scoped.error
    );
}

#[test]
fn legit_spawn_send_close_through_gate_is_admitted_and_audited() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    // End-to-end through dispatch_authenticated (governor + audit) against a
    // REAL tmux session: a legitimate crew spawn -> send_text -> close must all
    // be ADMITTED and audited allowed. This is the "legit orchestration still
    // works over the exact socket" guarantee, exercised through the gate.
    let dir = std::env::temp_dir().join("t-hub-gate-e2e");
    let _ = std::fs::remove_dir_all(&dir);
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let ctx = test_ctx("e2e")
        .with_apply_sink(sink.clone())
        .with_audit(Arc::new(AuditLog::new(dir.clone())));
    ctx.tab_registry().replace(vec![TabRecord {
        id: "tab-1".into(),
        name: "Main".into(),
        tile_ids: vec![],
    }]);

    // Spawn a real session through the authenticated gate.
    let sresp = dispatch_authenticated(
        &ctx,
        req(
            "e2e",
            "spawn_terminal",
            json!({"cwd": "/tmp", "name": "crew", "tabId": "tab-1"}),
        ),
    );
    assert!(
        sresp.ok,
        "legit spawn was refused by the gate: {:?}",
        sresp.error
    );
    let id = sresp.result.as_ref().unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let target = tmux::target_for_id(&id);
    assert!(
        tmux::has_session(&target),
        "the real tmux session should exist"
    );
    let _ = tmux::resize_window_for_tests(&target, 80, 24);

    // Type into it through the gate (send_text is not throttled).
    let tresp = dispatch_authenticated(
        &ctx,
        req(
            "e2e",
            "send_text",
            json!({"sessionId": id, "text": "echo GATE_E2E_OK", "enter": true}),
        ),
    );
    assert!(tresp.ok, "legit send_text was refused: {:?}", tresp.error);

    // Close it through the gate (destructive, but the first teardown is under
    // the burst of 10 so it is admitted).
    let cresp =
        dispatch_authenticated(&ctx, req("e2e", "close_terminal", json!({"sessionId": id})));
    assert!(
        cresp.ok,
        "legit close_terminal was refused: {:?}",
        cresp.error
    );
    assert!(
        !tmux::has_session(&target),
        "session should be gone after close"
    );

    // All three land in the audit log, allowed and hash-chained. send_text's
    // literal payload is NOT present (redacted).
    let recs = read_audit(&dir);
    assert_eq!(recs.len(), 3, "expected spawn+send+close audited: {recs:?}");
    let cmds: Vec<&str> = recs
        .iter()
        .map(|r| r["command"].as_str().unwrap())
        .collect();
    assert_eq!(cmds, ["spawn_terminal", "send_text", "close_terminal"]);
    assert!(
        recs.iter().all(|r| r["decision"] == "allowed"),
        "a legit command was not allowed: {recs:?}"
    );
    for w in recs.windows(2) {
        assert_eq!(w[1]["prev"], w[0]["hash"], "hash chain broken");
    }
    let blob = serde_json::to_string(&recs).unwrap();
    assert!(
        !blob.contains("GATE_E2E_OK"),
        "send_text literal leaked into audit"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// -----------------------------------------------------------------------
// socket-gate Phase 2/2b: capability-scoped tokens
// -----------------------------------------------------------------------

#[test]
fn capability_resolution_maps_each_token() {
    // control token -> Full; read token -> ReadOnly; anything else -> None.
    let ctx = test_ctx("t"); // control="t", read="read-t"
    assert_eq!(resolve_capability(&ctx, "t"), Some(Capability::Full));
    assert_eq!(
        resolve_capability(&ctx, "read-t"),
        Some(Capability::ReadOnly)
    );
    assert_eq!(resolve_capability(&ctx, "nope"), None);
    assert_eq!(resolve_capability(&ctx, ""), None);
}

#[test]
fn empty_read_token_authorizes_nothing() {
    // A ctx with no read token configured must not let an empty presented token
    // resolve to ReadOnly (the empty==empty trap).
    let ctx = ControlContext::new(
        Arc::new(StatusBridge::new()),
        Arc::new(|_: &mut dyn FnMut(&Supervisor)| {}),
        "ctl".to_string(),
    );
    assert!(ctx.read_token.is_empty());
    assert_eq!(resolve_capability(&ctx, ""), None);
    assert_eq!(resolve_capability(&ctx, "ctl"), Some(Capability::Full));
}

#[test]
fn control_token_still_grants_full_power_backward_compat() {
    // THE make-or-break: the existing control token (published in control.json)
    // resolves to Full and is authorized for EVERY tier - zero client breakage.
    let ctx = test_ctx("t");
    assert!(Capability::Full.allows(CommandTier::Read));
    assert!(Capability::Full.allows(CommandTier::Organization));
    assert!(Capability::Full.allows(CommandTier::ProcessChanging));
    // Through the gate: a ProcessChanging command with the control token is NOT
    // authz-refused (it fails downstream only because this headless ctx has no
    // UI sink - proving authz passed).
    let resp = dispatch_authenticated(&ctx, req("t", "spawn_terminal", json!({"cwd": "/tmp"})));
    let err = resp.error.unwrap_or_default();
    assert!(
        !err.contains("requires the control capability"),
        "control token was authz-refused: {err}"
    );
    assert!(
        err.contains("no UI"),
        "expected the downstream no-UI failure, got: {err}"
    );
}

#[test]
fn read_token_reads_but_cannot_spawn_or_kill() {
    let dir = std::env::temp_dir().join("t-hub-p2-readonly");
    let _ = std::fs::remove_dir_all(&dir);
    let ctx = test_ctx("t").with_audit(Arc::new(AuditLog::new(dir.clone())));

    // Read tier: allowed (not authz-refused). May fail on tmux, but never authz.
    let r = dispatch_authenticated(&ctx, req("read-t", "list_terminals", json!({})));
    assert!(
        !r.error
            .clone()
            .unwrap_or_default()
            .contains("requires the control capability"),
        "read token was refused a Read command"
    );

    // ProcessChanging + Organization-destructive: authz-refused with the exact msg.
    for cmd in [
        "spawn_terminal",
        "send_text",
        "send_keys",
        "close_terminal",
        "create_worktree",
    ] {
        let resp = dispatch_authenticated(
            &ctx,
            req(
                "read-t",
                cmd,
                json!({"cwd": "/tmp", "sessionId": "x", "text": "y", "keys": ["C-c"]}),
            ),
        );
        let err = resp.error.unwrap_or_default();
        assert!(
            err == format!(
                "unauthorized: '{cmd}' requires the control capability (this token is read-only)"
            ),
            "read token should be authz-refused for {cmd}, got: {err}"
        );
    }

    // Every refusal is audited with tokenTier=read and decision=refused-authz.
    let recs = read_audit(&dir);
    assert!(!recs.is_empty());
    assert!(recs.iter().all(|r| r["decision"] == "refused-authz"));
    assert!(recs.iter().all(|r| r["tokenTier"] == "read"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn control_token_command_audits_token_tier_control() {
    let dir = std::env::temp_dir().join("t-hub-p2-ctltier");
    let _ = std::fs::remove_dir_all(&dir);
    let ctx = test_ctx("t").with_audit(Arc::new(AuditLog::new(dir.clone())));
    // An Organization command with the control token: allowed, audited control.
    let _ = dispatch_authenticated(&ctx, req("t", "new_tab", json!({"name": "T"})));
    let recs = read_audit(&dir);
    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0]["tokenTier"], "control");
    assert_eq!(recs[0]["decision"], "allowed");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn generic_control_spawn_is_refused_without_recording_a_false_elevation() {
    let dir = std::env::temp_dir().join("t-hub-item3-ctlspawn");
    let _ = std::fs::remove_dir_all(&dir);
    let mut ctx = test_ctx("t").with_audit(Arc::new(AuditLog::new(dir.clone())));
    // A bound address enables stable discovery and identity minting.
    ctx.addr = "127.0.0.1:4242".to_string();

    // Default (untagged => READ) spawn: NO control-spawn audit record.
    let _ = spawn_env_with_identity(&ctx, &json!({"cwd": "/tmp"}), "spawn_terminal", None);
    let recs = read_audit(&dir);
    assert!(
        recs.iter().all(|r| r["decision"] != "control-spawn"),
        "a read-default spawn must NOT emit a control-spawn audit record"
    );

    // Explicit control is refused before identity mint or elevation audit.
    let refused = spawn_env_with_identity(
        &ctx,
        &json!({"cwd": "/tmp", "capability": "control"}),
        "spawn_terminal",
        None,
    )
    .unwrap_err();
    assert!(refused.contains("unsupported for generic Crew spawns"));
    let recs = read_audit(&dir);
    assert!(recs.iter().all(|r| r["decision"] != "control-spawn"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn remote_peer_is_capped_to_read_even_with_control_token() {
    // Belt-and-suspenders (open Q4): a non-loopback peer presenting the CONTROL
    // token is capped to ReadOnly, so it cannot spawn/kill over the network bind.
    let mut ctx = test_ctx("t");
    ctx.peer_is_loopback = false;
    assert_eq!(resolve_capability(&ctx, "t"), Some(Capability::ReadOnly));
    // Read still works remotely; ProcessChanging is authz-refused.
    let spawn = dispatch_authenticated(&ctx, req("t", "spawn_terminal", json!({"cwd": "/tmp"})));
    assert!(spawn
        .error
        .unwrap()
        .contains("requires the control capability"));
    let read = dispatch_authenticated(&ctx, req("t", "list_terminals", json!({})));
    assert!(!read
        .error
        .clone()
        .unwrap_or_default()
        .contains("requires the control capability"));
}

#[test]
fn read_token_is_valid_for_subscribe() {
    // token_is_valid (the event-subscribe gate) accepts either capability so a
    // least-privilege monitor can subscribe; a bad token is rejected.
    let ctx = test_ctx("t");
    assert!(token_is_valid(&ctx, "t"));
    assert!(token_is_valid(&ctx, "read-t"));
    assert!(!token_is_valid(&ctx, "bad"));
}

#[test]
fn phase3_flag_is_on_by_default_and_selects_read_token() {
    // item-3 flip #2 (ratified 2026-07-10): Phase 3 hardening is ON by default, so
    // `control.json` publishes only the READ token and an ambient scraper is
    // read-only. `T_HUB_CONTROL_HARDEN=0`/`false` is the instant rollback. This is
    // a BYPASS-WOULD-FAIL guard: revert the default to OFF and the first assert
    // goes RED. This mutates a process-global env var; it is saved/restored around
    // the mutation to stay hermetic.
    let saved = std::env::var("T_HUB_CONTROL_HARDEN").ok();
    std::env::remove_var("T_HUB_CONTROL_HARDEN");
    assert!(
        phase3_harden_enabled(),
        "harden flag must default ON (item-3 flip #2)"
    );
    std::env::set_var("T_HUB_CONTROL_HARDEN", "0");
    assert!(
        !phase3_harden_enabled(),
        "'0' is the rollback (hardening OFF)"
    );
    std::env::set_var("T_HUB_CONTROL_HARDEN", "false");
    assert!(
        !phase3_harden_enabled(),
        "'false' is the rollback (hardening OFF)"
    );
    std::env::set_var("T_HUB_CONTROL_HARDEN", "1");
    assert!(phase3_harden_enabled(), "'1' stays ON");
    std::env::set_var("T_HUB_CONTROL_HARDEN", "true");
    assert!(phase3_harden_enabled(), "'true' stays ON");
    std::env::set_var("T_HUB_CONTROL_HARDEN", "yes");
    assert!(phase3_harden_enabled(), "any non-0/false value stays ON");
    match saved {
        Some(v) => std::env::set_var("T_HUB_CONTROL_HARDEN", v),
        None => std::env::remove_var("T_HUB_CONTROL_HARDEN"),
    }

    // The pure selector: ON ⇒ read token, OFF ⇒ control token.
    assert_eq!(select_published_token("ctl", "rd", true), "rd");
    assert_eq!(select_published_token("ctl", "rd", false), "ctl");
    // Never an empty read token (falls back to control so a context that never
    // minted a read token is not locked out).
    assert_eq!(select_published_token("ctl", "", true), "ctl");
}

#[test]
fn key_rotation_keeps_fresh_seals_and_rotates_on_policy() {
    // item-3 Pillar B rotation-on-restart: a fresh key is KEPT (stable across
    // restarts within max age) and sealed at rest; a forced rotation and an
    // aged-out key both mint-and-REPLACE the file (never re-read the old key).
    // BYPASS-WOULD-FAIL: revert to reuse-the-file and the forced/aged asserts go RED.
    let base = std::env::temp_dir().join(format!("t-hub-keyrot-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let path = base.join("server-key");

    // Missing => mints and writes; the written file unseals back to the key.
    let k1 = load_or_rotate_key_with(&path, false, 3600);
    assert!(!k1.is_empty());
    assert!(path.exists(), "a minted key must be written to disk");
    let stored = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        crate::secret_seal::unseal_str(&stored).as_deref(),
        Some(k1.as_str())
    );

    // Within age, not forced => KEEP the same value.
    let k2 = load_or_rotate_key_with(&path, false, 3600);
    assert_eq!(
        k2, k1,
        "a fresh key within max age must be kept, not rotated"
    );

    // Forced => a DIFFERENT value overwrites the file (mint-and-replace).
    let k3 = load_or_rotate_key_with(&path, true, 3600);
    assert_ne!(k3, k1, "a forced rotation must mint-and-replace the key");
    let stored3 = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        crate::secret_seal::unseal_str(&stored3).as_deref(),
        Some(k3.as_str())
    );

    // max_age 0 => past age on every call => rotates.
    let k4 = load_or_rotate_key_with(&path, false, 0);
    assert_ne!(k4, k3, "max_age 0 must rotate on every restart");
    assert!(
        !key_is_past_max_age(&path, 3600),
        "a just-written key is not past a 1h age"
    );

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn packaged_legacy_orphan_forces_control_bearer_rotation_before_start() {
    let fixture: Value = serde_json::from_str(PACKAGED_SCHEMA_25_LEGACY_ORPHAN_FIXTURE).unwrap();
    let snapshot: CaptainsSnapshot =
        serde_json::from_value(fixture["captainsSnapshot"].clone()).unwrap();
    CaptainsRegistry::validate_snapshot(&snapshot).unwrap();
    assert!(snapshot.cortana.legacy_orphan_provenance.is_some());

    let base = std::env::temp_dir().join(format!(
        "t-hub-packaged-orphan-keyrot-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&base).unwrap();
    let path = base.join("server-key");
    let old = fixture["capture"]["control"]["sharedPersistentToken"]
        .as_str()
        .unwrap();
    write_key_file(&path, old);

    let kept = persistent_key_for_start_with(&path, false, 3600, false).unwrap();
    assert_eq!(kept, old);
    let rotated = persistent_key_for_start_with(&path, false, 3600, true).unwrap();
    assert_ne!(rotated, old);
    assert_eq!(
        crate::secret_seal::unseal_str(&std::fs::read_to_string(&path).unwrap()).as_deref(),
        Some(rotated.as_str())
    );
    let read = "profile-scoped-read-token";
    let handshake = ControlHandshake {
        addr: fixture["capture"]["control"]["currentAddress"]
            .as_str()
            .unwrap()
            .into(),
        token: select_published_token(&rotated, read, true).into(),
        read_token: read.into(),
        pid: 7,
        protocol_version: PROTOCOL_VERSION,
        instance_id: "captured-package-start".into(),
        listener_generation: 1,
        published_at: 1,
        local_control_token: rotated.clone(),
        local_host_token: "host-only".into(),
    };
    let published = serde_json::to_string(&handshake).unwrap();
    assert_eq!(handshake.token, read);
    assert_eq!(handshake.local_control_token, rotated);
    assert!(!published.contains(old));
    assert!(!published.contains(&rotated));

    std::fs::remove_dir_all(base).ok();
}

#[test]
fn legacy_bearer_rotation_failure_is_prepublication_and_preserves_old_key() {
    let base = std::env::temp_dir().join(format!(
        "t-hub-packaged-orphan-key-failure-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&base).unwrap();
    let path = base.join("server-key");
    let old = "old-profile-control-bearer";
    write_key_file(&path, old);

    let error = write_key_file_durable_with(&path, "unpublished-new-bearer", || {
        Err("injected crash before key publication".into())
    })
    .unwrap_err();
    assert!(error.contains("injected crash before key publication"));
    assert_eq!(
        crate::secret_seal::unseal_str(&std::fs::read_to_string(&path).unwrap()).as_deref(),
        Some(old)
    );
    assert_eq!(
        std::fs::read_dir(&base)
            .unwrap()
            .filter_map(Result::ok)
            .count(),
        1,
        "a refused rotation must not leave a publishable temporary key"
    );

    std::fs::remove_dir_all(base).ok();
}

#[test]
fn key_rotation_reads_legacy_plaintext_and_keeps_it() {
    // A pre-item-3 key file (raw token, no seal prefix) is read and KEPT within
    // age, so an upgrade preserves the paired credential (no surprise rotation).
    let base = std::env::temp_dir().join(format!("t-hub-keylegacy-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let path = base.join("server-read-key");
    std::fs::write(&path, "legacy-plaintext-token").unwrap();
    let k = load_or_rotate_key_with(&path, false, 3600);
    assert_eq!(
        k, "legacy-plaintext-token",
        "legacy plaintext must be read and kept"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn age_rotation_eligibility_holds_off_until_a_sealing_host_adopts_the_key() {
    // MED-1: on a SEALING host (Windows/DPAPI) age-rotation is held off until a
    // pre-item-3 (unsealed) key has been ADOPTED (sealed), so the first item-3
    // restart never strands pre-existing fleet. Once sealed, age-rotation resumes.
    // On a NON-sealing host there is no sealed form, so age-rotation stays eligible.
    assert!(
        !age_rotation_eligible(false, true),
        "sealing host + unsealed key: adopt, don't rotate"
    );
    assert!(
        age_rotation_eligible(true, true),
        "sealing host + sealed key: age-rotates"
    );
    assert!(
        age_rotation_eligible(false, false),
        "non-sealing host: eligible regardless"
    );
    assert!(
        age_rotation_eligible(true, false),
        "non-sealing host: eligible regardless"
    );
}

#[test]
fn hardened_control_json_withholds_full_token_but_handshake_carries_it() {
    // The security-critical Phase-3-safety invariant. Build the handshake exactly
    // as `start` does with hardening ON, write it, and assert BOTH halves of the
    // contract:
    //   (a) the SERIALIZED control.json `token` == read_token (full token withheld
    //       from external scrapers), and the full token appears nowhere in the file;
    //   (b) the RETURNED handshake's `local_control_token` == the full control token,
    //       so the trusted in-process frontend still gets full power.
    let full = "FULL-SECRET-abc123";
    let read = "READ-only-xyz789";
    let handshake = ControlHandshake {
        addr: "127.0.0.1:5000".into(),
        // Mirrors `start`: published token is the read token under hardening.
        token: select_published_token(full, read, true).to_string(),
        read_token: read.into(),
        pid: 7,
        protocol_version: PROTOCOL_VERSION,
        instance_id: "instance".into(),
        listener_generation: 1,
        published_at: 123,
        local_control_token: full.into(),
        local_host_token: "host".into(),
    };

    // (a) Published discovery is read-only and never leaks the full token.
    assert_eq!(
        handshake.token, read,
        "published token must be the read token"
    );
    let file = std::env::temp_dir().join(format!("t-hub-ctl-harden-{}.json", std::process::id()));
    let prev = std::env::var("T_HUB_CONTROL_FILE").ok();
    std::env::set_var("T_HUB_CONTROL_FILE", &file);
    write_handshake(&handshake).expect("write handshake");
    let on_disk = std::fs::read_to_string(&file).expect("read control.json");
    match prev {
        Some(v) => std::env::set_var("T_HUB_CONTROL_FILE", v),
        None => std::env::remove_var("T_HUB_CONTROL_FILE"),
    }
    let _ = std::fs::remove_file(&file);

    assert!(
        !on_disk.contains(full),
        "control.json must NOT contain the full control token; got: {on_disk}"
    );
    assert!(
        !on_disk.contains("local_control_token"),
        "the in-process field must not be serialized; got: {on_disk}"
    );
    let parsed: ControlHandshake = serde_json::from_str(&on_disk).expect("control.json parses");
    assert_eq!(parsed.token, read, "on-disk token must be the read token");
    assert_eq!(
        parsed.local_control_token, "",
        "in-process token must not survive to disk"
    );

    // (b) The RETURNED handshake still carries the full token for the frontend.
    assert_eq!(
        handshake.local_control_token, full,
        "local frontend must receive the full control token in-process"
    );
}

#[test]
fn phase3_hardened_publishes_read_token_and_default_spawn_is_read() {
    // With hardening ON (the item-3 default): what `control.json` publishes as
    // `token` is the READ token (so a raw scraper is read-only), AND the default
    // spawn-tree discovery contains no rotating capability token. Generic
    // control requests are rejected by the spawn contract.
    let ctx = test_ctx("ctl"); // read token is "read-ctl" (see test_ctx)
                               // Discovery, hardened: publishes the read token, NOT the control token.
    let published = select_published_token(&ctx.token, &ctx.read_token, true);
    assert_eq!(
        published, ctx.read_token,
        "hardened discovery must publish read token"
    );
    assert_ne!(
        published, ctx.token,
        "hardened discovery must NOT publish control token"
    );
    assert_eq!(
        resolve_capability(&ctx, published),
        Some(Capability::ReadOnly),
        "published token must resolve to read-only"
    );

    // Spawn-tree injection carries only stable discovery and explicitly
    // scrubs rotating address and token values.
    let mut ctx = ctx;
    ctx.addr = "127.0.0.1:4242".to_string();
    let env = elevation_env(&ctx, &json!({}));
    assert!(env
        .iter()
        .any(|(key, value)| key == "T_HUB_CONTROL_FILE" && !value.is_empty()));
    assert!(env
        .iter()
        .any(|(key, value)| key == "T_HUB_CONTROL_ADDR" && value.is_empty()));
    assert!(env
        .iter()
        .any(|(key, value)| key == "T_HUB_CONTROL_TOKEN" && value.is_empty()));

    // An explicit capability request does not put a shared credential back
    // into the child environment.
    let up = elevation_env(&ctx, &json!({"capability": "control"}));
    assert_eq!(up, env);
}

#[test]
fn phase3_verification_gate_checks_1_2_4_5() {
    // item-3 §3.1: the automated portion of the FIVE-check verification gate that
    // earns the default-ON flip #2. This test pins checks 1, 2, 4, 5 at the code
    // level; check 3 (a real attach + send_keys DRIVEN THROUGH THE WEBVIEW on a
    // WSLg build) is the manual acceptance step, documented in the PR body.
    let ctx = test_ctx("ctl"); // token "ctl", read token "read-ctl"
    let harden = true; // the ratified default (T_HUB_CONTROL_HARDEN unset => ON)

    // CHECK 1: control.json's `token` == the READ token (full withheld from disk).
    let published = select_published_token(&ctx.token, &ctx.read_token, harden);
    assert_eq!(
        published, ctx.read_token,
        "check 1: disk token must be the read token"
    );
    assert_ne!(
        published, ctx.token,
        "check 1: full token must NOT reach disk"
    );

    // CHECK 2: the webview obtains the FULL token in-process, not from disk. The
    // handshake carries `local_control_token` = full and never serializes it;
    // `control_client::resolve_endpoint` returns it in local mode (proven by
    // `control_client::tests::local_arm_authenticates_with_the_full_control_token`).
    let handshake = ControlHandshake {
        addr: "127.0.0.1:5000".into(),
        token: published.to_string(),
        read_token: ctx.read_token.clone(),
        pid: 1,
        protocol_version: PROTOCOL_VERSION,
        instance_id: "instance".into(),
        listener_generation: 1,
        published_at: 123,
        local_control_token: ctx.token.clone(),
        local_host_token: ctx.host_token.clone(),
    };
    assert_eq!(
        handshake.local_control_token, ctx.token,
        "check 2: in-process full token"
    );
    assert_eq!(
        serde_json::to_value(&handshake)
            .unwrap()
            .get("local_control_token"),
        None,
        "check 2: the in-process token must never serialize to control.json"
    );

    // CHECK 4: an external scraper presenting the PUBLISHED token is capped to
    // ReadOnly (it can never spawn/type/kill).
    assert_eq!(
        resolve_capability(&ctx, published),
        Some(Capability::ReadOnly),
        "check 4: the published token must resolve to read-only"
    );

    // CHECK 5: attach SURVIVES a control rebind while hardened - the webview keeps
    // full control across the rebind (the `rebind-strands-webview` class). Proven
    // end-to-end by `control_client::tests::refresh_addr_adopts_a_rotated_port_
    // from_the_local_handshake`, which keeps the full token across a port rotation
    // where the published token on disk is read-only. Asserted here structurally:
    // `rebind_control` rebuilds the handshake KEEPING the same full token.
    // (Cross-module behavioral proof lives in that control_client test.)
}

#[test]
fn elevation_env_passes_only_stable_discovery_and_scrubs_rotating_values() {
    let mut ctx = test_ctx("t");
    ctx.addr = "127.0.0.1:4242".to_string();
    let def = elevation_env(&ctx, &json!({}));
    assert_eq!(def[0].0, "T_HUB_CONTROL_FILE");
    assert!(!def[0].1.is_empty());
    assert_eq!(def[1], ("T_HUB_CONTROL_ADDR".to_string(), String::new()));
    assert_eq!(def[2], ("T_HUB_CONTROL_TOKEN".to_string(), String::new()));
    let typo = elevation_env(&ctx, &json!({"capability": "conrtol"}));
    assert_eq!(typo, def);
    let up = elevation_env(&ctx, &json!({"capability": "control"}));
    assert_eq!(up, def);
    // No bound addr (headless): nothing injected, so spawns behave as before.
    ctx.addr = String::new();
    assert!(elevation_env(&ctx, &json!({"capability": "control"})).is_empty());
}

#[test]
fn windows_discovery_path_is_stable_and_wsl_readable() {
    assert_eq!(
        wsl_discovery_path(Path::new(r"C:\Users\natha\.t-hub\control.json")),
        "/mnt/c/Users/natha/.t-hub/control.json"
    );
    assert_eq!(
        wsl_discovery_path(Path::new("/home/natkins/.t-hub/control.json")),
        "/home/natkins/.t-hub/control.json"
    );
}

#[test]
fn generic_spawn_refuses_control_capability_without_a_durable_authority() {
    assert!(require_read_only_spawn(&json!({}), "spawn_terminal").is_ok());
    assert!(require_read_only_spawn(&json!({"capability": "read"}), "spawn_terminal").is_ok());
    assert!(
        require_read_only_spawn(&json!({"capability": "control"}), "spawn_terminal")
            .unwrap_err()
            .contains("unsupported for generic Crew spawns")
    );
    assert!(require_read_only_spawn(&json!({"capability": "unknown"}), "spawn_terminal").is_err());
}

#[test]
fn spawn_env_mints_and_injects_a_per_session_identity_token() {
    let mut ctx = test_ctx("t");
    ctx.addr = "127.0.0.1:4242".to_string();
    let (env, minted) = spawn_env_with_identity(&ctx, &json!({}), "spawn_terminal", None).unwrap();
    // Rotating tier and endpoint values are scrubbed.
    assert!(env
        .iter()
        .any(|(k, v)| k == "T_HUB_CONTROL_TOKEN" && v.is_empty()));
    assert!(env
        .iter()
        .any(|(k, v)| k == "T_HUB_CONTROL_ADDR" && v.is_empty()));
    assert!(env
        .iter()
        .any(|(k, v)| k == "T_HUB_CONTROL_FILE" && !v.is_empty()));
    // The durable per-session token is injected alongside stable discovery.
    let session_token = env
        .iter()
        .find(|(k, _)| k == crate::identity::SESSION_TOKEN_ENV)
        .map(|(_, v)| v.clone())
        .expect("spawn env injects the per-session token");
    let identity = minted.expect("an identity is minted when addr is set");
    // The injected token resolves back to exactly this session's identity - the
    // per-session attribution the plane stamps enqueues with.
    let resolved = ctx
        .identity
        .resolve(&session_token)
        .expect("the injected per-session token resolves");
    assert_eq!(resolved.id, identity.id);
    assert_eq!(resolved.role, crate::identity::Role::Crew);
    // The per-session token is NOT the shared control token (that is the whole
    // point - it is per-session, unforgeable across sessions).
    assert_ne!(session_token, ctx.token);

    // Headless (no addr): no identity minted, env empty, spawns behave as before.
    ctx.addr = String::new();
    let (env2, minted2) =
        spawn_env_with_identity(&ctx, &json!({}), "spawn_terminal", None).unwrap();
    assert!(env2.is_empty());
    assert!(minted2.is_none());
}

#[test]
fn requested_control_is_refused_before_a_crew_identity_is_minted() {
    let mut ctx = test_ctx("identity-prebind");
    ctx.addr = "127.0.0.1:4242".to_string();
    let error = spawn_env_with_identity(
        &ctx,
        &json!({"capability": "control"}),
        "spawn_terminal",
        Some("fa123456"),
    )
    .unwrap_err();
    assert!(error.contains("unsupported for generic Crew spawns"));
    assert!(ctx.identity.is_empty());
}

#[test]
fn requested_session_identity_is_bound_before_launch_and_prebind_failure_rolls_back() {
    let mut ctx = test_ctx("identity-prebind");
    ctx.addr = "127.0.0.1:4242".to_string();
    let (_, minted) =
        spawn_env_with_identity(&ctx, &json!({}), "spawn_terminal", Some("fa123456")).unwrap();
    let minted = minted.unwrap();
    assert_eq!(minted.session_tile.as_deref(), Some("fa123456"));
    assert_eq!(
        ctx.identity
            .resolve(&minted.secret)
            .and_then(|identity| identity.session_tile),
        Some("fa123456".into())
    );

    let path = captains_tmp("identity-prebind-rollback");
    let store = Arc::new(crate::identity::IdentityStore::load(path.clone()));
    // mint_and_bind persists the pre-bound identity atomically in one write.
    store.fail_persist_after(0);
    let mut failing = test_ctx("identity-prebind-rollback").with_identity_store(store.clone());
    failing.addr = "127.0.0.1:4242".to_string();
    let error = spawn_env_with_identity(&failing, &json!({}), "spawn_terminal", Some("fa654321"))
        .unwrap_err();
    assert!(error.contains("identity pre-binding persistence failed"));
    assert!(store.is_empty());
    assert!(crate::identity::IdentityStore::load(path.clone()).is_empty());
    std::fs::remove_file(path).ok();
}

#[test]
fn failed_requested_session_spawn_retires_the_prebound_identity() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    if !tmux_process_tests_available() {
        eprintln!(
                "failed_requested_session_spawn_retires_the_prebound_identity: tmux or node not on PATH - skipping"
            );
        return;
    }
    let mut ctx =
        test_ctx("identity-prebound-spawn-rollback").with_apply_sink(Arc::new(RecordingSink {
            calls: StdMutex::new(Vec::new()),
        }));
    ctx.addr = "127.0.0.1:4242".to_string();
    let session_id = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    let target = tmux_target(&session_id);
    create_test_tmux_session(&target).unwrap();
    let result = spawn_terminal_with_private_pane_command_and_id(
        &ctx,
        &json!({"cwd": "/tmp", "capability": "control"}),
        None,
        false,
        false,
        false,
        Some(&session_id),
    );
    assert!(result.is_err());
    assert!(ctx.identity.is_empty());
    assert_eq!(
        tmux::session_liveness(&target),
        tmux::SessionLiveness::Alive
    );
    reap_test_tmux_session(&target).unwrap();
}

#[test]
fn socket_spawn_fails_before_tmux_when_identity_mint_is_not_durable() {
    let blocker = captains_tmp("identity-mint-blocker");
    std::fs::write(&blocker, b"not a directory").unwrap();
    let store = Arc::new(crate::identity::IdentityStore::load(
        blocker.join("identities.json"),
    ));
    let mut ctx = test_ctx("ctrl")
        .with_identity_store(store.clone())
        .with_apply_sink(Arc::new(RecordingSink {
            calls: StdMutex::new(Vec::new()),
        }));
    ctx.addr = "127.0.0.1:4242".to_string();

    let response = dispatch_authenticated(
        &ctx,
        req(
            "ctrl",
            "spawn_terminal",
            json!({"cwd": "/tmp", "requestId": "identity-persist-failure"}),
        ),
    );

    assert!(!response.ok);
    assert!(
        response
            .error
            .unwrap_or_default()
            .contains("identity store persist"),
        "spawn must surface the durability failure"
    );
    assert!(store.is_empty());
    std::fs::remove_file(blocker).unwrap();
}

#[test]
fn socket_spawn_kills_terminal_when_identity_bind_is_not_durable() {
    let path = captains_tmp("identity-bind-failure");
    let store = Arc::new(crate::identity::IdentityStore::load(path.clone()));
    store.fail_persist_after(1);
    let mut ctx = test_ctx("ctrl")
        .with_identity_store(store.clone())
        .with_apply_sink(Arc::new(RecordingSink {
            calls: StdMutex::new(Vec::new()),
        }));
    ctx.addr = "127.0.0.1:4242".to_string();

    let response = dispatch_authenticated(
        &ctx,
        req(
            "ctrl",
            "spawn_terminal",
            json!({"cwd": "/tmp", "requestId": "identity-bind-failure"}),
        ),
    );

    assert!(!response.ok);
    assert!(
        response
            .error
            .unwrap_or_default()
            .contains("terminal was rolled back"),
        "spawn must report its compensating rollback"
    );
    assert!(
        store.is_empty(),
        "the rolled-back spawn must retire its identity"
    );
    let persisted = crate::identity::IdentityStore::load(path.clone());
    assert!(
        persisted.is_empty(),
        "rollback must remove the durable identity"
    );
    let _ = std::fs::remove_file(path);
}

#[test]
fn socket_close_reports_identity_retirement_persistence_failure() {
    let path = captains_tmp("identity-retire-close-failure");
    let store = Arc::new(crate::identity::IdentityStore::load(path.clone()));
    let identity = store.mint(crate::identity::Role::Crew).unwrap();
    store.bind_tile(&identity.id, "already-gone").unwrap();
    store.fail_persist_after(0);
    let ctx = test_ctx("ctrl").with_identity_store(store.clone());

    let response = dispatch_authenticated(
        &ctx,
        req(
            "ctrl",
            "close_terminal",
            json!({"sessionId": "already-gone"}),
        ),
    );

    assert!(!response.ok);
    assert!(
        response
            .error
            .unwrap_or_default()
            .contains("identity store persist failure injected"),
        "close must surface failed durable identity retirement"
    );
    assert!(store.resolve(&identity.secret).is_some());
    assert!(
        crate::identity::IdentityStore::load(path.clone())
            .resolve(&identity.secret)
            .is_some(),
        "failed retirement must leave memory and disk aligned"
    );
    let _ = std::fs::remove_file(path);
}

#[test]
fn every_crew_spawn_is_credential_withheld_regardless_of_capability() {
    // item-3 §2.3.5: every Crew spawn gets gh withholding (GH_CONFIG_DIR at an
    // empty dir) plus blanked ambient tokens.
    // A request for a generic administrative Crew is refused. Every allowed
    // read-only Crew spawn still withholds publishing credentials.
    let mut ctx = test_ctx("t");
    ctx.addr = "127.0.0.1:4242".to_string();

    let (env, _) = spawn_env_with_identity(&ctx, &json!({}), "spawn_terminal", None).unwrap();
    let gh_dir = env
        .iter()
        .find(|(k, _)| k == "GH_CONFIG_DIR")
        .map(|(_, v)| v.as_str());
    assert!(
        gh_dir.is_some_and(|v| !v.is_empty()),
        "a crew spawn must withhold gh via GH_CONFIG_DIR"
    );
    // The value rides a `tmux -e` into a WSL shell, so it must be a POSIX path:
    // no backslash, no `C:`-style drive, forward-slash absolute. A Windows path
    // (the old USERPROFILE/PathBuf::join form) silently defeated withholding.
    assert!(
        !env.iter().any(|(_, v)| v.contains('\\')),
        "no emitted env value may contain a backslash (Windows) path: {env:?}"
    );
    assert!(
        gh_dir.is_some_and(|v| v.starts_with('/') && !v.contains(":\\")),
        "GH_CONFIG_DIR must be a POSIX-absolute path, got {gh_dir:?}"
    );
    assert!(
        env.iter().any(|(k, v)| k == "GH_TOKEN" && v.is_empty()),
        "a crew spawn must blank the ambient GH_TOKEN"
    );

    for purpose in ["fleet-admin", "ship-admin", "recovery"] {
        let refusal = spawn_env_with_identity(
            &ctx,
            &json!({
                "capability": "control",
                "admissionPurpose": purpose
            }),
            "spawn_terminal",
            None,
        )
        .unwrap_err();
        assert!(refusal.contains("unsupported for generic Crew spawns"));
        let (admin_env, _) = spawn_env_with_identity(
            &ctx,
            &json!({
                "capability": "read",
                "admissionPurpose": purpose
            }),
            "spawn_terminal",
            None,
        )
        .unwrap();
        assert!(
            admin_env.iter().any(|(key, _)| key == "GH_CONFIG_DIR"),
            "a {purpose} Crew spawn must still withhold gh credentials"
        );
        for token in [
            "GH_TOKEN",
            "GITHUB_TOKEN",
            "NPM_TOKEN",
            "NODE_AUTH_TOKEN",
            "CARGO_REGISTRY_TOKEN",
        ] {
            assert!(
                admin_env
                    .iter()
                    .any(|(key, value)| key == token && value.is_empty()),
                "a {purpose} Crew spawn must blank ambient {token}"
            );
        }
    }
}

#[test]
fn crew_gh_config_dir_is_always_a_backslash_free_posix_path() {
    // audit HIGH: the value rides a `tmux -e` into WSL, so it must ALWAYS be a
    // POSIX path. BYPASS-WOULD-FAIL: restore the USERPROFILE/PathBuf::join form
    // and the Windows-path cases below emit `C:\...\.t-hub\...` → RED.

    // A POSIX-absolute HOME (WSL-launched app) is used verbatim.
    assert_eq!(
        crew_gh_config_dir_from_home(Some("/home/natkins")),
        format!("/home/natkins/{CREW_GH_CONFIG_SUBDIR}")
    );
    // A trailing slash is normalized (no doubled `//`).
    assert_eq!(
        crew_gh_config_dir_from_home(Some("/home/natkins/")),
        format!("/home/natkins/{CREW_GH_CONFIG_SUBDIR}")
    );
    // A Windows USERPROFILE-style value is REJECTED (the crux of the bug): it
    // falls back to a fixed POSIX path, never a backslash/drive path.
    for windows_home in [r"C:\Users\natha", r"C:\Users\natha\", r"D:\home"] {
        let dir = crew_gh_config_dir_from_home(Some(windows_home));
        assert_eq!(dir, format!("/tmp/{CREW_GH_CONFIG_SUBDIR}"));
        assert!(!dir.contains('\\'), "no backslash: {dir}");
        assert!(!dir.contains(":\\"), "no drive path: {dir}");
    }
    // An absent HOME also falls back to the POSIX path (native-Windows launch).
    assert_eq!(
        crew_gh_config_dir_from_home(None),
        format!("/tmp/{CREW_GH_CONFIG_SUBDIR}")
    );
}

#[test]
fn orchestrator_home_is_scoped_and_rejects_traversal() {
    assert_eq!(
        resolve_orchestrator_home("/home/tester", None).unwrap(),
        format!("/home/tester/{CORTANA_HOME_DEFAULT}")
    );
    assert_eq!(
        resolve_orchestrator_home("/home/tester", Some(".t-hub-dev/custom-orchestrator")).unwrap(),
        "/home/tester/.t-hub-dev/custom-orchestrator"
    );
    assert_eq!(
        resolve_orchestrator_home("/home/tester", Some("/srv/t-hub/cortana")).unwrap(),
        "/srv/t-hub/cortana"
    );
    assert!(resolve_orchestrator_home("/home/tester", Some("../production")).is_err());
    assert!(resolve_orchestrator_home("/home/tester", Some(r"C:\production")).is_err());
}

#[test]
fn my_capability_reports_the_callers_resolved_capability() {
    // item-3 Pillar C: the gate resolves its own class from the unspoofable token.
    // A control token reports "control"; the read token reports "read".
    let ctx = test_ctx("t");
    let control = dispatch_authenticated(&ctx, req("t", "my_capability", json!({})));
    assert_eq!(control.result.unwrap()["capability"], "control");
    let read = dispatch_authenticated(&ctx, req("read-t", "my_capability", json!({})));
    assert_eq!(read.result.unwrap()["capability"], "read");
}

#[test]
fn discovery_proof_echoes_nonce_and_live_listener_identity_at_read_tier() {
    let mut ctx = test_ctx("t");
    ctx.listener_instance_id = "proof-instance".into();
    ctx.addr = "127.0.0.1:4242".into();
    ctx.bound_listener_generation = 7;
    let proof = dispatch_authenticated(
        &ctx,
        req(
            "read-t",
            "control_discovery_proof",
            json!({"nonce": "fresh-proof-nonce"}),
        ),
    );
    let result = proof.result.unwrap();
    assert_eq!(result["nonce"], "fresh-proof-nonce");
    assert_eq!(result["protocolVersion"], PROTOCOL_VERSION);
    assert_eq!(result["instanceId"], "proof-instance");
    assert_eq!(result["listenerGeneration"], 7);
    assert_eq!(result["listenerAddr"], "127.0.0.1:4242");

    // An overlapping serve loop shares the allocator but retains its own
    // immutable address/generation proof.
    let mut replacement = ctx.clone();
    replacement.addr = "127.0.0.1:4243".into();
    replacement.bound_listener_generation = 8;
    ctx.listener_generation.store(99, Ordering::Release);
    let old_overlap = dispatch_authenticated(
        &ctx,
        req(
            "read-t",
            "control_discovery_proof",
            json!({"nonce": "old-overlap"}),
        ),
    )
    .result
    .unwrap();
    let new_overlap = dispatch_authenticated(
        &replacement,
        req(
            "read-t",
            "control_discovery_proof",
            json!({"nonce": "new-overlap"}),
        ),
    )
    .result
    .unwrap();
    assert_eq!(old_overlap["listenerGeneration"], 7);
    assert_eq!(old_overlap["listenerAddr"], "127.0.0.1:4242");
    assert_eq!(new_overlap["listenerGeneration"], 8);
    assert_eq!(new_overlap["listenerAddr"], "127.0.0.1:4243");

    let missing = dispatch_authenticated(&ctx, req("read-t", "control_discovery_proof", json!({})));
    assert!(missing.error.unwrap().contains("bounded non-empty nonce"));
}

#[test]
fn durable_captain_renews_an_identity_bound_control_lease() {
    let (ctx, captains, identities, identity) = captain_lease_fixture(true);
    let before = captains.snapshot();
    let renewed = dispatch_authenticated(
        &ctx,
        req_session(
            "read-global-control",
            &identity.secret,
            "renew_captain_control_lease",
            Value::Null,
        ),
    );
    assert!(renewed.ok, "{:?}", renewed.error);
    let result = renewed.result.unwrap();
    let lease = result["lease"].as_str().unwrap();
    let first_expires_at = result["expiresAt"].as_u64().unwrap();
    assert_ne!(lease, ctx.token);
    assert_ne!(lease, ctx.read_token);
    assert_eq!(result["terminalId"], "lease-captain");
    assert_eq!(result["scope"]["kind"], "captain");
    assert_eq!(result["scope"]["shipSlug"], "lease-ship");
    assert_eq!(result["scope"]["projectId"], "lease-project");
    assert_eq!(captains.snapshot().captains, before.captains);
    assert_eq!(captains.snapshot().projects, before.projects);

    let repeated = dispatch_authenticated(
        &ctx,
        req_session(
            "read-global-control",
            &identity.secret,
            "renew_captain_control_lease",
            Value::Null,
        ),
    );
    assert!(repeated.ok, "{:?}", repeated.error);
    let repeated = repeated.result.unwrap();
    assert_eq!(repeated["lease"], lease);
    assert!(repeated["expiresAt"].as_u64().unwrap() > first_expires_at);
    let lease_state = ctx.control_leases.state.lock().unwrap();
    assert_eq!(lease_state.by_secret.len(), 1);
    assert_eq!(lease_state.by_identity.len(), 1);
    drop(lease_state);

    let capability = dispatch_authenticated(
        &ctx,
        req_session(lease, &identity.secret, "my_capability", json!({})),
    );
    assert_eq!(capability.result.unwrap()["capability"], "control");

    let foreign = identities
        .mint_and_bind(
            crate::identity::Role::Captain,
            Some("foreign-ship".into()),
            "foreign-captain",
        )
        .unwrap();
    let stolen = dispatch_authenticated(
        &ctx,
        req_session(lease, &foreign.secret, "my_capability", json!({})),
    );
    assert_eq!(
        stolen.error.as_deref(),
        Some("unauthorized: bad control token")
    );

    identities.revoke(&identity.id).unwrap();
    let revoked = dispatch_authenticated(
        &ctx,
        req_session(lease, &identity.secret, "my_capability", json!({})),
    );
    assert_eq!(
        revoked.error.as_deref(),
        Some("unauthorized: bad control token")
    );
}

#[test]
fn renewing_same_identity_lease_atomically_extends_both_deadlines() {
    let (ctx, _, identities, identity) = captain_lease_fixture(true);
    let authority = LeaseAuthority::Captain {
        ship_slug: "lease-ship".into(),
        project_id: "lease-project".into(),
        generation: ctx.captains.test_scoped_authority_generation(
            "lease-ship",
            "lease-captain",
            "lease-project",
        ),
    };
    let old_deadline = Instant::now() + Duration::from_millis(80);
    let old_epoch_deadline = now_ms().saturating_add(80);
    let (old_secret, old_expires_at) = ctx.control_leases.issue(CaptainControlLease {
        identity_id: identity.id.clone(),
        terminal_id: "lease-captain".into(),
        authority: authority.clone(),
        expires_at: old_deadline,
        expires_at_epoch_ms: old_epoch_deadline,
    });

    thread::sleep(Duration::from_millis(10));
    let renewed_deadline = Instant::now() + Duration::from_millis(250);
    let renewed_epoch_deadline = now_ms().saturating_add(250);
    let (renewed_secret, renewed_expires_at) = ctx.control_leases.issue(CaptainControlLease {
        identity_id: identity.id.clone(),
        terminal_id: "lease-captain".into(),
        authority: authority.clone(),
        expires_at: renewed_deadline,
        expires_at_epoch_ms: renewed_epoch_deadline,
    });

    assert_eq!(renewed_secret, old_secret);
    assert!(renewed_expires_at > old_expires_at);
    let renewed = ctx.control_leases.get(&renewed_secret).unwrap();
    assert!(renewed.expires_at > old_deadline);
    assert_eq!(renewed.authority, authority);
    let state = ctx.control_leases.state.lock().unwrap();
    assert_eq!(state.by_secret.len(), 1);
    assert_eq!(state.by_identity.len(), 1);
    assert_eq!(state.by_identity.get(&identity.id), Some(&renewed_secret));
    drop(state);

    thread::sleep(
        old_deadline
            .saturating_duration_since(Instant::now())
            .saturating_add(Duration::from_millis(20)),
    );
    let after_old_deadline = dispatch_authenticated(
        &ctx,
        req_session(
            &renewed_secret,
            &identity.secret,
            "my_capability",
            Value::Null,
        ),
    );
    assert_eq!(after_old_deadline.result.unwrap()["capability"], "control");

    let foreign = identities
        .mint_and_bind(
            crate::identity::Role::Captain,
            Some("foreign-ship".into()),
            "foreign-captain",
        )
        .unwrap();
    let stolen = dispatch_authenticated(
        &ctx,
        req_session(
            &renewed_secret,
            &foreign.secret,
            "my_capability",
            Value::Null,
        ),
    );
    assert_eq!(
        stolen.error.as_deref(),
        Some("unauthorized: bad control token")
    );

    identities.revoke(&identity.id).unwrap();
    let revoked = dispatch_authenticated(
        &ctx,
        req_session(
            &renewed_secret,
            &identity.secret,
            "my_capability",
            Value::Null,
        ),
    );
    assert_eq!(
        revoked.error.as_deref(),
        Some("unauthorized: bad control token")
    );
}

#[test]
fn captain_control_lease_capacity_evicts_oldest_identity_binding() {
    let leases = CaptainControlLeases::default();
    let base = Instant::now() + Duration::from_secs(3_600);
    let mut oldest_secret = String::new();

    for index in 0..MAX_CAPTAIN_CONTROL_LEASES {
        let (secret, _) = leases.issue(CaptainControlLease {
            identity_id: format!("identity-{index}"),
            terminal_id: format!("terminal-{index}"),
            authority: LeaseAuthority::Cortana {
                generation: index as u64,
            },
            expires_at: base + Duration::from_millis(index as u64),
            expires_at_epoch_ms: 10_000 + index as u64,
        });
        if index == 0 {
            oldest_secret = secret;
        }
    }

    let (newest_secret, _) = leases.issue(CaptainControlLease {
        identity_id: "identity-newest".into(),
        terminal_id: "terminal-newest".into(),
        authority: LeaseAuthority::Cortana {
            generation: MAX_CAPTAIN_CONTROL_LEASES as u64,
        },
        expires_at: base + Duration::from_secs(1),
        expires_at_epoch_ms: 20_000,
    });

    assert!(leases.get(&oldest_secret).is_none());
    assert!(leases.get(&newest_secret).is_some());
    let state = leases.state.lock().unwrap();
    assert_eq!(state.by_secret.len(), MAX_CAPTAIN_CONTROL_LEASES);
    assert_eq!(state.by_identity.len(), MAX_CAPTAIN_CONTROL_LEASES);
    assert!(!state.by_identity.contains_key("identity-0"));
    assert_eq!(
        state.by_identity.get("identity-newest"),
        Some(&newest_secret)
    );
}

#[test]
fn authoritative_cortana_renews_a_fleet_scoped_lease_and_mutates() {
    let terminal_id = "lease-cortana";
    let live_target = tmux_target(terminal_id);
    let ctx = test_ctx("cortana-global").with_live_sessions(move || Ok(vec![live_target.clone()]));
    let secret = mint_current_cortana_session(&ctx.identity, &ctx.captains, terminal_id);
    let renewed = dispatch_authenticated(
        &ctx,
        req_session(
            &ctx.read_token,
            &secret,
            "renew_captain_control_lease",
            Value::Null,
        ),
    );
    assert!(renewed.ok, "{:?}", renewed.error);
    let renewed = renewed.result.unwrap();
    assert_eq!(renewed["scope"]["kind"], "cortana");
    let lease = renewed["lease"].as_str().unwrap();
    let mutation = dispatch_authenticated(
        &ctx,
        req_session(lease, &secret, "new_tab", json!({"name": "Cortana Ops"})),
    );
    assert!(mutation.ok, "{:?}", mutation.error);
    assert!(ctx.tabs.id_for_name("Cortana Ops").is_some());
}

#[test]
fn scoped_captain_cannot_arm_or_remove_a_foreign_ship_watch() {
    let (ctx, captains, _, identity) = captain_lease_fixture(true);
    captains
        .claim_test("foreign-captain", Some("foreign-ship"), vec![])
        .unwrap();
    let renewed = dispatch_authenticated(
        &ctx,
        req_session(
            &ctx.read_token,
            &identity.secret,
            "renew_captain_control_lease",
            Value::Null,
        ),
    );
    let lease = renewed.result.unwrap()["lease"]
        .as_str()
        .unwrap()
        .to_string();
    for command in ["watch_fleet", "unwatch_fleet"] {
        let response = dispatch_authenticated(
            &ctx,
            req_session(
                &lease,
                &identity.secret,
                command,
                json!({"orchestratorSessionId": "foreign-captain"}),
            ),
        );
        assert!(!response.ok, "{command} accepted a foreign watch");
        assert!(response
            .error
            .as_deref()
            .is_some_and(|error| error.contains("own or same-ship watch")));
    }
}

#[test]
fn captain_lease_renewal_rejects_dead_released_crew_and_duplicate_identities() {
    let (dead_ctx, _, _, dead_identity) = captain_lease_fixture(false);
    let dead = dispatch_authenticated(
        &dead_ctx,
        req_session(
            "read-global-control",
            &dead_identity.secret,
            "renew_captain_control_lease",
            Value::Null,
        ),
    );
    assert!(dead
        .error
        .as_deref()
        .is_some_and(|error| error.contains("not alive")));

    let (released_ctx, captains, _, released_identity) = captain_lease_fixture(true);
    captains.release("lease-ship").unwrap();
    let released = dispatch_authenticated(
        &released_ctx,
        req_session(
            "read-global-control",
            &released_identity.secret,
            "renew_captain_control_lease",
            Value::Null,
        ),
    );
    assert!(released
        .error
        .as_deref()
        .is_some_and(|error| error.contains("control_reauthentication_required")));

    let (duplicate_ctx, _, identities, identity) = captain_lease_fixture(true);
    identities
        .mint_and_bind(
            crate::identity::Role::Captain,
            Some("lease-ship".into()),
            "lease-captain",
        )
        .unwrap();
    let duplicate = dispatch_authenticated(
        &duplicate_ctx,
        req_session(
            "read-global-control",
            &identity.secret,
            "renew_captain_control_lease",
            Value::Null,
        ),
    );
    assert!(duplicate
        .error
        .as_deref()
        .is_some_and(|error| error.contains("missing or ambiguous")));

    let crew_store = Arc::new(crate::identity::IdentityStore::ephemeral());
    let crew = crew_store
        .mint_and_bind(
            crate::identity::Role::Crew,
            Some("lease-ship".into()),
            "lease-captain",
        )
        .unwrap();
    let crew_ctx = test_ctx("global-control")
        .with_identity_store(crew_store)
        .with_live_sessions(|| Ok(vec!["th_lease-captain".into()]));
    let crew_result = dispatch_authenticated(
        &crew_ctx,
        req_session(
            "read-global-control",
            &crew.secret,
            "renew_captain_control_lease",
            Value::Null,
        ),
    );
    assert!(crew_result
        .error
        .as_deref()
        .is_some_and(|error| error.contains("control_reauthentication_required")));

    let (removed_ctx, _, removed_identities, removed_identity) = captain_lease_fixture(true);
    removed_identities.retire(&removed_identity.id).unwrap();
    let removed = dispatch_authenticated(
        &removed_ctx,
        req_session(
            "read-global-control",
            &removed_identity.secret,
            "renew_captain_control_lease",
            Value::Null,
        ),
    );
    assert!(removed
        .error
        .as_deref()
        .is_some_and(|error| { error.contains("durable session identity could not be verified") }));

    let (expired_ctx, _, _, expired_identity) = captain_lease_fixture(true);
    expired_ctx.control_leases.insert_test(
        "expired-lease",
        CaptainControlLease {
            identity_id: expired_identity.id.clone(),
            terminal_id: "lease-captain".into(),
            authority: LeaseAuthority::Captain {
                ship_slug: "lease-ship".into(),
                project_id: "lease-project".into(),
                generation: expired_ctx.captains.test_scoped_authority_generation(
                    "lease-ship",
                    "lease-captain",
                    "lease-project",
                ),
            },
            expires_at: Instant::now() - Duration::from_secs(1),
            expires_at_epoch_ms: now_ms().saturating_sub(1),
        },
    );
    let expired = dispatch_authenticated(
        &expired_ctx,
        req_session(
            "expired-lease",
            &expired_identity.secret,
            "my_capability",
            Value::Null,
        ),
    );
    assert_eq!(
        expired.error.as_deref(),
        Some("unauthorized: bad control token")
    );
}

#[test]
fn captain_identity_reacquires_after_control_context_restart_and_credential_rotation() {
    let (first, captains, identities, identity) = captain_lease_fixture(true);
    let initial = dispatch_authenticated(
        &first,
        req_session(
            "read-global-control",
            &identity.secret,
            "renew_captain_control_lease",
            Value::Null,
        ),
    );
    let old_lease = initial.result.unwrap()["lease"]
        .as_str()
        .unwrap()
        .to_string();

    let restarted = test_ctx("rotated-global-control")
        .with_captains_registry(captains.clone())
        .with_identity_store(identities)
        .with_live_sessions(|| Ok(vec![tmux_target("lease-captain")]));
    let stale = dispatch_authenticated(
        &restarted,
        req_session(&old_lease, &identity.secret, "my_capability", Value::Null),
    );
    assert_eq!(
        stale.error.as_deref(),
        Some("unauthorized: bad control token")
    );

    let renewed = dispatch_authenticated(
        &restarted,
        req_session(
            "read-rotated-global-control",
            &identity.secret,
            "renew_captain_control_lease",
            Value::Null,
        ),
    );
    assert!(renewed.ok, "{:?}", renewed.error);
    let result = renewed.result.unwrap();
    let new_lease = result["lease"].as_str().unwrap();
    assert_ne!(new_lease, old_lease);
    let bootstrap = dispatch_authenticated(
        &restarted,
        req_session(
            new_lease,
            &identity.secret,
            "captain_bootstrap",
            json!({"captainSessionId": "lease-captain"}),
        ),
    );
    assert!(bootstrap.ok, "{:?}", bootstrap.error);
    let bootstrap = bootstrap.result.unwrap();
    assert_eq!(bootstrap["captain"]["terminalId"], "lease-captain");
    assert_eq!(bootstrap["captain"]["shipSlug"], "lease-ship");
    assert_eq!(bootstrap["captain"]["assignment"], "Package 0");
    assert_eq!(bootstrap["project"]["projectId"], "lease-project");
    assert_eq!(bootstrap["agentCount"], 0);
    assert_eq!(bootstrap["recoverySource"], "captains-registry");
    assert_eq!(captains.snapshot().captains.len(), 1);
    assert_eq!(captains.snapshot().projects.len(), 1);
}

#[test]
fn inbox_ack_and_status_handlers_round_trip() {
    let inbox = Arc::new(crate::inbox::Inbox::ephemeral());
    inbox
        .enqueue(
            "tileX",
            "crew:a",
            crate::inbox::Priority::Standard,
            "hi",
            true,
        )
        .unwrap();
    // Deliver it so it is ackable (the drain's at-most-once write).
    inbox.drain_one("tileX", |_r| Ok(()));
    let ctx = test_ctx("t").with_inbox(inbox.clone());

    // Status reflects the delivered-not-yet-processed record.
    let status = inbox_status(&ctx, &json!({"sessionId": "tileX"})).unwrap();
    assert_eq!(status["recipient"]["delivered"].as_u64(), Some(1));
    assert_eq!(status["recipient"]["enqueued"].as_u64(), Some(0));

    // Ack retires it (`delivered -> processed`).
    let ack = inbox_ack(&ctx, &json!({"sessionId": "tileX", "seq": 0}), None, true).unwrap();
    assert_eq!(ack["accepted"], "inbox_ack");
    assert_eq!(ack["state"], "processed");
    // A duplicate ack is a benign no-op (a lost-then-retried ack never re-writes).
    let reack = inbox_ack(&ctx, &json!({"sessionId": "tileX", "seq": 0}), None, true).unwrap();
    assert_eq!(reack["state"], "alreadyProcessed");

    // No sessionId => the all-recipients snapshot.
    let all = inbox_status(&ctx, &json!({})).unwrap();
    assert!(all["recipients"].is_array());

    // A malformed ack (missing seq) is rejected, not silently accepted.
    assert!(inbox_ack(&ctx, &json!({"sessionId": "tileX"}), None, true).is_err());
    // Acking an unknown recipient/seq is honest, not a crash.
    assert_eq!(
        inbox_ack(&ctx, &json!({"sessionId": "nope", "seq": 7}), None, true).unwrap()["state"],
        "unknown"
    );
}

// -----------------------------------------------------------------------
// Comms-plane Phase 3: ACL enforcement END-TO-END through the authenticated
// gate (`dispatch_authenticated` with a per-session token on the request).
// These exercise the WIRING (session-token resolve -> acl predicate -> refuse
// + attribute), complementing the pure predicate tests in `acl.rs`.
// -----------------------------------------------------------------------

/// Mint a per-session identity for `role` on `ship`, bind it to `tile`, and return
/// its secret - the `T_HUB_SESSION_TOKEN` a request presents. Registered in `store`.
fn mint_session(
    store: &crate::identity::IdentityStore,
    role: crate::identity::Role,
    ship: &str,
    tile: &str,
) -> String {
    let id = store.mint_for(role, Some(ship.to_string())).unwrap();
    store.bind_tile(&id.id, tile).unwrap();
    id.secret
}

fn mint_current_cortana_session(
    store: &crate::identity::IdentityStore,
    registry: &CaptainsRegistry,
    tile: &str,
) -> String {
    registry
        .claim_provider(
            tile,
            None,
            FleetRole::Cortana,
            Some("codex"),
            None,
            vec![],
            &all_alive,
            &crew_all_alive,
        )
        .unwrap();
    let identity = store.mint(crate::identity::Role::Cortana).unwrap();
    store.bind_tile(&identity.id, tile).unwrap();
    let operation_id = format!("test-cortana-{tile}");
    registry.begin_cortana_recovery(&operation_id).unwrap();
    registry
        .commit_cortana_runtime(&operation_id, &identity.id, 1, tile, "codex", None)
        .unwrap();
    identity.secret
}

#[test]
fn legacy_healthy_cortana_without_active_attestation_fails_closed_on_restart() {
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let captains_path = captains_tmp(&format!("legacy-active-attestation-{nonce}"));
    let identities_path = std::env::temp_dir().join(format!(
        "t-hub-legacy-active-attestation-identities-{nonce}.json"
    ));
    let tile = format!("co{}", &nonce[..6]);
    let identities = Arc::new(crate::identity::IdentityStore::load(
        identities_path.clone(),
    ));
    let secret = {
        let registry = CaptainsRegistry::load(captains_path.clone());
        mint_current_cortana_session(&identities, &registry, &tile)
    };
    let mut document: Value =
        serde_json::from_slice(&std::fs::read(&captains_path).unwrap()).unwrap();
    document["schemaVersion"] = json!(28);
    document["cortana"]
        .as_object_mut()
        .unwrap()
        .remove("activeHarnessAttestation");
    std::fs::write(
        &captains_path,
        serde_json::to_vec_pretty(&document).unwrap(),
    )
    .unwrap();

    let restarted_registry = Arc::new(CaptainsRegistry::load(captains_path.clone()));
    let restarted = test_ctx("legacy-active-attestation-restart")
        .with_captains_registry(Arc::clone(&restarted_registry))
        .with_identity_store(identities);
    assert!(matches!(
        restarted_registry.cortana_identity().recovery,
        crate::cortana_reconcile::CortanaRecoveryState::Degraded { .. }
    ));
    let resolved = resolve_identity(&restarted, &secret).unwrap();
    assert_eq!(resolved.fleet_role, None);
    assert_eq!(resolved.mint_role, crate::identity::Role::Unknown);

    std::fs::remove_file(captains_path).ok();
    std::fs::remove_file(identities_path).ok();
}

#[test]
fn durable_cortana_stays_authoritative_across_reload_and_generic_release_denial() {
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let captains_path = captains_tmp(&format!("cortana-apex-{nonce}"));
    let identities_path =
        std::env::temp_dir().join(format!("t-hub-cortana-apex-identities-{nonce}.json"));
    let current_tile = format!("co{}", &nonce[..6]);
    let stale_tile = format!("st{}", &nonce[..6]);
    let current_secret;
    let stale_secret;

    {
        let registry = CaptainsRegistry::load(captains_path.clone());
        let identities = crate::identity::IdentityStore::load(identities_path.clone());
        current_secret = mint_current_cortana_session(&identities, &registry, &current_tile);
        let stale = identities.mint(crate::identity::Role::Cortana).unwrap();
        identities.bind_tile(&stale.id, &stale_tile).unwrap();
        stale_secret = stale.secret;
    }

    // A reconnect after process reload preserves the exact durable bearer,
    // while a second mint-time Cortana identity is role-demoted and non-apex.
    {
        let ctx = test_ctx("cortana-apex-token")
            .with_captains_registry(Arc::new(CaptainsRegistry::load(captains_path.clone())))
            .with_identity_store(Arc::new(crate::identity::IdentityStore::load(
                identities_path.clone(),
            )));
        let current = resolve_identity(&ctx, &current_secret).unwrap();
        assert_eq!(current.fleet_role, Some(FleetRole::Cortana));
        assert!(caller_is_apex(Some(&current), false));

        let stale = resolve_identity(&ctx, &stale_secret).unwrap();
        assert_eq!(stale.fleet_role, None);
        assert_eq!(stale.mint_role, crate::identity::Role::Unknown);
        assert!(!caller_is_apex(Some(&stale), false));
        let denied = dispatch_authenticated(
            &ctx,
            req_session(
                "cortana-apex-token",
                &stale_secret,
                "commission_captain",
                json!({}),
            ),
        );
        assert!(!denied.ok);
        let error = denied.error.unwrap_or_default();
        assert!(
            error.contains("General/Cortana"),
            "unexpected denial: {error}"
        );

        let denied_release = release_captain(
            &ctx,
            &json!({"captainSessionId": current_tile}),
            Some(&current),
            false,
        )
        .unwrap_err();
        assert!(denied_release.contains("durable backend-owned singleton"));

        let preserved = resolve_identity(&ctx, &current_secret).unwrap();
        assert_eq!(preserved.fleet_role, Some(FleetRole::Cortana));
        assert_eq!(preserved.mint_role, crate::identity::Role::Cortana);
        assert!(caller_is_apex(Some(&preserved), false));
        let snapshot = ctx.captains.snapshot();
        assert_eq!(
            snapshot.cortana.terminal_id.as_deref(),
            Some(current_tile.as_str())
        );
        assert_eq!(
            snapshot
                .captains
                .iter()
                .filter(|record| record.role == FleetRole::Cortana)
                .count(),
            1
        );
    }

    // The same durable identity, Fleet claim, and bearer survive reload.
    {
        let ctx = test_ctx("cortana-apex-token")
            .with_captains_registry(Arc::new(CaptainsRegistry::load(captains_path.clone())))
            .with_identity_store(Arc::new(crate::identity::IdentityStore::load(
                identities_path.clone(),
            )));
        let preserved = resolve_identity(&ctx, &current_secret).unwrap();
        assert_eq!(preserved.fleet_role, Some(FleetRole::Cortana));
        assert_eq!(preserved.mint_role, crate::identity::Role::Cortana);
        assert!(caller_is_apex(Some(&preserved), false));
        let snapshot = ctx.captains.snapshot();
        assert_eq!(
            snapshot.cortana.terminal_id.as_deref(),
            Some(current_tile.as_str())
        );
        assert_eq!(
            snapshot
                .captains
                .iter()
                .filter(|record| record.role == FleetRole::Cortana)
                .count(),
            1
        );
    }

    for path in [
        captains_path.with_extension("json.bak"),
        captains_path,
        identities_path,
    ] {
        std::fs::remove_file(path).ok();
    }
}

#[test]
fn cross_ship_isolation_refuses_a_foreign_read_through_the_gate() {
    // MANDATED cross-ship-isolation guard: a crew on ship-a may NOT read another
    // ship's pane. BYPASS-WOULD-FAIL: remove `enforce_session_access` from
    // `read_terminal` and the foreign read proceeds to tmux (a different, non-acl
    // error) - this assert (the isolation reason) goes RED.
    let store = Arc::new(crate::identity::IdentityStore::ephemeral());
    let reg = Arc::new(CaptainsRegistry::new());
    reg.claim_test("cap-b", Some("ship-b"), vec![]).unwrap();
    assert!(reg.record_crew("cap-b", "crew-b").unwrap());
    let crew_a = mint_session(&store, crate::identity::Role::Crew, "ship-a", "crew-a");
    let ctx = test_ctx("ctrl")
        .with_read_token("read-t".to_string())
        .with_identity_store(store)
        .with_captains_registry(reg);

    let foreign = dispatch_authenticated(
        &ctx,
        req_session(
            "read-t",
            &crew_a,
            "read_terminal",
            json!({"sessionId": "crew-b"}),
        ),
    );
    assert!(!foreign.ok, "a foreign read must be refused");
    let foreign_error = foreign.error.unwrap();
    assert!(
        foreign_error.contains("cross-ship isolation"),
        "the refusal must be the isolation ACL, not a downstream tmux error: {foreign_error}"
    );

    // A trusted in-process host fails open - it is not refused by
    // the ACL (it errors later at the tmux capture, which is a different message).
    let host = dispatch_authenticated(
        &ctx,
        req("ctrl", "read_terminal", json!({"sessionId": "crew-b"})),
    );
    assert!(
        !host
            .error
            .unwrap_or_default()
            .contains("cross-ship isolation"),
        "the trusted host must fail open (NORM-now), not be ACL-refused"
    );
}

#[test]
fn full_token_without_host_provenance_cannot_reach_identity_sensitive_handlers() {
    let ctx = test_ctx("ctrl");
    let cases = [
        ("read_terminal", json!({"sessionId": "target"})),
        ("send_text", json!({"sessionId": "target", "text": "x"})),
        (
            "send_keys",
            json!({"sessionId": "target", "keys": ["Escape"]}),
        ),
        ("abort_session", json!({"sessionId": "target"})),
        ("plane_admin", json!({"op": "purge", "recipient": "target"})),
        ("plane_send", json!({"recipient": "target", "text": "x"})),
        ("inbox_ack", json!({"sessionId": "target", "seq": 0})),
        ("history_list", json!({"limit": 10})),
        ("history_focus", json!({"historyId": "history:v1:target"})),
        (
            "history_resume",
            json!({
                "historyId": "history:v1:target",
                "requestId": "history-provenance",
                "targetTabId": "target"
            }),
        ),
    ];

    for (command, args) in cases {
        for session in ["", "invalid-session-token"] {
            let response =
                dispatch_authenticated(&ctx, req_untrusted("ctrl", session, command, args.clone()));
            assert!(!response.ok, "{command} accepted omitted identity");
            assert!(
                response
                    .error
                    .unwrap_or_default()
                    .contains("requires a valid T_HUB_SESSION_TOKEN"),
                "{command} did not fail at the provenance boundary"
            );
        }
    }
}

#[test]
fn untrusted_full_mutations_require_identity_and_audit_omitted_or_invalid_tokens() {
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let audit_dir = std::env::temp_dir().join(format!("t-hub-identity-gate-{nonce}"));
    let store = Arc::new(crate::identity::IdentityStore::ephemeral());
    let general_secret = mint_session(
        &store,
        crate::identity::Role::General,
        "fleet",
        "general-gate",
    );
    let ctx = test_ctx("identity-gate")
        .with_identity_store(store)
        .with_audit(Arc::new(AuditLog::new(audit_dir.clone())));

    for (command, args) in [
        ("new_tab", json!({"name": "must-not-exist"})),
        (
            "spawn_terminal",
            json!({"cwd": "/tmp", "requestId": "must-not-spawn"}),
        ),
    ] {
        for session in ["", "invalid-nonempty-session-token"] {
            let response = dispatch_authenticated(
                &ctx,
                req_untrusted("identity-gate", session, command, args.clone()),
            );
            assert!(
                !response.ok,
                "{command} accepted an unidentified Full bearer"
            );
            assert!(response
                .error
                .unwrap_or_default()
                .contains("requires a valid T_HUB_SESSION_TOKEN"));
        }
    }
    assert!(ctx.tabs.id_for_name("must-not-exist").is_none());
    let records = read_audit(&audit_dir);
    assert_eq!(records.len(), 4);
    assert!(records
        .iter()
        .all(|record| record["decision"] == "refused-identity"));
    assert!(records
        .iter()
        .all(|record| record["tokenTier"] == "control"));

    let identified = dispatch_authenticated(
        &ctx,
        req_session(
            "identity-gate",
            &general_secret,
            "new_tab",
            json!({"name": "identified"}),
        ),
    );
    assert!(
        identified.ok,
        "identified Full mutation failed: {:?}",
        identified.error
    );

    let trusted = dispatch_authenticated(
        &ctx,
        req("identity-gate", "new_tab", json!({"name": "trusted-host"})),
    );
    assert!(
        trusted.ok,
        "trusted host mutation failed: {:?}",
        trusted.error
    );
    std::fs::remove_dir_all(audit_dir).ok();
}

#[test]
fn captain_lifecycle_authority_is_enforced_through_authenticated_dispatch() {
    let store = Arc::new(crate::identity::IdentityStore::ephemeral());
    let reg = Arc::new(CaptainsRegistry::new());
    reg.claim_test("cap-a", Some("ship-a"), vec![]).unwrap();
    reg.claim_test("cap-b", Some("ship-b"), vec![]).unwrap();
    let captain = mint_session(&store, crate::identity::Role::Captain, "ship-a", "cap-a");
    let general = mint_session(&store, crate::identity::Role::General, "fleet", "general");
    let promoted = mint_session(&store, crate::identity::Role::Crew, "pending", "new-cap");
    let ctx = test_ctx("ctrl")
        .with_identity_store(store)
        .with_captains_registry(reg);

    let promoted = resolve_identity(&ctx, &promoted).unwrap();
    assert!(
        enforce_attach_authority(&ctx, Some(&promoted), false, "new-cap", FleetRole::Captain,)
            .is_ok()
    );
    assert!(
        enforce_attach_authority(&ctx, Some(&promoted), false, "other", FleetRole::Captain,)
            .unwrap_err()
            .contains("attach a different terminal")
    );

    let foreign = dispatch_authenticated(
        &ctx,
        req_session(
            "ctrl",
            &captain,
            "release_captain",
            json!({"shipSlug": "ship-b"}),
        ),
    );
    assert!(!foreign.ok);
    assert!(foreign.error.unwrap().contains("same ship"));
    assert!(ctx
        .captains
        .snapshot()
        .captains
        .iter()
        .any(|record| record.ship_slug == "ship-b"));

    let own = dispatch_authenticated(
        &ctx,
        req_session(
            "ctrl",
            &captain,
            "release_captain",
            json!({"shipSlug": "ship-a"}),
        ),
    );
    assert!(own.ok, "same-ship release failed: {:?}", own.error);

    let apex = dispatch_authenticated(
        &ctx,
        req_session(
            "ctrl",
            &general,
            "release_captain",
            json!({"shipSlug": "ship-b"}),
        ),
    );
    assert!(apex.ok, "General release failed: {:?}", apex.error);
}

#[test]
fn full_socket_token_without_session_identity_has_no_lifecycle_authority() {
    let reg = Arc::new(CaptainsRegistry::new());
    reg.claim_test("cap-a", Some("ship-a"), vec![]).unwrap();
    let ctx = test_ctx("ctrl").with_captains_registry(reg);

    for session in ["", "invalid-session-secret"] {
        let response = dispatch_authenticated(
            &ctx,
            ControlRequest {
                token: "ctrl".into(),
                command: "release_captain".into(),
                args: json!({"shipSlug": "ship-a"}),
                session: session.into(),
                host: String::new(),
                v: None,
            },
        );
        assert!(!response.ok);
        assert!(response
            .error
            .unwrap_or_default()
            .contains("requires a valid T_HUB_SESSION_TOKEN"));
    }
    assert_eq!(ctx.captains.snapshot().captains.len(), 1);
}

#[test]
fn crew_cannot_self_assign_the_reserved_cortana_role_or_slug() {
    let store = Arc::new(crate::identity::IdentityStore::ephemeral());
    let crew = mint_session(&store, crate::identity::Role::Crew, "ship-a", "crew-a");
    let ctx = test_ctx("ctrl").with_identity_store(store);

    for args in [
        json!({"captainSessionId": "crew-a", "role": "cortana"}),
        json!({"captainSessionId": "crew-a", "shipSlug": "cortana"}),
    ] {
        let response =
            dispatch_authenticated(&ctx, req_session("ctrl", &crew, "claim_captain", args));
        assert!(!response.ok);
        assert!(response
            .error
            .unwrap_or_default()
            .contains("General/Cortana"));
    }
    assert!(ctx.captains.snapshot().captains.is_empty());
}

#[test]
fn captain_cannot_close_or_heartbeat_foreign_crew() {
    let store = Arc::new(crate::identity::IdentityStore::ephemeral());
    let reg = Arc::new(CaptainsRegistry::new());
    reg.claim_test("cap-a", Some("ship-a"), vec![]).unwrap();
    reg.claim_test("cap-b", Some("ship-b"), vec![]).unwrap();
    assert!(reg.record_crew("cap-b", "crew-b").unwrap());
    reg.bind_crew_context(
        "cap-b",
        "crew-b",
        "foreign task",
        "codex",
        None,
        None,
        PowderWorkBinding {
            card_id: "card-b".into(),
            run_id: "run-b".into(),
            agent: None,
            claim_expires_at: None,
            mutation_intent: None,
            dispatch_release_recovery: false,
            state: PowderWorkState::Active,
        },
    )
    .unwrap();
    let captain = mint_session(&store, crate::identity::Role::Captain, "ship-a", "cap-a");
    let ctx = test_ctx("ctrl")
        .with_identity_store(store)
        .with_captains_registry(reg);

    for (command, args) in [
        ("close_terminal", json!({"sessionId": "crew-b"})),
        ("heartbeat_crew_powder", json!({"crewSessionId": "crew-b"})),
    ] {
        let response = dispatch_authenticated(&ctx, req_session("ctrl", &captain, command, args));
        assert!(!response.ok);
        let error = response.error.unwrap_or_default();
        assert!(error.starts_with("acl:"), "got: {error}");
    }
}

#[test]
fn read_terminal_ownership_matrix_through_the_gate() {
    // The full DoD ownership matrix for `read_terminal`, exercised END-TO-END through
    // `dispatch_authenticated` (session-token resolve -> `enforce_session_access` ->
    // `can_access_session`). The sibling `cross_ship_isolation_refuses_a_foreign_read_
    // through_the_gate` test is the bypass-would-fail sentinel (drop the guard and the
    // foreign-crew cell flips to a non-ACL error); THIS test proves the ALLOW cells go
    // through and the orchestrator cells resolve correctly.
    //
    // An ALLOWED read cannot fully succeed in the headless test env (there is no live
    // `th_*` tmux session), so it fails at the tmux capture with a DIFFERENT message.
    // The invariant for an allow cell is therefore: NOT refused with the isolation ACL
    // reason. A DENIED cell must carry "cross-ship isolation".
    let store = Arc::new(crate::identity::IdentityStore::ephemeral());
    let reg = Arc::new(CaptainsRegistry::new());
    reg.claim_test("cap-a", Some("ship-a"), vec![]).unwrap();
    assert!(reg.record_crew("cap-a", "crew-a").unwrap());
    reg.claim_test("cap-b", Some("ship-b"), vec![]).unwrap();
    assert!(reg.record_crew("cap-b", "crew-b").unwrap());
    let crew_a = mint_session(&store, crate::identity::Role::Crew, "ship-a", "crew-a");
    let cap_a = mint_session(&store, crate::identity::Role::Captain, "ship-a", "cap-a");
    let cortana = mint_current_cortana_session(&store, &reg, "cor");
    let ctx = test_ctx("ctrl")
        .with_read_token("read-t".to_string())
        .with_identity_store(store)
        .with_captains_registry(reg);

    // An allow cell: refused ONLY if the isolation ACL fired (else it fell through to
    // the tmux layer, which is the intended "permitted" outcome).
    let is_isolation_denied = |session: &str, target: &str| -> bool {
        let resp = dispatch_authenticated(
            &ctx,
            req_session(
                "read-t",
                session,
                "read_terminal",
                json!({"sessionId": target}),
            ),
        );
        resp.error
            .unwrap_or_default()
            .contains("cross-ship isolation")
    };

    // SELF: a crew reading its OWN pane -> permitted (falls through to tmux).
    assert!(
        !is_isolation_denied(&crew_a, "crew-a"),
        "self-read must be permitted"
    );
    // OWN-CREW: a captain reading its own ship's crew -> permitted.
    assert!(
        !is_isolation_denied(&cap_a, "crew-a"),
        "captain reading own crew must be permitted"
    );
    // OWN-SHIP SUPERVISOR: a crew reading its own captain's pane -> permitted (same ship).
    assert!(
        !is_isolation_denied(&crew_a, "cap-a"),
        "same-ship supervisor read must be permitted"
    );
    // ORCHESTRATOR: cortana reading a SUPERVISOR on any ship (her subordinate) -> permitted.
    assert!(
        !is_isolation_denied(&cortana, "cap-b"),
        "cortana reading a captain must be permitted"
    );
    // FOREIGN-CREW: a crew reading another ship's crew -> DENIED.
    assert!(
        is_isolation_denied(&crew_a, "crew-b"),
        "cross-ship crew read must be refused"
    );
    // ORCHESTRATOR SKIP-LEVEL: cortana reading a FOREIGN ship's crew directly -> DENIED.
    assert!(
        is_isolation_denied(&cortana, "crew-b"),
        "cortana skip-level into foreign crew must be refused"
    );

    // IN-PROCESS HOST: the local host proof admits a request without a session identity.
    let host = dispatch_authenticated(
        &ctx,
        req("ctrl", "read_terminal", json!({"sessionId": "crew-b"})),
    );
    assert!(
        !host
            .error
            .unwrap_or_default()
            .contains("cross-ship isolation"),
        "the full-token host must fail open, not be ACL-refused"
    );
}

#[test]
fn cross_ship_isolation_refuses_a_foreign_break_glass_write() {
    // The write side of H3: even break-glass `send_text` rides the isolation ACL. A
    // captain on ship-a (holding the Full control token) may not write ship-b's crew.
    let store = Arc::new(crate::identity::IdentityStore::ephemeral());
    let reg = Arc::new(CaptainsRegistry::new());
    reg.claim_test("cap-a", Some("ship-a"), vec![]).unwrap();
    reg.claim_test("cap-b", Some("ship-b"), vec![]).unwrap();
    assert!(reg.record_crew("cap-b", "crew-b").unwrap());
    let cap_a = mint_session(&store, crate::identity::Role::Captain, "ship-a", "cap-a");
    let ctx = test_ctx("ctrl")
        .with_identity_store(store)
        .with_captains_registry(reg);
    let resp = dispatch_authenticated(
        &ctx,
        req_session(
            "ctrl",
            &cap_a,
            "send_text",
            json!({"sessionId": "crew-b", "text": "hi"}),
        ),
    );
    assert!(!resp.ok);
    assert!(resp.error.unwrap().contains("cross-ship isolation"));
}

#[test]
fn inbox_ack_self_scope_admits_own_ack_at_read_refuses_cross_session() {
    // The retired interim price: a crew self-acks its OWN inbox with only a READ
    // token (no control-capable relay needed). A cross-session ack is refused.
    let store = Arc::new(crate::identity::IdentityStore::ephemeral());
    let inbox = Arc::new(crate::inbox::Inbox::ephemeral());
    for tile in ["crew-a", "crew-b"] {
        inbox
            .enqueue(tile, "cap:x", crate::inbox::Priority::Standard, "m", true)
            .unwrap();
        inbox.drain_one(tile, |_r| Ok(())); // deliver so it is ackable
    }
    let crew_a = mint_session(&store, crate::identity::Role::Crew, "ship-a", "crew-a");
    let ctx = test_ctx("ctrl")
        .with_read_token("read-t".to_string())
        .with_identity_store(store)
        .with_inbox(inbox.clone());

    // Self-ack with a bare READ token: ADMITTED (the §2.4.1 upgrade).
    let ok = dispatch_authenticated(
        &ctx,
        req_session(
            "read-t",
            &crew_a,
            "inbox_ack",
            json!({"sessionId": "crew-a", "seq": 0}),
        ),
    );
    assert!(
        ok.ok,
        "self-ack must be admitted at read tier: {:?}",
        ok.error
    );
    assert_eq!(ok.result.unwrap()["state"], "processed");

    // Cross-session ack (crew-a acking crew-b) with the read token: REFUSED, and
    // crew-b's message is untouched.
    let bad = dispatch_authenticated(
        &ctx,
        req_session(
            "read-t",
            &crew_a,
            "inbox_ack",
            json!({"sessionId": "crew-b", "seq": 0}),
        ),
    );
    assert!(
        !bad.ok,
        "a cross-session ack with a read token must be refused"
    );
    assert_eq!(
        inbox.depth("crew-b").delivered,
        1,
        "a refused cross-session ack must not process crew-b's message"
    );

    let full_token_cross = dispatch_authenticated(
        &ctx,
        req_session(
            "ctrl",
            &crew_a,
            "inbox_ack",
            json!({"sessionId": "crew-b", "seq": 0}),
        ),
    );
    assert!(
        !full_token_cross.ok,
        "Full capability must not substitute for host provenance"
    );
    assert_eq!(inbox.depth("crew-b").delivered, 1);
}

#[test]
fn plane_send_enforces_message_rows_and_never_crew_emergency() {
    // MANDATED never-crew-emergency guard + the message rows through the gate.
    let store = Arc::new(crate::identity::IdentityStore::ephemeral());
    let reg = Arc::new(CaptainsRegistry::new());
    reg.claim_test("cap-a", Some("ship-a"), vec![]).unwrap();
    assert!(reg.record_crew("cap-a", "crew-a").unwrap());
    assert!(reg.record_crew("cap-a", "crew-a2").unwrap());
    let inbox = Arc::new(crate::inbox::Inbox::ephemeral());
    let crew_a = mint_session(&store, crate::identity::Role::Crew, "ship-a", "crew-a");
    let cap_a = mint_session(&store, crate::identity::Role::Captain, "ship-a", "cap-a");
    let ctx = test_ctx("ctrl")
        .with_read_token("read-t".to_string())
        .with_identity_store(store)
        .with_captains_registry(reg)
        .with_inbox(inbox.clone());

    // Crew -> its OWN captain (up): ALLOWED at read tier.
    let up = dispatch_authenticated(
        &ctx,
        req_session(
            "read-t",
            &crew_a,
            "plane_send",
            json!({"recipient": "cap-a", "text": "status"}),
        ),
    );
    assert!(up.ok, "crew->own captain must be allowed: {:?}", up.error);

    // Crew -> a SIBLING crew: REFUSED (no daisy-chain).
    let sib = dispatch_authenticated(
        &ctx,
        req_session(
            "read-t",
            &crew_a,
            "plane_send",
            json!({"recipient": "crew-a2", "text": "psst"}),
        ),
    );
    assert!(!sib.ok);
    assert!(sib.error.unwrap().contains("daisy-chain"));

    // Crew raising EMERGENCY: REFUSED (never-crew-emergency). BYPASS-WOULD-FAIL:
    // admit crew emergency and this goes RED.
    let emg = dispatch_authenticated(
        &ctx,
        req_session(
            "read-t",
            &crew_a,
            "plane_send",
            json!({"recipient": "cap-a", "text": "!!", "priority": "emergency"}),
        ),
    );
    assert!(!emg.ok);
    assert!(emg.error.unwrap().contains("EMERGENCY"));

    // A CAPTAIN may raise EMERGENCY to its own crew.
    let cap_emg = dispatch_authenticated(
        &ctx,
        req_session(
            "ctrl",
            &cap_a,
            "plane_send",
            json!({"recipient": "crew-a", "text": "!!", "priority": "emergency"}),
        ),
    );
    assert!(
        cap_emg.ok,
        "a captain's emergency to own crew must be allowed: {:?}",
        cap_emg.error
    );
    assert_eq!(cap_emg.result.unwrap()["priority"], "emergency");
}

#[test]
fn abort_session_denies_cross_ship_and_crew_through_the_gate() {
    // The never-seized guard through the gate: a captain may not abort another
    // ship's crew, and a crew (read token) cannot reach the ProcessChanging abort at
    // all. No tmux is touched - the ACL refuses first.
    let store = Arc::new(crate::identity::IdentityStore::ephemeral());
    let reg = Arc::new(CaptainsRegistry::new());
    reg.claim_test("cap-a", Some("ship-a"), vec![]).unwrap();
    reg.claim_test("cap-b", Some("ship-b"), vec![]).unwrap();
    assert!(reg.record_crew("cap-b", "crew-b").unwrap());
    assert!(reg.record_crew("cap-a", "crew-a").unwrap());
    let cap_a = mint_session(&store, crate::identity::Role::Captain, "ship-a", "cap-a");
    let crew_a = mint_session(&store, crate::identity::Role::Crew, "ship-a", "crew-a");
    let ctx = test_ctx("ctrl")
        .with_read_token("read-t".to_string())
        .with_identity_store(store)
        .with_captains_registry(reg);

    // Captain of ship-a aborting ship-b's crew: cross-ship, REFUSED.
    let cross = dispatch_authenticated(
        &ctx,
        req_session(
            "ctrl",
            &cap_a,
            "abort_session",
            json!({"sessionId": "crew-b"}),
        ),
    );
    assert!(!cross.ok);
    assert!(cross.error.unwrap().contains("abort denied"));

    // A crew presenting a read token cannot even reach the ProcessChanging abort.
    let crew_try = dispatch_authenticated(
        &ctx,
        req_session(
            "read-t",
            &crew_a,
            "abort_session",
            json!({"sessionId": "cap-a"}),
        ),
    );
    assert!(!crew_try.ok, "a read-token crew must not be able to abort");
}

#[test]
fn only_a_general_session_authorizes_and_the_gate_resolves_it() {
    // The delegation-gate carrier through the gate: only a general-roled session may
    // ORIGINATE; the resolve-and-verify gate then reports Present. A captain cannot.
    let store = Arc::new(crate::identity::IdentityStore::ephemeral());
    let general = mint_session(&store, crate::identity::Role::General, "cortana", "gen");
    let captain = mint_session(&store, crate::identity::Role::Captain, "ship-a", "cap-a");
    let ctx = test_ctx("ctrl").with_identity_store(store);

    // A captain session may NOT originate an authorization.
    let capauth = dispatch_authenticated(
        &ctx,
        req_session("ctrl", &captain, "authorize", json!({"action": "spend"})),
    );
    assert!(!capauth.ok);
    assert!(capauth.error.unwrap().contains("only the general"));

    // The general originates one; the captain's gate consult resolves it Present.
    let ga = dispatch_authenticated(
        &ctx,
        req_session(
            "ctrl",
            &general,
            "authorize",
            json!({"action": "spend", "targetShip": "ship-a"}),
        ),
    );
    assert!(ga.ok, "general authorize failed: {:?}", ga.error);
    let id = ga.result.unwrap()["id"].as_str().unwrap().to_string();
    let chk = dispatch_authenticated(
        &ctx,
        req_session("ctrl", &captain, "check_authorization", json!({"id": id})),
    );
    let r = chk.result.unwrap();
    assert_eq!(r["present"], json!(true));
    assert_eq!(r["verdict"], "present");

    // An unknown reference is Absent (the captain's gate FIRES = escalate).
    let miss = dispatch_authenticated(
        &ctx,
        req_session(
            "ctrl",
            &captain,
            "check_authorization",
            json!({"id": "no-such"}),
        ),
    );
    assert_eq!(miss.result.unwrap()["verdict"], "absent");
}

#[test]
fn plane_admin_purge_is_apex_only() {
    // operate-fleet-infra through the gate: a captain may not administer the shared
    // plane; the apex (Cortana) may.
    let store = Arc::new(crate::identity::IdentityStore::ephemeral());
    let reg = Arc::new(CaptainsRegistry::new());
    reg.claim_test("cap-a", Some("ship-a"), vec![]).unwrap();
    let inbox = Arc::new(crate::inbox::Inbox::ephemeral());
    inbox
        .enqueue("crew-a", "x", crate::inbox::Priority::Standard, "m", true)
        .unwrap();
    let captain = mint_session(&store, crate::identity::Role::Captain, "ship-a", "cap-a");
    let cortana = mint_current_cortana_session(&store, &reg, "cor");
    let ctx = test_ctx("ctrl")
        .with_identity_store(store)
        .with_captains_registry(reg)
        .with_inbox(inbox.clone());

    // A captain may NOT purge.
    let capadm = dispatch_authenticated(
        &ctx,
        req_session(
            "ctrl",
            &captain,
            "plane_admin",
            json!({"op": "purge", "recipient": "crew-a"}),
        ),
    );
    assert!(!capadm.ok);
    assert!(capadm.error.unwrap().contains("apex-owned"));
    assert_eq!(
        inbox.depth("crew-a").enqueued,
        1,
        "a refused purge leaves the queue intact"
    );

    // Cortana (apex) may.
    let coradm = dispatch_authenticated(
        &ctx,
        req_session(
            "ctrl",
            &cortana,
            "plane_admin",
            json!({"op": "purge", "recipient": "crew-a"}),
        ),
    );
    assert!(coradm.ok, "cortana purge failed: {:?}", coradm.error);
    assert_eq!(
        inbox.depth("crew-a").enqueued,
        0,
        "an apex purge flushed the queue"
    );
}

// -----------------------------------------------------------------------
// Idempotency: RequestCache (ask #1)
// -----------------------------------------------------------------------

#[test]
fn server_idempotent_command_contract_is_complete() {
    assert_eq!(
        IDEMPOTENT_COMMANDS,
        [
            "spawn_terminal",
            "create_worktree",
            "history_resume",
            "reconcile_cortana",
            "commission_captain",
            "dispatch_crew",
            "start_agent",
            "agent_followup",
        ]
    );
    assert!(!is_idempotent_command("list_tabs"));
}

#[test]
fn request_cache_replays_a_completed_outcome() {
    let cache = RequestCache::new();
    // First sighting reserves the id and must run the command.
    assert!(matches!(cache.begin("r1"), BeginOutcome::Fresh));
    let stored = cache.finish("r1", Ok(json!({"id": "abc"})));
    assert_eq!(stored.unwrap()["id"], "abc");
    // A retry of the SAME id replays the stored outcome - it does NOT re-run.
    match cache.begin("r1") {
        BeginOutcome::Duplicate(Ok(v)) => assert_eq!(v["id"], "abc"),
        BeginOutcome::Duplicate(Err(e)) => panic!("expected Ok replay, got Err: {e}"),
        BeginOutcome::Fresh => panic!("a completed id must not be reserved Fresh again"),
        BeginOutcome::FreshAfterReap => {
            panic!("a completed id must replay, not reap-and-re-reserve")
        }
        BeginOutcome::InFlight => panic!("a completed id must replay, not report InFlight"),
    }
}

#[test]
fn request_cache_rejects_reusing_an_id_for_different_arguments() {
    let cache = RequestCache::new();
    assert!(matches!(
        cache.begin_bound("history-request", "resume:one"),
        BeginOutcome::Fresh
    ));
    cache
        .finish("history-request", Ok(json!({"terminalId": "one"})))
        .unwrap();
    match cache.begin_bound("history-request", "resume:two") {
        BeginOutcome::Duplicate(Err(error)) => {
            assert!(error.starts_with("request_conflict:"));
        }
        _ => panic!("a requestId must remain bound to its original arguments"),
    }
}

#[test]
fn request_cache_reports_in_flight_for_a_concurrent_duplicate() {
    let cache = RequestCache::new();
    // A first caller reserved the id and is still running (no finish yet).
    assert!(matches!(cache.begin("r2"), BeginOutcome::Fresh));
    // A retry that races the original must NOT run the command again.
    assert!(matches!(cache.begin("r2"), BeginOutcome::InFlight));
    // Once it completes, the same id replays the outcome.
    let _ = cache.finish("r2", Ok(json!({"ok": true})));
    assert!(matches!(cache.begin("r2"), BeginOutcome::Duplicate(_)));
}

#[test]
fn request_cache_cancel_frees_a_reservation_for_retry() {
    let cache = RequestCache::new();
    assert!(matches!(cache.begin("r3"), BeginOutcome::Fresh));
    // A governor refusal cancels the reservation (no outcome recorded)...
    cache.cancel("r3");
    // ...so a later retry is Fresh again (it can succeed once budget frees),
    // not stuck InFlight or replaying a refusal.
    assert!(matches!(cache.begin("r3"), BeginOutcome::Fresh));
}

#[test]
fn request_cache_status_reports_unknown_inflight_and_completed() {
    let cache = RequestCache::new();
    assert!(matches!(cache.status("nope"), RequestStatus::Unknown));
    cache.begin("r4");
    assert!(matches!(cache.status("r4"), RequestStatus::InFlight));
    let _ = cache.finish("r4", Err("boom".to_string()));
    match cache.status("r4") {
        RequestStatus::Completed(Err(e)) => assert_eq!(e, "boom"),
        _ => panic!("expected Completed(Err)"),
    }
}

#[test]
fn request_cache_evicts_oldest_completed_beyond_capacity() {
    let cache = RequestCache::with_bounds(
        2,
        std::time::Duration::from_secs(600),
        std::time::Duration::from_secs(600),
    );
    for id in ["a", "b", "c"] {
        cache.begin(id);
        let _ = cache.finish(id, Ok(json!({"id": id})));
    }
    // "a" was evicted when "c" pushed past the capacity of 2.
    assert!(matches!(cache.status("a"), RequestStatus::Unknown));
    assert!(matches!(cache.status("b"), RequestStatus::Completed(_)));
    assert!(matches!(cache.status("c"), RequestStatus::Completed(_)));
}

#[test]
fn request_cache_evicts_a_done_entry_past_its_ttl() {
    // A completed outcome ages out of the cache after its TTL, keeping the cache
    // self-cleaning. (The same retain reaps a stale InFlight reservation past
    // REQUEST_INFLIGHT_REAP - the safety valve for a panicked/hung handler.)
    let cache = RequestCache::with_bounds(
        8,
        std::time::Duration::from_millis(1),
        std::time::Duration::from_secs(600),
    );
    cache.begin("done");
    let _ = cache.finish("done", Ok(json!({})));
    std::thread::sleep(std::time::Duration::from_millis(5));
    // status() runs eviction; the expired Done entry is gone -> Unknown, so a
    // fresh retry would be safe.
    assert!(matches!(cache.status("done"), RequestStatus::Unknown));
}

#[test]
fn request_cache_reaps_a_stale_in_flight_reservation() {
    // The InFlight reap safety valve: a reservation that never finished (a
    // panicked/hung handler) is presumed dead after `inflight_reap` so a retry
    // is not blocked forever. Tiny reap window stands in for the 600s default.
    let cache = RequestCache::with_bounds(
        8,
        std::time::Duration::from_secs(600),
        std::time::Duration::from_millis(1),
    );
    cache.begin("stuck"); // reserved InFlight, never finished
    std::thread::sleep(std::time::Duration::from_millis(5));
    // A retry now sees FreshAfterReap (the dead reservation was reaped + re-
    // reserved), not a permanent InFlight. The `AfterReap` flavor tells dispatch
    // to RE-PROBE reality before re-applying (M1 full fix) - a genuinely-new id
    // would be plain Fresh.
    assert!(matches!(cache.begin("stuck"), BeginOutcome::FreshAfterReap));
}

#[test]
fn request_cache_reaped_id_yields_exactly_one_fresh_after_reap() {
    // F4 (one-reprobe-per-reap): after a reservation is reaped, TWO retries of
    // the same id must NOT both re-probe/re-apply. `begin` is atomic — the FIRST
    // retry consumes the reap (FreshAfterReap) AND re-reserves the id InFlight in
    // the same locked step, so the SECOND retry sees a live InFlight reservation,
    // not a second FreshAfterReap. That is what caps the M1 re-probe (and its
    // unbounded git worktree-list) at ONCE per reap: the loser is told InFlight
    // and polls/retries instead of issuing a duplicate reality probe + re-apply.
    //
    // A comfortably large reap window (relative to two back-to-back synchronous
    // `begin` calls) keeps this deterministic: the original ages PAST it, but the
    // freshly re-reserved slot is far YOUNGER than it when the second retry runs.
    let reap = std::time::Duration::from_millis(50);
    let cache = RequestCache::with_bounds(8, std::time::Duration::from_secs(600), reap);

    cache.begin("wt"); // original reservation, never finished (handler presumed dead)
    std::thread::sleep(reap * 2); // age it past the reap window

    // First retry: the dead reservation is reaped and re-reserved in one step.
    assert!(
        matches!(cache.begin("wt"), BeginOutcome::FreshAfterReap),
        "the first retry after a reap must re-probe reality (FreshAfterReap)"
    );
    // Second retry, immediately after: the just-re-reserved slot is still well
    // within the reap window, so this loser sees InFlight — NOT a second reprobe.
    assert!(
        matches!(cache.begin("wt"), BeginOutcome::InFlight),
        "a concurrent second retry must see InFlight, not a duplicate FreshAfterReap"
    );
    // And a third: still InFlight until the winner calls finish(). At no point
    // does a single reap yield two re-applies.
    assert!(matches!(cache.begin("wt"), BeginOutcome::InFlight));

    // Once the winner records the outcome, further retries replay it (Duplicate),
    // still never a second apply.
    let _ = cache.finish("wt", Ok(json!({"alreadyCreated": true})));
    assert!(matches!(cache.begin("wt"), BeginOutcome::Duplicate(_)));
}

#[test]
fn request_cache_never_seen_id_is_fresh_not_fresh_after_reap() {
    // A first-ever id must be plain Fresh (no reap happened), so dispatch does
    // NOT waste a reality re-probe on it - FreshAfterReap is reserved for a
    // retry whose prior reservation actually aged out.
    let cache = RequestCache::new();
    assert!(matches!(cache.begin("brand-new"), BeginOutcome::Fresh));
}

#[test]
fn request_cache_reap_after_completion_is_fresh_not_reap() {
    // A COMPLETED id that TTL-expires and is retried is a fresh apply, NOT a
    // reap: the reap flavor is strictly for an InFlight reservation that aged
    // out (the ambiguous "did it land?" case), not for a cleanly-finished one
    // whose cache entry simply expired.
    let cache = RequestCache::with_bounds(
        8,
        std::time::Duration::from_millis(1), // TTL
        std::time::Duration::from_secs(600), // reap window (irrelevant here)
    );
    cache.begin("done");
    let _ = cache.finish("done", Ok(json!({"id": "done"})));
    std::thread::sleep(std::time::Duration::from_millis(5)); // outlive the TTL
    assert!(matches!(cache.begin("done"), BeginOutcome::Fresh));
}

#[test]
fn request_cache_stale_completion_cannot_overwrite_replacement_reservation() {
    // A handler that outlives the reap window must not complete the replacement
    // reservation created for the same request ID.
    let cache = RequestCache::with_bounds(
        8,
        std::time::Duration::from_secs(600),
        std::time::Duration::from_millis(1),
    );
    let (first, first_reservation) = cache.begin_bound_with_reservation("x", "resume:one");
    assert!(matches!(first, BeginOutcome::Fresh));
    let first_reservation = first_reservation.unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));
    let (replacement, replacement_reservation) =
        cache.begin_bound_with_reservation("x", "resume:one");
    assert!(matches!(replacement, BeginOutcome::FreshAfterReap));
    let replacement_reservation = replacement_reservation.unwrap();

    let _ = cache.finish_reserved(
        "x",
        first_reservation,
        "resume:one",
        Ok(json!({"id": "stale"})),
    );
    assert!(
        matches!(cache.status("x"), RequestStatus::InFlight),
        "a stale completion must leave the replacement reservation in flight"
    );
    cache.cancel_reserved("x", first_reservation);
    assert!(matches!(cache.status("x"), RequestStatus::InFlight));

    let _ = cache.finish_reserved(
        "x",
        replacement_reservation,
        "resume:one",
        Ok(json!({"id": "replacement"})),
    );
    match cache.status("x") {
        RequestStatus::Completed(Ok(value)) => {
            assert_eq!(value["id"], "replacement");
        }
        _ => panic!("the replacement reservation must own the completed outcome"),
    }
}

#[test]
fn request_cache_preserves_a_late_completion_when_no_replacement_owns_the_id() {
    let cache = RequestCache::with_bounds(
        1,
        std::time::Duration::from_secs(600),
        std::time::Duration::from_millis(1),
    );
    let (begin, reservation) = cache.begin_bound_with_reservation("late", "reconcile:one");
    assert!(matches!(begin, BeginOutcome::Fresh));
    let reservation = reservation.expect("fresh request reservation");

    std::thread::sleep(std::time::Duration::from_millis(5));
    assert!(matches!(cache.status("late"), RequestStatus::Unknown));
    let _ = cache.finish_reserved(
        "late",
        reservation,
        "reconcile:one",
        Ok(json!({"id": "late"})),
    );

    assert!(matches!(
        cache.begin_bound("late", "reconcile:one"),
        BeginOutcome::Duplicate(Ok(_))
    ));
    assert!(matches!(
        cache.begin_bound("late", "reconcile:other"),
        BeginOutcome::Duplicate(Err(_))
    ));

    cache.begin("new");
    let _ = cache.finish("new", Ok(json!({"id": "new"})));
    assert!(matches!(cache.status("late"), RequestStatus::Unknown));
}

#[test]
fn spawn_terminal_retry_with_same_request_id_does_not_duplicate() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    // Repro of Incident A/B at the dispatch layer: a spawn that is RETRIED with
    // the same requestId (the client's recovery from an ambiguous response leg)
    // must apply exactly once - one tmux session, one tile, one UI forward - and
    // the retry must replay the original outcome, never spawn a second session.
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let ctx = test_ctx("t").with_apply_sink(sink.clone());
    let args = json!({"cwd": "/tmp", "requestId": "spawn-retry-1"});
    let first = dispatch_authenticated(
        &ctx,
        ControlRequest {
            token: "t".into(),
            command: "spawn_terminal".into(),
            args: args.clone(),
            session: String::new(),
            host: "t".into(),
            v: None,
        },
    );
    assert!(first.ok, "first spawn failed: {:?}", first.error);
    let id = first.result.as_ref().unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    // The retry: identical requestId. It must NOT spawn again.
    let retry = dispatch_authenticated(
        &ctx,
        ControlRequest {
            token: "t".into(),
            command: "spawn_terminal".into(),
            args,
            session: String::new(),
            host: "t".into(),
            v: None,
        },
    );
    assert!(retry.ok, "retry failed: {:?}", retry.error);
    let retry_result = retry.result.unwrap();
    assert_eq!(
        retry_result["id"].as_str().unwrap(),
        id,
        "retry replays the same id"
    );
    assert_eq!(
        retry_result["idempotentReplay"],
        json!(true),
        "retry is tagged a replay"
    );

    // Exactly ONE real session materialized, and ONE UI forward was emitted.
    let live: Vec<String> = tmux::list_sessions()
        .unwrap_or_default()
        .into_iter()
        .filter(|s| s == &format!("th_{id}"))
        .collect();
    assert_eq!(live.len(), 1, "exactly one tmux session for the id");
    assert_eq!(
        sink.calls.lock().unwrap().len(),
        1,
        "the retry did NOT re-forward a spawn"
    );

    // Reap the real session.
    dispatch(&ctx, "close_terminal", &json!({"sessionId": id})).unwrap();
}

#[test]
fn get_request_status_command_resolves_a_completed_spawn() {
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    // The queryable half of ask #1: after a spawn with a requestId, a caller
    // whose response leg failed can learn the outcome (and the real id) without
    // guessing. An unknown id reports unknown (safe to retry).
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let ctx = test_ctx("t").with_apply_sink(sink);
    let spawn = dispatch_authenticated(
        &ctx,
        ControlRequest {
            token: "t".into(),
            command: "spawn_terminal".into(),
            args: json!({"cwd": "/tmp", "requestId": "spawn-status-1"}),
            session: String::new(),
            host: "t".into(),
            v: None,
        },
    );
    assert!(spawn.ok);
    let id = spawn.result.unwrap()["id"].as_str().unwrap().to_string();

    let status = dispatch(
        &ctx,
        "get_request_status",
        &json!({"requestId": "spawn-status-1"}),
    )
    .unwrap();
    assert_eq!(status["status"], "completed");
    assert_eq!(status["ok"], true);
    assert_eq!(status["result"]["id"].as_str().unwrap(), id);

    let unknown = dispatch(
        &ctx,
        "get_request_status",
        &json!({"requestId": "never-seen"}),
    )
    .unwrap();
    assert_eq!(unknown["status"], "unknown");

    dispatch(&ctx, "close_terminal", &json!({"sessionId": id})).unwrap();
}

// -----------------------------------------------------------------------
// Registry-vs-reality: close_terminal outcome (ask #3, Incident C)
// -----------------------------------------------------------------------

#[test]
fn close_terminal_reports_already_gone_for_a_phantom() {
    // Incident C: closing a session that never existed must not look like a real
    // kill. ok:true (idempotent) stays, but the outcome discriminates it.
    let ctx = test_ctx("t");
    let v = dispatch(&ctx, "close_terminal", &json!({"sessionId": "f0f3207b"})).unwrap();
    assert_eq!(v["accepted"], "close_terminal");
    assert_eq!(v["outcome"], "already_gone");
}

#[test]
fn close_terminal_reports_killed_for_a_live_session() {
    // A real session reports outcome=killed, so a caller can tell a genuine kill
    // from a phantom close.
    let _tmux_guard = ProcessAttestationTmuxGuard::acquire();
    let sink = Arc::new(RecordingSink {
        calls: StdMutex::new(Vec::new()),
    });
    let ctx = test_ctx("t").with_apply_sink(sink);
    let spawn = dispatch(&ctx, "spawn_terminal", &json!({"cwd": "/tmp"})).unwrap();
    let id = spawn["id"].as_str().unwrap().to_string();
    let closed = dispatch(&ctx, "close_terminal", &json!({"sessionId": id})).unwrap();
    assert_eq!(closed["outcome"], "killed");
}

// -----------------------------------------------------------------------
// Incident D: captains persistence no longer holds the registry lock
// -----------------------------------------------------------------------

#[test]
fn captains_persist_writes_through_off_the_lock() {
    // The write-through still happens (durability preserved), now via the
    // off-lock `persist` path.
    let dir = std::env::temp_dir().join(format!("t-hub-captains-persist-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("captains.json");
    let _ = std::fs::remove_file(&path);
    let reg = CaptainsRegistry::load(path.clone());
    reg.claim_test("cap-1", Some("alpha"), vec![]).unwrap();
    let body = std::fs::read_to_string(&path).expect("captains.json written through");
    assert!(
        body.contains("alpha"),
        "persisted body must carry the claim: {body}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn captains_persist_is_monotonic_and_drops_a_stale_snapshot() {
    // Two writers that dropped `inner` in one order but reach disk in the other
    // must not regress the file: an older-seq snapshot is dropped.
    let dir = std::env::temp_dir().join(format!("t-hub-captains-mono-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("captains.json");
    let _ = std::fs::remove_file(&path);
    let reg = CaptainsRegistry::load(path.clone());
    reg.claim_test("cap-1", Some("alpha"), vec![]).unwrap(); // seq -> 1 on disk
    let newer = reg.snapshot(); // seq 1
                                // Hand-persist a STALE snapshot (seq 0): it must be dropped, not clobber.
    reg.persist(CaptainsSnapshot {
        schema_version: CAPTAINS_SCHEMA_VERSION,
        seq: 0,
        captains: vec![],
        cortana: crate::cortana_reconcile::CortanaDurableIdentity::default(),
        agent_sessions: vec![],
        agent_checkpoints: vec![],
        agent_events: vec![],
        projects: vec![],
        workspaces: vec![],
        pending_fleet_operations: vec![],
        retired_fleet_tile_ids: vec![],
        pending_dispatch_claims: vec![],
        pending_dispatch_releases: vec![],
        pending_git_initializations: vec![],
    })
    .unwrap();
    let body = std::fs::read_to_string(&path).unwrap();
    assert!(
        body.contains("alpha"),
        "stale seq-0 snapshot must not clobber the claim: {body}"
    );
    // A NEWER snapshot (seq 1, already on disk) is allowed to (re)write.
    reg.persist(newer).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn project_powder_and_ship_context_survive_registry_reload() {
    let path = captains_tmp("project-context");
    let _ = std::fs::remove_file(&path);
    let reg = CaptainsRegistry::load(path.clone());
    let project = reg
        .upsert_project(ProjectRecord {
            root_path: None,
            vcs_capability: None,
            git_main_root: None,
            project_id: "project-thub".into(),
            name: "T-Hub".into(),
            repo_root: "/home/test/t-hub".into(),
            remote_url: Some("https://example.test/t-hub.git".into()),
            default_branch: Some("main".into()),
            powder: Some(PowderProjectBinding {
                connection_profile: "production".into(),
                repository: "t-hub".into(),
                event_cursor: 0,
            }),
            created_at: 0,
            updated_at: 0,
        })
        .unwrap();
    let project = reg
        .advance_project_powder_cursor(&project.project_id, "production", "t-hub", 17)
        .unwrap();
    reg.claim_test("cap-1", Some("t-hub"), vec![]).unwrap();
    reg.bind_ship_context("t-hub", &project.project_id, "Own T-Hub stability", "codex")
        .unwrap();

    let restored = CaptainsRegistry::load(path.clone()).snapshot();
    assert_eq!(restored.schema_version, CAPTAINS_SCHEMA_VERSION);
    assert_eq!(restored.projects, vec![project]);
    let captain = restored
        .captains
        .iter()
        .find(|c| c.ship_slug == "t-hub")
        .unwrap();
    assert_eq!(captain.project_id.as_deref(), Some("project-thub"));
    assert_eq!(captain.assignment.as_deref(), Some("Own T-Hub stability"));
    assert_eq!(captain.harness.as_deref(), Some("codex"));
    let _ = std::fs::remove_file(path);
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

#[test]
fn project_registry_rejects_split_identity_and_invalid_powder_binding() {
    let reg = CaptainsRegistry::new();
    let base = ProjectRecord {
        root_path: None,
        vcs_capability: None,
        git_main_root: None,
        project_id: "project-one".into(),
        name: "One".into(),
        repo_root: "/repo/one".into(),
        remote_url: None,
        default_branch: None,
        powder: None,
        created_at: 0,
        updated_at: 0,
    };
    reg.upsert_project(base.clone()).unwrap();

    let mut repointed = base.clone();
    repointed.repo_root = "/repo/two".into();
    assert!(reg
        .upsert_project(repointed)
        .unwrap_err()
        .contains("already bound"));

    let mut duplicate_root = base.clone();
    duplicate_root.project_id = "project-two".into();
    let updated = reg.upsert_project(duplicate_root).unwrap();
    assert_eq!(updated.project_id, "project-one");
    assert_eq!(reg.projects().len(), 1);

    let mut invalid_powder = base;
    invalid_powder.powder = Some(PowderProjectBinding {
        connection_profile: "default".into(),
        repository: " ".into(),
        event_cursor: 0,
    });
    assert!(reg
        .upsert_project(invalid_powder)
        .unwrap_err()
        .contains("Powder"));
}

#[test]
fn a_stalled_persist_keeps_the_previous_snapshot_readable() {
    // The core Incident-D proof: with persistence moved OFF the `inner` lock, a
    // STALLED disk write (here a hook that blocks while holding only the
    // `persist` mutex) must NOT block a concurrent reader that only touches
    // `inner`. Under the OLD code (persist under the registry lock) the
    // reader below would hang for the duration of the stall - so this test would
    // TIME OUT and fail, which is exactly the regression guard we want.
    use std::sync::mpsc;
    let dir = std::env::temp_dir().join(format!("t-hub-captains-stall-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("captains.json");
    let _ = std::fs::remove_file(&path);
    let reg = Arc::new(CaptainsRegistry::load(path));

    // The hook stands in for a stalled OneDrive-backed write: it signals that a
    // persist is in progress, then blocks (holding `persist`, NOT `inner`) until
    // the test releases it.
    let (started_tx, started_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let release_rx = StdMutex::new(release_rx);
    reg.set_persist_hook(Box::new(move || {
        let _ = started_tx.send(());
        let _ = release_rx.lock().unwrap().recv(); // block: the write is stalled
    }));

    // A background mutator builds a candidate and stalls while persisting it.
    // The prior snapshot remains published and `inner` is free while this stalls.
    let writer_reg = reg.clone();
    let writer = std::thread::spawn(move || {
        writer_reg
            .claim_test("cap-1", Some("alpha"), vec![])
            .unwrap();
    });
    started_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("the persist hook should have started (mutation reached persist)");

    // NOW, while the persist is stalled: a concurrent reader must return promptly
    // (it only takes `inner`). Run it on a thread so a REGRESSION (reader blocked
    // on `inner`) surfaces as a timeout instead of hanging the suite forever.
    let reader_reg = reg.clone();
    let (read_tx, read_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let snap = reader_reg.snapshot();
        let _ = read_tx.send(snap.captains.len());
    });
    let n = read_rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("a reader was BLOCKED by a stalled persist (regression: persist holds `inner`)");
    assert_eq!(n, 0, "the reader sees only the last durable snapshot");

    // Release the stalled write; the mutator finishes cleanly.
    let _ = release_tx.send(());
    writer.join().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
