//! **Procedural glyph sprites** (native-pivot T7).
//!
//! Box-drawing (U+2500-U+257F), block elements (U+2580-U+259F) and the core
//! Powerline glyphs (U+E0B0-U+E0B7, U+E0A0, U+E0A2) are NOT rendered from font
//! glyphs - they are painted as axis-aligned quads over `paint_quad` (the frozen
//! §1.5 decision). Font-based box drawing never fills the cell exactly (gaps
//! between adjacent cells, mismatched line weights across fallback fonts), and
//! the Powerline glyphs live in a Private Use Area no system fallback covers.
//!
//! This module is gpui-free: [`sprite_rects`] turns a char + cell size into plain
//! cell-local rectangles (`SpriteRect`, with an alpha for the shade characters),
//! and the render layer maps them onto `paint_quad` calls. Everything here
//! unit-tests under `--no-default-features`.
//!
//! Diagonals, rounded corners and the Powerline chevrons/半circles are stroked as
//! runs of small squares along the ideal curve (quad-only approximation; slightly
//! soft edges, catalogued in `docs/T7-FONT-CATALOGUE.md`).

/// One cell-local rectangle of a sprite, in pixels relative to the cell's
/// top-left corner. `alpha` scales the cell's foreground color (1.0 for lines,
/// fractional for the ░▒▓ shades).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpriteRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub alpha: f32,
}

/// True when `c` is painted procedurally instead of shaped through a font.
/// Must stay consistent with [`sprite_rects`] (unit-tested).
pub fn is_sprite(c: char) -> bool {
    matches!(c,
        '\u{2500}'..='\u{259F}'          // box drawing + block elements
        | '\u{E0B0}'..='\u{E0B7}'        // powerline triangles/chevrons/semicircles
        | '\u{E0A0}'                     // powerline branch
        | '\u{E0A2}')                    // powerline padlock
}

