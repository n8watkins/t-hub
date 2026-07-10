//! Managed TTS-engine lifecycle: T-Hub owns the Kokoro server as a child
//! process, health-watches it, auto-restarts it (bounded backoff), and on a
//! persistent failure AUTO-FALLS-BACK to Piper so voice keeps flowing while the
//! general is always told (toast + Settings amber state). Auto-switches back to
//! the selected engine after a stability window. Approved design:
//! /tmp/flap-probe/LIFECYCLE-PROPOSAL.md (general-approved D1-D4).
//!
//! DESIGN SPLIT (the whole point of this module's shape):
//!   * The PURE STATE MACHINE ([`Supervisor`]) is platform-agnostic and takes an
//!     injected `now` millis clock, so every policy decision - down-detection,
//!     the fallback edge, restart backoff, max-retries-then-hold, the 30s
//!     switch-back hysteresis, the voice remap, and the startup squatter tiers -
//!     is unit-tested on any OS with no processes and no time.
//!   * The PLATFORM LAYER ([`platform`]) does the actual spawn/kill. Kokoro is a
//!     WSL Linux process reached from the Windows app, so it is spawned via
//!     `wsl.exe` with a no-orphan guarantee (WSL-side stdin lifeline + boot
//!     reaper, hardened on Windows by a Job Object). Piper is a NATIVE Windows
//!     process, so it is a plain job-owned child. Every spawn leg is BOUNDED:
//!     this host's evidence is that `wsl.exe` spawns go glacial under Windows
//!     memory pressure (see bounded_exec.rs / the residual control-flap fixes
//!     #45/#48/#50), so a slow spawn is a timeout+backoff, never a hang.
//!   * The RUNTIME DRIVER ([`runtime`]) glues them: a watcher thread that probes,
//!     feeds the state machine, executes its actions, and emits status/toasts to
//!     the webview over the control://event stream. It runs ONLY behind the
//!     default-OFF `T_HUB_MANAGED_KOKORO` flag, so shipping this module changes
//!     NOTHING at runtime until the migration flip - the live unit-owned Kokoro
//!     keeps serving untouched (proposal migration step 1).

use crate::voice::VoiceEngine;
use serde::Serialize;

// ---------------------------------------------------------------------------
// Config (overridable in tests via SupConfig so the policy is driven fast)
// ---------------------------------------------------------------------------

/// Tunables for the state machine. Real runtime uses [`SupConfig::default`];
/// tests shrink the windows so a whole fallback+recover cycle runs in a handful
/// of synthetic ticks.
#[derive(Debug, Clone, Copy)]
pub struct SupConfig {
    /// Consecutive failed probes before an engine is declared Down. >1 so a
    /// single dropped probe never triggers a fallback flap.
    pub down_threshold: u32,
    /// The primary must stay green (healthy) THIS long before we switch back to
    /// it from a fallback - D1's hysteresis against engine flap.
    pub switchback_green_ms: u64,
    /// Max restart attempts inside `restart_window_ms` before we STOP hammering
    /// the primary and simply hold on the standby (slow recovery re-probe still
    /// runs), so a corrupt-model crash-loop can't spin `wsl.exe` forever.
    pub max_restarts: u32,
    pub restart_window_ms: u64,
    /// Restart backoff schedule (ms), indexed by attempt number; the last entry
    /// is the cap for any further attempts. No tight crash loop.
    pub backoff_ms: &'static [u64],
}

impl Default for SupConfig {
    fn default() -> Self {
        Self {
            down_threshold: 3,
            switchback_green_ms: 30_000,
            max_restarts: 3,
            restart_window_ms: 90_000,
            // 1s, 2s, 4s, 8s, then cap at 30s.
            backoff_ms: &[1_000, 2_000, 4_000, 8_000, 30_000],
        }
    }
}

// ---------------------------------------------------------------------------
// Pure state
// ---------------------------------------------------------------------------

/// Reachability of one engine as the watcher currently sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Health {
    /// Not probed yet (rendered "checking" in the UI).
    Unknown,
    Up,
    Down,
}

/// Per-engine watcher bookkeeping.
#[derive(Debug, Clone, Copy)]
struct EngineTrack {
    health: Health,
    consecutive_fails: u32,
    /// When the engine most recently BECAME healthy (for the switch-back
    /// hysteresis). None whenever it is not currently Up.
    green_since: Option<u64>,
    // Restart bookkeeping (only meaningful for a managed primary).
    restart_attempts: u32,
    restart_window_start: Option<u64>,
    /// Earliest time the next restart may fire (backoff gate). 0 = eligible now.
    backoff_until: u64,
}

impl EngineTrack {
    fn new() -> Self {
        Self {
            health: Health::Unknown,
            consecutive_fails: 0,
            green_since: None,
            restart_attempts: 0,
            restart_window_start: None,
            backoff_until: 0,
        }
    }
}

/// The overall degraded flavor, surfaced to the UI as the green/amber/red ladder
/// (the approved #52 follow-up amber state lands here).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeLevel {
    /// Selected engine healthy and active.
    Green,
    /// Voice is flowing, but on the STANDBY engine (selected engine down).
    Amber,
    /// Both engines down - voice unavailable (captain path degrades to SAPI).
    Red,
    /// Nothing probed yet.
    Unknown,
}

/// The pure lifecycle state machine. No OS, no wall clock - every method takes
/// `now` (millis) so behavior is fully deterministic under test.
#[derive(Debug, Clone)]
pub struct Supervisor {
    cfg: SupConfig,
    /// The user's preferred engine (from voice.json) = the PRIMARY we manage.
    selected: VoiceEngine,
    /// Where synthesis is currently routed. Equals `selected` unless we've
    /// fallen back.
    active: VoiceEngine,
    kokoro: EngineTrack,
    piper: EngineTrack,
    /// True while `active != selected` due to a primary failure.
    degraded: bool,
}

/// A side effect the driver must carry out. The state machine only DECIDES; the
/// platform layer ACTS, which keeps the machine pure and testable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Ensure the given engine's child is running (spawn if not already up).
    EnsureRunning(VoiceEngine),
    /// (Re)start the given engine's child after a crash - distinct from
    /// EnsureRunning so the driver can log/emit a restart specifically.
    Restart(VoiceEngine),
    /// Route synthesis to this engine from now on.
    SetActive(VoiceEngine),
    /// Fire a user-visible toast (reuses notify kinds: "error" | "done").
    Toast {
        kind: &'static str,
        title: String,
        body: String,
    },
    /// Push a fresh runtime-status snapshot to the webview.
    EmitStatus,
}

/// What we found already serving the primary's port at app startup, fed to the
/// squatter classifier (D2). Kept as plain data so the tiering is pure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartupProbe {
    /// Something is answering on the port.
    pub served: bool,
    /// The occupant's `/health` self-identified engine, if it answered as a
    /// known TTS engine (None = not a TTS engine / no/blank identity).
    pub engine: Option<VoiceEngine>,
    /// The occupant is OUR interim systemd user unit (`systemctl --user
    /// is-active kokoro-tts.service`).
    pub is_our_unit: bool,
    /// The occupant's pid matches our lifeline pid-marker file (a leaked child
    /// of a previous app run).
    pub marker_matches: bool,
}

