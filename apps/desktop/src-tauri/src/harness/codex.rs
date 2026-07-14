//! Codex harness adapter (Phase-1 seam).
//!
//! Builds the interactive launch strings, the headless crew-turn pipeline, and
//! the permission-flag map for OpenAI Codex (`codex-cli`), all verified against
//! the fleet-pinned Codex 0.142.5. Everything here is a pure string builder; the
//! actual lifecycle producer that consumes `exec_turn_argv` is the PR-B
//! `t-hub-agent --codex-tap` mode.
//!
//! Steer/wake contract (HIGH-1): a Codex `exec` crew's pane is a plain login
//! shell BETWEEN turns, so a Codex crew is steered ONLY by injecting the SHELL
//! COMMAND that `exec_turn_argv(prompt, Some(thread_id), ...)` builds - never by
//! sending prose (which the shell would execute on a bypass-provisioned
//! workspace). See the crew-brief doctrine block.

use std::path::PathBuf;

use super::{home_dir, sh_single_quote, Harness, HarnessAdapter, PermMode};

/// The tap that reads Codex `exec --json` ThreadEvent JSONL from stdin and
/// appends journal entries (the PR-B producer). Kept as one constant so the
/// pipeline string and the doctrine stay in lockstep.
pub const CODEX_TAP: &str = "t-hub-agent --codex-tap";

/// The Codex adapter. Zero-sized: it only formats command strings.
pub struct CodexHarness;

impl HarnessAdapter for CodexHarness {
    fn harness(&self) -> Harness {
        Harness::Codex
    }

    fn fresh_argv(&self, prompt: &str) -> String {
        if prompt.is_empty() {
            "codex".to_string()
        } else {
            format!("codex {}", sh_single_quote(prompt))
        }
    }

    fn fresh_argv_with_permissions(&self, prompt: &str, perm: PermMode) -> String {
        let flags = self.permission_map(perm).join(" ");
        match (flags.is_empty(), prompt.is_empty()) {
            (true, true) => "codex".to_string(),
            (true, false) => format!("codex {}", sh_single_quote(prompt)),
            (false, true) => format!("codex {flags}"),
            (false, false) => format!("codex {flags} {}", sh_single_quote(prompt)),
        }
    }

    fn resume_argv(&self, session_id: &str) -> String {
        // Codex's interactive resume-by-id (verified on 0.142.5). The no-id
        // picker preset (`codex resume`) lives in SpawnMenu.tsx.
        format!("codex resume {}", sh_single_quote(session_id))
    }

    fn exec_turn_argv(&self, prompt: &str, resume: Option<&str>, perm: PermMode) -> String {
        // Headless one-turn pipeline, tee'd into the journal via the tap:
        //   fresh:  codex exec        --json <perm> '<prompt>' | t-hub-agent --codex-tap
        //   resume: codex exec resume '<id>' --json <perm> '<prompt>' | t-hub-agent --codex-tap
        // The perm flags come from `permission_map` uniformly (fresh AND resume),
        // so a resumed turn in a fresh worktree still carries --skip-git-repo-check.
        let flags = self.permission_map(perm).join(" ");
        let head = match resume {
            Some(id) => format!("codex exec resume {}", sh_single_quote(id)),
            None => "codex exec".to_string(),
        };
        format!(
            "{head} --json {flags} {} | {CODEX_TAP}",
            sh_single_quote(prompt)
        )
    }

    fn permission_map(&self, perm: PermMode) -> Vec<String> {
        match perm {
            // Crew default. --skip-git-repo-check lets exec run in fresh
            // worktrees; the long bypass flag (never the `--yolo` alias, which is
            // absent from the installed help) skips all approvals + sandboxing.
            PermMode::BypassPermissions => vec![
                "--dangerously-bypass-approvals-and-sandbox".to_string(),
                "--skip-git-repo-check".to_string(),
            ],
            // Approximate acceptEdits. Documented gap: no exact analog, and
            // network is off by default under workspace-write (so no `git push`).
            PermMode::AcceptEdits => vec!["--sandbox".to_string(), "workspace-write".to_string()],
            PermMode::Default => vec!["--sandbox".to_string(), "read-only".to_string()],
        }
    }

    fn session_home(&self) -> PathBuf {
        home_dir().join(".codex").join("sessions")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_argv_exact_strings() {
        let a = CodexHarness;
        assert_eq!(a.fresh_argv(""), "codex");
        assert_eq!(a.fresh_argv("build it"), "codex 'build it'");
    }

    #[test]
    fn fresh_argv_applies_explicit_unrestricted_permissions() {
        let a = CodexHarness;
        assert_eq!(
            a.fresh_argv_with_permissions("build it", PermMode::BypassPermissions),
            "codex --dangerously-bypass-approvals-and-sandbox --skip-git-repo-check 'build it'"
        );
    }

    #[test]
    fn resume_argv_exact_string() {
        let a = CodexHarness;
        assert_eq!(
            a.resume_argv("019f5390-6497-75c2-ad90-e721d1b6d1d5"),
            "codex resume '019f5390-6497-75c2-ad90-e721d1b6d1d5'"
        );
    }

    #[test]
    fn permission_map_exact_flags() {
        // The D4 permission-map table (test-locked exact strings).
        let a = CodexHarness;
        assert_eq!(
            a.permission_map(PermMode::BypassPermissions),
            vec![
                "--dangerously-bypass-approvals-and-sandbox",
                "--skip-git-repo-check"
            ]
        );
        assert_eq!(
            a.permission_map(PermMode::AcceptEdits),
            vec!["--sandbox", "workspace-write"]
        );
        assert_eq!(
            a.permission_map(PermMode::Default),
            vec!["--sandbox", "read-only"]
        );
    }

    #[test]
    fn exec_turn_argv_fresh_is_the_plan_pipeline() {
        // Locks the D3 fresh crew pipeline verbatim (and the LONG bypass flag,
        // never `--yolo`).
        let a = CodexHarness;
        assert_eq!(
            a.exec_turn_argv("do work", None, PermMode::BypassPermissions),
            "codex exec --json --dangerously-bypass-approvals-and-sandbox --skip-git-repo-check 'do work' | t-hub-agent --codex-tap"
        );
    }

    #[test]
    fn exec_turn_argv_resume_is_the_plan_steer_command() {
        // Locks the D3 steer/wake command shape (resume by thread id).
        let a = CodexHarness;
        assert_eq!(
            a.exec_turn_argv("next step", Some("thread-xyz"), PermMode::BypassPermissions),
            "codex exec resume 'thread-xyz' --json --dangerously-bypass-approvals-and-sandbox --skip-git-repo-check 'next step' | t-hub-agent --codex-tap"
        );
    }

    #[test]
    fn exec_turn_argv_quotes_hostile_prompts() {
        // A prompt with shell metachars must never break out of the single quotes.
        let a = CodexHarness;
        let cmd = a.exec_turn_argv("rm -rf / ; echo `id`", None, PermMode::BypassPermissions);
        assert!(cmd.contains("'rm -rf / ; echo `id`'"));
        assert!(cmd.ends_with("| t-hub-agent --codex-tap"));
    }

    #[test]
    fn session_home_is_codex_sessions() {
        let a = CodexHarness;
        assert!(a.session_home().ends_with(".codex/sessions"));
    }
}
