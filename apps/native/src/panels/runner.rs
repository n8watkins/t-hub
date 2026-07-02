//! Dev-runner panel state (T11): start / stop / tail a dev server per project.
//!
//! ## Why tmux-composed (the §3 "check what the socket offers" answer)
//!
//! The webview's dev runner is a hidden Tauri child process
//! (`devserver.rs::start_dev_server`/`stop_dev_server`) streaming
//! `devserver://<id>` Tauri events. NONE of that is reachable over the control
//! socket - those are `#[tauri::command]`s only, and the devserver channel does
//! not ride the control `EventFanout`. Per the brief, exposing them is a
//! documented FOLLOW-UP, not something T11 adds server-side.
//!
//! Instead the native runner composes commands the socket ALREADY audits:
//!
//!  - `spawn_terminal {cwd,name}` - creates a real, UI-adopted session at the
//!    project root (requires the app UI to be running; the response carries no
//!    session id, so the new session is identified by diffing `list_terminals`
//!    against a pre-spawn baseline and matching the pane cwd).
//!  - `send_text` - an adopt-marker echo proves the bound session is OUR fresh
//!    shell before anything else is ever typed into it; then the dev command
//!    runs wrapped with a nonce'd exit marker (`cmd; echo "EXIT:<code>"`) so
//!    the machine observes termination + exit code from capture text.
//!  - `read_terminal` - the tail: polling tmux capture text (already
//!    ANSI-stripped by `capture-pane -p`). No PTY attach, so tailing never
//!    resizes the session under the user's own tiles.
//!  - `send_keys ["C-c"]` to stop, `close_terminal` to kill the session.
//!
//! This makes the dev server a first-class session: visible in every client,
//! surviving native-client restarts. The machine only ever sends input to a
//! session it created (or that a probe explicitly bound); never to arbitrary
//! sessions.
//!
//! gpui-free and I/O-free: reducers emit [`RunnerCmd`]s that the feed (or the
//! probe) executes over the socket, so the whole machine unit-tests offline.

use std::collections::{HashMap, HashSet};

use super::files::DirEntry;
use super::preview::scan_local_urls;
use super::LiveSession;

/// How long a `spawn_terminal` may take to show up in `list_terminals`.
pub const SPAWN_TIMEOUT_MS: u64 = 15_000;
/// How long the adopt-marker echo may take to appear in capture text.
pub const ADOPT_TIMEOUT_MS: u64 = 10_000;
/// After C-c, wait this long before typing the stop-probe echo (an
/// interactive shell aborts the rest of the `cmd; echo EXIT` list on SIGINT,
/// so the exit marker never prints on a stop; a follow-up echo only executes
/// once the shell is back at its prompt, which is exactly the signal).
pub const STOP_PROBE_DELAY_MS: u64 = 1_500;
/// How long after C-c before we conclude the process ignored it.
pub const STOP_TIMEOUT_MS: u64 = 8_000;
/// Tail lines kept for the view.
pub const TAIL_KEEP: usize = 200;

/// The dev-runner state machine phases for one project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    /// No session bound.
    Idle,
    /// `spawn_terminal` issued; waiting for a new session (vs `baseline`) whose
    /// pane cwd matches the project root.
    Spawning { since_ms: u64, baseline: HashSet<String> },
    /// Session identified; adopt-marker echo sent, waiting to see it.
    Adopting { sid: String, since_ms: u64 },
    /// Bound to our shell; no dev command running.
    Ready { sid: String },
    /// Dev command sent (wrapped with the exit marker).
    Running { sid: String },
    /// C-c sent; waiting for the exit or stop-probe marker (`probed` = the
    /// follow-up echo has been typed).
    Stopping { sid: String, since_ms: u64, probed: bool },
    /// Exit marker observed. The session is still alive (back at the shell).
    Exited { sid: String, code: Option<i32> },
    /// Something went wrong; `reason` for the view. Start clears it.
    Failed { reason: String },
}

/// Socket work the reducers request; the feed executes these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunnerCmd {
    /// `spawn_terminal { cwd, name }`.
    Spawn { cwd: String, name: String },
    /// `send_text { sessionId, text, enter: true }`.
    SendText { sid: String, text: String },
    /// `send_keys { sessionId, keys }`.
    SendKeys { sid: String, keys: Vec<String> },
    /// `close_terminal { sessionId }`.
    Kill { sid: String },
}

