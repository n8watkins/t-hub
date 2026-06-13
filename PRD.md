**TERMHUB**

Terminal-first command center for persistent coding-agent sessions

**Detailed Product Requirements Document**

| **Document status** | Source-of-truth product and implementation specification |
| --- | --- |
| **Version** | 1.0 |
| **Date** | June 13, 2026 |
| **Owner** | Nathan Watkins |
| **V1 target** | Personal Windows 11 + WSL2 Ubuntu + zsh environment |

*Local-first. Agent-aware. WSL-native. No IDE bloat.*

# Document map

| **Section** | **Purpose** |
| --- | --- |
| 1-4 | Vision, scope, principles, terminology, and locked product decisions |
| 5-7 | User experience, core workflows, and functional requirements |
| 8-10 | State model, architecture, and research-backed implementation decisions |
| 11-14 | Privacy, performance, roadmap, and acceptance criteria |
| 15-18 | Testing, risks, out-of-scope items, and source references |

|   | **Reading note:** This PRD distinguishes three things that must never be conflated: the visual terminal tile, the live terminal process, and the resumable coding-agent conversation. Their lifecycles overlap, but they are not the same object. |
| --- | --- |

# 1. Executive summary

**TermHub is a full-screen, terminal-first control center for running and supervising many coding-agent sessions at once.** Its first release is optimized for one personal setup: Windows 11, WSL2 Ubuntu, zsh, and Claude Code installed inside WSL. The architecture remains adapter-based so Codex and other terminal agents can be added later without rebuilding the terminal, workspace, persistence, or file systems.

- **Primary experience:** A user-defined workspace tab containing an arbitrary number of resizable terminal tiles. There is no product-level maximum such as 8 or 12.
- **Primary control surface:** A collapsible sidebar that organizes workspace tabs, terminal/agent sessions, attention states, contextual files, usage, and WSL health.
- **Persistence promise:** Closing the UI does not kill attached work. A crash or reboot can reconstruct the prior active setup and resume exact Claude session IDs after user review.
- **Agent awareness:** TermHub knows whether each Claude session is working, waiting, asking a question, completed, failed, rate-limited, detached, or no longer resumable.
- **Just-enough files:** A fast contextual file tree, fuzzy search, Markdown reader, and lightweight editor remove the need to open VS Code or Notepad for small reads and edits.
- **Local-first:** No account, no cloud backend, and no telemetry by default. Secrets and file contents are excluded from logs and MCP responses by default.

|   | **North-star outcome:** Nathan can supervise 6-12 visible Claude terminals, 24 or more live/background terminals, and hundreds or thousands of historical session records without losing track of what is running, what needs attention, where it is working, or how to resume it. |
| --- | --- |

# 2. Product definition

## 2.1 Problem statement

Native terminals and terminal multiplexers can keep processes alive, but they do not provide an opinionated visual operating layer for many coding agents. Existing agent interfaces commonly dedicate a full screen to one project or conversation. That forces excessive tab switching, makes hidden completions easy to miss, and separates terminal work from basic file context, resource health, and recovery controls.

## 2.2 Product vision

**TermHub should feel like an operations cockpit for active software builds, not an IDE and not a chat application.** The terminal remains real and unrestricted. TermHub contributes organization, durable identity, agent state, recovery, contextual files, preview control, and automation around it.

## 2.3 Primary user

- A technically capable builder who is comfortable navigating directories and running commands but does not want to use Neovim or open a full IDE for small file tasks.
- Runs many Claude Code sessions concurrently inside WSL and often has multiple sessions associated with the same repository or directory.
- Uses a large monitor and values simultaneous visibility over one-project-at-a-time focus.
- Needs confidence that UI closure, WSL instability, or a Windows restart will not erase the map of active work.

## 2.4 Product principles

| **Principle** | **Requirement** |
| --- | --- |
| Terminal first | The terminal canvas receives the space. Supporting UI must justify every persistent pixel. |
| No artificial density cap | Tile count is an arbitrary collection. Practical readability and machine resources are the only limits. |
| Identity over inference | Track exact session IDs, process IDs, directories, and worktrees instead of guessing from recent activity. |
| Safe recovery | Never silently resume, duplicate, delete, or fork an agent session when the intended action is ambiguous. |
| Local and inspectable | State is stored locally in understandable records and can be exported or repaired. |
| Agent-agnostic core | Claude is the V1 adapter. Workspace, PTY, persistence, file, preview, and notification systems are generic. |
| Minimal file tooling | Excellent reading, searching, and small edits; no attempt to replace VS Code. |

# 3. Scope and release boundaries

## 3.1 V1 definition

- Personal Windows 11 application targeting one selected WSL2 Ubuntu distribution and the user's zsh environment.
- Claude Code adapter with exact session identity, status hooks, context/usage data, recovery, and session catalog.
- Arbitrary terminal canvases grouped into user-defined workspace tabs.
- tmux-backed process persistence and deterministic reconstruction after WSL/Windows restart.
- Contextual project/worktree file index, search, Markdown read mode, and lightweight editing.
- Local-only settings, logs, and state.

## 3.2 Near-V1 additions

- **V1.1:** Rate-limit scheduling/night mode, because it is central to the desired workflow but should only ship after exact recovery is trusted.
- **V1.5:** Managed Chromium preview, MCP command surface, active-file context injection, and deeper process attribution.

## 3.3 Explicitly out of scope for V1

- A full IDE, extension marketplace, debugger, language-server platform, or VS Code-compatible plugin host.
- Git staging, commits, merge conflict resolution, detailed diff UI, or destructive worktree management.
- Embedded browser content inside the terminal canvas.
- Cloud accounts, team syncing, remote telemetry, or hosted session storage.
- A custom terminal multiplexer replacing tmux.
- Perfect attribution of every subagent process and temporary worktree in the first release.

# 4. Terminology and conceptual model

