//! **Terminal session** for the native client (native-pivot T5, the render seam).
//!
//! `TermSession` is the §1.4 contract: it wraps an `alacritty_terminal::Term`
//! driven by `vte::ansi::Processor`, turns raw PTY bytes (the `PtyFrame::Out`
//! payloads that [`crate::wire::PtyHandle`] streams) into terminal state, and hands
//! the render layer a gpui-free [`Snapshot`] plus damage information.
//!
//! ## Why this module is graphics-free
//! Everything here is plain data (chars + RGB + flags). The cell -> `TextRun`
//! transform, shaping and painting live in [`crate::render`] behind the `gui`
//! feature. Keeping `term/` gpui-free means it compiles and unit-tests in WSL under
//! `--no-default-features` (same rationale as `wire/`), and it makes the render
//! seam a clean one-way data flow: bytes -> `Term` -> `Snapshot` -> GPU.
//!
//! ## The feed pattern (proven in the T2 spike)
//! `vte::ansi::Processor::advance(&mut term, bytes)` - exactly what the spike fed
//! its 16 grids. The render transform mirrors alacritty's `display/content.rs`
//! (consecutive same-style cells merge into one run).
//!
//! ## Damage
//! `alacritty_terminal` already tracks per-line damage. [`TermSession::take_damage`]
//! returns [`Damage::Full`] (whole viewport dirty - resize, scroll, alt-screen
//! switch, insert mode) or [`Damage::Lines`] (only these viewport rows changed),
//! then resets the terminal's damage state. The render layer uses this to rebuild
//! only the rows that changed rather than every row every frame (§1.5).

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Scroll;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{point_to_viewport, Config, TermDamage};
use alacritty_terminal::vte;
use alacritty_terminal::Term;
use vte::ansi::{Color as AnsiColor, CursorShape, NamedColor};

/// Scrollback retained per session. Matches the spike's grid config.
const SCROLLING_HISTORY: usize = 10_000;

/// Default foreground / background when a cell uses the terminal's default color.
/// The same palette the spike and the debug overlay use (a dark editor theme).
pub const DEFAULT_FG: Rgb = Rgb { r: 216, g: 222, b: 233 };
pub const DEFAULT_BG: Rgb = Rgb { r: 13, g: 17, b: 23 };

// ---------------------------------------------------------------------------
// gpui-free snapshot types (the render seam's data plane)
// ---------------------------------------------------------------------------

/// A plain 8-bit-per-channel color. gpui-free so `term/` needs no graphics crate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

/// One rendered cell: the glyph plus its already-resolved colors and attributes.
/// `inverse` is folded into `fg`/`bg` here so the render layer never re-derives it.
/// A `None` `bg` means "terminal default background" - the render layer skips the
/// background quad (the tile's base fill shows through), matching the spike.
#[derive(Clone, Debug, PartialEq)]
pub struct SnapCell {
    pub c: char,
    pub fg: Rgb,
    pub bg: Option<Rgb>,
    pub bold: bool,
    pub underline: bool,
}

/// Cursor position in viewport coordinates (row 0 == top visible line).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CursorPos {
    pub line: usize,
    pub col: usize,
    /// False when the cursor is hidden (DECTCEM off) or scrolled out of view.
    pub visible: bool,
}

/// A selection range in viewport coordinates (inclusive of both ends), as reported
/// by alacritty. `is_block` distinguishes rectangular from linewise selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelSpan {
    pub start: (usize, usize),
    pub end: (usize, usize),
    pub is_block: bool,
}

/// A full renderable snapshot: every viewport row's cells, plus cursor + selection.
/// Owned (no borrow of the `Term`) so the caller can drop the terminal lock before
/// shaping/painting. Built by [`TermSession::renderable`].
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub cols: u16,
    pub rows: u16,
    /// `rows_cells[r]` is row `r`'s cells left-to-right (wide-char spacers dropped).
    pub rows_cells: Vec<Vec<SnapCell>>,
    pub cursor: Option<CursorPos>,
    pub selection: Option<SelSpan>,
}

/// Only the rows the render layer asked for, plus the (cheap) cursor/selection.
/// Returned by [`TermSession::renderable_rows`] so a damage-clipped frame rebuilds
/// nothing it doesn't have to. `rows` is `(viewport_row, cells)` pairs.
#[derive(Clone, Debug, Default)]
pub struct PartialSnapshot {
    pub rows: Vec<(usize, Vec<SnapCell>)>,
    pub cursor: Option<CursorPos>,
    pub selection: Option<SelSpan>,
}

