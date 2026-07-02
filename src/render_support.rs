//! **gpui-free render helpers** (native-pivot T5, extended by T6): the pure logic
//! behind the render layer - keyboard/mouse-to-PTY encoding, input-action routing,
//! paste framing, find-bar key handling, and layout/hit-test math.
//!
//! These live outside `render/` (which is gpui-gated) precisely so they unit-test in
//! WSL under `--no-default-features`: the GPUI backend links system graphics libs
//! that the WSL box may lack, so gui-feature test binaries can't link there (see the
//! T4 Verify note). Keeping the testable logic gpui-free sidesteps that entirely.

use crate::term::ModeInfo;

/// Near-square grid dimensions for `n` tiles: `cols = ceil(sqrt(n))`, enough rows to
/// fit. n=12 -> 4x3, n=16 -> 4x4, n=9 -> 3x3.
pub fn grid_dims(n: usize) -> (usize, usize) {
    if n == 0 {
        return (1, 1);
    }
    let cols = (n as f64).sqrt().ceil() as usize;
    let rows = n.div_ceil(cols);
    (cols, rows)
}

/// A keystroke reduced to the fields the encoder needs - gpui-free so it (and
/// [`encode`]) can be tested without the graphics backend. The render layer builds
/// this from a `gpui::Keystroke`.
#[derive(Clone, Debug, Default)]
pub struct KeyChord {
    pub control: bool,
    pub alt: bool,
    pub shift: bool,
    pub platform: bool,
    /// The layout key (e.g. "a", "enter", "up").
    pub key: String,
    /// The character that would be typed (respects shift/layout), if any.
    pub key_char: Option<String>,
}

/// What a keystroke means for a terminal tile (T6). The render layer executes
/// these; deciding them is pure so it tests here. Chords follow alacritty's
/// defaults: Ctrl+Shift+C/V copy/paste, Ctrl+Shift+F find, Shift+PageUp/Down and
/// Shift+Home/End page the scrollback (passed to the app instead in alt screen).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyAction {
    /// Send these bytes to the PTY (and snap the viewport to the live bottom).
    Write(Vec<u8>),
    ScrollPageUp,
    ScrollPageDown,
    ScrollTop,
    ScrollBottom,
    Copy,
    Paste,
    OpenSearch,
    Ignore,
}

/// Route a keystroke: app chords first, scrollback chords next (unless the alt
/// screen owns paging), else PTY encoding.
pub fn key_action(k: &KeyChord, m: &ModeInfo) -> KeyAction {
    if k.control && k.shift && !k.alt && !k.platform {
        match k.key.as_str() {
            "c" => return KeyAction::Copy,
            "v" => return KeyAction::Paste,
            "f" => return KeyAction::OpenSearch,
            _ => {}
        }
    }
    if k.shift && !k.control && !k.alt && !k.platform && !m.alt_screen {
        match k.key.as_str() {
            "pageup" => return KeyAction::ScrollPageUp,
            "pagedown" => return KeyAction::ScrollPageDown,
            "home" => return KeyAction::ScrollTop,
            "end" => return KeyAction::ScrollBottom,
            _ => {}
        }
    }
    match encode(k, m) {
        Some(bytes) => KeyAction::Write(bytes),
        None => KeyAction::Ignore,
    }
}