| **Term** | **Definition** |
| --- | --- |
| Workspace tab | A user-named canvas containing any number of terminal tiles. It has no required semantic meaning. |
| Terminal tile | A visual viewport and thin header that attaches to a terminal session. Moving or hiding it does not move or kill the process. |
| Terminal session | The durable shell/process environment owned by TermHub, backed by a tmux session and represented by a stable TermHub ID. |
| Agent session | A provider-specific resumable conversation, such as a Claude Code session ID. One terminal session may currently have zero or one attached primary agent session. |
| Project root | The repository or directory from which the terminal/agent was launched. |
| Worktree context | The exact current worktree, branch, and current working directory associated with the selected agent session. |
| Active workspace state | The tabs, layouts, live terminal records, attachments, focus state, sidebar state, and recovery policy that should reopen after an interruption. |
| Historical session catalog | All known coding-agent sessions and project records, including closed, detached, resumable, expired, or archived entries. |
| File context | The selected project/worktree file index, current folder, recent files, key files, pins, and open editor file. |
| Preview target | A detected or declared URL associated with a terminal/agent session and, later, a managed browser page. |

|   | **Critical lifecycle rule:** Closing a terminal tile, ending a terminal process, deleting a historical record, and deleting a Claude transcript are four distinct actions. The UI must never treat them as synonyms. |
| --- | --- |

# 5. User experience specification

## 5.1 Full-screen structure

TermHub does not reserve conventional application chrome such as a permanent status bar, toolbar, or duplicated navigation row. The default content is the terminal canvas plus an optional collapsible sidebar. Native Windows window controls may remain available or be integrated into a minimal borderless frame, but they must not consume a product toolbar row.

```
FULL SCREEN
+-- Collapsible sidebar (optional / overlay / rail / expanded)
|   +-- Workspace tabs and session groups
|   +-- Attention states and notifications
|   +-- Contextual file mode
|   +-- Low-priority Claude usage / WSL health
|   +-- Settings
+-- Terminal canvas
    +-- Arbitrary number of terminal tiles
    +-- Auto-grid by default
    +-- Drag, swap, resize, split, zoom, focus
    +-- Tiny conditional tile header
```

## 5.2 Workspace tabs

- Workspace tabs are listed in the sidebar and are the default source of truth.
- Ctrl+Tab and configurable shortcuts cycle tabs and show a temporary switcher overlay.
- An optional top-edge tab strip can be enabled in Settings, but it is not enabled by default.
- A tab may contain 2, 6, 12, or any other number of tiles. No hard maximum is stored in the data model.
- Dragging a tile onto a tab entry moves only the visual placement; the terminal and agent process remain attached and alive.

## 5.3 Canvas layout

- **Default mode:** Responsive auto-grid. New tiles are inserted immediately after the focused tile in reading order, then the grid rebalances deterministically.
- **Manual mode:** Tiles can be resized, swapped, or split. When adding from the file tree in a manually arranged tab, TermHub splits the largest available tile region unless the user targets a specific drop zone.
- **Density controls:** Global terminal zoom, per-tile zoom, fit-to-tile, focus mode, and reset layout.
- **Hidden tabs:** Background processes continue, but terminal rendering and attached PTY clients may be suspended to conserve resources.

## 5.4 Terminal tile

```
status  Appturnity / Auth Refactor  ~/projects/appturnity  [Preview] [Files] [...]
```

- Header height should be visually minimal and scale with UI density settings rather than use a fixed physical size.
- Session label priority: user/custom Claude name, Claude-generated summary, first-prompt summary, then project folder plus a generic suffix. Raw IDs remain hidden in details.
- Preview appears only when a preview target exists or a restartable target was previously known.
- Files selects the terminal and opens the contextual file mode or reader/editor window.
- Status is shown through a subtle animated indicator, outline, badge, tooltip, and optional sound; no large status text is required inside every tile.

## 5.5 Sidebar

| **Area** | **Behavior** |
| --- | --- |
| Workspace/session tree | Tabs containing their active and recently detached terminal sessions. Sessions display provider, name, project, state, context, and attention badges as configured. |
| Attention queue | Questions, failures, and completed main-agent turns. Clicking switches tab, focuses tile, and acknowledges the visual notification. |
| Contextual files | For the selected session, show Files, Search, Recent, Key Files, and Pinned. The file area may replace the middle sidebar content while keeping tab navigation available. |
| Utility area | Low-priority rotating or user-selected view for Claude five-hour/weekly usage and WSL RAM/CPU/swap/health. |
| Settings | Keyboard binds, visual density, sidebar behavior, sounds, themes, recovery defaults, provider adapters, retention warnings, and MCP permissions. |

## 5.6 Notifications

- Default sound events: Claude asks a question, main agent completes a turn, and session fails/exits unexpectedly.
- No default rate-limit sound.
- Notification indicators appear on the affected tile and its sidebar session/tab entry, including when the tile is already visible.
- Per-project and per-event sounds are supported. Desktop notifications are optional and can be limited to when TermHub is unfocused.
- Orchestrator sessions may be classified as waiting on subagents rather than completed when background task signals remain active.

# 6. Core user workflows

## 6.1 First-run onboarding

**1.** Detect installed WSL distributions with `wsl -l -v`; default to Ubuntu and verify it is WSL 2.

**2.** Verify zsh, tmux, Git, and Claude Code inside Ubuntu. Offer to install only TermHub-owned helper components; do not silently alter the user's shell configuration.

**3.** Install the TermHub WSL agent and Claude integration hooks/status bridge.

**4.** Select one or more project roots to index, with the WSL home/projects area suggested.

**5.** Choose terminal font, default canvas density, sidebar mode, and notification defaults.

**6.** Explain Claude transcript retention and optionally offer to increase `cleanupPeriodDays` after explicit approval.

## 6.2 Spawn from the file tree

**1.** User right-clicks any folder and chooses Open terminal / Spin up agent.

