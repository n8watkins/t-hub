//! GPUI rendering for the sidebar overlays (T9). This is the ONLY overlay file
//! that touches gpui - every decision about WHAT to show was already made by the
//! gpui-free view-models (`rows()`/`active()`/`visible()`), so the fns here just
//! paint plain data.
//!
//! ## Mount contract (T8 / captain - also documented in the execution doc §5)
//!
//! ```ignore
//! let feed = OverlayFeed::spawn(client.clone());          // once per process
//! let overlays = cx.new(|_| OverlaySidebar::new(feed.clone()));
//! // in the sidebar shell, below the workspace list:
//! div().flex_1().child(overlays.clone())
//! // optional hooks:
//! feed.set_active_sessions(active);                       // tab-aware toast suppression
//! let host_rx = feed.host_requests();                     // ResumeSession{id,cwd} -> spawn a tile
//! ```
//! Or in one step: `OverlaySidebar::mount(client, cx) -> (Entity<_>, OverlayFeed)`.

use std::sync::Arc;

use gpui::prelude::*;
use gpui::{
    div, px, relative, App, ClickEvent, Context, Div, Entity, FontWeight, Hsla, Render, Rgba,
    SharedString, Window,
};

use super::feed::{OverlayAction, OverlayFeed};
use super::metrics::MetricLine;
use super::model::now_ms;
use super::recents::RecentRow;
use super::supervision::TreeView;
use super::toasts::Toast;
use super::usage::{Meter, UsageRows};
use crate::wire::ControlClient;

// The app.rs dark palette + the webview's neutral text scale.
const BG: (u8, u8, u8) = (13, 17, 23);
const TEXT: (u8, u8, u8) = (216, 222, 233);
const MUTED: (u8, u8, u8) = (140, 149, 164);
const FAINT: (u8, u8, u8) = (94, 102, 116);
const BORDER: (u8, u8, u8) = (38, 44, 54);
const CLAUDE: (u8, u8, u8) = (217, 119, 87); // #D97757

fn rgb8(c: (u8, u8, u8)) -> Hsla {
    Rgba { r: c.0 as f32 / 255.0, g: c.1 as f32 / 255.0, b: c.2 as f32 / 255.0, a: 1.0 }.into()
}

fn rgba8(c: (u8, u8, u8), a: f32) -> Hsla {
    Rgba { r: c.0 as f32 / 255.0, g: c.1 as f32 / 255.0, b: c.2 as f32 / 255.0, a }.into()
}

fn t(s: impl Into<SharedString>) -> SharedString {
    s.into()
}

/// The composed sidebar element: recents, supervision, toasts, usage, WSL
/// metrics, stacked in one flex column. Fills whatever box the host gives it
/// (the webview sidebar is 180-360px wide; anything in that range reads fine).
pub struct OverlaySidebar {
    feed: OverlayFeed,
    recents_open: bool,
    supervision_open: bool,
    usage_open: bool,
    wsl_open: bool,
}

impl OverlaySidebar {
    pub fn new(feed: OverlayFeed) -> Self {
        // Webview defaults: content sections expanded, bottom chrome collapsed
        // to their one-line summaries.
        Self { feed, recents_open: true, supervision_open: true, usage_open: false, wsl_open: false }
    }

    /// One-step mount: spawn the feed and create the view entity. Returns the
    /// feed too so the host can wire `host_requests()`/`set_active_sessions()`.
    pub fn mount(client: Arc<ControlClient>, cx: &mut App) -> (Entity<OverlaySidebar>, OverlayFeed) {
        let feed = OverlayFeed::spawn(client);
        let view = cx.new(|_| OverlaySidebar::new(feed.clone()));
        (view, feed)
    }
}

impl Render for OverlaySidebar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Same liveness model as GridView/DebugView: background threads mutate
        // shared state; the window repaints each frame.
        window.request_animation_frame();
        let now = now_ms();

        // Snapshot the view-models and drop the lock before building elements.
        let state = self.feed.state();
        let (toasts, recent_rows, recents_loaded, recents_error, trees, usage, metric_lines, wsl_summary, wsl_stale, wsl_error);
        {
            let mut st = state.lock();
            st.toasts.tick(now);
            toasts = st.toasts.visible();
            recent_rows = st.recents.rows(&st.index.open_cwds(), now);
            recents_loaded = st.recents.loaded;
            recents_error = st.recents.error.clone();
            trees = st.supervision.active();
            usage = st.usage.rows(now);
            metric_lines = st.metrics.rows();
            wsl_summary = st.metrics.summary();
            wsl_stale = st.metrics.is_stale(now);
            wsl_error = st.metrics.error.clone();
        }

        let mut root = div()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb8(BG))
            .text_color(rgb8(TEXT))
            .text_size(px(12.));
        if cfg!(target_os = "windows") {
            root = root.font_family("Segoe UI");
        }

        root
            .child(self.recents_section(&recent_rows, recents_loaded, recents_error.as_deref(), cx))
            .child(self.supervision_section(&trees, cx))
            .child(div().flex_1()) // push toasts + bottom chrome down
            .child(toasts_block(&toasts, &self.feed))
            .child(self.usage_section(&usage, cx))
            .child(self.wsl_section(&metric_lines, wsl_summary, wsl_stale, wsl_error.as_deref(), cx))
    }
}

