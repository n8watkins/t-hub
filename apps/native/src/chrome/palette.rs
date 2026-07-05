//! The command palette model (T-A): the webview `CommandPalette.tsx` ported
//! gpui-free - fuzzy search over the 22-command registry, keyboard
//! navigation, execute-on-enter, and the rebind capture flow.
//!
//! Everything here is plain state + reducers; the gui layer
//! (`chrome/palette_view.rs`) only draws what [`PaletteState`] says. Input
//! reaches [`PaletteState::handle_key`] through the keymap controller, which
//! gives the open palette the WHOLE keyboard (the webview equivalent: the
//! palette's search box is an editable target, so the global keymap stands
//! down while it is open).
//!
//! Deviation from the webview (documented in the T-A results entry): the
//! native palette is keyboard-only in this slice - rebind starts with `F2` (or
//! `Ctrl+R`) on the highlighted row instead of a per-row mouse button, and
//! rows are chosen with arrows + enter. The rebind flow itself is webview-exact: the next
//! chord binds (conflicts strip from the old owner, persisted immediately),
//! `Esc` cancels, lone modifiers don't bind.

use crate::chrome::actions::{CommandId, ALL_COMMANDS};
use crate::chrome::keymap::{Chord, Keymap};
use crate::render_support::KeyChord;

/// Case-insensitive subsequence match with the webview's exact scoring
/// (`fuzzyScore`): lower is better; gaps between matched characters and a
/// late first hit both cost. `None` when `query` is not a subsequence.
/// An empty query matches everything at 0.
pub fn fuzzy_score(text: &str, query: &str) -> Option<i64> {
    if query.is_empty() {
        return Some(0);
    }
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let q: Vec<char> = query.to_lowercase().chars().collect();
    let mut ti = 0usize;
    let mut score: i64 = 0;
    let mut first_idx: i64 = -1;
    let mut prev_idx: i64 = -1;
    for &ch in &q {
        let found = t[ti.min(t.len())..].iter().position(|&c| c == ch).map(|p| p + ti)?;
        if first_idx == -1 {
            first_idx = found as i64;
        }
        if prev_idx != -1 {
            score += found as i64 - prev_idx - 1;
        }
        prev_idx = found as i64;
        ti = found + 1;
    }
    Some(score + first_idx)
}

/// Score every registry command against the query: haystack is
/// `"{label} {description} {category}"`, sorted by score with the registry
/// order as the tie-break (webview parity).
pub fn results_for(query: &str) -> Vec<CommandId> {
    let mut hits: Vec<(i64, usize, CommandId)> = ALL_COMMANDS
        .iter()
        .enumerate()
        .filter_map(|(i, &cmd)| {
            let hay = format!("{} {} {}", cmd.label(), cmd.description(), cmd.category());
            fuzzy_score(&hay, query).map(|s| (s, i, cmd))
        })
        .collect();
    hits.sort_by_key(|&(s, i, _)| (s, i));
    hits.into_iter().map(|(_, _, cmd)| cmd).collect()
}

/// What a palette keystroke did (the controller translates these into
/// [`crate::chrome::actions::Effect`]s / a keymap save).
#[derive(Debug, PartialEq, Eq)]
pub enum PaletteOutcome {
    /// Consumed with no outward change (typing, navigation).
    Consumed,
    /// A rebind landed: the keymap changed, persist it.
    BindingsChanged,
    /// Run this command (the palette closed itself first).
    Execute(CommandId),
    /// The palette closed without executing.
    Closed,
}

/// The palette's whole UI state; the view draws exactly this.
#[derive(Default)]
pub struct PaletteState {
    pub open: bool,
    pub query: String,
    pub selected: usize,
    /// A row's rebind capture in progress: the next chord binds this command.
    pub rebind: Option<CommandId>,
    pub results: Vec<CommandId>,
}

impl PaletteState {
    /// Open fresh (webview opens with an empty query; state never leaks
    /// between opens). The keymap is unused today but keeps the signature
    /// honest for binding-aware result rows.
    pub fn open(&mut self, _keymap: &Keymap) {
        self.open = true;
        self.query.clear();
        self.selected = 0;
        self.rebind = None;
        self.results = results_for("");
    }

    pub fn close(&mut self) {
        self.open = false;
        self.rebind = None;
    }

    fn requery(&mut self) {
        self.results = results_for(&self.query);
        if self.selected >= self.results.len() {
            self.selected = self.results.len().saturating_sub(1);
        }
    }