/// The startup action the app takes for the primary port (D2). Never kills a
/// process it cannot positively identify as the TTS engine it manages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupAction {
    /// Port free - just spawn the managed child.
    Spawn,
    /// Our interim systemd unit owns it: `disable --now` then spawn (D3 keeps
    /// the unit file on disk, only disabled).
    DisableUnitThenSpawn,
    /// A leaked prior child (marker) or a bare same-engine server: reclaim the
    /// port (kill the confirmed-ours/known engine), then spawn.
    ReclaimThenSpawn,
    /// A stranger holds the port: DO NOT kill it. Run degraded on the standby
    /// and tell the general (D2's refuse-and-fallback).
    RefuseAndFallback,
}

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

/// The other engine (Piper <-> Kokoro). With two engines the standby is simply
/// "not the selected one".
fn other(engine: VoiceEngine) -> VoiceEngine {
    match engine {
        VoiceEngine::Piper => VoiceEngine::Kokoro,
        VoiceEngine::Kokoro => VoiceEngine::Piper,
    }
}

/// Classify a startup port occupant into the reclaim/refuse tiers (D2). Pure and
/// standalone so the whole policy is table-testable.
pub fn classify_startup(probe: StartupProbe, managed: VoiceEngine) -> StartupAction {
    if !probe.served {
        return StartupAction::Spawn;
    }
    // Our own interim unit: disable it (D3), then own the port.
    if probe.is_our_unit {
        return StartupAction::DisableUnitThenSpawn;
    }
    // A leaked child of ours, or a bare instance of the very engine we manage:
    // provably ours/known -> safe to reclaim.
    if probe.marker_matches || probe.engine == Some(managed) {
        return StartupAction::ReclaimThenSpawn;
    }
    // Anything else answering on our port is a STRANGER. Never kill it.
    StartupAction::RefuseAndFallback
}

// Voice remap on fallback (the wave-1 correctness catch - the selected Kokoro
// voice `af_heart` is not a Piper voice, so a fallback must substitute a valid
// Piper voice) lives FRONTEND-side (store/engine.ts `effectiveVoice`), because
// the in-app synthesis calls originate there (voiceAnnounce.ts / the Test
// button pass engine+voice to voice_tts). The captain path (announce.sh) hits
// 7478 directly and is out of the app's routing entirely, so there is no
// backend synthesis site to remap - keeping the remap in one place, frontend.

// ---------------------------------------------------------------------------
// Pure state machine
// ---------------------------------------------------------------------------

impl Supervisor {
    pub fn new(selected: VoiceEngine, cfg: SupConfig) -> Self {
        Self {
            cfg,
            selected,
            active: selected,
            kokoro: EngineTrack::new(),
            piper: EngineTrack::new(),
            degraded: false,
        }
    }

    pub fn active(&self) -> VoiceEngine {
        self.active
    }
    pub fn selected(&self) -> VoiceEngine {
        self.selected
    }

    fn track(&self, engine: VoiceEngine) -> &EngineTrack {
        match engine {
            VoiceEngine::Kokoro => &self.kokoro,
            VoiceEngine::Piper => &self.piper,
        }
    }
    fn track_mut(&mut self, engine: VoiceEngine) -> &mut EngineTrack {
        match engine {
            VoiceEngine::Kokoro => &mut self.kokoro,
            VoiceEngine::Piper => &mut self.piper,
        }
    }

    /// The green/amber/red ladder for the UI.
    pub fn level(&self) -> RuntimeLevel {
        let primary = self.track(self.selected).health;
        let standby = self.track(other(self.selected)).health;
        if self.degraded {
            // We've fallen back. Amber while the standby carries voice; red if it
            // too is down; a recovering primary stays amber until switch-back
            // actually flips `active` back (D1's hysteresis).
            return match standby {
                Health::Up | Health::Unknown => RuntimeLevel::Amber,
                Health::Down => RuntimeLevel::Red,
            };
        }
        match primary {
            Health::Up => RuntimeLevel::Green,
            // Not degraded yet (e.g. inside the down-threshold debounce): red only
            // if BOTH engines are down, else amber (we're about to fall back).
            Health::Down if standby == Health::Down => RuntimeLevel::Red,
            Health::Down => RuntimeLevel::Amber,
            Health::Unknown => RuntimeLevel::Unknown,
        }
    }

    /// Fold one probe result into the state and return the actions it triggers.
    pub fn on_probe(&mut self, engine: VoiceEngine, reachable: bool, now: u64) -> Vec<Action> {
        let down_threshold = self.cfg.down_threshold;
        let t = self.track_mut(engine);
        if reachable {
            t.consecutive_fails = 0;
            if t.health != Health::Up {
                t.health = Health::Up;
                // A fresh Down->Up recovery resets the restart budget.
                t.restart_attempts = 0;
                t.restart_window_start = None;
                t.backoff_until = 0;
            }
            // (Re)stamp the green clock on the FIRST ok after any gap - including
            // a transient single-fail that nulled it without dropping to Down -
            // so the switch-back hysteresis clock restarts on every recovery and
            // a flapping primary can never satisfy the stability window.
            if t.green_since.is_none() {
                t.green_since = Some(now);
            }
        } else {
            t.consecutive_fails += 1;
            t.green_since = None;
            if t.consecutive_fails >= down_threshold {
                t.health = Health::Down;
            }
        }
        self.reconcile(now)
    }

    /// Time-driven transitions (backoff restart eligibility, switch-back
    /// hysteresis). Called on every watcher tick.
    pub fn on_tick(&mut self, now: u64) -> Vec<Action> {
        self.reconcile(now)
    }

    /// The single decision point: given current health, decide fallback,
    /// restart, and switch-back. Idempotent - safe to call after every event.
    fn reconcile(&mut self, now: u64) -> Vec<Action> {
        let mut actions = Vec::new();
        let primary = self.selected;
        let standby = other(primary);
        let primary_health = self.track(primary).health;

        // --- Fallback edge: primary went Down while we were on it. ------------
        if !self.degraded && primary_health == Health::Down {
            self.degraded = true;
            self.active = standby;
            actions.push(Action::EnsureRunning(standby));
            actions.push(Action::SetActive(standby));
            actions.push(Action::Toast {
                kind: "error",
                title: "Voice fell back".to_string(),
                body: format!(
                    "{} is unreachable; announcements continue on {}.",
                    label(primary),
                    label(standby)
                ),
            });
            actions.push(Action::EmitStatus);
        }

        // --- Restart the primary on backoff while we're degraded. ------------
        if self.degraded && primary_health == Health::Down {
            if let Some(a) = self.maybe_restart_primary(now) {
                actions.push(a);
            }
        }

        // --- Keep the STANDBY alive while we depend on it (F3). --------------
        // The fallback edge spawns the standby once; if that spawn failed, or the
        // standby dies mid-fallback, re-issue EnsureRunning on a backoff so the
        // "both dead" cell recovers instead of sitting red forever. Backed off
        // (not a tight respawn loop).
        if self.degraded {
            if let Some(a) = self.maybe_ensure_standby(now) {
                actions.push(a);
            }
        }

        // --- Switch back once the primary has been green long enough (D1). ---
        if self.degraded && primary_health == Health::Up {
            let green_for = self
                .track(primary)
                .green_since
                .map(|g| now.saturating_sub(g))
                .unwrap_or(0);
            if green_for >= self.cfg.switchback_green_ms {
                self.degraded = false;
                self.active = primary;
                actions.push(Action::SetActive(primary));
                actions.push(Action::Toast {
                    kind: "done",
                    title: format!("{} recovered", label(primary)),
                    body: format!("Voice is back on {}.", label(primary)),
                });
                actions.push(Action::EmitStatus);
            }
        }

        actions
    }

