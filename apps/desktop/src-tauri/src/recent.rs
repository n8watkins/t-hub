//! Recent Claude sessions for the sidebar "Recent" list (feat/projects-sidebar).
//!
//! The sidebar's lower list is **Recent** — past Claude Code sessions the user
//! can RECALL: clicking one re-spawns a terminal in that session's directory and
//! resumes the conversation with `claude --resume <session-id>`. To populate it
//! we need, per past session: the Claude **session id**, the **cwd** it ran in,
//! a human **label** (Claude's own summary when present, else the project/cwd
//! basename), and a **last-seen** timestamp so the list sorts newest-first.
//!
//! ## Source of truth: the on-disk Claude transcripts
//!
//! Claude Code writes one JSONL transcript per session under
//! `~/.claude/projects/<encoded-project>/<session-id>.jsonl`:
//!   - the **filename** (sans `.jsonl`) IS the session id (the `-r <id>` handle);
//!   - lines of `type:"user"`/`"system"` carry a `"cwd"` field (the project dir);
//!   - a `type:"summary"` line carries Claude's auto-generated `"summary"` title
//!     when one has been produced;
//!   - the file's **mtime** is a faithful "last activity" stamp.
//!
//! These files survive app restarts, WSL restarts, and TermHub never having
//! touched the session — exactly the durable catalog "Recent" wants. We prefer
//! this over the in-memory supervision/agent catalog (which only knows sessions
//! observed live this run) so Recent is useful immediately on a cold launch.
//!
//! ## Crossing the Windows↔WSL boundary
//!
//! On Windows, Claude runs INSIDE WSL, so `~/.claude` is the *distro* home, not a
//! Windows path. Like the rest of the backend (tmux.rs / agent/mod.rs), we cross
//! the boundary with `wsl.exe`: a single `bash -lc` invocation lists + stats the
//! transcripts and prints one TSV row per session, which we parse here. On unix
//! (the dev / `cargo check` build) we read the filesystem directly. Both paths
//! converge on the same [`RecentSession`] list.
//!
//! Everything is best-effort: any failure (no WSL, missing dir, malformed file)
//! degrades to an empty list rather than erroring the UI.

use serde::Serialize;

/// How many recent sessions to return at most. The sidebar shows a scrollable
/// list; a couple dozen is plenty for "recall something I was just doing" without
/// shelling out to read hundreds of stale transcripts.
const RECENT_LIMIT: usize = 40;

/// One recallable past Claude session, mirrored by `src/ipc/recent.ts`
/// (`rename_all = "camelCase"`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentSession {
    /// Claude's session id (the transcript filename stem); the `--resume <id>`
    /// handle the frontend recall path passes back.
    pub id: String,
    /// The working directory the session ran in (a WSL-side path). Recall spawns
    /// the new terminal here so `claude --resume` finds the right project.
    pub cwd: String,
    /// A friendly label: Claude's own summary when known, else the cwd basename.
    pub label: String,
    /// Unix epoch SECONDS of last activity (the transcript mtime). Drives the
    /// newest-first ordering; the frontend may also render it as a relative time.
    pub last_seen: i64,
}

/// Tauri command: list recent recallable Claude sessions, newest first, capped at
/// [`RECENT_LIMIT`]. Best-effort — returns `Ok(vec![])` rather than `Err` when the
/// catalog can't be read, so the sidebar simply shows an empty Recent list.
#[tauri::command]
pub async fn recent_sessions() -> Result<Vec<RecentSession>, String> {
    // Read off the async runtime's blocking-friendly path: the work is filesystem
    // / process IO, so hop to a blocking thread to avoid stalling the executor.
    Ok(tauri::async_runtime::spawn_blocking(collect_recent)
        .await
        .unwrap_or_default())
}

/// Collect + sort the recent sessions (platform-dispatched). Never panics; any
/// error inside the platform reader degrades to an empty list.
fn collect_recent() -> Vec<RecentSession> {
    let mut sessions = read_sessions();
    crate::diag::diag_log(format!(
        "{{\"t\":\"recent\",\"m\":\"collect_recent: {} sessions before cap\"}}",
        sessions.len()
    ));
    // Newest first by last-seen, then cap.
    sessions.sort_by(|a, b| b.last_seen.cmp(&a.last_seen));
    sessions.truncate(RECENT_LIMIT);
    sessions
}