impl OverlaySidebar {
    fn recents_section(
        &self,
        rows: &[RecentRow],
        loaded: bool,
        error: Option<&str>,
        cx: &mut Context<Self>,
    ) -> Div {
        let header = section_header(
            "recents-header",
            "Recent",
            Some(count_hint(rows.len())),
            self.recents_open,
            cx.listener(|this, _: &ClickEvent, _, _| this.recents_open = !this.recents_open),
        );
        let mut section = div().flex().flex_col().px_2().pt_2().child(header);
        if !self.recents_open {
            return section;
        }
        if rows.is_empty() {
            let hint = match (loaded, error) {
                (_, Some(e)) => format!("recents unavailable: {e}"),
                (false, None) => "Loading...".to_string(),
                (true, None) => "No recent Claude sessions to resume.".to_string(),
            };
            return section.child(empty_hint(hint));
        }
        let mut list = div().id("recents-scroll").flex().flex_col().max_h(px(280.)).overflow_y_scroll();
        for (ix, row) in rows.iter().enumerate() {
            list = list.child(recent_row(ix, row, &self.feed));
        }
        section = section.child(list);
        section
    }

    fn supervision_section(&self, trees: &[TreeView], cx: &mut Context<Self>) -> Div {
        let header = section_header(
            "supervision-header",
            "Supervision",
            Some(count_hint(trees.len())),
            self.supervision_open,
            cx.listener(|this, _: &ClickEvent, _, _| this.supervision_open = !this.supervision_open),
        );
        let section = div().flex().flex_col().px_2().pt_2().child(header);
        if !self.supervision_open {
            return section;
        }
        if trees.is_empty() {
            return section.child(empty_hint("No subagent activity yet.".to_string()));
        }
        let mut list = div().id("supervision-scroll").flex().flex_col().gap_1().max_h(px(260.)).overflow_y_scroll();
        for tree in trees {
            list = list.child(tree_block(tree));
        }
        section.child(list)
    }

    fn usage_section(&self, usage: &UsageRows, cx: &mut Context<Self>) -> Div {
        let inline = if self.usage_open { None } else { Some(usage_inline(usage)) };
        let header = section_header(
            "usage-header",
            "Usage",
            inline,
            self.usage_open,
            cx.listener(|this, _: &ClickEvent, _, _| this.usage_open = !this.usage_open),
        );
        let mut section = div()
            .flex()
            .flex_col()
            .px_2()
            .py_1()
            .border_t_1()
            .border_color(rgb8(BORDER))
            .child(header);
        if !self.usage_open {
            return section;
        }
        let both = !usage.codex.is_empty();
        if both {
            section = section.child(provider_label("Claude", CLAUDE));
        }
        for m in &usage.claude {
            section = section.child(meter_row(m));
        }
        if both {
            section = section.child(provider_label("Codex", MUTED));
            for m in &usage.codex {
                section = section.child(meter_row(m));
            }
        }
        if let Some(cost) = usage.total_cost_usd {
            section = section.child(
                div()
                    .flex()
                    .justify_between()
                    .text_size(px(10.))
                    .text_color(rgb8(MUTED))
                    .child(t("Cost (all sessions)"))
                    .child(t(format!("${cost:.2}"))),
            );
        }
        section
    }