**2.** TermHub opens the preset chooser: Claude, Shell, Resume Claude, or Custom command.

**3.** TermHub queries known Claude sessions for that directory and repository worktrees.

**4.** If sessions exist, the dialog shows session name/summary, last activity, context information when available, worktree/branch, resumability, and whether the session is already live in TermHub.

**5.** User may create a new Claude session, resume an inactive exact session, focus an already-live session, or fork an existing session.

**6.** TermHub prevents accidental duplicate resume of the same exact Claude session. Advanced override is hidden behind an explicit warning because messages would interleave into one transcript.

**7.** The tile is inserted after the focused tile and the grid rebalances, or the targeted manual drop region is used.

## 6.3 Create a generic terminal

- Claude preset: start a new Claude session and capture the assigned exact ID through status/hooks.
- Shell preset: open zsh at the selected directory without starting an agent.
- Resume Claude preset: choose an exact existing session or named session; never silently use directory-only continue when multiple candidates exist.
- Custom command preset: run a user-saved command after the shell initializes.

## 6.4 Close, detach, stop, archive, delete

| **Action** | **Default effect** |
| --- | --- |
| Close tile | Detach the visual tile and preserve the tmux process/session record. Move it to Recent/Detached unless configured otherwise. |
| Stop terminal | Terminate the tmux session after confirmation if a process is active. |
| Archive record | Remove from normal active/recent views but retain metadata and provider linkage. |
| Forget TermHub record | Remove TermHub metadata after confirmation; does not delete provider transcript unless separately selected. |
| Delete Claude transcript | Never available to MCP by default. Requires a dedicated destructive flow and explicit confirmation. |

## 6.5 App reopen while WSL/tmux is alive

**1.** Load the last active workspace snapshot from SQLite.

**2.** Query the isolated TermHub tmux server and match sessions by stable TermHub ID.

**3.** Reattach visible terminal tiles; keep hidden-tab sessions detached until needed.

**4.** Restore canvas geometry, zoom, selected tab, selected terminal, sidebar mode, file context, and unread attention states.

## 6.6 Recovery after WSL or Windows restart

**1.** Show a recovery review rather than automatically opening every historical project.

**2.** List only terminal sessions that were part of the last active workspace snapshot or explicitly marked for auto-recovery.

**3.** For each entry show: project/worktree, previous process, exact Claude session, transcript availability, last activity, and selected recovery policy.

**4.** Allow Apply to all, grouped actions, or one-by-one review.

**5.** Possible actions: restore shell; resume exact Claude session; resume and send saved startup prompt; start a new session; skip; archive.

**6.** Write every attempted command and result to the crash/recovery journal.

## 6.7 Historical session catalog

- Display all known projects and agent sessions separately from the active workspace snapshot.
- Support filters for provider, project, worktree, active, resumable, expired, archived, context, and last activity.
- Store cheap metadata for hundreds or thousands of sessions. Full transcript availability is provider-dependent.
- TermHub metadata may remain indefinitely. A Claude session is marked non-resumable if its local transcript has been cleaned up or moved.

## 6.8 File reading and editing

**1.** Selecting a terminal changes file context to the session's exact current worktree and a shallow default tree view.

**2.** File search defaults to the current worktree, with toggles for repository and all registered projects.

**3.** Markdown opens in rendered read mode by default with a Source toggle.

**4.** Editing may open in a separate lightweight TermHub window or a temporary split; both are required, with the separate window implemented first.

**5.** Saving writes directly to WSL through the WSL agent with atomic replace semantics and external-change detection.

## 6.9 Preview

- V1: detect or explicitly register localhost URLs and open them in the configured external browser.
- V1.5: maintain one managed Chrome for Testing/Chromium page per preview target, reload it in the background, and bring the correct page to the foreground only on explicit Preview or an opt-in rule.
- Preview discovery priority: explicit TermHub declaration; Claude/footer link metadata; terminal URL detection; process/port association; saved project setting.

## 6.10 Rate-limit scheduling / night mode

**1.** Detect Claude rate-limit usage and reset timestamp from structured status data. Verified caveat: the statusline `rate_limits.*.resets_at` and `used_percentage` fields are present only for Claude.ai Pro/Max subscriptions and only after the session's first API response. The scheduler must therefore treat the reset time as initially unknown, capture it once a session has made at least one call, and degrade gracefully when the block is absent.

**2.** When a session becomes blocked, show a non-audio overlay/badge asking whether to auto-resume after reset.

**3.** Persist a scheduled recovery action with exact session ID, terminal ID, project/worktree, reset time, safety buffer, and continuation prompt.

**4.** At execution, verify the session is not already live elsewhere, the project was not paused, and the transcript still exists.

**5.** Recreate or attach the terminal, resume the exact session, wait for readiness, then submit the configured continuation message.

**6.** Allow cancel, snooze, apply-to-all, and global night-mode presets.

# 7. Functional requirements

