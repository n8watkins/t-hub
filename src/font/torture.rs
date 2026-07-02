//! **Font torture-test fixture** (native-pivot T7).
//!
//! [`torture_bytes`] returns a deterministic ANSI/UTF-8 byte stream that, when
//! fed to a terminal emulator (alacritty_terminal painted by the GPUI renderer),
//! displays one torture-test screen for font fidelity: box drawing in every
//! weight, block elements, Powerline glyphs, CJK width, combining marks, emoji,
//! ligature bait, truecolor ramps and gamma/blend samples.
//!
//! The same bytes are piped into WezTerm / Windows Terminal for side-by-side
//! visual comparison, so the stream is plain valid UTF-8 with standard SGR
//! escapes only (CSI ... m) - no cursor movement, no OSC. Every line ends in
//! CRLF and stays within 96 visible columns (wide chars counted as 2), so the
//! fixture fits a [`TORTURE_COLS`] x [`torture_rows`] grid without wrapping.
//!
//! This module is gpui-free (std only) and unit-tests under
//! `--no-default-features`.

/// Terminal width (columns) the fixture is designed for. Content stays within
/// 96 visible columns, leaving a safety margin against wrap.
pub const TORTURE_COLS: u16 = 100;

/// Terminal height (rows) needed to show the whole fixture: the actual line
/// count plus 2 rows of margin.
pub fn torture_rows() -> u16 {
    build_lines().len() as u16 + 2
}

/// The full torture-test screen as one byte stream. Each line is terminated by
/// CRLF; the stream ends with an SGR reset so no attributes leak.
pub fn torture_bytes() -> Vec<u8> {
    let lines = build_lines();
    let mut out = Vec::new();
    let last = lines.len().saturating_sub(1);
    for (i, line) in lines.iter().enumerate() {
        out.extend_from_slice(line.as_bytes());
        if i == last {
            // final SGR reset - leave the terminal in a clean state
            out.extend_from_slice(SGR0.as_bytes());
        }
        out.extend_from_slice(b"\r\n");
    }
    out
}

const SGR0: &str = "\x1b[0m";
const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";

fn fg(r: u8, g: u8, b: u8) -> String {
    format!("\x1b[38;2;{r};{g};{b}m")
}

fn bg(r: u8, g: u8, b: u8) -> String {
    format!("\x1b[48;2;{r};{g};{b}m")
}

/// Dim section label - visually separates sections without stealing attention
/// from the samples themselves.
fn label(lines: &mut Vec<String>, text: &str) {
    if !lines.is_empty() {
        lines.push(String::new());
    }
    lines.push(format!("{DIM}{text}{SGR0}"));
}

