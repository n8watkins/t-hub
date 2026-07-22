# Package 2 Windows E2E Requirements

Package 2 changes visible tile-header behavior, so source and Chromium tests are necessary but do not complete the package gate.
The final candidate must be exercised in the installed Windows Dev build through WebView2 before Package 2 can be called packaged or live-verified.

## Source gate

Run `pnpm test`, `pnpm typecheck`, `pnpm build`, and `pnpm test:browser` from `apps/desktop` against the exact candidate commit.
The browser suite must retain coverage for 1, 2, 4, 8, and 16 tiles, adjacent container-query widths, keyboard activation, repeated resizing, and emulated display scales.
Successful browser runs retain deterministic grid, breakpoint, keyboard, resize, and scale screenshots under `apps/desktop/test-results/`, which remains ignored by Git.
The source evidence must record the candidate commit and the passing command output.

## Installed Dev-build matrix

Build and install the exact Dev artifact only after the candidate commit has passed independent review and installation has been authorized.
Record the source commit, artifact path, artifact SHA-256, installed target, and installation time.
Do not substitute a Vite browser run, mocked component, or Production installation for this check.

Exercise the following matrix in the installed Windows Dev build:

- Display scaling at 100%, 125%, 150%, and 200%.
- One, two, four, eight, and sixteen visible tiles.
- A long terminal title and a long current-work name.
- A long linked-worktree branch chip with a dirty-state indicator.
- Captain and Cortana identity markers.
- Working, attention, completed, error, and idle status indicators where practical.
- The context meter enabled with a populated value.
- Window widths that place tiles on both sides of the full-to-short and short-to-icon breakpoints.
- A narrow width that forces the two-row header and the smallest practical tile width.

At every matrix point, Terminal, Files, and Preview must remain visible as nonzero controls inside the tile header.
No header control, identity marker, branch chip, status indicator, or context meter may overlap another visible control.
Long text may truncate or disappear according to the responsive priority rules, but it must not displace the three panel controls.
Preview must use the Eye icon and expose exactly `Preview` in visible text, its tooltip, and its accessible name.

## Keyboard and accessibility gate

Use Tab to reach Terminal, Files, and Preview in order at full-label and icon-only widths.
Activate the controls with Enter and Space and confirm the selected panel and `aria-pressed` state change together.
Confirm a screen reader announces Terminal, Files, and Preview without depending on the hidden responsive label spans.
Confirm the icon-only controls retain their full tooltips.

## Resize and performance gate

Resize repeatedly across every responsive tier while terminal output is active.
The header must not flicker between density modes, enter a resize loop, or make panel controls disappear.
Capture browser diagnostics or a performance trace that shows no `ResizeObserver loop` error and no repeated React DOM mutation caused solely by width changes.

## Evidence retention

Retain one screenshot for every tile-count and display-scale combination, plus focused screenshots at each breakpoint tier.
Retain the accessibility observations and resize-performance result with the artifact metadata.
Package 2 remains `packaged-unproven` until every installed-build requirement above is recorded against the exact candidate artifact.