    fn wsl_section(
        &self,
        lines: &[MetricLine],
        summary: Option<String>,
        stale: bool,
        error: Option<&str>,
        cx: &mut Context<Self>,
    ) -> Div {
        let mut hint = summary.unwrap_or_default();
        if stale {
            hint.push_str(" (stale)");
        }
        let header = section_header(
            "wsl-header",
            "WSL",
            if hint.is_empty() { None } else { Some(div().text_size(px(10.)).text_color(rgb8(MUTED)).child(t(hint))) },
            self.wsl_open,
            cx.listener(|this, _: &ClickEvent, _, _| this.wsl_open = !this.wsl_open),
        );
        let mut section = div()
            .flex()
            .flex_col()
            .px_2()
            .py_1()
            .border_t_1()
            .border_color(rgb8(BORDER))
            .child(header);
        if !self.wsl_open {
            return section;
        }
        if lines.is_empty() {
            let msg = error.map(|e| format!("host metrics unavailable: {e}")).unwrap_or_else(|| "Waiting for host metrics...".to_string());
            return section.child(empty_hint(msg));
        }
        for line in lines {
            let color = line.color.map(rgb8).unwrap_or_else(|| rgb8(MUTED));
            section = section.child(
                div()
                    .flex()
                    .justify_between()
                    .text_size(px(11.))
                    .child(div().text_color(rgb8(FAINT)).child(t(line.label)))
                    .child(div().text_color(color).truncate().child(t(line.value.clone()))),
            );
        }
        section
    }
}

/// Collapsible section header: chevron + uppercase label, optional right-side
/// hint, whole row clickable.
fn section_header(
    id: &'static str,
    title: &'static str,
    right: Option<Div>,
    open: bool,
    on_toggle: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> gpui::Stateful<Div> {
    let chevron = if open { "▾" } else { "▸" };
    let mut header = div()
        .id(id)
        .flex()
        .items_center()
        .gap_1()
        .py_1()
        .cursor_pointer()
        .hover(|s| s.bg(rgba8(TEXT, 0.04)))
        .rounded_sm()
        .child(div().text_size(px(9.)).text_color(rgb8(FAINT)).child(t(chevron)))
        .child(
            div()
                .text_size(px(10.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(rgb8(MUTED))
                .child(t(title.to_uppercase())),
        );
    if let Some(right) = right {
        header = header.child(div().flex_1()).child(right);
    }
    header.on_click(on_toggle)
}

fn count_hint(n: usize) -> Div {
    div().text_size(px(10.)).text_color(rgb8(FAINT)).child(t(format!("{n}")))
}

fn empty_hint(msg: String) -> Div {
    div().px_2().py_1().text_size(px(11.)).text_color(rgb8(FAINT)).truncate().child(t(msg))
}

/// One recent-session row: click = resume; the small × = dismiss/archive.
fn recent_row(ix: usize, row: &RecentRow, feed: &OverlayFeed) -> impl IntoElement {
    let mut subtitle = String::new();
    if let Some(wt) = &row.worktree {
        subtitle.push_str(&format!("⎇ {wt} · "));
    } else if row.title != row.folder {
        subtitle.push_str(&format!("{} · ", row.folder));
    }
    subtitle.push_str(&row.age);

    let resume_feed = feed.clone();
    let (resume_id, resume_cwd) = (row.id.clone(), row.cwd.clone());
    let dismiss_feed = feed.clone();
    let dismiss_cwd = row.cwd.clone();

    div()
        .id(("recent-row", ix))
        .flex()
        .items_center()
        .gap_2()
        .px_2()
        .py_1()
        .rounded_md()
        .cursor_pointer()
        .hover(|s| s.bg(rgba8(TEXT, 0.06)))
        .on_click(move |_, _, _| {
            resume_feed.send(OverlayAction::Resume {
                session_id: resume_id.clone(),
                cwd: resume_cwd.clone(),
            });
        })
        .child(div().text_size(px(12.)).text_color(rgb8(CLAUDE)).child(t("✳")))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .overflow_hidden()
                .child(
                    div()
                        .text_size(px(12.))
                        .font_weight(FontWeight::MEDIUM)
                        .truncate()
                        .child(t(row.title.clone())),
                )
                .child(div().text_size(px(10.)).text_color(rgb8(MUTED)).truncate().child(t(subtitle))),
        )
        .child(
            div()
                .id(("recent-x", ix))
                .px_1()
                .text_size(px(11.))
                .text_color(rgb8(FAINT))
                .cursor_pointer()
                .hover(|s| s.text_color(rgb8(TEXT)))
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    dismiss_feed.send(OverlayAction::Archive { cwd: dismiss_cwd.clone() });
                })
                .child(t("×")),
        )
}

