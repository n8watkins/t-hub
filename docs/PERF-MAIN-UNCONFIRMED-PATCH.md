# Performance Patch Accidentally Made In `main`

Date: 2026-06-27

## Context

This patch was made in the main worktree by mistake during what should have been
a review-only pass. It should be treated as unconfirmed work. Another agent should
review and either move it to a separate worktree/branch or discard it from `main`.

Main worktree at the time of this note:

- Path: `/home/natkins/projects/tools/t-hub/t-hub-app`
- Branch: `main`
- My authored files in this patch:
  - `apps/desktop/src/components/Terminal.tsx`
  - `apps/desktop/src/components/TerminalPool.tsx`
  - `apps/desktop/src/lib/windowMaximized.ts`

There are also other uncommitted files in `main` that I did not author during
this pass. Do not attribute those to this patch without checking their diffs.

## What Changed

### 1. Background terminal output throttling

Files:

- `apps/desktop/src/components/Terminal.tsx`
- `apps/desktop/src/components/TerminalPool.tsx`

Intent:

The terminal pool keeps every terminal mounted with `visible={true}`, even for
inactive workspace tabs, non-terminal panel views, and covered fullscreen tiles.
Before this patch, every mounted terminal used the same foreground `requestAnimationFrame`
flush path for output. That means background terminals could still decode, URL-scan,
activity-bump, and `term.write()` frequently while the user is switching pages,
maximizing/restoring, or using another tab.

Patch shape:

- Added a `foreground?: boolean` prop to `TerminalView`.
- `TerminalPool` computes which pooled terminal ids are actually on the active
  visible surface and passes `foreground={foregroundIds.has(id)}`.
- Foreground terminals still flush output on `requestAnimationFrame`.
- Background terminals flush on a slower timer:
  - `250ms` while the document is visible.
  - `1000ms` while the document is hidden.
  - immediate timer flush once queued background bytes exceed `512 KiB`.
- Added a lightweight `th-terminal-foreground` custom event so the existing
  output listener closure can react when a terminal moves foreground/background
  without remounting the xterm instance.

Expected benefit:

Less DOM/xterm work from inactive terminals during page/tab switches, minimize,
maximize/restore, and window-hidden periods, while preserving the no-remount
persistent terminal pool.

Review risks:

- Verify no output is lost when a background terminal becomes foreground.
- Verify localhost URL chips still appear when a hidden terminal prints a dev URL.
- Verify long-running noisy background commands do not accumulate excessive memory.
- Verify fresh spawn attach/seed ordering is unchanged.
- Verify switching tabs does not display stale terminal content for more than the
  intended background flush delay.

### 2. Coalesced maximized-state polling

File:

- `apps/desktop/src/lib/windowMaximized.ts`

Intent:

The shared maximized-state tracker already reduced duplicate subscriptions, but
it still called `isMaximized()` on every resize event. During resize/maximize
bursts, that can stack IPC calls.

Patch shape:

- Added rAF coalescing around resize-triggered `isMaximized()` refresh.
- Added an in-flight/pending guard so only one IPC call is active at a time, and
  a resize burst schedules at most one follow-up refresh.

Expected benefit:

Lower IPC churn during resize/maximize/restore without changing the external
`useWindowMaximized()` API.

Review risks:

- Verify the maximize/restore glyph updates after:
  - custom maximize button click,
  - Windows snap/maximize action,
  - keyboard maximize/restore,
  - double-click/titlebar behavior if supported.

## Verification Already Run

- `pnpm --filter t-hub-desktop typecheck` passed after the main patch.
- `pnpm --filter t-hub-desktop typecheck` was rerun after this note was added
  and passed again.

A confirming agent should still rerun it after moving/reconstructing the patch:

```sh
pnpm --filter t-hub-desktop typecheck
```

Runtime verification is still needed on Windows/WebView2. This patch is specifically
about perceived interaction performance, so typecheck is not enough.

## Suggested Confirming Tests

1. Start the desktop app with several active terminals across multiple workspace tabs.
2. Run a noisy command in an inactive tab, then interact with the active tab.
3. Switch back to the inactive tab and confirm output catches up without reload.
4. Open Files/Preview/Dev panel views and confirm covered terminals do not steal
   pointer input or render over the panel.
5. Minimize, restore, maximize, and restore the window while terminals are producing
   output.
6. Confirm titlebar maximize glyph remains correct after each maximize/restore path.
7. Confirm URL detection still creates Preview chips for dev-server URLs printed
   while a terminal is backgrounded.
