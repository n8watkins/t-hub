//! Human-facing rendering (AXI design language): terse, structured, aligned
//! columns, and a "Next" footer of suggested follow-up commands after every
//! view. Machine mode (`--json`) never reaches here — it emits the stable
//! envelope in `main.rs`. These helpers are the human half only.
//!
//! Two knobs, both on [`Ui`]:
//!   - `tty`: when stdout is NOT a terminal (an agent piped us), we drop the
//!     decorative column padding and keep the terse structured form. We never
//!     emit color, spinners, or cursor-move escapes in either mode.
//!   - `all`: human lists are capped (default 20) with a "showing N of M" note
//!     so output stays bounded for token economy; `--all` lifts the cap. JSON
//!     mode always carries the full, sorted set.

use serde_json::Value;

/// Default cap on rows shown in human mode (lift with `--all`).
pub const LIMIT: usize = 20;

/// Rendering context threaded through every view.
#[derive(Clone, Copy)]
pub struct Ui {
    /// stdout is a real terminal (align columns); false when piped.
    pub tty: bool,
    /// `--all` was passed (show every row, no cap).
    pub all: bool,
}

// ---- shared field / sort helpers -------------------------------------------

fn str_field(v: &Value, key: &str) -> String {
    v.get(key).and_then(|x| x.as_str()).unwrap_or("").to_string()
}

/// The `terminals` array from `list_terminals`, sorted by `id` for stable diffs.
pub fn sort_terminals(result: &Value) -> Vec<Value> {
    let mut arr = result
        .get("terminals")
        .and_then(|t| t.as_array())
        .cloned()
        .unwrap_or_default();
    arr.sort_by(|a, b| str_field(a, "id").cmp(&str_field(b, "id")));
    arr
}

/// The `tabs` array from `list_tabs`, sorted by `id` for stable diffs.
pub fn sort_tabs(result: &Value) -> Vec<Value> {
    let mut arr = result
        .get("tabs")
        .and_then(|t| t.as_array())
        .cloned()
        .unwrap_or_default();
    arr.sort_by(|a, b| str_field(a, "id").cmp(&str_field(b, "id")));
    arr
}

// ---- row formatting (tty-aware) --------------------------------------------

/// Format one row of columns. In tty mode, pad each column to `widths` for an
/// aligned table; piped, join with two spaces and no padding (terse, still
/// structured). Trailing empty columns are dropped.
fn row(ui: &Ui, cols: &[&str], widths: &[usize]) -> String {
    let last = cols.iter().rposition(|c| !c.is_empty()).unwrap_or(0);
    let mut out = String::from("  ");
    for (i, c) in cols.iter().enumerate().take(last + 1) {
        if i > 0 {
            out.push_str("  ");
        }
        if ui.tty && i < last {
            out.push_str(&format!("{:<width$}", c, width = widths.get(i).copied().unwrap_or(0)));
        } else {
            out.push_str(c);
        }
    }
    out
}

/// Print the `Next` footer: suggested, runnable follow-up commands. Column
/// alignment only in tty mode (it's decorative).
pub fn next(ui: &Ui, hints: &[(String, &str)]) {
    if hints.is_empty() {
        return;
    }
    let width = hints.iter().map(|(c, _)| c.len()).max().unwrap_or(0);
    println!("\nNext");
    for (cmd, desc) in hints {
        if ui.tty {
            println!("  {:<width$}  {}", cmd, desc, width = width);
        } else {
            println!("  {}  {}", cmd, desc);
        }
    }
}

/// The first session id (sorted) if any — used to make hints runnable verbatim.
fn first_id(terms: &[Value]) -> Option<String> {
    terms.first().map(|t| str_field(t, "id"))
}

/// Build a `th read <id>` style hint, filling a concrete id when we have one.
fn read_hint(id: &Option<String>) -> (String, &'static str) {
    match id {
        Some(i) => (format!("th read {i}"), "view that terminal's recent output"),
        None => ("th read <session>".to_string(), "view a terminal's recent output"),
    }
}

