//! The action registry and executor (T-A): the webview's 21-command registry
//! (`lib/commands.ts` + `keymapExecutor.ts`) ported as plain data + a pure
//! executor over the [`ChromeModel`].
//!
//! Every command carries the SAME id string, label, description, and category
//! as the webview registry, so persisted keymaps and muscle memory transfer
//! verbatim. Executing a command mutates the model directly (workspace jump,
//! tile cycle, new tab, close tile, zoom) and returns [`Effect`]s for the
//! gui layer to run - the side effects a gpui-free module cannot perform
//! (persisting, focus-notify bytes, raising a satellite OS window, re-speccing
//! tile fonts, PTY writes, host/server flows).
//!
//! Commands whose backing flow is NOT native yet (local spawn, worktree
//! create/list - the T-B daily-drive gaps) still exist in the registry and
//! keymap so bindings and the palette are complete; they dispatch through
//! [`HostCommand`] to the [`dispatch_host`] seam, which T-B replaces with the
//! real executor. Keymap-side semantics (T-A) and flow execution (T-B) meet at
//! that one function - no shared edits.

use crate::chrome::model::ChromeModel;
use crate::font::FontSpec;

// ---------------------------------------------------------------------------
// The registry
// ---------------------------------------------------------------------------

/// The webview's 21 commands (ids matching `lib/commands.ts` exactly - the
/// persisted keymap file uses these strings) plus the native-only additions:
/// `killSession` (N4: the webview hard-codes Ctrl+Shift+W in
/// `useLifecycleKeybinds.tsx`, native routes it through the registry so the
/// palette and rebinding get it for free), `toggleTileFullscreen` (N3), and
/// `togglePanels` (N5: the webview exposes panels as per-tile header tabs,
/// which need no command; the native cockpit mounts them as a toggleable
/// side surface).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CommandId {
    SpawnTerminal,
    CloseTerminal,
    KillSession,
    NewPlainWorkspace,
    NewWorktreeWorkspace,
    OpenWorktreesList,
    CycleTileNext,
    CycleTilePrev,
    ToggleFocusRegion,
    FocusTab1,
    FocusTab2,
    FocusTab3,
    FocusTab4,
    FocusTab5,
    FocusTab6,
    FocusTab7,
    FocusTab8,
    FocusTab9,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    CommandPalette,
    /// N3: fullscreen the focused tile / restore the grid. Native-first (the
    /// webview toggles this from the tile header button only).
    ToggleTileFullscreen,
    TogglePanels,
}

/// Registry order = the webview's COMMANDS list order (palette tie-break);
/// native-only commands (killSession slots after its close sibling,
/// toggleTileFullscreen and togglePanels append) ride along.
pub const ALL_COMMANDS: [CommandId; 24] = [
    CommandId::SpawnTerminal,
    CommandId::CloseTerminal,
    CommandId::KillSession,
    CommandId::NewPlainWorkspace,
    CommandId::NewWorktreeWorkspace,
    CommandId::OpenWorktreesList,
    CommandId::CycleTileNext,
    CommandId::CycleTilePrev,
    CommandId::ToggleFocusRegion,
    CommandId::FocusTab1,
    CommandId::FocusTab2,
    CommandId::FocusTab3,
    CommandId::FocusTab4,
    CommandId::FocusTab5,
    CommandId::FocusTab6,
    CommandId::FocusTab7,
    CommandId::FocusTab8,
    CommandId::FocusTab9,
    CommandId::ZoomIn,
    CommandId::ZoomOut,
    CommandId::ZoomReset,
    CommandId::CommandPalette,
    CommandId::ToggleTileFullscreen,
    CommandId::TogglePanels,
];

