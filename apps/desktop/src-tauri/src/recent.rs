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
//
// Both platforms now use the SAME std::fs core ([`read_sessions_from_dir`]); the
// only difference is the ROOT path. unix points at `$HOME/.claude/projects`;
// Windows resolves the WSL `$HOME` once (via `wsl.exe -- bash -lc 'echo $HOME'`,
// the one thing that reliably works) and reads the transcripts directly over the
// `\\wsl.localhost\<distro>\...` UNC share. We deliberately do NOT pass a complex
// multi-line script to `wsl.exe`: the diag log showed `wsl.exe` mangling a
// trailing path argument (it arrived empty, `$1`-based reads found nothing), so
// reading the share with std::fs sidesteps that entire class of arg-quoting bug.
// We only read ~40 small (32KB) prefixes, so the slower UNC bridge is fine here.
// ===========================================================================

/// Read at most `cap` bytes from the START of a file as lossy UTF-8. The recent
/// catalog only needs the early lines (cwd ~line 3; an early summary if present),
/// so we never read whole multi-MB transcripts. Platform-agnostic.
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

/// Read every `~/.claude/projects/<project>/<id>.jsonl` transcript under
/// `projects` into [`RecentSession`]s. Shared by both platforms (only the root
/// path differs); uses plain `std::fs` so it works identically over a Linux FS
/// and over the Windows `\\wsl.localhost\` UNC share. Best-effort: an unreadable
/// dir/entry is skipped, never fatal. PERF: only the first 32KB of each file is
/// read (the cwd/summary live near the top; bodies can be 10MB+).
fn read_sessions_from_dir(projects: &std::path::Path) -> Vec<RecentSession> {
    use std::time::UNIX_EPOCH;

    let Ok(project_dirs) = std::fs::read_dir(projects) else {
        crate::diag::diag_log(format!(
            "{{\"t\":\"recent\",\"m\":\"read_dir FAILED: {}\"}}",
            projects.display().to_string().replace('"', "'")
        ));
        return Vec::new();
    };

    let mut out = Vec::new();
    for project in project_dirs.flatten() {
        let Ok(files) = std::fs::read_dir(project.path()) else {
            continue;
        };
        for entry in files.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let Some(id) = path.file_stem().and_then(|s| s.to_str()).map(str::to_string) else {
                continue;
            };
            let last_seen = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let text = read_prefix(&path, 32 * 1024);
            let (cwd, summary) = parse_transcript(&text);
            if let Some(s) = make_session(id, last_seen, cwd, summary) {
                out.push(s);
            }
        }
    }
    crate::diag::diag_log(format!(
        "{{\"t\":\"recent\",\"m\":\"read_sessions_from_dir({}) -> {} sessions\"}}",
        projects.display().to_string().replace('"', "'"),
        out.len()
    ));
    out
}

/// Read the transcript catalog for this platform. unix reads
/// `$HOME/.claude/projects` directly; Windows resolves the WSL home and reads the
/// same dir over the UNC share.
fn read_sessions() -> Vec<RecentSession> {
    #[cfg(windows)]
    {
        read_sessions_windows()
    }
    #[cfg(not(windows))]
    {
        let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) else {
            return Vec::new();
        };
        read_sessions_from_dir(&home.join(".claude").join("projects"))
    }
}

// ---------------------------------------------------------------------------
// Windows: the transcripts live inside the WSL distro. Resolve the WSL $HOME via
// wsl.exe once, then read the catalog over the `\\wsl.localhost\` UNC share.
// ---------------------------------------------------------------------------

/// The WSL distro to read from (mirrors files.rs::host_distro so Recent and the
/// file index agree). Overridable via TERMHUB_DISTRO; defaults to the dev distro.
#[cfg(windows)]
fn host_distro() -> String {
    std::env::var("TERMHUB_DISTRO").unwrap_or_else(|_| "Ubuntu-24.04".to_string())
}