fn status_hint(id: &Option<String>) -> (String, &'static str) {
    match id {
        Some(i) => (format!("th status {i}"), "status + supervision tree for that session"),
        None => ("th status <session>".to_string(), "status for one session + its tree"),
    }
}

// ---- views ------------------------------------------------------------------

/// The fleet home view: live terminals + tabs + runnable next-commands footer.
pub fn home(terminals: &Value, tabs: &Value, ui: &Ui) {
    let terms = sort_terminals(terminals);
    let tabs_v = sort_tabs(tabs);

    println!(
        "T-Hub  ·  {} live terminal{}  ·  {} tab{}",
        terms.len(),
        plural(terms.len()),
        tabs_v.len(),
        plural(tabs_v.len()),
    );

    println!("\nTERMINALS");
    print_terminals(&terms, ui);

    println!("\nTABS");
    print_tabs(&tabs_v, tabs, ui);

    let id = first_id(&terms);
    next(
        ui,
        &[
            read_hint(&id),
            ("th status".to_string(), "FR-012 status across all sessions"),
            ("th health".to_string(), "WSL host snapshot"),
            ("th tabs".to_string(), "list workspace tabs"),
        ],
    );
}

/// `th ls` — the terminals table + footer.
pub fn terminals(result: &Value, ui: &Ui) {
    let terms = sort_terminals(result);
    println!("{} live terminal{}\n", terms.len(), plural(terms.len()));
    print_terminals(&terms, ui);
    let id = first_id(&terms);
    next(ui, &[read_hint(&id), status_hint(&id)]);
}

fn print_terminals(terms: &[Value], ui: &Ui) {
    if terms.is_empty() {
        println!("  (no live terminals)");
        return;
    }
    let shown = if ui.all { terms.len() } else { terms.len().min(LIMIT) };
    let slice = &terms[..shown];

    let idw = slice.iter().map(|t| str_field(t, "id").len()).max().unwrap_or(8).max(2);
    let stw = slice.iter().map(|t| str_field(t, "state").len()).max().unwrap_or(4).max(5);
    for t in slice {
        let id = str_field(t, "id");
        let state = str_field(t, "state");
        let title = str_field(t, "title");
        let cwd = str_field(t, "cwd");
        println!("{}", row(ui, &[&id, &state, &title, &cwd], &[idw, stw]));
    }
    if shown < terms.len() {
        println!(
            "  … showing {} of {} — use --all or --json for the rest",
            shown,
            terms.len()
        );
    }
}

fn print_tabs(tabs_v: &[Value], raw: &Value, ui: &Ui) {
    if tabs_v.is_empty() {
        let note = raw
            .get("note")
            .and_then(|n| n.as_str())
            .unwrap_or("no workspace tabs");
        println!("  (none — {note})");
        return;
    }
    let shown = if ui.all { tabs_v.len() } else { tabs_v.len().min(LIMIT) };
    let slice = &tabs_v[..shown];
    let idw = slice.iter().map(|t| str_field(t, "id").len()).max().unwrap_or(8).max(2);
    for t in slice {
        let id = str_field(t, "id");
        let name = str_field(t, "name");
        println!("{}", row(ui, &[&id, &name], &[idw]));
    }
    if shown < tabs_v.len() {
        println!(
            "  … showing {} of {} — use --all or --json for the rest",
            shown,
            tabs_v.len()
        );
    }
}

/// A `(session, status, ctx)` row for the multi-session `th status` view.
pub struct StatusRow {
    pub session: String,
    pub status: String,
    pub ctx: String,
}

/// Multi-session status table (`th status` with no arg). Rows arrive sorted.
pub fn status_table(rows: &[StatusRow], ui: &Ui) {
    println!("{} session{}\n", rows.len(), plural(rows.len()));
    if rows.is_empty() {
        println!("  (no sessions)");
        return;
    }
    let shown = if ui.all { rows.len() } else { rows.len().min(LIMIT) };
    let slice = &rows[..shown];

    let idw = slice.iter().map(|r| r.session.len()).max().unwrap_or(8).max(7);
    let stw = slice.iter().map(|r| r.status.len()).max().unwrap_or(6).max(6);
    println!("{}", row(ui, &["SESSION", "STATUS", "CTX"], &[idw, stw]));
    for r in slice {
        println!("{}", row(ui, &[&r.session, &r.status, &r.ctx], &[idw, stw]));
    }
    if shown < rows.len() {
        println!(
            "  … showing {} of {} — use --all or --json for the rest",
            shown,
            rows.len()
        );
    }

    let id = slice.first().map(|r| r.session.clone());
    next(ui, &[status_hint(&id), read_hint(&id)]);
}

