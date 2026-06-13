# TermHub

TermHub is a **terminal-first command center for running and supervising many persistent coding-agent (Claude Code) sessions at once**. The V1 target is a single personal setup: Windows 11 + WSL2 Ubuntu + zsh, with an adapter-based core so other terminal agents can be added later.

## Contents

- **[PRD.md](./PRD.md)** — TermHub Detailed Product Requirements Document, v1.0 (June 13, 2026). Source-of-truth product and implementation specification.
- **[REVIEW.md](./REVIEW.md)** — Technical review of the PRD: strengths, ranked risks, and Claude Code integration assumptions to validate.

The PRD was authored in Microsoft Word and converted to Markdown (headings, tables, lists, code blocks, and reference links preserved) so it can be reviewed and planned against directly in the repo.

## For reviewers

Start with `PRD.md`. Sections are organized as:

- **1–4** — Vision, scope, principles, terminology, locked product decisions
- **5–7** — User experience, core workflows, functional requirements
- **8–10** — State model, architecture, research-backed implementation decisions
- **11–14** — Privacy, performance, roadmap, acceptance criteria
- **15–18** — Testing, risks, out-of-scope items, source references
