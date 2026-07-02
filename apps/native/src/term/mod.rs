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
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Direction, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::search::{RegexIter, RegexSearch};
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{point_to_viewport, viewport_to_point, Config, TermDamage, TermMode};
use alacritty_terminal::vte;
use alacritty_terminal::Term;
use vte::ansi::{Color as AnsiColor, CursorShape, NamedColor};

pub mod scan;

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

/// A gpui-free snapshot of the terminal mode bits the input layer arbitrates on
/// (T6): mouse reporting, wheel behavior, cursor keys, paste framing, focus events.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ModeInfo {
    pub app_cursor: bool,
    pub bracketed_paste: bool,
    pub alt_screen: bool,
    pub alternate_scroll: bool,
    pub mouse_click: bool,
    pub mouse_drag: bool,
    pub mouse_motion: bool,
    pub sgr_mouse: bool,
    pub utf8_mouse: bool,
    pub focus_in_out: bool,
}

impl ModeInfo {
    /// True when the app asked for any mouse reporting (press/drag/motion). Shift
    /// overrides this for selection, per alacritty semantics.
    pub fn any_mouse(&self) -> bool {
        self.mouse_click || self.mouse_drag || self.mouse_motion
    }
}

/// Selection kind, mapped 1:1 onto alacritty's `SelectionType` (T6: single click =
/// Simple, ctrl+click = Block, double = Semantic/word, triple = Lines).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelKind {
    Simple,
    Block,
    Semantic,
    Lines,
}

impl From<SelKind> for SelectionType {
    fn from(k: SelKind) -> Self {
        match k {
            SelKind::Simple => SelectionType::Simple,
            SelKind::Block => SelectionType::Block,
            SelKind::Semantic => SelectionType::Semantic,
            SelKind::Lines => SelectionType::Lines,
        }
    }
}

/// A search match in **grid** coordinates (`line` may be negative into history),
/// inclusive of both ends - stable across scrolling, unlike viewport rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchHit {
    pub start: (i32, usize),
    pub end: (i32, usize),
}

/// A URL detected on the visible grid: the text plus its per-viewport-row column
/// segments (`(viewport_row, col_from, col_to)`, inclusive) for underline paint
/// and click hit-testing. A URL wrapped across rows carries one segment per row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UrlSpan {
    pub url: String,
    pub segs: Vec<(usize, usize, usize)>,
}

/// Split an inclusive grid-coordinate span into per-viewport-row column segments
/// (the same `(row, col_from, col_to)` shape [`UrlSpan`] uses). Rows scrolled out
/// of the viewport are dropped. Pure, so it unit-tests without a terminal.
pub fn grid_span_segs(
    start: (i32, usize),
    end: (i32, usize),
    display_offset: usize,
    rows: usize,
    cols: usize,
) -> Vec<(usize, usize, usize)> {
    let mut segs = Vec::new();
    if cols == 0 {
        return segs;
    }
    for line in start.0..=end.0 {
        let vr = line + display_offset as i32;
        if vr < 0 || vr as usize >= rows {
            continue;
        }
        let from = if line == start.0 { start.1 } else { 0 };
        let to = if line == end.0 { end.1 } else { cols - 1 };
        if from <= to {
            segs.push((vr as usize, from, to.min(cols - 1)));
        }
    }
    segs
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
    /// Compiled find-in-terminal query (T6). Kept here so next/prev and the
    /// per-frame visible-hit walk reuse one DFA instead of recompiling.
    search: Option<SearchCtx>,
}

/// The active search: the raw query (to skip recompiles) + its compiled regex.
struct SearchCtx {
    query: String,
    regex: RegexSearch,
}