/// Single-session status: `get_status` + `supervision_tree`.
pub fn status_one(session: &str, status: &Value, tree: &Value, ui: &Ui) {
    let st = status.get("status").and_then(|s| s.as_str()).unwrap_or("unknown");
    println!("session   {session}");
    println!("status    {st}");
    match status.get("snapshot") {
        Some(snap) if !snap.is_null() => {
            if let Some(pct) = snap.get("contextUsedPct").and_then(|p| p.as_f64()) {
                println!("context   {pct:.0}%");
            }
            if let Some(rl) = snap.get("rateLimitsPresent").and_then(|r| r.as_bool()) {
                println!("rateLimit {}", if rl { "present" } else { "none" });
            }
        }
        _ => println!("context   (no statusline snapshot)"),
    }

    if tree.is_null() {
        println!("tree      (no supervision tree for this session)");
    } else {
        println!("\nSUPERVISION TREE");
        println!("{}", serde_json::to_string_pretty(tree).unwrap_or_default());
    }

    let id = Some(session.to_string());
    next(
        ui,
        &[
            read_hint(&id),
            (format!("th send {session} <text>"), "type into this session"),
        ],
    );
}

/// `th read` — the captured terminal text.
pub fn read_output(result: &Value, ui: &Ui) {
    let text = result.get("text").and_then(|t| t.as_str()).unwrap_or("");
    let target = result.get("target").and_then(|t| t.as_str()).unwrap_or("");
    let hist = result.get("historyLines").and_then(|h| h.as_i64()).unwrap_or(0);
    let sid = result.get("sessionId").and_then(|s| s.as_str()).unwrap_or("<session>");
    println!("── {target}  (history: {hist} lines) ──");
    println!("{}", text.trim_end_matches('\n'));
    next(
        ui,
        &[
            (format!("th read {sid} --history 200"), "include more scrollback"),
            (format!("th send {sid} <text>"), "respond into this session"),
        ],
    );
}

/// `th tabs`.
pub fn tabs(result: &Value, ui: &Ui) {
    let tabs_v = sort_tabs(result);
    println!("{} tab{}\n", tabs_v.len(), plural(tabs_v.len()));
    print_tabs(&tabs_v, result, ui);
    next(ui, &[("th".to_string(), "fleet home view")]);
}

/// `th health` — the WSL host snapshot.
pub fn health(result: &Value, ui: &Ui) {
    let m = result.get("metrics").cloned().unwrap_or(Value::Null);
    let supervised = result.get("supervisedSessions").and_then(|s| s.as_i64()).unwrap_or(0);
    let f = |k: &str| m.get(k).and_then(|v| v.as_i64()).unwrap_or(0);

    println!("WSL host");
    println!("  cpuCount            {}", f("cpuCount"));
    if let Some(la) = m.get("loadAvg").and_then(|l| l.as_array()) {
        let parts: Vec<String> = la.iter().map(|v| format!("{:.2}", v.as_f64().unwrap_or(0.0))).collect();
        println!("  loadAvg             {}", parts.join(" "));
    }
    println!("  memTotalKib         {}", f("memTotalKib"));
    println!("  memAvailableKib     {}", f("memAvailableKib"));
    println!("  swapTotalKib        {}", f("swapTotalKib"));
    println!("  swapFreeKib         {}", f("swapFreeKib"));
    println!("  processCount        {}", f("processCount"));
    if let Some(d) = m.get("distro").and_then(|d| d.as_str()) {
        println!("  distro              {d}");
    }
    println!("  supervisedSessions  {supervised}");
    next(
        ui,
        &[
            ("th".to_string(), "fleet home view"),
            ("th ls".to_string(), "list live terminals"),
        ],
    );
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}
