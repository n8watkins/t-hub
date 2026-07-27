# `th` — the T-Hub CLI

`th` is T-Hub's canonical agent+human command line.
It is a thin **client** of the app's control socket: the same
NDJSON-over-loopback-TCP protocol the MCP server speaks.
That is the whole point - it lets the captain and crewmates drive T-Hub from
anywhere a shell reaches: inside Claude Code (via Bash), a raw terminal,
Tailscale SSH, a phone over SSH, or a script - with **no MCP runtime required**.

```
$ th
T-Hub  ·  12 live terminals  ·  0 tabs

TERMINALS
  052ccbb2  live   th_052ccbb2
  ...

Next
  th read 052ccbb2  view that terminal's recent output
  th status         FR-012 status across all sessions
  th health         WSL host snapshot
  th tabs           list workspace tabs
```

## Install

`th` must be reachable as a bare `th` from any terminal.
Pick one of the two paths below; both leave `th` on your `PATH`.

### 1. `cargo install` (recommended)

Installs the release binary into `~/.cargo/bin` (already on `PATH` for any Rust
toolchain):

```sh
cargo install --path apps/cli
```

Re-run the same command to upgrade after pulling new code.

### 2. Build + symlink into `~/.local/bin`

If you would rather not use `cargo install`:

```sh
cargo build --release --manifest-path apps/cli/Cargo.toml
ln -sf "$PWD/apps/cli/target/release/th" ~/.local/bin/th
```

(Ensure `~/.local/bin` is on your `PATH`.)

Either way, verify:

```sh
th --version
th        # fleet home view
```

> This install is **standalone**. It is deliberately **not** wired into the
> desktop Tauri build - `th` ships and versions on its own (crate version
> `0.2.0`), independent of the app.

## How `th` relates to the MCP server

Both `th` and `t-hub-mcp` are **thin clients of the same control socket**.
Neither embeds app state; both discover the running app and forward commands by
name over the loopback channel:

```
  Claude (MCP client)          human / agent shell
        │ JSON-RPC/stdio              │ argv
        ▼                             ▼
   t-hub-mcp  ─────┐          ┌─────  th (this crate)
                   │          │
                   ▼          ▼
        loopback TCP · NDJSON · {token, command, args}
                   │
                   ▼
        T-Hub app control listener (src/control.rs)
          authenticates the per-launch token,
          dispatches by command name (PRD §11.2 tiers)
```

- `t-hub-mcp` exists so **Claude** can call these commands as MCP tools.
- `th` exists so a **human or an agent in a plain shell** can call the same
  commands directly.
- The app's permission tiers (read / organization / process-changing) apply
  identically to both; `th` surfaces the app's gating verbatim and never tries
  to bypass it.

## Discovery

`th` locates the app exactly like the MCP server does, in order:

1. `$T_HUB_CONTROL_ADDR` + `$T_HUB_CONTROL_TOKEN` (pin the endpoint directly).
2. `$T_HUB_CONTROL_FILE` (a non-default handshake path).
3. `~/.t-hub/control.json` (the handshake file the running app writes).

## Commands

| Command | Maps to | Notes |
| --- | --- | --- |
| `th` | `list_terminals` + `list_tabs` | fleet home view + runnable next-hints |
| `th ls` | `list_terminals` | `--all` to lift the 20-row cap |
| `th read <session>` | `read_terminal` | `--history N` for scrollback |
| `th status [<session>]` | `list_terminals`+`get_status`, or `get_status`+`supervision_tree` | fleet table, or one session + its tree |
| `th send <session> <text…>` | `send_text` | `--no-enter` to skip the trailing Enter |
| `th spawn <cwd>` | `spawn_terminal` | opens a generic user shell; supervisor agent assignments must use `th agents start` |
| `th worktree ls [repoRoot]` | local git + `list_terminals` + `list_worktrees` | lifecycle table: BRANCH / DIRTY / MERGED / LEASED; JSON includes backend cleanup reservations |
| `th worktree new <repoRoot> <branch>` | `create_worktree` | opens a generic shell; use `th agents start` afterward for a supervised assignment; `--path P` defaults under `.claude/worktrees/`; `--tab T` names the tab |
| `th worktree rm <repoRoot> <path>` | `remove_worktree` | currently suspended by the backend safety gate |
| `th worktree prune [repoRoot]` | local git + `list_terminals` | reporting only; `--yes` is refused; `--force` includes unmerged candidates in the report |
| `th tabs` | `list_tabs` | |
| `th health` | `wsl_health` | |
| `th events` | `__subscribe_events` | streams the event bus until Ctrl-C |
| `th agents start` | `start_agent` | starts one durable Codex or Claude agent in an existing Project checkout; `--admission-purpose ordinary\|fleet-admin\|ship-admin\|recovery` selects the authorized capacity/capability class without exposing a raw capability override |
| `th agents list` | `list_agents` | bounded agent summaries by Captain or Project |
| `th agents show` | `get_agent` | full durable agent record |
| `th agents checkpoint` | `agent_checkpoint` | appends a bounded resume checkpoint |
| `th agents events` | `agent_events` | reads lifecycle and checkpoint events after a cursor |

