# T7 Font Catalogue - gpui 0.2.2 at terminal fidelity

What gpui 0.2.2 gets right and wrong for terminal text, measured with the T7 torture screen, plus the design decisions the findings forced.
Companion to the §5 T7 entry in [NATIVE-PIVOT-EXECUTION.md](./NATIVE-PIVOT-EXECUTION.md).

## Method

- The fixture: `apps/native/src/font/torture.rs`, a 15-section deterministic ANSI byte stream (rulers, box drawing light/heavy/double/dashed/rounded/diagonal, block elements, a Powerline prompt, CJK alignment fences, combining marks incl. zalgo/Thai/Arabic/Devanagari, an emoji suite, ligature bait, truecolor ramps, gamma pairs, a mixed grand finale).
- The harness: `cargo run --release --bin font-torture` opens offline tiles (no server, no PTY), one per configured font (`THN_TORTURE_FONTS="Cascadia Mono:13,Cascadia Code:13:lig"` is the default); `--emit` writes the same bytes to stdout for any other terminal.
- Runs: WSLg/X11 from WSL (Linux text stack, fontconfig + Noto), then the acceptance run on Windows (DirectWrite, real Cascadia Mono/Code, Segoe UI Emoji), then the same bytes in Windows Terminal on the same monitor.
- Screenshots: [T7-torture-native.png](./T7-torture-native.png) (native, both tiles, Windows) and [T7-torture-windows-terminal.png](./T7-torture-windows-terminal.png) (Windows Terminal, same content).

## What gpui 0.2.2 gets right

- **Shaping and OpenType features work at terminal scale.**
  `shape_line` + `ShapedLine::paint` with per-run fonts renders crisp DirectWrite text; the internal `LineLayoutCache` really does dedupe repeated shaping (§1.5 held through T5's damage benchmarks and survives T7's many-small-segments pattern).
  `FontFeatures` reaches the shaper: with Cascadia Code and `calt` on, `-> != === =>` ligate; `FontFeatures::disable_ligatures()` turns it off per tile.
  Crucially the ligated forms keep the summed monospace advance, so ligatures stay on the grid.