impl TermSession {
    /// Create a session at the given geometry. §1.4.
    pub fn new(cols: u16, rows: u16) -> Self {
        let (cols, rows) = clamp_geom(cols, rows);
        let size = TermSize::new(cols as usize, rows as usize);
        let config = Config { scrolling_history: SCROLLING_HISTORY, ..Config::default() };
        let term = Term::new(config, &size, NullListener);
        TermSession { term, parser: vte::ansi::Processor::new(), cols, rows, search: None }
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

    // -- T6: mode introspection ---------------------------------------------

    /// Snapshot the mode bits the input layer arbitrates on (mouse reporting,
    /// alt screen, cursor keys, paste framing). Screen-derived, per decision D5.
    pub fn mode_info(&self) -> ModeInfo {
        let m = self.term.mode();
        ModeInfo {
            app_cursor: m.contains(TermMode::APP_CURSOR),
            bracketed_paste: m.contains(TermMode::BRACKETED_PASTE),
            alt_screen: m.contains(TermMode::ALT_SCREEN),
            alternate_scroll: m.contains(TermMode::ALTERNATE_SCROLL),
            mouse_click: m.contains(TermMode::MOUSE_REPORT_CLICK),
            mouse_drag: m.contains(TermMode::MOUSE_DRAG),
            mouse_motion: m.contains(TermMode::MOUSE_MOTION),
            sgr_mouse: m.contains(TermMode::SGR_MOUSE),
            utf8_mouse: m.contains(TermMode::UTF8_MOUSE),
            focus_in_out: m.contains(TermMode::FOCUS_IN_OUT),
        }
    }

    // -- T6: scrollback viewport --------------------------------------------

    /// Lines currently scrolled back from the live bottom (0 == pinned to live).
    pub fn display_offset(&self) -> usize {
        self.term.grid().display_offset()
    }

    /// Total scrollback lines available above the viewport.
    pub fn history_size(&self) -> usize {
        self.term.grid().history_size()
    }

    pub fn scroll_page_up(&mut self) {
        self.term.scroll_display(Scroll::PageUp);
    }

    pub fn scroll_page_down(&mut self) {
        self.term.scroll_display(Scroll::PageDown);
    }

    pub fn scroll_to_top(&mut self) {
        self.term.scroll_display(Scroll::Top);
    }

    /// Bring a grid line into view: no-op if already visible, otherwise scroll so
    /// the line sits near the vertical center (find-jump behavior).
    pub fn scroll_to_line(&mut self, line: i32) {
        let rows = self.rows as i32;
        let offset = self.term.grid().display_offset() as i32;
        let vr = line + offset;
        if vr >= 0 && vr < rows {
            return;
        }
        let history = self.term.grid().history_size() as i32;
        let target = (rows / 2 - line).clamp(0, history);
        self.term.scroll_display(Scroll::Delta(target - offset));
    }

    // -- T6: selection (drives alacritty's own Selection; grid coords come from
    //    viewport coords + the current display offset) ------------------------

    /// Begin a selection at a viewport cell. `right_side` is which half of the
    /// cell the pointer hit (alacritty anchors selections to cell sides).
    pub fn start_selection(&mut self, kind: SelKind, vp_line: usize, col: usize, right_side: bool) {
        let point = self.vp_point(vp_line, col);
        let side = if right_side { Side::Right } else { Side::Left };
        self.term.selection = Some(Selection::new(kind.into(), point, side));
    }

    /// Extend the active selection to a viewport cell (drag / shift+click).
    pub fn update_selection(&mut self, vp_line: usize, col: usize, right_side: bool) {
        let point = self.vp_point(vp_line, col);
        let side = if right_side { Side::Right } else { Side::Left };
        if let Some(sel) = self.term.selection.as_mut() {
            sel.update(point, side);
        }
    }

    pub fn clear_selection(&mut self) {
        self.term.selection = None;
    }

    pub fn has_selection(&self) -> bool {
        self.term.selection.is_some()
    }

    /// The selected text (alacritty's own extraction: handles block/semantic/line
    /// kinds, wide chars and wrapped lines). `None` when nothing is selected.
    pub fn selection_text(&self) -> Option<String> {
        self.term.selection_to_string()
    }

    /// Viewport cell -> grid point, clamped into the grid.
    fn vp_point(&self, vp_line: usize, col: usize) -> Point {
        let vp = Point::new(
            vp_line.min(self.rows as usize - 1),
            Column(col.min(self.cols as usize - 1)),
        );
        viewport_to_point(self.term.grid().display_offset(), vp)
    }

    // -- T6: find in scrollback (alacritty's RegexSearch over the whole buffer) --

    /// Compile a literal query (smart case, metachars escaped). Returns false and
    /// clears the search when the query is empty or fails to compile. Recompiles
    /// only when the query actually changed.
    pub fn set_search(&mut self, query: &str) -> bool {
        if query.is_empty() {
            self.search = None;
            return false;
        }
        if self.search.as_ref().is_some_and(|c| c.query == query) {
            return true;
        }
        match RegexSearch::new(&scan::search_pattern(query)) {
            Ok(regex) => {
                self.search = Some(SearchCtx { query: query.to_string(), regex });
                true
            }
            Err(_) => {
                self.search = None;
                false
            }
        }
    }

    pub fn clear_search(&mut self) {
        self.search = None;
    }

    /// Find the next/previous match. `from == None` starts at the viewport's top
    /// (forward) or bottom (backward); otherwise the search continues past the
    /// given hit. Wraps around the buffer, per alacritty search semantics.
    pub fn find_next(&mut self, from: Option<&SearchHit>, forward: bool) -> Option<SearchHit> {
        let TermSession { term, search, cols, rows, .. } = self;
        let ctx = search.as_mut()?;
        let cols_n = *cols as usize;
        let top = -(term.grid().history_size() as i32);
        let bottom = *rows as i32 - 1;
        let offset = term.grid().display_offset() as i32;

        let step = |p: (i32, usize), fwd: bool| -> Point {
            let (mut line, mut col) = p;
            if fwd {
                col += 1;
                if col >= cols_n {
                    col = 0;
                    line += 1;
                    if line > bottom {
                        line = top; // wrap to the oldest line
                    }
                }
            } else if col > 0 {
                col -= 1;
            } else {
                col = cols_n - 1;
                line -= 1;
                if line < top {
                    line = bottom; // wrap to the newest line
                }
            }
            Point::new(Line(line), Column(col))
        };

        let (origin, direction, side) = match from {
            Some(h) if forward => (step(h.end, true), Direction::Right, Side::Left),
            Some(h) => (step(h.start, false), Direction::Left, Side::Right),
            None if forward => (Point::new(Line(-offset), Column(0)), Direction::Right, Side::Left),
            None => {
                (Point::new(Line(bottom - offset), Column(cols_n - 1)), Direction::Left, Side::Right)
            }
        };

        let m = term.search_next(&mut ctx.regex, origin, direction, side, None)?;
        Some(SearchHit {
            start: (m.start().line.0, m.start().column.0),
            end: (m.end().line.0, m.end().column.0),
        })
    }

    /// All matches on the visible viewport as per-row column segments, for the
    /// highlight overlay. Capped (a pathological query can match every cell).
    pub fn visible_search_hits(&mut self) -> Vec<(usize, usize, usize)> {
        const VISIBLE_HITS_CAP: usize = 256;
        let TermSession { term, search, cols, rows, .. } = self;
        let Some(ctx) = search.as_mut() else { return Vec::new() };
        let offset = term.grid().display_offset();
        let rows_n = *rows as usize;
        let cols_n = *cols as usize;
        let start = Point::new(Line(-(offset as i32)), Column(0));
        let end = Point::new(Line(rows_n as i32 - 1 - offset as i32), Column(cols_n - 1));
        RegexIter::new(start, end, Direction::Right, term, &mut ctx.regex)
            .take(VISIBLE_HITS_CAP)
            .flat_map(|m| {
                grid_span_segs(
                    (m.start().line.0, m.start().column.0),
                    (m.end().line.0, m.end().column.0),
                    offset,
                    rows_n,
                    cols_n,
                )
            })
            .collect()
    }

    /// `(ordinal, total)` of `hit` among all matches in the buffer, oldest-first,
    /// both capped at 999 (the find bar's "3/17" display). Ordinal 0 == not found.
    pub fn match_stats(&mut self, hit: &SearchHit) -> (usize, usize) {
        const STATS_CAP: usize = 999;
        let TermSession { term, search, cols, .. } = self;
        let Some(ctx) = search.as_mut() else { return (0, 0) };
        let start = Point::new(Line(-(term.grid().history_size() as i32)), Column(0));
        let end = Point::new(term.grid().bottommost_line(), Column(*cols as usize - 1));
        let mut ordinal = 0;
        let mut total = 0;
        for m in RegexIter::new(start, end, Direction::Right, term, &mut ctx.regex).take(STATS_CAP)
        {
            total += 1;
            if (m.start().line.0, m.start().column.0) == hit.start {
                ordinal = total;
            }
        }
        (ordinal, total)
    }

    // -- T6: URL detection (client-plane grid scan, decision D5) --------------

    /// Scan the visible viewport for URLs, joining wrapped rows into logical
    /// lines so a URL split across rows is detected whole. Column segments are
    /// grid columns (wide chars occupy two).
    pub fn visible_urls(&self) -> Vec<UrlSpan> {
        let rows = self.rows as usize;
        let content = self.term.renderable_content();
        let offset = content.display_offset;

        // Per viewport row: the row text, each char's (row, col, width), wrapped?
        let mut texts: Vec<String> = vec![String::new(); rows];
        let mut positions: Vec<Vec<(usize, usize, usize)>> = vec![Vec::new(); rows];
        let mut wrapped: Vec<bool> = vec![false; rows];
        for indexed in content.display_iter {
            let vr = indexed.point.line.0 + offset as i32;
            if vr < 0 || vr as usize >= rows {
                continue;
            }
            let vr = vr as usize;
            let flags = indexed.cell.flags;
            if flags.contains(Flags::WRAPLINE) {
                wrapped[vr] = true;
            }
            if flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                continue;
            }
            let width = if flags.contains(Flags::WIDE_CHAR) { 2 } else { 1 };
            texts[vr].push(indexed.cell.c);
            positions[vr].push((vr, indexed.point.column.0, width));
        }

        // Join wrapped runs into logical lines, scan, map char ranges to segments.
        let mut out = Vec::new();
        let mut r = 0;
        while r < rows {
            let mut text = std::mem::take(&mut texts[r]);
            let mut pos = std::mem::take(&mut positions[r]);
            let mut last = r;
            while wrapped[last] && last + 1 < rows {
                last += 1;
                text.push_str(&texts[last]);
                pos.extend(positions[last].iter().copied());
            }
            for m in scan::scan_urls(&text) {
                let mut segs: Vec<(usize, usize, usize)> = Vec::new();
                for &(row, col, width) in &pos[m.start..m.end] {
                    match segs.last_mut() {
                        Some(s) if s.0 == row => s.2 = col + width - 1,
                        _ => segs.push((row, col, col + width - 1)),
                    }
                }
                if !segs.is_empty() {
                    out.push(UrlSpan { url: m.url, segs });
                }
            }
            r = last + 1;
        }
        out
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

    // -- T6: modes -----------------------------------------------------------

    #[test]
    fn mode_info_tracks_private_modes() {
        let mut t = TermSession::new(20, 5);
        let base = t.mode_info();
        assert!(!base.alt_screen);
        assert!(!base.any_mouse());
        assert!(!base.app_cursor);
        assert!(!base.bracketed_paste);

        t.advance(b"\x1b[?1049h\x1b[?1000h\x1b[?1006h\x1b[?2004h\x1b[?1h");
        let m = t.mode_info();
        assert!(m.alt_screen);
        assert!(m.mouse_click);
        assert!(m.sgr_mouse);
        assert!(m.bracketed_paste);
        assert!(m.app_cursor);
        assert!(m.any_mouse());

        // The three mouse modes are mutually exclusive (alacritty semantics):
        // each newly set mode replaces the previous one.
        t.advance(b"\x1b[?1002h");
        let m = t.mode_info();
        assert!(m.mouse_drag && !m.mouse_click && !m.mouse_motion);
        t.advance(b"\x1b[?1003h");
        let m = t.mode_info();
        assert!(m.mouse_motion && !m.mouse_drag);

        t.advance(b"\x1b[?1003l");
        assert!(!t.mode_info().any_mouse());
    }

    // -- T6: selection -------------------------------------------------------

    #[test]
    fn simple_selection_extracts_text() {
        let mut t = TermSession::new(20, 3);
        t.advance(b"hello world");
        t.start_selection(SelKind::Simple, 0, 0, false);
        t.update_selection(0, 4, true);
        assert_eq!(t.selection_text().as_deref(), Some("hello"));
        assert!(t.has_selection());
        t.clear_selection();
        assert!(!t.has_selection());
        assert_eq!(t.selection_text(), None);
    }

    #[test]
    fn semantic_selection_grabs_the_word() {
        let mut t = TermSession::new(20, 3);
        t.advance(b"hello world");
        t.start_selection(SelKind::Semantic, 0, 7, false);
        assert_eq!(t.selection_text().as_deref(), Some("world"));
    }

    #[test]
    fn line_selection_grabs_the_line() {
        let mut t = TermSession::new(20, 3);
        t.advance(b"hello world");
        t.start_selection(SelKind::Lines, 0, 3, false);
        let text = t.selection_text().expect("line selection");
        assert!(text.starts_with("hello world"), "text = {text:?}");
    }

    #[test]
    fn block_selection_is_rectangular() {
        let mut t = TermSession::new(20, 3);
        t.advance(b"abcd\r\nefgh");
        t.start_selection(SelKind::Block, 0, 1, false);
        t.update_selection(1, 2, true);
        assert_eq!(t.selection_text().as_deref(), Some("bc\nfg"));
    }

    #[test]
    fn selection_survives_scrollback_offset() {
        let mut t = TermSession::new(10, 3);
        for i in 0..30 {
            t.advance(format!("line{i}\r\n").as_bytes());
        }
        t.scroll(5); // scrolled back; viewport coords now map into history
        let snap = t.renderable();
        let row0: String = snap.rows_cells[0].iter().map(|c| c.c).collect();
        t.start_selection(SelKind::Lines, 0, 0, false);
        let text = t.selection_text().expect("selection in scrollback");
        assert!(text.starts_with(row0.trim_end()), "text {text:?} vs row {row0:?}");
    }

    // -- T6: scrollback state ------------------------------------------------

    #[test]
    fn display_offset_tracks_scroll_and_snap() {
        let mut t = TermSession::new(10, 3);
        for i in 0..30 {
            t.advance(format!("l{i}\r\n").as_bytes());
        }
        assert_eq!(t.display_offset(), 0);
        assert!(t.history_size() > 0);
        t.scroll(5);
        assert_eq!(t.display_offset(), 5);
        t.scroll_page_up();
        assert_eq!(t.display_offset(), 8); // one page = rows(3)
        t.scroll_to_bottom();
        assert_eq!(t.display_offset(), 0);
        t.scroll_to_top();
        assert_eq!(t.display_offset(), t.history_size());
        t.scroll_page_down();
        assert_eq!(t.display_offset(), t.history_size() - 3);
    }

    #[test]
    fn viewport_pins_to_content_when_scrolled_back() {
        // alacritty semantics: scrolled-back viewport stays on the same content
        // as new output arrives (the offset grows); typing snaps via
        // scroll_to_bottom, which the input layer calls.
        let mut t = TermSession::new(10, 3);
        for i in 0..20 {
            t.advance(format!("l{i}\r\n").as_bytes());
        }
        t.scroll(4);
        let before: String = t.renderable().rows_cells[0].iter().map(|c| c.c).collect();
        t.advance(b"new output\r\n");
        assert_eq!(t.display_offset(), 5, "offset grows to pin content");
        let after: String = t.renderable().rows_cells[0].iter().map(|c| c.c).collect();
        assert_eq!(before, after, "visible content unchanged by new output");
    }

    // -- T6: search ----------------------------------------------------------

    #[test]
    fn search_finds_wraps_and_reports_stats() {
        let mut t = TermSession::new(10, 3);
        for i in 0..20 {
            t.advance(format!("line{i}\r\n").as_bytes());
        }
        assert!(t.set_search("line3"));
        // line3 is in history above the viewport; forward search wraps to it.
        let hit = t.find_next(None, true).expect("wraps to history match");
        assert!(hit.start.0 < 0, "match in history: {hit:?}");
        let (ordinal, total) = t.match_stats(&hit);
        assert_eq!((ordinal, total), (1, 1));

        // Jump makes it visible.
        t.scroll_to_line(hit.start.0);
        assert!(t.display_offset() > 0);
        let vr = hit.start.0 + t.display_offset() as i32;
        assert!((0..3).contains(&vr), "hit row {vr} visible");
    }

    #[test]
    fn search_next_and_prev_cycle_matches() {
        let mut t = TermSession::new(20, 5);
        t.advance(b"foo one\r\nfoo two\r\nfoo three");
        assert!(t.set_search("foo"));
        let h1 = t.find_next(None, true).expect("first");
        let h2 = t.find_next(Some(&h1), true).expect("second");
        let h3 = t.find_next(Some(&h2), true).expect("third");
        assert!(h1.start.0 < h2.start.0 && h2.start.0 < h3.start.0);
        assert_eq!(t.match_stats(&h2), (2, 3));
        // Wraps forward past the last match back to the first.
        let h4 = t.find_next(Some(&h3), true).expect("wrap");
        assert_eq!(h4, h1);
        // And backward from the first back to the last.
        let h5 = t.find_next(Some(&h1), false).expect("wrap back");
        assert_eq!(h5, h3);
    }

    #[test]
    fn search_is_smart_case_and_literal() {
        let mut t = TermSession::new(20, 4);
        t.advance(b"HELLO a.b ab");
        assert!(t.set_search("hello"));
        assert!(t.find_next(None, true).is_some(), "lowercase query is insensitive");
        assert!(t.set_search("Hello"));
        assert!(t.find_next(None, true).is_none(), "uppercase query is sensitive");
        assert!(t.set_search("a.b"));
        let hit = t.find_next(None, true).expect("literal dot");
        assert_eq!(hit.start.1, 6, "matches 'a.b', not 'ab': {hit:?}");
    }

    #[test]
    fn visible_search_hits_map_to_viewport_segments() {
        let mut t = TermSession::new(20, 4);
        t.advance(b"foo bar foo");
        assert!(t.set_search("foo"));
        let segs = t.visible_search_hits();
        assert_eq!(segs, vec![(0, 0, 2), (0, 8, 10)]);
        t.clear_search();
        assert!(t.visible_search_hits().is_empty());
    }

    #[test]
    fn grid_span_segs_split_multi_row_spans() {
        // Span from history row into the viewport, offset 1, 3 rows, 10 cols.
        assert_eq!(
            grid_span_segs((-1, 7), (1, 2), 1, 3, 10),
            vec![(0, 7, 9), (1, 0, 9), (2, 0, 2)]
        );
        // Rows outside the viewport are dropped.
        assert_eq!(grid_span_segs((-5, 0), (-4, 3), 1, 3, 10), Vec::<(usize, usize, usize)>::new());
    }

    // -- T6: URL detection ---------------------------------------------------

    #[test]
    fn visible_urls_reports_grid_columns() {
        let mut t = TermSession::new(40, 3);
        t.advance(b"see https://example.com/x now");
        let urls = t.visible_urls();
        assert_eq!(urls.len(), 1);
        assert_eq!(urls[0].url, "https://example.com/x");
        assert_eq!(urls[0].segs, vec![(0, 4, 24)]);
    }

    #[test]
    fn visible_urls_account_for_wide_chars() {
        let mut t = TermSession::new(40, 3);
        // A wide CJK char occupies columns 0-1, so the URL starts at column 2.
        t.advance("你https://x.co".as_bytes());
        let urls = t.visible_urls();
        assert_eq!(urls.len(), 1);
        assert_eq!(urls[0].url, "https://x.co");
        assert_eq!(urls[0].segs, vec![(0, 2, 13)]);
    }

    #[test]
    fn visible_urls_join_wrapped_rows() {
        let mut t = TermSession::new(10, 4);
        t.advance(b"http://abc.de/fgh");
        let urls = t.visible_urls();
        assert_eq!(urls.len(), 1);
        assert_eq!(urls[0].url, "http://abc.de/fgh");
        assert_eq!(urls[0].segs, vec![(0, 0, 9), (1, 0, 6)]);
    }
}