## Worktree lifecycle (treehouse-style)

The captain's staffing loop creates a worktree per crew task and reaps it after merge.
`th worktree ls / prune / new` automate that lifecycle.

Git facts (worktrees, dirtiness, merge state) are read **locally** - `th` runs `git` directly, since the repo is on this filesystem.
The control socket is consulted for **lease** data and backend-owned Cargo cleanup reservations.
Lease discovery is layered: `list_terminals` cwds first, then pane paths straight off the `t-hub` tmux socket (`$T_HUB_TMUX_SOCKET` to override) for older app builds or when the app is down.
Reservation discovery has no local fallback, so it is available only while the backend can answer `list_worktrees`.
A session leases the *deepest* worktree containing its pane's current path, so a crew session inside `.claude/worktrees/x` does not also lease the repo root above it.
Note the granularity: `pane_current_path` tracks where the pane currently *is*, so a session that `cd`-ed elsewhere temporarily drops its lease.

### `th worktree ls [repoRoot]`

One row per worktree: PATH, BRANCH, DIRTY (uncommitted change count), MERGED (branch fully merged into origin's default branch), LEASED (the live session id rooted there).
`repoRoot` defaults to the current directory's repo.
`--json` adds the no-force prune verdict (`prunable` + `reason`) and nullable `retirementReservation` data per worktree.
See [the CLI contract](../../docs/cli-contract.md) for the guarded Cargo cleanup workflow that owns those reservations.

### `th worktree prune [repoRoot]`

Reports worktrees that appear **merged AND clean AND unleased** without removing worktrees, closing sessions, or deleting branches.
Execution remains suspended until the authoritative worktree safety service can prove ownership, retention, and exact-target removability.
Reporting rules, in doctrine order:

- output is always a dry-run report, and `--yes` is refused before endpoint discovery;
- a **dirty** worktree is never removed, no flag overrides that;
- a **leased** worktree is hands-off, no flag overrides that;
- an **unmerged** branch is included as a forced candidate only with `--force`, and the plan prints exactly which commits would be lost (`REAP*` rows);
- every skip prints the protecting reason;
- if lease state cannot be verified (no control socket *and* no tmux), prune **refuses to report a plan** rather than guess.

### `th worktree new`

`th worktree new` always delegates creation to the backend and does not recycle an existing worktree.
Without `--path`, it derives a new path under `<repoRoot>/.claude/worktrees/` from the branch leaf.
An active Cargo cleanup reservation covering the requested path blocks creation before Git or runtime state is created.

## Agent ergonomics

- **`--json`** on read commands emits a stable envelope you can parse:
  `{ ok, command, data, error }`. `data` is the full, sorted set (never capped);
  on failure `data` is `null` and `error` is `{ code, kind, message }`.
- **Exit codes** are a stable taxonomy - branch on `$?`:

  | code | meaning |
  | --- | --- |
  | `0` | success |
  | `2` | usage / bad arguments |
  | `3` | app not running (discovery or connect failed) |
  | `4` | operation failed (the app answered `ok:false`, or a local git step failed) |
  | `5` | gated / permission-denied (e.g. the spawn gate) |
  | `6` | control protocol-version mismatch |

  A gated action or an app-down case never exits `0`.
- **Bounded + deterministic**: human lists are sorted by id and capped at 20
  (`--all` or `--json` for the rest), so repeated calls diff cleanly and stay
  cheap in tokens.
- **Pipe-clean**: when stdout is not a terminal, `th` drops column padding but
  keeps the terse structured form; it never emits color, spinners, or cursor
  escapes.
