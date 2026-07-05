//! The hybrid keymap (T-A): the webview's three-tier key routing
//! (`prefixKeyHandler.ts` + `store/keybindings.ts` + `Canvas.tsx` capture
//! handler) ported gpui-free.
//!
//! ## The three tiers (checked in this order, before any key reaches a tile)
//! 1. **Guard** - an "editable target" (native: the focused tile's find bar;
//!    rename mode never reaches the keymap) disarms the prefix and passes the
//!    key through untouched.
//! 2. **Prefix mode** - after the leader chord (`ctrl+b` by default) arms, the
//!    next key within 1.5s resolves a PREFIXED binding by its bare key
//!    (modifiers ignored); tapping the leader again sends the literal control
//!    byte to the terminal (tmux parity: `ctrl+b ctrl+b` types `0x02`); an
//!    unbound key disarms and falls through.
//! 3. **Direct chords** - `ctrl+t` / `ctrl+w` / `ctrl+tab` / `ctrl+1..9` /
//!    `ctrl+j` / `ctrl+k` / the zoom trio dispatch registry commands
//!    immediately. Unbound chords fall through to the tile.
//!
//! Bindings are REBINDABLE with the webview's conflict rule: assigning a chord
//! (or a prefixed bare key) strips it from any other command in the same tier,
//! atomically. The whole keymap persists as JSON at
//! `~/.t-hub/native-keymap.json` (`THN_KEYMAP` overrides), the same
//! `{prefixKey, direct, prefixed}` shape as the webview's
//! `t-hub.keybindings.v1` localStorage entry, sanitized the same way on load
//! (unknown ids and unparseable chords drop; a corrupt file falls back to the
//! defaults wholesale).
//!
//! [`KeyController`] bundles keymap + palette + focus region behind the ONE
//! entry point the view calls per keystroke ([`KeyController::on_key`]);
//! everything it returns is plain data ([`Handled`]), so the full dispatch
//! stack tests under `--no-default-features`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use crate::chrome::actions::{execute, CommandId, Effect, Region, ALL_COMMANDS};
use crate::chrome::model::ChromeModel;
use crate::chrome::palette::{PaletteOutcome, PaletteState};
use crate::render_support::KeyChord;

/// Webview `PREFIX_TIMEOUT_MS`: an armed prefix silently disarms after this.
pub const PREFIX_TIMEOUT_MS: u64 = 1_500;

// ---------------------------------------------------------------------------
// Chords
// ---------------------------------------------------------------------------

/// A normalized chord: modifiers + one lowercase key name, the unit both
/// binding maps and the persisted file speak. Cmd/platform folds into `ctrl`
/// (webview parity: macOS Cmd and Windows Ctrl are one binding).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chord {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub key: String,
}

/// gpui key names that are modifiers on their own (no chord to build).
fn is_modifier_name(key: &str) -> bool {
    matches!(
        key,
        "control" | "ctrl" | "shift" | "alt" | "platform" | "cmd" | "meta" | "win" | "function"
    )
}

impl Chord {
    /// Build from a keystroke; `None` for a lone modifier press.
    pub fn from_key(kc: &KeyChord) -> Option<Chord> {
        let key = kc.key.trim().to_lowercase();
        if key.is_empty() || is_modifier_name(&key) {
            return None;
        }
        Some(Chord {
            ctrl: kc.control || kc.platform,
            shift: kc.shift,
            alt: kc.alt,
            key,
        })
    }

    /// Parse a persisted chord string (webview `normalizeChord` semantics):
    /// `+`-joined, case-insensitive, `control`/`cmd`/`meta` fold into ctrl,
    /// `option` into alt, `plus` spells the `+` key. `None` when empty or
    /// modifier-only.
    pub fn parse(s: &str) -> Option<Chord> {
        let mut c = Chord { ctrl: false, shift: false, alt: false, key: String::new() };
        for part in s.split('+') {
            let part = part.trim().to_lowercase();
            match part.as_str() {
                "" => continue,
                "ctrl" | "control" | "cmd" | "meta" => c.ctrl = true,
                "shift" => c.shift = true,
                "alt" | "option" => c.alt = true,
                "plus" => c.key = "+".to_string(),
                k => c.key = k.to_string(),
            }
        }
        if c.key.is_empty() {
            return None;
        }
        Some(c)
    }

    /// Canonical persisted form: `ctrl+shift+alt+<key>` order, the `+` key
    /// spelled `plus` (webview chord format).
    pub fn to_persist(&self) -> String {
        let mut s = String::new();
        if self.ctrl {
            s.push_str("ctrl+");
        }
        if self.shift {
            s.push_str("shift+");
        }
        if self.alt {
            s.push_str("alt+");
        }
        s.push_str(if self.key == "+" { "plus" } else { &self.key });
        s
    }