/// Damage since the last [`TermSession::take_damage`] call (§1.4/§1.5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Damage {
    /// The whole viewport changed (resize, scroll, alt-screen switch, insert mode).
    Full,
    /// Only these viewport rows changed. Empty == nothing changed this interval.
    Lines(Vec<usize>),
}

// ---------------------------------------------------------------------------
// Color mapping (gpui-free; RGB out). Lifted from the T2 spike's proven mapping.
// ---------------------------------------------------------------------------

/// The classic xterm 256-color cube + grayscale ramp, with a VS-Code-ish base 16.
fn xterm256(i: u8) -> Rgb {
    const BASE: [(u8, u8, u8); 16] = [
        (0, 0, 0), (205, 49, 49), (13, 188, 121), (229, 229, 16),
        (36, 114, 200), (188, 63, 188), (17, 168, 205), (229, 229, 229),
        (102, 102, 102), (241, 76, 76), (35, 209, 139), (245, 245, 67),
        (59, 142, 234), (214, 112, 214), (41, 184, 219), (255, 255, 255),
    ];
    let (r, g, b) = match i {
        0..=15 => BASE[i as usize],
        16..=231 => {
            let n = i - 16;
            let c = [0u8, 95, 135, 175, 215, 255];
            (c[(n / 36) as usize], c[((n / 6) % 6) as usize], c[(n % 6) as usize])
        }
        _ => {
            let v = 8 + 10 * (i - 232);
            (v, v, v)
        }
    };
    Rgb { r, g, b }
}