impl CommandId {
    /// The webview id string (persisted keymap key).
    pub fn as_str(self) -> &'static str {
        match self {
            CommandId::SpawnTerminal => "spawnTerminal",
            CommandId::CloseTerminal => "closeTerminal",
            CommandId::KillSession => "killSession",
            CommandId::NewPlainWorkspace => "newPlainWorkspace",
            CommandId::NewWorktreeWorkspace => "newWorktreeWorkspace",
            CommandId::OpenWorktreesList => "openWorktreesList",
            CommandId::CycleTileNext => "cycleTileNext",
            CommandId::CycleTilePrev => "cycleTilePrev",
            CommandId::ToggleFocusRegion => "toggleFocusRegion",
            CommandId::FocusTab1 => "focusTab1",
            CommandId::FocusTab2 => "focusTab2",
            CommandId::FocusTab3 => "focusTab3",
            CommandId::FocusTab4 => "focusTab4",
            CommandId::FocusTab5 => "focusTab5",
            CommandId::FocusTab6 => "focusTab6",
            CommandId::FocusTab7 => "focusTab7",
            CommandId::FocusTab8 => "focusTab8",
            CommandId::FocusTab9 => "focusTab9",
            CommandId::ZoomIn => "zoomIn",
            CommandId::ZoomOut => "zoomOut",
            CommandId::ZoomReset => "zoomReset",
            CommandId::CommandPalette => "commandPalette",
            CommandId::ToggleTileFullscreen => "toggleTileFullscreen",
            CommandId::TogglePanels => "togglePanels",
        }
    }

    pub fn parse(s: &str) -> Option<CommandId> {
        ALL_COMMANDS.iter().copied().find(|c| c.as_str() == s)
    }

    /// Palette row text, verbatim from the webview registry.
    pub fn label(self) -> &'static str {
        match self {
            CommandId::SpawnTerminal => "New terminal",
            CommandId::CloseTerminal => "Close terminal",
            CommandId::KillSession => "Kill session",
            CommandId::NewPlainWorkspace => "New workspace",
            CommandId::NewWorktreeWorkspace => "New worktree workspace",
            CommandId::OpenWorktreesList => "List worktrees",
            CommandId::CycleTileNext => "Next terminal",
            CommandId::CycleTilePrev => "Previous terminal",
            CommandId::ToggleFocusRegion => "Toggle focus: terminal / sidebar",
            CommandId::FocusTab1 => "Jump to workspace 1",
            CommandId::FocusTab2 => "Jump to workspace 2",
            CommandId::FocusTab3 => "Jump to workspace 3",
            CommandId::FocusTab4 => "Jump to workspace 4",
            CommandId::FocusTab5 => "Jump to workspace 5",
            CommandId::FocusTab6 => "Jump to workspace 6",
            CommandId::FocusTab7 => "Jump to workspace 7",
            CommandId::FocusTab8 => "Jump to workspace 8",
            CommandId::FocusTab9 => "Jump to workspace 9",
            CommandId::ZoomIn => "Zoom in",
            CommandId::ZoomOut => "Zoom out",
            CommandId::ZoomReset => "Reset zoom",
            CommandId::CommandPalette => "Command palette",
            CommandId::ToggleTileFullscreen => "Toggle tile fullscreen",
            CommandId::TogglePanels => "Toggle panels",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            CommandId::SpawnTerminal => "Spawn a terminal after the focused tile",
            CommandId::CloseTerminal => "Close the focused terminal's tile (session survives)",
            CommandId::KillSession => {
                "Kill the focused terminal's tmux session for real (asks to confirm)"
            }
            CommandId::NewPlainWorkspace => "Open a new empty tab (no repo, no worktree)",
            CommandId::NewWorktreeWorkspace => {
                "Branch the focused repo into a sibling worktree and open it in a new tab"
            }
            CommandId::OpenWorktreesList => {
                "Show the focused repo's worktrees to re-open or remove"
            }
            CommandId::CycleTileNext => "Focus the next tile (across all workspaces)",
            CommandId::CycleTilePrev => "Focus the previous tile (across all workspaces)",
            CommandId::ToggleFocusRegion => {
                "Move keyboard focus between the terminal area and the sidebar"
            }
            CommandId::FocusTab1 => "Activate the workspace tab at position 1",
            CommandId::FocusTab2 => "Activate the workspace tab at position 2",
            CommandId::FocusTab3 => "Activate the workspace tab at position 3",
            CommandId::FocusTab4 => "Activate the workspace tab at position 4",
            CommandId::FocusTab5 => "Activate the workspace tab at position 5",
            CommandId::FocusTab6 => "Activate the workspace tab at position 6",
            CommandId::FocusTab7 => "Activate the workspace tab at position 7",
            CommandId::FocusTab8 => "Activate the workspace tab at position 8",
            CommandId::FocusTab9 => "Activate the workspace tab at position 9",
            CommandId::ZoomIn => "Increase terminal font size",
            CommandId::ZoomOut => "Decrease terminal font size",
            CommandId::ZoomReset => "Reset terminal font size",
            CommandId::CommandPalette => "Open the fuzzy command palette",
            CommandId::ToggleTileFullscreen => {
                "Expand the focused tile to fill the grid, or restore the grid"
            }
            CommandId::TogglePanels => {
                "Show or hide the Files / Preview / Dev panels beside the grid"
            }
        }
    }

    pub fn category(self) -> &'static str {
        match self {
            CommandId::SpawnTerminal
            | CommandId::CloseTerminal
            | CommandId::KillSession
            | CommandId::ToggleTileFullscreen => "Terminals",
            CommandId::NewPlainWorkspace
            | CommandId::NewWorktreeWorkspace
            | CommandId::OpenWorktreesList => "Workspaces",
            CommandId::CycleTileNext
            | CommandId::CycleTilePrev
            | CommandId::ToggleFocusRegion
            | CommandId::FocusTab1
            | CommandId::FocusTab2
            | CommandId::FocusTab3
            | CommandId::FocusTab4
            | CommandId::FocusTab5
            | CommandId::FocusTab6
            | CommandId::FocusTab7
            | CommandId::FocusTab8
            | CommandId::FocusTab9 => "Navigation",
            CommandId::ZoomIn | CommandId::ZoomOut | CommandId::ZoomReset => "Zoom",
            CommandId::CommandPalette | CommandId::TogglePanels => "App",
        }
    }

    /// The workspace index a focusTab command targets, if it is one.
    fn tab_index(self) -> Option<usize> {
        Some(match self {
            CommandId::FocusTab1 => 0,
            CommandId::FocusTab2 => 1,
            CommandId::FocusTab3 => 2,
            CommandId::FocusTab4 => 3,
            CommandId::FocusTab5 => 4,
            CommandId::FocusTab6 => 5,
            CommandId::FocusTab7 => 6,
            CommandId::FocusTab8 => 7,
            CommandId::FocusTab9 => 8,
            _ => return None,
        })
    }
}