fn build_lines() -> Vec<String> {
    let mut l: Vec<String> = Vec::new();

    // 1. column ruler - for eyeballing horizontal drift
    label(
        &mut l,
        "1. column ruler (| marker every 10 columns, to col 90)",
    );
    let mut tens = String::new();
    for d in 0..9u8 {
        tens.push(char::from(b'0' + d));
        tens.push_str("         ");
    }
    l.push(tens);
    l.push("|123456789".repeat(9));

    // 2. ascii
    label(&mut l, "2. ascii");
    l.push(
        "The quick brown fox jumps over the lazy dog. \
         SPHINX OF BLACK QUARTZ, JUDGE MY VOW. 0123456789"
            .into(),
    );
    l.push("!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~".into());

    // 3. box drawing light
    label(&mut l, "3. box drawing light");
    l.push("┌──────────┬──────────┐".into());
    l.push("│ light    │ frame    │".into());
    l.push("├──────────┼──────────┤".into());
    l.push("│ interior │ text     │".into());
    l.push("└──────────┴──────────┘".into());

    // 4. box drawing heavy + mixed-weight junctions
    label(&mut l, "4. box drawing heavy + mixed weight");
    l.push("┏━━━━━━━━━━┳━━━━━━━━━━┓".into());
    l.push("┃ heavy    ┃ frame    ┃".into());
    l.push("┣━━━━━━━━━━╋━━━━━━━━━━┫".into());
    l.push("┃ interior ┃ text     ┃".into());
    l.push("┗━━━━━━━━━━┻━━━━━━━━━━┛".into());
    l.push("mixed weight: ┍ ┎ ┱ ┲ ╊ ╉ ┽ ╀".into());

    // 5. box drawing double + single/double junctions
    label(&mut l, "5. box drawing double + single/double junctions");
    l.push("╔══════════╦══════════╗".into());
    l.push("║ double   ║ frame    ║".into());
    l.push("╠══════════╬══════════╣".into());
    l.push("║ interior ║ text     ║".into());
    l.push("╚══════════╩══════════╝".into());
    l.push("junctions: ╒ ╓ ╕ ╖ ╞ ╟ ╡ ╢ ╤ ╥ ╧ ╨ ╪ ╫".into());

    // 6. dashes, rounded corners, diagonals, half lines
    label(&mut l, "6. dashes, rounded, diagonals, half lines");
    l.push("┄┄┄ ┅┅┅ ┆┆┆ ┇┇┇ ┈┈┈ ┉┉┉ ┊┊┊ ┋┋┋ ╌╌╌ ╍╍╍ ╎╎╎ ╏╏╏".into());
    l.push("╭──────────╮".into());
    l.push("│ rounded  │".into());
    l.push("╰──────────╯".into());
    l.push("diagonals: ╱╱╱ ╲╲╲ ╳╳╳ ╱╲╱╲".into());
    l.push("half lines: ╴ ╵ ╶ ╷ ╸ ╹ ╺ ╻ ╼ ╽ ╾ ╿  joined: ╶╴ ╺╸ ╾╼ ╽╿".into());

    // 7. block elements
    label(&mut l, "7. block elements");
    l.push("lower ▁▂▃▄▅▆▇█  left ▏▎▍▌▋▊▉█  halves ▀▄▌▐  edges ▔▕".into());
    l.push("shades ░░░▒▒▒▓▓▓███  quadrants ▖▗▘▙▚▛▜▝▞▟".into());
    l.push(format!(
        "{}{} ░▒▓█ ▀▄▌▐ ▖▚▞▟ {SGR0} truecolor fg on truecolor bg",
        fg(255, 170, 0),
        bg(30, 30, 96),
    ));

    // 8. powerline
    label(&mut l, "8. powerline");
    // three-segment prompt chain: blue user -> gray cwd -> dark git branch
    l.push(format!(
        "{}{} user {}{}\u{e0b0}{} ~/code {}{}\u{e0b0}{} \u{e0a0} main {SGR0}{}\u{e0b0}{SGR0}",
        bg(38, 79, 120),
        fg(255, 255, 255),
        fg(38, 79, 120),
        bg(90, 90, 90),
        fg(235, 235, 235),
        fg(90, 90, 90),
        bg(45, 45, 45),
        fg(220, 220, 220),
        fg(45, 45, 45),
    ));
    l.push(
        "separators: |\u{e0b0}|\u{e0b1}|\u{e0b2}|\u{e0b3}| \
         solid-right thin-right solid-left thin-left"
            .into(),
    );
    l.push(
        "semicircles: |\u{e0b4}|\u{e0b5}|\u{e0b6}|\u{e0b7}|  \
         symbols: |\u{e0a0}|\u{e0a1}|\u{e0a2}| branch ln padlock"
            .into(),
    );

    // 9. cjk wide chars - fences must align if wide chars occupy exactly 2 cells
    label(&mut l, "9. cjk wide chars + alignment fences");
    l.push("|你好世界|一二三四|".into());
    l.push("|abcdefgh|12345678|".into());
    l.push("abc コンニチハ def 한글 ghi 漢字 jkl".into());

    // 10. combining marks
    label(&mut l, "10. combining marks");
    l.push(
        "marks |a\u{301}|e\u{301}|n\u{303}|o\u{308}|u\u{30a}| \
         acute acute tilde diaeresis ring"
            .into(),
    );
    l.push(
        "zalgo |z\u{300}\u{316}a\u{301}\u{330}l\u{302}\u{323}g\u{303}\u{347}o\u{308}\u{324}|"
            .into(),
    );
    l.push("thai |\u{e01}\u{e33} \u{e01}\u{e49} \u{e0d}|  arabic |سلام|  devanagari |नमस्ते|".into());

    // 11. emoji - fences make cell-width behavior visible
    label(&mut l, "11. emoji");
    l.push("plain |😀|🚀|⭐|✅|  text-vs-vs16 |❤|❤\u{fe0f}|  skin |👍|👍🏽|".into());
    l.push(
        "flags |\u{1f1fa}\u{1f1f8}|\u{1f1ef}\u{1f1f5}|  \
         zwj-family |👨\u{200d}👩\u{200d}👧|  cloud |☁|☁\u{fe0f}|"
            .into(),
    );

    // 12. ligature bait - a terminal grid should keep these per-cell
    label(&mut l, "12. ligature bait");
    l.push("-> <- => <= >= != == === !== :: ::: |> <| || && ++ -- ... /* */ <!-- --> www".into());
    l.push("if (a != b && c >= d) { x |> f(y) } // => ok".into());

    // 13. truecolor ramps - fg 0..255 and bg 255..0 on the same half-block glyph
    label(
        &mut l,
        "13. truecolor ramps (fg 0->255, bg 255->0, 64 half blocks)",
    );
    for (name, m) in [
        ("R", (true, false, false)),
        ("G", (false, true, false)),
        ("B", (false, false, true)),
        ("K", (true, true, true)),
    ] {
        let mut line = format!("{name} ");
        for i in 0..64u32 {
            let v = (i * 255 / 63) as u8;
            let w = 255 - v;
            let ch = |on: bool, x: u8| if on { x } else { 0 };
            line.push_str(&fg(ch(m.0, v), ch(m.1, v), ch(m.2, v)));
            line.push_str(&bg(ch(m.0, w), ch(m.1, w), ch(m.2, w)));
            line.push('▀');
        }
        line.push_str(SGR0);
        l.push(line);
    }

    // 14. gamma / blend
    label(&mut l, "14. gamma / blend");
    l.push(format!(
        "{}{} The quick brown fox {SGR0} {}{} The quick brown fox {SGR0}",
        fg(255, 255, 255),
        bg(0, 0, 0),
        fg(0, 0, 0),
        bg(255, 255, 255),
    ));
    l.push(format!(
        "{DIM}dim: the quick brown fox{SGR0}  {BOLD}bold: the quick brown fox{SGR0}"
    ));
    l.push(format!(
        "{}{} ─│┌┐└┘├┤┬┴┼ {SGR0} {}{} ─│┌┐└┘├┤┬┴┼ {SGR0}",
        fg(255, 255, 255),
        bg(0, 0, 0),
        fg(0, 0, 0),
        bg(255, 255, 255),
    ));

    // 15. grand finale - double frame, mixed title, light inner divider
    label(&mut l, "15. grand finale");
    l.push(format!("╔══ 端末 🚀 \u{e0a0} torture {}╗", "═".repeat(21)));
    l.push(format!(
        "║ CJK 你好 and kana コンニチハ here{}║",
        " ".repeat(8)
    ));
    l.push(format!("╟{}╢", "─".repeat(42)));
    l.push(format!(
        "║ emoji 😀 │ powerline \u{e0b0}\u{e0b1} │ ok{}║",
        " ".repeat(13)
    ));
    l.push(format!("╚{}╝", "═".repeat(42)));

    l
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Strip CSI sequences: ESC '[' parameter/intermediate bytes, terminated by
    /// a final byte in 0x40..=0x7E.
    fn strip_csi(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                assert_eq!(chars.next(), Some('['), "only CSI escapes are allowed");
                for c2 in chars.by_ref() {
                    if ('\x40'..='\x7e').contains(&c2) {
                        assert_eq!(c2, 'm', "only SGR (CSI ... m) is allowed");
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    /// Approximate visible column count: wide = 2 for CJK/kana/hangul/emoji
    /// ranges, 0 for combining marks, VS16, ZWJ and the second char of a
    /// regional-indicator pair, 1 otherwise.
    fn visible_cols(s: &str) -> usize {
        let mut cols = 0;
        let mut prev_ri = false;
        for c in strip_csi(s).chars() {
            let cp = c as u32;
            let is_ri = (0x1f1e6..=0x1f1ff).contains(&cp);
            let w = if (0x0300..=0x036f).contains(&cp)
                || cp == 0xfe0f
                || cp == 0x200d
                || (is_ri && prev_ri)
            {
                0
            } else if (0x1100..=0x115f).contains(&cp)
                || (0x2e80..=0xa4cf).contains(&cp)
                || (0xac00..=0xd7a3).contains(&cp)
                || (0xf900..=0xfaff).contains(&cp)
                || (0xff00..=0xff60).contains(&cp)
                || (0x1f300..=0x1faff).contains(&cp)
            {
                2
            } else {
                1
            };
            prev_ri = is_ri && !prev_ri;
            cols += w;
        }
        cols
    }

    #[test]
    fn stream_is_valid_utf8() {
        let s = String::from_utf8(torture_bytes()).expect("torture stream must be valid UTF-8");
        assert!(!s.is_empty());
    }

    #[test]
    fn contains_representative_chars() {
        let s = String::from_utf8(torture_bytes()).unwrap();
        for c in ['┌', '╬', '\u{e0b0}', '你', '\u{301}', '😀', '▀', '░'] {
            assert!(s.contains(c), "missing representative char {c:?}");
        }
    }

    #[test]
    fn no_line_exceeds_96_visible_columns() {
        let s = String::from_utf8(torture_bytes()).unwrap();
        for (i, line) in s.split("\r\n").enumerate() {
            let cols = visible_cols(line);
            assert!(cols <= 96, "line {i} is {cols} visible cols: {line:?}");
            assert!((cols as u16) < TORTURE_COLS);
        }
    }

    #[test]
    fn rows_is_line_count_plus_margin() {
        let s = String::from_utf8(torture_bytes()).unwrap();
        let crlf = s.matches("\r\n").count();
        assert_eq!(torture_rows() as usize, crlf + 2);
        // every line ends in CRLF - no bare \n or \r anywhere
        assert_eq!(s.matches('\n').count(), crlf);
        assert_eq!(s.matches('\r').count(), crlf);
        assert!(s.ends_with("\r\n"));
    }
}
