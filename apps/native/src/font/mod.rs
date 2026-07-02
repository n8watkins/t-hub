//! **Font subsystem** for the native client (native-pivot T7).
//!
//! Everything here is gpui-free (same rationale as `term/` and
//! `render_support.rs`): classification, sprite geometry, row segmentation and
//! the per-tile font configuration are plain data transforms that unit-test in
//! WSL under `--no-default-features`. The GPUI glue - building `gpui::Font`s
//! from a [`FontSpec`], mapping [`sprites::SpriteRect`]s onto `paint_quad`,
//! shaping segments - lives in `render/` behind the `gui` feature.
//!
//! ## Why rows are segmented (the wide-char column math)
//! T5 shaped each row as ONE line and painted it at the row origin, so every
//! cell's screen position depended on the previous glyphs' shaped advances.
//! That is only correct while every glyph comes from the primary monospace font
//! (advance == cell width). The moment a glyph falls back (CJK, emoji, symbols)
//! its advance is whatever the fallback font says, and every cell after it
//! drifts off the grid - while selection, cursor, URL underlines and mouse
//! hit-testing (T6) all assume `x == col * cell_w`.
//!
//! [`segment_cells`] therefore splits a row into independently positioned
//! segments, each painted at its own `col * cell_w`:
//! - runs of plain ASCII cells stay together (one shaped line - this is also
//!   what lets programming ligatures form, and the primary font's advances are
//!   exact by construction, since `cell_w` is measured from it);
//! - every other text cell (wide chars, non-ASCII, cells carrying combining
//!   marks) becomes a single-cell segment, so fallback advance error is
//!   confined to the cell's own box and can never push neighbors off-grid;
//! - box-drawing / Powerline cells become sprite segments, painted as quads
//!   ([`sprites`]) rather than shaped through a font (frozen §1.5 decision).

pub mod sprites;
pub mod torture;

use crate::term::SnapCell;

/// Default family/size, matching the T5 constants ("Cascadia Mono" is present
/// on the Windows box, §1.5; 13px/16px line height).
pub const DEFAULT_FAMILY: &str = "Cascadia Mono";
pub const DEFAULT_SIZE: f32 = 13.0;
/// line_height : font_size ratio (T5 used 16/13).
const LINE_HEIGHT_RATIO: f32 = 16.0 / 13.0;

// ---------------------------------------------------------------------------
// Per-tile font configuration (T7b plumbing)
// ---------------------------------------------------------------------------

/// Per-tile font configuration. `TileSpec.font` carries one of these; tiles
/// without it fall back to [`FontSpec::from_env`] (the `THN_FONT` override or
/// the defaults above).
#[derive(Clone, Debug, PartialEq)]
pub struct FontSpec {
    pub family: String,
    /// Font size in px. Line height derives via [`FontSpec::line_height`].
    pub size: f32,
    /// OpenType `calt` on/off. On by default: the shaper only forms ligatures
    /// the font defines (Cascadia Mono defines none; Cascadia Code does), and
    /// rows are rebuilt whole so no stale half-ligature can survive an edit.
    pub ligatures: bool,
}

impl Default for FontSpec {
    fn default() -> Self {
        FontSpec { family: DEFAULT_FAMILY.to_string(), size: DEFAULT_SIZE, ligatures: true }
    }
}

impl FontSpec {
    /// The row advance for this size (rounded to whole px to keep the grid
    /// crisp).
    pub fn line_height(&self) -> f32 {
        (self.size * LINE_HEIGHT_RATIO).round()
    }

    /// Parse `"Family"`, `"Family:14"`, `"Family:14:lig"` / `":nolig"`.
    /// Returns `None` for an empty family or an unparseable size.
    pub fn parse(s: &str) -> Option<FontSpec> {
        let mut parts = s.split(':');
        let family = parts.next()?.trim();
        if family.is_empty() {
            return None;
        }
        let mut spec = FontSpec { family: family.to_string(), ..FontSpec::default() };
        if let Some(size) = parts.next() {
            let size = size.trim();
            if !size.is_empty() {
                spec.size = size.parse::<f32>().ok().filter(|s| *s >= 5.0 && *s <= 72.0)?;
            }
        }
        match parts.next().map(str::trim) {
            Some("lig") => spec.ligatures = true,
            Some("nolig") => spec.ligatures = false,
            Some(_) => return None,
            None => {}
        }
        Some(spec)
    }

    /// The default spec, overridable via `THN_FONT="Family[:size[:lig|nolig]]"`.
    pub fn from_env() -> FontSpec {
        std::env::var("THN_FONT")
            .ok()
            .and_then(|s| FontSpec::parse(&s))
            .unwrap_or_default()
    }
}