// ---------------------------------------------------------------------------
// Effects
// ---------------------------------------------------------------------------

/// Which region owns the keyboard (webview `focusedRegion`): tile-cycle
/// commands act on workspace tabs while the sidebar holds focus, and plain
/// keys stop reaching the terminal (they navigate the workspace list instead).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Region {
    #[default]
    Tiles,
    Sidebar,
}

/// A flow the native client cannot run locally yet: the T-B seam. The cwd of
/// the focused tile rides along (the webview captures it the same way for the
/// worktree prompt/list).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostCommand {
    SpawnTerminal,
    NewWorktreeWorkspace,
    OpenWorktreesList,
}

/// What the gui layer must do after a command mutated the model. Plain data so
/// the executor tests headless.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    /// The layout changed: `save_layout()` + `sync_active_sessions()`.
    PersistLayout,
    /// Focus moved between tiles: send focus-out/in (mode 1004) to the
    /// terminals. The model is already updated.
    FocusChanged { old: Option<String>, new: String },
    /// A tile left the layout (native close = detach): drop its pool entries,
    /// persist, re-sync active sessions - the tile-close `x` path.
    TileClosed(String),
    /// The target workspace is torn off: activate its OS window (the sidebar
    /// row-click parity; the main grid cannot show a satellite).
    RaiseSatellite(u64),
    /// Workspace `tab`'s font override changed: re-spec its tiles' fonts (the
    /// model already carries the new [`FontSpec`]).
    FontChanged { tab: usize },
    /// Send literal bytes to the focused tile's PTY (prefix double-tap).
    Literal(Vec<u8>),
    /// Dispatch a host/server flow through [`dispatch_host`] (T-B seam), with
    /// the focused tile's cwd attached by the view.
    Host(HostCommand),
    /// The user asked to KILL a session (N4): the view opens the confirm
    /// dialog (busy-aware); nothing is mutated until the user confirms there.
    ConfirmKill(String),
    /// Show/hide the panels side surface (N5). The open flag is view state
    /// (like the palette), not model state - the view flips it, roots the
    /// panels feed at the focused tile's cwd, and routes keyboard focus.
    TogglePanels,
}