| **ID** | **Area** | **Requirement** |
| --- | --- | --- |
| FR-001 | Canvas | Store and render an arbitrary number of terminal tiles per workspace tab. No hard maximum in schema or UI. |
| FR-002 | Canvas | Provide responsive auto-grid, deterministic insertion, drag/swap, resize, manual splits, global/per-tile zoom, focus mode, and layout reset. |
| FR-003 | Tabs | Create, rename, reorder, duplicate-layout, and delete empty workspace tabs. Move terminal tiles between tabs without process interruption. |
| FR-004 | Terminal | Launch interactive WSL zsh terminals through a PTY and back each TermHub terminal with a stable tmux session. |
| FR-005 | Terminal | Detach hidden terminals while preserving processes and tmux scrollback; reattach and restore display when visible. |
| FR-006 | Agent adapters | Support provider presets through an adapter interface. Claude is the only required V1 provider. |
| FR-007 | Claude | Capture session ID, name/summary, transcript path, cwd, project directory, worktree, context, usage, rate limits, and lifecycle events. |
| FR-008 | Claude | Prevent accidental concurrent attachment to the same exact session ID; offer Focus existing or Fork. |
| FR-009 | Catalog | Maintain active workspace records separately from the historical project/agent-session catalog. |
| FR-010 | Recovery | Autosave workspace and session metadata transactionally after every material state change. |
| FR-011 | Recovery | Provide reviewed reconstruction after WSL/Windows restart with per-session and apply-to-all policies. |
| FR-012 | Status | Represent working, waiting-on-subagents, needs-question, needs-permission, completed, failed, rate-limited, detached, restoring, and expired states. |
| FR-013 | Notifications | Display subtle tile/sidebar indicators and optional event/project-specific sounds and desktop notifications. |
| FR-014 | Files | Persist a project/worktree file index, hydrate it at startup, and update it incrementally through WSL filesystem events. |
| FR-015 | Files | Provide fuzzy basename/path/extension search with current-folder, worktree, repository, and global scopes. |
| FR-016 | Files | Provide shallow tree, Recent, Key Files, and Pinned views and remember per-session/worktree navigation state. |
| FR-017 | Editor | Provide rendered Markdown, read-only source, editable source, and safe .env raw/structured editing in separate window and temporary split. |
| FR-018 | Preview | Register/detect a preview URL and expose a conditional Preview action. Managed Chromium is V1.5. |
| FR-019 | System | Show compact WSL RAM, swap, CPU/load, distro state, TermHub/Claude process count, and warnings in the sidebar utility area. |
| FR-020 | Scheduling | Persist and execute opt-in exact-session continuation after a Claude rate-limit reset, with safety checks and audit logs. |
| FR-021 | MCP | Expose read and safe organization actions by default; require confirmation or explicit settings for destructive/process-changing actions. |
| FR-022 | Settings | Support configurable keyboard binds, sidebar mode, optional top switcher, density, themes, sounds, recovery, retention warning, and adapter settings. |
| FR-023 | Privacy | Operate without an account and without telemetry by default. Never index or emit .env values, file contents, prompts, or transcript content unless specifically requested. |

# 8. State and data model

## 8.1 Core entities

```
WorkspaceTab
  id, name, order, layout_mode, layout_json, zoom_default

TerminalRecord
  id, tab_id, tmux_server, tmux_session, project_id, cwd, shell,
  state, last_seen_at, close_behavior, recovery_policy, custom_command

AgentSessionRecord
  provider, provider_session_id, terminal_id?, project_id, worktree_id?,
  display_name, summary, transcript_path, created_at, last_activity_at,
  context_used_pct, resumability, live_attachment_state, provider_metadata

ProjectRecord
  id, root_path, repo_root, display_name, distro, indexed_at, settings

WorktreeRecord
  id, project_id, path, branch, source, last_verified_at

FileIndexEntry
  project_id, worktree_id, relative_path, kind, extension, modified_at, flags

ScheduledAction
  id, terminal_id, agent_session_id, execute_at, action, prompt, state

EventJournalEntry
  timestamp, source, entity_id, event_type, payload, result
```

## 8.2 Two-track persistence

| **Track** | **What it stores** | **Reopen behavior** |
| --- | --- | --- |
| Active workspace snapshot | The exact UI/session arrangement the user was actively working with: tabs, tiles, live terminal links, focus, sidebar, editors, scheduled actions. | Loaded automatically after normal UI reopen; offered for reviewed reconstruction after WSL/Windows restart. |
| Historical catalog | All known projects and agent sessions, even if their tiles were closed or their transcript expired. | Never floods the active canvas. Available through catalog/search and can spawn/resume a new tile. |

## 8.3 Agent resumability states

| **State** | **Meaning** |
| --- | --- |
| Live in TermHub | Exact provider session is attached to a known running terminal/tmux session. |
| Live externally | Hooks or provider state indicate activity, but TermHub does not own the visible terminal. |
| Resumable | No live process is attached, but the provider transcript/session data exists. |
| Fork recommended | The same exact session is already live; starting another terminal should fork rather than resume. |
| Expired/unavailable | Metadata remains, but required transcript/session data no longer exists. |
| Unknown | Imported metadata cannot yet be verified; user may inspect or attempt recovery. |

# 9. Technical architecture

## 9.1 Recommended stack

| **Layer** | **Technology** | **Reason** |
| --- | --- | --- |
| Desktop shell | Tauri 2 | Small native shell, Rust command layer, multi-window support, scoped permissions, notifications, SQLite, and sidecar support. |
| Frontend | React + TypeScript + Tailwind | Matches the owner's stack and supports a high-density, virtualized interface. |
| UI state | Zustand + TanStack Query | Fast local interaction state plus explicit asynchronous backend/query state. |
| Terminal renderer | xterm.js + Fit + WebGL + Search + Unicode; Serialize where useful | Proven terminal emulation and GPU-backed rendering; avoids writing an emulator. |
| Windows PTY | portable-pty / ConPTY | Interactive PTY control for `wsl.exe`; Tauri shell spawn alone is not a terminal. |
| Durable mux | tmux on isolated `termhub` socket | Keeps shell and Claude alive while UI clients detach; battle-tested and scriptable. |
| WSL control plane | Bundled TermHub Linux agent over persistent stdio NDJSON | Centralizes tmux, files, Git/worktrees, system metrics, and Claude hook events inside WSL. |
| Persistence | SQLite in Windows app data + WSL event journal | Transactional UI state and durable recovery, with event replay when UI was closed. |
| File watcher | notify/inotify inside WSL | Watch Linux project files from the Linux side rather than through `\\wsl$`. |
| Reader/editor | CodeMirror 6 + Markdown renderer | Lightweight normal editing without becoming an IDE. |
| Managed preview | On-demand Playwright sidecar + Chrome for Testing (V1.5) | Reliable page mapping, background reload, tab activation, and isolated browser profile. |
| MCP | Local MCP server backed by the same internal command bus (V1.5) | Claude can organize TermHub without fragile UI automation. |

