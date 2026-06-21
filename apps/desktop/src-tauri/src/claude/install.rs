//! The I/O layer over the pure hook-merge helpers in [`crate::claude::hooks`]:
//! read `~/.claude/settings.json`, apply the non-destructive merge / removal,
//! and write it back — atomically and only with explicit consent.
//!
//! ## Safety (REVIEW risk: "hook install edits ~/.claude/settings.json")
//!   - **Consent is mandatory**: [`install_hooks`] takes a `consent: bool` and
//!     refuses without it. The UI must collect this explicitly.
//!   - **Non-destructive**: the actual JSON surgery is the tested-pure
//!     [`hooks::merge_into_settings`] (preserves user hooks + all non-hook
//!     keys; idempotent) / [`hooks::remove_from_settings`] (strips only our
//!     marker-tagged entries).
//!   - **Survives hand-edits**: we parse whatever is on disk; a missing file is
//!     treated as `{}`; a malformed file is reported (we never blindly
//!     overwrite unreadable JSON).
//!   - **Atomic write**: write to a temp file in the same dir, then rename over
//!     the target, so a crash mid-write can't truncate the user's settings.
//!   - **Backup**: before the first write we copy the existing file to
//!     `settings.json.t-hub-bak` so the user can always recover the pre-install
//!     state.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

use crate::claude::hooks;

/// `~/.claude` config dir, honoring `CLAUDE_CONFIG_DIR` (which relocates the
/// whole Claude store — REVIEW §10.2).
///
/// **Unix only.** On Windows the Claude store we must edit lives *inside WSL*
/// (Claude Code runs there), not at the Windows `HOME`; see [`settings_path`].
#[cfg(unix)]
fn claude_config_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        let dir = PathBuf::from(dir);
        if !dir.as_os_str().is_empty() {
            return Some(dir);
        }
    }
    std::env::var_os("HOME").map(|h| Path::new(&h).join(".claude"))
}

/// Path to the user-scope `settings.json` (where global hooks live).
///
/// **The Windows↔WSL gotcha (PROBLEM 1):** T-Hub is a *Windows* process, but
/// Claude Code runs *inside WSL* and reads `~/.claude/settings.json` at the WSL
/// `$HOME` (e.g. `/home/<user>/.claude/...`). The Windows `HOME` env var (if set
/// at all) points at `C:\Users\<user>`, so writing there has no effect on Claude.
///
/// So on Windows we resolve the **WSL** home by shelling
/// `wsl.exe -d <distro> -- bash -lc 'echo $HOME'` once (distro from
/// `T_HUB_DISTRO`, default `Ubuntu-24.04`), then target the file via its UNC
/// form `\\wsl.localhost\<distro>\home\<user>\.claude\settings.json`, which
/// std::fs can read/write directly from Windows. On unix we keep the native
/// `HOME` / `CLAUDE_CONFIG_DIR` behavior.
#[cfg(unix)]
fn settings_path() -> Result<PathBuf> {
    claude_config_dir()
        .map(|d| d.join("settings.json"))
        .ok_or_else(|| anyhow!("could not resolve ~/.claude (no HOME / CLAUDE_CONFIG_DIR)"))
}

#[cfg(windows)]
fn settings_path() -> Result<PathBuf> {
    let distro = wsl_distro();
    let home = wsl_home(&distro)?; // e.g. "/home/natkins"
    Ok(wsl_settings_unc(&distro, &home))
}

/// The WSL distro to target, mirroring `crate::default_distro` (env
/// `T_HUB_DISTRO`, default `Ubuntu-24.04`).
#[cfg(windows)]
fn wsl_distro() -> String {
    std::env::var("T_HUB_DISTRO").unwrap_or_else(|_| "Ubuntu-24.04".to_string())
}

