# Repository Agent Instructions

- Commit each verified logical change separately with a clear message.
- Never add an agent name as a commit co-author.
- Do not manually edit generated changelogs or other files marked as generated.
- Put each full sentence on its own physical line when writing or substantially editing long Markdown documents.
- Reproduce user-visible bugs through the closest practical end-to-end flow before implementing a fix.
- Preserve `.lavish/` and `docs/DECK-AGENTS-DESIGN.md` unless the General explicitly authorizes changing them.

## CLI development

When creating or modifying CLI commands, follow `docs/cli-contract.md`.

- Keep core business logic separate from command parsing and output formatting.
- Preserve stable `--json` contracts and treat machine-readable changes as API changes.
- Do not introduce interactive prompts.
- Use structured errors and the documented exit codes.
- Keep stdout clean and parseable in JSON mode.
- Update CLI contract tests whenever machine-readable behavior changes.