/// Map an alacritty cell color to RGB. `None` means "default background" for a bg
/// slot (render skips the quad); a fg slot never returns `None` (falls to default).
fn ansi_to_rgb(c: AnsiColor, is_fg: bool) -> Option<Rgb> {
    match c {
        AnsiColor::Spec(rgb) => Some(Rgb { r: rgb.r, g: rgb.g, b: rgb.b }),
        AnsiColor::Indexed(i) => Some(xterm256(i)),
        AnsiColor::Named(nc) => {
            let d = nc as usize;
            if d < 16 {
                Some(xterm256(d as u8))
            } else {
                match nc {
                    NamedColor::Foreground | NamedColor::BrightForeground => Some(DEFAULT_FG),
                    NamedColor::Background => {
                        if is_fg {
                            Some(DEFAULT_BG)
                        } else {
                            None // default bg -> no quad
                        }
                    }
                    _ => Some(DEFAULT_FG),
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// TermSession
// ---------------------------------------------------------------------------

/// A no-op alacritty event listener. The native client observes state by polling
/// `renderable()`/`take_damage()` each frame rather than via alacritty's event
/// proxy (screen-derived signals per decision D5), so nothing needs delivering.
#[derive(Clone, Copy, Default)]
pub struct NullListener;
impl EventListener for NullListener {
    fn send_event(&self, _event: Event) {}
}

/// One terminal session: an `alacritty_terminal::Term` + its `vte` byte parser,
/// at a fixed `cols x rows` geometry. Fed by [`TermSession::advance`], read by
/// [`TermSession::renderable`]/[`TermSession::take_damage`]. §1.4.
pub struct TermSession {
    term: Term<NullListener>,
    parser: vte::ansi::Processor,
    cols: u16,
    rows: u16,
}

impl TermSession {
    /// Create a session at the given geometry. §1.4.
    pub fn new(cols: u16, rows: u16) -> Self {
        let (cols, rows) = clamp_geom(cols, rows);
        let size = TermSize::new(cols as usize, rows as usize);
        let config = Config { scrolling_history: SCROLLING_HISTORY, ..Config::default() };
        let term = Term::new(config, &size, NullListener);
        TermSession { term, parser: vte::ansi::Processor::new(), cols, rows }
    }

    /// Feed raw PTY output bytes (a `PtyFrame::Out` payload). §1.4.
    pub fn advance(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
    }

    /// Resize the terminal (and its scrollback reflow). §1.4.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let (cols, rows) = clamp_geom(cols, rows);
        if (cols, rows) == (self.cols, self.rows) {
            return;
        }
        self.cols = cols;
        self.rows = rows;
        self.term.resize(TermSize::new(cols as usize, rows as usize));
    }

    /// Scroll the display viewport into scrollback (positive == toward history).
    /// Not part of the frozen §1.4 interface (additive; T6 owns full scroll UX),
    /// but needed for the T5 scroll acceptance.
    pub fn scroll(&mut self, lines: i32) {
        if lines != 0 {
            self.term.scroll_display(Scroll::Delta(lines));
        }
    }

    /// Jump the viewport back to the live bottom (e.g. on new input).
    pub fn scroll_to_bottom(&mut self) {
        self.term.scroll_display(Scroll::Bottom);
    }

    pub fn cols(&self) -> u16 {
        self.cols
    }

    pub fn rows(&self) -> u16 {
        self.rows
    }

    /// Full renderable snapshot: every viewport row's cells + cursor + selection.
    /// Owned, so the caller may drop the terminal lock before shaping. §1.4.
    pub fn renderable(&self) -> Snapshot {
        let rows = self.rows as usize;
        let content = self.term.renderable_content();
        let display_offset = content.display_offset;

        let mut rows_cells: Vec<Vec<SnapCell>> =
            (0..rows).map(|_| Vec::with_capacity(self.cols as usize)).collect();
        for indexed in content.display_iter {
            let vr = indexed.point.line.0 + display_offset as i32;
            if vr < 0 || vr as usize >= rows {
                continue;
            }
            if let Some(cell) = to_snap_cell(indexed.cell) {
                rows_cells[vr as usize].push(cell);
            }
        }

        Snapshot {
            cols: self.cols,
            rows: self.rows,
            rows_cells,
            cursor: cursor_from(&content.cursor, display_offset, rows),
            selection: selection_from(content.selection),
        }
    }

    /// Like [`renderable`](Self::renderable) but only materializes the requested
    /// viewport rows (a damage-clipped frame rebuilds nothing else). Additive to
    /// §1.4 - documented as a deviation. The cell walk still visits every cell
    /// (alacritty's `display_iter` has no per-row entry point), but that pointer
    /// walk is cheap; the costly per-cell allocation is what gets clipped to the
    /// rows that actually changed.
    pub fn renderable_rows(&self, want: &[usize]) -> PartialSnapshot {
        let rows = self.rows as usize;
        let content = self.term.renderable_content();
        let display_offset = content.display_offset;

        let mut out: Vec<(usize, Vec<SnapCell>)> =
            want.iter().filter(|&&r| r < rows).map(|&r| (r, Vec::new())).collect();
        for indexed in content.display_iter {
            let vr = indexed.point.line.0 + display_offset as i32;
            if vr < 0 || vr as usize >= rows {
                continue;
            }
            let vr = vr as usize;
            if let Some(slot) = out.iter_mut().find(|(r, _)| *r == vr) {
                if let Some(cell) = to_snap_cell(indexed.cell) {
                    slot.1.push(cell);
                }
            }
        }

        PartialSnapshot {
            rows: out,
            cursor: cursor_from(&content.cursor, display_offset, rows),
            selection: selection_from(content.selection),
        }
    }

    /// Damage accumulated since the last call, then reset it. §1.4.
    pub fn take_damage(&mut self) -> Damage {
        let rows = self.rows as usize;
        let damage = match self.term.damage() {
            TermDamage::Full => Damage::Full,
            // Collect fully before `reset_damage` (releases the `&mut` borrow).
            TermDamage::Partial(iter) => {
                Damage::Lines(iter.map(|b| b.line).filter(|&l| l < rows).collect())
            }
        };
        self.term.reset_damage();
        damage
    }
}

/// Clamp geometry to at least 1x1 so alacritty never sees a zero dimension (it
/// indexes unconditionally and would panic on a 0-column resize during layout).
fn clamp_geom(cols: u16, rows: u16) -> (u16, u16) {
    (cols.max(1), rows.max(1))
}

/// Resolve one alacritty cell into a gpui-free [`SnapCell`], or `None` for a
/// wide-char spacer slot (the render layer never paints those). Inverse video is
/// resolved into fg/bg here.
fn to_snap_cell(cell: &alacritty_terminal::term::cell::Cell) -> Option<SnapCell> {
    if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
        return None;
    }
    let mut fg = ansi_to_rgb(cell.fg, true).unwrap_or(DEFAULT_FG);
    let mut bg = ansi_to_rgb(cell.bg, false);
    if cell.flags.contains(Flags::INVERSE) {
        let old_fg = fg;
        fg = bg.unwrap_or(DEFAULT_BG);
        bg = Some(old_fg);
    }
    Some(SnapCell {
        c: cell.c,
        fg,
        bg,
        bold: cell.flags.intersects(Flags::BOLD),
        underline: cell.flags.intersects(Flags::UNDERLINE),
    })
}

/// Map alacritty's cursor into a viewport [`CursorPos`], honoring hidden shape and
/// scroll-out-of-view.
fn cursor_from(
    cursor: &alacritty_terminal::term::RenderableCursor,
    display_offset: usize,
    rows: usize,
) -> Option<CursorPos> {
    let visible = cursor.shape != CursorShape::Hidden;
    match point_to_viewport(display_offset, cursor.point) {
        Some(p) if p.line < rows => Some(CursorPos { line: p.line, col: p.column.0, visible }),
        // Off-viewport (scrolled away): report position clamped, marked invisible.
        _ => Some(CursorPos { line: 0, col: 0, visible: false }),
    }
}

/// Map alacritty's selection range into a gpui-free [`SelSpan`] in viewport coords.
fn selection_from(
    sel: Option<alacritty_terminal::selection::SelectionRange>,
) -> Option<SelSpan> {
    sel.map(|s| SelSpan {
        start: (s.start.line.0.max(0) as usize, s.start.column.0),
        end: (s.end.line.0.max(0) as usize, s.end.column.0),
        is_block: s.is_block,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advance_renders_plain_text() {
        let mut t = TermSession::new(20, 5);
        t.advance(b"hello");
        let snap = t.renderable();
        assert_eq!(snap.rows, 5);
        assert_eq!(snap.cols, 20);
        let row0: String = snap.rows_cells[0].iter().map(|c| c.c).collect();
        assert!(row0.starts_with("hello"), "row0 = {row0:?}");
    }

    #[test]
    fn sgr_colors_and_bold_are_resolved() {
        let mut t = TermSession::new(20, 3);
        // bright-red bold 'X' then reset.
        t.advance(b"\x1b[1;31mX\x1b[0m");
        let snap = t.renderable();
        let cell = &snap.rows_cells[0][0];
        assert_eq!(cell.c, 'X');
        assert!(cell.bold, "expected bold");
        assert_eq!(cell.fg, xterm256(1)); // named red -> palette index 1
    }

    #[test]
    fn truecolor_background_is_captured() {
        let mut t = TermSession::new(20, 3);
        t.advance(b"\x1b[48;2;10;20;30mA\x1b[0m");
        let snap = t.renderable();
        assert_eq!(snap.rows_cells[0][0].bg, Some(Rgb { r: 10, g: 20, b: 30 }));
    }

    #[test]
    fn newline_advances_row_and_cursor_tracks() {
        let mut t = TermSession::new(20, 4);
        t.advance(b"a\r\nb");
        let snap = t.renderable();
        assert_eq!(snap.rows_cells[0][0].c, 'a');
        assert_eq!(snap.rows_cells[1][0].c, 'b');
        let cur = snap.cursor.expect("cursor present");
        assert!(cur.visible);
        assert_eq!(cur.line, 1);
        assert_eq!(cur.col, 1);
    }

    #[test]
    fn first_frame_is_full_damage_then_writes_report_lines() {
        let mut t = TermSession::new(20, 4);
        // A brand-new terminal starts fully damaged.
        assert_eq!(t.take_damage(), Damage::Full);
        // After reset, a single-line write reports only that line as damaged.
        t.advance(b"hi");
        match t.take_damage() {
            Damage::Full => {} // acceptable (some ops force full); still correct
            Damage::Lines(lines) => {
                assert!(lines.contains(&0), "line 0 should be damaged: {lines:?}");
            }
        }
        // With nothing written, damage is empty (cursor may re-damage its own line
        // only if it moved; a no-op advance leaves it put).
        let d = t.take_damage();
        if let Damage::Lines(lines) = d {
            assert!(lines.len() <= 1, "idle interval should be ~no damage: {lines:?}");
        }
    }

    #[test]
    fn renderable_rows_matches_full_for_requested_rows() {
        let mut t = TermSession::new(20, 5);
        t.advance(b"row0\r\nrow1\r\nrow2");
        let full = t.renderable();
        let partial = t.renderable_rows(&[1]);
        assert_eq!(partial.rows.len(), 1);
        let (r, cells) = &partial.rows[0];
        assert_eq!(*r, 1);
        assert_eq!(cells, &full.rows_cells[1]);
    }

    #[test]
    fn resize_changes_geometry() {
        let mut t = TermSession::new(20, 5);
        t.resize(40, 10);
        assert_eq!((t.cols(), t.rows()), (40, 10));
        let snap = t.renderable();
        assert_eq!(snap.rows_cells.len(), 10);
    }
}