/// Resolve the WSL `$HOME` for `distro` by shelling a login bash once. Uses the
/// CREATE_NO_WINDOW flag (0x08000000 — same as `tmux.rs`) so no console flashes.
#[cfg(windows)]
fn wsl_home(distro: &str) -> Result<String> {
    use std::os::windows::process::CommandExt;
    let out = std::process::Command::new("wsl.exe")
        .arg("-d")
        .arg(distro)
        .arg("--")
        .arg("bash")
        .arg("-lc")
        .arg("echo $HOME")
        .creation_flags(0x0800_0000)
        .output()
        .with_context(|| format!("running `wsl.exe -d {distro} -- bash -lc 'echo $HOME'`"))?;
    if !out.status.success() {
        return Err(anyhow!(
            "wsl.exe -d {distro} could not resolve $HOME (exit {:?}): {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let home = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if home.is_empty() {
        return Err(anyhow!("wsl.exe -d {distro} returned an empty $HOME"));
    }
    Ok(home)
}

/// Build the Windows-visible UNC path to the WSL `~/.claude/settings.json`:
/// `\\wsl.localhost\<distro>\<home-with-backslashes>\.claude\settings.json`.
/// The WSL `$HOME` (a POSIX path like `/home/natkins`) maps under the distro
/// share with `/` → `\`. std::fs reads/writes this directly from Windows.
#[cfg(windows)]
fn wsl_settings_unc(distro: &str, wsl_home: &str) -> PathBuf {
    let home_rel = wsl_home.trim_start_matches('/').replace('/', "\\");
    let s = format!(r"\\wsl.localhost\{distro}\{home_rel}\.claude\settings.json");
    PathBuf::from(s)
}

/// Read the current settings JSON. A missing file → `{}` (a fresh install);
/// a present-but-unparseable file is an error (we will not clobber it).
fn read_settings(path: &Path) -> Result<serde_json::Value> {
    match std::fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(serde_json::json!({})),
        Ok(text) => serde_json::from_str(&text)
            .with_context(|| format!("parsing existing {path:?} (refusing to overwrite)")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(serde_json::json!({})),
        Err(e) => Err(e).with_context(|| format!("reading {path:?}")),
    }
}

/// Atomically write `value` (pretty-printed) to `path`: write a sibling temp
/// file, fsync, then rename over the target.
fn write_settings_atomic(path: &Path, value: &serde_json::Value) -> Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| anyhow!("settings path has no parent dir"))?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {dir:?}"))?;

    let text = serde_json::to_string_pretty(value).context("serializing settings")?;
    let tmp = path.with_extension("json.t-hub-tmp");
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp).with_context(|| format!("creating {tmp:?}"))?;
        f.write_all(text.as_bytes()).context("writing temp settings")?;
        f.write_all(b"\n").ok();
        f.flush().ok();
        f.sync_data().ok();
    }
    std::fs::rename(&tmp, path).with_context(|| format!("renaming {tmp:?} -> {path:?}"))?;
    Ok(())
}

/// Make a one-time backup of the existing settings before our first write.
/// Best-effort: a failure here is logged by the caller, not fatal (the atomic
/// write is the real safety net), but we surface it so the UI can warn.
fn backup_once(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(()); // nothing to back up (fresh install).
    }
    let bak = path.with_extension("json.t-hub-bak");
    if bak.exists() {
        return Ok(()); // keep the earliest backup; don't overwrite it.
    }
    std::fs::copy(path, &bak)
        .map(|_| ())
        .with_context(|| format!("backing up {path:?} -> {bak:?}"))
}

/// The outcome of an install/uninstall, surfaced to the UI.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallReport {
    /// The settings.json path that was written.
    pub settings_path: String,
    /// Whether a backup was made (or already existed).
    pub backed_up: bool,
    /// Number of hook events T-Hub now manages (post-op).
    pub managed_events: usize,
    /// Human summary.
    pub message: String,
}

/// Count how many top-level hook events contain a T-Hub-managed (marker)
/// command after an op — for the report.
fn count_managed(settings: &serde_json::Value) -> usize {
    let Some(hooks) = settings.get("hooks").and_then(|h| h.as_object()) else {
        return 0;
    };
    hooks
        .values()
        .filter(|groups| {
            groups
                .as_array()
                .map(|arr| {
                    arr.iter().any(|g| {
                        serde_json::to_string(g)
                            .map(|s| s.contains(hooks::T_HUB_HOOK_MARKER))
                            .unwrap_or(false)
                    })
                })
                .unwrap_or(false)
        })
        .count()
}

