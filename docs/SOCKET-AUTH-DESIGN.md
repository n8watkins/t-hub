# Control-Socket Authorization Design

Status: DESIGN - awaiting captain + general review. Do NOT implement until the go is given.
Author: crew `socket-gate` (ship t-hub-native).
Scope: server-side authorization tier, fleet spawn budget, audit trail, and migration for the T-Hub control socket.

---

## 1. Threat model

### What the socket is

The control socket (`apps/desktop/src-tauri/src/control.rs`) is a newline-delimited-JSON (NDJSON) request/response channel.
It binds `127.0.0.1:0` (ephemeral loopback port) by default, with an opt-in Tailscale remote bind (`T_HUB_BIND_TAILSCALE=1` / `T_HUB_CONTROL_BIND`) gated OFF by default (`control.rs:1137`, `1152-1208`).
Every session in the fleet - crews, captains, the orchestrator, the MCP server, and the probe scripts - reaches the app through this one socket.
It is the substrate the whole org model runs on: spawning crews, typing into them, and killing them all flow through here.

### The one credential today

A single per-launch UUID token (`ControlContext.token`, `control.rs:1034`) gates every command.
It is minted by `control::persistent_key()` (`control.rs:1089`) or overridden by `T_HUB_CONTROL_TOKEN` (`lib.rs:256`), and published to `~/.t-hub/control.json` (mode 0600 on unix, `control.rs:1113-1127`).
Authentication is a single constant-time compare in `dispatch_authenticated()` (`control.rs:1825`): token matches -> `dispatch()` runs the command; token fails -> `unauthorized: bad control token`.

There is exactly one authorization state: **you have the token, or you do not.**
A holder of the token can `spawn_terminal`, `send_text`, `send_keys`, and `close_terminal` (`control.rs:1918-1921`) with no server-side confirmation, no budget, and no rate limit.

### The critical gap

`confirmationRequired` is a **client-side MCP annotation only** (`t-hub-mcp/src/tools.rs:84`).
It decorates the tool description a well-behaved MCP client shows; it is never checked by the server.
Any raw-socket caller that read the token from `~/.t-hub/control.json` bypasses the annotation entirely and gets full spawn/type/kill power.
The response bodies claim `"audited": true` (`control.rs:3216-3235`, `3269`, `3305`, `3343`) but **no audit sink exists** - the flag is aspirational.

### Adversary and the attack we must stop

The realistic adversary is not an outsider without the token.
It is a **legitimate, permissionless token-holder induced by prompt injection**.
The orchestrator (and crews) routinely read untrusted content - web pages, terminal output, git diffs, tool results - any of which can carry an injection payload.
A payload that says "spawn 200 sessions" or "close every terminal" would, today, execute fleet-wide with no human checkpoint and no ceiling, because the token-holder is trusted absolutely.

Two distinct harms:

1. **Blast radius / runaway** - unbounded `spawn_terminal` (resource exhaustion, fork-bomb-by-agent) or unbounded `close_terminal` (fleet wipe). No cap exists.
2. **Privilege breadth** - every session that can read (monitoring, a crew tailing logs) also holds full kill power, because there is only one token. The injection surface and the dangerous capability are held by the same credential.

### The hard constraint

Legitimate autonomous orchestration MUST keep working.
Captains spawn crews and `send_text` to them without a human in the loop; the general spawns captains the same way (see `scripts/probes/t1_lib.py`, the MCP `spawn_terminal`/`send_text` tools).
Any gate that prompts a human on every spawn breaks the product.
The org model's normal fan-out is bursty by design (span ~3-6 children per level), so the gate must tolerate short bursts of ~6-8 spawns.

### Accepted limitation (state it plainly)

This is a **same-user** trust boundary.
An attacker who already has arbitrary code execution as the user can read any token file, read the env of any process, or drive tmux directly - no socket gate stops that, and this design does not claim to.
The goal is narrower and achievable: **bound the blast radius of injection-driven commands, and enforce least privilege so the read surface does not carry kill power.**

---

## 2. Current architecture (verified map)