// ---------------------------------------------------------------------------
// The executor
// ---------------------------------------------------------------------------

/// Zoom clamp, webview `clampFont` parity (`workspace.ts`): 6..=28, rounded.
const MIN_FONT_SIZE: f32 = 6.0;
const MAX_FONT_SIZE: f32 = 28.0;

fn clamp_font(n: f32) -> f32 {
    if !n.is_finite() {
        return FontSpec::from_env().size;
    }
    n.round().clamp(MIN_FONT_SIZE, MAX_FONT_SIZE)
}

/// Run one command against the model. Local commands mutate the model here;
/// the returned effects are the gui layer's follow-ups. `region` is the
/// keyboard focus region ([`CommandId::ToggleFocusRegion`] flips it; the
/// cycle commands read it - webview `keymapExecutor.ts` `doCycle`).
///
/// [`CommandId::CommandPalette`] is NOT handled here - opening the palette is
/// keymap-controller state, see `keymap::KeyController::run`.
pub fn execute(cmd: CommandId, model: &mut ChromeModel, region: &mut Region) -> Vec<Effect> {
    if let Some(i) = cmd.tab_index() {
        return focus_tab(model, i);
    }
    match cmd {
        CommandId::SpawnTerminal => vec![Effect::Host(HostCommand::SpawnTerminal)],
        CommandId::NewWorktreeWorkspace => {
            vec![Effect::Host(HostCommand::NewWorktreeWorkspace)]
        }
        CommandId::OpenWorktreesList => vec![Effect::Host(HostCommand::OpenWorktreesList)],
        CommandId::KillSession => {
            // No model mutation here: killing is destructive, so the view
            // confirms first and runs the whole flow (server kill + tile drop)
            // on confirm.
            match model.focused.clone() {
                Some(id) => vec![Effect::ConfirmKill(id)],
                None => Vec::new(),
            }
        }
        CommandId::CloseTerminal => {
            let Some(id) = model.focused.clone() else { return Vec::new() };
            if model.close_tile(&id) {
                vec![Effect::TileClosed(id)]
            } else {
                Vec::new()
            }
        }
        CommandId::NewPlainWorkspace => {
            model.add_tab();
            vec![Effect::PersistLayout]
        }
        CommandId::CycleTileNext => cycle(model, *region, 1),
        CommandId::CycleTilePrev => cycle(model, *region, -1),
        CommandId::ToggleFocusRegion => {
            *region = match *region {
                Region::Tiles => Region::Sidebar,
                Region::Sidebar => Region::Tiles,
            };
            Vec::new()
        }
        CommandId::ZoomIn => zoom(model, 1.0),
        CommandId::ZoomOut => zoom(model, -1.0),
        CommandId::ZoomReset => zoom_reset(model),
        CommandId::ToggleTileFullscreen => {
            // Toggle on the active tab's focused tile (N3). Transient state
            // like the webview's `fullscreenId`: nothing to persist; the view
            // repaints and the PTY refits through the normal geometry path.
            let Some(id) = model.focused.clone() else { return Vec::new() };
            model.toggle_fullscreen(model.active, &id);
            Vec::new()
        }
        CommandId::CommandPalette => Vec::new(),
        CommandId::TogglePanels => vec![Effect::TogglePanels],
        _ => unreachable!("focusTab handled above"),
    }
}

/// Jump to workspace `i` (webview `setActiveTabByIndex`). A torn-off target
/// cannot show in the main grid; raise its OS window instead (the sidebar
/// row-click behavior).
fn focus_tab(model: &mut ChromeModel, i: usize) -> Vec<Effect> {
    let Some(tab) = model.tabs.get(i) else { return Vec::new() };
    if tab.satellite {
        return vec![Effect::RaiseSatellite(tab.wsid)];
    }
    if model.active == i {
        return Vec::new();
    }
    model.set_active(i);
    vec![Effect::PersistLayout]
}

fn cycle(model: &mut ChromeModel, region: Region, dir: i64) -> Vec<Effect> {
    match region {
        Region::Tiles => cycle_tile_global(model, dir),
        Region::Sidebar => cycle_tab(model, dir),
    }
}