/// Install T-Hub's hook handlers into `~/.claude/settings.json`.
///
/// `agent_bin` is the resolved WSL path to the hook entrypoint (the
/// `t-hub-agent` binary; it gains a `--hook <EVENT>` mode). Refuses without
/// `consent`. Non-destructive + atomic + backed up. Resolves the settings path
/// from the environment; see [`install_hooks_at`] for the path-injected core.
// Kept as the "install everything" convenience + a stable entry point; the
// command path uses install_hooks_events. Tests use install_hooks_at.
#[allow(dead_code)]
pub fn install_hooks(agent_bin: &str, consent: bool) -> Result<InstallReport> {
    let all: Vec<String> = hooks::HOOK_EVENTS.iter().map(|s| s.to_string()).collect();
    install_hooks_events(agent_bin, consent, &all)
}

/// Install ONLY the selected hook `events`, reconciling the managed set to be
/// exactly that selection (so unchecking an event uninstalls it). Empty = all.
pub fn install_hooks_events(
    agent_bin: &str,
    consent: bool,
    events: &[String],
) -> Result<InstallReport> {
    // Resolve the REAL absolute path to t-hub-agent. Claude Code runs hooks via
    // `/bin/sh` with a minimal PATH that does NOT include `~/.local/bin`, so a
    // bare `t-hub-agent` (the UI default) or a stale `/usr/bin/t-hub-agent`
    // fails with "not found" and no hook ever fires. We resolve a concrete path
    // inside WSL instead of trusting the passed value.
    let resolved = resolve_agent_bin(agent_bin);
    install_hooks_at_events(&settings_path()?, &resolved, consent, events)
}

/// The subset of T-Hub hook events currently installed in the user's
/// settings.json (so the UI can pre-check the right boxes).
pub fn managed_event_names() -> Result<Vec<String>> {
    let existing = read_settings(&settings_path()?)?;
    Ok(hooks::managed_events(&existing))
}

/// Resolve the absolute path to the `t-hub-agent` binary that the hooks will
/// invoke. Prefers a login-shell `command -v` (finds wherever it's installed),
/// then `~/.local/bin/t-hub-agent` (the standard install location), then the
/// value the caller passed as a last resort.
#[cfg(windows)]
fn resolve_agent_bin(passed: &str) -> String {
    use std::os::windows::process::CommandExt;
    let distro = wsl_distro();
    if let Ok(out) = std::process::Command::new("wsl.exe")
        .args(["-d", &distro, "--", "bash", "-lc", "command -v t-hub-agent"])
        .creation_flags(0x0800_0000)
        .output()
    {
        if out.status.success() {
            let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if p.starts_with('/') {
                return p;
            }
        }
    }
    if let Ok(home) = wsl_home(&distro) {
        return format!("{home}/.local/bin/t-hub-agent");
    }
    passed.to_string()
}