/// Fallback families appended to every tile font (color emoji + symbols).
/// Families missing on the platform are skipped by the platform text system
/// (verified in gpui 0.2.2's DirectWrite backend, which also always appends
/// the system fallback chain after these).
pub fn fallback_families() -> Vec<String> {
    #[cfg(target_os = "windows")]
    let list: &[&str] = &["Segoe UI Emoji", "Segoe UI Symbol"];
    #[cfg(target_os = "macos")]
    let list: &[&str] = &["Apple Color Emoji", "Menlo"];
    #[cfg(all(unix, not(target_os = "macos")))]
    let list: &[&str] = &["Noto Color Emoji", "DejaVu Sans Mono", "Noto Sans Symbols 2"];
    list.iter().map(|s| s.to_string()).collect()
}

// ---------------------------------------------------------------------------
// Classification
// ---------------------------------------------------------------------------

/// Heuristic emoji ranges (for the catalogue/tests; the paint path does not
/// branch on this - font fallback resolves emoji, and the wide/non-ASCII
/// segmentation rule already isolates them positionally).
pub fn is_emoji(c: char) -> bool {
    matches!(c as u32,
        0x1F300..=0x1F5FF   // misc symbols & pictographs
        | 0x1F600..=0x1F64F // emoticons
        | 0x1F680..=0x1F6FF // transport
        | 0x1F900..=0x1F9FF // supplemental symbols
        | 0x1FA70..=0x1FAFF // symbols & pictographs ext-A
        | 0x1F1E6..=0x1F1FF // regional indicators (flags)
        | 0x2600..=0x27BF   // misc symbols + dingbats
        | 0x2B00..=0x2BFF)  // misc symbols & arrows (⭐ etc.)
}

/// Plain printable ASCII - the only chars whose primary-font advance is
/// guaranteed to equal the measured `cell_w` (see module doc).
fn is_plain_ascii(c: char) -> bool {
    (' '..='~').contains(&c)
}

// ---------------------------------------------------------------------------
// Row segmentation
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SegKind {
    /// Shaped through the tile font and painted at `col * cell_w`.
    Text,
    /// Painted procedurally as quads ([`sprites`]), never shaped.
    Sprite,
}

/// One independently positioned slice of a row: `cells[start..end]` starting at
/// grid column `col`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Seg {
    pub col: usize,
    pub start: usize,
    pub end: usize,
    pub kind: SegKind,
}

/// Split a snapshot row into positioned segments (rules in the module doc).
/// Column accounting follows each cell's grid `width` (wide chars occupy 2),
/// which matches the T6 column math exactly because both come from the same
/// alacritty flags.
pub fn segment_cells(cells: &[SnapCell]) -> Vec<Seg> {
    let mut segs: Vec<Seg> = Vec::new();
    let mut col = 0usize;
    for (i, cell) in cells.iter().enumerate() {
        let kind =
            if sprites::is_sprite(cell.c) { SegKind::Sprite } else { SegKind::Text };
        // A text cell whose advance we cannot trust gets its own segment.
        let isolate = kind == SegKind::Text
            && (cell.width > 1 || !cell.zw.is_empty() || !is_plain_ascii(cell.c));

        let extend = !isolate
            && segs.last().is_some_and(|s| {
                s.kind == kind
                    && s.end == i
                    // Never extend a segment that is itself an isolate.
                    && (kind == SegKind::Sprite || !seg_is_isolate(cells, s))
            });
        if extend {
            segs.last_mut().unwrap().end = i + 1;
        } else {
            segs.push(Seg { col, start: i, end: i + 1, kind });
        }
        col += cell.width.max(1) as usize;
    }
    segs
}

/// Whether an existing segment was created by the isolate rule (single cell
/// that is wide / non-ASCII / mark-bearing).
fn seg_is_isolate(cells: &[SnapCell], s: &Seg) -> bool {
    if s.end - s.start != 1 {
        return false;
    }
    let c = &cells[s.start];
    c.width > 1 || !c.zw.is_empty() || !is_plain_ascii(c.c)
}