## 9.2 Process topology

```
Windows 11
  TermHub.exe (Tauri / Rust)
    +-- React WebView: sidebar, canvases, xterm.js, editor windows
    +-- SQLite state and recovery journal
    +-- PTY manager
    |     +-- wsl.exe -> tmux attach (one client per visible tile)
    +-- WSL control bridge
    |     +-- wsl.exe -d Ubuntu -- termhub-agent --stdio
    +-- Optional V1.5 browser sidecar
          +-- Playwright -> isolated Chrome for Testing profile

WSL2 Ubuntu
  tmux server socket: termhub
    +-- one named tmux session per TermHub terminal
  termhub-agent
    +-- tmux/session registry and commands
    +-- file index + inotify watcher
    +-- git/worktree queries
    +-- WSL metrics
    +-- Claude hook/status event ingestion
  Claude Code sessions and transcripts under ~/.claude/
```

## 9.3 Why Tauri, not Electron

Tauri is recommended because most heavy workloads already live inside WSL. The desktop application needs a high-density WebView, native process/PTY control, local persistence, and multiple lightweight windows; it does not need to bundle a full Chromium runtime for every window. Tauri also supports scoped shell permissions, SQLite, notifications, and external sidecars. The managed preview browser is launched separately and only when needed. [R10] [R14] [R18]

## 9.4 Terminal and tmux model

- Use one TermHub-owned tmux session per terminal record, typically with one pane. This makes independent attach/detach, cwd, recovery, and ownership straightforward.
- Run TermHub sessions on an isolated tmux socket (`tmux -L termhub`) so the app does not interfere with the user's existing tmux sessions.
- Each visible tile has one PTY client running `wsl.exe` and attaching to the corresponding tmux session. Hidden tabs can detach their PTY clients while the tmux programs continue.
- On remount, use tmux scrollback/capture plus fresh attachment to restore display. xterm serialization is a display optimization, not the authoritative process state.
- Do not use tmux control mode in V1. It could reduce the number of attached clients later, but it adds a protocol/parser and pane-routing system before the core product is proven.

## 9.5 WSL control agent

A small bundled Linux binary is installed into the selected Ubuntu distribution. The Windows Rust core maintains one long-lived `wsl.exe ... termhub-agent --stdio` bridge using newline-delimited JSON. This avoids depending on WSL localhost networking mode and prevents repeated process startup for file, Git, metrics, and tmux queries.

- The agent owns no UI and can restart independently.
- Claude hooks append events to a WSL-side journal and notify the agent when available. Events remain recoverable if the Windows app is closed.
- The agent performs atomic file reads/writes and returns structured errors.
- Protocol messages are versioned so the Windows app can update the agent safely.

## 9.6 Claude adapter

- Install TermHub hook handlers for SessionStart, SessionEnd, UserPromptSubmit, Stop, StopFailure, PermissionRequest, Notification, SubagentStart/Stop, TaskCreated/Completed, CwdChanged, WorktreeCreate/Remove, and relevant file/config events.
- Install a status bridge that receives Claude's JSON status data and writes the latest session snapshot keyed by exact session ID.
- Use official Agent SDK session listing/info APIs for on-demand import and details, but do not poll them aggressively until their memory behavior is benchmarked. The persistent TermHub index should be driven by hooks plus incremental transcript/file metadata.
- The provider adapter exposes generic operations: discover sessions, start new, resume exact, fork, get status, get context, get rate limits, and verify resumability.

## 9.7 File indexing

- Build the index in WSL to use native Linux paths, Git ignore rules, and inotify.
- Persist compact metadata in SQLite; load it into memory on app startup.
- Ignore `.git`, dependency/build directories, binary blobs, and configurable patterns. Index names and metadata, not file contents.
- Search uses a precomputed normalized path plus basename, extension, folder, and key-file flags.
- Default tree depth is shallow and cached per session/worktree. Folder expansion is UI state, not a full rescan.

## 9.8 Preview control

- V1 opens a detected URL externally and avoids duplicate launches when possible.
- V1.5 launches a dedicated Chrome for Testing profile because modern Chrome requires a non-default data directory for remote debugging.
- Store a stable preview-target ID and browser page/target ID. Reload in the background by default; activate only on explicit user action or opt-in automation.
- The Playwright/Node sidecar is started on demand so it does not add idle memory to the core terminal manager.

# 10. Research-backed implementation decisions

## 10.1 Exact session ID is mandatory

Claude saves sessions continuously and supports exact resume by ID or name. `--continue` resumes the most recent session in the current directory. A directory can contain multiple sessions, and the session picker can widen across worktrees or all projects. Most importantly, Anthropic states that resuming the same session in two terminals without forking causes both terminals' messages to interleave into one transcript. TermHub therefore treats exact provider session ID as the identity and blocks accidental duplicate live attachment. [R1] [R3]

## 10.2 Session history is not indefinite by default

Claude stores local JSONL transcripts under `~/.claude/projects/` and removes them after 30 days by default, configurable through `cleanupPeriodDays`. TermHub can keep its own cheap metadata indefinitely, but exact Claude resumability depends on the original transcript remaining available. The UI must display Resumable versus Expired, and onboarding should offer an explicit longer-retention setting without silently changing Claude configuration. [R1] [R2]

|   | **Privacy implication:** Claude transcripts are plaintext and can contain secrets if an agent reads or prints them. Extending retention increases recovery value and increases local secret exposure. TermHub must explain this tradeoff. |
| --- | --- |

## 10.3 Existing-session discovery

Claude's Agent SDK exposes session enumeration, resume, fork, and a custom session store (`listSessions`, `getSessionInfo`, `getSessionMessages`, `rename`, `tag`). Verified caveat: the SDK metadata surface is thinner than a full catalog — the exact session ID and the message transcript are available, but summary, cwd, branch, and first prompt are **not** returned as session metadata and must be derived by parsing the transcript or by maintaining TermHub's own index. This reinforces the decision to keep a lightweight TermHub index for the normal UI and to use the SDK for import, resume, fork, and verification rather than as the primary metadata source. [R6]