// ---------------------------------------------------------------------------
// Shared parsing — turn one transcript's (id, mtime, raw bytes) into a record.
// Used by BOTH platform readers so the cwd/summary extraction lives in one place.
// ---------------------------------------------------------------------------

/// The last non-empty path segment of `cwd` (POSIX or Windows separators), or the
/// whole string if it has none. Used to label a session by its project directory
/// when Claude has not produced a summary yet.
fn cwd_basename(cwd: &str) -> &str {
    cwd.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(cwd)
}

/// Extract `(cwd, summary)` from a transcript's JSONL text. We scan lines for the
/// FIRST `"cwd"` we can find (every working line carries the same project dir) and
/// the LAST `"summary"` (Claude refines it over the conversation; the latest wins).
/// Either may be absent. Kept tolerant: malformed lines are skipped, not fatal.
fn parse_transcript(text: &str) -> (Option<String>, Option<String>) {
    let mut cwd: Option<String> = None;
    let mut summary: Option<String> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if cwd.is_none() {
            if let Some(c) = v.get("cwd").and_then(|c| c.as_str()) {
                if !c.is_empty() {
                    cwd = Some(c.to_string());
                }
            }
        }
        // A dedicated summary line (type:"summary") carries Claude's title.
        if v.get("type").and_then(|t| t.as_str()) == Some("summary") {
            if let Some(s) = v.get("summary").and_then(|s| s.as_str()) {
                if !s.trim().is_empty() {
                    summary = Some(s.trim().to_string());
                }
            }
        }
    }
    (cwd, summary)
}

/// Build a [`RecentSession`] from a transcript's id + mtime + parsed cwd/summary.
/// Returns `None` when there is no usable cwd (we can't recall a session we don't
/// know the directory for — `claude --resume` would land in the wrong place).
fn make_session(id: String, last_seen: i64, cwd: Option<String>, summary: Option<String>) -> Option<RecentSession> {
    let cwd = cwd?;
    let label = summary.unwrap_or_else(|| cwd_basename(&cwd).to_string());
    Some(RecentSession {
        id,
        cwd,
        label,
        last_seen,
    })
}

// ===========================================================================
// Platform readers.
// ===========================================================================

/// Read the transcript catalog for this platform. Windows shells into WSL (where
/// `~/.claude` actually lives); unix reads the filesystem directly.
fn read_sessions() -> Vec<RecentSession> {
    #[cfg(windows)]
    {
        read_sessions_windows()
    }
    #[cfg(not(windows))]
    {
        read_sessions_unix()
    }
}

// ---------------------------------------------------------------------------
// unix (dev / WSL build): read ~/.claude/projects directly.
// ---------------------------------------------------------------------------

/// unix reader: walk `~/.claude/projects/*/*.jsonl`, stat each for its mtime, and
/// parse its cwd/summary. Self-contained and dependency-free.
/// Read at most `cap` bytes from the START of a file as lossy UTF-8. The recent
/// catalog only needs the early lines (cwd ~line 3; an early summary if present),
/// so we never read whole multi-MB transcripts.
#[cfg(not(windows))]
fn read_prefix(path: &std::path::Path, cap: usize) -> String {
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(path) else {
        return String::new();
    };
    let mut buf = vec![0u8; cap];
    let n = f.read(&mut buf).unwrap_or(0);
    buf.truncate(n);
    String::from_utf8_lossy(&buf).into_owned()
}