/// Focus the next/previous tile ACROSS workspaces (webview `cycleTileGlobal`):
/// the flat tile list over main (non-satellite) tabs in tab order, wrapping;
/// entering another tab activates it. Satellite tiles are skipped - they paint
/// in their own windows and the main grid cannot show them.
fn cycle_tile_global(model: &mut ChromeModel, dir: i64) -> Vec<Effect> {
    let flat: Vec<(usize, String)> = model
        .tabs
        .iter()
        .enumerate()
        .filter(|(_, t)| !t.satellite)
        .flat_map(|(i, t)| t.tiles.iter().map(move |id| (i, id.clone())))
        .collect();
    if flat.is_empty() {
        return Vec::new();
    }
    let old = model.focused.clone();
    let pos = old
        .as_deref()
        .and_then(|f| flat.iter().position(|(i, id)| *i == model.active && id == f));
    let next = match pos {
        Some(p) => (p as i64 + dir).rem_euclid(flat.len() as i64) as usize,
        // No current position (empty active tab / satellite focus): start at
        // an end so the first cycle lands on the first/last tile.
        None => {
            if dir > 0 {
                0
            } else {
                flat.len() - 1
            }
        }
    };
    let (tab, id) = flat[next].clone();
    if old.as_deref() == Some(id.as_str()) && tab == model.active {
        return Vec::new();
    }
    let mut effects = Vec::new();
    if tab != model.active {
        model.set_active(tab);
        effects.push(Effect::PersistLayout);
    }
    model.set_focused(&id);
    effects.push(Effect::FocusChanged { old, new: id });
    effects
}

/// Cycle the ACTIVE workspace among main (non-satellite) tabs (webview
/// `cycleTab`, what the cycle commands do while the sidebar holds focus).
fn cycle_tab(model: &mut ChromeModel, dir: i64) -> Vec<Effect> {
    let mains: Vec<usize> = model
        .tabs
        .iter()
        .enumerate()
        .filter(|(_, t)| !t.satellite)
        .map(|(i, _)| i)
        .collect();
    if mains.len() < 2 {
        return Vec::new();
    }
    let pos = mains.iter().position(|&i| i == model.active).unwrap_or(0);
    let next = mains[(pos as i64 + dir).rem_euclid(mains.len() as i64) as usize];
    if next == model.active {
        return Vec::new();
    }
    model.set_active(next);
    vec![Effect::PersistLayout]
}

/// The active workspace's zoom base: its font override, else the `THN_FONT` /
/// built-in default (what its tiles actually render with today).
fn zoom_base(model: &ChromeModel) -> FontSpec {
    model.tabs[model.active].font.clone().unwrap_or_else(FontSpec::from_env)
}

/// Zoom the active workspace's font by `delta` px, clamped (webview zoom trio
/// semantics on the native per-workspace font override).
fn zoom(model: &mut ChromeModel, delta: f32) -> Vec<Effect> {
    let mut spec = zoom_base(model);
    let size = clamp_font(spec.size + delta);
    if (size - spec.size).abs() < f32::EPSILON {
        return Vec::new();
    }
    spec.size = size;
    let tab = model.active;
    model.tabs[tab].font = Some(spec);
    vec![Effect::FontChanged { tab }, Effect::PersistLayout]
}

/// Reset the active workspace's font size to the default base (`THN_FONT` /
/// built-in), keeping any family/ligature override.
fn zoom_reset(model: &mut ChromeModel) -> Vec<Effect> {
    let tab = model.active;
    let Some(mut spec) = model.tabs[tab].font.clone() else { return Vec::new() };
    let base = FontSpec::from_env().size;
    if (spec.size - base).abs() < f32::EPSILON {
        return Vec::new();
    }
    spec.size = base;
    model.tabs[tab].font = Some(spec);
    vec![Effect::FontChanged { tab }, Effect::PersistLayout]
}