/// The quads for sprite char `c` in a `w x h` px cell, or `None` when `c` is not
/// a sprite char. Rects are clamped to the cell box.
pub fn sprite_rects(c: char, w: f32, h: f32) -> Option<Vec<SpriteRect>> {
    if !is_sprite(c) || w < 1.0 || h < 1.0 {
        return None;
    }
    let mut g = Painter::new(w, h);
    let (cx, cy, t, d) = (g.cx, g.cy, g.t, g.d);
    let e = t / 2.0;
    match c {
        // -- light/heavy line arms (corners, tees, crosses, half lines) --------
        c if lh_arms(c).is_some() => g.paint_arms(lh_arms(c).unwrap()),

        // -- dashed lines: (horizontal?, dash count, heavy?) -------------------
        '\u{2504}' => g.dashes(true, 3, false),
        '\u{2505}' => g.dashes(true, 3, true),
        '\u{2506}' => g.dashes(false, 3, false),
        '\u{2507}' => g.dashes(false, 3, true),
        '\u{2508}' => g.dashes(true, 4, false),
        '\u{2509}' => g.dashes(true, 4, true),
        '\u{250A}' => g.dashes(false, 4, false),
        '\u{250B}' => g.dashes(false, 4, true),
        '\u{254C}' => g.dashes(true, 2, false),
        '\u{254D}' => g.dashes(true, 2, true),
        '\u{254E}' => g.dashes(false, 2, false),
        '\u{254F}' => g.dashes(false, 2, true),

        // -- double lines (═ ║ and every single/double junction) ---------------
        '\u{2550}' => { g.hline(cy - d, 0.0, w, t); g.hline(cy + d, 0.0, w, t); }
        '\u{2551}' => { g.vline(cx - d, 0.0, h, t); g.vline(cx + d, 0.0, h, t); }
        '\u{2552}' => { g.hline(cy - d, cx - e, w, t); g.hline(cy + d, cx - e, w, t); g.vline(cx, cy - d - e, h, t); }
        '\u{2553}' => { g.hline(cy, cx - d - e, w, t); g.vline(cx - d, cy - e, h, t); g.vline(cx + d, cy - e, h, t); }
        '\u{2554}' => { g.hline(cy - d, cx - d - e, w, t); g.vline(cx - d, cy - d - e, h, t); g.hline(cy + d, cx + d - e, w, t); g.vline(cx + d, cy + d - e, h, t); }
        '\u{2555}' => { g.hline(cy - d, 0.0, cx + e, t); g.hline(cy + d, 0.0, cx + e, t); g.vline(cx, cy - d - e, h, t); }
        '\u{2556}' => { g.hline(cy, 0.0, cx + d + e, t); g.vline(cx - d, cy - e, h, t); g.vline(cx + d, cy - e, h, t); }
        '\u{2557}' => { g.hline(cy - d, 0.0, cx + d + e, t); g.vline(cx + d, cy - d - e, h, t); g.hline(cy + d, 0.0, cx - d + e, t); g.vline(cx - d, cy + d - e, h, t); }
        '\u{2558}' => { g.hline(cy - d, cx - e, w, t); g.hline(cy + d, cx - e, w, t); g.vline(cx, 0.0, cy + d + e, t); }
        '\u{2559}' => { g.hline(cy, cx - d - e, w, t); g.vline(cx - d, 0.0, cy + e, t); g.vline(cx + d, 0.0, cy + e, t); }
        '\u{255A}' => { g.vline(cx - d, 0.0, cy + d + e, t); g.hline(cy + d, cx - d - e, w, t); g.vline(cx + d, 0.0, cy - d + e, t); g.hline(cy - d, cx + d - e, w, t); }
        '\u{255B}' => { g.hline(cy - d, 0.0, cx + e, t); g.hline(cy + d, 0.0, cx + e, t); g.vline(cx, 0.0, cy + d + e, t); }
        '\u{255C}' => { g.hline(cy, 0.0, cx + d + e, t); g.vline(cx - d, 0.0, cy + e, t); g.vline(cx + d, 0.0, cy + e, t); }
        '\u{255D}' => { g.vline(cx + d, 0.0, cy + d + e, t); g.hline(cy + d, 0.0, cx + d + e, t); g.vline(cx - d, 0.0, cy - d + e, t); g.hline(cy - d, 0.0, cx - d + e, t); }
        '\u{255E}' => { g.vline(cx, 0.0, h, t); g.hline(cy - d, cx - e, w, t); g.hline(cy + d, cx - e, w, t); }
        '\u{255F}' => { g.vline(cx - d, 0.0, h, t); g.vline(cx + d, 0.0, h, t); g.hline(cy, cx + d - e, w, t); }
        '\u{2560}' => { g.vline(cx - d, 0.0, h, t); g.vline(cx + d, 0.0, cy - d + e, t); g.hline(cy - d, cx + d - e, w, t); g.vline(cx + d, cy + d - e, h, t); g.hline(cy + d, cx + d - e, w, t); }
        '\u{2561}' => { g.vline(cx, 0.0, h, t); g.hline(cy - d, 0.0, cx + e, t); g.hline(cy + d, 0.0, cx + e, t); }
        '\u{2562}' => { g.vline(cx - d, 0.0, h, t); g.vline(cx + d, 0.0, h, t); g.hline(cy, 0.0, cx - d + e, t); }
        '\u{2563}' => { g.vline(cx + d, 0.0, h, t); g.vline(cx - d, 0.0, cy - d + e, t); g.hline(cy - d, 0.0, cx - d + e, t); g.vline(cx - d, cy + d - e, h, t); g.hline(cy + d, 0.0, cx - d + e, t); }
        '\u{2564}' => { g.hline(cy - d, 0.0, w, t); g.hline(cy + d, 0.0, w, t); g.vline(cx, cy + d - e, h, t); }
        '\u{2565}' => { g.hline(cy, 0.0, w, t); g.vline(cx - d, cy - e, h, t); g.vline(cx + d, cy - e, h, t); }
        '\u{2566}' => { g.hline(cy - d, 0.0, w, t); g.hline(cy + d, 0.0, cx - d + e, t); g.hline(cy + d, cx + d - e, w, t); g.vline(cx - d, cy + d - e, h, t); g.vline(cx + d, cy + d - e, h, t); }
        '\u{2567}' => { g.hline(cy - d, 0.0, w, t); g.hline(cy + d, 0.0, w, t); g.vline(cx, 0.0, cy - d + e, t); }
        '\u{2568}' => { g.hline(cy, 0.0, w, t); g.vline(cx - d, 0.0, cy + e, t); g.vline(cx + d, 0.0, cy + e, t); }
        '\u{2569}' => { g.hline(cy + d, 0.0, w, t); g.hline(cy - d, 0.0, cx - d + e, t); g.hline(cy - d, cx + d - e, w, t); g.vline(cx - d, 0.0, cy - d + e, t); g.vline(cx + d, 0.0, cy - d + e, t); }
        '\u{256A}' => { g.vline(cx, 0.0, h, t); g.hline(cy - d, 0.0, w, t); g.hline(cy + d, 0.0, w, t); }
        '\u{256B}' => { g.hline(cy, 0.0, w, t); g.vline(cx - d, 0.0, h, t); g.vline(cx + d, 0.0, h, t); }
        '\u{256C}' => {
            g.vline(cx - d, 0.0, cy - d + e, t); g.hline(cy - d, 0.0, cx - d + e, t);
            g.vline(cx + d, 0.0, cy - d + e, t); g.hline(cy - d, cx + d - e, w, t);
            g.vline(cx - d, cy + d - e, h, t); g.hline(cy + d, 0.0, cx - d + e, t);
            g.vline(cx + d, cy + d - e, h, t); g.hline(cy + d, cx + d - e, w, t);
        }

        // -- rounded corners (arcs stroked as squares) -------------------------
        '\u{256D}' => g.arc_corner(true, true),   // ╭ down+right
        '\u{256E}' => g.arc_corner(true, false),  // ╮ down+left
        '\u{256F}' => g.arc_corner(false, false), // ╯ up+left
        '\u{2570}' => g.arc_corner(false, true),  // ╰ up+right

        // -- diagonals ---------------------------------------------------------
        '\u{2571}' => g.stroke(&[(0.0, h), (w, 0.0)]),
        '\u{2572}' => g.stroke(&[(0.0, 0.0), (w, h)]),
        '\u{2573}' => { g.stroke(&[(0.0, h), (w, 0.0)]); g.stroke(&[(0.0, 0.0), (w, h)]); }

        // -- block elements ----------------------------------------------------
        '\u{2580}' => g.rect(0.0, 0.0, w, cy),                       // upper half
        '\u{2581}'..='\u{2588}' => {                                 // lower eighths
            let k = (c as u32 - 0x2580) as f32;
            g.rect(0.0, h - h * k / 8.0, w, h * k / 8.0);
        }
        '\u{2589}'..='\u{258F}' => {                                 // left eighths
            let k = (0x2590 - c as u32) as f32;
            g.rect(0.0, 0.0, w * k / 8.0, h);
        }
        '\u{2590}' => g.rect(cx, 0.0, w - cx, h),                    // right half
        '\u{2591}' => g.rect_a(0.0, 0.0, w, h, 0.25),                // light shade
        '\u{2592}' => g.rect_a(0.0, 0.0, w, h, 0.50),                // medium shade
        '\u{2593}' => g.rect_a(0.0, 0.0, w, h, 0.75),                // dark shade
        '\u{2594}' => g.rect(0.0, 0.0, w, h / 8.0),                  // upper eighth
        '\u{2595}' => g.rect(w - w / 8.0, 0.0, w / 8.0, h),          // right eighth
        '\u{2596}'..='\u{259F}' => g.quadrants(c),

        // -- powerline ---------------------------------------------------------
        '\u{E0B0}' => g.triangle(true),  // solid right-pointing
        '\u{E0B2}' => g.triangle(false), // solid left-pointing
        '\u{E0B1}' => g.stroke(&[(0.0, 0.0), (w - e, cy), (0.0, h)]),
        '\u{E0B3}' => g.stroke(&[(w, 0.0), (e, cy), (w, h)]),
        '\u{E0B4}' => g.semicircle(true),  // solid right half-disc
        '\u{E0B6}' => g.semicircle(false), // solid left half-disc
        '\u{E0B5}' => g.arc_ellipse(true),
        '\u{E0B7}' => g.arc_ellipse(false),
        '\u{E0A0}' => g.branch(),
        '\u{E0A2}' => g.padlock(),

        _ => unreachable!("is_sprite() and sprite_rects() disagree on {c:?}"),
    }
    Some(g.out)
}