#[cfg(not(windows))]
fn read_sessions_unix() -> Vec<RecentSession> {
    use std::time::UNIX_EPOCH;

    let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) else {
        return Vec::new();
    };
    let projects = home.join(".claude").join("projects");
    let Ok(project_dirs) = std::fs::read_dir(&projects) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for project in project_dirs.flatten() {
        let Ok(files) = std::fs::read_dir(project.path()) else {
            continue;
        };
        for entry in files.flatten() {
            let path = entry.path();
            // Only `<session-id>.jsonl` transcripts.
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let Some(id) = path.file_stem().and_then(|s| s.to_str()).map(str::to_string) else {
                continue;
            };
            // mtime (epoch seconds) as the last-seen stamp.
            let last_seen = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            // PERF: only the first 32KB (cwd is ~line 3; an early summary if any).
            // Whole transcripts can be 10MB+; we never need the body.
            let text = read_prefix(&path, 32 * 1024);
            let (cwd, summary) = parse_transcript(&text);
            if let Some(s) = make_session(id, last_seen, cwd, summary) {
                out.push(s);
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Windows: the transcripts live inside the WSL distro, so cross with `wsl.exe`.
// ---------------------------------------------------------------------------

/// Windows reader: ask WSL to enumerate + stat the transcripts and stream their
/// contents back in one shot, then parse the result here. We emit a per-file
/// record framed so each one's id, mtime, and raw JSONL are unambiguous even
/// though the JSONL itself contains newlines.
///
/// Frame format (printed by the `bash -lc` script, one block per transcript):
/// ```text
/// \x1e<session-id>\t<mtime-epoch-secs>\t<byte-length>\x1f<raw jsonl bytes>
/// ```
/// `\x1e` (record separator) starts a block; `\x1f` (unit separator) ends the
/// header; `<byte-length>` lets us slice exactly the file's bytes regardless of
/// embedded newlines. Control chars chosen because they never appear in paths or
/// JSON text. Best-effort: a failed spawn / non-zero exit yields an empty list.
/// The WSL distro to shell into (mirrors files.rs::host_distro so Recent and the
/// file index agree on which distro holds `~/.claude`). Overridable via
/// TERMHUB_DISTRO; defaults to the dev distro.
#[cfg(windows)]
fn host_distro() -> String {
    std::env::var("TERMHUB_DISTRO").unwrap_or_else(|_| "Ubuntu-24.04".to_string())
}

/// Resolve the WSL `$HOME` for `distro` by shelling a login bash once (the proven
/// pattern from claude/install.rs::wsl_home). Returns None on failure/empty so the
/// caller degrades to an empty Recent list. `echo $HOME` is the one thing that
/// reliably resolves the distro home from a Windows GUI spawn.
#[cfg(windows)]
fn wsl_home(distro: &str) -> Option<String> {
    use std::os::windows::process::CommandExt;
    let out = std::process::Command::new("wsl.exe")
        .arg("-d")
        .arg(distro)
        .arg("--")
        .arg("bash")
        .arg("-lc")
        .arg("echo $HOME")
        .creation_flags(0x0800_0000)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let home = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if home.is_empty() {
        None
    } else {
        Some(home)
    }
}

#[cfg(windows)]
fn read_sessions_windows() -> Vec<RecentSession> {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    // List the most-recently-modified transcripts first and cap how many we read
    // so we never slurp hundreds of stale conversations. For each, print the
    // framed header (id, mtime, byte length) followed by the raw file bytes.
    //
    // `stat -c %Y` = mtime epoch secs; `%s` = byte size. `ls -t` orders newest
    // first; `head` caps the count. Quoting keeps paths with spaces intact.
    let limit = RECENT_LIMIT;
    // PERF: read only the first 32KB of each transcript, NOT the whole file. The
    // session cwd is on the first user line (~line 3, under 1KB in) and a summary,
    // when present, sits near the top too. Some transcripts are 10MB+; cat-ing the
    // newest 40 was ~160MB over the wsl.exe pipe and made Recent take many seconds
    // (effectively "not loading"). The prefix is all we need; `size` is the byte
    // count the frame declares so the parser slices exactly what we sent.
    let cap = 32 * 1024;
    // The projects dir is passed as $1 (an ABSOLUTE path resolved in Rust below),
    // NOT derived from $HOME inside the script: `echo $HOME` works, but the empty
    // result we saw in the diag log means relying on $HOME mid-script was fragile,
    // so we mirror files.rs (which passes absolute paths as $1 and works). We also
    // enumerate with `find` instead of a shell glob (`"$dir"/*/*.jsonl`), since a
    // glob that expands to nothing silently yields zero output; `find` is robust.
    // Diagnostics go to STDERR (TH_NODIR / TH_DIR_OK / TH_COUNT) and are logged
    // unconditionally, so a future empty list is self-explaining.
    let script = format!(
        r#"
dir="$1"
if [ ! -d "$dir" ]; then echo "TH_NODIR:$dir HOME=$HOME" >&2; exit 0; fi
echo "TH_DIR_OK:$dir" >&2
count=$(find "$dir" -mindepth 2 -maxdepth 2 -name '*.jsonl' 2>/dev/null | wc -l)
echo "TH_COUNT:$count" >&2
find "$dir" -mindepth 2 -maxdepth 2 -name '*.jsonl' -printf '%T@\t%p\n' 2>/dev/null \
  | sort -rn | head -n {limit} | while IFS=$'\t' read -r mt f; do
  id=$(basename "$f" .jsonl)
  mtime=${{mt%.*}}
  fsize=$(stat -c %s "$f" 2>/dev/null || echo 0)
  size=$fsize; [ "$fsize" -gt {cap} ] && size={cap}
  printf '\036%s\t%s\t%s\037' "$id" "$mtime" "$size"
  head -c "$size" "$f" 2>/dev/null
done
"#
    );

    let mut cmd = Command::new("wsl.exe");
    // Target the distro EXPLICITLY (-d), matching files.rs's working pattern.
    // Resolve the WSL $HOME once (the proven install.rs approach) and pass the
    // absolute projects dir as $1 ($0 is a label, like files.rs's wsl_bash).
    let distro = host_distro();
    let projects_dir = match wsl_home(&distro) {
        Some(home) => format!("{}/.claude/projects", home.trim_end_matches('/')),
        None => {
            crate::diag::diag_log(format!(
                "{{\"t\":\"recent\",\"m\":\"wsl_home FAILED (distro={distro}); cannot locate ~/.claude\"}}"
            ));
            return Vec::new();
        }
    };
    cmd.arg("-d")
        .arg(&distro)
        .arg("--")
        .arg("bash")
        .arg("-lc")
        .arg(&script)
        .arg("termhub")
        .arg(&projects_dir);
    // CREATE_NO_WINDOW: suppress the brief console flash every `wsl.exe` spawn
    // would otherwise show (same flag tmux.rs uses).
    cmd.creation_flags(0x0800_0000);

    // DIAG: this path is best-effort and used to swallow every failure silently,
    // which made an empty Recent list impossible to debug from a release build.
    // Log the spawn result, exit status, and byte counts so the file diag log
    // shows exactly why Recent is empty (no wsl / non-zero exit / zero stdout).
    let output = match cmd.output() {
        Ok(o) => o,
        Err(e) => {
            crate::diag::diag_log(format!(
                "{{\"t\":\"recent\",\"m\":\"wsl.exe spawn FAILED (distro={distro}): {e}\"}}"
            ));
            return Vec::new();
        }
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        crate::diag::diag_log(format!(
            "{{\"t\":\"recent\",\"m\":\"wsl.exe exit {:?} (distro={distro}); stderr={}\"}}",
            output.status.code(),
            stderr.trim().replace('"', "'").chars().take(300).collect::<String>()
        ));
        return Vec::new();
    }
    // Always surface the script's stderr diagnostics (TH_DIR_OK / TH_NODIR /
    // TH_COUNT) so a zero result is explained even on a "success" exit.
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.trim().is_empty() {
        crate::diag::diag_log(format!(
            "{{\"t\":\"recent\",\"m\":\"script stderr: {}\"}}",
            stderr.trim().replace('"', "'").chars().take(300).collect::<String>()
        ));
    }
    let sessions = parse_framed(&output.stdout);
    crate::diag::diag_log(format!(
        "{{\"t\":\"recent\",\"m\":\"wsl.exe OK (distro={distro}): {} stdout bytes -> {} sessions parsed\"}}",
        output.stdout.len(),
        sessions.len()
    ));
    sessions
}

/// Parse the framed `\x1e id \t mtime \t len \x1f <bytes>` stream the Windows WSL
/// script prints into [`RecentSession`]s. Lives only on Windows (the unix reader
/// has no frame), kept here next to its producer.
#[cfg(windows)]
fn parse_framed(bytes: &[u8]) -> Vec<RecentSession> {
    const RS: u8 = 0x1e; // record separator: starts a per-file block
    const US: u8 = 0x1f; // unit separator: ends the header

    let mut out = Vec::new();
    // Split on the record separator; the first chunk before any RS is preamble.
    for block in bytes.split(|&b| b == RS).skip(1) {
        // Header (id \t mtime \t len) up to the unit separator, then the bytes.
        let Some(us_pos) = block.iter().position(|&b| b == US) else {
            continue;
        };
        let header = String::from_utf8_lossy(&block[..us_pos]);
        let mut parts = header.split('\t');
        let (Some(id), Some(mtime), Some(len)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let id = id.trim().to_string();
        if id.is_empty() {
            continue;
        }
        let last_seen = mtime.trim().parse::<i64>().unwrap_or(0);
        let len = len.trim().parse::<usize>().unwrap_or(0);
        let body = &block[us_pos + 1..];
        // Slice exactly the file's bytes (guards against any trailing framing).
        let body = if len <= body.len() { &body[..len] } else { body };
        let text = String::from_utf8_lossy(body);
        let (cwd, summary) = parse_transcript(&text);
        if let Some(s) = make_session(id, last_seen, cwd, summary) {
            out.push(s);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cwd_basename_handles_separators_and_trailing_slash() {
        assert_eq!(cwd_basename("/home/natkins/n8builds/tools"), "tools");
        assert_eq!(cwd_basename("/home/natkins/n8builds/tools/"), "tools");
        assert_eq!(cwd_basename("C:\\Users\\natha\\proj"), "proj");
        assert_eq!(cwd_basename("solo"), "solo");
    }

    #[test]
    fn parse_transcript_extracts_first_cwd_and_last_summary() {
        let text = r#"
{"type":"mode","sessionId":"s1"}
{"type":"user","cwd":"/home/u/proj","sessionId":"s1"}
{"type":"summary","summary":"early title"}
{"type":"user","cwd":"/home/u/proj"}
{"type":"summary","summary":"final title"}
"#;
        let (cwd, summary) = parse_transcript(text);
        assert_eq!(cwd.as_deref(), Some("/home/u/proj"));
        assert_eq!(summary.as_deref(), Some("final title"));
    }

    #[test]
    fn parse_transcript_tolerates_garbage_lines() {
        let text = "not json\n{\"cwd\":\"/x\"}\nalso bad";
        let (cwd, summary) = parse_transcript(text);
        assert_eq!(cwd.as_deref(), Some("/x"));
        assert_eq!(summary, None);
    }

    #[test]
    fn make_session_requires_a_cwd() {
        // No cwd → unrecallable → dropped.
        assert!(make_session("id".into(), 1, None, None).is_none());
        // With a cwd but no summary, the label falls back to the cwd basename.
        let s = make_session("id".into(), 5, Some("/home/u/proj".into()), None).unwrap();
        assert_eq!(s.label, "proj");
        assert_eq!(s.last_seen, 5);
        // A summary, when present, wins as the label.
        let s2 = make_session("id".into(), 5, Some("/home/u/proj".into()), Some("Do a thing".into())).unwrap();
        assert_eq!(s2.label, "Do a thing");
    }

    #[cfg(windows)]
    #[test]
    fn parse_framed_decodes_blocks_with_embedded_newlines() {
        // Two framed blocks; the JSONL bodies contain newlines that must NOT be
        // mistaken for record boundaries (we slice by the declared byte length).
        let body1 = "{\"cwd\":\"/a\"}\n{\"type\":\"summary\",\"summary\":\"A\"}\n";
        let body2 = "{\"cwd\":\"/b\"}\n";
        let mut buf = Vec::new();
        buf.extend_from_slice(b"preamble noise");
        buf.push(0x1e);
        buf.extend_from_slice(format!("id1\t100\t{}", body1.len()).as_bytes());
        buf.push(0x1f);
        buf.extend_from_slice(body1.as_bytes());
        buf.push(0x1e);
        buf.extend_from_slice(format!("id2\t200\t{}", body2.len()).as_bytes());
        buf.push(0x1f);
        buf.extend_from_slice(body2.as_bytes());

        let sessions = parse_framed(&buf);
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].id, "id1");
        assert_eq!(sessions[0].cwd, "/a");
        assert_eq!(sessions[0].label, "A");
        assert_eq!(sessions[0].last_seen, 100);
        assert_eq!(sessions[1].id, "id2");
        assert_eq!(sessions[1].cwd, "/b");
        assert_eq!(sessions[1].label, "b"); // basename fallback
    }
}