| Concern | Where | Notes |
| --- | --- | --- |
| Bind | `control.rs:1137` | `TcpListener::bind("127.0.0.1:0")`; opt-in Tailscale bind `1152-1208`. |
| IP gate | `control.rs:1421-1425` | `is_allowed_peer()` - loopback + Tailscale CGNAT; silent close otherwise. |
| Token mint | `control.rs:1089` / `lib.rs:256` | Persistent UUID or `T_HUB_CONTROL_TOKEN` override. |
| Token publish | `control.rs:1113-1127` | `~/.t-hub/control.json`, 0600, fields `addr`/`token`/`pid`/`protocol_version`. |
| Connection lifecycle | `handle_conn()` `control.rs:1415` | accept -> IP gate -> per-request NDJSON loop; per-conn `peer_is_loopback` set at `1438`. |
| **Single auth choke-point** | `dispatch_authenticated()` `control.rs:1825` | constant-time token check, then `dispatch()`. Every non-attach/non-subscribe command flows through here. |
| Command router | `dispatch()` `control.rs:1842-1935` | string `match`; commands are already grouped by tier in-source. |
| Process-changing handlers | `control.rs:3115` (spawn), `3257` (send_text), `3282` (send_keys), `3325` (close) | act on `th_*` tmux sessions the app owns. |
| Conn cap | `control.rs:1379-1385` | `MAX_CONNS = 256`; the ONLY existing limit, and it is per-connection, not per-spawn. |
| Client: probes | `scripts/probes/t1_lib.py` | reads `~/.t-hub/control.json`, one connect+send+read per call, no retry loop. |
| Client: MCP | `t-hub-mcp/src/control_client.rs:139` | fresh TCP per `tools/call`; honors `T_HUB_CONTROL_ADDR`/`T_HUB_CONTROL_TOKEN` env (`:65-66`). |
| Client tiers (advisory) | `t-hub-mcp/src/tools.rs:29-45` | `Read` / `Organization` / `ProcessChanging` / `Theme`; `confirmationRequired` set only for `ProcessChanging` (`:84`). Never enforced server-side. |

Two facts drive the whole design:

- **There is one server-side choke-point** (`dispatch_authenticated`, `control.rs:1825`) that every command passes through. The gate and the budget belong here (or in a helper it calls) so they cannot drift per-command.
- **The env-injection capability channel already exists on both ends.** The server honors `T_HUB_CONTROL_TOKEN` (`lib.rs:256`) and the MCP client honors it too (`control_client.rs:66`). We do not need new plumbing to hand a specific token to a specific spawned session - we reuse this.

---

## 3. Recommended gate: capability-scoped tokens that descend the spawn tree

### The idea

Replace "one token = all power" with **two minted capabilities**, and let the dangerous one flow only down the legitimate spawn tree.

- `read_token` - authorizes the **Read** tier (and, by policy, **Organization** mutations that are non-destructive; see tiering table). Published in `~/.t-hub/control.json` for ambient discovery.
- `control_token` - authorizes **everything**, including the **ProcessChanging** tier (spawn / send_text / send_keys / close). **Not** published to the ambient discovery file once hardening is on; delivered only via `T_HUB_CONTROL_TOKEN` to sessions the app (or a captain, via the app) deliberately elevates.

The server maps the presented token to a capability set and checks it against the command's tier at the single choke-point.
Because the capability descends through env injection, **capability follows the legitimate spawn tree**: the app is the root of trust, it injects `control_token` into the orchestrator/captain sessions it spawns, and those can spawn crews. A crew that only got `read_token` (or an agent that merely scraped `control.json`) can observe the fleet but cannot spawn, type into, or kill anything.

This is the cleanest option of the three the brief floated, and it composes the strengths of the others:

- It is a **capability tier on the token** (option A) - simple to reason about, table-driven, one compare becomes two.
- It gives a real **caller-identity notion** (option C) at the granularity that matters: "was this session handed the control capability by the app?" - without needing per-caller PKI, because the existing `T_HUB_CONTROL_TOKEN` env channel already carries it down the tree.
- It leaves room for an **elevated-confirm handshake** (option B) as a narrow Phase 3 layer for the destructive-beyond-budget subset, without putting a human in the loop on the common path.

### Why not the alternatives as the primary gate

