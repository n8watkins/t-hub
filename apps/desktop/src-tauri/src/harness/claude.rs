//! Claude harness adapter (Phase-1 seam).
//!
//! A thin delegation to the existing literal Claude command strings - the ones
//! the `SpawnMenu.tsx` presets and the recall path already use. Phase 1 does NOT
//! migrate `claude/hooks.rs`, `claude/install.rs`, or the Claude recall scan
//! behind this trait; those stay on their existing code paths (explicit Phase-2
//! non-goal). The purpose here is to lock the "no regression on the Claude path"
//! guarantee as an executable contract: the argv strings this returns are
//! byte-identical to what shipped before the seam.

use std::path::PathBuf;

use super::{home_dir, sh_single_quote, Harness, HarnessAdapter, PermMode};

/// The Claude adapter. Zero-sized: it only formats command strings.
pub struct ClaudeHarness;

impl HarnessAdapter for ClaudeHarness {
    fn harness(&self) -> Harness {
        Harness::Claude
    }

    fn fresh_argv(&self, prompt: &str) -> String {
        if prompt.is_empty() {
            "claude".to_string()
        } else {
            format!("claude {}", sh_single_quote(prompt))
        }
    }

    fn resume_argv(&self, session_id: &str) -> String {
        // Matches the recall path's `claude --resume '<id>'` (the interactive,
        // id-specific resume). The no-id picker preset lives in SpawnMenu.tsx.
        format!("claude --resume {}", sh_single_quote(session_id))
    }

    fn exec_turn_argv(&self, prompt: &str, resume: Option<&str>, perm: PermMode) -> String {
        // Claude's headless print mode. Phase 1 crews are Codex `exec`; this
        // Claude form exists for symmetry/tests and does NOT change any live
        // Claude launch path (which still rides the existing hooks producer).
        let flags = self.permission_map(perm).join(" ");
        let flags = if flags.is_empty() {
            String::new()
        } else {
            format!("{flags} ")
        };
        match resume {
            Some(id) => format!(
                "claude --resume {} -p {}{}",
                sh_single_quote(id),
                flags,
                sh_single_quote(prompt)
            ),
            None => format!("claude -p {}{}", flags, sh_single_quote(prompt)),
        }
    }

    fn permission_map(&self, perm: PermMode) -> Vec<String> {
        match perm {
            PermMode::BypassPermissions => vec!["--dangerously-skip-permissions".to_string()],
            PermMode::AcceptEdits => {
                vec!["--permission-mode".to_string(), "acceptEdits".to_string()]
            }
            // Default posture is the absence of any override (today's behavior).
            PermMode::Default => vec![],
        }
    }

    fn session_home(&self) -> PathBuf {
        home_dir().join(".claude").join("projects")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_argv_exact_strings() {
        let a = ClaudeHarness;
        assert_eq!(a.fresh_argv(""), "claude");
        assert_eq!(a.fresh_argv("do the thing"), "claude 'do the thing'");
    }

    #[test]
    fn resume_argv_exact_string() {
        // No-regression lock: the recall path emits exactly this for a Claude row.
        let a = ClaudeHarness;
        assert_eq!(
            a.resume_argv("abc-123"),
            "claude --resume 'abc-123'"
        );
    }

    #[test]
    fn permission_map_exact_flags() {
        let a = ClaudeHarness;
        assert_eq!(
            a.permission_map(PermMode::BypassPermissions),
            vec!["--dangerously-skip-permissions"]
        );
        assert_eq!(
            a.permission_map(PermMode::AcceptEdits),
            vec!["--permission-mode", "acceptEdits"]
        );
        assert_eq!(a.permission_map(PermMode::Default), Vec::<String>::new());
    }

    #[test]
    fn exec_turn_argv_shapes() {
        let a = ClaudeHarness;
        assert_eq!(
            a.exec_turn_argv("go", None, PermMode::BypassPermissions),
            "claude -p --dangerously-skip-permissions 'go'"
        );
        assert_eq!(
            a.exec_turn_argv("go", Some("id-1"), PermMode::Default),
            "claude --resume 'id-1' -p 'go'"
        );
    }

    #[test]
    fn session_home_is_claude_projects() {
        let a = ClaudeHarness;
        assert!(a.session_home().ends_with(".claude/projects"));
    }
}