    /// One keystroke while open. The caller guarantees `self.open`.
    pub fn handle_key(&mut self, kc: &KeyChord, keymap: &mut Keymap) -> PaletteOutcome {
        // Rebind capture owns the keyboard (webview: a window-level capture
        // listener): Esc cancels, lone modifiers wait, any chord binds.
        if let Some(cmd) = self.rebind {
            if kc.key == "escape" {
                self.rebind = None;
                return PaletteOutcome::Consumed;
            }
            let Some(chord) = Chord::from_key(kc) else {
                return PaletteOutcome::Consumed;
            };
            keymap.set_direct(cmd, Some(chord));
            self.rebind = None;
            return PaletteOutcome::BindingsChanged;
        }

        // Ctrl+R = rebind, the F2 alias: WSLg/RAIL swallows F-keys, and inside
        // the open palette Ctrl+R is free (the palette owns the keyboard).
        if kc.control && !kc.alt && kc.key == "r" {
            self.rebind = self.results.get(self.selected).copied();
            return PaletteOutcome::Consumed;
        }

        match kc.key.as_str() {
            "escape" => {
                self.close();
                return PaletteOutcome::Closed;
            }
            "enter" => {
                if let Some(&cmd) = self.results.get(self.selected) {
                    self.close();
                    return PaletteOutcome::Execute(cmd);
                }
                return PaletteOutcome::Consumed;
            }
            "down" => {
                if !self.results.is_empty() {
                    self.selected = (self.selected + 1) % self.results.len();
                }
                return PaletteOutcome::Consumed;
            }
            "up" => {
                if !self.results.is_empty() {
                    self.selected =
                        (self.selected + self.results.len() - 1) % self.results.len();
                }
                return PaletteOutcome::Consumed;
            }
            "f2" => {
                self.rebind = self.results.get(self.selected).copied();
                return PaletteOutcome::Consumed;
            }
            "backspace" => {
                self.query.pop();
                self.requery();
                return PaletteOutcome::Consumed;
            }
            _ => {}
        }

        // Printable input extends the query (same acceptance as rename mode:
        // a produced char, no ctrl/platform, no control chars).
        if !kc.control && !kc.platform {
            if let Some(ch) = kc.key_char.as_deref() {
                if !ch.is_empty() && !ch.chars().any(char::is_control) {
                    self.query.push_str(ch);
                    self.requery();
                    return PaletteOutcome::Consumed;
                }
            }
        }

        // Everything else is swallowed while the palette is open.
        PaletteOutcome::Consumed
    }
}

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn kc(key: &str) -> KeyChord {
        KeyChord { key: key.to_string(), key_char: Some(key.to_string()), ..KeyChord::default() }
    }

    fn named(key: &str) -> KeyChord {
        KeyChord { key: key.to_string(), key_char: None, ..KeyChord::default() }
    }

    fn open_palette() -> (PaletteState, Keymap) {
        let mut p = PaletteState::default();
        let k = Keymap::default();
        p.open(&k);
        (p, k)
    }

    // -- fuzzy ------------------------------------------------------------------

    #[test]
    fn empty_query_matches_all_at_zero() {
        assert_eq!(fuzzy_score("anything", ""), Some(0));
        assert_eq!(results_for("").len(), ALL_COMMANDS.len());
    }

    #[test]
    fn non_subsequence_misses() {
        assert_eq!(fuzzy_score("zoom in", "xq"), None);
    }

    #[test]
    fn tight_match_beats_scattered() {
        // "zo" in "zoom" is adjacent from index 0 = 0; in "z_o" it gaps.
        assert_eq!(fuzzy_score("zoom", "zo"), Some(0));
        assert_eq!(fuzzy_score("z o", "zo"), Some(1));
        assert!(fuzzy_score("zoom", "zo") < fuzzy_score("z o", "zo"));
    }

    #[test]
    fn earlier_first_hit_wins() {
        assert!(fuzzy_score("in front", "in") < fuzzy_score("margin", "in"));
    }

    #[test]
    fn case_insensitive() {
        assert_eq!(fuzzy_score("Zoom In", "zi"), fuzzy_score("zoom in", "ZI"));
    }

    #[test]
    fn results_prefer_score_then_registry_order() {
        let r = results_for("zoom");
        assert_eq!(r[0], CommandId::ZoomIn, "first zoom-labeled command in registry order");
        assert!(r.contains(&CommandId::ZoomOut) && r.contains(&CommandId::ZoomReset));
        // A query that matches nothing empties the list.
        assert!(results_for("qqqq").is_empty());
    }

    // -- state machine ------------------------------------------------------------

    #[test]
    fn open_resets_state() {
        let (mut p, k) = open_palette();
        p.query = "junk".into();
        p.selected = 5;
        p.open(&k);
        assert!(p.open && p.query.is_empty() && p.selected == 0 && p.rebind.is_none());
        assert_eq!(p.results.len(), ALL_COMMANDS.len());
    }

    #[test]
    fn typing_filters_and_clamps_selection() {
        let (mut p, mut k) = open_palette();
        p.selected = 20;
        for ch in ["z", "o", "o", "m"] {
            assert_eq!(p.handle_key(&kc(ch), &mut k), PaletteOutcome::Consumed);
        }
        assert_eq!(p.query, "zoom");
        assert!(p.results.len() < ALL_COMMANDS.len());
        assert!(p.selected < p.results.len(), "selection clamped into range");
        p.handle_key(&named("backspace"), &mut k);
        assert_eq!(p.query, "zoo");
    }

    #[test]
    fn arrows_wrap_both_ways() {
        let (mut p, mut k) = open_palette();
        p.handle_key(&named("up"), &mut k);
        assert_eq!(p.selected, ALL_COMMANDS.len() - 1, "up from 0 wraps to the end");
        p.handle_key(&named("down"), &mut k);
        assert_eq!(p.selected, 0, "down from the end wraps to 0");
    }

    #[test]
    fn enter_executes_and_closes() {
        let (mut p, mut k) = open_palette();
        p.handle_key(&named("down"), &mut k);
        let expected = p.results[1];
        assert_eq!(p.handle_key(&named("enter"), &mut k), PaletteOutcome::Execute(expected));
        assert!(!p.open);
    }

    #[test]
    fn escape_closes_without_executing() {
        let (mut p, mut k) = open_palette();
        assert_eq!(p.handle_key(&named("escape"), &mut k), PaletteOutcome::Closed);
        assert!(!p.open);
    }

    #[test]
    fn rebind_captures_a_chord_and_persists_conflict_free() {
        let (mut p, mut k) = open_palette();
        // Highlight "Close terminal" (row 1 in registry order on empty query).
        p.handle_key(&named("down"), &mut k);
        assert_eq!(p.handle_key(&named("f2"), &mut k), PaletteOutcome::Consumed);
        assert_eq!(p.rebind, Some(CommandId::CloseTerminal));
        // A lone modifier waits.
        let mut shift = named("shift");
        shift.shift = true;
        assert_eq!(p.handle_key(&shift, &mut k), PaletteOutcome::Consumed);
        assert!(p.rebind.is_some());
        // The chord binds; the old owner (spawnTerminal held ctrl+t) is stripped.
        let mut chord = named("t");
        chord.control = true;
        assert_eq!(p.handle_key(&chord, &mut k), PaletteOutcome::BindingsChanged);
        assert!(p.rebind.is_none());
        assert_eq!(
            k.direct_for(&Chord::parse("ctrl+t").unwrap()),
            Some(CommandId::CloseTerminal)
        );
        assert_eq!(k.direct_of(CommandId::SpawnTerminal), None);
        assert!(p.open, "rebinding keeps the palette open");
    }

    #[test]
    fn ctrl_r_also_starts_rebind() {
        let (mut p, mut k) = open_palette();
        let mut ctrl_r = named("r");
        ctrl_r.control = true;
        assert_eq!(p.handle_key(&ctrl_r, &mut k), PaletteOutcome::Consumed);
        assert_eq!(p.rebind, Some(p.results[0]));
        // Plain `r` still types into the query.
        p.rebind = None;
        p.handle_key(&kc("r"), &mut k);
        assert_eq!(p.query, "r");
    }

    #[test]
    fn escape_cancels_rebind_but_keeps_palette() {
        let (mut p, mut k) = open_palette();
        p.handle_key(&named("f2"), &mut k);
        assert!(p.rebind.is_some());
        assert_eq!(p.handle_key(&named("escape"), &mut k), PaletteOutcome::Consumed);
        assert!(p.rebind.is_none());
        assert!(p.open);
    }

    #[test]
    fn unhandled_keys_are_swallowed() {
        let (mut p, mut k) = open_palette();
        let mut ctrl_x = named("x");
        ctrl_x.control = true;
        assert_eq!(p.handle_key(&ctrl_x, &mut k), PaletteOutcome::Consumed);
        assert!(p.query.is_empty(), "ctrl-chords do not type into the query");
    }

    #[test]
    fn enter_on_empty_results_is_consumed() {
        let (mut p, mut k) = open_palette();
        for ch in ["q", "q", "q", "q"] {
            p.handle_key(&kc(ch), &mut k);
        }
        assert!(p.results.is_empty());
        assert_eq!(p.handle_key(&named("enter"), &mut k), PaletteOutcome::Consumed);
        assert!(p.open, "nothing to run - stays open");
    }
}
