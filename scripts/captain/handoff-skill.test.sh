#!/usr/bin/env bash
# Validate the Handoff skill's cross-harness contract and generated metadata.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"
SKILL="$REPO_ROOT/skills/handoff/SKILL.md"
COMMAND="$REPO_ROOT/skills/handoff/assets/claude-command.md"
VALIDATOR="${SKILL_VALIDATOR:-${CODEX_HOME:-${HOME}/.codex}/skills/.system/skill-creator/scripts/quick_validate.py}"

if [ -f "$VALIDATOR" ]; then
  python3 "$VALIDATOR" "$REPO_ROOT/skills/handoff" >/dev/null
else
  test "$(sed -n '1p' "$SKILL")" = '---'
  grep -Eq '^name: handoff$' "$SKILL"
  grep -Eq '^description: .+' "$SKILL"
  test -f "$REPO_ROOT/skills/handoff/agents/openai.yaml"
fi
grep -Fq 'search tracked files for handoff documents' "$SKILL"
grep -Fq 'Never stage, commit, revert, or rewrite unrelated changes.' "$SKILL"
grep -Fq 'Never infer permission to push from prior pushes.' "$SKILL"
grep -Fq 'Never edit generated files' "$SKILL"
grep -Fq 'use `captain_checkpoint`' "$SKILL"
grep -Fq 'active Powder-backed card' "$SKILL"
grep -Fq 'Never retrieve or pass Powder credentials' "$SKILL"
grep -Fq 'Verification commands and exact outcomes' "$SKILL"
grep -Fq 'Runtime and deployment state observed directly' "$SKILL"
grep -Fq 'External dependencies, credentials, services, approvals, and reachability' "$SKILL"
test "$(head -n 1 "$COMMAND")" = '---'
grep -Fq 'argument-hint:' "$COMMAND"
grep -Fq '~/.claude/skills/handoff/SKILL.md' "$COMMAND"

echo "handoff-skill.test: PASS"