    /// Pretty form for the palette and HUD: `Ctrl+Shift+Tab`.
    pub fn format(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if self.ctrl {
            parts.push("Ctrl".to_string());
        }
        if self.shift {
            parts.push("Shift".to_string());
        }
        if self.alt {
            parts.push("Alt".to_string());
        }
        let mut key = self.key.clone();
        if let Some(first) = key.get(..1) {
            let up = first.to_uppercase();
            key.replace_range(..1, &up);
        }
        parts.push(key);
        parts.join("+")
    }
}

/// The literal control byte a double-tapped prefix types into the terminal
/// (webview `literalForPrefix`): `ctrl+<letter>` maps to its C0 byte
/// (`ctrl+b` = 0x02); anything else has no literal.
pub fn literal_for_prefix(c: &Chord) -> Option<Vec<u8>> {
    if !c.ctrl || c.shift || c.alt {
        return None;
    }
    let mut chars = c.key.chars();
    let ch = chars.next()?;
    if chars.next().is_some() || !ch.is_ascii_lowercase() {
        return None;
    }
    Some(vec![ch as u8 - b'a' + 1])
}

/// The bare key of a keystroke, modifiers ignored (webview `bareKeyFromEvent`,
/// the prefixed-binding lookup: `ctrl+b` then `shift+W` still resolves `w`).
fn bare_key(kc: &KeyChord) -> Option<String> {
    let key = kc.key.trim().to_lowercase();
    if key.is_empty() || is_modifier_name(&key) {
        return None;
    }
    Some(key)
}

// ---------------------------------------------------------------------------
// The keymap
// ---------------------------------------------------------------------------

/// Both binding tiers plus the leader chord. Maps are command -> binding
/// (the webview's persisted direction); chord lookup scans - ~22 entries.
#[derive(Clone, Debug, PartialEq)]
pub struct Keymap {
    pub prefix: Chord,
    direct: BTreeMap<CommandId, Chord>,
    prefixed: BTreeMap<CommandId, String>,
}

impl Default for Keymap {
    /// The webview's seeded defaults (`store/keybindings.ts:43-90`), verbatim.
    fn default() -> Self {
        let mut direct = BTreeMap::new();
        for (cmd, chord) in [
            (CommandId::SpawnTerminal, "ctrl+t"),
            (CommandId::CloseTerminal, "ctrl+w"),
            (CommandId::KillSession, "ctrl+shift+w"),
            (CommandId::CycleTileNext, "ctrl+tab"),
            (CommandId::CycleTilePrev, "ctrl+shift+tab"),
            (CommandId::FocusTab1, "ctrl+1"),
            (CommandId::FocusTab2, "ctrl+2"),
            (CommandId::FocusTab3, "ctrl+3"),
            (CommandId::FocusTab4, "ctrl+4"),
            (CommandId::FocusTab5, "ctrl+5"),
            (CommandId::FocusTab6, "ctrl+6"),
            (CommandId::FocusTab7, "ctrl+7"),
            (CommandId::FocusTab8, "ctrl+8"),
            (CommandId::FocusTab9, "ctrl+9"),
            (CommandId::ZoomIn, "ctrl+="),
            (CommandId::ZoomOut, "ctrl+-"),
            (CommandId::ZoomReset, "ctrl+0"),
            (CommandId::ToggleFocusRegion, "ctrl+j"),
            (CommandId::CommandPalette, "ctrl+k"),
        ] {
            direct.insert(cmd, Chord::parse(chord).expect("default chord parses"));
        }
        let mut prefixed = BTreeMap::new();
        for (cmd, key) in [
            (CommandId::NewPlainWorkspace, "c"),
            (CommandId::NewWorktreeWorkspace, "w"),
            (CommandId::SpawnTerminal, "t"),
            (CommandId::CloseTerminal, "x"),
            (CommandId::CommandPalette, "p"),
            (CommandId::ToggleFocusRegion, "o"),
            (CommandId::CycleTileNext, "n"),
            (CommandId::CycleTilePrev, "b"),
            (CommandId::OpenWorktreesList, "l"),
        ] {
            prefixed.insert(cmd, key.to_string());
        }
        Keymap { prefix: Chord::parse("ctrl+b").expect("default prefix"), direct, prefixed }
    }
}

impl Keymap {
    pub fn direct_for(&self, chord: &Chord) -> Option<CommandId> {
        self.direct.iter().find(|(_, c)| *c == chord).map(|(cmd, _)| *cmd)
    }

    pub fn prefixed_for(&self, key: &str) -> Option<CommandId> {
        self.prefixed.iter().find(|(_, k)| k.as_str() == key).map(|(cmd, _)| *cmd)
    }

