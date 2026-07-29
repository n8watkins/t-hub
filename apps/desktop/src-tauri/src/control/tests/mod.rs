use super::*;

mod admin;
mod admission;
mod agents;
mod attestation;
mod audit;
mod capability;
mod captains;
mod commission;
mod comms;
mod cortana_bootstrap;
mod cortana_launch;
mod cortana_quarantine;
mod events;
mod file_commands;
mod fleet;
mod history;
mod idempotency;
mod identity;
mod isolation;
mod keys;
mod leases;
mod listener;
mod preview;
mod projects;
mod protocol;
mod registry_claim;
mod registry_persistence;
mod registry_schema;
mod registry_workspaces;
mod spawn;
mod status;
mod tabs;
mod terminal;
mod worktrees;

use std::sync::{mpsc, Mutex as StdMutex};
use std::thread;

// Real tmux fixture progress can be delayed substantially by the parallel
// workspace suite, while thirty seconds remains a bounded failure signal.
#[cfg(unix)]
const TEST_ASYNC_FIXTURE_TIMEOUT: Duration = Duration::from_secs(30);

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
    let audit_dir = std::env::temp_dir().join(format!(
        "t-hub-ctl-test-{token}-{}",
        uuid::Uuid::new_v4().simple()
    ));
    // A known read token so capability tests can present it; distinct from the
    // control token so ReadOnly vs Full resolution is exercised.
    let mut ctx = ControlContext::new(Arc::new(StatusBridge::new()), visitor, token.to_string())
        .with_read_token(format!("read-{token}"))
        .with_audit(Arc::new(crate::audit::AuditLog::new(audit_dir)));
    ctx.host_token = token.to_string();
    ctx
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

/// `T_HUB_CONTROL_FILE` is PROCESS-GLOBAL: `handshake_path` reads it, and
/// `discovery_file_for_spawn` derives from it the discovery path stamped into
/// the environment of every runtime the control plane launches. A test that
/// mutates it therefore rewrites what every OTHER test in the process observes,
/// including a real-runtime fixture that stamps a tmux session with one value
/// and then re-derives the same value to recognize the runtime it just
/// launched. Mutating it unguarded makes that stamp-then-compare straddle the
/// change and fail.
///
/// [`ControlFileEnv`] is the only sanctioned seam for that variable.
/// [`ControlFileEnv::pin`] holds it still for the caller's scope;
/// [`ControlFileEnv::set`] overrides it and restores the previous value - or
/// its absence - on drop. Both take the same exclusive lock, so no mutation can
/// land between a stamp and its comparison, and no test can leak an override,
/// or the ambient value's deletion, into the rest of the process.
///
/// Poison is ignored: a failed env test must not cascade into unrelated cases.
static CONTROL_FILE_ENV_LOCK: StdMutex<()> = StdMutex::new(());

const CONTROL_FILE_ENV: &str = "T_HUB_CONTROL_FILE";

#[must_use = "the pin only holds while the guard is alive"]
struct ControlFileEnv {
    _lock: std::sync::MutexGuard<'static, ()>,
    previous: Option<std::ffi::OsString>,
    overridden: bool,
}

impl ControlFileEnv {
    fn lock() -> std::sync::MutexGuard<'static, ()> {
        CONTROL_FILE_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Hold the ambient `T_HUB_CONTROL_FILE` still without changing it.
    fn pin() -> Self {
        Self {
            _lock: Self::lock(),
            previous: None,
            overridden: false,
        }
    }

    /// Point `T_HUB_CONTROL_FILE` at `path` until the guard drops.
    fn set(path: impl AsRef<std::ffi::OsStr>) -> Self {
        let lock = Self::lock();
        let previous = std::env::var_os(CONTROL_FILE_ENV);
        std::env::set_var(CONTROL_FILE_ENV, path);
        Self {
            _lock: lock,
            previous,
            overridden: true,
        }
    }
}

impl Drop for ControlFileEnv {
    fn drop(&mut self) {
        if !self.overridden {
            return;
        }
        match self.previous.take() {
            Some(previous) => std::env::set_var(CONTROL_FILE_ENV, previous),
            None => std::env::remove_var(CONTROL_FILE_ENV),
        }
    }
}

/// Serialize process-attestation fixtures and keep an anchor alive while a
/// case runs. This prevents one test's final-session shutdown from racing
/// another test's session creation. Dropping the guard reaps the anchor and
/// independently probes its absence, including after a successful final
/// removal that tmux reports as `server exited unexpectedly`.
///
/// The guard also pins the process-global `T_HUB_CONTROL_FILE` (see
/// [`ControlFileEnv`]): every real-runtime fixture stamps that value into the
/// sessions it launches and re-derives it to recognize them, so it has to hold
/// still for the whole case.
struct ProcessAttestationTmuxGuard {
    _lifecycle: tmux::TestLifecycleGuard,
    _control_file: ControlFileEnv,
}

impl ProcessAttestationTmuxGuard {
    fn acquire() -> Self {
        // Pin the control-file BEFORE the tmux lifecycle lock so every holder
        // takes the two in one order; the fields release them in reverse.
        let control_file = ControlFileEnv::pin();
        Self {
            _lifecycle: tmux::TestLifecycleGuard::acquire(),
            _control_file: control_file,
        }
    }
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

// ---- s27: attach path vs client churn -----------------------------------

use std::time::Duration;

/// The attach-churn tests share the process-global forwarder counter (and
/// real tmux sessions), so they run serialized; everything else in this
/// module stays parallel. Poison is ignored: a failed churn test must not
/// cascade into the other one.
static ATTACH_TEST_SERIAL: StdMutex<()> = StdMutex::new(());

static REBIND_TEST_SEQ: AtomicU64 = AtomicU64::new(0);

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

// ---- Captains registry (captain-chat phase 2) -------------------------

/// A unique temp path for a captains persistence file (removed by the caller).
fn captains_tmp(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "t-hub-captains-test-{tag}-{}.json",
        uuid::Uuid::new_v4().simple()
    ))
}

const SCHEMA_13_REGISTRY_FIXTURE: &str = include_str!("../../fixtures/captains-schema-13.json");
const SCHEMA_17_REGISTRY_FIXTURE: &str = include_str!("../../fixtures/captains-schema-17.json");
const SCHEMA_18_REGISTRY_FIXTURE: &str = include_str!("../../fixtures/captains-schema-18.json");
const PACKAGED_SCHEMA_25_LEGACY_ORPHAN_FIXTURE: &str =
    include_str!("../../fixtures/captains-schema-25-packaged-legacy-orphan.json");
const PACKAGED_SCHEMA_25_OBSERVED_LAUNCH_FIXTURE: &str =
    include_str!("../../fixtures/captains-schema-25-packaged-observed-launch.json");

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

fn clean_audit(dir: &std::path::Path) {
    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::remove_file(crate::audit::head_path_for_test(dir));
    let _ = std::fs::remove_file(crate::audit::key_path_for_test(dir));
    let _ = std::fs::remove_file(crate::audit::journal_path_for_test(dir));
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
