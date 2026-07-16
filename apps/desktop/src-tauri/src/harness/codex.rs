//! Codex harness adapter (Phase-1 seam).
//!
//! Builds the interactive launch strings, the headless crew-turn pipeline, and
//! the permission-flag map for OpenAI Codex (`codex-cli`), all verified against
//! the installed Codex 0.144.4. Everything here is a pure string builder; the
//! actual lifecycle producer that consumes `exec_turn_argv` is the PR-B
//! `t-hub-agent --codex-tap` mode.
//!
//! Steer/wake contract (HIGH-1): a Codex `exec` crew's pane is a plain login
//! shell BETWEEN turns, so a Codex crew is steered ONLY by injecting the SHELL
//! COMMAND that `exec_turn_argv(prompt, Some(thread_id), ...)` builds - never by
//! sending prose (which the shell would execute on a bypass-provisioned
//! workspace). See the crew-brief doctrine block.

use std::path::PathBuf;

use super::{
    home_dir, leading_permission_options, provider_arguments, sh_single_quote, Harness,
    HarnessAdapter, HarnessPermissionAttestation, HarnessProcessEvidence, LaunchAttestationError,
    PermMode,
};

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
        // `--skip-git-repo-check` belongs to `codex exec`, not the interactive
        // `codex` command. Append it independently of the sandbox posture so both
        // fresh and resumed turns can run from newly-created worktrees.
        let mut flags = self.permission_map(perm);
        flags.push("--skip-git-repo-check".to_string());
        let flags = flags.join(" ");
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
            // Crew default. The long bypass flag (never the `--yolo` alias, which
            // is absent from the installed help) skips all approvals + sandboxing.
            PermMode::BypassPermissions => {
                vec!["--dangerously-bypass-approvals-and-sandbox".to_string()]
            }
            // Approximate acceptEdits. Documented gap: no exact analog, and
            // network is off by default under workspace-write (so no `git push`).
            PermMode::AcceptEdits => vec!["--sandbox".to_string(), "workspace-write".to_string()],
            PermMode::Default => vec!["--sandbox".to_string(), "read-only".to_string()],
        }
    }

    fn attest_permissions(
        &self,
        evidence: &HarnessProcessEvidence,
        expected: PermMode,
    ) -> Result<HarnessPermissionAttestation, LaunchAttestationError> {
        let args = provider_arguments(evidence, Harness::Codex)?;
        let options = leading_permission_options(args);
        let bypass = options
            .iter()
            .any(|(flag, _)| *flag == "--dangerously-bypass-approvals-and-sandbox");
        let sandbox = options
            .iter()
            .find(|(flag, _)| *flag == "--sandbox")
            .and_then(|(_, value)| *value);
        let conflicting = options
            .iter()
            .any(|(flag, _)| matches!(*flag, "--yolo" | "--full-auto" | "--ask-for-approval"));

        let valid = match expected {
            PermMode::BypassPermissions => {
                if bypass && (sandbox.is_some() || conflicting) {
                    return Err(LaunchAttestationError::ConflictingPermission);
                }
                if !bypass {
                    return Err(if sandbox.is_some() || conflicting {
                        LaunchAttestationError::WrongPermission
                    } else {
                        LaunchAttestationError::MissingPermission
                    });
                }
                true
            }
            PermMode::AcceptEdits => {
                if bypass || conflicting {
                    return Err(LaunchAttestationError::ConflictingPermission);
                }
                sandbox == Some("workspace-write")
            }
            PermMode::Default => {
                if bypass || conflicting {
                    return Err(LaunchAttestationError::ConflictingPermission);
                }
                sandbox == Some("read-only")
            }
        };
        if !valid {
            return Err(if sandbox.is_some() {
                LaunchAttestationError::WrongPermission
            } else {
                LaunchAttestationError::MissingPermission
            });
        }
        Ok(HarnessPermissionAttestation {
            provider: Harness::Codex,
            permission: expected,
        })
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
            "codex --dangerously-bypass-approvals-and-sandbox 'build it'"
        );
        assert!(!a
            .fresh_argv_with_permissions("build it", PermMode::BypassPermissions)
            .contains("--skip-git-repo-check"));
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
            vec!["--dangerously-bypass-approvals-and-sandbox"]
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
    fn exec_turn_argv_keeps_repo_bypass_independent_of_permissions() {
        let a = CodexHarness;
        assert_eq!(
            a.exec_turn_argv("do work", None, PermMode::AcceptEdits),
            "codex exec --json --sandbox workspace-write --skip-git-repo-check 'do work' | t-hub-agent --codex-tap"
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

    #[test]
    fn permission_attestation_accepts_only_direct_codex_bypass() {
        let adapter = CodexHarness;
        let correct = HarnessProcessEvidence::test(
            2,
            20,
            &[
                "node",
                "/opt/codex",
                "--dangerously-bypass-approvals-and-sandbox",
                "work",
            ],
        );
        assert_eq!(
            adapter
                .attest_permissions(&correct, PermMode::BypassPermissions)
                .unwrap()
                .permission,
            PermMode::BypassPermissions
        );

        let missing = HarnessProcessEvidence::test(2, 20, &["codex", "work"]);
        assert_eq!(
            adapter
                .attest_permissions(&missing, PermMode::BypassPermissions)
                .unwrap_err(),
            LaunchAttestationError::MissingPermission
        );
        let wrong =
            HarnessProcessEvidence::test(2, 20, &["codex", "--sandbox", "workspace-write", "work"]);
        assert_eq!(
            adapter
                .attest_permissions(&wrong, PermMode::BypassPermissions)
                .unwrap_err(),
            LaunchAttestationError::WrongPermission
        );
    }

    #[test]
    fn permission_attestation_rejects_codex_wrappers_conflicts_and_wrong_provider() {
        let adapter = CodexHarness;
        let wrapper = HarnessProcessEvidence::test(
            3,
            30,
            &[
                "node",
                "/opt/wrapper",
                "--dangerously-bypass-approvals-and-sandbox",
            ],
        );
        assert_eq!(
            adapter
                .attest_permissions(&wrapper, PermMode::BypassPermissions)
                .unwrap_err(),
            LaunchAttestationError::WrapperObscured
        );
        let conflicting = HarnessProcessEvidence::test(
            3,
            30,
            &[
                "codex",
                "--dangerously-bypass-approvals-and-sandbox",
                "--sandbox",
                "read-only",
                "work",
            ],
        );
        assert_eq!(
            adapter
                .attest_permissions(&conflicting, PermMode::BypassPermissions)
                .unwrap_err(),
            LaunchAttestationError::ConflictingPermission
        );
        let claude = HarnessProcessEvidence::test(
            3,
            30,
            &["claude", "--dangerously-skip-permissions", "work"],
        );
        assert_eq!(
            adapter
                .attest_permissions(&claude, PermMode::BypassPermissions)
                .unwrap_err(),
            LaunchAttestationError::WrongProvider
        );
    }
}