    /// Emit a Restart action iff the backoff gate is open and we're still inside
    /// the retry budget; otherwise hold on the standby (slow re-probe recovers).
    fn maybe_restart_primary(&mut self, now: u64) -> Option<Action> {
        let primary = self.selected;
        let cfg = self.cfg;
        let t = self.track_mut(primary);

        // Roll the retry window.
        match t.restart_window_start {
            Some(start) if now.saturating_sub(start) > cfg.restart_window_ms => {
                t.restart_window_start = Some(now);
                t.restart_attempts = 0;
            }
            None => t.restart_window_start = Some(now),
            _ => {}
        }

        if t.restart_attempts >= cfg.max_restarts {
            return None; // budget exhausted - hold on standby, keep re-probing
        }
        if now < t.backoff_until {
            return None; // still backing off
        }

        let idx = (t.restart_attempts as usize).min(cfg.backoff_ms.len() - 1);
        let delay = cfg.backoff_ms[idx];
        t.restart_attempts += 1;
        t.backoff_until = now + delay;
        Some(Action::Restart(primary))
    }

    /// Emit a backed-off `EnsureRunning(standby)` while the standby is Down and
    /// we depend on it (F3). Unlike the primary restart there is no give-up
    /// budget: the standby is the last line of voice, so we keep (slowly)
    /// retrying it until it comes up.
    fn maybe_ensure_standby(&mut self, now: u64) -> Option<Action> {
        let standby = other(self.selected);
        let cfg = self.cfg;
        let t = self.track_mut(standby);
        if t.health != Health::Down {
            return None;
        }
        if now < t.backoff_until {
            return None;
        }
        let idx = (t.restart_attempts as usize).min(cfg.backoff_ms.len() - 1);
        t.restart_attempts += 1;
        t.backoff_until = now + cfg.backoff_ms[idx];
        Some(Action::EnsureRunning(standby))
    }

    /// A serializable view for the `engine_runtime_status` command + events.
    pub fn snapshot(&self) -> SupervisorSnapshot {
        SupervisorSnapshot {
            managed: true,
            selected_engine: self.selected,
            active_engine: self.active,
            degraded: self.degraded,
            level: self.level(),
            kokoro: self.kokoro.health,
            piper: self.piper.health,
        }
    }
}

/// A short human label for an engine (toast/UI text).
fn label(engine: VoiceEngine) -> &'static str {
    match engine {
        VoiceEngine::Kokoro => "Kokoro",
        VoiceEngine::Piper => "Piper",
    }
}

/// The snapshot the webview consumes (command return + `engine://runtime_status`
/// event payload). `managed=false` is what an UNMANAGED build reports so the UI
/// can fall back to #52's direct dual-engine probes.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SupervisorSnapshot {
    /// True when the managed lifecycle is running (flag on); false = legacy.
    pub managed: bool,
    pub selected_engine: VoiceEngine,
    pub active_engine: VoiceEngine,
    pub degraded: bool,
    pub level: RuntimeLevel,
    pub kokoro: Health,
    pub piper: Health,
}

impl SupervisorSnapshot {
    /// The snapshot an UNMANAGED (flag-off) build returns: the supervisor isn't
    /// running, so the frontend keeps using its own #52 health probes.
    fn unmanaged() -> Self {
        Self {
            managed: false,
            selected_engine: VoiceEngine::default(),
            active_engine: VoiceEngine::default(),
            degraded: false,
            level: RuntimeLevel::Unknown,
            kokoro: Health::Unknown,
            piper: Health::Unknown,
        }
    }
}

// ---------------------------------------------------------------------------
// Runtime flag
// ---------------------------------------------------------------------------

/// Whether the managed lifecycle runs. DEFAULT OFF: shipping this module must
/// not disturb the live unit-owned Kokoro (proposal migration step 1); the flag
/// flips only when the migration is deliberately performed on a new binary.
pub fn managed_enabled() -> bool {
    parse_managed_flag(std::env::var("T_HUB_MANAGED_KOKORO").ok().as_deref())
}

/// Pure parse of the flag value (split out so the default-off contract is
/// tested without mutating the process env). Only explicit on-tokens enable it.
fn parse_managed_flag(v: Option<&str>) -> bool {
    matches!(v, Some("1") | Some("true") | Some("on"))
}

// ---------------------------------------------------------------------------
// Command
// ---------------------------------------------------------------------------

/// Current managed-lifecycle status for Settings. Returns the live supervisor
/// snapshot when managed, else an `unmanaged` marker so the webview knows to use
/// its own #52 probes. Reads shared state under a short lock - never blocks on a
/// probe (the watcher thread owns probing).
#[tauri::command]
pub fn engine_runtime_status(
    state: tauri::State<'_, runtime::SharedSnapshot>,
) -> SupervisorSnapshot {
    state.0.lock().map(|s| s.clone()).unwrap_or_else(|p| p.into_inner().clone())
}

// ---------------------------------------------------------------------------
// Platform layer (spawn/kill) - cfg-gated; compiled everywhere, runs on Windows
// ---------------------------------------------------------------------------

pub mod platform {
    //! Actual process spawn/kill across the Windows-app -> WSL boundary. This is
    //! the risk center; it is deliberately thin and driven entirely by the pure
    //! state machine above. NOT exercised by the Linux unit suite (no wsl.exe /
    //! no piper .exe there) - see the module tests for what IS hermetic (the
    //! lifeline script content, the reaper decision, the bounded-spawn plumbing).

    use crate::voice::VoiceEngine;
    use std::time::Duration;

    /// Bound on the run-to-completion adoption/reap calls (`systemctl --user
    /// disable --now`, the pid-marker kill). Generous because this host's
    /// evidence is that `wsl.exe` goes glacial under Windows memory pressure
    /// (bounded_exec.rs, the #45/#48/#50 control-flap fixes): a slow call must be
    /// a bounded timeout, never an indefinite hang that wedges startup.
    ///
    /// (The engine SPAWN itself needs no such per-call bound: `Command::spawn`
    /// returns as soon as the relay process is created; a glacial WSL boot then
    /// shows up as the health probe staying red, which the backoff + amber
    /// "starting" state already handle - so there is no spawn call that can hang.)
    pub const ADOPT_TIMEOUT: Duration = Duration::from_secs(15);

    /// The lifeline pid-marker path (inside WSL): the boot reaper reads it to
    /// reclaim a child leaked by a previous app run.
    pub const KOKORO_PID_MARKER: &str = "/tmp/thub-kokoro.pid";

