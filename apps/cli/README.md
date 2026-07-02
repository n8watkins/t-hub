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
> `0.1.0`), independent of the app.

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
| `th spawn <cwd>` | `spawn_terminal` | **gated off** in the running build (exit 5) |
| `th worktree new <repoRoot> <branch>` | `create_worktree` | `--path P` (defaults under `.claude/worktrees/`), `--tab T` |
| `th worktree rm <repoRoot> <path>` | `remove_worktree` | `--force` |
| `th tabs` | `list_tabs` | |
| `th health` | `wsl_health` | |
| `th events` | `__subscribe_events` | streams the event bus until Ctrl-C |

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
  | `4` | server error (the app answered `ok:false`) |
  | `5` | gated / permission-denied (e.g. the spawn gate) |
  | `6` | control protocol-version mismatch |

  A gated action or an app-down case never exits `0`.
- **Bounded + deterministic**: human lists are sorted by id and capped at 20
  (`--all` or `--json` for the rest), so repeated calls diff cleanly and stay
  cheap in tokens.
- **Pipe-clean**: when stdout is not a terminal, `th` drops column padding but
  keeps the terse structured form; it never emits color, spinners, or cursor
  escapes.