/// The T-B seam: flows the native client cannot run locally yet (local spawn,
/// worktree create/list) land here with the focused tile's cwd. T-B's
/// daily-drive executor replaces this body (or forwards into its channel);
/// until then the request is logged exactly like `app.rs` logs
/// `HostRequest::ResumeSession`.
pub fn dispatch_host(cmd: &HostCommand, cwd: Option<&str>) {
    log::info!("keymap host request (T-B executor seam, not wired yet): {cmd:?} cwd={cwd:?}");
}

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chrome::model::Workspace;

    fn model_with(tabs: Vec<(&str, Vec<&str>, bool)>) -> ChromeModel {
        let tabs = tabs
            .into_iter()
            .map(|(name, tiles, satellite)| {
                let mut w = Workspace::new(name);
                w.tiles = tiles.into_iter().map(str::to_string).collect();
                w.satellite = satellite;
                w
            })
            .collect();
        ChromeModel::from_layout(tabs, 0)
    }

    #[test]
    fn registry_ids_round_trip() {
        for cmd in ALL_COMMANDS {
            assert_eq!(CommandId::parse(cmd.as_str()), Some(cmd));
            assert!(!cmd.label().is_empty());
            assert!(!cmd.description().is_empty());
            assert!(!cmd.category().is_empty());
        }
        assert_eq!(CommandId::parse("nope"), None);
        // The five webview categories, all populated.
        for cat in ["Terminals", "Workspaces", "Navigation", "Zoom", "App"] {
            assert!(ALL_COMMANDS.iter().any(|c| c.category() == cat), "{cat} empty");
        }
    }

    #[test]
    fn toggle_panels_emits_view_effect_only() {
        let mut m = model_with(vec![("A", vec!["t1"], false)]);
        let mut r = Region::Tiles;
        let focused_before = m.focused.clone();
        let fx = execute(CommandId::TogglePanels, &mut m, &mut r);
        assert_eq!(fx, vec![Effect::TogglePanels]);
        // Panels visibility is view state, not model state: nothing mutated.
        assert_eq!(m.tabs.len(), 1);
        assert_eq!(m.active, 0);
        assert_eq!(m.focused, focused_before);
        assert_eq!(r, Region::Tiles);
    }

    #[test]
    fn focus_tab_activates_and_persists() {
        let mut m = model_with(vec![("A", vec!["t1"], false), ("B", vec!["t2"], false)]);
        let mut r = Region::Tiles;
        let fx = execute(CommandId::FocusTab2, &mut m, &mut r);
        assert_eq!(m.active, 1);
        assert_eq!(fx, vec![Effect::PersistLayout]);
        // Already active: no effects.
        assert!(execute(CommandId::FocusTab2, &mut m, &mut r).is_empty());
        // Out of range: no effects, no change.
        assert!(execute(CommandId::FocusTab9, &mut m, &mut r).is_empty());
        assert_eq!(m.active, 1);
    }

    #[test]
    fn focus_tab_satellite_raises_window() {
        let mut m = model_with(vec![("A", vec!["t1"], false), ("B", vec!["t2"], true)]);
        let wsid = m.tabs[1].wsid;
        let mut r = Region::Tiles;
        let fx = execute(CommandId::FocusTab2, &mut m, &mut r);
        assert_eq!(fx, vec![Effect::RaiseSatellite(wsid)]);
        assert_eq!(m.active, 0, "satellite tab must not activate in the main grid");
    }

    #[test]
    fn cycle_wraps_within_tab() {
        let mut m = model_with(vec![("A", vec!["t1", "t2"], false)]);
        m.set_focused("t2");
        let mut r = Region::Tiles;
        let fx = execute(CommandId::CycleTileNext, &mut m, &mut r);
        assert_eq!(m.focused.as_deref(), Some("t1"));
        assert_eq!(
            fx,
            vec![Effect::FocusChanged { old: Some("t2".into()), new: "t1".into() }]
        );
    }

    #[test]
    fn cycle_crosses_tabs_and_skips_satellites() {
        let mut m = model_with(vec![
            ("A", vec!["t1"], false),
            ("S", vec!["s1"], true),
            ("B", vec!["t2"], false),
        ]);
        m.set_focused("t1");
        let mut r = Region::Tiles;
        let fx = execute(CommandId::CycleTileNext, &mut m, &mut r);
        assert_eq!(m.active, 2, "cycle must skip the satellite tab");
        assert_eq!(m.focused.as_deref(), Some("t2"));
        assert_eq!(
            fx,
            vec![
                Effect::PersistLayout,
                Effect::FocusChanged { old: Some("t1".into()), new: "t2".into() }
            ]
        );
    }

    #[test]
    fn cycle_prev_wraps_backward() {
        let mut m = model_with(vec![("A", vec!["t1"], false), ("B", vec!["t2"], false)]);
        m.set_focused("t1");
        let mut r = Region::Tiles;
        execute(CommandId::CycleTilePrev, &mut m, &mut r);
        assert_eq!(m.active, 1);
        assert_eq!(m.focused.as_deref(), Some("t2"));
    }

    #[test]
    fn cycle_no_tiles_is_noop() {
        let mut m = model_with(vec![("A", vec![], false)]);
        let mut r = Region::Tiles;
        assert!(execute(CommandId::CycleTileNext, &mut m, &mut r).is_empty());
        // Single tile: cycling lands on itself, no effects.
        let mut m = model_with(vec![("A", vec!["t1"], false)]);
        m.set_focused("t1");
        assert!(execute(CommandId::CycleTileNext, &mut m, &mut r).is_empty());
    }

    #[test]
    fn sidebar_region_cycles_tabs() {
        let mut m = model_with(vec![
            ("A", vec!["t1"], false),
            ("S", vec!["s1"], true),
            ("B", vec!["t2"], false),
        ]);
        let mut r = Region::Tiles;
        execute(CommandId::ToggleFocusRegion, &mut m, &mut r);
        assert_eq!(r, Region::Sidebar);
        let fx = execute(CommandId::CycleTileNext, &mut m, &mut r);
        assert_eq!(m.active, 2, "sidebar cycle skips the satellite tab");
        assert_eq!(fx, vec![Effect::PersistLayout]);
        execute(CommandId::CycleTileNext, &mut m, &mut r);
        assert_eq!(m.active, 0, "wraps");
        execute(CommandId::ToggleFocusRegion, &mut m, &mut r);
        assert_eq!(r, Region::Tiles);
    }

    #[test]
    fn kill_session_confirms_without_mutating() {
        let mut m = model_with(vec![("A", vec!["t1", "t2"], false)]);
        m.set_focused("t1");
        let mut r = Region::Tiles;
        // Kill is destructive: the executor only asks the view to confirm;
        // the tile stays until the user does.
        let fx = execute(CommandId::KillSession, &mut m, &mut r);
        assert_eq!(fx, vec![Effect::ConfirmKill("t1".into())]);
        assert!(m.contains_tile("t1"));
        // Nothing focused: no effects.
        let mut m = model_with(vec![("A", vec![], false)]);
        assert!(execute(CommandId::KillSession, &mut m, &mut r).is_empty());
    }

    #[test]
    fn close_terminal_detaches_focused() {
        let mut m = model_with(vec![("A", vec!["t1", "t2"], false)]);
        m.set_focused("t1");
        let mut r = Region::Tiles;
        let fx = execute(CommandId::CloseTerminal, &mut m, &mut r);
        assert_eq!(fx, vec![Effect::TileClosed("t1".into())]);
        assert!(!m.contains_tile("t1"));
        // Nothing focused: no effects.
        let mut m = model_with(vec![("A", vec![], false)]);
        assert!(execute(CommandId::CloseTerminal, &mut m, &mut r).is_empty());
    }

    #[test]
    fn new_plain_workspace_activates() {
        let mut m = model_with(vec![("Workspace 1", vec!["t1"], false)]);
        let mut r = Region::Tiles;
        let fx = execute(CommandId::NewPlainWorkspace, &mut m, &mut r);
        assert_eq!(m.tabs.len(), 2);
        assert_eq!(m.active, 1);
        assert_eq!(fx, vec![Effect::PersistLayout]);
    }

    #[test]
    fn host_commands_dispatch_through_seam() {
        let mut m = model_with(vec![("A", vec!["t1"], false)]);
        let mut r = Region::Tiles;
        assert_eq!(
            execute(CommandId::SpawnTerminal, &mut m, &mut r),
            vec![Effect::Host(HostCommand::SpawnTerminal)]
        );
        assert_eq!(
            execute(CommandId::NewWorktreeWorkspace, &mut m, &mut r),
            vec![Effect::Host(HostCommand::NewWorktreeWorkspace)]
        );
        assert_eq!(
            execute(CommandId::OpenWorktreesList, &mut m, &mut r),
            vec![Effect::Host(HostCommand::OpenWorktreesList)]
        );
    }

    #[test]
    fn toggle_tile_fullscreen_acts_on_the_focused_tile() {
        let mut m = model_with(vec![("A", vec!["t1", "t2"], false)]);
        m.set_focused("t2");
        let mut r = Region::Tiles;
        assert!(execute(CommandId::ToggleTileFullscreen, &mut m, &mut r).is_empty());
        assert_eq!(m.tabs[0].fullscreen.as_deref(), Some("t2"));
        execute(CommandId::ToggleTileFullscreen, &mut m, &mut r);
        assert_eq!(m.tabs[0].fullscreen, None);
        // Nothing focused: no-op.
        let mut m = model_with(vec![("A", vec![], false)]);
        execute(CommandId::ToggleTileFullscreen, &mut m, &mut r);
        assert_eq!(m.tabs[0].fullscreen, None);
    }

    #[test]
    fn zoom_steps_and_clamps() {
        let mut m = model_with(vec![("A", vec!["t1"], false)]);
        m.tabs[0].font = Some(FontSpec { family: "X".into(), size: 13.0, ligatures: true });
        let mut r = Region::Tiles;
        let fx = execute(CommandId::ZoomIn, &mut m, &mut r);
        assert_eq!(m.tabs[0].font.as_ref().unwrap().size, 14.0);
        assert_eq!(fx, vec![Effect::FontChanged { tab: 0 }, Effect::PersistLayout]);
        execute(CommandId::ZoomOut, &mut m, &mut r);
        execute(CommandId::ZoomOut, &mut m, &mut r);
        assert_eq!(m.tabs[0].font.as_ref().unwrap().size, 12.0);
        // Clamp at the floor: stepping below 6 pins and then no-ops.
        m.tabs[0].font.as_mut().unwrap().size = 6.0;
        assert!(execute(CommandId::ZoomOut, &mut m, &mut r).is_empty());
        m.tabs[0].font.as_mut().unwrap().size = 28.0;
        assert!(execute(CommandId::ZoomIn, &mut m, &mut r).is_empty());
    }

    #[test]
    fn zoom_without_override_starts_from_default_base() {
        let mut m = model_with(vec![("A", vec!["t1"], false)]);
        let base = FontSpec::from_env().size;
        let mut r = Region::Tiles;
        execute(CommandId::ZoomIn, &mut m, &mut r);
        assert_eq!(m.tabs[0].font.as_ref().unwrap().size, clamp_font(base + 1.0));
    }

    #[test]
    fn zoom_reset_returns_to_base_keeping_family() {
        let mut m = model_with(vec![("A", vec!["t1"], false)]);
        let base = FontSpec::from_env().size;
        m.tabs[0].font =
            Some(FontSpec { family: "Custom".into(), size: base + 4.0, ligatures: false });
        let mut r = Region::Tiles;
        let fx = execute(CommandId::ZoomReset, &mut m, &mut r);
        let f = m.tabs[0].font.as_ref().unwrap();
        assert_eq!(f.size, base);
        assert_eq!(f.family, "Custom");
        assert!(!f.ligatures);
        assert_eq!(fx, vec![Effect::FontChanged { tab: 0 }, Effect::PersistLayout]);
        // Already at base / no override: no effects.
        assert!(execute(CommandId::ZoomReset, &mut m, &mut r).is_empty());
        let mut m2 = model_with(vec![("A", vec!["t1"], false)]);
        assert!(execute(CommandId::ZoomReset, &mut m2, &mut r).is_empty());
    }

    #[test]
    fn zoom_acts_on_the_active_workspace() {
        let mut m = model_with(vec![("A", vec!["t1"], false), ("B", vec!["t2"], false)]);
        m.set_active(1);
        m.tabs[1].font = Some(FontSpec { family: "X".into(), size: 10.0, ligatures: true });
        let mut r = Region::Tiles;
        let fx = execute(CommandId::ZoomIn, &mut m, &mut r);
        assert_eq!(fx[0], Effect::FontChanged { tab: 1 });
        assert!(m.tabs[0].font.is_none(), "inactive workspace untouched");
        assert_eq!(m.tabs[1].font.as_ref().unwrap().size, 11.0);
    }
}