- **Elevated-confirm on every process-changing command** breaks autonomous orchestration - captains spawn crews with no human present. Rejected as the primary gate; kept as a narrow Phase 3 escalation.
- **Per-caller cryptographic identity** (mTLS / signed requests / per-captain keypairs) is the theoretically cleanest attribution, but it is heavy, needs a key-distribution story the product does not have, and does not stop the induced-orchestrator case any better than budget + tiering do. Deferred as a future extension (Phase 3+: per-captain tokens minted at `claim_captain` to authenticate `spawnedBy`).

### Enforcement shape

At `dispatch_authenticated()` (`control.rs:1825`), replace the single compare with a capability resolution:

```
fn resolve_caps(ctx, presented_token) -> Option<CapSet>   // None => unauthorized
    // constant-time compare against EACH known token; the matched one yields its CapSet.
    // compare all candidates (do not early-return) so timing does not leak which matched.

fn required_tier(command) -> Tier                          // table-driven, single source of truth
```

Flow:

1. `resolve_caps` - no match -> `unauthorized: bad control token` (unchanged message, no leak).
2. `required_tier(command)` - the command's tier from one authoritative table (see below).
3. If the caller's `CapSet` does not include the required tier -> refuse with a specific, machine-readable error (see §5 error convention), and **audit the refusal**.
4. Otherwise run the fleet-budget check (§4 - only for ProcessChanging), then `dispatch()`.

`required_tier` must be a single table keyed by command name, derived from the same grouping already present in `dispatch()` (`control.rs:1843-1934`) and mirrored from the MCP `Tier` enum (`tools.rs:29`).
Sharing one table between the MCP tool list and the server gate prevents the annotation-vs-enforcement drift that caused this whole problem.

### Constant-time note

Preserve `ct_token_eq` semantics (`control.rs:1811`): compare the presented token against every known capability token, accumulate matches without early return, so timing does not reveal which (if any) token matched.

### Tier -> capability mapping (proposed)

| Tier | Commands (examples) | `read_token` | `control_token` |
| --- | --- | --- | --- |
| Read | `list_terminals`, `get_status`, `read_terminal`, `search_files`, `list_tabs`, `supervision_tree` | allow | allow |
| Organization | `focus_session`, `move_tile`, `rename_tab`, `new_tab`, `close_tab`, `open_file`, `claim_captain` | allow (policy: non-destructive UI/registry mutations) | allow |
| Organization-destructive | `create_worktree`, `remove_worktree`, `archive_recent_project` | **deny** | allow |
| ProcessChanging | `spawn_terminal`, `send_text`, `send_keys`, `close_terminal` | **deny** | allow (+ budget, §4) |

`create_worktree` spawns and mutates the filesystem, so it is treated as control-tier despite living in the Organization block today (`control.rs:1896`).
The exact Read-vs-Organization split for the `read_token` is a policy call for the general; the safe default is `read_token` = Read tier only, and Organization requires `control_token`. Recommend starting strict (read = Read only) and loosening if a real read-only consumer needs a UI mutation.

---

## 4. Fleet spawn budget and rate limit (additive, ships first)

This layer is **identity-independent** - it bounds blast radius even for a fully-trusted `control_token` holder that has been injection-hijacked.
It is safe to ship before the auth tier because it only *refuses past a ceiling*; within normal orchestration limits it is invisible.

### Governor

A shared `SpawnGovernor` (an `Arc` on `ControlContext`, like the existing `ACTIVE_CONNS` atomic at `control.rs:1379`), consulted in `dispatch_authenticated` for ProcessChanging commands:

- **Max concurrent sessions** - hard cap on live `th_*` sessions the server owns. Derive the count from the authoritative session/tab registry (not a naive counter, which drifts when sessions die without `close_terminal`). Reconcile on each spawn. Default `64`, env-overridable `T_HUB_MAX_SESSIONS`.
- **Spawn rate** - token-bucket: sustained `20/min`, burst `8`. Burst >= 8 covers a captain fanning out 6 crew plus slack; sustained 20/min covers multi-level fan-out without letting a runaway loop win. Env `T_HUB_SPAWN_RATE` / `T_HUB_SPAWN_BURST`.
- **Hard ceiling** - an absolute concurrent stop that no override can exceed (e.g. `128`). Defense against a mis-set env override.
- **Destructive rate** - throttle `close_terminal` (and kill-style `send_keys` like `C-c`) to e.g. `15/min` burst `10`, so an injection cannot wipe the fleet in one tight loop. A crew closing its own handful of tiles stays well under.

