//! **gpui-free render helpers** (native-pivot T5): the pure logic behind the render
//! layer - keyboard-to-PTY encoding and the tile layout math.
//!
//! These live outside `render/` (which is gpui-gated) precisely so they unit-test in
//! WSL under `--no-default-features`: the GPUI backend links system graphics libs
//! that the WSL box may lack, so gui-feature test binaries can't link there (see the
//! T4 Verify note). Keeping the testable logic gpui-free sidesteps that entirely.

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

/// Encode a keystroke into terminal input bytes. Baseline coverage for T5; T6 does
/// the full job (app-cursor mode, bracketed paste, kitty protocol, mouse reporting).
pub fn encode(k: &KeyChord) -> Option<Vec<u8>> {
    let key = k.key.as_str();

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

    let named: Option<&[u8]> = match key {
        "enter" => Some(b"\r"),
        "escape" => Some(b"\x1b"),
        "backspace" => Some(b"\x7f"),
        "tab" => {
            if k.shift {
                Some(b"\x1b[Z")
            } else {
                Some(b"\t")
            }
        }
        "up" => Some(b"\x1b[A"),
        "down" => Some(b"\x1b[B"),
        "right" => Some(b"\x1b[C"),
        "left" => Some(b"\x1b[D"),
        "home" => Some(b"\x1b[H"),
        "end" => Some(b"\x1b[F"),
        "pageup" => Some(b"\x1b[5~"),
        "pagedown" => Some(b"\x1b[6~"),
        "insert" => Some(b"\x1b[2~"),
        "delete" => Some(b"\x1b[3~"),
        "space" => Some(b" "),
        _ => None,
    };
    if let Some(bytes) = named {
        return Some(with_alt(k.alt, bytes.to_vec()));
    }

    // Printable: prefer key_char (respects shift/layout), fall back to a 1-char key.
    if !k.platform {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn chord(key: &str, key_char: Option<&str>) -> KeyChord {
        KeyChord { key: key.to_string(), key_char: key_char.map(String::from), ..Default::default() }
    }

    #[test]
    fn printable_uses_key_char() {
        assert_eq!(encode(&chord("a", Some("a"))), Some(b"a".to_vec()));
        // Shifted char comes through key_char.
        let mut k = chord("1", Some("!"));
        k.shift = true;
        assert_eq!(encode(&k), Some(b"!".to_vec()));
    }

    #[test]
    fn named_keys_encode_to_escape_sequences() {
        assert_eq!(encode(&chord("enter", None)), Some(b"\r".to_vec()));
        assert_eq!(encode(&chord("backspace", None)), Some(b"\x7f".to_vec()));
        assert_eq!(encode(&chord("escape", None)), Some(b"\x1b".to_vec()));
        assert_eq!(encode(&chord("up", None)), Some(b"\x1b[A".to_vec()));
        assert_eq!(encode(&chord("down", None)), Some(b"\x1b[B".to_vec()));
        assert_eq!(encode(&chord("right", None)), Some(b"\x1b[C".to_vec()));
        assert_eq!(encode(&chord("left", None)), Some(b"\x1b[D".to_vec()));
        assert_eq!(encode(&chord("home", None)), Some(b"\x1b[H".to_vec()));
        assert_eq!(encode(&chord("pageup", None)), Some(b"\x1b[5~".to_vec()));
    }

    #[test]
    fn shift_tab_is_cbt() {
        assert_eq!(encode(&chord("tab", None)), Some(b"\t".to_vec()));
        let mut k = chord("tab", None);
        k.shift = true;
        assert_eq!(encode(&k), Some(b"\x1b[Z".to_vec()));
    }

    #[test]
    fn ctrl_letter_maps_to_control_byte() {
        for (key, byte) in [("c", 3u8), ("a", 1), ("d", 4)] {
            let mut k = chord(key, Some(key));
            k.control = true;
            assert_eq!(encode(&k), Some(vec![byte]), "ctrl-{key}");
        }
    }

    #[test]
    fn alt_prefixes_escape() {
        let mut k = chord("b", Some("b"));
        k.alt = true;
        assert_eq!(encode(&k), Some(vec![0x1b, b'b']));
    }

    #[test]
    fn platform_chord_is_not_sent_to_the_pty() {
        // Cmd/Win/Super chords are app shortcuts, not terminal input.
        let mut k = chord("c", Some("c"));
        k.platform = true;
        assert_eq!(encode(&k), None);
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
