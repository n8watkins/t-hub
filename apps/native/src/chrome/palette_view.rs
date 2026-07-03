//! GPUI rendering for the T-A keymap chrome: the command palette modal, the
//! armed-prefix HUD pill, and the sidebar-focus-region pill. This is the ONLY
//! keymap file that touches gpui - every decision about WHAT to show was made
//! by the gpui-free models ([`crate::chrome::palette::PaletteState`],
//! [`crate::chrome::keymap::KeyController`]); the fns here just draw plain
//! data, the `overlays/view.rs` way.
//!
//! Input never lands here: the palette is keyboard-driven and the cockpit's
//! `on_key` routes every keystroke through the controller first, so this
//! element carries no listeners (and therefore never fights the canvas's
//! mouse hit zones underneath it).

use gpui::prelude::*;
use gpui::{div, px, Div, FontWeight, Hsla, Rgba, SharedString};

use crate::chrome::actions::Region;
use crate::chrome::keymap::{binding_hint, KeyController};

// The overlays/view.rs palette (itself the app.rs dark palette + the webview
// text scale), so the modal reads as one family with the sidebar.
const BG: (u8, u8, u8) = (13, 17, 23);
const ROW_ACTIVE: (u8, u8, u8) = (33, 42, 54);
const TEXT: (u8, u8, u8) = (216, 222, 233);
const MUTED: (u8, u8, u8) = (140, 149, 164);
const FAINT: (u8, u8, u8) = (94, 102, 116);
const BORDER: (u8, u8, u8) = (38, 44, 54);
const ACCENT: (u8, u8, u8) = (128, 200, 255);
const AMBER: (u8, u8, u8) = (230, 180, 80);

/// Rows the list shows at once; the selection windows through longer results.
const VISIBLE_ROWS: usize = 12;

fn rgb8(c: (u8, u8, u8)) -> Hsla {
    Rgba { r: c.0 as f32 / 255.0, g: c.1 as f32 / 255.0, b: c.2 as f32 / 255.0, a: 1.0 }.into()
}

fn rgba8(c: (u8, u8, u8), a: f32) -> Hsla {
    Rgba { r: c.0 as f32 / 255.0, g: c.1 as f32 / 255.0, b: c.2 as f32 / 255.0, a }.into()
}

fn t(s: impl Into<SharedString>) -> SharedString {
    s.into()
}

/// The keymap overlay stack for one window frame: palette modal (top-center)
/// and/or the prefix-HUD / sidebar-region pill (bottom-center). `None` when
/// nothing keymap-related is on screen - the common case, costing nothing.
pub fn key_overlay(keys: &mut KeyController, now: u64) -> Option<Div> {
    let hud = keys.hud_label(now);
    let sidebar = keys.region == Region::Sidebar;
    if !keys.palette.open && hud.is_none() && !sidebar {
        return None;
    }

    let mut root = div().absolute().inset_0().flex().flex_col().items_center().text_size(px(12.));
    if cfg!(target_os = "windows") {
        root = root.font_family("Segoe UI");
    }
    if keys.palette.open {
        root = root.child(palette_modal(keys));
    }
    root = root.child(div().flex_1()); // pin the pills to the bottom
    if let Some(label) = hud {
        root = root.child(pill(
            ACCENT,
            vec![(format!("{label} "), ACCENT, true), ("- waiting for key...".into(), MUTED, false)],
        ));
    } else if sidebar {
        root = root.child(pill(
            AMBER,
            vec![
                ("SIDEBAR ".into(), AMBER, true),
                ("- arrows switch workspace, Enter returns".into(), MUTED, false),
            ],
        ));
    }
    Some(root.child(div().h(px(18.))))
}

/// A bottom-center status pill (the webview `PrefixHint` look).
fn pill(border: (u8, u8, u8), parts: Vec<(String, (u8, u8, u8), bool)>) -> Div {
    let mut row = div()
        .flex()
        .flex_row()
        .px_3()
        .py_1()
        .rounded_md()
        .bg(rgba8(BG, 0.92))
        .border_1()
        .border_color(rgba8(border, 0.65));
    for (text, color, bold) in parts {
        let mut span = div().text_color(rgb8(color)).child(t(text));
        if bold {
            span = span.font_weight(FontWeight::BOLD);
        }
        row = row.child(span);
    }
    row
}

/// The palette modal: query line, the windowed result rows, footer hints.
fn palette_modal(keys: &KeyController) -> Div {
    let p = &keys.palette;
    let total = p.results.len();

    // Window the rows around the selection (no scroll machinery: the window
    // slides so the highlighted row is always on screen).
    let start = if total <= VISIBLE_ROWS {
        0
    } else {
        p.selected.saturating_sub(VISIBLE_ROWS - 1).min(total - VISIBLE_ROWS)
    };
    let end = (start + VISIBLE_ROWS).min(total);

    let query_line: Div = if p.rebind.is_some() {
        div()
            .text_color(rgb8(ACCENT))
            .child(t("Press a key combination... (Esc to cancel)"))
    } else if p.query.is_empty() {
        div().text_color(rgb8(FAINT)).child(t("Type to search commands..."))
    } else {
        div().text_color(rgb8(TEXT)).child(t(format!("> {}", p.query)))
    };

    let mut list = div().flex().flex_col();
    if total == 0 {
        list = list.child(
            div().px_2().py_1().text_color(rgb8(FAINT)).child(t("No matching commands.")),
        );
    }
    for (i, &cmd) in p.results[start..end].iter().enumerate() {
        let idx = start + i;
        let selected = idx == p.selected;
        let rebinding = selected && p.rebind.is_some();

        let hint: (String, (u8, u8, u8)) = if rebinding {
            ("press a key...".into(), ACCENT)
        } else {
            (binding_hint(&keys.keymap, cmd), MUTED)
        };

        let mut row = div().flex().flex_col().px_2().py_1();
        if selected {
            row = row.bg(rgb8(ROW_ACTIVE)).border_l_2().border_color(rgb8(ACCENT));
        }
        let mut main = div()
            .flex()
            .flex_row()
            .justify_between()
            .child(div().text_color(rgb8(TEXT)).child(t(cmd.label())));
        main = main.child(
            div()
                .flex()
                .flex_row()
                .gap_2()
                .child(div().text_color(rgb8(hint.1)).child(t(hint.0)))
                .child(div().text_color(rgb8(FAINT)).child(t(cmd.category()))),
        );
        row = row.child(main);
        if selected {
            row = row.child(
                div().text_size(px(11.)).text_color(rgb8(MUTED)).child(t(cmd.description())),
            );
        }
        list = list.child(row);
    }

    let mut footer = div()
        .flex()
        .flex_row()
        .justify_between()
        .px_2()
        .py_1()
        .border_t_1()
        .border_color(rgb8(BORDER))
        .text_size(px(10.))
        .text_color(rgb8(FAINT))
        .child(t("Up/Down navigate · Enter run · F2/Ctrl+R rebind · Esc close"));
    if total > VISIBLE_ROWS {
        footer = footer.child(t(format!("{} of {total}", p.selected + 1)));
    }

    div()
        .mt(px(80.))
        .w(px(560.))
        .flex()
        .flex_col()
        .bg(rgba8(BG, 0.97))
        .border_1()
        .border_color(rgb8(BORDER))
        .rounded_md()
        .child(
            div()
                .px_2()
                .py_1p5()
                .border_b_1()
                .border_color(rgb8(BORDER))
                .child(query_line),
        )
        .child(list)
        .child(footer)
}