Accounting: a successful `spawn_terminal` increments concurrent; a `close_terminal` OR an observed session exit decrements. Because concurrent is read from the registry, a leaked/crashed session self-corrects on the next reconcile.

### Refusal

Past any limit, refuse with a clear, machine-readable error (§5) and audit it (§6) - never silently drop, so a legitimate caller that hit a transient burst limit sees exactly why and can back off.
Clients do not retry-loop (`t1_lib.py`, `control_client.rs` are one-shot), so a refusal surfaces straight to the agent as a tool error - which is the desired "you are being rate-limited, stop" signal.

Optionally emit a control event on refusal so a live monitor (the captain overlay) sees fleet-budget pressure in real time.

---

## 5. Error convention

Reuse the existing `ControlResponse::err` shape (`{"ok":false,"error":"..."}`, `control.rs:82-104`) so no client parsing changes.
Make the new refusals greppable and distinct so agents and audits can classify them:

- Authz: `unauthorized: '<command>' requires the control capability (this token is read-only)`
- Concurrent cap: `refused: fleet at concurrent-session cap (<n>/<max>); close sessions or raise T_HUB_MAX_SESSIONS`
- Rate: `refused: spawn rate limit (<rate>/min); retry shortly`
- Hard ceiling: `refused: hard session ceiling (<ceiling>) reached`

Keep the bad-token message byte-for-byte identical to today (`unauthorized: bad control token`) so we do not leak capability structure to an unauthenticated probe.

---

## 6. Audit trail with teeth

The `"audited": true` flag is currently a lie; give it a sink.

- **What**: an append-only JSONL log at `~/.t-hub/audit/control-YYYYMMDD.jsonl` (0600), one line per Organization/Organization-destructive/ProcessChanging command AND per refusal.
- **Fields**: `ts`, `command`, `tier`, `decision` (`allowed` | `refused-authz` | `refused-cap` | `refused-rate` | `refused-ceiling`), `sessionId`/`target`, `spawnedBy`, `peer` (`loopback` | tailnet-ip), `tokenTier` (`read` | `control`), and a **redacted** args summary. For `send_text`, log `text` length + a hash, NOT the literal content (it can carry secrets/prompts). For `send_keys`, log the key names (they are not sensitive and are exactly what we want to see for kill patterns).
- **Teeth, concretely**:
  1. It is the **enforcement input**, not just a record - the governor's counters and the rate buckets are the same data the log captures, so "what the log says happened" and "what the gate allowed" cannot diverge.
  2. **Tamper-evident**: chain each line with a running hash of the previous line, so a truncation or edit is detectable.
  3. **Live signal**: mirror refusals (and optionally spawns) onto the existing event fanout (`control.rs:999`) so the captain overlay can surface fleet pressure and denials as they happen.
- **Where**: write from `dispatch_authenticated` after the decision, so allowed and refused paths both land. Batch/fsync to avoid per-command sync cost; a small buffered writer behind a mutex is enough at fleet command rates.

---

## 7. Migration

The design is deliberately staged so nothing breaks on day one.

### Callers today

- `t1_lib.py` (`scripts/probes/*`) - reads `token` from `control.json`, presents it, one-shot. No retry loop to disturb.
- MCP server (`control_client.rs`) - reads `control.json` `token` or `T_HUB_CONTROL_TOKEN` env, fresh TCP per call.
- The app itself - uses `ctx.token` in-process; unaffected by any file/env change.

### Non-breaking rollout

1. **Phase 1 (budget + audit)**: no token change at all. Existing single token keeps full power. Callers are untouched. Only new behavior is *refusal past a ceiling*, which within normal orchestration is never hit. Ship first.
2. **Phase 2 (tiering, backward-compatible)**: mint `read_token` in addition to the existing token; treat the **existing `token` field in `control.json` as the `control_token`** (full power) for now. Add a new `read_token` field to `control.json`. Every current caller that reads `token` keeps full power - zero breakage. New read-only consumers are pointed at `read_token`.
3. **Phase 2b (least-privilege adoption)**: teach the app to inject `T_HUB_CONTROL_TOKEN=<control_token>` into the sessions it elevates (orchestrator, captains) at spawn time - the env channel already exists on both ends (`lib.rs:256`, `control_client.rs:66`). Crews it spawns for pure work get `read_token` unless the captain elevates them. `spawnedBy` can now be sanity-checked against whether the caller even holds control-tier.
4. **Phase 3 (hardening flip, config-gated, default OFF)**: stop publishing `control_token` to `control.json` (publish only `read_token` there); require the control capability to arrive via env down the spawn tree. This is the step that actually closes the "scrape control.json -> full power" hole, so it is gated behind a config flag and rolled out once Phase 2b adoption is verified across probes, MCP, and the app. Provide a documented migration note for any external script that assumed `control.json.token` was omnipotent.