/// Encode a keystroke into terminal input bytes (xterm semantics, mirroring
/// alacritty's default bindings): app-cursor SS3 arrows, `CSI 1;m` modified
/// arrows/Home/End, tilde keys with modifiers, F1-F12, Ctrl-letter control bytes,
/// Alt-as-ESC-prefix, AltGr text, printable text via `key_char`.
pub fn encode(k: &KeyChord, m: &ModeInfo) -> Option<Vec<u8>> {
    let key = k.key.as_str();

    // AltGr arrives as ctrl+alt with a produced char: type the char, no ESC prefix.
    if k.control && k.alt && !k.platform {
        if let Some(kc) = &k.key_char {
            if !kc.is_empty() {
                return Some(kc.as_bytes().to_vec());
            }
        }
    }

    // Ctrl-letter (and a few ctrl symbols) -> control byte.
    if k.control && !k.platform && key.len() == 1 {
        let ch = key.as_bytes()[0];
        let byte = match ch {
            b'a'..=b'z' => Some(ch - b'a' + 1),
            b'A'..=b'Z' => Some(ch - b'A' + 1),
            b'@' | b' ' => Some(0),
            b'[' => Some(27),
            b'\\' => Some(28),
            b']' => Some(29),
            b'^' => Some(30),
            b'_' => Some(31),
            _ => None,
        };
        if let Some(b) = byte {
            return Some(with_alt(k.alt, vec![b]));
        }
    }

    // xterm modifier parameter: 1 + shift(1) + alt(2) + ctrl(4). 1 == unmodified.
    let modp = 1 + k.shift as u8 + 2 * (k.alt as u8) + 4 * (k.control as u8);

    // Arrows + Home/End: SS3 final in app-cursor mode, CSI otherwise; any modifier
    // forces the `CSI 1;m X` form (and disables the SS3 variant), per xterm.
    let cursor_final: Option<u8> = match key {
        "up" => Some(b'A'),
        "down" => Some(b'B'),
        "right" => Some(b'C'),
        "left" => Some(b'D'),
        "home" => Some(b'H'),
        "end" => Some(b'F'),
        _ => None,
    };
    if let Some(c) = cursor_final {
        return Some(if modp == 1 {
            let intro = if m.app_cursor { b'O' } else { b'[' };
            vec![0x1b, intro, c]
        } else {
            format!("\x1b[1;{modp}{}", c as char).into_bytes()
        });
    }

    // Tilde keys (Insert/Delete/PgUp/PgDn, F5-F12): `CSI n ~` / `CSI n;m ~`.
    let tilde_num: Option<u8> = match key {
        "insert" => Some(2),
        "delete" => Some(3),
        "pageup" => Some(5),
        "pagedown" => Some(6),
        "f5" => Some(15),
        "f6" => Some(17),
        "f7" => Some(18),
        "f8" => Some(19),
        "f9" => Some(20),
        "f10" => Some(21),
        "f11" => Some(23),
        "f12" => Some(24),
        _ => None,
    };
    if let Some(n) = tilde_num {
        return Some(if modp == 1 {
            format!("\x1b[{n}~").into_bytes()
        } else {
            format!("\x1b[{n};{modp}~").into_bytes()
        });
    }

    // F1-F4: SS3 P/Q/R/S unmodified, `CSI 1;m P..S` with modifiers.
    let fkey_final: Option<u8> = match key {
        "f1" => Some(b'P'),
        "f2" => Some(b'Q'),
        "f3" => Some(b'R'),
        "f4" => Some(b'S'),
        _ => None,
    };
    if let Some(c) = fkey_final {
        return Some(if modp == 1 {
            vec![0x1b, b'O', c]
        } else {
            format!("\x1b[1;{modp}{}", c as char).into_bytes()
        });
    }

    let named: Option<&[u8]> = match key {
        "enter" => Some(b"\r"),
        "escape" => Some(b"\x1b"),
        "backspace" => {
            if k.control {
                Some(b"\x08")
            } else {
                Some(b"\x7f")
            }
        }
        "tab" => {
            if k.shift {
                Some(b"\x1b[Z")
            } else {
                Some(b"\t")
            }
        }
        "space" => Some(b" "),
        _ => None,
    };
    if let Some(bytes) = named {
        return Some(with_alt(k.alt, bytes.to_vec()));
    }

    // Printable: prefer key_char (respects shift/layout), fall back to a 1-char key.
    if !k.platform && !k.control {
        if let Some(kc) = &k.key_char {
            if !kc.is_empty() {
                return Some(with_alt(k.alt, kc.as_bytes().to_vec()));
            }
        }
        if key.chars().count() == 1 {
            return Some(with_alt(k.alt, key.as_bytes().to_vec()));
        }
    }
    None
}

/// Prefix ESC for Alt/Meta chords (the standard xterm meta-sends-escape).
fn with_alt(alt: bool, mut bytes: Vec<u8>) -> Vec<u8> {
    if alt {
        let mut out = Vec::with_capacity(bytes.len() + 1);
        out.push(0x1b);
        out.append(&mut bytes);
        out
    } else {
        bytes
    }
}

// ---------------------------------------------------------------------------
// Mouse reporting (T6): X10 / UTF-8 (1005) / SGR (1006) encodings
// ---------------------------------------------------------------------------

/// What kind of mouse report to emit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseKind {
    Press,
    Release,
    Motion,
}

/// Wheel "buttons" in the mouse-reporting protocol.
pub const MOUSE_WHEEL_UP: u8 = 64;
pub const MOUSE_WHEEL_DOWN: u8 = 65;
/// The "no button" code used for hover motion reports (mode 1003).
pub const MOUSE_NO_BUTTON: u8 = 3;