/// One project's dev runner.
#[derive(Debug)]
pub struct RunnerState {
    pub root: String,
    pub phase: Phase,
    /// The command line to run (editable; defaults detected per webview rules).
    pub command: String,
    command_edited: bool,
    /// Default-command detection (`pnpm-lock.yaml` -> `pnpm dev`) lifecycle.
    pub default_requested: bool,
    pub default_resolved: bool,
    /// Marker nonce; bumped per adopt and per run so stale markers in
    /// scrollback (earlier runs, earlier client instances) never match.
    nonce: u64,
    /// Local dev URL detected in the current run's output.
    pub url: Option<String>,
    /// Last capture tail for the view (replaced per poll, not appended).
    pub tail: Vec<String>,
    /// Non-fatal annotation for the view (e.g. "C-c did not end it").
    pub note: Option<String>,
}

impl RunnerState {
    /// `nonce_seed` keys the run markers; pass something instance-unique
    /// (the feed passes wall-clock ms) so a rebound session's old markers
    /// cannot collide.
    pub fn new(root: &str, nonce_seed: u64) -> Self {
        RunnerState {
            root: root.to_string(),
            phase: Phase::Idle,
            command: "npm run dev".to_string(),
            command_edited: false,
            default_requested: false,
            default_resolved: false,
            nonce: nonce_seed,
            url: None,
            tail: Vec::new(),
            note: None,
        }
    }

    /// The bound session id, in any phase that has one.
    pub fn sid(&self) -> Option<&str> {
        match &self.phase {
            Phase::Adopting { sid, .. }
            | Phase::Ready { sid }
            | Phase::Running { sid }
            | Phase::Stopping { sid, .. }
            | Phase::Exited { sid, .. } => Some(sid),
            _ => None,
        }
    }

    /// Whether the feed should be polling capture text for this runner.
    pub fn wants_tail(&self) -> bool {
        matches!(
            self.phase,
            Phase::Adopting { .. } | Phase::Running { .. } | Phase::Stopping { .. }
        )
    }

    pub fn set_command(&mut self, cmd: &str) {
        self.command = cmd.to_string();
        self.command_edited = true;
    }

    /// Fold the project root's `list_dir` for default-command detection
    /// (webview parity: `pnpm dev` when a pnpm-lock.yaml exists, else
    /// `npm run dev`). Never clobbers a user-edited command.
    pub fn fold_default_dir(&mut self, entries: &[DirEntry]) {
        self.default_resolved = true;
        if self.command_edited {
            return;
        }
        if entries.iter().any(|e| e.name == "pnpm-lock.yaml") {
            self.command = "pnpm dev".to_string();
        }
    }

    // -- actions -------------------------------------------------------------

    /// Start: from Idle/Failed spawns a session; from Ready/Exited runs the
    /// command in the bound session.
    pub fn start(&mut self, now_ms: u64, live: &[LiveSession]) -> Vec<RunnerCmd> {
        self.note = None;
        match &self.phase {
            Phase::Idle | Phase::Failed { .. } => {
                let baseline: HashSet<String> = live.iter().map(|s| s.id.clone()).collect();
                self.phase = Phase::Spawning { since_ms: now_ms, baseline };
                let name = format!("dev: {}", basename(&self.root));
                vec![RunnerCmd::Spawn { cwd: self.root.clone(), name }]
            }
            Phase::Ready { sid } | Phase::Exited { sid, .. } => {
                let sid = sid.clone();
                self.nonce += 1;
                self.url = None;
                let text = wrapped_command(&self.command, self.nonce);
                self.phase = Phase::Running { sid: sid.clone() };
                vec![RunnerCmd::SendText { sid, text }]
            }
            _ => Vec::new(),
        }
    }

    /// Stop the running command with C-c (the session stays).
    pub fn stop(&mut self, now_ms: u64) -> Vec<RunnerCmd> {
        if let Phase::Running { sid } = &self.phase {
            let sid = sid.clone();
            self.phase = Phase::Stopping { sid: sid.clone(), since_ms: now_ms, probed: false };
            vec![RunnerCmd::SendKeys { sid, keys: vec!["C-c".to_string()] }]
        } else {
            Vec::new()
        }
    }

