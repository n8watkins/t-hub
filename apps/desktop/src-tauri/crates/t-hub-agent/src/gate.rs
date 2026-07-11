//! item-3 Pillar C: the `PreToolUse` outward-facing command gate.
//!
//! The control-socket tier machinery governs only the control socket; a crew's raw
//! `git push` / `gh pr merge` / `gh workflow run release.yml` / spend CLI runs through
//! its own Bash tool and never reaches the server. This module is the SECOND
//! enforcement substrate: a blocking Claude Code `PreToolUse` hook (matcher `Bash`)
//! installed by the app's hook installer. It reads the hook JSON on stdin, resolves
//! the caller's capability class from the running app, classifies the command, and
//! BLOCKS (deny) an outward-facing action a crew may not take - or a significant
//! deploy / spend that lacks a verified general authorization.
//!
//! Honest limits (design §2.3, graded): this catches a command issued through the
//! Bash tool (the normal path). A crew that evades the Bash tool is caught only by
//! credential-withholding (`GH_CONFIG_DIR`, server-side) - the wall is "hook OR
//! missing credential", two independent layers (N3). The gate FAILS CLOSED: any
//! error, an unresolved caller class, or an unavailable confirm signal is treated as
//! the least-privilege / no-authorization case and denies the outward-facing patterns.
//!
//! The pure classification core ([`classify`], [`deploy_significance`], [`decide`]) is
//! fully unit-tested; [`run`] glues it to stdin, the app (capability resolve), and the
//! Claude Code `PreToolUse` decision output.

use std::collections::BTreeSet;
use std::io::Read;

/// The caller's resolved capability class (from the app's `my_capability`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallerClass {
    /// A crew / read-token session: may NOT take any outward-facing action.
    Read,
    /// A control-token session (captain / orchestrator): may push/merge; a significant
    /// deploy or a spend/publish still requires a verified general authorization.
    Control,
}

/// The outcome-pattern class of a Bash command (item-3 §2.3.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandClass {
    /// Not outward-facing: allowed for any caller.
    Benign,
    /// A push to a protected branch, or any force-push.
    ProtectedPush,
    /// A merge of a pull request or into a protected branch.
    Merge,
    /// A deploy / release run, carrying its target (e.g. a workflow file name).
    Deploy { target: String },
    /// Money-spend or a public publish (npm/cargo publish, a paid API/deploy CLI).
    SpendOrPublish,
}

/// Whether a deploy TARGET is routine-reversible or significant (production OR
/// user-facing OR irreversible), per the ratified rule (general-decision #3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Significance {
    /// Known-routine + reversible: captain-owned, never waits on the general.
    Routine,
    /// production OR user-facing OR irreversible (or unknown => fail-closed).
    Significant,
}

/// The gate's decision on one command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Let Claude's normal permission flow proceed (the gate does not auto-approve).
    Allow,
    /// Block the command with a reason routed to the sanctioned path.
    Deny(String),
}

/// The protected branches a push/merge is gated against. Kept small and conventional;
/// the credential-withholding layer is the real wall for anything missed here.
const PROTECTED_BRANCHES: &[&str] = &["main", "master", "release", "production", "prod"];

/// The per-target significance registry (declared, overridable in-repo). Unknown
/// targets FAIL CLOSED to significant; only explicitly-listed targets are routine, and
/// a routine caller may not self-declare a registry-significant target as routine
/// (declare-UP only, enforced in [`decide`] by ignoring any caller "routine" claim).
#[derive(Debug, Clone, Default)]
pub struct SignificanceRegistry {
    /// Deploy targets known to be routine-reversible (e.g. a named preview workflow).
    routine: BTreeSet<String>,
}

impl SignificanceRegistry {
    /// The built-in default: nothing is routine, so every deploy target is significant
    /// until explicitly declared routine in-repo. `release.yml` (the real production
    /// release path) is deliberately NOT routine.
    pub fn builtin() -> Self {
        SignificanceRegistry {
            routine: BTreeSet::new(),
        }
    }

    /// Load the in-repo override (`<cwd>/.t-hub/deploy-significance.json`, shape
    /// `{"routine": ["preview.yml", ...]}`) merged onto the built-in default. A missing
    /// or malformed file leaves the built-in (fail-closed) registry - never a failure.
    pub fn load_for_cwd(cwd: &str) -> Self {
        let mut reg = Self::builtin();
        let path = std::path::Path::new(cwd)
            .join(".t-hub")
            .join("deploy-significance.json");
        if let Ok(body) = std::fs::read_to_string(&path) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
                if let Some(arr) = v.get("routine").and_then(|r| r.as_array()) {
                    for t in arr.iter().filter_map(|t| t.as_str()) {
                        reg.routine.insert(t.to_string());
                    }
                }
            }
        }
        reg
    }

    /// The significance of a deploy target: routine iff explicitly listed, else
    /// significant (fail-closed-to-significant for any unknown/production target).
    pub fn significance(&self, target: &str) -> Significance {
        if self.routine.contains(target) {
            Significance::Routine
        } else {
            Significance::Significant
        }
    }
}