/// Encode one mouse event for the PTY. `button`: 0/1/2 = left/middle/right,
/// [`MOUSE_NO_BUTTON`] for buttonless motion, [`MOUSE_WHEEL_UP`]/[`MOUSE_WHEEL_DOWN`]
/// for the wheel. `cell` is the 0-based viewport cell; the wire is 1-based.
/// `mods` = (shift, alt, ctrl). SGR (1006) uses `CSI < b;x;y M|m`; otherwise the
/// legacy `CSI M` 3-byte form, with coordinates clamped at 223 unless UTF-8 mouse
/// (1005) extends them to 2015.
pub fn encode_mouse(
    kind: MouseKind,
    button: u8,
    cell: (usize, usize),
    mods: (bool, bool, bool),
    m: &ModeInfo,
) -> Vec<u8> {
    let (col, row) = cell;
    let (shift, alt, ctrl) = mods;
    // Legacy encoding can't say WHICH button was released; SGR can.
    let mut cb = if kind == MouseKind::Release && !m.sgr_mouse { 3 } else { button };
    if kind == MouseKind::Motion {
        cb += 32;
    }
    if shift {
        cb += 4;
    }
    if alt {
        cb += 8;
    }
    if ctrl {
        cb += 16;
    }
    if m.sgr_mouse {
        let suffix = if kind == MouseKind::Release { 'm' } else { 'M' };
        format!("\x1b[<{cb};{};{}{suffix}", col + 1, row + 1).into_bytes()
    } else {
        let mut out = vec![0x1b, b'[', b'M', 32 + cb];
        push_mouse_coord(&mut out, col, m.utf8_mouse);
        push_mouse_coord(&mut out, row, m.utf8_mouse);
        out
    }
}

/// One legacy mouse coordinate: `32 + 1-based value`, one byte clamped at 255, or
/// a UTF-8 code point up to 2047 in 1005 mode.
fn push_mouse_coord(out: &mut Vec<u8>, v: usize, utf8: bool) {
    let c = v + 33;
    if utf8 && c > 127 {
        let ch = char::from_u32(c.min(2047) as u32).unwrap_or(' ');
        let mut buf = [0u8; 4];
        out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
    } else {
        out.push(c.min(255) as u8);
    }
}

/// Wheel scrolling while the alt screen is active (mode 1007 "alternate scroll"):
/// arrow keypresses instead of viewport scrolling - this is what makes the wheel
/// work in less/vim/htop. Positive `lines` == wheel up == Up arrows.
pub fn alt_scroll_bytes(lines: i32, app_cursor: bool) -> Vec<u8> {
    if lines == 0 {
        return Vec::new();
    }
    let seq: &[u8] = match (lines > 0, app_cursor) {
        (true, true) => b"\x1bOA",
        (true, false) => b"\x1b[A",
        (false, true) => b"\x1bOB",
        (false, false) => b"\x1b[B",
    };
    seq.repeat(lines.unsigned_abs() as usize)
}

// ---------------------------------------------------------------------------
// Paste (T6)
// ---------------------------------------------------------------------------

/// Frame pasted text for the PTY. Bracketed paste (mode 2004) wraps the text in
/// `ESC[200~ .. ESC[201~` and strips any embedded end marker (paste injection
/// guard, as alacritty does). Plain paste normalizes newlines to CR.
pub fn encode_paste(text: &str, bracketed: bool) -> Vec<u8> {
    if bracketed {
        let mut out = b"\x1b[200~".to_vec();
        out.extend_from_slice(text.replace("\x1b[201~", "").as_bytes());
        out.extend_from_slice(b"\x1b[201~");
        out
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

// ---------------------------------------------------------------------------
// Find-bar key routing (T6)
// ---------------------------------------------------------------------------

/// What a keystroke means while the find bar is open on the focused tile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SearchKey {
    /// Append this text to the query.
    Input(String),
    Backspace,
    Next,
    Prev,
    Close,
    Ignore,
}

/// Route a keystroke for the find bar: Enter/F3 next, Shift+Enter/Shift+F3 prev,
/// Escape (or the Ctrl+Shift+F toggle) closes, printable text edits the query.
pub fn search_key(k: &KeyChord) -> SearchKey {
    match k.key.as_str() {
        "escape" => return SearchKey::Close,
        "enter" | "f3" => {
            return if k.shift { SearchKey::Prev } else { SearchKey::Next };
        }
        "backspace" => return SearchKey::Backspace,
        "f" if k.control && k.shift && !k.alt && !k.platform => return SearchKey::Close,
        _ => {}
    }
    if !k.platform && (!k.control || k.alt) {
        if let Some(kc) = &k.key_char {
            if !kc.is_empty() && !kc.chars().any(char::is_control) {
                return SearchKey::Input(kc.clone());
            }
        }
    }
    SearchKey::Ignore
}

// ---------------------------------------------------------------------------
// Hit-testing math (T6)
// ---------------------------------------------------------------------------

/// A pixel position resolved to a terminal cell. `right_side` is which half of
/// the cell was hit (selection anchoring); `inside` is false when the position
/// was outside the grid and had to be clamped (drags clamp; presses that start
/// outside the grid should not report to the app).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellHit {
    pub col: usize,
    pub row: usize,
    pub right_side: bool,
    pub inside: bool,
}