## 10.4 Worktree truth hierarchy

**1.** Claude structured status fields and CwdChanged/worktree hook events.

**2.** `git rev-parse`, `git branch --show-current`, and `git worktree list --porcelain` verification inside WSL.

**3.** Optional TermHub/agent-authored metadata contract for provider-specific edge cases.

A Claude-authored MD file is acceptable as a fallback integration contract, but not as the primary source of truth because it can become stale or be skipped. [R4] [R5]

## 10.5 WSL detection and execution

TermHub should run `wsl -l -v` to discover installed distributions, state, and WSL version, and `wsl --status` / `wsl --version` for diagnostics. Commands should always target the selected distribution explicitly. [R7]

## 10.6 tmux remains the correct V1 persistence layer

tmux sessions can detach while their programs continue and later reattach. Its control mode was specifically designed for graphical terminals such as iTerm2, but V1 should use conventional per-terminal attachment because it is substantially simpler. Control mode is a later optimization if 12 visible clients become a measured bottleneck. [R8] [R9]

## 10.7 Open-file context can be injected safely

Claude's UserPromptSubmit hook can add `additionalContext` before a submitted prompt is processed. TermHub can associate an open file and optional selection with the exact Claude session, then inject only the path/selection metadata. This avoids rewriting CLAUDE.md and avoids automatically copying file or secret contents. [R5]

# 11. Privacy, security, and permissions

## 11.1 Defaults

- No TermHub account.
- No telemetry by default.
- Optional crash-report export is generated locally and reviewed before sharing.
- No file content in indexes, logs, notifications, analytics, or MCP responses by default.
- Environment variable values are masked and excluded from logs and clipboard history.
- All destructive actions require explicit confirmation unless the user separately enables a narrow automation policy.

## 11.2 MCP permission tiers

| **Tier** | **Examples** | **Default** |
| --- | --- | --- |
| Read | List tabs/sessions, status, context, WSL health, file paths, preview targets. | Allowed |
| Organization | Focus session, move tile, rename tab/session, open a file, register a preview. | Allowed with visible audit event |
| Process-changing | Start/resume/stop a terminal, schedule continuation, send terminal input. | Confirmation required |
| Destructive | Delete TermHub record, delete transcript, remove worktree, change broad settings. | Denied unless explicitly enabled; confirmation still required |
| Secret-bearing | Read .env value, file content, transcript messages. | Denied by default and never returned implicitly |

## 11.3 Threat considerations

- A malicious project could emit terminal escape sequences or misleading localhost URLs. Sanitize metadata and require confirmation before opening non-local or unexpected origins.
- Hooks run in repositories and must authenticate to the local TermHub bridge with a per-install secret or restricted local socket/journal permissions.
- Tauri capability scopes should expose only the specific sidecars, URLs, and file locations TermHub needs.
- The managed browser must use an isolated profile rather than the user's personal Chrome profile.
- Recovery prompts and automatic terminal input must be auditable and cancellable.

# 12. Performance and reliability requirements

| **Metric** | **Target** |
| --- | --- |
| Visible terminal target | 12 simultaneous visible xterm.js tiles on the owner's large display with smooth focused typing. |
| Live/background target | 24 live terminal/tmux sessions as a tested target, with no explicit schema limit. |
| Historical scale | Hundreds to thousands of project/agent-session metadata records without UI degradation. |
| Input latency | Focused keypress-to-PTY write should feel immediate; target p95 under 30 ms excluding WSL/application response. |
| Hidden rendering | No continuous xterm/WebGL repaint for hidden workspace tabs. |
| Autosave durability | Material UI/session changes committed within 500 ms; SQLite transactions survive abrupt app termination. |
| Startup | Last workspace shell visible quickly; long indexing and catalog reconciliation continue incrementally. |
| File search | Typical indexed file-name query responds within 100 ms after index hydration. |
| Memory posture | TermHub overhead remains materially lower than the Claude/Node workloads it supervises; preview sidecar remains off until needed. |

## 12.1 Rendering strategy

- Batch PTY output before crossing Rust-to-WebView IPC.
- Use one event stream per terminal with backpressure and bounded queues.
- Mount xterm.js only for visible or imminently visible tiles.
- Use WebGL renderer when available and fall back safely.
- Bound frontend scrollback; rely on tmux capture for older process history.
- Throttle nonfocused visible tiles during extreme output while never delaying focused terminal input.

# 13. Delivery roadmap

| **Release** | **Purpose** | **Included** | **Exit criteria** |
| --- | --- | --- | --- |
| 0.1 - Playable proof | Prove the terminal nucleus. | Tauri shell; xterm; ConPTY/WSL; isolated tmux; arbitrary auto-grid; add/remove/focus; close/reopen UI and reattach. | At least 6 live Claude terminals can be rearranged and survive UI closure without process loss. |
| 0.5 - Personal alpha | Replace the normal terminal for daily multi-agent work. | Workspace tabs; sidebar; exact Claude IDs; duplicate-session guard; hooks/status; context/usage; autosave; recovery review; keyboard navigation; WSL health. | A full workday with 6-12 visible sessions and reliable working/waiting/completed awareness. |
| 1.0 - Daily driver | Remove routine dependence on VS Code/Notepad. | Persistent file index; worktree context; fuzzy search; Recent/Key/Pinned; Markdown reader; separate editor and quick split; sounds; settings; catalog; crash journal. | Files open instantly for the selected worktree; small edits save safely; historical sessions are navigable. |
| 1.1 - Night mode | Resume opted-in work after subscription reset. | Rate-limit overlay; reset scheduler; exact resume; continuation prompts; apply-to-all; cancel/audit safeguards. | A deliberately scheduled session resumes once, at the right time, without duplicating a live session. |
| 1.5 - Automation and preview | Integrate external browser and agent control. | Managed Chromium; background reload; MCP; open-file prompt context; process attribution improvements. | One preview page per project/session is reused reliably; Claude can organize TermHub within permission boundaries. |
| 2.0 - Agent operations | Handle deeper context and parallel structures. | Context threshold/handoff policies; subagent/worktree mapping; provider adapters; API usage dashboards. | TermHub can explain and transition complex parallel agent work without relying on terminal motion. |