/// Tokenize a shell-ish command line loosely (whitespace split, strip simple quotes).
/// Good enough for pattern classification; the credential layer backstops evasion.
fn tokens(command: &str) -> Vec<String> {
    command
        .split_whitespace()
        .map(|t| t.trim_matches(|c| c == '"' || c == '\'').to_string())
        .collect()
}

/// Whether any token names a protected branch (exact, or a `refs/heads/<b>` form).
fn mentions_protected_branch(toks: &[String]) -> bool {
    toks.iter().any(|t| {
        let bare = t.rsplit('/').next().unwrap_or(t);
        PROTECTED_BRANCHES.contains(&bare)
    })
}

/// Classify a Bash command's outward-facing pattern class. Conservative: when in
/// doubt about a push target it treats a bare `git push` (no explicit safe remote
/// branch) as protected, so the gate over-blocks rather than under-blocks.
pub fn classify(command: &str) -> CommandClass {
    let toks = tokens(command);
    if toks.is_empty() {
        return CommandClass::Benign;
    }
    let joined = command.to_lowercase();
    let has = |p: &str| toks.iter().any(|t| t == p);

    // Spend / publish CLIs (money or a public release).
    if (has("npm") && has("publish"))
        || (has("cargo") && has("publish"))
        || (has("pnpm") && has("publish"))
        || (has("yarn") && has("publish"))
        || joined.contains("gh release create")
    {
        return CommandClass::SpendOrPublish;
    }

    // Deploy / release runs (carry a target = the workflow file / name).
    if joined.contains("gh workflow run") || (has("gh") && has("workflow") && has("run")) {
        // The target is the token after `run` (a workflow file/name), else "unknown".
        let target = toks
            .iter()
            .position(|t| t == "run")
            .and_then(|i| toks.get(i + 1))
            .cloned()
            .unwrap_or_else(|| "unknown".to_string());
        return CommandClass::Deploy { target };
    }

    // Merges.
    if joined.contains("gh pr merge") || (has("git") && has("merge") && mentions_protected_branch(&toks))
    {
        return CommandClass::Merge;
    }

    // Pushes: any force-push, or a push touching a protected branch, or a bare
    // `git push` with no explicit branch (target unknown => treat as protected).
    if has("git") && has("push") {
        let forced = toks
            .iter()
            .any(|t| t == "--force" || t == "-f" || t.starts_with("--force-with-lease"));
        if forced || mentions_protected_branch(&toks) {
            return CommandClass::ProtectedPush;
        }
        // A bare `git push` (only `git push` plus flags, no explicit non-protected
        // ref) has an ambiguous target - fail-closed to protected.
        let explicit_ref = toks
            .iter()
            .skip_while(|t| *t != "push")
            .skip(1)
            .any(|t| !t.starts_with('-'));
        if !explicit_ref {
            return CommandClass::ProtectedPush;
        }
    }

    CommandClass::Benign
}

/// The gate decision for a classified command by a resolved caller class. Fail-closed:
/// a `Read` caller is denied EVERY outward-facing pattern; a `Control` caller may
/// push/merge (its job) but a significant deploy or a spend/publish requires a verified
/// general authorization (`authorized`), which defaults absent => deny.
pub fn decide(
    class: &CommandClass,
    caller: CallerClass,
    registry: &SignificanceRegistry,
    authorized: bool,
) -> Decision {
    if matches!(class, CommandClass::Benign) {
        return Decision::Allow;
    }
    if caller == CallerClass::Read {
        return Decision::Deny(format!(
            "t-hub gate: a crew (read-tier) session may not run outward-facing actions \
             (push/merge/deploy/spend). Route this through the sanctioned path (a \
             control-capable relay / your captain). [{}]",
            class_label(class)
        ));
    }
    // Control-class caller.
    match class {
        CommandClass::Benign => Decision::Allow,
        CommandClass::ProtectedPush | CommandClass::Merge => Decision::Allow,
        CommandClass::SpendOrPublish => {
            if authorized {
                Decision::Allow
            } else {
                Decision::Deny(
                    "t-hub gate (R-C1): a spend / publish requires a verified general \
                     authorization present in the plane; none found - blocking."
                        .to_string(),
                )
            }
        }
        CommandClass::Deploy { target } => match registry.significance(target) {
            Significance::Routine => Decision::Allow,
            Significance::Significant => {
                if authorized {
                    Decision::Allow
                } else {
                    Decision::Deny(format!(
                        "t-hub gate (R-H1): '{target}' is a significant (production / \
                         user-facing / irreversible or unknown) deploy and requires a \
                         verified general confirm; none found - blocking."
                    ))
                }
            }
        },
    }
}

