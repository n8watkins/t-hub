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
//!     `settings.json.termhub-bak` so the user can always recover the pre-install
//!     state.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

use crate::claude::hooks;

/// `~/.claude` config dir, honoring `CLAUDE_CONFIG_DIR` (which relocates the
/// whole Claude store — REVIEW §10.2).
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
fn settings_path() -> Result<PathBuf> {
    claude_config_dir()
        .map(|d| d.join("settings.json"))
        .ok_or_else(|| anyhow!("could not resolve ~/.claude (no HOME / CLAUDE_CONFIG_DIR)"))
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
    let tmp = path.with_extension("json.termhub-tmp");
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
    let bak = path.with_extension("json.termhub-bak");
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
    /// Number of hook events TermHub now manages (post-op).
    pub managed_events: usize,
    /// Human summary.
    pub message: String,
}

/// Count how many top-level hook events contain a TermHub-managed (marker)
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
                            .map(|s| s.contains(hooks::TERMHUB_HOOK_MARKER))
                            .unwrap_or(false)
                    })
                })
                .unwrap_or(false)
        })
        .count()
}

/// Install TermHub's hook handlers into `~/.claude/settings.json`.
///
/// `agent_bin` is the resolved WSL path to the hook entrypoint (the
/// `termhub-agent` binary; it gains a `--hook <EVENT>` mode). Refuses without
/// `consent`. Non-destructive + atomic + backed up. Resolves the settings path
/// from the environment; see [`install_hooks_at`] for the path-injected core.
pub fn install_hooks(agent_bin: &str, consent: bool) -> Result<InstallReport> {
    install_hooks_at(&settings_path()?, agent_bin, consent)
}

/// Path-injected core of [`install_hooks`] — operates on an explicit
/// `settings.json` path (no env reads), so it is race-free under test.
pub fn install_hooks_at(path: &Path, agent_bin: &str, consent: bool) -> Result<InstallReport> {
    if !consent {
        return Err(anyhow!(
            "refusing to modify {} without explicit consent",
            path.display()
        ));
    }
    let existing = read_settings(path)?;
    let backed_up = backup_once(path).is_ok();
    let merged = hooks::merge_into_settings(&existing, agent_bin);
    write_settings_atomic(path, &merged)?;
    let managed = count_managed(&merged);
    Ok(InstallReport {
        settings_path: path.display().to_string(),
        backed_up,
        managed_events: managed,
        message: format!("Installed TermHub handlers for {managed} hook events."),
    })
}

/// Remove TermHub's hook handlers (clean uninstall), leaving the user's own
/// hooks and all non-hook settings intact. Idempotent.
pub fn uninstall_hooks() -> Result<InstallReport> {
    uninstall_hooks_at(&settings_path()?)
}

/// Path-injected core of [`uninstall_hooks`].
pub fn uninstall_hooks_at(path: &Path) -> Result<InstallReport> {
    let existing = read_settings(path)?;
    let cleaned = hooks::remove_from_settings(&existing);
    write_settings_atomic(path, &cleaned)?;
    Ok(InstallReport {
        settings_path: path.display().to_string(),
        backed_up: false,
        managed_events: count_managed(&cleaned),
        message: "Removed TermHub hook handlers.".to_string(),
    })
}

/// Report whether TermHub hooks are currently installed (any marker present)
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
        let dir = std::env::temp_dir().join(format!("termhub-install-{tag}-{ts}"));
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
        let err = install_hooks_at(&path, "/usr/bin/termhub-agent", false).unwrap_err();
        assert!(err.to_string().contains("consent"));
        cleanup(&path);
    }

    #[test]
    fn install_creates_settings_with_managed_hooks() {
        let path = temp_settings("create");
        let report = install_hooks_at(&path, "/usr/bin/termhub-agent", true).unwrap();
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

        let report = install_hooks_at(&path, "/usr/bin/termhub-agent", true).unwrap();
        assert!(report.backed_up, "a backup must be made over an existing file");
        assert!(path.with_extension("json.termhub-bak").exists());

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
    fn install_then_uninstall_is_clean_and_idempotent() {
        let path = temp_settings("roundtrip");
        let seed = serde_json::json!({
            "hooks": { "PreToolUse": [ { "matcher": "*", "hooks": [
                { "type": "command", "command": "echo keepme" }
            ] } ] }
        });
        write_settings_atomic(&path, &seed).unwrap();

        install_hooks_at(&path, "/usr/bin/termhub-agent", true).unwrap();
        assert!(hooks_installed_at(&path).unwrap());
        // Idempotent install: second install keeps exactly one set per event.
        install_hooks_at(&path, "/usr/bin/termhub-agent", true).unwrap();

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
        let err = install_hooks_at(&path, "/usr/bin/termhub-agent", true).unwrap_err();
        assert!(err.to_string().contains("parsing"));
        cleanup(&path);
    }
}