/// One orchestrator tree block: status dot + label, counts line, child rows.
fn tree_block(tree: &TreeView) -> impl IntoElement {
    let mut counts = format!("{} running · {} done", tree.running, tree.done);
    if tree.outstanding_tasks > 0 {
        counts.push_str(&format!(" · {} task(s)", tree.outstanding_tasks));
    }
    let mut block = div()
        .flex()
        .flex_col()
        .px_2()
        .py_1()
        .rounded_md()
        .bg(rgba8(TEXT, 0.03))
        .child(
            div()
                .flex()
                .items_center()
                .gap_1()
                .child(status_dot(tree.status.color()))
                .child(
                    div()
                        .flex_1()
                        .text_size(px(11.))
                        .font_weight(FontWeight::MEDIUM)
                        .truncate()
                        .child(t(tree.label.clone())),
                )
                .child(
                    div().text_size(px(9.)).text_color(rgb8(MUTED)).child(t(tree.status.label())),
                ),
        )
        .child(div().text_size(px(10.)).text_color(rgb8(FAINT)).child(t(counts)));
    for child in &tree.children {
        let color = if child.running { TEXT } else { MUTED };
        let mut row = div()
            .flex()
            .items_center()
            .gap_1()
            .pl_2()
            .child(status_dot(if child.running { (96, 165, 250) } else { (115, 115, 115) }))
            .child(div().flex_1().text_size(px(10.)).text_color(rgb8(color)).truncate().child(t(child.label.clone())));
        if let Some(d) = &child.duration {
            row = row.child(div().text_size(px(9.)).text_color(rgb8(FAINT)).child(t(d.clone())));
        }
        block = block.child(row);
    }
    block
}

fn status_dot(color: (u8, u8, u8)) -> Div {
    div().w(px(6.)).h(px(6.)).rounded_full().bg(rgb8(color)).flex_none()
}

/// The transient toast cards (click to dismiss).
fn toasts_block(toasts: &[Toast], feed: &OverlayFeed) -> Div {
    let mut block = div().flex().flex_col().gap_1().px_2().pb_1();
    for toast in toasts {
        let state = feed.state();
        let seq = toast.seq;
        block = block.child(
            div()
                .id(("toast", toast.seq as usize))
                .flex()
                .flex_col()
                .px_2()
                .py_1()
                .rounded_md()
                .bg(rgba8(toast.kind.color(), 0.12))
                .border_l_2()
                .border_color(rgb8(toast.kind.color()))
                .cursor_pointer()
                .on_click(move |_, _, _| state.lock().toasts.dismiss(seq))
                .child(
                    div()
                        .text_size(px(11.))
                        .font_weight(FontWeight::MEDIUM)
                        .truncate()
                        .child(t(toast.title.clone())),
                )
                .child(
                    div().text_size(px(10.)).text_color(rgb8(MUTED)).truncate().child(t(toast.body.clone())),
                ),
        );
    }
    block
}

/// Collapsed usage readout: "wk 66% · 5h 40%" with per-value colors.
fn usage_inline(usage: &UsageRows) -> Div {
    let mut inline = div().flex().items_center().gap_1().text_size(px(10.));
    let mut spans: Vec<(String, (u8, u8, u8))> = Vec::new();
    for (short, m) in [("wk", usage.claude.first()), ("5h", usage.claude.get(1))] {
        if let (Some(m), Some(left)) = (m, m.and_then(|m| m.left_pct)) {
            spans.push((format!("{short} {left:.0}%"), m.color));
        }
    }
    if spans.is_empty() {
        spans.push(("-".to_string(), MUTED));
    }
    for (text, color) in spans {
        inline = inline.child(div().text_color(rgb8(color)).child(t(text)));
    }
    inline
}

fn provider_label(name: &'static str, color: (u8, u8, u8)) -> Div {
    div()
        .flex()
        .items_center()
        .gap_1()
        .pt_1()
        .child(div().w(px(6.)).h(px(6.)).rounded_full().bg(rgb8(color)))
        .child(div().text_size(px(10.)).text_color(rgb8(MUTED)).child(t(name)))
}

/// One usage meter: label + "NN% left" + the used-fraction bar.
fn meter_row(m: &Meter) -> Div {
    let readout = match m.left_pct {
        Some(left) => format!("{left:.0}% left"),
        None => "-".to_string(),
    };
    let mut row = div()
        .flex()
        .flex_col()
        .py_1()
        .child(
            div()
                .flex()
                .justify_between()
                .text_size(px(10.))
                .child(div().text_color(rgb8(FAINT)).child(t(m.label)))
                .child(div().text_color(rgb8(if m.left_pct.is_some() { TEXT } else { FAINT })).child(t(readout))),
        )
        .child(
            div().h(px(3.)).w_full().rounded_full().bg(rgba8(TEXT, 0.08)).child(
                div()
                    .h_full()
                    .rounded_full()
                    .w(relative((m.used_pct.unwrap_or(0.0) / 100.0).clamp(0.0, 1.0)))
                    .bg(rgb8(m.color)),
            ),
        );
    if let Some(resets) = &m.resets {
        row = row.child(div().text_size(px(9.)).text_color(rgb8(FAINT)).child(t(resets.clone())));
    }
    row
}