    /// The WSL-side lifeline wrapper. This is the PRIMARY no-orphan guarantee and
    /// it needs NO Windows API: the app launches this via `wsl.exe` holding a
    /// stdin pipe; when the app process dies for ANY reason (clean exit OR crash
    /// OR kill), the OS closes the pipe, `wsl.exe` sees EOF, `cat` returns, and
    /// the wrapper kills the server's whole process group. Because pipe-EOF is
    /// delivered by the kernel, this fires even when the app runs no exit hooks -
    /// which is exactly the app-crash case the old unsupervised server couldn't
    /// survive. `setsid` puts the server in its own group so the group-kill is
    /// clean, and the pid marker lets the boot reaper mop up if this ever fails.
    ///
    /// `repo_dir` is the absolute Kokoro repo path (holds start.sh). Built as a
    /// single-quoted-safe heredoc-free string; the driver passes it to
    /// `wsl.exe -e bash -c` (—`-e bash` is deliberate: a bare `wsl.exe` runs the
    /// default shell, which on this host is zsh; this ship root-caused blank
    /// labels to that trap).
    pub fn lifeline_script(repo_dir: &str) -> String {
        format!(
            "set -u\n\
             cd '{repo_dir}' || exit 1\n\
             setsid ./start.sh &\n\
             SRV=$!\n\
             echo \"$SRV\" > '{marker}' 2>/dev/null || true\n\
             cleanup() {{ kill -TERM -\"$SRV\" 2>/dev/null; rm -f '{marker}' 2>/dev/null; }}\n\
             trap 'cleanup; exit 0' TERM INT HUP\n\
             cat\n\
             cleanup\n",
            repo_dir = repo_dir,
            marker = KOKORO_PID_MARKER,
        )
    }

    /// Assign a just-spawned child to a kill-on-close Windows Job Object so the
    /// relay (and thus, together with the lifeline, the whole tree) cannot outlive
    /// the app even on a hard crash.
    ///
    /// COMPILE-VERIFIED ON WINDOWS ONLY: the Linux dev env cannot build or run
    /// this leg (no Win32). It is defense-in-depth over the lifeline+reaper, which
    /// already close the orphan hole cross-platform; if the `windows`-crate
    /// feature set needs a tweak, the Windows build surfaces it (honest E2E limit,
    /// same discipline as PR #50/#52). On non-Windows it is a no-op.
    #[cfg(windows)]
    pub fn assign_kill_on_close_job(child: &std::process::Child) -> std::io::Result<()> {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, SetInformationJobObject,
            JobObjectExtendedLimitInformation, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };
        unsafe {
            let job = CreateJobObjectW(None, None)?;
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const core::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )?;
            AssignProcessToJobObject(job, HANDLE(child.as_raw_handle() as _))?;
            // NOTE: the job handle is intentionally leaked - it must stay open for
            // the app's lifetime so KILL_ON_JOB_CLOSE fires when the app process
            // (and thus this handle) is torn down by the OS.
            std::mem::forget(job);
        }
        Ok(())
    }

    #[cfg(not(windows))]
    pub fn assign_kill_on_close_job(_child: &std::process::Child) -> std::io::Result<()> {
        Ok(())
    }

    /// The `wsl.exe -e bash -c <lifeline>` command for the managed Kokoro child.
    /// Returns a configured (not-yet-spawned) Command so the driver can attach a
    /// stdin pipe (the lifeline) and a job object. `CREATE_NO_WINDOW` on Windows
    /// keeps the relay windowless (matches tmux.rs/git.rs/files.rs).
    pub fn kokoro_command(repo_dir: &str) -> std::process::Command {
        let mut cmd = std::process::Command::new("wsl.exe");
        cmd.arg("-e").arg("bash").arg("-c").arg(lifeline_script(repo_dir));
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        cmd
    }

    /// The native-Windows Piper command (`piper-tts-server.exe`), lazily spawned
    /// on fallback. Piper runs on the app's OWN OS, so it is a plain job-owned
    /// child with no WSL boundary. `exe_path` comes from config; `port` lets the
    /// driver stand a fresh instance up (or adopt an existing 7477).
    pub fn piper_command(exe_path: &str, port: u16) -> std::process::Command {
        let mut cmd = std::process::Command::new(exe_path);
        cmd.env("PORT", port.to_string());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let _ = VoiceEngine::Piper; // engine-tagged for call-site clarity
        cmd
    }

    /// The bounded `systemctl --user disable --now kokoro-tts.service` adoption
    /// call (D3: disable, do not delete). Run through bounded_exec so a hung
    /// systemctl never wedges startup.
    pub fn disable_interim_unit_command() -> std::process::Command {
        let mut cmd = std::process::Command::new("wsl.exe");
        cmd.arg("-e")
            .arg("bash")
            .arg("-c")
            .arg("systemctl --user disable --now kokoro-tts.service");
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        cmd
    }
}

// ---------------------------------------------------------------------------
// Runtime driver (thread loop + shared state) - runs ONLY when flag on
// ---------------------------------------------------------------------------

pub mod runtime {
    //! The watcher thread that turns the pure state machine into live behavior:
    //! probe -> feed the machine -> execute its actions (spawn/kill via the
    //! platform layer) -> emit status + toasts to the webview. Runs ONLY when
    //! `managed_enabled()`; not exercised by the unit suite (it needs real
    //! wsl.exe / piper.exe), so it is kept thin and delegates every DECISION to
    //! the tested state machine - it only ACTS.
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    /// Steady-state probe cadence. Deliberately unhurried: this is an ambient
    /// "is it still up?" watch, and each probe is already bounded (2s) backend
    /// side; a slow spawn is covered by the amber "starting" state, not a busy
    /// loop.
    const PROBE_INTERVAL: Duration = Duration::from_secs(5);

    /// Shared latest snapshot the `engine_runtime_status` command reads. Managed
    /// as Tauri state; the watcher thread writes it, commands read it.
    pub struct SharedSnapshot(pub Arc<Mutex<SupervisorSnapshot>>);

    impl SharedSnapshot {
        /// Start UNMANAGED (flag off): the command reports `managed:false` and the
        /// webview keeps using its own #52 probes. When the flag is on the watcher
        /// thread overwrites this with live snapshots.
        pub fn new_unmanaged() -> Self {
            SharedSnapshot(Arc::new(Mutex::new(SupervisorSnapshot::unmanaged())))
        }
        pub fn handle(&self) -> Arc<Mutex<SupervisorSnapshot>> {
            self.0.clone()
        }
    }

    /// Milliseconds since an arbitrary fixed base, for the state machine's `now`.
    /// Monotonic (Instant-based) so it never jumps backwards on a clock change.
    pub fn now_ms(base: Instant) -> u64 {
        base.elapsed().as_millis() as u64
    }

    /// Everything the driver needs to manage the engines. Paths come from config
    /// so the driver has no hard-coded install locations.
    pub struct StartOpts {
        pub selected: VoiceEngine,
        /// Absolute Kokoro repo dir (holds start.sh) inside WSL.
        pub kokoro_repo_dir: String,
        /// Absolute path to the native-Windows Piper server exe.
        pub piper_exe: String,
        pub cfg: SupConfig,
    }

    /// Build StartOpts from the deployment env, or None if the required install
    /// paths aren't configured. The migration step sets `T_HUB_KOKORO_DIR`
    /// (WSL path to the repo) and `T_HUB_PIPER_EXE` (Windows path to the piper
    /// server). Returning None keeps the flag-on path FAIL-SAFE: with no config
    /// we don't spawn anything blindly.
    pub fn opts_from_env(selected: VoiceEngine) -> Option<StartOpts> {
        let kokoro_repo_dir = std::env::var("T_HUB_KOKORO_DIR").ok().filter(|s| !s.trim().is_empty())?;
        let piper_exe = std::env::var("T_HUB_PIPER_EXE").ok().filter(|s| !s.trim().is_empty())?;
        Some(StartOpts {
            selected,
            kokoro_repo_dir,
            piper_exe,
            cfg: SupConfig::default(),
        })
    }