/// Resolve the WSL `$HOME` for `distro` by shelling a login bash once (the proven
/// pattern from claude/install.rs::wsl_home). `echo $HOME` is a SINGLE simple arg,
/// so it doesn't trip the wsl.exe multi-arg mangling that broke the old reader.
/// Returns None on failure/empty so the caller degrades to an empty list.
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

/// Map a WSL POSIX home (`/home/natkins`) onto the Windows UNC share path for the
/// projects dir: `\\wsl.localhost\<distro>\home\natkins\.claude\projects`. Same
/// mapping files.rs::to_host_path uses for its std::fs fallback.
#[cfg(windows)]
fn projects_unc(distro: &str, wsl_home: &str) -> std::path::PathBuf {
    let home_rel = wsl_home.trim_start_matches('/').replace('/', "\\");
    std::path::PathBuf::from(format!(
        "\\\\wsl.localhost\\{distro}\\{home_rel}\\.claude\\projects"
    ))
}

#[cfg(windows)]
fn read_sessions_windows() -> Vec<RecentSession> {
    let distro = host_distro();
    let Some(home) = wsl_home(&distro) else {
        crate::diag::diag_log(format!(
            "{{\"t\":\"recent\",\"m\":\"wsl_home FAILED (distro={distro}); cannot locate ~/.claude\"}}"
        ));
        return Vec::new();
    };
    let projects = projects_unc(&distro, &home);
    crate::diag::diag_log(format!(
        "{{\"t\":\"recent\",\"m\":\"windows reader: home={home} -> {}\"}}",
        projects.display().to_string().replace('"', "'")
    ));
    read_sessions_from_dir(&projects)
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

    #[test]
    fn read_sessions_from_dir_reads_transcripts_and_prefixes() {
        // Build a fake ~/.claude/projects with two project dirs holding transcripts,
        // then assert the shared std::fs reader extracts id/cwd/label and ignores
        // non-jsonl files. This is the SAME code path Windows uses over UNC.
        use std::io::Write;
        let tmp = std::env::temp_dir().join(format!("th_recent_test_{}", std::process::id()));
        let proj_a = tmp.join("proj-a");
        let proj_b = tmp.join("proj-b");
        std::fs::create_dir_all(&proj_a).unwrap();
        std::fs::create_dir_all(&proj_b).unwrap();

        let mut f = std::fs::File::create(proj_a.join("aaaa.jsonl")).unwrap();
        writeln!(f, "{{\"type\":\"mode\"}}").unwrap();
        writeln!(f, "{{\"type\":\"user\",\"cwd\":\"/home/u/alpha\"}}").unwrap();
        writeln!(f, "{{\"type\":\"summary\",\"summary\":\"Alpha work\"}}").unwrap();

        let mut g = std::fs::File::create(proj_b.join("bbbb.jsonl")).unwrap();
        writeln!(g, "{{\"type\":\"user\",\"cwd\":\"/home/u/beta\"}}").unwrap();
        // A non-jsonl file that must be ignored.
        std::fs::File::create(proj_b.join("notes.txt")).unwrap();

        let mut got = read_sessions_from_dir(&tmp);
        got.sort_by(|a, b| a.id.cmp(&b.id));
        std::fs::remove_dir_all(&tmp).ok();

        assert_eq!(got.len(), 2, "two transcripts, txt ignored");
        assert_eq!(got[0].id, "aaaa");
        assert_eq!(got[0].cwd, "/home/u/alpha");
        assert_eq!(got[0].label, "Alpha work"); // summary wins
        assert_eq!(got[1].id, "bbbb");
        assert_eq!(got[1].cwd, "/home/u/beta");
        assert_eq!(got[1].label, "beta"); // basename fallback
    }

    #[test]
    fn read_prefix_caps_at_n_bytes() {
        use std::io::Write;
        let p = std::env::temp_dir().join(format!("th_prefix_{}.txt", std::process::id()));
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(&vec![b'x'; 100_000]).unwrap();
        let s = read_prefix(&p, 1024);
        std::fs::remove_file(&p).ok();
        assert_eq!(s.len(), 1024);
    }
}