    /// Kill the bound session entirely (close_terminal) and unbind.
    pub fn kill(&mut self) -> Vec<RunnerCmd> {
        let out = match self.sid() {
            Some(sid) => vec![RunnerCmd::Kill { sid: sid.to_string() }],
            None => Vec::new(),
        };
        self.phase = Phase::Idle;
        self.url = None;
        self.tail.clear();
        out
    }

    /// Bind an EXISTING session (skipping the spawn) - the probe uses this on
    /// its own disposable session. The adopt-marker handshake still runs, so
    /// nothing beyond one `echo` is ever sent until the session proves to be a
    /// responsive shell. Only bind sessions you created.
    pub fn bind_existing(&mut self, sid: &str, now_ms: u64) -> Vec<RunnerCmd> {
        self.nonce += 1;
        self.phase = Phase::Adopting { sid: sid.to_string(), since_ms: now_ms };
        vec![RunnerCmd::SendText { sid: sid.to_string(), text: adopt_command(self.nonce) }]
    }

    // -- observations ----------------------------------------------------------

    /// Fold a `list_terminals` sweep: identifies the spawned session, and
    /// fails the machine when the bound session disappears.
    pub fn on_sessions(&mut self, live: &[LiveSession], now_ms: u64) -> Vec<RunnerCmd> {
        match &self.phase {
            Phase::Spawning { baseline, .. } => {
                let root = normalize_dir(&self.root);
                let candidate = live
                    .iter()
                    .find(|s| !baseline.contains(&s.id) && normalize_dir(&s.cwd) == root);
                if let Some(s) = candidate {
                    let sid = s.id.clone();
                    self.nonce += 1;
                    self.phase = Phase::Adopting { sid: sid.clone(), since_ms: now_ms };
                    return vec![RunnerCmd::SendText { sid, text: adopt_command(self.nonce) }];
                }
                Vec::new()
            }
            _ => {
                if let Some(sid) = self.sid() {
                    if !live.iter().any(|s| s.id == sid) {
                        self.phase = Phase::Failed { reason: "session gone".to_string() };
                        self.url = None;
                    }
                }
                Vec::new()
            }
        }
    }

    /// Fold a capture-text poll for the bound session.
    pub fn on_capture(&mut self, sid: &str, text: &str, _now_ms: u64) {
        if self.sid() != Some(sid) {
            return;
        }
        self.tail = text.lines().map(|l| l.trim_end().to_string()).collect();
        if self.tail.len() > TAIL_KEEP {
            self.tail.drain(..self.tail.len() - TAIL_KEEP);
        }
        match &self.phase {
            Phase::Adopting { sid, .. } => {
                let marker = adopt_marker(self.nonce);
                if text.lines().any(|l| l.trim() == marker) {
                    self.phase = Phase::Ready { sid: sid.clone() };
                }
            }
            Phase::Running { sid } | Phase::Stopping { sid, .. } => {
                let sid = sid.clone();
                if let Some(code) = find_exit_code(text, self.nonce) {
                    self.phase = Phase::Exited { sid, code };
                    return;
                }
                // The stop-probe echo only runs once the shell is back at its
                // prompt; seeing it means the command is gone (exit code
                // unobservable - SIGINT aborted the `; echo EXIT` list).
                if matches!(self.phase, Phase::Stopping { .. }) {
                    let stopped = stop_marker(self.nonce);
                    if text.lines().any(|l| l.trim() == stopped) {
                        self.phase = Phase::Exited { sid, code: None };
                        return;
                    }
                }
                // Newest local URL in the capture wins. (Capture text may still
                // show a previous run's URL further up; in practice a project's
                // dev URL is stable and the newest occurrence is the live one.)
                if let Some(u) = scan_local_urls(text).last() {
                    self.url = Some(u.open_target());
                }
            }
            _ => {}
        }
    }