    /// A managed child plus the pieces that must stay alive with it. For Kokoro,
    /// keeping `stdin` open IS the lifeline: dropping it (or the app dying) closes
    /// the pipe and the WSL-side wrapper group-kills the server.
    struct EngineChild {
        child: std::process::Child,
        _stdin: Option<std::process::ChildStdin>,
    }

    impl Drop for EngineChild {
        fn drop(&mut self) {
            // Best-effort: closing stdin fires the lifeline; also kill the relay.
            self._stdin.take();
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    /// Start the watcher thread (idempotent at the call site: only invoked once
    /// from setup, behind the flag). Detached `std::thread` per this crate's
    /// long-lived-task convention (see spawn_agent_connect in lib.rs).
    pub fn start(
        fanout: Arc<crate::control::EventFanout>,
        shared: Arc<Mutex<SupervisorSnapshot>>,
        opts: StartOpts,
    ) {
        std::thread::Builder::new()
            .name("t-hub-engine-supervisor".into())
            .spawn(move || run(fanout, shared, opts))
            .ok();
    }

    fn run(
        fanout: Arc<crate::control::EventFanout>,
        shared: Arc<Mutex<SupervisorSnapshot>>,
        opts: StartOpts,
    ) {
        let base = Instant::now();
        let mut sup = Supervisor::new(opts.selected, opts.cfg);
        let mut children: std::collections::HashMap<u8, EngineChild> =
            std::collections::HashMap::new();

        // Startup adoption (D2/D3): decide what to do about anything already on
        // the primary's port before we spawn.
        let primary = opts.selected;
        let action = classify_startup(startup_probe(primary), primary);
        apply_startup_action(action, primary);

        // Spawn the primary if we now own the port.
        if !matches!(action, StartupAction::RefuseAndFallback) {
            ensure_running(primary, &opts, &mut children);
        }

        loop {
            let now = now_ms(base);
            // Probe the primary always, and the standby whenever it's active
            // (degraded) so recovery on both is seen.
            let mut acts = Vec::new();
            for engine in engines_to_probe(&sup) {
                let base_url = crate::voice::base_url_for_engine(engine);
                let reachable = crate::voice::probe_health_at(engine, &base_url).reachable;
                acts.extend(sup.on_probe(engine, reachable, now));
            }
            acts.extend(sup.on_tick(now));
            for a in acts {
                execute(&a, &opts, &mut children, &fanout, &sup);
            }
            // Always refresh the shared snapshot so the command is never stale.
            write_snapshot(&shared, &sup);
            std::thread::sleep(PROBE_INTERVAL);
        }
    }

    fn engines_to_probe(sup: &Supervisor) -> Vec<VoiceEngine> {
        let mut v = vec![sup.selected()];
        if sup.active() != sup.selected() {
            v.push(sup.active());
        }
        v
    }

    fn key(engine: VoiceEngine) -> u8 {
        match engine {
            VoiceEngine::Piper => 0,
            VoiceEngine::Kokoro => 1,
        }
    }

    fn execute(
        action: &Action,
        opts: &StartOpts,
        children: &mut std::collections::HashMap<u8, EngineChild>,
        fanout: &Arc<crate::control::EventFanout>,
        sup: &Supervisor,
    ) {
        match action {
            Action::EnsureRunning(e) => ensure_running(*e, opts, children),
            Action::Restart(e) => {
                children.remove(&key(*e)); // Drop kills the old child
                ensure_running(*e, opts, children);
            }
            Action::SetActive(_) => { /* reflected in the snapshot */ }
            Action::Toast { kind, title, body } => {
                let payload = serde_json::json!({ "kind": kind, "title": title, "body": body });
                fanout.emit_event("engine://toast", &payload);
            }
            Action::EmitStatus => {
                let payload = serde_json::to_value(sup.snapshot()).unwrap_or_default();
                fanout.emit_event("engine://runtime_status", &payload);
            }
        }
    }

    /// Spawn an engine's child if we don't already manage a live one AND nothing
    /// external already serves its port (adopt-existing, D4's cold-lazy).
    fn ensure_running(
        engine: VoiceEngine,
        opts: &StartOpts,
        children: &mut std::collections::HashMap<u8, EngineChild>,
    ) {
        if children.contains_key(&key(engine)) {
            return;
        }
        let base_url = crate::voice::base_url_for_engine(engine);
        if crate::voice::probe_health_at(engine, &base_url).reachable {
            return; // adopt an already-running instance rather than duplicate
        }
        if let Some(c) = spawn_engine(engine, opts) {
            children.insert(key(engine), c);
        }
    }

    fn spawn_engine(engine: VoiceEngine, opts: &StartOpts) -> Option<EngineChild> {
        use std::process::Stdio;
        let mut cmd = match engine {
            VoiceEngine::Kokoro => {
                let mut c = platform::kokoro_command(&opts.kokoro_repo_dir);
                // stdin piped = the lifeline; stdout/stderr discarded (journald
                // is gone once app-managed, but a crashy engine surfaces via the
                // health probe, not logs we'd have to babysit).
                c.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null());
                c
            }
            VoiceEngine::Piper => {
                let mut c = platform::piper_command(&opts.piper_exe, engine.default_port());
                c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
                c
            }
        };
        let mut child = cmd.spawn().ok()?;
        let stdin = child.stdin.take();
        // Best-effort job-object hardening (no-op off Windows; the lifeline is the
        // portable guarantee).
        let _ = platform::assign_kill_on_close_job(&child);
        Some(EngineChild { child, _stdin: stdin })
    }

    /// Probe the primary's port at startup and build the squatter-policy input
    /// from REAL signals (F1 fix). Critically, the `engine` field comes from the
    /// occupant's SELF-IDENTIFIED `/health` (`probe_identity_at`), NOT a hardcoded
    /// assumption - so a foreign HTTP server squatting the port (which answers
    /// with no recognized `engine`, incl. a 4xx) yields `engine: None` and is
    /// classified a STRANGER (never reclaimed/adopted). `marker_matches` is
    /// computed honestly (the marker pid is alive AND currently owns the port),
    /// not hardcoded false.
    fn startup_probe(engine: VoiceEngine) -> StartupProbe {
        let base_url = crate::voice::base_url_for_engine(engine);
        if !crate::voice::probe_health_at(engine, &base_url).reachable {
            return StartupProbe { served: false, engine: None, is_our_unit: false, marker_matches: false };
        }
        StartupProbe {
            served: true,
            engine: crate::voice::probe_identity_at(&base_url), // real identity, not assumed
            is_our_unit: interim_unit_active(),
            marker_matches: marker_pid_owns_port(engine),
        }
    }

    fn interim_unit_active() -> bool {
        run_bounded_bash("systemctl --user is-active kokoro-tts.service")
            .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "active")
            .unwrap_or(false)
    }

    /// The pid currently LISTENING on `port` inside WSL (via `ss`), or None. The
    /// output parse is split into a pure, tested helper.
    fn port_owner_pid(port: u16) -> Option<u32> {
        let out = run_bounded_bash(&format!("ss -H -ltnp 'sport = :{port}' 2>/dev/null")).ok()?;
        parse_ss_pid(&String::from_utf8_lossy(&out.stdout))
    }

    /// Extract the first `pid=<n>` from `ss -p` output. Pure so the (untestable)
    /// WSL call is thin and the parsing is covered by a unit test.
    pub(crate) fn parse_ss_pid(ss_stdout: &str) -> Option<u32> {
        let marker = "pid=";
        let idx = ss_stdout.find(marker)? + marker.len();
        let digits: String = ss_stdout[idx..].chars().take_while(|c| c.is_ascii_digit()).collect();
        digits.parse().ok()
    }

