//! GPUI rendering for the panels (T11). The ONLY panels file that touches
//! gpui - every decision about WHAT to show was made by the gpui-free
//! view-models; the fns here paint plain data (same rule as `overlays::view`).
//!
//! ## Mount contract (captain / T10-T12 - also documented in the execution doc §5)
//!
//! ```ignore
//! let feed = PanelsFeed::spawn(client.clone());            // once per process
//! let panels = cx.new(|cx| PanelHost::new(feed.clone(), cx.focus_handle()));
//! // as a tile or side surface - fills whatever box the host gives it:
//! div().flex_1().child(panels.clone())
//! // optional hooks:
//! feed.set_root("/path/to/project");                       // bind Files+Run to a tile's cwd
//! feed.note_session_urls(&tmux_id, urls);                  // push T6 visible_urls scans
//! ```
//! Or in one step: `PanelHost::mount(client, cx) -> (Entity<PanelHost>, PanelsFeed)`.
//!
//! `PanelHost` is `Focusable`; typing goes to the active tab's text field
//! (Files: the fuzzy-search box; Run: the command line once clicked). The
//! host must focus the entity (or let the user click into it) for keyboard
//! input - the standalone `panel-window` bin shows the wiring.

use std::sync::Arc;

use gpui::prelude::*;
use gpui::{
    div, px, App, ClickEvent, Context, Div, Entity, FocusHandle, Focusable, FontWeight, Hsla,
    KeyDownEvent, Render, Rgba, SharedString, Stateful, Window,
};

use super::feed::{PanelAction, PanelsFeed};
use super::files::{segment_text, HitRow, TreeRow};
use super::preview::{Probe, SessionUrls};
use super::runner::{Phase, RunnerState};
use super::Project;
use crate::wire::ControlClient;

// The app.rs dark palette + the webview's neutral text scale (same constants
// as overlays::view; duplicated because they are deliberately module-private).
const BG: (u8, u8, u8) = (13, 17, 23);
const TEXT: (u8, u8, u8) = (216, 222, 233);
const MUTED: (u8, u8, u8) = (140, 149, 164);
const FAINT: (u8, u8, u8) = (94, 102, 116);
const BORDER: (u8, u8, u8) = (38, 44, 54);
const ACCENT: (u8, u8, u8) = (217, 119, 87); // #D97757
const GREEN: (u8, u8, u8) = (63, 185, 80);
const AMBER: (u8, u8, u8) = (210, 153, 34);
const RED: (u8, u8, u8) = (248, 81, 73);
const BLUE: (u8, u8, u8) = (96, 165, 250);

fn rgb8(c: (u8, u8, u8)) -> Hsla {
    Rgba { r: c.0 as f32 / 255.0, g: c.1 as f32 / 255.0, b: c.2 as f32 / 255.0, a: 1.0 }.into()
}

fn rgba8(c: (u8, u8, u8), a: f32) -> Hsla {
    Rgba { r: c.0 as f32 / 255.0, g: c.1 as f32 / 255.0, b: c.2 as f32 / 255.0, a }.into()
}

fn t(s: impl Into<SharedString>) -> SharedString {
    s.into()
}

fn mono_family() -> &'static str {
    if cfg!(target_os = "windows") {
        "Cascadia Mono"
    } else {
        "monospace"
    }
}

/// Which panel tab is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelTab {
    Files,
    Preview,
    Run,
}

/// Which text field owns typing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputTarget {
    Search,
    Command,
}

/// The composed panels element: a tab strip (Files | Preview | Run) over the
/// active panel. Fills whatever box the host gives it.
pub struct PanelHost {
    feed: PanelsFeed,
    tab: PanelTab,
    focus: FocusHandle,
    input: InputTarget,
    /// Local edit buffer for the runner command line (committed on Enter).
    cmd_draft: Option<String>,
}

impl PanelHost {
    pub fn new(feed: PanelsFeed, focus: FocusHandle) -> Self {
        Self { feed, tab: PanelTab::Files, focus, input: InputTarget::Search, cmd_draft: None }
    }

    /// One-step mount: spawn the feed and create the view entity. Returns the
    /// feed too so the host can wire `set_root`/`note_session_urls`.
    pub fn mount(client: Arc<ControlClient>, cx: &mut App) -> (Entity<PanelHost>, PanelsFeed) {
        let feed = PanelsFeed::spawn(client);
        let view = {
            let feed = feed.clone();
            cx.new(|cx| PanelHost::new(feed, cx.focus_handle()))
        };
        (view, feed)
    }