fn class_label(class: &CommandClass) -> &'static str {
    match class {
        CommandClass::Benign => "benign",
        CommandClass::ProtectedPush => "protected-push",
        CommandClass::Merge => "merge",
        CommandClass::Deploy { .. } => "deploy",
        CommandClass::SpendOrPublish => "spend-or-publish",
    }
}

/// Run the `--gate` mode: read the `PreToolUse` hook JSON on stdin, decide, and emit
/// the Claude Code decision. On ALLOW it exits 0 with no output (the normal permission
/// flow proceeds - the gate never auto-approves); on DENY it prints the block JSON and
/// exits 0. Any parse/IO error fails CLOSED (deny) for an outward-facing command and
/// otherwise allows a plainly-benign command through.
pub fn run() {
    let mut raw = String::new();
    if std::io::stdin().read_to_string(&mut raw).is_err() {
        // Cannot read the hook payload: allow (we cannot even identify a command).
        return;
    }
    let payload: serde_json::Value = serde_json::from_str(&raw).unwrap_or(serde_json::Value::Null);

    // The Bash command + cwd from the PreToolUse hook payload.
    let command = payload
        .get("tool_input")
        .and_then(|t| t.get("command"))
        .and_then(|c| c.as_str())
        .unwrap_or("");
    let cwd = payload.get("cwd").and_then(|c| c.as_str()).unwrap_or(".");

    let class = classify(command);
    if matches!(class, CommandClass::Benign) {
        return; // fast path: nothing outward-facing.
    }

    // Resolve the caller's capability class from the running app; fail-closed to Read.
    let caller = resolve_caller_class().unwrap_or(CallerClass::Read);
    let registry = SignificanceRegistry::load_for_cwd(cwd);
    // The verified general authorization is carried by item-1's plane; until that
    // substrate reaches this out-of-process hook it is treated as ABSENT (fail-closed).
    let authorized = general_authorization_present();

    match decide(&class, caller, &registry, authorized) {
        Decision::Allow => {}
        Decision::Deny(reason) => emit_deny(&reason),
    }
}

/// Emit the Claude Code `PreToolUse` block decision (JSON on stdout, exit 0).
fn emit_deny(reason: &str) {
    let out = serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason,
        }
    });
    println!("{out}");
}

/// Resolve the caller's capability class by asking the running app over the control
/// socket (`my_capability`) with THIS session's injected `T_HUB_CONTROL_ADDR` /
/// `T_HUB_CONTROL_TOKEN`. The token is the unspoofable class signal: a crew holds only
/// the read token and cannot present the control token it never received. `None` on any
/// error (missing env, unreachable app, malformed reply) => the caller fail-closes to
/// Read.
fn resolve_caller_class() -> Option<CallerClass> {
    let addr = std::env::var("T_HUB_CONTROL_ADDR").ok().filter(|a| !a.is_empty())?;
    let token = std::env::var("T_HUB_CONTROL_TOKEN").ok().filter(|t| !t.is_empty())?;
    let cap = control_my_capability(&addr, &token)?;
    match cap.as_str() {
        "control" => Some(CallerClass::Control),
        _ => Some(CallerClass::Read),
    }
}

/// One-shot control request for `my_capability`, returning the `capability` string.
/// A minimal line-delimited JSON round-trip (mirrors the app's control protocol) so
/// the gate needs no heavier client. Best-effort; `None` on any failure.
fn control_my_capability(addr: &str, token: &str) -> Option<String> {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpStream;
    use std::time::Duration;

    let stream = TcpStream::connect(addr).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(3))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(3))).ok();
    let mut writer = stream.try_clone().ok()?;
    let mut frame = serde_json::to_vec(&serde_json::json!({
        "token": token,
        "command": "my_capability",
        "args": {},
        "v": 2,
    }))
    .ok()?;
    frame.push(b'\n');
    writer.write_all(&frame).ok()?;
    writer.flush().ok()?;

    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).ok()?;
    let resp: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    resp.get("result")
        .and_then(|r| r.get("capability"))
        .and_then(|c| c.as_str())
        .map(|s| s.to_string())
}