At every phase, `T_HUB_CONTROL_TOKEN` remains the universal override for test harnesses and the dev proof.

---

## 8. Phased delivery

| Phase | Content | Safety | Breaks anything? |
| --- | --- | --- | --- |
| **1** | Fleet spawn budget + rate/destructive limits + audit-log-with-teeth. | Additive; refuse-past-ceiling only. | No (within normal orchestration bursts). |
| **2** | Capability-scoped tokens; table-driven `required_tier` gate at `dispatch_authenticated`; publish `read_token`; existing token stays full-power. | Backward-compatible. | No. |
| **2b** | App injects `control_token` via env into elevated sessions; new read consumers use `read_token`; audit `tokenTier`. | Opt-in least-privilege. | No. |
| **3** | Config-gated flip: `control.json` publishes read-only; control capability only via the spawn tree. Optional narrow elevated-confirm for destructive-beyond-budget. Optional per-captain identity tokens at `claim_captain`. | The real hardening; gated OFF by default. | Only for callers assuming `control.json.token` is omnipotent - documented + migrated in 2b. |

Ship 1 immediately (it is the blast-radius cap and stands alone).
Land 2 + 2b together so tiering and least-privilege adoption move as one.
Hold 3 behind a flag until adoption is proven.

---

## 9. Risks and mitigations

- **Budget too tight breaks legitimate fan-out.** The org model fans out 3-6 children/level, sometimes several levels near-simultaneously. Mitigation: burst >= 8, sustained 20/min, all env-overridable; log near-limit; size from real captain/general behavior before shipping, and watch the audit log during rollout.
- **Concurrent-count drift** (sessions dying without `close_terminal`). Mitigation: derive concurrent from the authoritative session/tab registry and reconcile on every spawn, never a free-running counter.
- **Timing leak from multi-token compare.** Mitigation: constant-time compare against every candidate token, no early return; derive tier from the matched one.
- **Tier flip breaks probes/external scripts** that assumed `control.json.token` = omnipotent. Mitigation: Phase 3 is config-gated and default OFF; `token` stays full-power through Phase 2; documented migration note; `T_HUB_CONTROL_TOKEN` override always available.
- **Elevated-confirm breaking autonomy** if scoped too broadly. Mitigation: it is Phase 3 and narrow - only destructive-beyond-budget, never the common in-budget spawn path.
- **Same-user ceiling.** None of this defeats an attacker with code-exec as the user (they can read env/process memory of an elevated session). Documented as accepted (§1); the win is bounding injection-driven blast radius and enforcing least privilege on the read surface.
- **Audit log growth / write cost.** Mitigation: daily rotation, buffered/fsync-batched writes, 0600; hash-chain adds one hash per line, negligible at command rates.
- **Two-token complexity in `control.json`.** Mitigation: additive field, documented schema, `read_token` optional so old readers ignore it.

---

## 10. Open questions for the general / captain

1. **Read-vs-Organization split for `read_token`**: should a read-only token be able to `focus_tab` / `move_tile` (UI ergonomics), or is Read-tier only the safe default? Recommend: Read-only to start.
2. **Budget numbers**: are `64` concurrent / `20`-per-min / burst `8` the right shape for the largest fleets the general runs? Need one real large-fan-out example to size against.
3. **Phase 3 confirm**: is a human-in-the-loop escalation for destructive-beyond-budget wanted at all, or is budget + audit sufficient and we skip the confirm handshake entirely?
4. **Remote (Tailscale) tier**: should remote peers be capped to `read_token` regardless of the token they present, as a belt-and-suspenders on the opt-in remote bind?