/// Resolve a position (relative to the grid's top-left corner, in pixels) to a
/// cell, clamping into the `cols x rows` grid.
pub fn cell_from_pixel(
    rel_x: f32,
    rel_y: f32,
    cell_w: f32,
    line_h: f32,
    cols: u16,
    rows: u16,
) -> CellHit {
    let max_col = cols.max(1) as usize - 1;
    let max_row = rows.max(1) as usize - 1;
    let inside = rel_x >= 0.0
        && rel_y >= 0.0
        && rel_x < cols as f32 * cell_w
        && rel_y < rows as f32 * line_h;
    let col_f = (rel_x / cell_w).max(0.0);
    let col = (col_f.floor() as usize).min(max_col);
    let row = ((rel_y / line_h).max(0.0).floor() as usize).min(max_row);
    let right_side = col_f - col_f.floor() > 0.5;
    CellHit { col, row, right_side, inside }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chord(key: &str, key_char: Option<&str>) -> KeyChord {
        KeyChord { key: key.to_string(), key_char: key_char.map(String::from), ..Default::default() }
    }

    fn modes() -> ModeInfo {
        ModeInfo::default()
    }

    #[test]
    fn printable_uses_key_char() {
        assert_eq!(encode(&chord("a", Some("a")), &modes()), Some(b"a".to_vec()));
        // Shifted char comes through key_char.
        let mut k = chord("1", Some("!"));
        k.shift = true;
        assert_eq!(encode(&k, &modes()), Some(b"!".to_vec()));
    }

    #[test]
    fn named_keys_encode_to_escape_sequences() {
        assert_eq!(encode(&chord("enter", None), &modes()), Some(b"\r".to_vec()));
        assert_eq!(encode(&chord("backspace", None), &modes()), Some(b"\x7f".to_vec()));
        assert_eq!(encode(&chord("escape", None), &modes()), Some(b"\x1b".to_vec()));
        assert_eq!(encode(&chord("up", None), &modes()), Some(b"\x1b[A".to_vec()));
        assert_eq!(encode(&chord("down", None), &modes()), Some(b"\x1b[B".to_vec()));
        assert_eq!(encode(&chord("right", None), &modes()), Some(b"\x1b[C".to_vec()));
        assert_eq!(encode(&chord("left", None), &modes()), Some(b"\x1b[D".to_vec()));
        assert_eq!(encode(&chord("home", None), &modes()), Some(b"\x1b[H".to_vec()));
        assert_eq!(encode(&chord("pageup", None), &modes()), Some(b"\x1b[5~".to_vec()));
    }

    #[test]
    fn app_cursor_mode_switches_arrows_to_ss3() {
        let m = ModeInfo { app_cursor: true, ..Default::default() };
        assert_eq!(encode(&chord("up", None), &m), Some(b"\x1bOA".to_vec()));
        assert_eq!(encode(&chord("end", None), &m), Some(b"\x1bOF".to_vec()));
        // Modifiers force the CSI form even in app-cursor mode.
        let mut k = chord("up", None);
        k.control = true;
        assert_eq!(encode(&k, &m), Some(b"\x1b[1;5A".to_vec()));
    }

    #[test]
    fn modified_arrows_and_tilde_keys_carry_the_modifier() {
        let mut k = chord("left", None);
        k.shift = true;
        k.alt = true;
        assert_eq!(encode(&k, &modes()), Some(b"\x1b[1;4D".to_vec()));
        let mut k = chord("delete", None);
        k.control = true;
        assert_eq!(encode(&k, &modes()), Some(b"\x1b[3;5~".to_vec()));
        let mut k = chord("pageup", None);
        k.shift = true;
        assert_eq!(encode(&k, &modes()), Some(b"\x1b[5;2~".to_vec()));
    }

    #[test]
    fn function_keys_encode() {
        assert_eq!(encode(&chord("f1", None), &modes()), Some(b"\x1bOP".to_vec()));
        assert_eq!(encode(&chord("f4", None), &modes()), Some(b"\x1bOS".to_vec()));
        assert_eq!(encode(&chord("f5", None), &modes()), Some(b"\x1b[15~".to_vec()));
        assert_eq!(encode(&chord("f12", None), &modes()), Some(b"\x1b[24~".to_vec()));
        let mut k = chord("f1", None);
        k.control = true;
        assert_eq!(encode(&k, &modes()), Some(b"\x1b[1;5P".to_vec()));
        let mut k = chord("f5", None);
        k.shift = true;
        assert_eq!(encode(&k, &modes()), Some(b"\x1b[15;2~".to_vec()));
    }

    #[test]
    fn shift_tab_is_cbt() {
        assert_eq!(encode(&chord("tab", None), &modes()), Some(b"\t".to_vec()));
        let mut k = chord("tab", None);
        k.shift = true;
        assert_eq!(encode(&k, &modes()), Some(b"\x1b[Z".to_vec()));
    }

    #[test]
    fn ctrl_letter_maps_to_control_byte() {
        for (key, byte) in [("c", 3u8), ("a", 1), ("d", 4)] {
            let mut k = chord(key, Some(key));
            k.control = true;
            assert_eq!(encode(&k, &modes()), Some(vec![byte]), "ctrl-{key}");
        }
        let mut k = chord("backspace", None);
        k.control = true;
        assert_eq!(encode(&k, &modes()), Some(b"\x08".to_vec()));
    }

    #[test]
    fn alt_prefixes_escape_and_altgr_types_text() {
        let mut k = chord("b", Some("b"));
        k.alt = true;
        assert_eq!(encode(&k, &modes()), Some(vec![0x1b, b'b']));
        // AltGr (ctrl+alt with a produced char) types the char with no prefix.
        let mut k = chord("q", Some("@"));
        k.control = true;
        k.alt = true;
        assert_eq!(encode(&k, &modes()), Some(b"@".to_vec()));
    }

    #[test]
    fn platform_chord_is_not_sent_to_the_pty() {
        // Cmd/Win/Super chords are app shortcuts, not terminal input.
        let mut k = chord("c", Some("c"));
        k.platform = true;
        assert_eq!(encode(&k, &modes()), None);
    }

    #[test]
    fn key_action_routes_app_and_scroll_chords() {
        let mut copy = chord("c", None);
        copy.control = true;
        copy.shift = true;
        assert_eq!(key_action(&copy, &modes()), KeyAction::Copy);
        let mut paste = chord("v", None);
        paste.control = true;
        paste.shift = true;
        assert_eq!(key_action(&paste, &modes()), KeyAction::Paste);
        let mut find = chord("f", None);
        find.control = true;
        find.shift = true;
        assert_eq!(key_action(&find, &modes()), KeyAction::OpenSearch);

        let mut pgup = chord("pageup", None);
        pgup.shift = true;
        assert_eq!(key_action(&pgup, &modes()), KeyAction::ScrollPageUp);
        let mut home = chord("home", None);
        home.shift = true;
        assert_eq!(key_action(&home, &modes()), KeyAction::ScrollTop);

        // In the alt screen the app owns paging: Shift+PageUp goes to the PTY.
        let alt = ModeInfo { alt_screen: true, ..Default::default() };
        assert_eq!(key_action(&pgup, &alt), KeyAction::Write(b"\x1b[5;2~".to_vec()));

        // Plain keys still write; Ctrl+C is terminal input, not Copy.
        assert_eq!(key_action(&chord("a", Some("a")), &modes()), KeyAction::Write(b"a".to_vec()));
        let mut cc = chord("c", Some("c"));
        cc.control = true;
        assert_eq!(key_action(&cc, &modes()), KeyAction::Write(vec![3]));
    }

    #[test]
    fn mouse_sgr_encoding() {
        let m = ModeInfo { sgr_mouse: true, ..Default::default() };
        let none = (false, false, false);
        assert_eq!(encode_mouse(MouseKind::Press, 0, (4, 2), none, &m), b"\x1b[<0;5;3M".to_vec());
        assert_eq!(encode_mouse(MouseKind::Release, 0, (4, 2), none, &m), b"\x1b[<0;5;3m".to_vec());
        assert_eq!(encode_mouse(MouseKind::Motion, 0, (4, 2), none, &m), b"\x1b[<32;5;3M".to_vec());
        assert_eq!(
            encode_mouse(MouseKind::Press, MOUSE_WHEEL_UP, (0, 0), none, &m),
            b"\x1b[<64;1;1M".to_vec()
        );
        // Modifiers add 4/8/16.
        assert_eq!(
            encode_mouse(MouseKind::Press, 2, (0, 0), (false, false, true), &m),
            b"\x1b[<18;1;1M".to_vec()
        );
    }

    #[test]
    fn mouse_legacy_encoding_clamps_and_utf8_extends() {
        let m = modes(); // neither SGR nor UTF-8
        let none = (false, false, false);
        assert_eq!(
            encode_mouse(MouseKind::Press, 0, (4, 2), none, &m),
            vec![0x1b, b'[', b'M', 32, 37, 35]
        );
        // Legacy release loses the button (always 3).
        assert_eq!(
            encode_mouse(MouseKind::Release, 2, (4, 2), none, &m),
            vec![0x1b, b'[', b'M', 35, 37, 35]
        );
        // Coordinates clamp at one byte...
        assert_eq!(
            encode_mouse(MouseKind::Press, 0, (300, 0), none, &m),
            vec![0x1b, b'[', b'M', 32, 255, 33]
        );
        // ...unless UTF-8 mouse extends them (333 = 0x14D -> two UTF-8 bytes).
        let m = ModeInfo { utf8_mouse: true, ..Default::default() };
        assert_eq!(
            encode_mouse(MouseKind::Press, 0, (300, 0), none, &m),
            vec![0x1b, b'[', b'M', 32, 0xC5, 0x8D, 33]
        );
    }

    #[test]
    fn alt_scroll_sends_arrows() {
        assert_eq!(alt_scroll_bytes(2, false), b"\x1b[A\x1b[A".to_vec());
        assert_eq!(alt_scroll_bytes(-1, true), b"\x1bOB".to_vec());
        assert!(alt_scroll_bytes(0, false).is_empty());
    }

    #[test]
    fn paste_frames_and_normalizes() {
        assert_eq!(encode_paste("hi", false), b"hi".to_vec());
        assert_eq!(encode_paste("a\r\nb\nc", false), b"a\rb\rc".to_vec());
        assert_eq!(encode_paste("hi", true), b"\x1b[200~hi\x1b[201~".to_vec());
        // Embedded end marker is stripped (paste injection guard).
        assert_eq!(
            encode_paste("a\x1b[201~b", true),
            b"\x1b[200~ab\x1b[201~".to_vec()
        );
    }

    #[test]
    fn search_keys_route() {
        assert_eq!(search_key(&chord("enter", None)), SearchKey::Next);
        let mut k = chord("enter", None);
        k.shift = true;
        assert_eq!(search_key(&k), SearchKey::Prev);
        assert_eq!(search_key(&chord("escape", None)), SearchKey::Close);
        assert_eq!(search_key(&chord("backspace", None)), SearchKey::Backspace);
        assert_eq!(search_key(&chord("x", Some("x"))), SearchKey::Input("x".into()));
        let mut k = chord("f", None);
        k.control = true;
        k.shift = true;
        assert_eq!(search_key(&k), SearchKey::Close);
        let mut k = chord("a", Some("a"));
        k.control = true;
        assert_eq!(search_key(&k), SearchKey::Ignore);
    }

    #[test]
    fn cell_from_pixel_resolves_and_clamps() {
        let hit = cell_from_pixel(25.0, 40.0, 10.0, 16.0, 80, 24);
        assert_eq!((hit.col, hit.row), (2, 2));
        assert!(!hit.right_side);
        assert!(hit.inside);
        let hit = cell_from_pixel(27.0, 40.0, 10.0, 16.0, 80, 24);
        assert!(hit.right_side);
        // Outside clamps and reports it.
        let hit = cell_from_pixel(-5.0, 10_000.0, 10.0, 16.0, 80, 24);
        assert_eq!((hit.col, hit.row), (0, 23));
        assert!(!hit.inside);
    }

    #[test]
    fn grid_dims_are_near_square() {
        assert_eq!(grid_dims(12), (4, 3));
        assert_eq!(grid_dims(16), (4, 4));
        assert_eq!(grid_dims(9), (3, 3));
        assert_eq!(grid_dims(4), (2, 2));
        assert_eq!(grid_dims(1), (1, 1));
        assert_eq!(grid_dims(0), (1, 1));
    }
}