// ---------------------------------------------------------------------------
// Light/heavy arm table (U+2500-U+254B minus the dashed range, U+2574-U+257F)
// ---------------------------------------------------------------------------

/// Arm weights `[up, down, left, right]`: 0 = none, 1 = light, 2 = heavy.
fn lh_arms(c: char) -> Option<[u8; 4]> {
    Some(match c {
        '─' => [0, 0, 1, 1], '━' => [0, 0, 2, 2], '│' => [1, 1, 0, 0], '┃' => [2, 2, 0, 0],
        '┌' => [0, 1, 0, 1], '┍' => [0, 1, 0, 2], '┎' => [0, 2, 0, 1], '┏' => [0, 2, 0, 2],
        '┐' => [0, 1, 1, 0], '┑' => [0, 1, 2, 0], '┒' => [0, 2, 1, 0], '┓' => [0, 2, 2, 0],
        '└' => [1, 0, 0, 1], '┕' => [1, 0, 0, 2], '┖' => [2, 0, 0, 1], '┗' => [2, 0, 0, 2],
        '┘' => [1, 0, 1, 0], '┙' => [1, 0, 2, 0], '┚' => [2, 0, 1, 0], '┛' => [2, 0, 2, 0],
        '├' => [1, 1, 0, 1], '┝' => [1, 1, 0, 2], '┞' => [2, 1, 0, 1], '┟' => [1, 2, 0, 1],
        '┠' => [2, 2, 0, 1], '┡' => [2, 1, 0, 2], '┢' => [1, 2, 0, 2], '┣' => [2, 2, 0, 2],
        '┤' => [1, 1, 1, 0], '┥' => [1, 1, 2, 0], '┦' => [2, 1, 1, 0], '┧' => [1, 2, 1, 0],
        '┨' => [2, 2, 1, 0], '┩' => [2, 1, 2, 0], '┪' => [1, 2, 2, 0], '┫' => [2, 2, 2, 0],
        '┬' => [0, 1, 1, 1], '┭' => [0, 1, 2, 1], '┮' => [0, 1, 1, 2], '┯' => [0, 1, 2, 2],
        '┰' => [0, 2, 1, 1], '┱' => [0, 2, 2, 1], '┲' => [0, 2, 1, 2], '┳' => [0, 2, 2, 2],
        '┴' => [1, 0, 1, 1], '┵' => [1, 0, 2, 1], '┶' => [1, 0, 1, 2], '┷' => [1, 0, 2, 2],
        '┸' => [2, 0, 1, 1], '┹' => [2, 0, 2, 1], '┺' => [2, 0, 1, 2], '┻' => [2, 0, 2, 2],
        '┼' => [1, 1, 1, 1], '┽' => [1, 1, 2, 1], '┾' => [1, 1, 1, 2], '┿' => [1, 1, 2, 2],
        '╀' => [2, 1, 1, 1], '╁' => [1, 2, 1, 1], '╂' => [2, 2, 1, 1], '╃' => [2, 1, 2, 1],
        '╄' => [2, 1, 1, 2], '╅' => [1, 2, 2, 1], '╆' => [1, 2, 1, 2], '╇' => [2, 1, 2, 2],
        '╈' => [1, 2, 2, 2], '╉' => [2, 2, 2, 1], '╊' => [2, 2, 1, 2], '╋' => [2, 2, 2, 2],
        '╴' => [0, 0, 1, 0], '╵' => [1, 0, 0, 0], '╶' => [0, 0, 0, 1], '╷' => [0, 1, 0, 0],
        '╸' => [0, 0, 2, 0], '╹' => [2, 0, 0, 0], '╺' => [0, 0, 0, 2], '╻' => [0, 2, 0, 0],
        '╼' => [0, 0, 1, 2], '╽' => [1, 2, 0, 0], '╾' => [0, 0, 2, 1], '╿' => [2, 1, 0, 0],
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// Geometry painter
// ---------------------------------------------------------------------------

/// Rect accumulator with the cell's derived metrics: `t` = light line thickness,
/// heavy = `2t`, `d` = the double-line half-gap, (`cx`, `cy`) = cell center.
struct Painter {
    w: f32,
    h: f32,
    cx: f32,
    cy: f32,
    t: f32,
    d: f32,
    out: Vec<SpriteRect>,
}

impl Painter {
    fn new(w: f32, h: f32) -> Self {
        let t = (w.min(h) / 8.0).round().max(1.0);
        Painter { w, h, cx: w / 2.0, cy: h / 2.0, t, d: 2.0 * t, out: Vec::new() }
    }

    /// Push a rect clamped to the cell box (skips degenerate slivers).
    fn rect_a(&mut self, x: f32, y: f32, w: f32, h: f32, alpha: f32) {
        let x0 = x.max(0.0);
        let y0 = y.max(0.0);
        let x1 = (x + w).min(self.w);
        let y1 = (y + h).min(self.h);
        if x1 - x0 > 0.01 && y1 - y0 > 0.01 {
            self.out.push(SpriteRect { x: x0, y: y0, w: x1 - x0, h: y1 - y0, alpha });
        }
    }

    fn rect(&mut self, x: f32, y: f32, w: f32, h: f32) {
        self.rect_a(x, y, w, h, 1.0);
    }

    /// Horizontal strip centered on `y`, from `x0` to `x1`, `t` thick.
    fn hline(&mut self, y: f32, x0: f32, x1: f32, t: f32) {
        self.rect(x0, y - t / 2.0, x1 - x0, t);
    }

    /// Vertical strip centered on `x`, from `y0` to `y1`, `t` thick.
    fn vline(&mut self, x: f32, y0: f32, y1: f32, t: f32) {
        self.rect(x - t / 2.0, y0, t, y1 - y0);
    }

    /// The four line arms of a box-drawing char, meeting cleanly at the center.
    /// Every present arm is extended past the center by half the thickest arm so
    /// mixed-weight joints have no notch (overdraw in one color is invisible).
    fn paint_arms(&mut self, arms: [u8; 4]) {
        let th = |a: u8| -> f32 {
            match a {
                1 => self.t,
                2 => self.t * 2.0,
                _ => 0.0,
            }
        };
        let [tu, td, tl, tr] = [th(arms[0]), th(arms[1]), th(arms[2]), th(arms[3])];
        let j = tu.max(td).max(tl).max(tr) / 2.0;
        let (w, h, cx, cy) = (self.w, self.h, self.cx, self.cy);
        if tu > 0.0 {
            self.vline(cx, 0.0, cy + j, tu);
        }
        if td > 0.0 {
            self.vline(cx, cy - j, h, td);
        }
        if tl > 0.0 {
            self.hline(cy, 0.0, cx + j, tl);
        }
        if tr > 0.0 {
            self.hline(cy, cx - j, w, tr);
        }
    }

    /// `n` dashes along the cell axis (the ┄┆╌-family).
    fn dashes(&mut self, horizontal: bool, n: u32, heavy: bool) {
        let t = if heavy { self.t * 2.0 } else { self.t };
        let span = if horizontal { self.w } else { self.h };
        let seg = span / n as f32;
        let dash = seg * 0.6;
        for i in 0..n {
            let a = i as f32 * seg + (seg - dash) / 2.0;
            if horizontal {
                self.hline(self.cy, a, a + dash, t);
            } else {
                self.vline(self.cx, a, a + dash, t);
            }
        }
    }

    /// Quadrant blocks U+2596-U+259F.
    fn quadrants(&mut self, c: char) {
        // [upper-left, upper-right, lower-left, lower-right]
        let q = match c {
            '▖' => [false, false, true, false],
            '▗' => [false, false, false, true],
            '▘' => [true, false, false, false],
            '▙' => [true, false, true, true],
            '▚' => [true, false, false, true],
            '▛' => [true, true, true, false],
            '▜' => [true, true, false, true],
            '▝' => [false, true, false, false],
            '▞' => [false, true, true, false],
            '▟' => [false, true, true, true],
            _ => return,
        };
        let (cx, cy, w, h) = (self.cx, self.cy, self.w, self.h);
        if q[0] {
            self.rect(0.0, 0.0, cx, cy);
        }
        if q[1] {
            self.rect(cx, 0.0, w - cx, cy);
        }
        if q[2] {
            self.rect(0.0, cy, cx, h - cy);
        }
        if q[3] {
            self.rect(cx, cy, w - cx, h - cy);
        }
    }

    /// Stroke a polyline with `t`-sized squares every half-pixel (quad-only
    /// approximation of an arbitrary line; used for diagonals and chevrons).
    fn stroke(&mut self, points: &[(f32, f32)]) {
        let t = self.t;
        for seg in points.windows(2) {
            let (x0, y0) = seg[0];
            let (x1, y1) = seg[1];
            let len = ((x1 - x0).powi(2) + (y1 - y0).powi(2)).sqrt();
            let steps = (len * 2.0).ceil().max(1.0) as u32;
            for i in 0..=steps {
                let f = i as f32 / steps as f32;
                let x = x0 + (x1 - x0) * f;
                let y = y0 + (y1 - y0) * f;
                self.rect(x - t / 2.0, y - t / 2.0, t, t);
            }
        }
    }

    /// Rounded corner ╭╮╯╰: the straight stubs plus a quarter-circle arc of
    /// radius `min(cx, cy)` stroked as squares.
    fn arc_corner(&mut self, down: bool, right: bool) {
        let (w, h, cx, cy, t) = (self.w, self.h, self.cx, self.cy, self.t);
        let r = cx.min(cy);
        let e = t / 2.0;
        // Vertical stub toward the open edge, horizontal stub toward the side.
        if down {
            self.vline(cx, cy + r - e, h, t);
        } else {
            self.vline(cx, 0.0, cy - r + e, t);
        }
        if right {
            self.hline(cy, cx + r - e, w, t);
        } else {
            self.hline(cy, 0.0, cx - r + e, t);
        }
        // Arc center is diagonally inside the turn; quarter sweep.
        let ac = (if right { cx + r } else { cx - r }, if down { cy + r } else { cy - r });
        let (a0, a1) = match (down, right) {
            (true, true) => (180.0_f32, 270.0),  // ╭
            (true, false) => (270.0, 360.0),     // ╮
            (false, false) => (0.0, 90.0),       // ╯
            (false, true) => (90.0, 180.0),      // ╰
        };
        let steps = ((r * 2.0).ceil() as u32).max(4);
        for i in 0..=steps {
            let a = (a0 + (a1 - a0) * i as f32 / steps as f32).to_radians();
            let x = ac.0 + r * a.cos();
            let y = ac.1 + r * a.sin();
            self.rect(x - t / 2.0, y - t / 2.0, t, t);
        }
    }

    /// Powerline solid triangle (E0B0 points right, E0B2 points left): 1px
    /// vertical slices whose height tapers linearly toward the tip.
    fn triangle(&mut self, right: bool) {
        let (w, h, cy) = (self.w, self.h, self.cy);
        let cols = w.ceil() as u32;
        for i in 0..cols {
            let x = i as f32;
            let f = if right { x / w } else { (w - x - 1.0).max(0.0) / w };
            let y0 = cy * f;
            self.rect(x, y0, (w - x).min(1.0), h - 2.0 * y0);
        }
    }

    /// Powerline solid half-disc (E0B4/E0B6): elliptical slices (rx = cell
    /// width, ry = half height) with the flat edge on the attached side.
    fn semicircle(&mut self, right: bool) {
        let (w, h, cy) = (self.w, self.h, self.cy);
        let cols = w.ceil() as u32;
        for i in 0..cols {
            let x = i as f32;
            let fx = if right { x / w } else { (w - x - 1.0).max(0.0) / w };
            let dy = cy * (1.0 - fx * fx).max(0.0).sqrt();
            self.rect(x, cy - dy, (w - x).min(1.0), (2.0 * dy).min(h));
        }
    }

    /// Powerline thin arc (E0B5/E0B7): the half-disc outline stroked as squares.
    fn arc_ellipse(&mut self, right: bool) {
        let (w, cy, t) = (self.w, self.cy, self.t);
        let steps = ((w + cy) as u32).max(8);
        for i in 0..=steps {
            let a = (-90.0 + 180.0 * i as f32 / steps as f32).to_radians();
            let x = w * a.cos();
            let x = if right { x } else { w - x };
            let y = cy + cy * a.sin();
            self.rect(x - t / 2.0, y - t / 2.0, t, t);
        }
    }

    /// Powerline branch (E0A0), simplified: a trunk with commit dots and one
    /// diagonal branch to a third dot.
    fn branch(&mut self) {
        let (w, h, t) = (self.w, self.h, self.t);
        let dot = (t * 2.5).min(w / 3.0);
        let trunk_x = 0.32 * w;
        let branch_x = 0.74 * w;
        self.vline(trunk_x, 0.24 * h, 0.76 * h, t);
        self.stroke(&[(trunk_x, 0.62 * h), (branch_x, 0.38 * h)]);
        for (x, y) in [(trunk_x, 0.17 * h), (trunk_x, 0.83 * h), (branch_x, 0.31 * h)] {
            self.rect(x - dot / 2.0, y - dot / 2.0, dot, dot);
        }
    }

    /// Powerline padlock (E0A2), simplified: solid body + squared shackle.
    fn padlock(&mut self) {
        let (w, h, t) = (self.w, self.h, self.t);
        let e = t / 2.0;
        self.rect(0.20 * w, 0.48 * h, 0.60 * w, 0.36 * h);
        self.vline(0.34 * w, 0.24 * h, 0.50 * h, t);
        self.vline(0.66 * w, 0.24 * h, 0.50 * h, t);
        self.hline(0.24 * h, 0.34 * w - e, 0.66 * w + e, t);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: f32 = 9.0;
    const H: f32 = 18.0;

    /// Every sprite char yields at least one in-bounds rect, and `is_sprite`
    /// agrees with `sprite_rects` across the whole covered range plus neighbors.
    #[test]
    fn coverage_and_bounds() {
        let mut sprite_chars: Vec<char> = (0x2500..=0x259F_u32)
            .chain(0xE0B0..=0xE0B7)
            .chain([0xE0A0, 0xE0A2])
            .filter_map(char::from_u32)
            .collect();
        sprite_chars.sort_unstable();
        for c in &sprite_chars {
            assert!(is_sprite(*c), "{c:?} should classify as sprite");
            let rects = sprite_rects(*c, W, H).unwrap_or_else(|| panic!("{c:?} has no rects"));
            assert!(!rects.is_empty(), "{c:?} produced zero rects");
            for r in &rects {
                assert!(r.w > 0.0 && r.h > 0.0, "{c:?} degenerate rect {r:?}");
                assert!(
                    r.x >= -0.01 && r.y >= -0.01 && r.x + r.w <= W + 0.01 && r.y + r.h <= H + 0.01,
                    "{c:?} rect out of cell: {r:?}"
                );
                assert!(r.alpha > 0.0 && r.alpha <= 1.0, "{c:?} bad alpha {r:?}");
            }
        }
        // Non-sprites stay in the font path.
        for c in ['A', '≠', '→', '\u{E0A1}', '\u{E0B8}', '你', '😀'] {
            assert!(!is_sprite(c), "{c:?} wrongly classified as sprite");
            assert!(sprite_rects(c, W, H).is_none());
        }
    }

    #[test]
    fn light_horizontal_spans_full_width_at_center() {
        // Drawn as two overlapping arms (left + right); their union must span
        // the full width, centered, at light thickness.
        let rects = sprite_rects('─', W, H).unwrap();
        let min_x = rects.iter().map(|r| r.x).fold(f32::MAX, f32::min);
        let max_x = rects.iter().map(|r| r.x + r.w).fold(0.0_f32, f32::max);
        assert_eq!((min_x, max_x), (0.0, W));
        for r in &rects {
            assert!((r.y + r.h / 2.0 - H / 2.0).abs() < 0.6, "not centered: {r:?}");
            assert_eq!(r.h, 1.0); // light thickness = round(min(9,18)/8) = 1
        }
    }

    #[test]
    fn heavy_is_twice_light() {
        let light = &sprite_rects('─', W, H).unwrap()[0];
        let heavy = &sprite_rects('━', W, H).unwrap()[0];
        assert_eq!(heavy.h, light.h * 2.0);
    }

    #[test]
    fn corner_arms_meet_at_center() {
        // ┌ = down + right: one vertical rect reaching the bottom, one horizontal
        // reaching the right edge, overlapping at the center.
        let rects = sprite_rects('┌', W, H).unwrap();
        assert_eq!(rects.len(), 2);
        let vert = rects.iter().find(|r| r.h > r.w).expect("vertical arm");
        let horiz = rects.iter().find(|r| r.w > r.h).expect("horizontal arm");
        assert_eq!(vert.y + vert.h, H, "down arm reaches bottom edge");
        assert_eq!(horiz.x + horiz.w, W, "right arm reaches right edge");
        assert!(vert.y <= H / 2.0 && horiz.x <= W / 2.0, "arms overlap the center");
    }

    #[test]
    fn full_block_fills_cell_and_shades_use_alpha() {
        let full = &sprite_rects('█', W, H).unwrap()[0];
        assert_eq!((full.x, full.y, full.w, full.h, full.alpha), (0.0, 0.0, W, H, 1.0));
        for (c, a) in [('░', 0.25), ('▒', 0.50), ('▓', 0.75)] {
            let r = &sprite_rects(c, W, H).unwrap()[0];
            assert_eq!((r.w, r.h, r.alpha), (W, H, a), "{c:?}");
        }
    }

    #[test]
    fn lower_eighths_grow_upward() {
        let one = &sprite_rects('▁', W, H).unwrap()[0];
        assert!((one.h - H / 8.0).abs() < 0.01 && (one.y - (H - H / 8.0)).abs() < 0.01);
        let half = &sprite_rects('▄', W, H).unwrap()[0];
        assert!((half.h - H / 2.0).abs() < 0.01 && (half.y - H / 2.0).abs() < 0.01);
    }

    #[test]
    fn quadrants_cover_expected_corners() {
        // ▚ = upper-left + lower-right.
        let rects = sprite_rects('▚', W, H).unwrap();
        assert_eq!(rects.len(), 2);
        assert!(rects.iter().any(|r| r.x == 0.0 && r.y == 0.0));
        assert!(rects.iter().any(|r| r.x == W / 2.0 && r.y == H / 2.0));
    }

    #[test]
    fn powerline_triangle_tapers_toward_tip() {
        let rects = sprite_rects('\u{E0B0}', W, H).unwrap();
        assert_eq!(rects.len(), W.ceil() as usize);
        // First slice nearly full height, later slices strictly shrinking.
        assert!((rects[0].h - H).abs() < 0.01, "base slice full height: {:?}", rects[0]);
        for pair in rects.windows(2) {
            assert!(pair[1].h < pair[0].h, "slices must taper: {pair:?}");
        }
        // Left-pointing mirror tapers the other way.
        let left = sprite_rects('\u{E0B2}', W, H).unwrap();
        assert!(left.last().unwrap().h > left[0].h);
    }

    #[test]
    fn double_lines_are_two_parallel_strips() {
        let rects = sprite_rects('═', W, H).unwrap();
        assert_eq!(rects.len(), 2);
        assert!(rects[0].y < rects[1].y, "two distinct horizontals");
        assert_eq!(rects[0].w, W);
        assert_eq!(rects[1].w, W);
    }

    #[test]
    fn dashes_leave_gaps() {
        let rects = sprite_rects('┄', W, H).unwrap();
        assert_eq!(rects.len(), 3);
        let covered: f32 = rects.iter().map(|r| r.w).sum();
        assert!(covered < W * 0.8, "dashes should not cover the full width");
    }

    #[test]
    fn diagonal_stroke_spans_the_cell() {
        let rects = sprite_rects('╱', W, H).unwrap();
        let min_y = rects.iter().map(|r| r.y).fold(f32::MAX, f32::min);
        let max_y = rects.iter().map(|r| r.y + r.h).fold(0.0_f32, f32::max);
        assert!(min_y < 1.0 && max_y > H - 1.0, "diagonal spans the full height");
    }

    #[test]
    fn tiny_cells_do_not_panic_or_escape_bounds() {
        for c in ['─', '╬', '╭', '╳', '█', '\u{E0B0}', '\u{E0A0}'] {
            for (w, h) in [(1.0, 2.0), (2.0, 2.0), (3.0, 5.0)] {
                if let Some(rects) = sprite_rects(c, w, h) {
                    for r in rects {
                        assert!(r.x >= 0.0 && r.y >= 0.0 && r.x + r.w <= w + 0.01 && r.y + r.h <= h + 0.01);
                    }
                }
            }
        }
    }
}