    /// True iff our lifeline pid-marker names a LIVE pid that ALSO currently owns
    /// the primary port - i.e. a genuine leaked child of a prior app run, not a
    /// stale marker whose pid was reused (F2: the marker alone is never trusted).
    fn marker_pid_owns_port(engine: VoiceEngine) -> bool {
        let marker_pid = run_bounded_bash(&format!("cat '{}' 2>/dev/null", platform::KOKORO_PID_MARKER))
            .ok()
            .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse::<u32>().ok());
        match (marker_pid, port_owner_pid(engine.default_port())) {
            (Some(m), Some(owner)) => m == owner,
            _ => false,
        }
    }

    fn apply_startup_action(action: StartupAction, primary: VoiceEngine) {
        match action {
            StartupAction::DisableUnitThenSpawn => {
                // D3: disable (not delete) the interim unit so it stops racing us.
                // F7: verify the disable actually took before we treat the port as
                // ours - a failed disable leaves a Restart=always unit that would
                // fight the managed child.
                let _ = crate::bounded_exec::output_with_timeout(
                    platform::disable_interim_unit_command(),
                    platform::ADOPT_TIMEOUT,
                );
                if interim_unit_active() {
                    crate::diag::diag_log(
                        "engine_supervisor: `systemctl --user disable --now kokoro-tts.service` \
                         did NOT deactivate the unit - refusing to spawn a managed child while \
                         the unit still owns the port (would fight Restart=always)".to_string(),
                    );
                }
            }
            StartupAction::ReclaimThenSpawn => {
                // F2: reclaim by PORT OWNERSHIP with an identity re-verify AT KILL
                // TIME - never a blind kill of a (possibly reused) stale marker pid.
                reclaim_current_port_owner(primary);
            }
            // Spawn: nothing to clear. RefuseAndFallback: leave the stranger be -
            // the loop falls back to the standby and toasts on the first probe.
            StartupAction::Spawn | StartupAction::RefuseAndFallback => {}
        }
    }

    /// Reclaim the primary port by killing WHOEVER OWNS IT NOW, but only after
    /// re-verifying at kill time that the live occupant is STILL provably our
    /// engine (F2). This eliminates the PID-reuse hazard of the old stale-marker
    /// kill: we never trust a remembered pid, and we abort if the occupant
    /// changed identity between classification and reclaim.
    fn reclaim_current_port_owner(engine: VoiceEngine) {
        let base_url = crate::voice::base_url_for_engine(engine);
        if crate::voice::probe_identity_at(&base_url) != Some(engine) {
            // Occupant is no longer provably ours (raced / a stranger) - do NOT
            // kill. The loop treats the port as unavailable and falls back.
            crate::diag::diag_log(
                "engine_supervisor: reclaim aborted - port occupant is no longer \
                 the identified engine at kill time".to_string(),
            );
            return;
        }
        let Some(pid) = port_owner_pid(engine.default_port()) else { return };
        // Group-kill the CURRENT owner (its pgid), falling back to the bare pid.
        let script = format!(
            "pgid=$(ps -o pgid= -p {pid} 2>/dev/null | tr -d ' '); \
             if [ -n \"$pgid\" ]; then kill -TERM -\"$pgid\" 2>/dev/null || kill -TERM {pid} 2>/dev/null; \
             else kill -TERM {pid} 2>/dev/null; fi",
            pid = pid
        );
        let _ = run_bounded_bash(&script);
    }

    /// Run a bash one-liner inside WSL, bounded (never hangs startup - this host's
    /// `wsl.exe` goes glacial under memory pressure, #45/#48/#50). Windowless on
    /// Windows. Every WSL call in the runtime routes through here.
    fn run_bounded_bash(script: &str) -> std::io::Result<std::process::Output> {
        let mut cmd = std::process::Command::new("wsl.exe");
        cmd.arg("-e").arg("bash").arg("-c").arg(script);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000);
        }
        crate::bounded_exec::output_with_timeout(cmd, platform::ADOPT_TIMEOUT)
    }

    fn write_snapshot(shared: &Arc<Mutex<SupervisorSnapshot>>, sup: &Supervisor) {
        if let Ok(mut g) = shared.lock() {
            *g = sup.snapshot();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shrunk config so a whole fallback + restart + recover cycle runs in a
    /// handful of synthetic millis: down after 2 fails, switch back after 100ms
    /// green, at most 2 restarts per 1000ms, backoff 10/20/40ms.
    fn test_cfg() -> SupConfig {
        SupConfig {
            down_threshold: 2,
            switchback_green_ms: 100,
            max_restarts: 2,
            restart_window_ms: 1000,
            backoff_ms: &[10, 20, 40],
        }
    }

    fn kokoro_primary() -> Supervisor {
        Supervisor::new(VoiceEngine::Kokoro, test_cfg())
    }

    fn has_toast<'a>(actions: &'a [Action], kind: &str) -> Option<&'a Action> {
        actions.iter().find(
            |a| matches!(a, Action::Toast { kind: k, .. } if *k == kind),
        )
    }

    // --- down detection -----------------------------------------------------

    #[test]
    fn down_requires_the_threshold_of_consecutive_fails() {
        let mut s = kokoro_primary();
        s.on_probe(VoiceEngine::Kokoro, true, 0); // Up
        assert_eq!(s.track(VoiceEngine::Kokoro).health, Health::Up);
        // One fail is below threshold (2) - not Down, no fallback.
        let a = s.on_probe(VoiceEngine::Kokoro, false, 1);
        assert_eq!(s.track(VoiceEngine::Kokoro).health, Health::Up);
        assert!(!s.degraded);
        assert!(a.is_empty());
        // Second consecutive fail crosses the threshold -> Down + fallback.
        let a = s.on_probe(VoiceEngine::Kokoro, false, 2);
        assert_eq!(s.track(VoiceEngine::Kokoro).health, Health::Down);
        assert!(s.degraded);
        assert!(has_toast(&a, "error").is_some());
    }

    #[test]
    fn a_recovery_probe_resets_the_fail_run() {
        let mut s = kokoro_primary();
        s.on_probe(VoiceEngine::Kokoro, false, 0); // fails=1
        s.on_probe(VoiceEngine::Kokoro, true, 1); // recover, run cleared
        s.on_probe(VoiceEngine::Kokoro, false, 2); // fails=1 again, NOT down
        assert_eq!(s.track(VoiceEngine::Kokoro).health, Health::Up);
        assert!(!s.degraded);
    }

    // --- fallback edge ------------------------------------------------------

    #[test]
    fn primary_down_edge_falls_back_and_restarts_once() {
        let mut s = kokoro_primary();
        s.on_probe(VoiceEngine::Kokoro, true, 0);
        s.on_probe(VoiceEngine::Kokoro, false, 1);
        let a = s.on_probe(VoiceEngine::Kokoro, false, 2);
        // Active flips to the standby, and voice keeps flowing there.
        assert_eq!(s.active(), VoiceEngine::Piper);
        assert!(a.contains(&Action::SetActive(VoiceEngine::Piper)));
        assert!(a.contains(&Action::EnsureRunning(VoiceEngine::Piper)));
        // The general is told (error toast), and status is pushed.
        assert!(has_toast(&a, "error").is_some());
        assert!(a.contains(&Action::EmitStatus));
        // The proposal's "do both on the edge": a restart is also kicked.
        assert!(a.contains(&Action::Restart(VoiceEngine::Kokoro)));
    }

    #[test]
    fn fallback_toast_fires_once_not_on_every_down_probe() {
        let mut s = kokoro_primary();
        s.on_probe(VoiceEngine::Kokoro, true, 0);
        s.on_probe(VoiceEngine::Kokoro, false, 1);
        let first = s.on_probe(VoiceEngine::Kokoro, false, 2);
        assert!(has_toast(&first, "error").is_some());
        // Still down on the next probe - no duplicate fallback toast.
        let again = s.on_probe(VoiceEngine::Kokoro, false, 3);
        assert!(has_toast(&again, "error").is_none());
    }

    // --- restart backoff + budget ------------------------------------------

    fn count_restarts(actions: &[Action]) -> usize {
        actions
            .iter()
            .filter(|a| matches!(a, Action::Restart(_)))
            .count()
    }

    #[test]
    fn restarts_follow_backoff_then_stop_at_the_budget() {
        let mut s = kokoro_primary();
        s.on_probe(VoiceEngine::Kokoro, true, 0);
        s.on_probe(VoiceEngine::Kokoro, false, 1);
        // Down edge at t=2: restart #1, backoff_until = 2 + 10 = 12.
        let a = s.on_probe(VoiceEngine::Kokoro, false, 2);
        assert_eq!(count_restarts(&a), 1);
        // Before backoff elapses: no restart.
        assert_eq!(count_restarts(&s.on_tick(5)), 0);
        // At t=12: restart #2, backoff_until = 12 + 20 = 32.
        assert_eq!(count_restarts(&s.on_tick(12)), 1);
        // Budget is 2 restarts - t=32 must NOT restart again (hold on standby).
        assert_eq!(count_restarts(&s.on_tick(32)), 0);
        assert_eq!(count_restarts(&s.on_tick(100)), 0);
        // Still degraded, still carrying on Piper.
        assert!(s.degraded);
        assert_eq!(s.active(), VoiceEngine::Piper);
    }

    #[test]
    fn recovery_resets_the_restart_budget() {
        let mut s = kokoro_primary();
        s.on_probe(VoiceEngine::Kokoro, true, 0);
        s.on_probe(VoiceEngine::Kokoro, false, 1);
        s.on_probe(VoiceEngine::Kokoro, false, 2); // down, restart #1
        s.on_tick(12); // restart #2 (budget now spent)
        assert_eq!(count_restarts(&s.on_tick(40)), 0);
        // Kokoro recovers -> budget resets; a later crash can restart again.
        s.on_probe(VoiceEngine::Kokoro, true, 50);
        s.on_probe(VoiceEngine::Kokoro, false, 60);
        let a = s.on_probe(VoiceEngine::Kokoro, false, 61);
        assert_eq!(count_restarts(&a), 1, "budget should reset after a recovery");
    }

    // --- switch-back hysteresis (D1) ---------------------------------------

    #[test]
    fn switches_back_only_after_the_green_stability_window() {
        let mut s = kokoro_primary();
        s.on_probe(VoiceEngine::Kokoro, true, 0);
        s.on_probe(VoiceEngine::Kokoro, false, 1);
        s.on_probe(VoiceEngine::Kokoro, false, 2); // fallback to Piper
        assert_eq!(s.active(), VoiceEngine::Piper);

        // Kokoro comes back at t=50 (green clock starts).
        s.on_probe(VoiceEngine::Kokoro, true, 50);
        // Before 100ms of green: still on Piper.
        assert_eq!(count_restarts(&s.on_tick(120)), 0);
        assert_eq!(s.active(), VoiceEngine::Piper, "must not switch back early");
        assert!(s.degraded);
        // At t=150 (>= 100ms green): switch back + recovery toast.
        let a = s.on_tick(150);
        assert_eq!(s.active(), VoiceEngine::Kokoro);
        assert!(!s.degraded);
        assert!(a.contains(&Action::SetActive(VoiceEngine::Kokoro)));
        assert!(has_toast(&a, "done").is_some());
    }

    #[test]
    fn a_flapping_primary_never_satisfies_the_stability_window() {
        let mut s = kokoro_primary();
        s.on_probe(VoiceEngine::Kokoro, true, 0);
        s.on_probe(VoiceEngine::Kokoro, false, 1);
        s.on_probe(VoiceEngine::Kokoro, false, 2); // degraded
        // Recovers at 50...
        s.on_probe(VoiceEngine::Kokoro, true, 50);
        // ...but a transient fail at 90 nulls the green clock (still above the
        // down-threshold so health stays Up, but the stability window restarts).
        s.on_probe(VoiceEngine::Kokoro, false, 90);
        s.on_probe(VoiceEngine::Kokoro, true, 95); // green clock restarts at 95
        // t=180 is 130ms after the FIRST recovery but only 85ms after the last -
        // must NOT have switched back.
        s.on_tick(180);
        assert_eq!(s.active(), VoiceEngine::Piper, "flap must not switch back");
        // t=196 (>=100ms of uninterrupted green since 95): now it switches.
        s.on_tick(196);
        assert_eq!(s.active(), VoiceEngine::Kokoro);
    }

    // --- standby keep-alive (F3) -------------------------------------------

    #[test]
    fn standby_is_re_ensured_on_backoff_when_it_dies_while_degraded() {
        let mut s = kokoro_primary();
        s.on_probe(VoiceEngine::Kokoro, true, 0);
        s.on_probe(VoiceEngine::Piper, true, 0);
        // Fall back to Piper.
        s.on_probe(VoiceEngine::Kokoro, false, 1);
        s.on_probe(VoiceEngine::Kokoro, false, 2);
        assert_eq!(s.active(), VoiceEngine::Piper);
        // Now the STANDBY (Piper) also dies while we depend on it.
        s.on_probe(VoiceEngine::Piper, false, 3);
        let down = s.on_probe(VoiceEngine::Piper, false, 4); // Piper now Down
        // It gets an EnsureRunning(standby) (immediately eligible: backoff_until 0).
        assert!(down.contains(&Action::EnsureRunning(VoiceEngine::Piper)));
        // Not a tight loop: a tick before the backoff elapses re-issues nothing.
        assert!(!s.on_tick(5).contains(&Action::EnsureRunning(VoiceEngine::Piper)));
        // After the backoff, it retries again (no give-up budget for the standby).
        assert!(s.on_tick(100).contains(&Action::EnsureRunning(VoiceEngine::Piper)));
    }

    // --- level ladder -------------------------------------------------------

    #[test]
    fn level_ladder_green_amber_red_unknown() {
        let mut s = kokoro_primary();
        assert_eq!(s.level(), RuntimeLevel::Unknown); // nothing probed
        s.on_probe(VoiceEngine::Kokoro, true, 0);
        assert_eq!(s.level(), RuntimeLevel::Green); // primary up, active
        // Fall back with Piper up -> amber.
        s.on_probe(VoiceEngine::Piper, true, 0);
        s.on_probe(VoiceEngine::Kokoro, false, 1);
        s.on_probe(VoiceEngine::Kokoro, false, 2);
        assert_eq!(s.level(), RuntimeLevel::Amber);
        // Piper also dies -> red (both down, voice unavailable).
        s.on_probe(VoiceEngine::Piper, false, 3);
        s.on_probe(VoiceEngine::Piper, false, 4);
        assert_eq!(s.level(), RuntimeLevel::Red);
    }

    // (Voice remap is frontend-side - see store/engine.ts `effectiveVoice` +
    // its vitest; there is no backend synthesis site to remap.)

    // --- startup squatter policy (D2) --------------------------------------

    #[test]
    fn startup_free_port_just_spawns() {
        let p = StartupProbe { served: false, engine: None, is_our_unit: false, marker_matches: false };
        assert_eq!(classify_startup(p, VoiceEngine::Kokoro), StartupAction::Spawn);
    }

    #[test]
    fn startup_our_interim_unit_is_disabled_then_owned() {
        let p = StartupProbe {
            served: true,
            engine: Some(VoiceEngine::Kokoro),
            is_our_unit: true,
            marker_matches: false,
        };
        assert_eq!(
            classify_startup(p, VoiceEngine::Kokoro),
            StartupAction::DisableUnitThenSpawn
        );
    }

    #[test]
    fn startup_reclaims_a_leaked_marked_child_or_bare_same_engine() {
        // Our lifeline marker matches a leaked child.
        let leaked = StartupProbe { served: true, engine: Some(VoiceEngine::Kokoro), is_our_unit: false, marker_matches: true };
        assert_eq!(classify_startup(leaked, VoiceEngine::Kokoro), StartupAction::ReclaimThenSpawn);
        // A bare same-engine server (e.g. a manual nohup) is provably the engine
        // we manage -> safe to reclaim.
        let bare = StartupProbe { served: true, engine: Some(VoiceEngine::Kokoro), is_our_unit: false, marker_matches: false };
        assert_eq!(classify_startup(bare, VoiceEngine::Kokoro), StartupAction::ReclaimThenSpawn);
    }

    #[test]
    fn startup_refuses_to_kill_a_stranger() {
        // A different engine on our port, or an unidentifiable occupant: never
        // kill it - run degraded on the standby and tell the general.
        let other_engine = StartupProbe { served: true, engine: Some(VoiceEngine::Piper), is_our_unit: false, marker_matches: false };
        assert_eq!(classify_startup(other_engine, VoiceEngine::Kokoro), StartupAction::RefuseAndFallback);
        let unknown = StartupProbe { served: true, engine: None, is_our_unit: false, marker_matches: false };
        assert_eq!(classify_startup(unknown, VoiceEngine::Kokoro), StartupAction::RefuseAndFallback);
    }

    /// F1 regression: a REACHABLE-but-unidentified occupant (a foreign HTTP
    /// server / a 4xx that `probe_health_at` reports as reachable but whose
    /// `/health` carries no recognized `engine`) must classify as a STRANGER, so
    /// the runtime neither reclaims nor adopts it. This is the exact input the
    /// old `startup_probe` could never produce (it hardcoded engine=Some) - now
    /// `probe_identity_at` yields None for it and this path is reachable.
    #[test]
    fn f1_reachable_but_unidentified_occupant_is_a_stranger() {
        let foreign_but_reachable = StartupProbe {
            served: true,
            engine: None, // probe_identity_at returns None for a non-TTS/4xx body
            is_our_unit: false,
            marker_matches: false,
        };
        assert_eq!(
            classify_startup(foreign_but_reachable, VoiceEngine::Kokoro),
            StartupAction::RefuseAndFallback,
            "a reachable stranger must NOT be reclaimed or adopted"
        );
    }

    /// F2 helper: the ss-output pid parse the reclaim/marker checks rely on.
    #[test]
    fn parse_ss_pid_extracts_the_owning_pid() {
        let ss = "LISTEN 0 5 127.0.0.1:7478 0.0.0.0:* users:((\"python\",pid=3564749,fd=4))";
        assert_eq!(super::runtime::parse_ss_pid(ss), Some(3564749));
        assert_eq!(super::runtime::parse_ss_pid("LISTEN 0 5 127.0.0.1:7478"), None);
        assert_eq!(super::runtime::parse_ss_pid(""), None);
    }

    // --- selected=piper generalization -------------------------------------

    #[test]
    fn works_symmetrically_when_piper_is_the_selected_primary() {
        let mut s = Supervisor::new(VoiceEngine::Piper, test_cfg());
        s.on_probe(VoiceEngine::Piper, true, 0);
        assert_eq!(s.active(), VoiceEngine::Piper);
        s.on_probe(VoiceEngine::Piper, false, 1);
        let a = s.on_probe(VoiceEngine::Piper, false, 2);
        // Falls back to Kokoro as the standby.
        assert_eq!(s.active(), VoiceEngine::Kokoro);
        assert!(a.contains(&Action::SetActive(VoiceEngine::Kokoro)));
    }

    // --- snapshot serialization --------------------------------------------

    #[test]
    fn snapshot_serializes_camel_case_for_the_webview() {
        let mut s = kokoro_primary();
        s.on_probe(VoiceEngine::Kokoro, true, 0);
        let v = serde_json::to_value(s.snapshot()).unwrap();
        assert_eq!(v.get("managed").and_then(|x| x.as_bool()), Some(true));
        assert_eq!(v.get("selectedEngine").and_then(|x| x.as_str()), Some("kokoro"));
        assert_eq!(v.get("activeEngine").and_then(|x| x.as_str()), Some("kokoro"));
        assert_eq!(v.get("level").and_then(|x| x.as_str()), Some("green"));
        assert_eq!(v.get("kokoro").and_then(|x| x.as_str()), Some("up"));
    }

    #[test]
    fn unmanaged_snapshot_reports_managed_false() {
        let v = serde_json::to_value(SupervisorSnapshot::unmanaged()).unwrap();
        assert_eq!(v.get("managed").and_then(|x| x.as_bool()), Some(false));
        assert_eq!(v.get("level").and_then(|x| x.as_str()), Some("unknown"));
    }

    // --- lifeline script (the no-orphan primary guarantee) -----------------

    #[test]
    fn lifeline_script_has_the_no_orphan_essentials() {
        let script = platform::lifeline_script("/home/x/kokoro-tts");
        assert!(script.contains("/home/x/kokoro-tts"), "cd's into the repo");
        assert!(script.contains("setsid ./start.sh"), "own process group for a clean group-kill");
        assert!(script.contains("cat"), "blocks on stdin to detect parent death via EOF");
        assert!(script.contains("kill -TERM -\"$SRV\""), "group-kills on death");
        assert!(script.contains("trap"), "also kills on a delivered TERM/HUP");
        assert!(script.contains(platform::KOKORO_PID_MARKER), "writes the reaper pid marker");
        // F2: the marker is UNLINKED on clean exit so a stale marker can't later
        // drive a kill / a false marker_matches.
        assert!(script.contains("rm -f"), "cleans up the pid marker on exit");
    }

    #[test]
    fn managed_flag_defaults_off() {
        // Whatever the ambient env, the parser only accepts explicit on-tokens.
        assert!(!super::parse_managed_flag(None));
        assert!(!super::parse_managed_flag(Some("")));
        assert!(!super::parse_managed_flag(Some("0")));
        assert!(!super::parse_managed_flag(Some("off")));
        assert!(super::parse_managed_flag(Some("1")));
        assert!(super::parse_managed_flag(Some("true")));
        assert!(super::parse_managed_flag(Some("on")));
    }
}