    /// Advance timeouts (and emit the delayed stop-probe echo). Call once per
    /// feed tick; execute any returned commands.
    pub fn on_tick(&mut self, now_ms: u64) -> Vec<RunnerCmd> {
        match &mut self.phase {
            Phase::Spawning { since_ms, .. }
                if now_ms.saturating_sub(*since_ms) > SPAWN_TIMEOUT_MS =>
            {
                self.phase = Phase::Failed {
                    reason: "spawn not adopted in time - is the T-Hub app UI running?".to_string(),
                };
            }
            Phase::Adopting { since_ms, .. }
                if now_ms.saturating_sub(*since_ms) > ADOPT_TIMEOUT_MS =>
            {
                self.phase = Phase::Failed {
                    reason: "bound session never echoed the adopt marker".to_string(),
                };
            }
            Phase::Stopping { sid, since_ms, probed } => {
                let elapsed = now_ms.saturating_sub(*since_ms);
                if elapsed > STOP_TIMEOUT_MS {
                    let sid = sid.clone();
                    self.note = Some("C-c did not end it; still running".to_string());
                    self.phase = Phase::Running { sid };
                } else if !*probed && elapsed >= STOP_PROBE_DELAY_MS {
                    *probed = true;
                    return vec![RunnerCmd::SendText {
                        sid: sid.clone(),
                        text: stop_probe_command(self.nonce),
                    }];
                }
            }
            _ => {}
        }
        Vec::new()
    }

    /// Force the machine into Failed (e.g. the feed's `spawn_terminal` request
    /// was rejected outright - no UI sink connected to adopt the tile).
    pub fn fail(&mut self, reason: String) {
        self.phase = Phase::Failed { reason };
        self.url = None;
    }

    /// A short status label for the view.
    pub fn status_label(&self) -> String {
        match &self.phase {
            Phase::Idle => "idle".to_string(),
            Phase::Spawning { .. } => "spawning session...".to_string(),
            Phase::Adopting { .. } => "binding shell...".to_string(),
            Phase::Ready { .. } => "ready".to_string(),
            Phase::Running { .. } => "running".to_string(),
            Phase::Stopping { .. } => "stopping...".to_string(),
            Phase::Exited { code: Some(c), .. } => format!("exited ({c})"),
            Phase::Exited { code: None, .. } => "exited".to_string(),
            Phase::Failed { reason } => format!("failed: {reason}"),
        }
    }
}

/// All runners, one per project root.
#[derive(Debug, Default)]
pub struct RunnersState {
    runners: HashMap<String, RunnerState>,
}

impl RunnersState {
    /// Get-or-create the runner for `root` (`now_ms` seeds its marker nonce).
    pub fn ensure(&mut self, root: &str, now_ms: u64) -> &mut RunnerState {
        self.runners
            .entry(root.to_string())
            .or_insert_with(|| RunnerState::new(root, now_ms))
    }

    pub fn get(&self, root: &str) -> Option<&RunnerState> {
        self.runners.get(root)
    }

    pub fn get_mut(&mut self, root: &str) -> Option<&mut RunnerState> {
        self.runners.get_mut(root)
    }

    /// True while any runner awaits spawn identification (the feed tightens
    /// its `list_terminals` cadence during this window).
    pub fn any_spawning(&self) -> bool {
        self.runners.values().any(|r| matches!(r.phase, Phase::Spawning { .. }))
    }

    /// `(root, sid)` pairs the feed should capture-poll this tick.
    pub fn tail_targets(&self) -> Vec<(String, String)> {
        self.runners
            .values()
            .filter(|r| r.wants_tail())
            .filter_map(|r| r.sid().map(|sid| (r.root.clone(), sid.to_string())))
            .collect()
    }

    /// Roots whose default command still needs a `list_dir` fetch; marks them
    /// requested.
    pub fn take_default_fetches(&mut self) -> Vec<String> {
        self.runners
            .values_mut()
            .filter(|r| !r.default_requested && !r.default_resolved)
            .map(|r| {
                r.default_requested = true;
                r.root.clone()
            })
            .collect()
    }

    /// Fold a `list_terminals` sweep into every runner.
    pub fn on_sessions(&mut self, live: &[LiveSession], now_ms: u64) -> Vec<RunnerCmd> {
        let mut out = Vec::new();
        for r in self.runners.values_mut() {
            out.extend(r.on_sessions(live, now_ms));
        }
        out
    }