|   | **Sequencing note (verified 2026-06-13):** Claude Code already ships `SubagentStart`/`SubagentStop` (each carrying a unique `agent_id`), `TaskCreated`/`TaskCompleted`, and `Elicitation` hooks. The read-only parallel-agent awareness listed under 2.0 — orchestrator→subagent tree, per-subagent state, and waiting-on-subagents classification — therefore has its full event substrate available today and should be pulled forward into 0.5/1.0 as a first-class part of the status model rather than deferred. The deeper 2.0 work (context-threshold handoff policies and automated worktree mapping) remains later. |
| --- | --- |

## 13.1 Estimated implementation sequence

| **Window** | **Focus** |
| --- | --- |
| Days 1-4 | Tauri/xterm/PTY spike; launch Ubuntu zsh; create isolated tmux session; attach/detach/reconnect. |
| Days 5-8 | Arbitrary canvas, responsive grid, terminal lifecycle registry, basic SQLite autosave. |
| Weeks 2-3 | Workspace tabs/sidebar, WSL agent, Claude hooks/status, exact ID capture, duplicate guard, recovery review. |
| Weeks 4-5 | File index/search, worktree context, Markdown reader/editor, historical session catalog. |
| Week 6 | Performance hardening at 12 visible / 24 live, crash testing, notifications, settings. |
| Week 7 | Night-mode scheduler and guarded exact-session continuation. |
| Weeks 8-10 | Managed Chromium and MCP if core reliability gates pass. |

# 14. Acceptance criteria

## 14.1 V0.1

- Create 12 terminal tiles in one workspace tab without a hard-cap warning or schema failure.
- Typing remains responsive in the focused tile while other terminals produce output.
- Move and resize tiles; adding a tile chooses a deterministic position.
- Close TermHub, reopen it, and reattach to all still-running tmux sessions.
- Closing a tile does not kill its process unless Stop is explicitly chosen.

## 14.2 V0.5

- Start two distinct Claude sessions in the same directory and display them as separate exact IDs with human-readable labels.
- Attempt to resume an already-live exact session and receive Focus existing / Fork choices rather than a second unsafe resume.
- Receive and display question, main-turn completed, and failure notifications in the tile and sidebar.
- After WSL shutdown, open recovery review and resume selected exact sessions without opening every catalog record.
- Display Claude context and subscription usage when those fields are available, and degrade gracefully when absent.

## 14.3 V1.0

- Selecting a terminal instantly loads the correct project/worktree file context from the hydrated index.
- Search `md`, `.env`, or a multi-token path query and receive relevant files in under the target latency.
- Open Markdown in rendered mode and edit a config file in a TermHub editor window; save is atomic and visible to the running agent.
- Browse active, detached, resumable, and expired historical Claude sessions across projects.
- The app remains usable with 12 visible, 24 live/background, and at least 1,000 catalog records in the test fixture.

## 14.4 V1.1

- A rate-limited session offers a scheduled resume at the structured reset time with a configurable buffer.
- Restarting TermHub does not lose the schedule.
- The scheduler refuses to run when the same session is already live, transcript is unavailable, or the schedule was cancelled.
- Successful continuation occurs exactly once and appears in the event journal.

# 15. Test strategy

| **Test class** | **Required scenarios** |
| --- | --- |
| Unit | Layout insertion/rebalance; state reducers; session collision checks; recovery policy resolution; file search ranking; URL detection; permission decisions. |
| Rust integration | PTY spawn/resize/input; WSL agent protocol; tmux creation/attach/detach/capture; SQLite migrations; process cleanup. |
| Claude adapter | New session, exact resume, fork, same-directory multiple sessions, hooks missing/late, context absent, transcript cleaned up, rate-limit reset. |
| Crash/recovery | Kill WebView, kill TermHub, `wsl --shutdown`, reboot simulation, corrupted snapshot, missing tmux session, stale PID, partial recovery. |
| Performance | 12 visible with output; 24 live background; rapid tab switching; large monorepo index; 1,000+ session catalog. |
| Security | Malicious terminal escape sequences, unsafe URLs, secret-bearing .env, MCP destructive calls, hook spoofing, path traversal, symlink handling. |
| UX | Keyboard-only navigation, sidebar collapsed/expanded, high-DPI monitor, font zoom, sounds, focus restoration, editor external-change conflict. |

# 16. Risks and mitigations

| **Risk** | **Impact** | **Mitigation** |
| --- | --- | --- |
| PTY/ConPTY edge cases | Broken control keys, resize, Unicode, or EOF handling. | Build a spike first; pin tested portable-pty version; maintain a PTY integration harness and fallback diagnostics. |
| Too many visible WebGL terminals | GPU/memory pressure or sluggish input. | Visibility virtualization, bounded scrollback, batched writes, render throttling, measured soft warnings rather than a hard tile cap. |
| tmux state divergence | TermHub record points to missing or renamed session. | Isolated socket, stable generated names, reconciliation on startup, and an explicit orphan-repair flow. |
| Claude integration changes | Hooks/status schema or CLI flags change. | Versioned provider adapter, capability detection, changelog tests, and graceful fallback to transcript metadata. |
| Session retention | Historical record exists but cannot resume. | Separate metadata/resumability states; warn at onboarding; allow longer cleanup period; future transcript archive adapter. |
| Duplicate exact session | Interleaved transcript and confusing agent behavior. | Live attachment registry, external hook detection, default block, and explicit fork flow. |
| WSL file watcher gaps | Stale index. | Periodic low-priority reconciliation plus watcher; manual Refresh; index health indicator. |
| Automation surprises | Night mode or MCP changes work unexpectedly. | Opt-in per session, confirmation tiers, audit journal, cancellation, and single-execution idempotency keys. |
| Scope creep into IDE | Core terminal manager never becomes reliable. | Gate language servers, Git UI, debugging, and extension systems as non-goals. |