    fn set_tab(&mut self, tab: PanelTab) {
        self.tab = tab;
        self.input = match tab {
            PanelTab::Run => InputTarget::Command,
            _ => InputTarget::Search,
        };
        if tab != PanelTab::Run {
            self.cmd_draft = None;
        }
    }

    fn on_key(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        match (self.tab, self.input) {
            (PanelTab::Files, _) => {
                let st = self.feed.state();
                let (query, viewer_open) = {
                    let s = st.lock();
                    (s.files.query.clone(), s.files.viewer.is_some())
                };
                match ks.key.as_str() {
                    "escape" => {
                        if viewer_open {
                            self.feed.send(PanelAction::CloseViewer);
                        } else if !query.is_empty() {
                            self.feed.send(PanelAction::SetQuery(String::new()));
                        }
                    }
                    "backspace" => {
                        if !query.is_empty() {
                            let mut q = query;
                            q.pop();
                            self.feed.send(PanelAction::SetQuery(q));
                        }
                    }
                    "enter" => {
                        // Open the top hit (webview: click; Enter is the
                        // keyboard shortcut for the same).
                        let (root, first) = {
                            let s = st.lock();
                            (s.files.root.clone(), s.files.hits.first().map(|h| h.rel_path.clone()))
                        };
                        if let (Some(root), Some(rel)) = (root, first) {
                            self.feed.send(PanelAction::OpenFile(format!("{root}/{rel}")));
                        }
                    }
                    _ => {
                        if !ks.modifiers.control && !ks.modifiers.platform {
                            if let Some(kc) = ks.key_char.as_deref() {
                                if !kc.is_empty() && !kc.chars().any(char::is_control) {
                                    self.feed.send(PanelAction::SetQuery(format!("{query}{kc}")));
                                }
                            }
                        }
                    }
                }
            }
            (PanelTab::Run, InputTarget::Command) => {
                let current = self.cmd_draft.clone().unwrap_or_else(|| {
                    let st = self.feed.state();
                    let s = st.lock();
                    s.selected_runner().map(|r| r.command.clone()).unwrap_or_default()
                });
                match ks.key.as_str() {
                    "enter" => {
                        if let Some(draft) = self.cmd_draft.take() {
                            self.feed.send(PanelAction::RunnerSetCommand(draft));
                        }
                    }
                    "escape" => {
                        self.cmd_draft = None;
                    }
                    "backspace" => {
                        let mut c = current;
                        c.pop();
                        self.cmd_draft = Some(c);
                    }
                    _ => {
                        if !ks.modifiers.control && !ks.modifiers.platform {
                            if let Some(kc) = ks.key_char.as_deref() {
                                if !kc.is_empty() && !kc.chars().any(char::is_control) {
                                    self.cmd_draft = Some(format!("{current}{kc}"));
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        cx.notify();
    }
}

impl Focusable for PanelHost {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for PanelHost {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Same liveness model as the overlays: background threads mutate the
        // shared state; the window repaints each frame.
        window.request_animation_frame();

        let mut root = div()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb8(BG))
            .text_color(rgb8(TEXT))
            .text_size(px(12.));
        if cfg!(target_os = "windows") {
            root = root.font_family("Segoe UI");
        }

        let body = match self.tab {
            PanelTab::Files => self.files_panel().into_any_element(),
            PanelTab::Preview => self.preview_panel().into_any_element(),
            PanelTab::Run => self.run_panel().into_any_element(),
        };

        root.child(self.tab_strip(cx)).child(body)
    }
}

// ---------------------------------------------------------------------------
// Chrome
// ---------------------------------------------------------------------------

impl PanelHost {
    fn tab_strip(&self, cx: &mut Context<Self>) -> Div {
        let mut strip = div()
            .flex()
            .items_center()
            .gap_1()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(rgb8(BORDER));
        for (tab, label) in
            [(PanelTab::Files, "Files"), (PanelTab::Preview, "Preview"), (PanelTab::Run, "Run")]
        {
            let active = self.tab == tab;
            strip = strip.child(
                div()
                    .id(label)
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .cursor_pointer()
                    .text_size(px(11.))
                    .when(active, |d| {
                        d.bg(rgba8(ACCENT, 0.15)).text_color(rgb8(TEXT)).font_weight(FontWeight::MEDIUM)
                    })
                    .when(!active, |d| d.text_color(rgb8(MUTED)).hover(|s| s.bg(rgba8(TEXT, 0.05))))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, _| this.set_tab(tab)))
                    .child(t(label)),
            );
        }
        strip
    }

    /// The `‹ project ›` picker row shared by Files and Run.
    fn project_picker(&self, projects: &[Project], selected: Option<&str>) -> Div {
        let name = match selected {
            Some(r) => match projects.iter().find(|p| p.root == r) {
                Some(p) => format!("{} ({})", p.name, p.sessions),
                // A host-bound root (feed.set_root) with no live session in it.
                None => r.trim_end_matches('/').rsplit('/').next().unwrap_or(r).to_string(),
            },
            None => "no project".to_string(),
        };
        let prev = self.feed.clone();
        let next = self.feed.clone();
        div()
            .flex()
            .items_center()
            .gap_1()
            .child(arrow_button("proj-prev", "‹", move |_| prev.send(PanelAction::CycleProject(-1))))
            .child(
                div()
                    .flex_1()
                    .text_size(px(11.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(rgb8(TEXT))
                    .truncate()
                    .child(t(name)),
            )
            .child(arrow_button("proj-next", "›", move |_| next.send(PanelAction::CycleProject(1))))
    }
}

fn arrow_button(
    id: &'static str,
    glyph: &'static str,
    on_click: impl Fn(&ClickEvent) + 'static,
) -> Stateful<Div> {
    div()
        .id(id)
        .px_1()
        .rounded_sm()
        .cursor_pointer()
        .text_color(rgb8(MUTED))
        .hover(|s| s.bg(rgba8(TEXT, 0.06)).text_color(rgb8(TEXT)))
        .on_click(move |ev, _, _| on_click(ev))
        .child(t(glyph))
}

fn hint(msg: impl Into<String>) -> Div {
    div().px_2().py_1().text_size(px(11.)).text_color(rgb8(FAINT)).child(t(msg.into()))
}

fn toggle_chip(
    id: &'static str,
    label: &'static str,
    on: bool,
    on_click: impl Fn(&ClickEvent) + 'static,
) -> Stateful<Div> {
    div()
        .id(id)
        .px_1()
        .rounded_sm()
        .cursor_pointer()
        .text_size(px(9.))
        .when(on, |d| d.text_color(rgb8(TEXT)).bg(rgba8(TEXT, 0.08)))
        .when(!on, |d| d.text_color(rgb8(FAINT)))
        .hover(|s| s.bg(rgba8(TEXT, 0.12)))
        .on_click(move |ev, _, _| on_click(ev))
        .child(t(label))
}

// ---------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------

impl PanelHost {
    fn files_panel(&self) -> Div {
        let state = self.feed.state();
        let (projects, selected, query, searching, tree, hits, viewer, git, root, error);
        {
            let s = state.lock();
            projects = s.projects.clone();
            selected = s.selected_root.clone();
            query = s.files.query.clone();
            searching = s.files.searching;
            tree = s.files.tree_rows();
            hits = s.files.hit_rows();
            viewer = s.files.viewer.clone();
            git = s.files.git.clone();
            root = s.files.root.clone();
            error = s.error.clone();
        }

        let mut panel = div().flex().flex_col().flex_1().min_h(px(0.)).px_2().pt_1();

        // Header: picker + git + toggles.
        let mut header = div().flex().items_center().gap_2().pb_1();
        header = header.child(div().flex_1().child(self.project_picker(&projects, selected.as_deref())));
        if let Some(g) = &git {
            if g.is_repo {
                let mut label = format!("⎇ {}", g.branch.as_deref().unwrap_or("detached"));
                if g.dirty_count > 0 {
                    label.push_str(&format!(" ·{}", g.dirty_count));
                }
                if g.is_linked_worktree {
                    label.push_str(" ⧉");
                }
                header = header.child(
                    div().text_size(px(10.)).text_color(rgb8(MUTED)).truncate().child(t(label)),
                );
            }
        }
        {
            let (dot_on, ign_on) = {
                let s = state.lock();
                (!s.files.hide_dotfiles, s.files.show_ignored)
            };
            let f1 = self.feed.clone();
            let f2 = self.feed.clone();
            let f3 = self.feed.clone();
            header = header
                .child(toggle_chip("dotfiles", ".dot", dot_on, move |_| {
                    f1.send(PanelAction::ToggleDotfiles)
                }))
                .child(toggle_chip("ignored", "ign", ign_on, move |_| {
                    f2.send(PanelAction::ToggleShowIgnored)
                }))
                .child(toggle_chip("refresh", "↻", false, move |_| {
                    f3.send(PanelAction::RefreshTree)
                }));
        }
        panel = panel.child(header);

        // Viewer takes over the panel body when open.
        if let Some(v) = viewer {
            let close = self.feed.clone();
            let rel = root
                .as_deref()
                .and_then(|r| v.path.strip_prefix(r))
                .map(|p| p.trim_start_matches('/').to_string())
                .unwrap_or_else(|| v.path.clone());
            panel = panel.child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .py_1()
                    .border_b_1()
                    .border_color(rgb8(BORDER))
                    .child(
                        div()
                            .id("viewer-close")
                            .px_1()
                            .cursor_pointer()
                            .text_color(rgb8(MUTED))
                            .hover(|s| s.text_color(rgb8(TEXT)))
                            .on_click(move |_, _, _| close.send(PanelAction::CloseViewer))
                            .child(t("‹ back")),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(11.))
                            .font_weight(FontWeight::MEDIUM)
                            .truncate()
                            .child(t(rel)),
                    )
                    .child(div().text_size(px(9.)).text_color(rgb8(FAINT)).child(t(fmt_size(v.size)))),
            );
            let mut body = div()
                .id("viewer-scroll")
                .flex()
                .flex_col()
                .flex_1()
                .min_h(px(0.))
                .overflow_y_scroll()
                .font_family(mono_family())
                .text_size(px(11.));
            if v.loading {
                body = body.child(hint("loading..."));
            } else if let Some(e) = &v.error {
                body = body.child(hint(format!("open failed: {e}")));
            } else {
                for line in &v.lines {
                    body = body.child(
                        div().whitespace_nowrap().child(t(if line.is_empty() {
                            " ".to_string()
                        } else {
                            line.clone()
                        })),
                    );
                }
                if v.truncated {
                    body = body.child(hint("… truncated at 2 MiB by the server read cap"));
                } else if v.clipped {
                    body = body.child(hint("… long file: remaining lines not rendered"));
                }
            }
            return panel.child(body);
        }

        // Search box (implicitly focused on the Files tab: type to search).
        let search_row = div()
            .flex()
            .items_center()
            .gap_1()
            .px_1()
            .py_1()
            .rounded_md()
            .bg(rgba8(TEXT, 0.05))
            .child(div().text_size(px(10.)).text_color(rgb8(FAINT)).child(t("⌕")))
            .child(if query.is_empty() {
                div().flex_1().text_size(px(11.)).text_color(rgb8(FAINT)).child(t("type to fuzzy-search"))
            } else {
                div()
                    .flex_1()
                    .text_size(px(11.))
                    .font_family(mono_family())
                    .truncate()
                    .child(t(format!("{query}▏")))
            })
            .child(if searching {
                div().text_size(px(9.)).text_color(rgb8(FAINT)).child(t("…"))
            } else {
                div()
            });
        panel = panel.child(search_row);

        // Hits (when searching) or the tree.
        let mut list = div()
            .id("files-scroll")
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .pt_1();
        if let Some(e) = &error {
            list = list.child(hint(format!("server unavailable: {e}")));
        }
        if !query.is_empty() {
            if hits.is_empty() && !searching {
                list = list.child(hint("no matches"));
            }
            for (ix, h) in hits.iter().enumerate() {
                list = list.child(self.hit_row(ix, h, root.as_deref()));
            }
        } else {
            if tree.is_empty() {
                list = list.child(hint(if projects.is_empty() {
                    "no live sessions -> no project roots yet"
                } else {
                    "empty"
                }));
            }
            for (ix, r) in tree.iter().enumerate() {
                list = list.child(self.tree_row(ix, r));
            }
        }
        panel.child(list)
    }

    fn tree_row(&self, ix: usize, row: &TreeRow) -> Stateful<Div> {
        let indent = px(8. + row.depth as f32 * 12.);
        if let Some(note) = &row.note {
            return div()
                .id(("tree-note", ix))
                .pl(indent)
                .text_size(px(10.))
                .text_color(rgb8(FAINT))
                .child(t(note.clone()));
        }
        let feed = self.feed.clone();
        let path = row.path.clone();
        let is_dir = row.is_dir;
        let glyph = if !row.is_dir {
            "  "
        } else if row.expanded {
            "▾ "
        } else {
            "▸ "
        };
        div()
            .id(("tree-row", ix))
            .flex()
            .items_center()
            .pl(indent)
            .pr_1()
            .py_0p5()
            .rounded_sm()
            .cursor_pointer()
            .hover(|s| s.bg(rgba8(TEXT, 0.06)))
            .on_click(move |_, _, _| {
                if is_dir {
                    feed.send(PanelAction::ToggleDir(path.clone()));
                } else {
                    feed.send(PanelAction::OpenFile(path.clone()));
                }
            })
            .child(div().text_size(px(9.)).text_color(rgb8(FAINT)).child(t(glyph)))
            .child(
                div()
                    .text_size(px(11.))
                    .font_family(mono_family())
                    .text_color(if row.is_dir { rgb8(TEXT) } else { rgb8(MUTED) })
                    .truncate()
                    .child(t(row.name.clone())),
            )
    }

    fn hit_row(&self, ix: usize, hit: &HitRow, root: Option<&str>) -> Stateful<Div> {
        let feed = self.feed.clone();
        let full = match root {
            Some(r) => format!("{r}/{}", hit.rel_path),
            None => hit.rel_path.clone(),
        };
        let mut text = div().flex_1().flex().overflow_hidden().font_family(mono_family()).text_size(px(11.));
        for (seg, hl) in segment_text(&hit.rel_path, &hit.spans) {
            text = text.child(
                div()
                    .whitespace_nowrap()
                    .when(hl, |d| d.text_color(rgb8(ACCENT)).font_weight(FontWeight::BOLD))
                    .when(!hl, |d| d.text_color(rgb8(MUTED)))
                    .child(t(seg)),
            );
        }
        let mut row = div()
            .id(("hit-row", ix))
            .flex()
            .items_center()
            .gap_1()
            .px_1()
            .py_0p5()
            .rounded_sm()
            .cursor_pointer()
            .hover(|s| s.bg(rgba8(TEXT, 0.06)))
            .on_click(move |_, _, _| feed.send(PanelAction::OpenFile(full.clone())))
            .child(text);
        if hit.is_key_file {
            row = row.child(div().text_size(px(9.)).text_color(rgb8(AMBER)).child(t("★")));
        }
        row
    }
}

fn fmt_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

// ---------------------------------------------------------------------------
// Preview
// ---------------------------------------------------------------------------

impl PanelHost {
    fn preview_panel(&self) -> Stateful<Div> {
        let state = self.feed.state();
        let groups: Vec<SessionUrls> = {
            let s = state.lock();
            s.preview.rows().into_iter().cloned().collect()
        };

        let mut panel = div()
            .id("preview-scroll")
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .px_2()
            .pt_1();
        if groups.is_empty() {
            return panel
                .child(hint("No local dev URLs detected yet."))
                .child(hint("URLs printed by any session (vite, next, wrangler, ...) appear here."));
        }
        for (gx, g) in groups.iter().enumerate() {
            let mut sub = g.session_title.to_string();
            if !g.cwd.is_empty() {
                let base = g.cwd.trim_end_matches('/').rsplit('/').next().unwrap_or("");
                sub = format!("{sub} · {base}");
            }
            if !g.live {
                sub.push_str(" · session gone");
            }
            panel = panel.child(
                div()
                    .pt_1()
                    .text_size(px(10.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(rgb8(MUTED))
                    .truncate()
                    .child(t(sub)),
            );
            for (ux, e) in g.urls.iter().enumerate() {
                let (color, status) = match &e.probe {
                    Probe::Unknown => (FAINT, String::new()),
                    Probe::Probing => (AMBER, "…".to_string()),
                    Probe::Reachable { status: Some(s) } => (GREEN, format!("{s}")),
                    Probe::Reachable { status: None } => (GREEN, "up".to_string()),
                    Probe::Refused => (RED, "down".to_string()),
                };
                let open = e.url.open_target();
                let session = g.session.clone();
                let canonical = e.url.canonical();
                let feed = self.feed.clone();
                let mut row = div()
                    .id(("url-row", gx * 100 + ux))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_1()
                    .py_0p5()
                    .rounded_sm()
                    .cursor_pointer()
                    .hover(|s| s.bg(rgba8(TEXT, 0.06)))
                    .on_click(move |_, _, _| crate::render::open_url(&open))
                    .child(div().w(px(6.)).h(px(6.)).rounded_full().bg(rgb8(color)).flex_none())
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_family(mono_family())
                            .text_color(rgb8(BLUE))
                            .truncate()
                            .child(t(e.url.open_target())),
                    );
                if let Some(title) = &e.title {
                    row = row.child(
                        div()
                            .text_size(px(10.))
                            .text_color(rgb8(MUTED))
                            .truncate()
                            .child(t(title.clone())),
                    );
                }
                if !status.is_empty() {
                    row = row.child(div().text_size(px(9.)).text_color(rgb8(color)).child(t(status)));
                }
                row = row.child(
                    div()
                        .id(("url-reprobe", gx * 100 + ux))
                        .px_1()
                        .text_size(px(10.))
                        .text_color(rgb8(FAINT))
                        .cursor_pointer()
                        .hover(|s| s.text_color(rgb8(TEXT)))
                        .on_click(move |_, _, cx| {
                            cx.stop_propagation();
                            feed.send(PanelAction::Reprobe {
                                session: session.clone(),
                                canonical: canonical.clone(),
                            });
                        })
                        .child(t("↻")),
                );
                panel = panel.child(row);
            }
        }
        panel = panel.child(
            div().pt_2().text_size(px(9.)).text_color(rgb8(FAINT)).child(t(
                "click = open in browser (no in-window embed in gpui 0.2.2) · ↻ = re-probe",
            )),
        );
        panel
    }
}

// ---------------------------------------------------------------------------
// Run
// ---------------------------------------------------------------------------

impl PanelHost {
    fn run_panel(&self) -> Div {
        let state = self.feed.state();
        let (projects, selected, runner) = {
            let s = state.lock();
            let runner = s.selected_runner().map(RunnerView::of);
            (s.projects.clone(), s.selected_root.clone(), runner)
        };

        let mut panel = div().flex().flex_col().flex_1().min_h(px(0.)).px_2().pt_1();
        panel = panel.child(
            div().pb_1().child(self.project_picker(&projects, selected.as_deref())),
        );
        if selected.is_none() {
            return panel.child(hint("no project selected"));
        }

        // Command line (click focuses typing on the Run tab; Enter commits).
        let editing = self.cmd_draft.is_some();
        let cmd_text = self
            .cmd_draft
            .clone()
            .or_else(|| runner.as_ref().map(|r| r.command.clone()))
            .unwrap_or_else(|| "npm run dev".to_string());
        panel = panel.child(
            div()
                .flex()
                .items_center()
                .gap_1()
                .px_1()
                .py_1()
                .rounded_md()
                .bg(rgba8(TEXT, 0.05))
                .child(div().text_size(px(10.)).text_color(rgb8(FAINT)).child(t("$")))
                .child(
                    div()
                        .flex_1()
                        .text_size(px(11.))
                        .font_family(mono_family())
                        .truncate()
                        .child(t(if editing { format!("{cmd_text}▏") } else { cmd_text.clone() })),
                )
                .child(div().text_size(px(9.)).text_color(rgb8(FAINT)).child(t(if editing {
                    "enter to save"
                } else {
                    "type to edit"
                }))),
        );

        // Status + controls.
        let (label, phase_color, can_start, can_stop, has_sid) = match runner.as_ref() {
            None => ("idle".to_string(), FAINT, true, false, false),
            Some(r) => {
                let color = match r.phase_kind {
                    PhaseKind::Running => GREEN,
                    PhaseKind::Busy => AMBER,
                    PhaseKind::Failed => RED,
                    PhaseKind::Idle => FAINT,
                };
                (r.label.clone(), color, r.can_start, r.can_stop, r.has_sid)
            }
        };
        let mut status_row = div()
            .flex()
            .items_center()
            .gap_2()
            .py_1()
            .child(div().w(px(6.)).h(px(6.)).rounded_full().bg(rgb8(phase_color)).flex_none())
            .child(div().text_size(px(10.)).text_color(rgb8(MUTED)).truncate().child(t(label)));
        if let Some(note) = runner.as_ref().and_then(|r| r.note.clone()) {
            status_row = status_row
                .child(div().text_size(px(9.)).text_color(rgb8(AMBER)).truncate().child(t(note)));
        }
        panel = panel.child(status_row);

        if let Some(url) = runner.as_ref().and_then(|r| r.url.clone()) {
            let open = url.clone();
            panel = panel.child(
                div()
                    .id("runner-url")
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_1()
                    .py_0p5()
                    .rounded_sm()
                    .cursor_pointer()
                    .hover(|s| s.bg(rgba8(TEXT, 0.06)))
                    .on_click(move |_, _, _| crate::render::open_url(&open))
                    .child(div().text_size(px(10.)).text_color(rgb8(FAINT)).child(t("↗")))
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_family(mono_family())
                            .text_color(rgb8(BLUE))
                            .truncate()
                            .child(t(url)),
                    ),
            );
        }

        let mut controls = div().flex().items_center().gap_2().py_1();
        if can_start {
            let feed = self.feed.clone();
            controls = controls.child(action_button("runner-start", "▶ Run", GREEN, move |_| {
                feed.send(PanelAction::RunnerStart)
            }));
        }
        if can_stop {
            let feed = self.feed.clone();
            controls = controls.child(action_button("runner-stop", "■ Stop", AMBER, move |_| {
                feed.send(PanelAction::RunnerStop)
            }));
        }
        if has_sid {
            let feed = self.feed.clone();
            controls = controls.child(action_button("runner-kill", "✕ Kill session", RED, move |_| {
                feed.send(PanelAction::RunnerKill)
            }));
        }
        panel = panel.child(controls);

        // Tail.
        let tail = runner.map(|r| r.tail).unwrap_or_default();
        let mut tail_box = div()
            .id("runner-tail")
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .p_1()
            .rounded_md()
            .bg(rgba8(TEXT, 0.03))
            .font_family(mono_family())
            .text_size(px(10.))
            .text_color(rgb8(MUTED));
        if tail.is_empty() {
            tail_box = tail_box.child(hint("session output appears here"));
        }
        for line in &tail {
            tail_box = tail_box.child(div().whitespace_nowrap().child(t(if line.is_empty() {
                " ".to_string()
            } else {
                line.clone()
            })));
        }
        panel.child(tail_box)
    }
}

fn action_button(
    id: &'static str,
    label: &'static str,
    color: (u8, u8, u8),
    on_click: impl Fn(&ClickEvent) + 'static,
) -> Stateful<Div> {
    div()
        .id(id)
        .px_2()
        .py_0p5()
        .rounded_md()
        .cursor_pointer()
        .text_size(px(11.))
        .bg(rgba8(color, 0.12))
        .text_color(rgb8(color))
        .hover(|s| s.bg(rgba8(color, 0.25)))
        .on_click(move |ev, _, _| on_click(ev))
        .child(t(label))
}

/// Plain-data snapshot of a runner for painting (lock dropped before layout).
struct RunnerView {
    command: String,
    label: String,
    note: Option<String>,
    url: Option<String>,
    tail: Vec<String>,
    phase_kind: PhaseKind,
    can_start: bool,
    can_stop: bool,
    has_sid: bool,
}

enum PhaseKind {
    Idle,
    Busy,
    Running,
    Failed,
}

impl RunnerView {
    fn of(r: &RunnerState) -> Self {
        let phase_kind = match r.phase {
            Phase::Running { .. } => PhaseKind::Running,
            Phase::Spawning { .. } | Phase::Adopting { .. } | Phase::Stopping { .. } => {
                PhaseKind::Busy
            }
            Phase::Failed { .. } => PhaseKind::Failed,
            Phase::Idle | Phase::Ready { .. } | Phase::Exited { .. } => PhaseKind::Idle,
        };
        let can_start = matches!(
            r.phase,
            Phase::Idle | Phase::Failed { .. } | Phase::Ready { .. } | Phase::Exited { .. }
        );
        let tail_keep = 120;
        let tail = if r.tail.len() > tail_keep {
            r.tail[r.tail.len() - tail_keep..].to_vec()
        } else {
            r.tail.clone()
        };
        RunnerView {
            command: r.command.clone(),
            label: r.status_label(),
            note: r.note.clone(),
            url: r.url.clone(),
            tail,
            phase_kind,
            can_start,
            can_stop: matches!(r.phase, Phase::Running { .. }),
            has_sid: r.sid().is_some(),
        }
    }
}
