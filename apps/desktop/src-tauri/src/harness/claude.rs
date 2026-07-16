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

use super::{
    home_dir, leading_permission_options, provider_arguments, sh_single_quote, Harness,
    HarnessAdapter, HarnessPermissionAttestation, HarnessProcessEvidence, LaunchAttestationError,
    PermMode,
};

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

    fn fresh_argv_with_permissions(&self, prompt: &str, perm: PermMode) -> String {
        let flags = self.permission_map(perm).join(" ");
        match (flags.is_empty(), prompt.is_empty()) {
            (true, true) => "claude".to_string(),
            (true, false) => format!("claude {}", sh_single_quote(prompt)),
            (false, true) => format!("claude {flags}"),
            (false, false) => format!("claude {flags} {}", sh_single_quote(prompt)),
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

    fn attest_permissions(
        &self,
        evidence: &HarnessProcessEvidence,
        expected: PermMode,
    ) -> Result<HarnessPermissionAttestation, LaunchAttestationError> {
        let args = provider_arguments(evidence, Harness::Claude)?;
        let options = leading_permission_options(args)?;
        options.ensure_unique(&["--dangerously-skip-permissions", "--permission-mode"])?;
        let bypass = options.contains("--dangerously-skip-permissions");
        let permission_mode = options.value("--permission-mode");

        let valid = match expected {
            PermMode::BypassPermissions => {
                if bypass && permission_mode.is_some() {
                    return Err(LaunchAttestationError::ConflictingPermission);
                }
                if !bypass {
                    return Err(if permission_mode.is_some() {
                        LaunchAttestationError::WrongPermission
                    } else {
                        LaunchAttestationError::MissingPermission
                    });
                }
                true
            }
            PermMode::AcceptEdits => {
                if bypass {
                    return Err(LaunchAttestationError::ConflictingPermission);
                }
                permission_mode == Some("acceptEdits")
            }
            PermMode::Default => {
                if bypass || permission_mode.is_some() {
                    return Err(LaunchAttestationError::ConflictingPermission);
                }
                true
            }
        };
        if !valid {
            return Err(if permission_mode.is_some() {
                LaunchAttestationError::WrongPermission
            } else {
                LaunchAttestationError::MissingPermission
            });
        }
        Ok(HarnessPermissionAttestation {
            provider: Harness::Claude,
            permission: expected,
        })
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
    fn fresh_argv_applies_explicit_unrestricted_permissions() {
        let a = ClaudeHarness;
        assert_eq!(
            a.fresh_argv_with_permissions("do the thing", PermMode::BypassPermissions),
            "claude --dangerously-skip-permissions 'do the thing'"
        );
    }

    #[test]
    fn resume_argv_exact_string() {
        // No-regression lock: the recall path emits exactly this for a Claude row.
        let a = ClaudeHarness;
        assert_eq!(a.resume_argv("abc-123"), "claude --resume 'abc-123'");
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

    #[test]
    fn permission_attestation_accepts_only_direct_claude_bypass() {
        let adapter = ClaudeHarness;
        let correct = HarnessProcessEvidence::test(
            2,
            20,
            &["claude", "--dangerously-skip-permissions", "work"],
        );
        assert_eq!(
            adapter
                .attest_permissions(&correct, PermMode::BypassPermissions)
                .unwrap()
                .permission,
            PermMode::BypassPermissions
        );

        let missing = HarnessProcessEvidence::test(2, 20, &["claude", "work"]);
        assert_eq!(
            adapter
                .attest_permissions(&missing, PermMode::BypassPermissions)
                .unwrap_err(),
            LaunchAttestationError::MissingPermission
        );
        let wrong = HarnessProcessEvidence::test(
            2,
            20,
            &["claude", "--permission-mode", "acceptEdits", "work"],
        );
        assert_eq!(
            adapter
                .attest_permissions(&wrong, PermMode::BypassPermissions)
                .unwrap_err(),
            LaunchAttestationError::WrongPermission
        );
    }

    #[test]
    fn permission_attestation_rejects_claude_wrappers_conflicts_and_wrong_provider() {
        let adapter = ClaudeHarness;
        let wrapper = HarnessProcessEvidence::test(
            3,
            30,
            &["node", "/opt/wrapper", "--dangerously-skip-permissions"],
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
                "claude",
                "--dangerously-skip-permissions",
                "--permission-mode",
                "acceptEdits",
                "work",
            ],
        );
        assert_eq!(
            adapter
                .attest_permissions(&conflicting, PermMode::BypassPermissions)
                .unwrap_err(),
            LaunchAttestationError::ConflictingPermission
        );
        let codex = HarnessProcessEvidence::test(
            3,
            30,
            &[
                "codex",
                "--dangerously-bypass-approvals-and-sandbox",
                "work",
            ],
        );
        assert_eq!(
            adapter
                .attest_permissions(&codex, PermMode::BypassPermissions)
                .unwrap_err(),
            LaunchAttestationError::WrongProvider
        );
    }

    #[test]
    fn permission_attestation_rejects_claude_missing_values_and_repeated_flags() {
        let adapter = ClaudeHarness;
        for evidence in [
            HarnessProcessEvidence::test(4, 40, &["claude", "--permission-mode"]),
            HarnessProcessEvidence::test(4, 40, &["claude", "--permission-mode="]),
        ] {
            assert_eq!(
                adapter
                    .attest_permissions(&evidence, PermMode::BypassPermissions)
                    .unwrap_err(),
                LaunchAttestationError::MissingPermission
            );
        }
        for evidence in [
            HarnessProcessEvidence::test(
                4,
                40,
                &[
                    "claude",
                    "--dangerously-skip-permissions",
                    "--dangerously-skip-permissions",
                ],
            ),
            HarnessProcessEvidence::test(
                4,
                40,
                &[
                    "claude",
                    "--permission-mode=acceptEdits",
                    "--permission-mode",
                    "default",
                ],
            ),
        ] {
            assert_eq!(
                adapter
                    .attest_permissions(&evidence, PermMode::BypassPermissions)
                    .unwrap_err(),
                LaunchAttestationError::ConflictingPermission
            );
        }
    }
}
