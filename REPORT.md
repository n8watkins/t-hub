# Crew report: P1 UI batch (branch `ui-batch`)

Ship: t-hub-app. Cut from origin/main 56e7da1 (v0.3.56). No build, no version bump, no /no-mistakes.
Live app (pid 7632) untouched - verified entirely via typecheck + vitest.

## PR

Single PR from `ui-batch` -> `main` with four cleanly-scoped commits (one per item).

**Why one PR, not three:** the captain assigned a single worktree/branch (`ui-batch`) and asked to push
that branch. `Tile.tsx` is shared by items 1 and 2 (the header button and the header ctx% gate), so
splitting into separate branch-PRs would mean cross-branch churn on the same file for no benefit. Each
commit is independently scoped and revertible, which is the separability the brief asked for.

PR: https://github.com/n8watkins/t-hub/pull/44

## Verification

- `tsc --noEmit`: clean (exit 0).
- `vitest run`: 26 files, **280 tests pass**. New/changed suites:
  - `store/sessionContext.test.ts` - /clear reset + stale-guard (2 new).
  - `lib/captainAttribution.test.ts` - captain/orchestrator/non-captain resolution (6 new).
  - `lib/notify.test.ts` - strict chime policy (4 new).
  - `store/workspace.restart.test.ts` - same-slot placement + kill + failure path (3 new).
- No Rust touched → cargo not required.
- The `restartTerminal failed Error: spawn boom` line in the vitest output is an expected
  `console.error` from the spawn-failure test, not a failure.

## Per-item status

### Item 1 - Kill + restart tile button - DONE (commit ab0ec2e)
Confirm-guarded control in the tile header, next to the refresh (⟳), using a distinct counter-clockwise
`RotateCcw` glyph. One click (after confirm) spawns a FRESH tmux session rooted at the same folder, drops
it into this tile's exact tab + slot, then kills the old session (process tree).

- **Approach - frontend store action, not a backend control command (justified):** the tile lifecycle
  (spawn / kill / placement) is already frontend-managed via the Tauri `spawnTerminal` / `killTerminal`
  IPCs and the workspace store's `order` arrays - the exact path the "+" new-tile and "×" close already
  use. `restartTerminal` composes those battle-tested primitives and does the id swap **in place at the
  old slot** atomically in the store, so placement is exact and there is no new tmux logic. A backend
  `restart_terminal` would instead have to round-trip through the control registry, whose frontend
  adoption **rebuilds the whole tab order from the snapshot** (append-only placement, no index insert),
  giving *less* precise slot control - and could not be unit-tested with the control socket wedged.
- **Confirm-guarded always** (unlike ×, which skips the dialog when idle): a stray click here kills a
  live agent and restarts it, so it must never fire on a misclick.

### Item 2 - Header ctx% as a setting (default OFF) + stale-after-/clear fix - DONE (commit 5534bba)
- New `showHeaderContextMeter` setting (default **false**), wired through the settings store exactly like
  the other display toggles, surfaced in **Settings > General > Tiles**. The tile header `<ContextMeter>`
  (Tile.tsx) is now gated on it. The sidebar captain rows (`CaptainsList.tsx`) still show context
  regardless - see the note on the "bottom bar" below.
- **Stale-after-/clear bug - verified and fixed.** Data-flow trace: statusline JSON -> `context_used_pct`
  (status.rs, `None` when there is no `context_window` block) -> `status://snapshot` -> the
  `sessionContext` store keyed by `th_<id>`. The store's `ingest` **dropped** any snapshot with no
  `contextUsedPct` (`if (snap.contextUsedPct == null) return s`), so once a session was cleared - `/clear`
  empties the window, so the statusline stops reporting a `context_window` - the old, now-wrong number
  stayed pinned until the next turn repopulated it. Fix: a **fresher** snapshot for a session we already
  track that carries no context now **resets** (deletes) that session's reading. Guarded on `prev.ts`
  so a stale/out-of-order empty frame can't clobber a newer reading. Test:
  `resets a session's reading when a fresher snapshot reports no context (/clear)`.
- **"Bottom Claude-config bar" note:** there is no standalone bottom config bar in the desktop app today
  (it exists only as a mockup in the marketing site `apps/site`). The always-on ctx% surface in the real
  app is the sidebar captain rows (`CaptainsList.tsx`), which I left **untouched** - it shows context
  regardless of the new setting, matching the brief's "keeps showing always" requirement.

### Item 3 - Captain notification attribution (spoken + visual) - DONE (commit 585280b)
New `lib/captainAttribution.ts`: resolve a Claude session id -> its tile (`th_<id>`, via the supervision
reverse index) -> the captains registry, yielding `Captain <ship>` for a pinned captain (its stable
identity: rename > cwd folder > tab name > registry ship slug) or the orchestrator's brand name
("Cortana"), and `null` for a regular session. Threaded into **both** producers the general receives:
- **spoken** (`voiceAnnounce.ts`): `Captain alpha needs your attention` (was `<label> needs your attention`).
- **visual** (`notify.ts`): captain-named titles/bodies (was generic "A session is waiting on your input.").

Non-captain sessions keep their exact prior wording.

### Item 4 - Chime simplification (general's later add) - DONE (commit b0317c0)
Chimes stay **ungated by dictation** (no scribe gating added, per the general). Cut the trigger set to a
strict, attention-worthy minimum mirroring the voice-announce doctrine.

**Before → after chime triggers** (`notify.ts`):

| Trigger | Before | After | Rationale |
|---|---|---|---|
| `needsQuestion` | chime (attention) | **chime** | decision needed |
| `needsPermission` | chime (attention) | **chime** | decision needed |
| `completed` | chime (done) | **chime** | a captain/crew finished |
| `failed` | chime (error) | **chime** | a blocker |
| `rateLimited` | chime (error) | **silent** | transient overlay on a still-working session; auto-continue rides it out; the voice model refuses to speak it |
| terminal `state === "error"` | chime (error) | **removed** | low-level transport edge (often teardown); already covered by the agent `failed` status and the tile's own error indicator |
| non-zero process `exit` | chime (error) | **removed** | fires on any ordinary command returning non-zero (a failing test, a grep with no match, Ctrl-C = 130) - pure noise |

`statusToNotification` exported + `notify.test.ts` pins the strict policy. The now-unused `onExit`/`onState`
subscriptions (and their import) were removed.

## Overlap flagged (voice-gate branch)

Another crew (branch `voice-gate`) is fixing the dictation **gating** of the speak path. I touched two
files in that neighborhood but kept strictly to **content/naming**, never gating semantics:
- `lib/voiceAnnounce.ts` - changed **only** the two text-construction lines (the `text = ...` subject);
  the Scribe hold/flush/debounce gate is untouched.
- `lib/notify.ts` - the chime trim + attribution; not part of the speak path's scribe gate.

Expect a small textual overlap on `voiceAnnounce.ts` if `voice-gate` also edits near the announce line.
