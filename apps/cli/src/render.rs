//! Human-facing rendering (AXI design language): terse, structured, aligned
//! columns, and a "Next" footer of suggested follow-up commands after every
//! view. Machine mode (`--json`) bypasses all of this and prints raw result
//! JSON — these helpers are the human half only.

use serde_json::Value;

/// Print a `Next` footer of suggested follow-up commands. Every command view
/// ends with one so the operator always has an obvious next move (AXI feel).
pub fn next(hints: &[(&str, &str)]) {
    if hints.is_empty() {
        return;
    }
    let width = hints.iter().map(|(c, _)| c.len()).max().unwrap_or(0);
    println!("\nNext");
    for (cmd, desc) in hints {
        println!("  {:<width$}  {}", cmd, desc, width = width);
    }
}

/// A terminal row as returned by `list_terminals`.
struct Terminal {
    id: String,
    state: String,
    title: String,
    cwd: String,
}

fn terminals_of(result: &Value) -> Vec<Terminal> {
    result
        .get("terminals")
        .and_then(|t| t.as_array())
        .map(|arr| {
            arr.iter()
                .map(|t| Terminal {
                    id: str_field(t, "id"),
                    state: str_field(t, "state"),
                    title: str_field(t, "title"),
                    cwd: str_field(t, "cwd"),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn str_field(v: &Value, key: &str) -> String {
    v.get(key).and_then(|x| x.as_str()).unwrap_or("").to_string()
}

/// The fleet home view: live terminals + tabs + a next-commands footer.
pub fn home(terminals: &Value, tabs: &Value) {
    let terms = terminals_of(terminals);
    let tab_count = tabs
        .get("tabs")
        .and_then(|t| t.as_array())
        .map(|a| a.len())
        .unwrap_or(0);

    println!(
        "T-Hub  ·  {} live terminal{}  ·  {} tab{}",
        terms.len(),
        plural(terms.len()),
        tab_count,
        plural(tab_count),
    );

    println!("\nTERMINALS");
    print_terminals(&terms);

    println!("\nTABS");
    if tab_count == 0 {
        let note = tabs
            .get("note")
            .and_then(|n| n.as_str())
            .unwrap_or("no workspace tabs");
        println!("  (none — {note})");
    } else if let Some(arr) = tabs.get("tabs").and_then(|t| t.as_array()) {
        for tab in arr {
            println!(
                "  {:<10}  {}",
                str_field(tab, "id"),
                str_field(tab, "name")
            );
        }
    }

    next(&[
        ("th read <session>", "view a terminal's recent output"),
        ("th status", "FR-012 status across all sessions"),
        ("th send <session> <text>", "type into a session"),
        ("th health", "WSL host snapshot"),
        ("th tabs", "list workspace tabs"),
    ]);
}

/// `th ls` — just the terminals table + footer.
pub fn terminals(result: &Value) {
    let terms = terminals_of(result);
    println!("{} live terminal{}", terms.len(), plural(terms.len()));
    println!();
    print_terminals(&terms);
    next(&[
        ("th read <session>", "view a terminal's recent output"),
        ("th status <session>", "status + supervision tree for one session"),
    ]);
}

fn print_terminals(terms: &[Terminal]) {
    if terms.is_empty() {
        println!("  (no live terminals)");
        return;
    }
    let idw = terms.iter().map(|t| t.id.len()).max().unwrap_or(8).max(2);
    let stw = terms.iter().map(|t| t.state.len()).max().unwrap_or(4).max(5);
    for t in terms {
        let mut line = format!(
            "  {:<idw$}  {:<stw$}  {}",
            t.id,
            t.state,
            t.title,
            idw = idw,
            stw = stw
        );
        if !t.cwd.is_empty() {
            line.push_str(&format!("  {}", t.cwd));
        }
        println!("{line}");
    }
}

/// A `(session, status, ctx)` row for the multi-session `th status` view.
pub struct StatusRow {
    pub session: String,
    pub status: String,
    pub ctx: String,
}

/// Multi-session status table (`th status` with no arg).
pub fn status_table(rows: &[StatusRow]) {
    println!("{} session{}", rows.len(), plural(rows.len()));
    println!();
    if rows.is_empty() {
        println!("  (no sessions)");
        return;
    }
    let idw = rows.iter().map(|r| r.session.len()).max().unwrap_or(8).max(7);
    let stw = rows.iter().map(|r| r.status.len()).max().unwrap_or(6).max(6);
    println!(
        "  {:<idw$}  {:<stw$}  {}",
        "SESSION",
        "STATUS",
        "CTX",
        idw = idw,
        stw = stw
    );
    for r in rows {
        println!(
            "  {:<idw$}  {:<stw$}  {}",
            r.session,
            r.status,
            r.ctx,
            idw = idw,
            stw = stw
        );
    }
    next(&[
        ("th status <session>", "drill into one session + its supervision tree"),
        ("th read <session>", "view a session's recent output"),
    ]);
}

/// Single-session status: `get_status` + `supervision_tree`.
pub fn status_one(session: &str, status: &Value, tree: &Value) {
    let st = status
        .get("status")
        .and_then(|s| s.as_str())
        .unwrap_or("unknown");
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

    next(&[
        ("th read <session>", "view this session's recent output"),
        ("th send <session> <text>", "type into this session"),
    ]);
}

/// `th read` — the captured terminal text.
pub fn read_output(result: &Value) {
    let text = result.get("text").and_then(|t| t.as_str()).unwrap_or("");
    let target = result.get("target").and_then(|t| t.as_str()).unwrap_or("");
    let hist = result
        .get("historyLines")
        .and_then(|h| h.as_i64())
        .unwrap_or(0);
    println!("── {target}  (history: {hist} lines) ──");
    println!("{}", text.trim_end_matches('\n'));
    let sid = result
        .get("sessionId")
        .and_then(|s| s.as_str())
        .unwrap_or("<session>");
    next(&[
        (
            "th read <session> --history 200",
            "include more scrollback",
        ),
        ("th send <session> <text>", "respond into this session"),
    ]);
    let _ = sid;
}

/// `th tabs`.
pub fn tabs(result: &Value) {
    let arr = result.get("tabs").and_then(|t| t.as_array());
    match arr {
        Some(a) if !a.is_empty() => {
            println!("{} tab{}", a.len(), plural(a.len()));
            println!();
            for tab in a {
                println!(
                    "  {:<10}  {}",
                    str_field(tab, "id"),
                    str_field(tab, "name")
                );
            }
        }
        _ => {
            let note = result
                .get("note")
                .and_then(|n| n.as_str())
                .unwrap_or("no workspace tabs");
            println!("0 tabs");
            println!("  (none — {note})");
        }
    }
    next(&[("th", "fleet home view")]);
}

/// `th health` — the WSL host snapshot.
pub fn health(result: &Value) {
    let m = result.get("metrics").cloned().unwrap_or(Value::Null);
    let supervised = result
        .get("supervisedSessions")
        .and_then(|s| s.as_i64())
        .unwrap_or(0);
    let f = |k: &str| m.get(k).and_then(|v| v.as_i64()).unwrap_or(0);

    println!("WSL host");
    println!("  cpuCount        {}", f("cpuCount"));
    if let Some(la) = m.get("loadAvg").and_then(|l| l.as_array()) {
        let parts: Vec<String> = la.iter().map(|v| format!("{:.2}", v.as_f64().unwrap_or(0.0))).collect();
        println!("  loadAvg         {}", parts.join(" "));
    }
    println!("  memTotalKib     {}", f("memTotalKib"));
    println!("  memAvailKib     {}", f("memAvailableKib"));
    println!("  swapTotalKib    {}", f("swapTotalKib"));
    println!("  swapFreeKib     {}", f("swapFreeKib"));
    println!("  processCount    {}", f("processCount"));
    if let Some(d) = m.get("distro").and_then(|d| d.as_str()) {
        println!("  distro          {d}");
    }
    println!("  supervisedSessions {supervised}");
    next(&[("th", "fleet home view"), ("th ls", "list live terminals")]);
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}
