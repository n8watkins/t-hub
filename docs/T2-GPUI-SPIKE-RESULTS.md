# T2 - GPUI spike results (2026-07-01)

> **VERDICT: GPUI passes both §6 gates. Framework decision: GPUI, not Iced.**
> Spike built at rung R1 (minimal gpui app outside Zed); no fallback rungs needed.
> Evidence on disk: `C:\Users\natha\spikes\gpui-spike\` (source, build.log, fps-run*.log, gpu-run*.log, run-run*.log).

## G1 - frame budget: PASS

Variant measured: the full one.
Real `alacritty_terminal` Terms (100x30) fed synthetic styled ANSI (~150 lines/s/grid: truecolor, indexed-256, bold, underline, bg spans, box-drawing) from 16 feeder threads via `vte::ansi::Processor::advance`, painted via gpui `shape_line` + `paint`/`paint_background` per row inside a `canvas` element, driven by `request_animation_frame`.
Deliberately worst-case: every visible row of every grid rebuilt and re-shaped every frame, no damage tracking, whole window repainted.
Machine: 2560x1440 @ 180Hz primary display, Ryzen 7 9700X, RX 7800 XT.

| Condition | 12 grids | 16 grids |
|---|---|---|
| Run 1 (window fully occluded) | avg 177.6, min 159 | avg 144.6, min 82 |
| Run 2 (screenshot-verified visible) | avg 153.2, min 133 | avg 89.3, min 82, max 91 |
| Run 3 (user actively using desktop) | avg 163.5, min 101 | avg 101.1, min 86 |
| Run 4 (+ explicit shaped-line cache) | avg 164.0, min 125 | avg 90.2, min 84 |
| Seconds below 55 fps, all runs, while painting | **0** | **0** |

- 12 grids: 150-180 fps visible (~85-100% of the 180Hz refresh).
- 16 grids: rock-steady ~90 fps = half-refresh vsync quantization (inferred from the precise halving); never below 82 fps anywhere, including while the user actively worked the machine.
- Scene-build CPU: ~3.1ms @ 12 grids, ~4.7-5.9ms @ 16 grids, single main thread.
- GPU 3D-engine utilization: **3.5-7.3%** - the 7800 XT is nearly idle; the ceiling is CPU-side per-frame cell iteration in the spike's brute-force renderer, not GPUI's compositor.
- Run 4 finding: an explicit Zed-style shaped-line cache changed nothing - gpui's internal `LineLayoutCache` already absorbs repeat shaping across frames.
- Honest framing: against GPUI's own 8ms/120fps budget, 12 grids pass outright; 16 grids sit at ~10-11ms full-frame on a 180Hz panel (stable, zero drops).
  On a 60-120Hz display both stages are a full lock.
  Only a strict "16 grids at 180 fps" reading fails, and the headroom evidence (4% GPU, brute-force renderer) says a production damage-driven renderer closes the gap.
- Note: fps=0 seconds in run 2 were the window being minimized (gpui suspends painting when iconic); excluded from stats.
  Occlusion-without-minimize does not stop or throttle painting (run 1).

## G2 - adapter: PASS (discrete by default, no forcing needed)

- gpui's own startup log: `Using GPU: AMD Radeon RX 7800 XT` + `Created device with Direct3D 11.1 feature level`.
- Spike's DXGI enumeration: adapter[0] = RX 7800 XT (16176MB), adapter[1] = integrated Radeon Graphics (485MB), adapter[2] = WARP.
- Perf counters (`\GPU Engine(pid_*)\Utilization Percentage`): 100% of the process's samples across all runs are on the discrete card's LUID; zero samples on the integrated LUID.
- Mechanism (vendored 0.2.2 source, `platform/windows/directx_devices.rs::get_adapter`): first D3D11-capable adapter in plain `EnumAdapters` order - no `EnumAdapterByGpuPreference(HIGH_PERFORMANCE)`, no env override.
  It wins here because the display hangs off the discrete card.
  zed #36798 remains real for laptop-style hybrids; the forcing lever there is Windows Settings > Graphics per-app "High performance" (OS-level, works on DXGI apps).

## Buildability outside Zed: PROVEN, and cheap

- **`gpui = "0.2.2"` from crates.io** (not a git dep) + `alacritty_terminal = "0.26.0"` + `vte 0.15.0` + `windows 0.61`, rustc/cargo 1.95.0 MSVC.
- First full release build: exit 0, **165 seconds wall**, 442 crates, zero errors, zero workarounds.
  Leaf rebuilds 3-4s.
- All needed APIs (`canvas`, `shape_line`, `ShapedLine::paint/paint_background`, `request_animation_frame`, `paint_quad`, `TextRun`) exist in 0.2.2.
- This CORRECTS the earlier research claim that GPUI has no published crate: 0.2.2 shipped on crates.io (Oct 2025).
  Caveat: gpui git-main has since split entry points (`gpui_platform::application()`); pin 0.2.2 or budget for that API shift.

## GPL chain note

`cargo tree -i ztracing` and `-i sum_tree` both return "did not match any packages" in the 713-package lockfile.
The zed #55470 GPL-3.0 chain (`sum_tree -> ztracing`) is **absent from published gpui 0.2.2**.
Caveat: name-level evidence only (no full cargo-deny run); consuming gpui from zed git-main could differ.
Moot for licensing intent (T-Hub stays open-source), but nice to have.

## Not tested

4K/high-DPI, multi-window atlases (T10 will cover), long-run memory, gpui git-main.