/// True when two equal-length probe runs (wide `M`s vs narrow `i`s) shaped to
/// different widths: a monospace face advances every glyph identically, so a
/// mismatch means the requested family is missing and the platform substituted
/// a proportional font (the T7 drift bug's root cause). 1% of the wide run
/// tolerates sub-pixel shaping jitter; a real proportional face differs by
/// whole pixels per glyph, so an 80-char probe lands far past the threshold.
pub fn looks_proportional(wide_run_px: f32, narrow_run_px: f32) -> bool {
    (wide_run_px - narrow_run_px).abs() > wide_run_px.max(1.0) * 0.01
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::DEFAULT_FG;

    fn cell(c: char) -> SnapCell {
        SnapCell {
            c,
            fg: DEFAULT_FG,
            bg: None,
            bold: false,
            underline: false,
            width: 1,
            zw: Vec::new(),
        }
    }

    fn wide(c: char) -> SnapCell {
        SnapCell { width: 2, ..cell(c) }
    }

    // -- FontSpec -------------------------------------------------------------

    #[test]
    fn font_spec_parse_valid_forms() {
        let s = FontSpec::parse("Cascadia Code").unwrap();
        assert_eq!(s.family, "Cascadia Code");
        assert_eq!(s.size, DEFAULT_SIZE);
        assert!(s.ligatures);

        let s = FontSpec::parse("JetBrains Mono:16").unwrap();
        assert_eq!((s.family.as_str(), s.size), ("JetBrains Mono", 16.0));
        assert_eq!(s.line_height(), 20.0); // 16 * 16/13 = 19.7 -> 20

        let s = FontSpec::parse("Cascadia Code:14:nolig").unwrap();
        assert!(!s.ligatures);
        assert!(FontSpec::parse("").is_none());
        assert!(FontSpec::parse("Foo:huge").is_none());
        assert!(FontSpec::parse("Foo:14:bogus").is_none());
        assert!(FontSpec::parse("Foo:1000").is_none(), "size out of range");
    }

    #[test]
    fn default_line_height_matches_t5_constants() {
        assert_eq!(FontSpec::default().line_height(), 16.0);
    }

    // -- classification --------------------------------------------------------

    #[test]
    fn emoji_classification_samples() {
        for c in ['😀', '🚀', '⭐', '✅', '🇺', '☁'] {
            assert!(is_emoji(c), "{c:?}");
        }
        for c in ['a', '你', '─', '\u{0301}'] {
            assert!(!is_emoji(c), "{c:?}");
        }
    }

    // -- segmentation -----------------------------------------------------------

    #[test]
    fn ascii_run_is_one_segment() {
        let cells: Vec<SnapCell> = "hello world".chars().map(cell).collect();
        let segs = segment_cells(&cells);
        assert_eq!(segs, vec![Seg { col: 0, start: 0, end: 11, kind: SegKind::Text }]);
    }

    #[test]
    fn wide_chars_isolate_and_advance_two_columns() {
        // "a你b" -> [a][你][b] with b at column 3.
        let cells = vec![cell('a'), wide('你'), cell('b')];
        let segs = segment_cells(&cells);
        assert_eq!(
            segs,
            vec![
                Seg { col: 0, start: 0, end: 1, kind: SegKind::Text },
                Seg { col: 1, start: 1, end: 2, kind: SegKind::Text },
                Seg { col: 3, start: 2, end: 3, kind: SegKind::Text },
            ]
        );
    }

    #[test]
    fn consecutive_wide_chars_stay_isolated() {
        // Two CJK chars never merge (each positions at its own column).
        let cells = vec![wide('你'), wide('好')];
        let segs = segment_cells(&cells);
        assert_eq!(segs.len(), 2);
        assert_eq!((segs[0].col, segs[1].col), (0, 2));
    }

    #[test]
    fn sprites_merge_but_break_text() {
        // "a─│b" -> text[a], sprite[─│], text[b] at col 3.
        let cells = vec![cell('a'), cell('─'), cell('│'), cell('b')];
        let segs = segment_cells(&cells);
        assert_eq!(
            segs,
            vec![
                Seg { col: 0, start: 0, end: 1, kind: SegKind::Text },
                Seg { col: 1, start: 1, end: 3, kind: SegKind::Sprite },
                Seg { col: 3, start: 3, end: 4, kind: SegKind::Text },
            ]
        );
    }

    #[test]
    fn combining_marks_isolate_their_cell() {
        let mut e = cell('e');
        e.zw = vec!['\u{0301}'];
        let cells = vec![cell('a'), e, cell('b')];
        let segs = segment_cells(&cells);
        assert_eq!(segs.len(), 3, "mark-bearing cell is its own segment: {segs:?}");
        assert_eq!(segs[1], Seg { col: 1, start: 1, end: 2, kind: SegKind::Text });
    }

    #[test]
    fn non_ascii_narrow_chars_isolate() {
        // '→' is width 1 but not in the primary-advance-guaranteed set.
        let cells = vec![cell('a'), cell('→'), cell('b')];
        let segs = segment_cells(&cells);
        assert_eq!(segs.len(), 3);
        // And two isolates in a row do not merge with each other.
        let cells = vec![cell('→'), cell('→')];
        assert_eq!(segment_cells(&cells).len(), 2);
    }

    #[test]
    fn empty_row_yields_no_segments() {
        assert!(segment_cells(&[]).is_empty());
    }

    // -- looks_proportional -----------------------------------------------------

    #[test]
    fn monospace_probe_widths_do_not_warn() {
        // Identical advances, and sub-pixel shaping jitter, both pass.
        assert!(!looks_proportional(624.0, 624.0));
        assert!(!looks_proportional(624.0, 623.4));
    }

    #[test]
    fn proportional_probe_widths_warn() {
        // A substituted proportional face: 80 'M's shape far wider than 80 'i's.
        assert!(looks_proportional(800.0, 350.0));
        // Even a narrow-ish proportional face is whole pixels per glyph apart.
        assert!(looks_proportional(624.0, 560.0));
    }

    #[test]
    fn proportional_probe_is_zero_safe() {
        assert!(!looks_proportional(0.0, 0.0));
    }
}