    pub fn direct_of(&self, cmd: CommandId) -> Option<&Chord> {
        self.direct.get(&cmd)
    }

    pub fn prefixed_of(&self, cmd: CommandId) -> Option<&str> {
        self.prefixed.get(&cmd).map(String::as_str)
    }

    /// Rebind a command's direct chord, stripping the chord from any other
    /// command first (webview `setBinding`: at most one owner per chord).
    /// `None` unbinds.
    pub fn set_direct(&mut self, cmd: CommandId, chord: Option<Chord>) {
        match chord {
            Some(chord) => {
                self.direct.retain(|other, c| *other == cmd || *c != chord);
                self.direct.insert(cmd, chord);
            }
            None => {
                self.direct.remove(&cmd);
            }
        }
    }

    /// Rebind a command's prefixed bare key (webview `setPrefixedBinding`).
    pub fn set_prefixed(&mut self, cmd: CommandId, key: Option<String>) {
        match key {
            Some(key) => {
                let key = key.trim().to_lowercase();
                if key.is_empty() {
                    self.prefixed.remove(&cmd);
                    return;
                }
                self.prefixed.retain(|other, k| *other == cmd || *k != key);
                self.prefixed.insert(cmd, key);
            }
            None => {
                self.prefixed.remove(&cmd);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

/// On-disk shape: the webview's `t-hub.keybindings.v1` value plus an explicit
/// version field (the persist.rs convention). Raw string maps - sanitizing
/// happens on load, exactly like the webview's `coercePersisted`.
#[derive(Debug, Serialize, Deserialize)]
struct KeymapFile {
    version: u32,
    #[serde(rename = "prefixKey")]
    prefix_key: String,
    #[serde(default)]
    direct: BTreeMap<String, String>,
    #[serde(default)]
    prefixed: BTreeMap<String, String>,
}

impl KeymapFile {
    fn from_keymap(k: &Keymap) -> KeymapFile {
        KeymapFile {
            version: 1,
            prefix_key: k.prefix.to_persist(),
            direct: k.direct.iter().map(|(c, ch)| (c.as_str().into(), ch.to_persist())).collect(),
            prefixed: k.prefixed.iter().map(|(c, k)| (c.as_str().into(), k.clone())).collect(),
        }
    }

    /// Sanitize into a keymap: unknown command ids and unparseable values
    /// drop silently; an unusable prefix falls back to `ctrl+b`. The file's
    /// maps REPLACE the defaults wholesale (an unbound default stays unbound),
    /// webview `coercePersisted` parity.
    fn into_keymap(self) -> Keymap {
        let prefix = Chord::parse(&self.prefix_key)
            .unwrap_or_else(|| Chord::parse("ctrl+b").expect("fallback prefix"));
        let mut direct = BTreeMap::new();
        for (id, chord) in self.direct {
            if let (Some(cmd), Some(c)) = (CommandId::parse(&id), Chord::parse(&chord)) {
                direct.insert(cmd, c);
            }
        }
        let mut prefixed = BTreeMap::new();
        for (id, key) in self.prefixed {
            let key = key.trim().to_lowercase();
            if let (Some(cmd), false) = (CommandId::parse(&id), key.is_empty()) {
                prefixed.insert(cmd, key);
            }
        }
        Keymap { prefix, direct, prefixed }
    }
}

/// The keymap file path: `THN_KEYMAP` override, else
/// `~/.t-hub/native-keymap.json` next to the layout file.
pub fn keymap_path() -> PathBuf {
    if let Ok(p) = std::env::var("THN_KEYMAP") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    let home = std::env::var("HOME")
        .ok()
        .or_else(|| std::env::var("USERPROFILE").ok())
        .unwrap_or_default();
    let mut p = PathBuf::from(home);
    p.push(".t-hub");
    p.push("native-keymap.json");
    p
}

/// Load the keymap, or `None` when the file is missing (first run - defaults).
/// A corrupt file is an error the caller downgrades to defaults (never fatal).
pub fn load(path: &Path) -> Result<Option<Keymap>> {
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("read keymap {}", path.display())),
    };
    let file: KeymapFile = serde_json::from_str(&raw)
        .with_context(|| format!("parse keymap {}", path.display()))?;
    Ok(Some(file.into_keymap()))
}

/// Save atomically (temp file + rename), the persist.rs convention.
pub fn save(path: &Path, keymap: &Keymap) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("create keymap dir {}", dir.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    let body = serde_json::to_string_pretty(&KeymapFile::from_keymap(keymap))
        .context("serialize keymap")?;
    std::fs::write(&tmp, body).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("rename {} over {}", tmp.display(), path.display()))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// The controller
// ---------------------------------------------------------------------------

/// What the view does with a keystroke after the controller saw it.
#[derive(Debug, PartialEq)]
pub enum Handled {
    /// Not consumed: route to the focused tile's terminal core as before.
    Pass,
    /// Consumed by the keymap/palette; run these effects (possibly none).
    Consumed(Vec<Effect>),
}

/// Keymap + palette + focus region behind one per-keystroke entry point. Owned
/// by the cockpit's shared state; every mutation runs on the gpui main thread
/// (the same lock discipline as the rest of `CockpitState`).
pub struct KeyController {
    pub keymap: Keymap,
    pub palette: PaletteState,
    pub region: Region,
    /// Arm timestamp (ms since the cockpit epoch) while the prefix waits for
    /// its second key.
    armed_at: Option<u64>,
    path: PathBuf,
}

impl KeyController {
    pub fn new(keymap: Keymap, path: PathBuf) -> Self {
        KeyController {
            keymap,
            palette: PaletteState::default(),
            region: Region::default(),
            armed_at: None,
            path,
        }
    }

    /// Boot: load the persisted keymap (defaults when missing; a corrupt file
    /// logs and falls back - the webview's load path).
    pub fn load_default() -> Self {
        let path = keymap_path();
        let keymap = match load(&path) {
            Ok(Some(k)) => k,
            Ok(None) => Keymap::default(),
            Err(e) => {
                log::warn!("keymap load failed (using defaults): {e:#}");
                Keymap::default()
            }
        };
        KeyController::new(keymap, path)
    }

    /// Persist the keymap (best-effort, like `save_layout`).
    fn save_keymap(&self) {
        if let Err(e) = save(&self.path, &self.keymap) {
            log::warn!("keymap save failed: {e:#}");
        }
    }

    /// Whether the prefix is armed at `now` (lazy expiry: no timer thread; the
    /// continuous repaint reads this every frame, so the HUD hides on time).
    pub fn armed(&mut self, now: u64) -> bool {
        if let Some(at) = self.armed_at {
            if now.saturating_sub(at) >= PREFIX_TIMEOUT_MS {
                self.armed_at = None;
            }
        }
        self.armed_at.is_some()
    }

    /// The HUD label while armed: the formatted leader chord.
    pub fn hud_label(&mut self, now: u64) -> Option<String> {
        self.armed(now).then(|| self.keymap.prefix.format())
    }

    /// Run a registry command. The palette open is controller state (the
    /// webview's `openPalette` callback); everything else executes.
    pub fn run(&mut self, cmd: CommandId, model: &mut ChromeModel) -> Vec<Effect> {
        if cmd == CommandId::CommandPalette {
            self.palette.open(&self.keymap);
            return Vec::new();
        }
        execute(cmd, model, &mut self.region)
    }

    /// The per-keystroke entry point, called by the view BEFORE the tile core
    /// (the webview's capture-phase handler). `guard` is the editable-target
    /// check (native: the focused tile's find bar owns the keyboard);
    /// `now` is ms since the cockpit epoch.
    pub fn on_key(
        &mut self,
        kc: &KeyChord,
        model: &mut ChromeModel,
        guard: bool,
        now: u64,
    ) -> Handled {
        // Tier 0: editable guard - disarm and pass (webview `isEditableTarget`).
        if guard {
            self.armed_at = None;
            return Handled::Pass;
        }

        // The palette owns the whole keyboard while open (its search box is
        // the webview's editable target; navigation/rebind keys are its own).
        if self.palette.open {
            self.armed_at = None;
            let outcome = self.palette.handle_key(kc, &mut self.keymap);
            return Handled::Consumed(match outcome {
                PaletteOutcome::Consumed => Vec::new(),
                PaletteOutcome::BindingsChanged => {
                    self.save_keymap();
                    Vec::new()
                }
                PaletteOutcome::Execute(cmd) => self.run(cmd, model),
                PaletteOutcome::Closed => Vec::new(),
            });
        }

        let Some(chord) = Chord::from_key(kc) else {
            // Lone modifier: never consumed, never disarms (a prefix chord
            // necessarily presses its modifier first).
            return Handled::Pass;
        };

        // Tier 1: prefix mode.
        if self.armed(now) {
            self.armed_at = None;
            if chord == self.keymap.prefix {
                // Double-tap: type the leader's literal control byte.
                return Handled::Consumed(match literal_for_prefix(&self.keymap.prefix) {
                    Some(bytes) => vec![Effect::Literal(bytes)],
                    None => Vec::new(),
                });
            }
            if let Some(key) = bare_key(kc) {
                if let Some(cmd) = self.keymap.prefixed_for(&key) {
                    return Handled::Consumed(self.run(cmd, model));
                }
            }
            // Unbound second key: disarmed, falls through to the tile.
            return Handled::Pass;
        }

        // Tier 2: arm the prefix.
        if chord == self.keymap.prefix {
            self.armed_at = Some(now);
            return Handled::Consumed(Vec::new());
        }

        // Tier 3: direct chords.
        if let Some(cmd) = self.keymap.direct_for(&chord) {
            return Handled::Consumed(self.run(cmd, model));
        }

        // Sidebar focus region: the keyboard drives the workspace list, not
        // the (invisible-to-focus) terminal. Arrows step the active workspace,
        // enter/escape hand focus back to the tiles, anything else is
        // swallowed (webview parity: a DOM-focused sidebar eats typing).
        if self.region == Region::Sidebar {
            match kc.key.as_str() {
                "down" => return Handled::Consumed(execute_sidebar_cycle(model, 1)),
                "up" => return Handled::Consumed(execute_sidebar_cycle(model, -1)),
                "enter" | "escape" => {
                    self.region = Region::Tiles;
                    return Handled::Consumed(Vec::new());
                }
                _ => return Handled::Consumed(Vec::new()),
            }
        }

        Handled::Pass
    }

    /// Rebind a direct chord from the palette's capture flow and persist.
    pub fn rebind_direct(&mut self, cmd: CommandId, chord: Chord) {
        self.keymap.set_direct(cmd, Some(chord));
        self.save_keymap();
    }
}

/// Arrow-key workspace stepping while the sidebar region holds focus: the
/// region-aware half of the cycle commands, without needing a binding.
fn execute_sidebar_cycle(model: &mut ChromeModel, dir: i64) -> Vec<Effect> {
    let mut region = Region::Sidebar;
    let cmd = if dir > 0 { CommandId::CycleTileNext } else { CommandId::CycleTilePrev };
    execute(cmd, model, &mut region)
}

/// The registry rows the palette shows: every command with its current
/// bindings rendered for display (`Ctrl+K` / `Ctrl+B W`).
pub fn binding_hint(keymap: &Keymap, cmd: CommandId) -> String {
    let direct = keymap.direct_of(cmd).map(Chord::format);
    let prefixed = keymap.prefixed_of(cmd).map(|k| {
        let mut key = k.to_uppercase();
        if key.is_empty() {
            key.push('?');
        }
        format!("{} {}", keymap.prefix.format(), key)
    });
    match (direct, prefixed) {
        (Some(d), Some(p)) => format!("{d} · {p}"),
        (Some(d), None) => d,
        (None, Some(p)) => p,
        (None, None) => "-".to_string(),
    }
}

/// Every command must be reachable: used by tests and the palette (a command
/// with no binding still lists and runs from the palette).
pub fn all_commands() -> impl Iterator<Item = CommandId> {
    ALL_COMMANDS.iter().copied()
}

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chrome::model::Workspace;

    fn kc(key: &str) -> KeyChord {
        KeyChord { key: key.to_string(), key_char: Some(key.to_string()), ..KeyChord::default() }
    }

    fn ctrl(key: &str) -> KeyChord {
        KeyChord { control: true, key: key.to_string(), key_char: None, ..KeyChord::default() }
    }

    fn model2() -> ChromeModel {
        let mk = |name: &str, tiles: &[&str]| {
            let mut w = Workspace::new(name);
            w.tiles = tiles.iter().map(|s| s.to_string()).collect();
            w
        };
        ChromeModel::from_layout(vec![mk("A", &["t1", "t2"]), mk("B", &["t3"])], 0)
    }

    fn controller() -> KeyController {
        let path = std::env::temp_dir()
            .join(format!("thn-keymap-test-{}-{:p}", std::process::id(), &PREFIX_TIMEOUT_MS));
        KeyController::new(Keymap::default(), path)
    }

    // -- chords ---------------------------------------------------------------

    #[test]
    fn chord_folds_platform_into_ctrl() {
        let mut k = kc("t");
        k.platform = true;
        let c = Chord::from_key(&k).unwrap();
        assert!(c.ctrl);
        assert_eq!(c.to_persist(), "ctrl+t");
    }

    #[test]
    fn lone_modifier_builds_no_chord() {
        assert_eq!(Chord::from_key(&kc("shift")), None);
        assert_eq!(Chord::from_key(&kc("control")), None);
        assert_eq!(Chord::from_key(&KeyChord::default()), None);
    }

    #[test]
    fn parse_normalizes_aliases_and_order() {
        let c = Chord::parse("Shift+Control+Tab").unwrap();
        assert_eq!(c.to_persist(), "ctrl+shift+tab");
        assert_eq!(Chord::parse("cmd+k").unwrap().to_persist(), "ctrl+k");
        assert_eq!(Chord::parse("option+x").unwrap().to_persist(), "alt+x");
        assert_eq!(Chord::parse("ctrl+plus").unwrap().key, "+");
        assert_eq!(Chord::parse("ctrl+plus").unwrap().to_persist(), "ctrl+plus");
        assert_eq!(Chord::parse(""), None);
        assert_eq!(Chord::parse("ctrl+shift"), None);
    }

    #[test]
    fn format_pretty_prints() {
        assert_eq!(Chord::parse("ctrl+shift+tab").unwrap().format(), "Ctrl+Shift+Tab");
        assert_eq!(Chord::parse("ctrl+=").unwrap().format(), "Ctrl+=");
    }

    #[test]
    fn literal_for_ctrl_letters_only() {
        assert_eq!(literal_for_prefix(&Chord::parse("ctrl+b").unwrap()), Some(vec![0x02]));
        assert_eq!(literal_for_prefix(&Chord::parse("ctrl+a").unwrap()), Some(vec![0x01]));
        assert_eq!(literal_for_prefix(&Chord::parse("ctrl+shift+b").unwrap()), None);
        assert_eq!(literal_for_prefix(&Chord::parse("ctrl+1").unwrap()), None);
        assert_eq!(literal_for_prefix(&Chord::parse("ctrl+tab").unwrap()), None);
    }

    // -- defaults -------------------------------------------------------------

    #[test]
    fn default_bindings_match_the_webview_seed() {
        let k = Keymap::default();
        assert_eq!(k.prefix.to_persist(), "ctrl+b");
        for (cmd, chord) in [
            (CommandId::SpawnTerminal, "ctrl+t"),
            (CommandId::CloseTerminal, "ctrl+w"),
            (CommandId::KillSession, "ctrl+shift+w"),
            (CommandId::CycleTileNext, "ctrl+tab"),
            (CommandId::CycleTilePrev, "ctrl+shift+tab"),
            (CommandId::FocusTab1, "ctrl+1"),
            (CommandId::FocusTab9, "ctrl+9"),
            (CommandId::ZoomIn, "ctrl+="),
            (CommandId::ZoomOut, "ctrl+-"),
            (CommandId::ZoomReset, "ctrl+0"),
            (CommandId::ToggleFocusRegion, "ctrl+j"),
            (CommandId::CommandPalette, "ctrl+k"),
        ] {
            assert_eq!(k.direct_of(cmd).unwrap().to_persist(), chord, "{cmd:?}");
        }
        for (cmd, key) in [
            (CommandId::NewPlainWorkspace, "c"),
            (CommandId::NewWorktreeWorkspace, "w"),
            (CommandId::SpawnTerminal, "t"),
            (CommandId::CloseTerminal, "x"),
            (CommandId::CommandPalette, "p"),
            (CommandId::ToggleFocusRegion, "o"),
            (CommandId::CycleTileNext, "n"),
            (CommandId::CycleTilePrev, "b"),
            (CommandId::OpenWorktreesList, "l"),
        ] {
            assert_eq!(k.prefixed_of(cmd), Some(key), "{cmd:?}");
        }
    }

    // -- dispatch -------------------------------------------------------------

    #[test]
    fn direct_chord_dispatches() {
        let mut c = controller();
        let mut m = model2();
        let h = c.on_key(&ctrl("2"), &mut m, false, 0);
        assert_eq!(h, Handled::Consumed(vec![Effect::PersistLayout]));
        assert_eq!(m.active, 1);
    }

    #[test]
    fn unbound_chord_passes() {
        let mut c = controller();
        let mut m = model2();
        assert_eq!(c.on_key(&ctrl("y"), &mut m, false, 0), Handled::Pass);
        assert_eq!(c.on_key(&kc("a"), &mut m, false, 0), Handled::Pass);
    }

    #[test]
    fn prefix_arms_then_resolves_bare_key() {
        let mut c = controller();
        let mut m = model2();
        assert_eq!(c.on_key(&ctrl("b"), &mut m, false, 0), Handled::Consumed(Vec::new()));
        assert!(c.armed(1));
        // `c` creates a workspace (prefixed binding), modifiers ignored.
        let mut second = kc("c");
        second.shift = true;
        let h = c.on_key(&second, &mut m, false, 100);
        assert_eq!(h, Handled::Consumed(vec![Effect::PersistLayout]));
        assert_eq!(m.tabs.len(), 3);
        assert!(!c.armed(101), "prefix disarms after resolving");
    }

    #[test]
    fn prefix_double_tap_sends_literal() {
        let mut c = controller();
        let mut m = model2();
        c.on_key(&ctrl("b"), &mut m, false, 0);
        let h = c.on_key(&ctrl("b"), &mut m, false, 100);
        assert_eq!(h, Handled::Consumed(vec![Effect::Literal(vec![0x02])]));
        assert!(!c.armed(101));
    }

    #[test]
    fn prefix_unbound_key_disarms_and_passes() {
        let mut c = controller();
        let mut m = model2();
        c.on_key(&ctrl("b"), &mut m, false, 0);
        assert_eq!(c.on_key(&kc("z"), &mut m, false, 100), Handled::Pass);
        assert!(!c.armed(101));
    }

    #[test]
    fn prefix_times_out() {
        let mut c = controller();
        let mut m = model2();
        c.on_key(&ctrl("b"), &mut m, false, 0);
        assert!(c.armed(PREFIX_TIMEOUT_MS - 1));
        // At/after the deadline the arm is gone; `t` falls to the direct tier
        // (unbound there without ctrl) and passes through.
        let h = c.on_key(&kc("t"), &mut m, false, PREFIX_TIMEOUT_MS);
        assert_eq!(h, Handled::Pass);
        // Re-arming still works.
        c.on_key(&ctrl("b"), &mut m, false, PREFIX_TIMEOUT_MS + 10);
        assert!(c.armed(PREFIX_TIMEOUT_MS + 11));
    }

    #[test]
    fn guard_disarms_and_passes() {
        let mut c = controller();
        let mut m = model2();
        c.on_key(&ctrl("b"), &mut m, false, 0);
        assert_eq!(c.on_key(&ctrl("t"), &mut m, true, 100), Handled::Pass);
        assert!(!c.armed(101), "editable target disarms a stale prefix");
    }

    #[test]
    fn lone_modifier_keeps_the_arm() {
        let mut c = controller();
        let mut m = model2();
        c.on_key(&ctrl("b"), &mut m, false, 0);
        assert_eq!(c.on_key(&kc("shift"), &mut m, false, 50), Handled::Pass);
        assert!(c.armed(100), "pressing shift alone must not break the chord");
    }

    #[test]
    fn double_tap_of_a_nonletter_prefix_consumes_without_literal() {
        let mut c = controller();
        c.keymap.prefix = Chord::parse("ctrl+space").unwrap();
        let mut m = model2();
        let mut space = ctrl("space");
        space.key_char = None;
        c.on_key(&space, &mut m, false, 0);
        assert_eq!(c.on_key(&space, &mut m, false, 100), Handled::Consumed(Vec::new()));
    }

    // -- region ---------------------------------------------------------------

    #[test]
    fn sidebar_region_owns_plain_keys() {
        let mut c = controller();
        let mut m = model2();
        c.on_key(&ctrl("j"), &mut m, false, 0);
        assert_eq!(c.region, Region::Sidebar);
        // Arrows step the workspace list.
        assert_eq!(
            c.on_key(&kc("down"), &mut m, false, 10),
            Handled::Consumed(vec![Effect::PersistLayout])
        );
        assert_eq!(m.active, 1);
        c.on_key(&kc("up"), &mut m, false, 20);
        assert_eq!(m.active, 0);
        // Plain typing is swallowed, chords still dispatch.
        assert_eq!(c.on_key(&kc("a"), &mut m, false, 30), Handled::Consumed(Vec::new()));
        assert_eq!(
            c.on_key(&ctrl("tab"), &mut m, false, 40),
            Handled::Consumed(vec![Effect::PersistLayout])
        );
        assert_eq!(m.active, 1, "ctrl+tab cycles WORKSPACES while sidebar-focused");
        // Enter returns to the tiles.
        c.on_key(&kc("enter"), &mut m, false, 50);
        assert_eq!(c.region, Region::Tiles);
    }

    // -- rebinding + persistence ----------------------------------------------

    #[test]
    fn set_direct_strips_the_old_owner() {
        let mut k = Keymap::default();
        k.set_direct(CommandId::CloseTerminal, Some(Chord::parse("ctrl+t").unwrap()));
        assert_eq!(k.direct_for(&Chord::parse("ctrl+t").unwrap()), Some(CommandId::CloseTerminal));
        assert_eq!(k.direct_of(CommandId::SpawnTerminal), None, "old owner stripped");
        assert_eq!(k.direct_for(&Chord::parse("ctrl+w").unwrap()), None, "old chord released");
        k.set_direct(CommandId::CloseTerminal, None);
        assert_eq!(k.direct_of(CommandId::CloseTerminal), None);
    }

    #[test]
    fn set_prefixed_strips_the_old_owner() {
        let mut k = Keymap::default();
        k.set_prefixed(CommandId::CloseTerminal, Some("T".to_string()));
        assert_eq!(k.prefixed_for("t"), Some(CommandId::CloseTerminal));
        assert_eq!(k.prefixed_of(CommandId::SpawnTerminal), None);
    }

    #[test]
    fn keymap_round_trips_through_disk() {
        let dir = std::env::temp_dir().join(format!("thn-keymap-rt-{}", std::process::id()));
        let path = dir.join("native-keymap.json");
        let mut k = Keymap::default();
        k.set_direct(CommandId::CommandPalette, Some(Chord::parse("ctrl+shift+p").unwrap()));
        k.prefix = Chord::parse("ctrl+a").unwrap();
        save(&path, &k).unwrap();
        let loaded = load(&path).unwrap().unwrap();
        assert_eq!(loaded, k);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_sanitizes_and_replaces_wholesale() {
        let file = KeymapFile {
            version: 1,
            prefix_key: "ctrl+".into(),
            direct: [
                ("spawnTerminal".to_string(), "ctrl+shift+t".to_string()),
                ("removedInV2".to_string(), "ctrl+q".to_string()),
                ("closeTerminal".to_string(), "".to_string()),
            ]
            .into(),
            prefixed: [
                ("commandPalette".to_string(), " P ".to_string()),
                ("nope".to_string(), "z".to_string()),
            ]
            .into(),
        };
        let k = file.into_keymap();
        assert_eq!(k.prefix.to_persist(), "ctrl+b", "bad prefix falls back");
        assert_eq!(
            k.direct_of(CommandId::SpawnTerminal).unwrap().to_persist(),
            "ctrl+shift+t"
        );
        assert_eq!(k.direct_of(CommandId::CloseTerminal), None, "empty chord drops");
        assert_eq!(k.direct_of(CommandId::CommandPalette), None, "file replaces defaults");
        assert_eq!(k.prefixed_of(CommandId::CommandPalette), Some("p"), "trimmed + lowered");
        assert_eq!(k.prefixed_for("z"), None, "unknown id drops");
    }

    #[test]
    fn missing_and_corrupt_files() {
        let dir = std::env::temp_dir().join(format!("thn-keymap-bad-{}", std::process::id()));
        let path = dir.join("native-keymap.json");
        assert!(load(&path).unwrap().is_none(), "missing file = first run");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, "{not json").unwrap();
        assert!(load(&path).is_err(), "corrupt file is a (downgradable) error");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn binding_hints_render_both_tiers() {
        let k = Keymap::default();
        assert_eq!(binding_hint(&k, CommandId::CommandPalette), "Ctrl+K · Ctrl+B P");
        assert_eq!(binding_hint(&k, CommandId::NewPlainWorkspace), "Ctrl+B C");
        assert_eq!(binding_hint(&k, CommandId::FocusTab1), "Ctrl+1");
        let mut k = k;
        k.set_direct(CommandId::FocusTab1, None);
        assert_eq!(binding_hint(&k, CommandId::FocusTab1), "-");
    }

    // -- palette through the controller -----------------------------------------

    #[test]
    fn ctrl_k_opens_palette_and_it_owns_keys() {
        let mut c = controller();
        let mut m = model2();
        assert_eq!(c.on_key(&ctrl("k"), &mut m, false, 0), Handled::Consumed(Vec::new()));
        assert!(c.palette.open);
        // While open, a direct chord is NOT dispatched (palette swallows).
        let before = m.active;
        c.on_key(&ctrl("2"), &mut m, false, 10);
        assert_eq!(m.active, before);
        assert!(c.palette.open);
    }

    #[test]
    fn prefix_p_opens_palette() {
        let mut c = controller();
        let mut m = model2();
        c.on_key(&ctrl("b"), &mut m, false, 0);
        c.on_key(&kc("p"), &mut m, false, 100);
        assert!(c.palette.open);
    }

    #[test]
    fn palette_enter_runs_the_selected_command() {
        let mut c = controller();
        let mut m = model2();
        c.on_key(&ctrl("k"), &mut m, false, 0);
        // Type "new workspace" enough to isolate the command, then run it.
        for ch in "new workspace".chars() {
            let mut k = kc(&ch.to_string());
            if ch == ' ' {
                k.key = "space".into();
                k.key_char = Some(" ".into());
            }
            c.on_key(&k, &mut m, false, 10);
        }
        let tabs_before = m.tabs.len();
        let h = c.on_key(&kc("enter"), &mut m, false, 20);
        assert_eq!(h, Handled::Consumed(vec![Effect::PersistLayout]));
        assert_eq!(m.tabs.len(), tabs_before + 1);
        assert!(!c.palette.open, "execute closes the palette");
    }
}