#[cfg(unix)]
fn resolve_agent_bin(passed: &str) -> String {
    // An absolute path that actually exists wins.
    if passed.starts_with('/') && Path::new(passed).exists() {
        return passed.to_string();
    }
    if let Ok(out) = std::process::Command::new("bash")
        .args(["-lc", "command -v t-hub-agent"])
        .output()
    {
        if out.status.success() {
            let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if p.starts_with('/') {
                return p;
            }
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        return Path::new(&home)
            .join(".local/bin/t-hub-agent")
            .to_string_lossy()
            .into_owned();
    }
    passed.to_string()
}

/// Path-injected core of [`install_hooks`] — operates on an explicit
/// `settings.json` path (no env reads), so it is race-free under test.
#[allow(dead_code)] // used by tests + as the "install everything at path" helper
pub fn install_hooks_at(path: &Path, agent_bin: &str, consent: bool) -> Result<InstallReport> {
    let all: Vec<String> = hooks::HOOK_EVENTS.iter().map(|s| s.to_string()).collect();
    install_hooks_at_events(path, agent_bin, consent, &all)
}

/// Path-injected, subset-aware install. Reconciles the managed set to EXACTLY
/// `events`: strips all T-Hub hooks first, then merges the selection, so an
/// unchecked event is removed. Empty `events` is treated as "all".
pub fn install_hooks_at_events(
    path: &Path,
    agent_bin: &str,
    consent: bool,
    events: &[String],
) -> Result<InstallReport> {
    if !consent {
        return Err(anyhow!(
            "refusing to modify {} without explicit consent",
            path.display()
        ));
    }
    let all_events: Vec<String>;
    let events: &[String] = if events.is_empty() {
        all_events = hooks::HOOK_EVENTS.iter().map(|s| s.to_string()).collect();
        &all_events
    } else {
        events
    };
    let existing = read_settings(path)?;
    let backed_up = backup_once(path).is_ok();
    // Strip every T-Hub hook, then merge exactly the selection — the managed
    // set ends up equal to `events` (deselecting an event uninstalls it).
    let cleaned = hooks::remove_from_settings(&existing);
    let event_refs: Vec<&str> = events.iter().map(|s| s.as_str()).collect();
    let merged = hooks::merge_into_settings_for(&cleaned, agent_bin, &event_refs);
    // Also install the Claude `statusLine` (the USAGE data source). The hooks
    // alone never feed the status bridge — Claude's statusline is a SEPARATE
    // setting that runs `t-hub-agent --statusline`, which journals a
    // StatusSnapshot the core re-emits on `status://snapshot`. Without this,
    // the sidebar USAGE strip shows only dashes. Respects a user-authored
    // statusLine (merge_statusline_into_settings leaves a non-managed one alone).
    let merged = hooks::merge_statusline_into_settings(&merged, agent_bin);
    write_settings_atomic(path, &merged)?;
    let managed = count_managed(&merged);
    let statusline_on = hooks::statusline_managed(&merged);
    crate::diag::diag_log(format!(
        "claude/install: wrote {} (hooks={managed}, statusLine={statusline_on}, agent_bin={agent_bin})",
        path.display()
    ));
    Ok(InstallReport {
        settings_path: path.display().to_string(),
        backed_up,
        managed_events: managed,
        message: if statusline_on {
            format!("Installed T-Hub handlers for {managed} hook events + usage statusline.")
        } else {
            format!(
                "Installed T-Hub handlers for {managed} hook events. \
                 (Kept your existing Claude statusLine — usage may not report.)"
            )
        },
    })
}

/// Remove T-Hub's hook handlers (clean uninstall), leaving the user's own
/// hooks and all non-hook settings intact. Idempotent.
pub fn uninstall_hooks() -> Result<InstallReport> {
    uninstall_hooks_at(&settings_path()?)
}

/// Path-injected core of [`uninstall_hooks`].
pub fn uninstall_hooks_at(path: &Path) -> Result<InstallReport> {
    let existing = read_settings(path)?;
    let cleaned = hooks::remove_from_settings(&existing);
    // Also remove our managed statusLine (leaves a user-authored one intact).
    let cleaned = hooks::remove_statusline_from_settings(&cleaned);
    write_settings_atomic(path, &cleaned)?;
    Ok(InstallReport {
        settings_path: path.display().to_string(),
        backed_up: false,
        managed_events: count_managed(&cleaned),
        message: "Removed T-Hub hook handlers + usage statusline.".to_string(),
    })
}

/// Report whether T-Hub hooks are currently installed (any marker present)
/// without modifying anything — for the UI to show install state.
pub fn hooks_installed() -> Result<bool> {
    hooks_installed_at(&settings_path()?)
}

/// Path-injected core of [`hooks_installed`].
pub fn hooks_installed_at(path: &Path) -> Result<bool> {
    let existing = read_settings(path)?;
    Ok(count_managed(&existing) > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unique temp settings.json path per test. Race-free: tests use the
    /// path-injected `*_at` functions and never touch the process env, so they
    /// parallelize safely (the previous CLAUDE_CONFIG_DIR approach raced).
    fn temp_settings(tag: &str) -> PathBuf {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("t-hub-install-{tag}-{ts}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("settings.json")
    }

    fn cleanup(path: &Path) {
        if let Some(dir) = path.parent() {
            std::fs::remove_dir_all(dir).ok();
        }
    }

    #[test]
    fn install_requires_consent() {
        let path = temp_settings("consent");
        let err = install_hooks_at(&path, "/usr/bin/t-hub-agent", false).unwrap_err();
        assert!(err.to_string().contains("consent"));
        cleanup(&path);
    }

    #[test]
    fn install_creates_settings_with_managed_hooks() {
        let path = temp_settings("create");
        let report = install_hooks_at(&path, "/usr/bin/t-hub-agent", true).unwrap();
        assert!(report.managed_events >= 15);
        assert!(path.exists());
        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(count_managed(&written) >= 15);
        cleanup(&path);
    }

    #[test]
    fn install_preserves_user_keys_and_backs_up() {
        let path = temp_settings("preserve");
        // Seed a user settings file with a non-hook key and a user hook.
        let seed = serde_json::json!({
            "model": "opus",
            "cleanupPeriodDays": 30,
            "hooks": {
                "PreToolUse": [ { "matcher": "*", "hooks": [
                    { "type": "command", "command": "echo user-hook" }
                ] } ]
            }
        });
        write_settings_atomic(&path, &seed).unwrap();

        let report = install_hooks_at(&path, "/usr/bin/t-hub-agent", true).unwrap();
        assert!(report.backed_up, "a backup must be made over an existing file");
        assert!(path.with_extension("json.t-hub-bak").exists());

        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        // User keys preserved.
        assert_eq!(written["model"], "opus");
        assert_eq!(written["cleanupPeriodDays"], 30);
        // User hook preserved.
        let pre = written["hooks"]["PreToolUse"].as_array().unwrap();
        assert!(pre
            .iter()
            .any(|g| serde_json::to_string(g).unwrap().contains("user-hook")));
        cleanup(&path);
    }

    #[test]
    fn install_writes_statusline_and_uninstall_removes_it() {
        let path = temp_settings("statusline");
        install_hooks_at(&path, "/usr/bin/t-hub-agent", true).unwrap();
        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        // statusLine installed and points at --statusline.
        assert!(hooks::statusline_managed(&written), "statusLine must be installed");
        assert!(written["statusLine"]["command"]
            .as_str()
            .unwrap()
            .contains("--statusline"));

        uninstall_hooks_at(&path).unwrap();
        let after: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(after.get("statusLine").is_none(), "statusLine must be removed on uninstall");
        cleanup(&path);
    }

    #[test]
    fn install_does_not_steal_user_statusline() {
        let path = temp_settings("user-statusline");
        let seed = serde_json::json!({
            "statusLine": { "type": "command", "command": "my-own.sh" }
        });
        write_settings_atomic(&path, &seed).unwrap();
        install_hooks_at(&path, "/usr/bin/t-hub-agent", true).unwrap();
        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        // User's statusLine survives; ours is NOT forced in.
        assert_eq!(written["statusLine"]["command"].as_str(), Some("my-own.sh"));
        assert!(!hooks::statusline_managed(&written));
        // Uninstall must NOT remove the user's statusLine.
        uninstall_hooks_at(&path).unwrap();
        let after: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(after["statusLine"]["command"].as_str(), Some("my-own.sh"));
        cleanup(&path);
    }

    #[test]
    fn install_then_uninstall_is_clean_and_idempotent() {
        let path = temp_settings("roundtrip");
        let seed = serde_json::json!({
            "hooks": { "PreToolUse": [ { "matcher": "*", "hooks": [
                { "type": "command", "command": "echo keepme" }
            ] } ] }
        });
        write_settings_atomic(&path, &seed).unwrap();

        install_hooks_at(&path, "/usr/bin/t-hub-agent", true).unwrap();
        assert!(hooks_installed_at(&path).unwrap());
        // Idempotent install: second install keeps exactly one set per event.
        install_hooks_at(&path, "/usr/bin/t-hub-agent", true).unwrap();

        let r = uninstall_hooks_at(&path).unwrap();
        assert_eq!(r.managed_events, 0);
        assert!(!hooks_installed_at(&path).unwrap());

        // User hook survived the round-trip.
        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let pre = written["hooks"]["PreToolUse"].as_array().unwrap();
        assert!(pre
            .iter()
            .any(|g| serde_json::to_string(g).unwrap().contains("keepme")));
        cleanup(&path);
    }

    #[test]
    fn refuses_to_overwrite_malformed_settings() {
        let path = temp_settings("malformed");
        std::fs::write(&path, "{ this is not json ").unwrap();
        let err = install_hooks_at(&path, "/usr/bin/t-hub-agent", true).unwrap_err();
        assert!(err.to_string().contains("parsing"));
        cleanup(&path);
    }

    #[cfg(windows)]
    #[test]
    fn wsl_settings_unc_maps_posix_home_to_distro_share() {
        // POSIX `$HOME` → UNC under the distro share, `/` → `\`, with the
        // leading slash dropped so we don't get a doubled separator.
        let p = super::wsl_settings_unc("Ubuntu-24.04", "/home/natkins");
        assert_eq!(
            p.to_string_lossy(),
            r"\\wsl.localhost\Ubuntu-24.04\home\natkins\.claude\settings.json"
        );
    }
}