# 17. Locked decisions and remaining non-blockers

## 17.1 Locked

- Product name: TermHub (working name).
- V1 is a personal tool, but the core architecture remains distributable.
- Windows 11 + WSL2 Ubuntu + zsh + Claude Code inside WSL.
- Arbitrary terminal tile count; performance target 12 visible and 24 live/background.
- Auto-grid default with drag, resize, split, and deterministic insertion.
- Sidebar-first tab/session control; no required permanent top tab strip or bottom status bar.
- Both separate editor window and temporary split; separate window first.
- File search defaults to current worktree with broader scope toggles.
- Managed Chromium is V1.5.
- Night mode is V1.1, immediately after core V1 reliability.
- MCP can perform safe reads/organization; process-changing/destructive actions use confirmation tiers.
- No account and no telemetry by default.

## 17.2 Non-blocking choices for implementation

- Final visual theme and iconography.
- Default sound pack and whether completion sound is app-unfocused-only.
- Exact global keyboard defaults, subject to collision testing on Windows.
- Whether the native Windows frame or a minimal borderless frame is the default.
- Initial suggested Claude transcript retention value presented during onboarding.

# 18. Research sources

The following primary documentation informed the implementation and constraints in this PRD. Product decisions that depend on provider behavior should be revalidated against the installed Claude Code version during implementation.

**[R1]** Claude Code - Manage sessions: exact resume, picker scope, local JSONL storage, cleanup behavior, and concurrent same-session warning. [https://code.claude.com/docs/en/sessions](https://code.claude.com/docs/en/sessions)

**[R2]** Claude Code - Data usage and local transcript retention. [https://code.claude.com/docs/en/data-usage](https://code.claude.com/docs/en/data-usage)

**[R3]** Claude Code - CLI reference, including resume/fork/session commands and agent listing. [https://code.claude.com/docs/en/cli-reference](https://code.claude.com/docs/en/cli-reference)

**[R4]** Claude Code - Status line JSON fields for session, context, subscription rate limits, cwd, and worktree. [https://code.claude.com/docs/en/statusline](https://code.claude.com/docs/en/statusline)

**[R5]** Claude Code - Hooks reference, including UserPromptSubmit, CwdChanged, Worktree events, tasks, subagents, stop/failure, and permissions. [https://code.claude.com/docs/en/hooks](https://code.claude.com/docs/en/hooks)

**[R6]** Claude Agent SDK - Session listing, metadata, messages, resume, and custom session-store behavior. [https://code.claude.com/docs/en/agent-sdk/sessions](https://code.claude.com/docs/en/agent-sdk/sessions)

**[R7]** Microsoft WSL - Basic commands for distribution/version discovery and explicit distribution execution. [https://learn.microsoft.com/en-us/windows/wsl/basic-commands](https://learn.microsoft.com/en-us/windows/wsl/basic-commands)

**[R8]** tmux official wiki - Sessions, detach/reattach, and background process behavior. [https://github.com/tmux/tmux/wiki/Getting-Started](https://github.com/tmux/tmux/wiki/Getting-Started)

**[R9]** tmux official wiki - Control mode protocol for graphical terminal integrations. [https://github.com/tmux/tmux/wiki/Control-Mode](https://github.com/tmux/tmux/wiki/Control-Mode)

**[R10]** Tauri 2 - Shell process spawning and capability permissions. [https://v2.tauri.app/plugin/shell/](https://v2.tauri.app/plugin/shell/)

**[R11]** portable-pty - Cross-platform PTY abstraction used by WezTerm. [https://docs.rs/crate/portable-pty/latest](https://docs.rs/crate/portable-pty/latest)

**[R12]** xterm.js - Web terminal renderer and headless/serialization support. [https://github.com/xtermjs/xterm.js/](https://github.com/xtermjs/xterm.js/)

**[R13]** notify-rs - Cross-platform filesystem notifications; in WSL this uses Linux inotify. [https://github.com/notify-rs/notify](https://github.com/notify-rs/notify)

**[R14]** Tauri SQL plugin - SQLite support through sqlx. [https://v2.tauri.app/plugin/sql/](https://v2.tauri.app/plugin/sql/)

**[R15]** Playwright BrowserType - Launching or connecting to Chromium browsers over CDP. [https://playwright.dev/docs/api/class-browsertype](https://playwright.dev/docs/api/class-browsertype)

**[R16]** Chrome for Developers - Remote debugging requires a non-default user data directory in Chrome 136+. [https://developer.chrome.com/blog/remote-debugging-port](https://developer.chrome.com/blog/remote-debugging-port)

**[R17]** Chrome DevTools Protocol - Page reload and bringToFront. [https://chromedevtools.github.io/devtools-protocol/tot/Page/](https://chromedevtools.github.io/devtools-protocol/tot/Page/)

**[R18]** Tauri - Native notification plugin. [https://v2.tauri.app/plugin/notification/](https://v2.tauri.app/plugin/notification/)

**[R19]** Tauri - Bundling a Node.js application as an on-demand sidecar. [https://v2.tauri.app/learn/sidecar-nodejs/](https://v2.tauri.app/learn/sidecar-nodejs/)

|   | **PRD status:** This document contains enough product and implementation detail to begin a technical spike without further product clarification. Any unanswered visual choices should be implemented as settings or deferred until the terminal/session nucleus is proven. |
| --- | --- |