    /// Advance every runner's timeouts; execute any returned commands.
    pub fn on_tick(&mut self, now_ms: u64) -> Vec<RunnerCmd> {
        let mut out = Vec::new();
        for r in self.runners.values_mut() {
            out.extend(r.on_tick(now_ms));
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Markers
// ---------------------------------------------------------------------------
//
// Both markers ride ordinary `echo`s, with the marker string QUOTE-SPLIT in
// the typed command (`"__TH_PANEL_""ADOPT_1__"`), so the terminal's echo of
// the typing itself never contains the contiguous marker - only the shell's
// OUTPUT does. That makes "the marker appears in capture text" a reliable
// signal even though capture shows the typed line too.

/// The adopt-echo output line for `nonce`.
pub fn adopt_marker(nonce: u64) -> String {
    format!("__TH_PANEL_ADOPT_{nonce}__")
}

/// The command typed to prove the bound session is our shell.
pub fn adopt_command(nonce: u64) -> String {
    format!("echo \"__TH_PANEL_\"\"ADOPT_{nonce}__\"")
}

fn exit_prefix(nonce: u64) -> String {
    format!("__TH_PANEL_EXIT_{nonce}__:")
}

/// The stop-probe output line for `nonce`.
pub fn stop_marker(nonce: u64) -> String {
    format!("__TH_PANEL_STOP_{nonce}__")
}

/// The follow-up typed after C-c: it only executes (and its marker only
/// appears) once the shell is back at its prompt. If the dev process ignored
/// the C-c, this line lands in ITS stdin instead - one line of noise, which
/// is why it is typed once, after [`STOP_PROBE_DELAY_MS`].
pub fn stop_probe_command(nonce: u64) -> String {
    format!("echo \"__TH_PANEL_\"\"STOP_{nonce}__\"")
}

/// Wrap the user's dev command so its termination prints `EXIT_<nonce>__:<$?>`.
pub fn wrapped_command(cmd: &str, nonce: u64) -> String {
    format!("{cmd}; echo \"__TH_PANEL_\"\"EXIT_{nonce}__:$?\"")
}

/// Find the exit marker for `nonce` in capture text. `Some(code)` when seen;
/// the inner Option is None if the code failed to parse.
pub fn find_exit_code(text: &str, nonce: u64) -> Option<Option<i32>> {
    let prefix = exit_prefix(nonce);
    for line in text.lines().rev() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix(&prefix) {
            return Some(rest.trim().parse::<i32>().ok());
        }
    }
    None
}

/// Trailing-slash-insensitive dir compare key.
fn normalize_dir(p: &str) -> &str {
    let p = p.trim_end_matches('/');
    if p.is_empty() {
        "/"
    } else {
        p
    }
}

fn basename(p: &str) -> &str {
    normalize_dir(p).rsplit('/').next().unwrap_or(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn live(entries: &[(&str, &str)]) -> Vec<LiveSession> {
        entries
            .iter()
            .map(|(id, cwd)| LiveSession {
                id: id.to_string(),
                title: format!("th_{id}"),
                cwd: cwd.to_string(),
            })
            .collect()
    }

    /// Simulate the capture echo of typing `cmd` (what tmux shows for the
    /// typed line) followed by output lines.
    fn capture(typed: &str, output: &[&str]) -> String {
        let mut s = format!("$ {typed}\n");
        for l in output {
            s.push_str(l);
            s.push('\n');
        }
        s
    }

    #[test]
    fn full_happy_path_spawn_adopt_run_url_stop_exit() {
        let mut r = RunnerState::new("/proj/app", 100);
        let before = live(&[("aaa", "/other")]);

        // Start from Idle: spawns.
        let cmds = r.start(1_000, &before);
        assert_eq!(cmds.len(), 1);
        assert!(matches!(&cmds[0], RunnerCmd::Spawn { cwd, name }
            if cwd == "/proj/app" && name == "dev: app"));
        assert!(matches!(r.phase, Phase::Spawning { .. }));

        // A new session at the right cwd appears: adopt echo goes out.
        let after = live(&[("aaa", "/other"), ("bbb", "/proj/app/")]);
        let cmds = r.on_sessions(&after, 2_000);
        assert_eq!(cmds.len(), 1);
        let RunnerCmd::SendText { sid, text } = &cmds[0] else { panic!("expected SendText") };
        assert_eq!(sid, "bbb");
        assert_eq!(text, &adopt_command(101));
        assert!(matches!(r.phase, Phase::Adopting { .. }));

        // The echoed TYPING alone must not adopt (quote-split guard)...
        r.on_capture("bbb", &capture(&adopt_command(101), &[]), 2_500);
        assert!(matches!(r.phase, Phase::Adopting { .. }), "typed echo alone must not adopt");
        // ...the OUTPUT line does.
        r.on_capture("bbb", &capture(&adopt_command(101), &[&adopt_marker(101)]), 3_000);
        assert!(matches!(r.phase, Phase::Ready { .. }));

        // Start again: runs the wrapped command in the bound session.
        r.set_command("npm run dev");
        let cmds = r.start(4_000, &after);
        assert_eq!(cmds.len(), 1);
        let RunnerCmd::SendText { sid, text } = &cmds[0] else { panic!("expected SendText") };
        assert_eq!(sid, "bbb");
        assert_eq!(text, &wrapped_command("npm run dev", 102));
        assert!(matches!(r.phase, Phase::Running { .. }));

        // Output with a dev URL: detected (0.0.0.0 opens as localhost).
        let out = capture(text, &["> vite", "Local: http://0.0.0.0:5173/"]);
        r.on_capture("bbb", &out, 5_000);
        assert!(matches!(r.phase, Phase::Running { .. }));
        assert_eq!(r.url.as_deref(), Some("http://localhost:5173/"));
        assert!(r.tail.iter().any(|l| l.contains("vite")), "tail folded");

        // Stop: C-c, then the delayed stop-probe echo goes out, and its
        // marker at the prompt proves the process ended.
        let cmds = r.stop(6_000);
        assert!(matches!(&cmds[0], RunnerCmd::SendKeys { sid, keys }
            if sid == "bbb" && keys == &vec!["C-c".to_string()]));
        assert!(r.on_tick(6_000 + STOP_PROBE_DELAY_MS - 1).is_empty(), "probe waits");
        let cmds = r.on_tick(6_000 + STOP_PROBE_DELAY_MS);
        assert!(matches!(&cmds[0], RunnerCmd::SendText { sid, text }
            if sid == "bbb" && text == &stop_probe_command(102)));
        assert!(r.on_tick(6_000 + STOP_PROBE_DELAY_MS + 10).is_empty(), "probed once");
        let out = capture(text, &["^C", &stop_probe_command(102), &stop_marker(102)]);
        r.on_capture("bbb", &out, 8_000);
        assert!(matches!(r.phase, Phase::Exited { code: None, .. }));

        // Kill: close_terminal + unbound.
        let cmds = r.kill();
        assert!(matches!(&cmds[0], RunnerCmd::Kill { sid } if sid == "bbb"));
        assert!(matches!(r.phase, Phase::Idle));
    }

    #[test]
    fn exit_marker_requires_matching_nonce_and_ignores_typed_echo() {
        let cmd = wrapped_command("sleep 5", 7);
        // The typed line's echo (quote-split) must not read as an exit.
        assert_eq!(find_exit_code(&capture(&cmd, &[]), 7), None);
        // A STALE marker (older nonce) must not match either.
        assert_eq!(find_exit_code("__TH_PANEL_EXIT_6__:0\n", 7), None);
        // The real output does, and the last occurrence wins.
        let text = "__TH_PANEL_EXIT_7__:1\nnoise\n__TH_PANEL_EXIT_7__:0\n";
        assert_eq!(find_exit_code(text, 7), Some(Some(0)));
    }

    #[test]
    fn spawning_ignores_wrong_cwd_and_baseline_sessions() {
        let mut r = RunnerState::new("/proj", 0);
        r.start(0, &live(&[("old", "/proj")]));
        // Baseline session at the right cwd: NOT ours.
        assert!(r.on_sessions(&live(&[("old", "/proj")]), 100).is_empty());
        // New session at the wrong cwd: not ours either.
        assert!(r.on_sessions(&live(&[("old", "/proj"), ("new1", "/elsewhere")]), 200).is_empty());
        assert!(matches!(r.phase, Phase::Spawning { .. }));
        // New session at the right cwd: adopted.
        let cmds = r.on_sessions(&live(&[("old", "/proj"), ("new2", "/proj")]), 300);
        assert!(matches!(&cmds[0], RunnerCmd::SendText { sid, .. } if sid == "new2"));
    }

    #[test]
    fn timeouts_spawn_adopt_stop() {
        let mut r = RunnerState::new("/p", 0);
        r.start(0, &[]);
        r.on_tick(SPAWN_TIMEOUT_MS + 1);
        assert!(matches!(&r.phase, Phase::Failed { reason } if reason.contains("spawn")));

        // Adopt timeout.
        let mut r = RunnerState::new("/p", 0);
        r.bind_existing("sss", 0);
        r.on_tick(ADOPT_TIMEOUT_MS + 1);
        assert!(matches!(&r.phase, Phase::Failed { reason } if reason.contains("adopt")));

        // Stop timeout falls back to Running with a note (the stop-probe
        // fires along the way and its marker never shows).
        let mut r = RunnerState::new("/p", 0);
        r.bind_existing("sss", 0);
        r.on_capture("sss", &adopt_marker(1), 10);
        r.start(20, &[]);
        r.stop(30);
        let probe = r.on_tick(30 + STOP_PROBE_DELAY_MS);
        assert_eq!(probe.len(), 1, "stop-probe echo goes out");
        r.on_tick(30 + STOP_TIMEOUT_MS + 1);
        assert!(matches!(r.phase, Phase::Running { .. }));
        assert!(r.note.as_deref().unwrap().contains("C-c"));
    }

    #[test]
    fn exit_marker_during_stopping_still_reports_the_code() {
        // Some paths (non-interactive shells, the command exiting right as
        // C-c lands) still print the EXIT marker; the code wins over the
        // codeless stop marker.
        let mut r = RunnerState::new("/p", 0);
        r.bind_existing("sss", 0);
        r.on_capture("sss", &adopt_marker(1), 10);
        r.start(20, &[]);
        r.stop(30);
        r.on_capture("sss", "__TH_PANEL_EXIT_2__:130\n", 40);
        assert!(matches!(r.phase, Phase::Exited { code: Some(130), .. }));
    }

    #[test]
    fn bound_session_vanishing_fails_the_machine() {
        let mut r = RunnerState::new("/p", 0);
        r.bind_existing("sss", 0);
        r.on_capture("sss", &adopt_marker(1), 10);
        r.start(20, &[]);
        r.on_sessions(&live(&[("other", "/x")]), 30);
        assert!(matches!(&r.phase, Phase::Failed { reason } if reason.contains("gone")));
        assert!(r.url.is_none());
    }

    #[test]
    fn capture_for_a_different_session_is_ignored() {
        let mut r = RunnerState::new("/p", 0);
        r.bind_existing("sss", 0);
        r.on_capture("zzz", &adopt_marker(1), 10);
        assert!(matches!(r.phase, Phase::Adopting { .. }));
    }

    #[test]
    fn default_command_detection_pnpm_and_edit_guard() {
        let pnpm = vec![DirEntry {
            name: "pnpm-lock.yaml".into(),
            path: "/p/pnpm-lock.yaml".into(),
            is_dir: false,
            size: 1,
        }];
        let mut r = RunnerState::new("/p", 0);
        r.fold_default_dir(&pnpm);
        assert_eq!(r.command, "pnpm dev");

        let mut r = RunnerState::new("/p", 0);
        r.fold_default_dir(&[]);
        assert_eq!(r.command, "npm run dev");

        // A user edit is never clobbered by late detection.
        let mut r = RunnerState::new("/p", 0);
        r.set_command("cargo run");
        r.fold_default_dir(&pnpm);
        assert_eq!(r.command, "cargo run");
    }

    #[test]
    fn runners_state_tail_targets_and_default_fetches() {
        let mut rs = RunnersState::default();
        rs.ensure("/a", 0);
        rs.ensure("/b", 0);
        assert_eq!(rs.take_default_fetches().len(), 2);
        assert!(rs.take_default_fetches().is_empty(), "requested once");

        rs.get_mut("/a").unwrap().bind_existing("sa", 0);
        assert_eq!(rs.tail_targets(), vec![("/a".to_string(), "sa".to_string())]);
        // Ready needs no tail.
        rs.get_mut("/a").unwrap().on_capture("sa", &adopt_marker(1), 10);
        assert!(rs.tail_targets().is_empty());
    }
}