/// Whether a verified general authorization for THIS action is present. item-1's plane
/// carries + verifies the confirm/authorization; until that substrate reaches this
/// out-of-process hook, the gate treats it as ABSENT (fail-closed) so a significant
/// deploy / spend is BLOCKED rather than let through unconfirmed. This is the honest
/// interim price (§2.4.1 class): the general performs a significant deploy directly.
fn general_authorization_present() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn benign_commands_are_not_outward_facing() {
        for cmd in ["ls -la", "cargo test", "git status", "git commit -m x", "git pull"] {
            assert_eq!(classify(cmd), CommandClass::Benign, "misclassified: {cmd}");
        }
    }

    #[test]
    fn pushes_merges_deploys_spends_are_classified() {
        assert_eq!(classify("git push --force origin feature"), CommandClass::ProtectedPush);
        assert_eq!(classify("git push origin main"), CommandClass::ProtectedPush);
        // A bare push has an ambiguous target => fail-closed to protected.
        assert_eq!(classify("git push"), CommandClass::ProtectedPush);
        // An explicit non-protected branch push is benign.
        assert_eq!(classify("git push origin my-feature"), CommandClass::Benign);
        assert_eq!(classify("gh pr merge 57 --squash"), CommandClass::Merge);
        assert_eq!(
            classify("gh workflow run release.yml -f variant=prod"),
            CommandClass::Deploy { target: "release.yml".to_string() }
        );
        assert_eq!(classify("npm publish"), CommandClass::SpendOrPublish);
        assert_eq!(classify("cargo publish"), CommandClass::SpendOrPublish);
        assert_eq!(classify("gh release create v1.2.3"), CommandClass::SpendOrPublish);
    }

    #[test]
    fn registry_is_fail_closed_to_significant() {
        let reg = SignificanceRegistry::builtin();
        // Unknown / release targets are significant.
        assert_eq!(reg.significance("release.yml"), Significance::Significant);
        assert_eq!(reg.significance("anything-unlisted"), Significance::Significant);
        // Only an explicitly-listed target is routine.
        let mut reg2 = SignificanceRegistry::builtin();
        reg2.routine.insert("preview.yml".to_string());
        assert_eq!(reg2.significance("preview.yml"), Significance::Routine);
        assert_eq!(reg2.significance("release.yml"), Significance::Significant);
    }

    #[test]
    fn crew_is_denied_every_outward_facing_pattern() {
        let reg = SignificanceRegistry::builtin();
        for cmd in [
            "git push origin main",
            "git push --force origin x",
            "gh pr merge 57",
            "gh workflow run release.yml",
            "gh workflow run preview.yml",
            "npm publish",
        ] {
            let d = decide(&classify(cmd), CallerClass::Read, &reg, false);
            assert!(matches!(d, Decision::Deny(_)), "crew must be denied: {cmd}");
        }
        // Even a routine deploy is denied for crew (crew never deploy).
        let mut routine = SignificanceRegistry::builtin();
        routine.routine.insert("preview.yml".to_string());
        let d = decide(
            &classify("gh workflow run preview.yml"),
            CallerClass::Read,
            &routine,
            false,
        );
        assert!(matches!(d, Decision::Deny(_)), "crew never deploy, even routine");
    }

    #[test]
    fn control_may_push_merge_but_significant_deploy_and_spend_need_authorization() {
        let reg = SignificanceRegistry::builtin();
        // Push / merge: a captain's job, allowed.
        assert_eq!(decide(&classify("git push origin main"), CallerClass::Control, &reg, false), Decision::Allow);
        assert_eq!(decide(&classify("gh pr merge 57"), CallerClass::Control, &reg, false), Decision::Allow);
        // Significant deploy without authorization: denied (fail-closed).
        assert!(matches!(
            decide(&classify("gh workflow run release.yml"), CallerClass::Control, &reg, false),
            Decision::Deny(_)
        ));
        // ... and allowed WITH a verified authorization.
        assert_eq!(
            decide(&classify("gh workflow run release.yml"), CallerClass::Control, &reg, true),
            Decision::Allow
        );
        // Spend / publish without authorization: denied.
        assert!(matches!(
            decide(&classify("npm publish"), CallerClass::Control, &reg, false),
            Decision::Deny(_)
        ));
    }

    #[test]
    fn control_routine_deploy_is_not_starved() {
        // A registry-routine deploy by a control caller is allowed WITHOUT waiting on
        // the general (the routine-reversible path is never starved).
        let mut reg = SignificanceRegistry::builtin();
        reg.routine.insert("preview.yml".to_string());
        assert_eq!(
            decide(&classify("gh workflow run preview.yml"), CallerClass::Control, &reg, false),
            Decision::Allow
        );
    }
}