- **Color emoji come for free via fallback.**
  Glyph runs carry `is_emoji` and route to `window.paint_emoji`; DirectWrite system fallback finds Segoe UI Emoji without configuration, and `Font.fallbacks` lets us prepend explicit families (missing families are skipped gracefully and the system chain is always appended - verified in gpui's `direct_write.rs`).
  😀 🚀 ⭐ ✅ 👍 render in color inside terminal rows.
- **Combining marks compose.**
  `e` + U+0301 shapes to é with zero advance; zalgo stacks, Thai vowel/tone marks and Devanagari matras all attach to their base cell.
- **Gamma-corrected text blending.**
  The Windows shaders apply DirectWrite-style contrast/alpha correction (`alpha_correction.hlsl`, `gamma_ratios`); white-on-black vs black-on-white pairs look balanced and match Windows Terminal side by side.
  Our `Rgb -> Rgba -> Hsla` conversion is a pure float HSL transform of sRGB components (no gamma munging), so SGR truecolor survives the round trip; the 64-step fg+bg ramps render smoothly with no banding beyond the source quantization.
- **Quad alpha blending in sRGB space.**
  Translucent overlay quads (cursor, selection, search highlights, shade sprites) blend predictably and match the webview's feel.

## What gpui gets wrong (and what T7 does about it)

- **No per-cell placement: one shaped line drifts off the grid.**
  T5 shaped each row as a single line, so every cell's x position depended on prior glyph advances; any fallback glyph (CJK, emoji, symbols) whose advance is not exactly `2 * cell_w` (or `1 *`) pushes the rest of the row off the grid that T6's selection/cursor/URL math assumes.
  On WSL with a missing family this drifted by ~80% per CJK char.
  **Fix: `font::segment_cells`** splits each row into independently positioned segments painted at `col * cell_w`: plain-ASCII runs stay together (exact by construction, ligature-friendly), while wide, non-ASCII and mark-bearing cells become single-cell segments so fallback advance error stays inside the cell's own box.
  After the fix, `|你好世界|一二三四|` aligns pixel-exact over `|abcdefgh|12345678|` on Windows.
- **`TextRun.background_color` paints under the shaped advance, not the grid cell.**
  A fallback glyph with an off-grid advance would leave bg slivers.
  **Fix:** SGR backgrounds are painted as explicit merged grid-column quads before any text; runs never carry a background.
- **Private Use Area Powerline glyphs have no fallback.**
  DirectWrite system fallback cannot map U+E0Bx, so font-based rendering is tofu unless a patched Nerd Font is installed.
  This is exactly why §1.5 froze the sprite decision; Windows Terminal itself draws E0B0-E0B3 procedurally and shows ♦ placeholders for E0A0/E0A1/E0A2.
  **Fix: `font::sprites`** draws box drawing U+2500-U+257F (arm-table light/heavy, dashes, doubles with correct junction joins, rounded arcs, diagonals), blocks U+2580-U+259F (eighths, halves, quadrants, alpha shades) and Powerline E0B0-E0B7 + E0A0 (branch) + E0A2 (padlock) as `paint_quad` rects.
  The native tile consequently renders branch/padlock where Windows Terminal shows placeholders.
- **Silent family substitution.**
  If the configured family is not installed, the platform substitutes without telling us; on WSL the substitute was a PROPORTIONAL face and ASCII runs left the grid (isolated cells stayed on it, making the drift obvious).
  The primary family must be an installed monospace font - the standard terminal contract; "Cascadia Mono" is present on the Windows box (§1.5).
  A metrics-based sanity check (warn when the space and M advances differ) is a cheap future guard.

## Behaviors inherited from alacritty (match Alacritty, differ from Windows Terminal)

These are grid-semantics choices made by `alacritty_terminal`, not gpui; they are faithful to the cell model T6's math depends on.

- **Skin-tone modifiers occupy their own cell**: `👍🏽` renders as 👍 plus a separate modifier swatch cell; WT grapheme-clusters them into one glyph.
- **ZWJ sequences render as constituent emoji**: `👨‍👩‍👧` is three heads in six columns; WT renders one composed family glyph.
- **Flags render as regional-indicator letter pairs** (Windows ships no flag emoji at all); WT shows the same letters, clustered tighter.
- **VS16 presentation is fallback-driven**: both `❤` and `❤️` come back color, because DirectWrite fallback resolves U+2764 to Segoe UI Emoji regardless of variation selector; WT keeps the bare form monochrome.
- **No contextual shaping across cells**: Arabic renders isolated forms in logical order (no bidi), Devanagari conjuncts split per cell - standard cell-grid terminal behavior.

## Known gaps (native, after T7)

- SGR dim (SGR 2) is not represented in `SnapCell`, so dim text renders at full brightness (WT dims). Small follow-up in `term/`.
- U+E0A1 (Powerline LN) and E0B8+ (the extra triangle/corner set) are not sprites and fall to font fallback (tofu on Windows).
- Sprite diagonals, rounded arcs and chevrons are stroked as runs of small squares (quad-only approximation); at 13px they read clean, but WT's true AA lines are slightly smoother. A `PathBuilder` upgrade is possible if it ever matters.
- Underline under a wide fallback glyph follows the glyph advance rather than the 2-cell width (runs carry the underline; cell-exact underlines would need the bg-quad treatment).
- Sprite cells ignore the underline flag entirely (rare combination).
- Cell backgrounds and sprites are not snapped to device pixels; at fractional display scales a 1px line can land on a half-pixel and soften. Not visible at scale 1.0.

## Per-tile font config (T7b plumbing shipped)

- `font::FontSpec { family, size, ligatures }`; `line_height` derives at the T5 ratio (16/13, rounded).
- `TileSpec.font: Option<FontSpec>` carries a per-tile override; unset tiles use `THN_FONT="Family[:size[:lig|nolig]]"` or the Cascadia Mono 13 default.
- Each tile resolves its own gpui normal/bold `Font` (emoji/symbol fallback families attached, `calt` dropped when ligatures are off) and probes its own `Metrics` (cell width measured by shaping 80 Ms at the tile's size).
- Tile geometry, input hit-testing, wheel math and overlays all use the tile's own metrics, so mixed-font/mixed-size grids work today (the torture screen runs Mono 13 next to Code 13 in one window).
- What T8's chrome needs to expose: a per-tile `FontSpec` in its layout model plus a settings surface; the render layer is done.

## Reproduce

```sh
# native window, two tiles (Mono vs Code+lig)
cargo run --release --bin font-torture
# custom tiles
THN_TORTURE_FONTS="Cascadia Mono:13,JetBrains Mono:14:nolig" cargo run --release --bin font-torture
# same bytes in any other terminal
cargo run --release --bin font-torture -- --emit > /tmp/t7.ans && cat /tmp/t7.ans
```
