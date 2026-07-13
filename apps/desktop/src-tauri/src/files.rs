//! File index, fuzzy search, shallow directory listing, and a capped text
//! reader for the Files panel (PRD §6.8 reading/editing, §9.7 file indexing;
//! FR-014/015/016/017).
//!
//! Scope for this module (the V1 nucleus of the Files feature):
//!   - Walk a project root and build a compact **in-memory** index of relative
//!     paths / basenames / extensions, honoring `.gitignore` and skipping
//!     `.git`, dependency/build dirs, and binary blobs (PRD §9.7: "Index names
//!     and metadata, not file contents").
//!   - Fuzzy basename/path/extension ranking over that index.
//!   - A shallow `list_dir` for the tree (folder expansion is UI state, not a
//!     rescan — PRD §9.7).
//!   - A size-capped `read_text_file` for the reader.
//!
//! Deliberately **out of scope** here (later workstreams, noted in the report):
//!   - SQLite persistence + startup hydration + inotify incremental updates
//!     (PRD §9.7 / FR-014). This index is rebuilt on demand and cached per root
//!     in memory only.
//!   - Editing / atomic-save / external-change detection (PRD §6.8.5 / FR-017).
//!   - Routing through the WSL agent for native Linux paths (FR-014). In this
//!     WSL dev environment a native path already *is* the Linux path, so we
//!     index the path we are given directly.
//!
//! Boundaries: this file owns its own state ([`FileIndexState`], registered in
//! `lib.rs` as Tauri-managed state). It is self-contained and shares nothing
//! with the agent/supervision/status modules.

// Some index fields (e.g. `is_key_file`) and helpers are surfaced over IPC and
// exercised by tests but may not all be read from within the crate yet.
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ignore::WalkBuilder;
use parking_lot::Mutex;
use serde::Serialize;

/// Maximum number of bytes [`read_text_file`] will return. Larger files are
/// rejected so the reader never tries to render a multi-megabyte blob. ~2 MiB
/// comfortably covers source files and typical Markdown docs.
const MAX_READ_BYTES: u64 = 2 * 1024 * 1024;

/// How many bytes of a file we sniff to decide "is this text or a binary blob".
const SNIFF_BYTES: usize = 8 * 1024;

/// Directory names that are always pruned during indexing regardless of
/// `.gitignore` (PRD §9.7: ignore `.git`, dependency/build directories). These
/// are skipped *in addition to* whatever `.gitignore` excludes.
const PRUNED_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target", // Rust build output
    "dist",
    "build",
    "out",
    ".next",
    ".svelte-kit",
    ".turbo",
    ".cache",
    ".venv",
    "venv",
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    "vendor",
    ".gradle",
    ".idea",
    ".vscode",
];

/// Basenames considered "key files" — the high-signal project entry points the
/// UI's Key Files view leans on (FR-016). Matched case-insensitively, and any
/// `readme*`/`changelog*`/`license*` variant also counts (handled in code).
const KEY_FILE_NAMES: &[&str] = &[
    "package.json",
    "cargo.toml",
    "pyproject.toml",
    "go.mod",
    "tsconfig.json",
    "tauri.conf.json",
    "dockerfile",
    "makefile",
    ".gitignore",
    ".env",
    ".env.example",
];

/// One indexed file. Compact on purpose: names + metadata, never contents.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileEntry {
    /// Path relative to the indexed root, using `/` separators.
    pub rel_path: String,
    /// Final path component (e.g. `lib.rs`).
    pub basename: String,
    /// Lowercased extension without the dot (e.g. `rs`), or `""` if none.
    pub ext: String,
    /// True for high-signal project files (see [`KEY_FILE_NAMES`]).
    pub is_key_file: bool,
}

impl FileEntry {
    fn from_rel(rel_path: String) -> Self {
        let basename = rel_path.rsplit('/').next().unwrap_or(&rel_path).to_string();
        let ext = Path::new(&basename)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .unwrap_or_default();
        let is_key_file = is_key_file(&basename);
        Self {
            rel_path,
            basename,
            ext,
            is_key_file,
        }
    }
}

/// Whether a basename is a "key file" (case-insensitive; covers README/LICENSE/
/// CHANGELOG variants by prefix).
fn is_key_file(basename: &str) -> bool {
    let lower = basename.to_ascii_lowercase();
    if KEY_FILE_NAMES.contains(&lower.as_str()) {
        return true;
    }
    lower.starts_with("readme")
        || lower.starts_with("changelog")
        || lower.starts_with("license")
        || lower.starts_with("licence")
}

/// The compact in-memory index for one project root.
#[derive(Debug, Clone)]
pub struct ProjectIndex {
    /// Absolute, normalized root the entries are relative to.
    pub root: PathBuf,
    pub entries: Vec<FileEntry>,
}

/// What `index_project` returns to the UI — a summary, not the whole index
/// (the index can be large; the UI searches it via `search_files`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexSummary {
    /// The root that was indexed (normalized, absolute when possible).
    pub root: String,
    /// Number of files in the index.
    pub count: usize,
}

/// A ranked search result.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileHit {
    pub rel_path: String,
    pub basename: String,
    pub ext: String,
    pub is_key_file: bool,
    /// Higher is a better match. Opaque to the UI beyond ordering.
    pub score: i64,
}

/// One shallow directory entry for the tree (`list_dir`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirEntry {
    /// Final path component.
    pub name: String,
    /// Absolute path to this entry (so the UI can drill in / open directly).
    pub path: String,
    pub is_dir: bool,
    /// File size in bytes (0 for directories).
    pub size: u64,
}

/// The capped result of reading a text file for the reader.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileContents {
    pub path: String,
    /// Lowercased extension without the dot (drives Markdown-vs-plain rendering).
    pub ext: String,
    /// The decoded UTF-8 text (lossy for stray non-UTF-8 bytes).
    pub text: String,
    /// True if the file was longer than [`MAX_READ_BYTES`] and `text` is a prefix.
    pub truncated: bool,
    /// Total size of the file on disk, in bytes.
    pub size: u64,
}

/// Tauri-managed state: a small cache of `root -> index` so repeated searches
/// after one `index_project` don't re-walk the tree. Cleared implicitly by
/// re-indexing the same root.
#[derive(Default)]
pub struct FileIndexState {
    indexes: Mutex<HashMap<PathBuf, Arc<ProjectIndex>>>,
}

impl FileIndexState {
    pub fn new() -> Self {
        Self::default()
    }

    fn get(&self, root: &Path) -> Option<Arc<ProjectIndex>> {
        self.indexes.lock().get(root).cloned()
    }

    fn put(&self, index: ProjectIndex) -> Arc<ProjectIndex> {
        let arc = Arc::new(index);
        self.indexes.lock().insert(arc.root.clone(), arc.clone());
        arc
    }
}

/// Normalize a user-supplied path to an absolute, lexically-clean form. We
/// first route it through [`to_host_path`] (a no-op on unix; WSL→UNC on Windows)
/// so a native Linux project path is reachable from the Windows-side process,
/// then canonicalize when the path exists (resolves symlinks/`..`); otherwise we
/// fall back to the host path as given so error messages stay meaningful.
fn normalize(path: &str) -> PathBuf {
    let host = to_host_path(path);
    // On Windows, do NOT canonicalize a WSL UNC path. `std::fs::canonicalize`
    // rewrites `\\wsl.localhost\<distro>\...` into the verbatim extended form
    // `\\?\UNC\wsl.localhost\<distro>\...`, which the fast-path detector
    // (`unc_to_posix`) doesn't recognize — silently forcing list_dir/index back
    // onto the slow `std::fs` UNC read. Keep the clean `\\wsl.localhost\` form so
    // the native-WSL fast path stays in play. (canonicalize only resolved
    // symlinks/`..`, which the WSL-side `find`/`rg` handle themselves.)
    #[cfg(windows)]
    {
        let s = host.to_string_lossy();
        if s.starts_with("\\\\wsl.localhost\\") || s.starts_with("\\\\wsl$\\") {
            return host;
        }
    }
    std::fs::canonicalize(&host).unwrap_or(host)
}

/// The WSL distro projects live in, as seen from the Windows host. Mirrors the
/// agent bridge's `default_distro` (lib.rs): overridable via `T_HUB_DISTRO`,
/// defaulting to the dev distro. Only consulted on Windows.
///
/// Canonical definition for the crate: `git.rs`, `recent.rs`, and `devserver.rs`
/// all call `crate::files::host_distro()` rather than re-declaring this (one
/// source of truth for the distro default + `T_HUB_DISTRO` override).
#[cfg(windows)]
pub(crate) fn host_distro() -> String {
    std::env::var("T_HUB_DISTRO").unwrap_or_else(|_| "Ubuntu-24.04".to_string())
}

/// Translate a path so the *Windows-side* file commands can reach a project that
/// physically lives on WSL's ext4 filesystem.
///
/// The PTY/agent layers pass native WSL paths (`/home/natkins/...`) straight to
/// `wsl.exe`, which resolves them inside the distro. These file commands instead
/// use `std::fs` directly on the Windows host, where a bare POSIX absolute path
/// is meaningless. WSL exposes every distro's root over the
/// `\\wsl.localhost\<distro>\` UNC share, so we rewrite a leading-`/` absolute
/// POSIX path into that UNC form (with `/`→`\`). Paths that are already
/// Windows-shaped (`C:\...`, `\\wsl.localhost\...`, UNC) or relative are passed
/// through untouched.
///
/// On unix this is the identity function: a native path already *is* the Linux
/// path, so the project is indexed directly (see the module-level scope note).
fn to_host_path(path: &str) -> PathBuf {
    #[cfg(windows)]
    {
        // Already a Windows/UNC path (drive-letter, `\\server\...`, or a
        // `\\wsl.localhost\...` / `\\wsl$\...` share) — leave it alone.
        let is_windows_shaped =
            path.starts_with("\\\\") || path.as_bytes().get(1).map(|&b| b == b':').unwrap_or(false);
        // A POSIX-absolute path ("/home/...") that is NOT already a forward-slash
        // UNC ("//wsl.localhost/...") is a WSL path we must map onto the share.
        let is_posix_abs = path.starts_with('/') && !path.starts_with("//");
        if !is_windows_shaped && is_posix_abs {
            let distro = host_distro();
            // `\\wsl.localhost\<distro>` + the POSIX path with `/`→`\`.
            let tail = path.replace('/', "\\");
            let unc = format!("\\\\wsl.localhost\\{distro}{tail}");
            return PathBuf::from(unc);
        }
        PathBuf::from(path)
    }
    #[cfg(unix)]
    {
        PathBuf::from(path)
    }
}

// ---------------------------------------------------------------------------
// Windows: native-in-WSL fast paths (avoid the slow `\\wsl.localhost\` UNC/9P
// bridge for directory listing + indexing by shelling into the distro itself).
// ---------------------------------------------------------------------------

/// Recover a POSIX/WSL path from a path that [`to_host_path`] mapped onto the
/// `\\wsl.localhost\<distro>\...` (or legacy `\\wsl$\<distro>\...`) UNC share,
/// or that is already a bare POSIX path. Returns `None` for genuine Windows
/// paths (a `C:\...` drive path), which must keep using `std::fs`.
///
/// e.g. `\\wsl.localhost\Ubuntu-24.04\home\natkins\proj` → `/home/natkins/proj`.
#[cfg(windows)]
fn unc_to_posix(path: &Path) -> Option<String> {
    let s = path.to_string_lossy();
    // Already a bare POSIX path (shouldn't usually reach here post-normalize,
    // but be lenient): pass through.
    if s.starts_with('/') {
        return Some(s.into_owned());
    }
    // Peel a verbatim extended-length prefix first (`std::fs::canonicalize`
    // emits these): `\\?\UNC\wsl.localhost\...` -> `\\wsl.localhost\...`, and a
    // plain `\\?\C:\...` -> `C:\...` (which then won't match the WSL prefixes
    // below and correctly returns None for a real drive path).
    let s: std::borrow::Cow<str> = if let Some(rest) = s.strip_prefix("\\\\?\\UNC\\") {
        std::borrow::Cow::Owned(format!("\\\\{rest}"))
    } else if let Some(rest) = s.strip_prefix("\\\\?\\") {
        std::borrow::Cow::Owned(rest.to_string())
    } else {
        s
    };
    // Strip a `\\wsl.localhost\<distro>` or `\\wsl$\<distro>` prefix.
    for prefix in ["\\\\wsl.localhost\\", "\\\\wsl$\\"] {
        if let Some(rest) = s.strip_prefix(prefix) {
            // `rest` is `<distro>\home\natkins\...`; drop the distro segment.
            let tail = match rest.split_once('\\') {
                Some((_distro, tail)) => tail,
                // `\\wsl.localhost\<distro>` with no trailing path → distro root.
                None => "",
            };
            let posix = format!("/{}", tail.replace('\\', "/"));
            return Some(posix);
        }
    }
    None
}

/// Build a `wsl.exe -d <distro> --cd <cwd> -- bash -lc '<script>'` command with
/// the console window suppressed (`CREATE_NO_WINDOW`). The target dir is passed
/// via wsl.exe's OWN `--cd` flag, NOT as a trailing `bash -lc` argv word: wsl.exe
/// MANGLES a trailing arg when combined with a `-lc` script (proven in recent.rs
/// — the arg arrived EMPTY in the GUI-spawned process, so `cd "$1"` ran in an
/// empty dir and listing/indexing silently returned nothing → "Files not loading
/// at all"). With `--cd`, the shell already starts in `cwd`, so the scripts just
/// operate on `.` and never touch `$1`. (claude/install.rs + git.rs use the same
/// `--cd` form for the same reason.)
#[cfg(windows)]
fn wsl_bash(distro: &str, script: &str, cwd: &str) -> std::process::Command {
    use std::os::windows::process::CommandExt;
    use std::process::Command;
    let mut c = Command::new("wsl.exe");
    // `-e` (exec) runs bash DIRECTLY. A bare `--` makes wsl.exe route the command
    // through the user's default login shell (e.g. zsh) instead of bash, which both
    // changes shell semantics and mangles scripts — see tmux.rs `pane_info_command`.
    c.arg("-d")
        .arg(distro)
        .arg("--cd")
        .arg(cwd)
        .arg("-e")
        .arg("bash")
        .arg("-lc")
        .arg(script);
    c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW (see tmux.rs)
    c
}

/// Shallow directory listing performed natively inside WSL. Prints one line per
/// entry as `name\t<d|f>` and parses it into [`DirEntry`]s (dirs first, then
/// files, alphabetical — matching [`read_dir_shallow_fs`]). The absolute `path`
/// of each entry is rebuilt as a POSIX path so the UI can drill in / open it.
///
/// **Directory-only gitignore rule** (mirrors [`read_dir_shallow_fs`]): when the
/// dir is inside a git work tree we run only the *directory* children through
/// `git check-ignore --stdin` and drop the ignored ones — ignored DIRECTORIES
/// (`node_modules`, `dist`, …) are hidden, but FILES are always emitted, so
/// gitignored config like `.env`/`.env.local` stays visible while browsing. The
/// earlier script ran files through `check-ignore` too and so hid `.env`. Outside
/// a git work tree there's nothing to ignore against (the bare listing). The
/// always-prune dirs are dropped by name in the parse loop on top of this.
///
/// When `show_ignored` is true the filter is skipped: every child is emitted,
/// ignored dirs included (the parse loop still drops `.git`).
#[cfg(windows)]
fn wsl_list_dir(dir: &str, show_ignored: bool) -> Result<Vec<DirEntry>, String> {
    let distro = host_distro();
    // `find -maxdepth 1` lists immediate children only (shallow); `-printf` emits
    // `name\t<type>` where type is `d` for dirs and `f` for everything else.
    // GNU find's `%y` is the file type letter; map non-`d` to `f`. We filter `.`
    // / `..` out in the parse loop.
    //
    // Directory-only gitignore: when inside a work tree, feed ONLY the directory
    // entries to `git check-ignore --stdin` (with a trailing `/` so directory
    // patterns like `build/` match), collect the ignored dir set, then emit every
    // file plus only the dirs NOT in that set. `check-ignore` exits 1 when nothing
    // matches, so `|| true` keeps the pipeline alive. Already in `dir` via wsl.exe
    // --cd, so we operate on `.` (no `cd "$1"`).
    const SCRIPT_FILTER: &str = r#"
emit() { find . -maxdepth 1 -mindepth 1 -printf '%f\t%y\n' 2>/dev/null; }
if git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  ign=$(emit | while IFS=$'\t' read -r f y; do
          [ "$y" = d ] && printf '%s/\n' "$f"
        done | git check-ignore --stdin 2>/dev/null | sed 's#/$##' || true)
  emit | while IFS=$'\t' read -r f y; do
    if [ "$y" = d ]; then
      skip=
      while IFS= read -r ig; do [ "$f" = "$ig" ] && skip=1 && break; done <<EOF
$ign
EOF
      [ -z "$skip" ] && printf '%s\t%s\n' "$f" "$y"
    else
      printf '%s\t%s\n' "$f" "$y"
    fi
  done
else
  emit
fi
"#;
    // "Show ignored": no filtering — list every child (ignored dirs included).
    const SCRIPT_ALL: &str = r#"find . -maxdepth 1 -mindepth 1 -printf '%f\t%y\n' 2>/dev/null"#;
    let script = if show_ignored {
        SCRIPT_ALL
    } else {
        SCRIPT_FILTER
    };
    // Bounded (LOCAL_IO): a `find` + per-entry `git check-ignore` over `dir`; a slow
    // git / UNC / large dir must not park the file-index handler this runs on.
    let output = crate::bounded_exec::output_with_timeout(
        wsl_bash(&distro, script, dir),
        crate::bounded_exec::LOCAL_IO_TIMEOUT,
    )
    .map_err(|e| format!("failed to spawn/await wsl.exe: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("__TH_NODIR__") {
            return Err(format!("not a directory: {dir}"));
        }
        return Err(format!("wsl list_dir failed: {}", stderr.trim()));
    }

    let base = dir.trim_end_matches('/');
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut out = Vec::new();
    for line in stdout.lines() {
        let (name, ty) = match line.split_once('\t') {
            Some(parts) => parts,
            None => continue,
        };
        if name.is_empty() || name == "." || name == ".." {
            continue;
        }
        // GNU find `%y`: `d` for a directory; `l` is a symlink (we treat as file
        // unless it resolves to a dir — `%y` shows the link itself, so we re-stat
        // symlinks below). Treat `d` as dir, everything else as file.
        let is_dir = ty == "d";
        if is_dir && PRUNED_DIRS.contains(&name) {
            continue; // prune dependency/build dirs from the tree (PRD §9.7)
        }
        out.push(DirEntry {
            name: name.to_string(),
            path: format!("{base}/{name}"),
            is_dir,
            // Size is not surfaced in the tree UI; computing it would cost an
            // extra stat per entry. Report 0 (dirs already report 0 on the fs
            // path too); the reader stats the real size on open.
            size: 0,
        });
    }
    out.sort_by(|a, b| {
        b.is_dir.cmp(&a.is_dir).then_with(|| {
            a.name
                .to_ascii_lowercase()
                .cmp(&b.name.to_ascii_lowercase())
        })
    });
    Ok(out)
}

/// Enumerate every indexable file under `root` natively inside WSL, returning
/// `/`-separated paths RELATIVE to `root`. Uses `rg --files` (ripgrep: honors
/// `.gitignore`, extremely fast), falling back to `git ls-files` then `find` if
/// ripgrep is unavailable in the distro. This replaces walking the tree over the
/// slow UNC bridge. Pruned dependency/build dirs are excluded in-script so the
/// result matches the `std::fs` walker's shape.
#[cfg(windows)]
fn wsl_list_files(root: &str) -> Result<Vec<String>, String> {
    let distro = host_distro();
    // Build the find-fallback prune expression from PRUNED_DIRS so all three
    // enumeration strategies agree with the std::fs walker.
    let prune = PRUNED_DIRS
        .iter()
        .map(|d| format!("-name '{d}'"))
        .collect::<Vec<_>>()
        .join(" -o ");
    // `rg --files` already honors .gitignore and skips .git; we still drop the
    // always-pruned dirs explicitly (rg via --glob) so node_modules/target/etc
    // never appear even when not gitignored. The git + find fallbacks prune too.
    let globs = PRUNED_DIRS
        .iter()
        .map(|d| format!("--glob '!{d}/**'"))
        .collect::<Vec<_>>()
        .join(" ");
    // Already in `root` via wsl.exe --cd, so operate on `.` (no `cd "$1"`).
    let script = format!(
        r#"
if command -v rg >/dev/null 2>&1; then
  rg --files --hidden --no-messages {globs} --glob '!.git/**' .
elif git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  git ls-files --cached --others --exclude-standard
else
  find . \( {prune} \) -prune -o -type f -print
fi
"#
    );
    // Bounded (LOCAL_IO): `rg`/`git ls-files`/`find` over the WHOLE project tree; a
    // large repo or slow FS must not park the file-search handler this runs on.
    let output = crate::bounded_exec::output_with_timeout(
        wsl_bash(&distro, &script, root),
        crate::bounded_exec::LOCAL_IO_TIMEOUT,
    )
    .map_err(|e| format!("failed to spawn/await wsl.exe: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("__TH_NODIR__") {
            return Err(format!("not a directory: {root}"));
        }
        return Err(format!("wsl index failed: {}", stderr.trim()));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut rels: Vec<String> = stdout
        .lines()
        .map(|l| l.trim().trim_start_matches("./"))
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect();
    rels.sort();
    rels.dedup();
    Ok(rels)
}

/// Build a [`ProjectIndex`] from a precomputed relative-path list (the WSL fast
/// path on Windows). Mirrors the metadata the `std::fs` walker derives per file
/// ([`FileEntry::from_rel`]), keeping the index shape identical so `search_files`
/// and the frontend are unaffected. No per-file open/sniff: the enumeration
/// already excluded VCS/build dirs, and re-reading every file's first bytes over
/// the bridge would defeat the point of the native enumeration.
#[cfg(windows)]
fn build_index_from_rels(root: &Path, rels: Vec<String>) -> ProjectIndex {
    let mut entries: Vec<FileEntry> = rels.into_iter().map(FileEntry::from_rel).collect();
    entries.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    ProjectIndex {
        root: root.to_path_buf(),
        entries,
    }
}

/// Cap on entries traversed when building an index over the CONTROL CHANNEL (M2b
/// hardening). The local Tauri path passes `None` — a user indexing their own
/// project must never be capped (large monorepos are legitimate). Over the opt-in
/// network bind, an authenticated peer requesting `index_project` on `/` or `C:\`
/// would otherwise drive an unbounded in-process walk; this bounds the traversal
/// and returns a clear error telling the caller to scope to a project subdirectory.
/// Generous: a real project is far under this — only a pathological root trips it.
const CONTROL_INDEX_MAX_ENTRIES: usize = 200_000;

/// The error returned when a control-channel index walk exceeds
/// [`CONTROL_INDEX_MAX_ENTRIES`] — phrased so the caller knows to narrow the root.
fn index_too_large_err(root: &Path, cap: usize) -> String {
    format!(
        "refusing to index more than {cap} entries under {} over the control \
         channel; scope the request to a project subdirectory",
        root.display()
    )
}

/// Build (or rebuild) the index for `root`, honoring `.gitignore` + pruned dirs
/// + a binary sniff, and walk it into a flat [`ProjectIndex`]. `max_entries`
/// bounds the traversal for the control-channel callers (`None` = uncapped, for
/// the local Tauri path) — see [`CONTROL_INDEX_MAX_ENTRIES`].
fn build_index(root: &Path, max_entries: Option<usize>) -> Result<ProjectIndex, String> {
    // Windows fast path: a native WSL project lives on ext4 behind the slow
    // `\\wsl.localhost\` UNC/9P bridge. Walking it with `std::fs` is painfully
    // slow, so enumerate the file list natively inside the distro instead (rg →
    // git ls-files → find). The resulting index has the same shape as the walker.
    #[cfg(windows)]
    {
        if let Some(posix) = unc_to_posix(root) {
            let rels = wsl_list_files(&posix)?;
            // rg already ran (a bounded child spawn), but cap the returned list so
            // a pathological root can't balloon the index in memory.
            if let Some(cap) = max_entries {
                if rels.len() > cap {
                    return Err(index_too_large_err(root, cap));
                }
            }
            return Ok(build_index_from_rels(root, rels));
        }
    }

    if !root.is_dir() {
        return Err(format!("not a directory: {}", root.display()));
    }

    let mut entries = Vec::new();
    // Bound total traversal (M2b): count every entry the walker yields — files,
    // dirs, AND skips — not just indexed files, so a tree of mostly-empty dirs
    // can't walk unbounded. Uncapped when `max_entries` is `None` (local path).
    let mut visited = 0usize;

    // `ignore::WalkBuilder` gives us .gitignore + global gitignore + .ignore
    // semantics for free. We additionally prune the always-skip dirs and skip
    // files that sniff as binary.
    let walker = WalkBuilder::new(root)
        .hidden(false) // we still want dotfiles like .env.example; we prune .git explicitly
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .parents(true)
        .filter_entry(|e| {
            // Prune the always-skip directories by name.
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                if let Some(name) = e.file_name().to_str() {
                    if PRUNED_DIRS.contains(&name) {
                        return false;
                    }
                }
            }
            true
        })
        .build();

    for result in walker {
        if let Some(cap) = max_entries {
            visited += 1;
            if visited > cap {
                return Err(index_too_large_err(root, cap));
            }
        }
        let dent = match result {
            Ok(d) => d,
            Err(_) => continue, // unreadable entry: skip, don't fail the whole walk
        };
        // Only index files (the root dir itself and subdirs are not entries).
        if !dent.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let path = dent.path();
        // Skip binary blobs by content sniff (cheap; only first few KiB).
        if is_probably_binary(path) {
            continue;
        }
        let rel = match path.strip_prefix(root) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let rel_str = rel_to_slash(rel);
        if rel_str.is_empty() {
            continue;
        }
        entries.push(FileEntry::from_rel(rel_str));
    }

    // Stable, predictable order: by relative path. Search reorders by score.
    entries.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));

    Ok(ProjectIndex {
        root: root.to_path_buf(),
        entries,
    })
}

/// Convert a relative path to a `/`-separated string (normalizes Windows `\`).
fn rel_to_slash(rel: &Path) -> String {
    let mut parts = Vec::new();
    for comp in rel.components() {
        if let std::path::Component::Normal(os) = comp {
            parts.push(os.to_string_lossy().into_owned());
        }
    }
    parts.join("/")
}

/// Cheap binary sniff: read up to [`SNIFF_BYTES`] and treat the file as binary
/// if it contains a NUL byte. This is the same heuristic Git uses and is good
/// enough to keep images/executables/archives out of a text index/reader.
fn is_probably_binary(path: &Path) -> bool {
    use std::io::Read;
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return true, // can't read → don't index it as text
    };
    let mut buf = [0u8; SNIFF_BYTES];
    let n = match file.read(&mut buf) {
        Ok(n) => n,
        Err(_) => return true,
    };
    buf[..n].contains(&0)
}

// ---------------------------------------------------------------------------
// Fuzzy matching
// ---------------------------------------------------------------------------

/// Subsequence fuzzy match with a small scoring model tuned for file paths.
/// Returns `None` when `needle` is not a subsequence of `haystack`.
///
/// Scoring rewards: matches in the basename over the directory portion, runs of
/// consecutive characters, matches at word boundaries (`/`, `_`, `-`, `.`, or a
/// camelCase hump), and an exact-prefix start. Earlier matches beat later ones.
fn fuzzy_score(haystack: &str, needle: &str) -> Option<i64> {
    if needle.is_empty() {
        return Some(0);
    }
    let h: Vec<char> = haystack.chars().collect();
    let hl: Vec<char> = haystack.chars().map(|c| c.to_ascii_lowercase()).collect();
    let n: Vec<char> = needle.chars().map(|c| c.to_ascii_lowercase()).collect();

    let mut score: i64 = 0;
    let mut hi = 0usize;
    let mut prev_match: Option<usize> = None;

    for &nc in &n {
        // Advance haystack to the next occurrence of nc.
        let mut found = None;
        while hi < hl.len() {
            if hl[hi] == nc {
                found = Some(hi);
                break;
            }
            hi += 1;
        }
        let idx = found?; // not a subsequence
                          // Base reward for a matched char.
        score += 10;
        // Consecutive-run bonus.
        if let Some(prev) = prev_match {
            if idx == prev + 1 {
                score += 12;
            } else {
                // Gap penalty grows with distance (capped) so tight matches win.
                let gap = (idx - prev) as i64;
                score -= gap.min(8);
            }
        } else {
            // First matched char: prefix start is best.
            if idx == 0 {
                score += 18;
            }
        }
        // Word-boundary bonus.
        if is_boundary(&h, idx) {
            score += 9;
        }
        // Exact-case (the original char matched without lowercasing) small bonus.
        if h[idx] == nc {
            score += 1;
        }
        prev_match = Some(idx);
        hi = idx + 1;
    }

    // Prefer shorter haystacks (a hit in `a.rs` beats the same in `deep/a.rs`).
    score -= (h.len() as i64) / 16;
    Some(score)
}

/// Is position `idx` a "word boundary" in `chars` (start, or preceded by a
/// separator, or a lower→upper camelCase hump)?
fn is_boundary(chars: &[char], idx: usize) -> bool {
    if idx == 0 {
        return true;
    }
    let prev = chars[idx - 1];
    if matches!(prev, '/' | '\\' | '_' | '-' | '.' | ' ') {
        return true;
    }
    // camelCase hump: previous lower, current upper.
    prev.is_ascii_lowercase() && chars[idx].is_ascii_uppercase()
}

/// Score a single entry against `query`, taking the best of basename / full-path
/// / extension matches (with basename weighted highest). Returns `None` if the
/// query matches none of them.
fn score_entry(entry: &FileEntry, query: &str) -> Option<i64> {
    // An extension-style query like ".rs" or "rs" should rank exact-ext hits high.
    let ext_query = query.strip_prefix('.').unwrap_or(query);
    let ext_bonus = if !ext_query.is_empty() && entry.ext == ext_query.to_ascii_lowercase() {
        40
    } else {
        0
    };

    let base = fuzzy_score(&entry.basename, query).map(|s| s + 25); // basename weighted up
    let path = fuzzy_score(&entry.rel_path, query);

    let best = match (base, path) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    };

    match best {
        Some(s) => Some(s + ext_bonus + if entry.is_key_file { 5 } else { 0 }),
        // Pure extension query that matched the ext but not as a subsequence.
        None if ext_bonus > 0 => Some(ext_bonus),
        None => None,
    }
}

/// Rank `index` against `query`, returning up to `limit` hits best-first.
fn search_index(index: &ProjectIndex, query: &str, limit: usize) -> Vec<FileHit> {
    let query = query.trim();
    if query.is_empty() {
        // Empty query: return key files first, then a stable prefix of the index.
        let mut hits: Vec<FileHit> = index
            .entries
            .iter()
            .map(|e| FileHit {
                rel_path: e.rel_path.clone(),
                basename: e.basename.clone(),
                ext: e.ext.clone(),
                is_key_file: e.is_key_file,
                score: if e.is_key_file { 1 } else { 0 },
            })
            .collect();
        hits.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then_with(|| a.rel_path.cmp(&b.rel_path))
        });
        hits.truncate(limit);
        return hits;
    }

    let mut scored: Vec<FileHit> = index
        .entries
        .iter()
        .filter_map(|e| {
            score_entry(e, query).map(|score| FileHit {
                rel_path: e.rel_path.clone(),
                basename: e.basename.clone(),
                ext: e.ext.clone(),
                is_key_file: e.is_key_file,
                score,
            })
        })
        .collect();

    // Best score first; tie-break by shorter path then lexical for stability.
    scored.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.rel_path.len().cmp(&b.rel_path.len()))
            .then_with(|| a.rel_path.cmp(&b.rel_path))
    });
    scored.truncate(limit);
    scored
}

/// Shallow directory listing for the tree view.
///
/// On Windows, when the caller hands us a native POSIX/WSL path (`/home/...`),
/// we list it **natively inside WSL** via [`wsl_list_dir`] rather than reading it
/// over the `\\wsl.localhost\` UNC bridge. Cold UNC directory reads over the 9P
/// bridge take seconds each; a native `wsl.exe` listing is essentially instant.
/// Anything else (a real Windows path on Windows, or any path on unix) falls
/// through to the native [`read_dir_shallow_fs`] over `std::fs`.
///
/// `show_ignored` is forwarded to both paths: false (default) applies the
/// directory-only gitignore rule (hide ignored dirs, always show ignored files);
/// true lists everything (ignored dirs included), only ever pruning `.git`.
fn read_dir_shallow(dir: &Path, show_ignored: bool) -> Result<Vec<DirEntry>, String> {
    #[cfg(windows)]
    {
        // Detect a POSIX-absolute path the way `to_host_path` does. The path
        // arrives here already routed through `normalize`/`to_host_path`, so a
        // WSL path is now in UNC form (`\\wsl.localhost\<distro>\home\...`). We
        // peel the UNC prefix back off to a POSIX path and list it inside WSL.
        if let Some(posix) = unc_to_posix(dir) {
            return wsl_list_dir(&posix, show_ignored);
        }
    }
    read_dir_shallow_fs(dir, show_ignored)
}

/// Native shallow listing: directories first, then files, each alphabetical.
/// The instant path on unix; the UNC-over-9P (slow) path on Windows for any
/// non-WSL Windows path.
///
/// **Gitignore rule (the refinement over plain "respect `.gitignore`"):** the
/// gitignore filter applies to **directories only**. Ignored *directories*
/// (`node_modules`, `dist`, `build`, `target`, `.next`, `coverage`, …) are
/// hidden so the tree isn't drowned in bulk noise; ignored *files* are always
/// SHOWN, so config that's conventionally gitignored — `.env`, `.env.local`,
/// `.env.*`, and friends — is visible while browsing (the user wants `.env`
/// visible for sure). The earlier version filtered files too and so hid `.env`.
///
/// We get this in two passes:
///   1. A depth-1 [`ignore::WalkBuilder`] walk with the SAME ignore stack as
///      [`build_index`] (gitignore + global gitignore + `.git/info/exclude` +
///      parent gitignores). Because the walk prunes ignored *and* always-prune
///      directories during descent, the dirs it yields are exactly the dirs we
///      want — ignored dirs never appear. (Its non-ignored *files* are kept too.)
///   2. A raw [`std::fs::read_dir`] of just this directory that adds back any
///      **file** entries the walk dropped as gitignored. Only files are added
///      (never dirs), so ignored directories stay hidden while ignored files
///      reappear. `.git` files are still skipped (VCS plumbing).
///
/// When `show_ignored` is true the rule is bypassed entirely: every entry is
/// listed (raw `read_dir`, ignored dirs included) except `.git`, which is always
/// pruned as un-browsable VCS internals.
fn read_dir_shallow_fs(dir: &Path, show_ignored: bool) -> Result<Vec<DirEntry>, String> {
    if !dir.is_dir() {
        return Err(format!("not a directory: {}", dir.display()));
    }

    // "Show ignored" ON: a plain shallow listing of everything, ignored dirs
    // (node_modules/target/…) and ignored files alike — only `.git` is pruned.
    if show_ignored {
        return read_dir_raw(dir, |_name, _is_dir| true);
    }

    // Depth-1 walk: the root itself arrives at depth 0 (skipped), its immediate
    // children at depth 1. `filter_entry` prunes the always-skip dirs by name so
    // they never even descend; the ignore settings mirror `build_index` exactly
    // so the tree's DIRECTORIES match what the search index would keep.
    let walker = WalkBuilder::new(dir)
        .max_depth(Some(1))
        .hidden(false) // keep dotfiles like .env.example; .git is pruned by name
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .parents(true)
        .filter_entry(|e| {
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                if let Some(name) = e.file_name().to_str() {
                    if PRUNED_DIRS.contains(&name) {
                        return false;
                    }
                }
            }
            true
        })
        .build();

    let mut out = Vec::new();
    let mut seen_files: std::collections::HashSet<String> = std::collections::HashSet::new();
    for result in walker {
        let dent = match result {
            Ok(d) => d,
            Err(_) => continue, // unreadable entry: skip, don't fail the whole list
        };
        // Skip the root directory itself (depth 0); we only want its children.
        if dent.depth() == 0 {
            continue;
        }
        let path = dent.path();
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        let is_dir = dent.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if !is_dir {
            seen_files.insert(name.clone());
        }
        // `metadata()` here is cheap (the walker already stat'd the entry); fall
        // back to 0 if it's somehow unavailable. Dirs always report 0.
        let size = if is_dir {
            0
        } else {
            dent.metadata().map(|m| m.len()).unwrap_or(0)
        };
        out.push(DirEntry {
            name,
            path: path.to_string_lossy().into_owned(),
            is_dir,
            size,
        });
    }

    // Second pass: add back gitignored FILES the walk omitted, so `.env` & co.
    // show. Only files are added (a `false` for dirs in the closure), so ignored
    // directories stay hidden — the gitignore filter remains directory-only.
    let extras = read_dir_raw(dir, |name, is_dir| !is_dir && !seen_files.contains(name))?;
    out.extend(extras);

    out.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir) // dirs (true) before files (false)
            .then_with(|| {
                a.name
                    .to_ascii_lowercase()
                    .cmp(&b.name.to_ascii_lowercase())
            })
    });
    Ok(out)
}

/// Raw shallow `std::fs::read_dir` of `dir`, keeping each entry for which
/// `keep(name, is_dir)` returns true. `.git` is ALWAYS skipped (un-browsable VCS
/// plumbing). Used both for the "Show ignored" listing and to add gitignored
/// files back onto the directory-only-filtered listing. Not sorted here — the
/// caller merges + sorts.
fn read_dir_raw(dir: &Path, keep: impl Fn(&str, bool) -> bool) -> Result<Vec<DirEntry>, String> {
    let rd = std::fs::read_dir(dir).map_err(|e| format!("read_dir failed: {e}"))?;
    let mut out = Vec::new();
    for ent in rd.flatten() {
        let name = match ent.file_name().into_string() {
            Ok(n) => n,
            Err(_) => continue, // non-UTF-8 name: skip
        };
        if name == ".git" {
            continue;
        }
        // `file_type()` avoids a follow-symlink stat; fall back to a full stat
        // only if it's unavailable. Treat anything we can't classify as a file.
        let is_dir = match ent.file_type() {
            Ok(ft) => ft.is_dir(),
            Err(_) => ent.metadata().map(|m| m.is_dir()).unwrap_or(false),
        };
        if !keep(&name, is_dir) {
            continue;
        }
        let path = ent.path();
        let size = if is_dir {
            0
        } else {
            ent.metadata().map(|m| m.len()).unwrap_or(0)
        };
        out.push(DirEntry {
            name,
            path: path.to_string_lossy().into_owned(),
            is_dir,
            size,
        });
    }
    Ok(out)
}

/// Read a text file with a hard size cap, returning lossy-UTF-8 text. Rejects
/// binary blobs (the reader is for text/Markdown only).
fn read_text_capped(path: &Path) -> Result<FileContents, String> {
    use std::io::Read;
    let meta = std::fs::metadata(path).map_err(|e| format!("stat failed: {e}"))?;
    if meta.is_dir() {
        return Err(format!("is a directory: {}", path.display()));
    }
    let size = meta.len();

    if is_probably_binary(path) {
        return Err(format!("not a text file: {}", path.display()));
    }

    let file = std::fs::File::open(path).map_err(|e| format!("open failed: {e}"))?;
    let truncated = size > MAX_READ_BYTES;
    let mut buf = Vec::with_capacity(size.min(MAX_READ_BYTES) as usize);
    file.take(MAX_READ_BYTES)
        .read_to_end(&mut buf)
        .map_err(|e| format!("read failed: {e}"))?;

    let text = String::from_utf8_lossy(&buf).into_owned();
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();

    Ok(FileContents {
        path: path.to_string_lossy().into_owned(),
        ext,
        text,
        truncated,
        size,
    })
}

// ---------------------------------------------------------------------------
// Control-channel entry points (additive; reuse the private helpers above)
// ---------------------------------------------------------------------------
//
// These let the MCP control listener (`crate::control`) drive file search +
// reading without going through the Tauri `#[command]` wrappers, which require a
// `tauri::State` borrow that only exists inside the invoke handler. They are thin
// re-exports of the exact same `normalize` + `build_index` + `search_index` +
// `read_text_capped` logic the commands use — no behavior of the existing
// commands changes.

/// Control-channel fuzzy search: normalize `root`, index it (cached on `state`),
/// and rank `query`. Mirrors the `search_files` command body.
pub fn control_search(
    state: &FileIndexState,
    root: &str,
    query: &str,
    limit: usize,
    enforce_scope: bool,
    allowed_roots: &[PathBuf],
) -> Result<Vec<FileHit>, String> {
    let root = scoped_path(root, enforce_scope, allowed_roots)?;
    // The walk cap is a REMOTE DoS bound; loopback (the local user's own project,
    // possibly a huge monorepo) indexes uncapped.
    let cap = if enforce_scope {
        Some(CONTROL_INDEX_MAX_ENTRIES)
    } else {
        None
    };
    let index = match state.get(&root) {
        Some(i) => i,
        None => state.put(build_index(&root, cap)?),
    };
    Ok(search_index(&index, query, limit.clamp(1, 1000)))
}

/// Parse the `T_HUB_REMOTE_FILE_ROOTS` allowlist: colon-separated absolute
/// (WSL/POSIX) project roots a REMOTE peer may browse/read under. Empties trimmed.
/// Each root is RESOLVED to its real symlink-free path once here (so the per-request
/// scope check compares resolved-vs-resolved); a root that can't be resolved (doesn't
/// exist / not a WSL path) is dropped — fail-closed (it can only deny, never widen).
fn parse_file_roots(raw: &str) -> Vec<PathBuf> {
    raw.split(':')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(resolve_real_posix)
        .collect()
}

/// The OPERATOR allowlist of roots a remote peer may access (server-split #23,
/// security review). Read ONCE from `T_HUB_REMOTE_FILE_ROOTS`; **empty by default**,
/// so remote file access is OFF until the operator names specific roots (fail-closed,
/// opt-in — like the M2b network bind itself). Loopback callers ignore it entirely.
pub fn remote_file_roots() -> &'static [PathBuf] {
    static ROOTS: std::sync::OnceLock<Vec<PathBuf>> = std::sync::OnceLock::new();
    ROOTS.get_or_init(|| {
        std::env::var("T_HUB_REMOTE_FILE_ROOTS")
            .ok()
            .map(|s| parse_file_roots(&s))
            .unwrap_or_default()
    })
}

// --- Remote file-access scope (server-split #23/#26/#27) -------------------
// For a REMOTE peer the boundary is the OPERATOR allowlist (T_HUB_REMOTE_FILE_ROOTS),
// NOT the peer-chosen index — so a peer can't widen scope by `index_project`-ing a
// sensitive dir. Empty allowlist ⇒ deny everything (the default). We reject `..`,
// then resolve symlinks AUTHORITATIVELY before the under-root check (#26: inside WSL
// on Windows, where std::fs::canonicalize can't be trusted over the 9P UNC bridge),
// and the read targets the host form of the RESOLVED path so a symlink can't redirect
// it after the check. Loopback (same machine) bypasses all of this.

/// Shared pre-check: an empty allowlist denies all remote access (the default), and a
/// `..` component is rejected outright (belt-and-suspenders ahead of resolution).
fn remote_precheck(path: &str, allowed_roots: &[PathBuf]) -> Result<(), String> {
    if allowed_roots.is_empty() {
        return Err("remote file/worktree access is disabled — set \
                    T_HUB_REMOTE_FILE_ROOTS to a colon-separated list of allowed \
                    roots to enable it"
            .to_string());
    }
    if Path::new(path)
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(format!(
            "rejecting a path containing '..' over the control channel: {path}"
        ));
    }
    Ok(())
}

/// Authoritative symlink-resolving real path for the scope check, as a POSIX path.
/// On unix the daemon IS the WSL/Linux host, so `canonicalize` resolves natively. On
/// Windows the daemon reaches WSL over the 9P UNC bridge, where `std::fs::canonicalize`
/// can't be relied on to resolve ext4 symlinks — so we run `realpath` INSIDE the
/// distro on the POSIX form (#26). Returns None ⇒ the caller DENIES (fail-closed):
/// path doesn't exist, isn't a WSL path, or the wsl.exe call failed.
fn resolve_real_posix(path: &str) -> Option<PathBuf> {
    #[cfg(not(windows))]
    {
        std::fs::canonicalize(path).ok()
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use std::process::Command;
        // POSIX form: pass-through for a bare POSIX path, strip the WSL-UNC prefix
        // for a host path; a non-WSL drive path ⇒ None ⇒ caller fail-closes.
        let posix = unc_to_posix(&PathBuf::from(path))?;
        let mut c = Command::new("wsl.exe");
        c.arg("-d")
            .arg(host_distro())
            .arg("-e")
            .arg("realpath")
            .arg("--")
            .arg(&posix);
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
                                       // Bounded (WSL_PROBE): a single `realpath`; this is a scope-validation gate
                                       // on file-read requests, so a wedged WSL must not park the handler.
        let out =
            crate::bounded_exec::output_with_timeout(c, crate::bounded_exec::WSL_PROBE_TIMEOUT)
                .ok()?;
        if !out.status.success() {
            return None;
        }
        let resolved = String::from_utf8(out.stdout).ok()?;
        let resolved = resolved.trim();
        if !resolved.starts_with('/') {
            return None;
        }
        Some(PathBuf::from(resolved))
    }
}

/// True if a RESOLVED (symlink-free POSIX) path equals or is nested under one of the
/// (also-resolved) `allowed_roots`. Component-wise via `Path::starts_with`, so
/// `/a/proj` does NOT match `/a/proj-secret`.
fn under_allowed_root(resolved: &Path, allowed_roots: &[PathBuf]) -> bool {
    allowed_roots
        .iter()
        .any(|root| resolved == root || resolved.starts_with(root))
}

/// Convert a resolved POSIX path back to the host form the file readers use (identity
/// on unix; the `\\wsl.localhost\...` UNC on Windows). The read then targets exactly
/// the resolved path the scope check validated — a symlink can't redirect it after.
fn host_of_resolved(real_posix: &Path) -> PathBuf {
    to_host_path(&real_posix.to_string_lossy())
}

/// The POSIX form of a control-channel path for the ancestor walk: identity on unix
/// (already POSIX), the WSL-UNC→POSIX mapping on Windows.
fn posix_form(path: &str) -> String {
    #[cfg(not(windows))]
    {
        path.to_string()
    }
    #[cfg(windows)]
    {
        unc_to_posix(&PathBuf::from(path)).unwrap_or_else(|| path.to_string())
    }
}

fn scoped_path(path: &str, enforce: bool, allowed_roots: &[PathBuf]) -> Result<PathBuf, String> {
    if !enforce {
        return Ok(normalize(path));
    }
    remote_precheck(path, allowed_roots)?;
    let real =
        resolve_real_posix(path).ok_or_else(|| format!("cannot resolve {path} on the host"))?;
    if !under_allowed_root(&real, allowed_roots) {
        return Err(format!(
            "path is outside the allowed remote roots ({}): {}",
            allowed_roots.len(),
            real.display()
        ));
    }
    Ok(host_of_resolved(&real))
}

/// Like [`scoped_path`] but for a path that may NOT exist yet — a worktree dir a
/// remote peer asks to CREATE (`create_worktree`), or an existing one to remove.
/// Rejects `..`, then resolves the deepest EXISTING ancestor and requires it under an
/// allowed root, so the (symlink-free, `..`-free) new tail lands inside it. Loopback
/// is unrestricted. Returns the normalized host target.
pub fn scoped_create_path(
    path: &str,
    enforce: bool,
    allowed_roots: &[PathBuf],
) -> Result<PathBuf, String> {
    if !enforce {
        return Ok(normalize(path));
    }
    remote_precheck(path, allowed_roots)?;
    let posix = posix_form(path);
    let mut ancestor = Path::new(&posix);
    let resolved_ancestor = loop {
        if let Some(resolved) = resolve_real_posix(&ancestor.to_string_lossy()) {
            break resolved;
        }
        match ancestor.parent() {
            Some(parent) => ancestor = parent,
            None => return Err(format!("cannot resolve any existing ancestor of {path}")),
        }
    };
    if !under_allowed_root(&resolved_ancestor, allowed_roots) {
        return Err(format!(
            "path is outside the allowed remote roots ({}): {path}",
            allowed_roots.len(),
        ));
    }
    Ok(normalize(path))
}

/// Control-channel capped text read (server-split #23 — the Files reader over the
/// socket): scope `path` (remote peers only, to `allowed_roots`), then read it
/// through the same size-capped, binary-rejecting reader as the `read_text_file`
/// command.
pub fn control_read_text(
    path: &str,
    enforce_scope: bool,
    allowed_roots: &[PathBuf],
) -> Result<FileContents, String> {
    let p = scoped_path(path, enforce_scope, allowed_roots)?;
    read_text_capped(&p)
}

/// Control-channel shallow directory listing (server-split #23 — the Files tree
/// over the socket): scope `path` (remote peers only), then list it exactly as the
/// `list_dir` command does (dirs first, the directory-only gitignore rule).
pub fn control_list_dir(
    path: &str,
    show_ignored: bool,
    enforce_scope: bool,
    allowed_roots: &[PathBuf],
) -> Result<Vec<DirEntry>, String> {
    let dir = scoped_path(path, enforce_scope, allowed_roots)?;
    read_dir_shallow(&dir, show_ignored)
}

/// Control-channel index build: walk `root`, cache the index in the control
/// channel's own [`FileIndexState`], and return its summary — the server-side
/// mirror of the `index_project` command. A subsequent [`control_search`] on the
/// same root reuses this cache. (Server-split M3: the file index served by the
/// daemon, so a thin client warms + searches the REMOTE tree's index.)
pub fn control_index(
    state: &FileIndexState,
    root: &str,
    enforce_scope: bool,
    allowed_roots: &[PathBuf],
) -> Result<IndexSummary, String> {
    let root = scoped_path(root, enforce_scope, allowed_roots)?;
    // Remote: cap the walk (DoS bound). Loopback indexes the user's own project
    // uncapped (a large local monorepo is legitimate).
    let cap = if enforce_scope {
        Some(CONTROL_INDEX_MAX_ENTRIES)
    } else {
        None
    };
    let index = build_index(&root, cap)?;
    let root_str = index.root.to_string_lossy().into_owned();
    let arc = state.put(index);
    Ok(IndexSummary {
        root: root_str,
        count: arc.entries.len(),
    })
}

// ---------------------------------------------------------------------------
// Tauri commands (registered in lib.rs; mirrored in src/ipc/files.ts)
// ---------------------------------------------------------------------------

/// Walk `root`, build the compact in-memory index (cached by root), and return a
/// summary (root + file count). Subsequent `search_files` calls reuse the cache.
#[tauri::command]
pub async fn index_project(
    state: tauri::State<'_, FileIndexState>,
    root: String,
) -> Result<IndexSummary, String> {
    let root = normalize(&root);
    // The walk is a full `wsl.exe rg --files` spawn (blocking on a child); run it
    // off the Tokio executor so it can't pin a worker. `build_index` borrows only
    // an owned `PathBuf`, so the `'static` closure captures a clone — no `&State`
    // crosses the `.await`. The `FileIndexState` Mutex is touched only by the
    // brief `put` AFTER the walk completes, never held across it.
    let walk_root = root.clone();
    let index = tauri::async_runtime::spawn_blocking(move || build_index(&walk_root, None))
        .await
        .map_err(|e| format!("index_project task failed: {e}"))??;
    let count = index.entries.len();
    let root_str = index.root.to_string_lossy().into_owned();
    state.put(index);
    Ok(IndexSummary {
        root: root_str,
        count,
    })
}

/// Fuzzy-search the index for `root`. If the root has not been indexed yet (or
/// the cache was lost), it is indexed on demand first.
#[tauri::command]
pub async fn search_files(
    state: tauri::State<'_, FileIndexState>,
    root: String,
    query: String,
    limit: Option<usize>,
) -> Result<Vec<FileHit>, String> {
    let root = normalize(&root);
    // Warm path: a cached `Arc<ProjectIndex>` is a cheap clone under the Mutex (it
    // acquires and releases internally), so the lock is never held across any
    // blocking work. Cold path: `build_index` is a full `wsl.exe rg --files` walk
    // — hop it onto a blocking thread so it can't pin a Tokio worker, then cache
    // the result. The closure captures only an owned `PathBuf` clone, never
    // `&State`, and `put` re-acquires the Mutex AFTER the walk.
    let index = match state.get(&root) {
        Some(i) => i,
        None => {
            let walk_root = root.clone();
            let built = tauri::async_runtime::spawn_blocking(move || build_index(&walk_root, None))
                .await
                .map_err(|e| format!("search_files task failed: {e}"))??;
            state.put(built)
        }
    };
    let limit = limit.unwrap_or(50).clamp(1, 1000);
    Ok(search_index(&index, &query, limit))
}

/// Shallow directory listing for the tree view (no recursion; folder expansion
/// is a follow-up `list_dir` call, per PRD §9.7).
///
/// `show_ignored` (optional, default false) toggles the directory-only gitignore
/// rule: false hides ignored DIRECTORIES (`node_modules`, …) while always showing
/// ignored FILES (`.env`, …); true lists everything except `.git`. Wired to the
/// Files panel's "Show ignored" toggle.
#[tauri::command]
pub async fn list_dir(path: String, show_ignored: Option<bool>) -> Result<Vec<DirEntry>, String> {
    let dir = normalize(&path);
    // `read_dir_shallow` does a UNC `\\wsl.localhost\` directory read on Windows
    // (or a native `wsl.exe` child spawn) — blocking IO. Run it off the Tokio
    // executor so it can't pin a worker thread. The closure captures only an
    // owned `PathBuf` clone of `dir` (so it stays `'static + Send`); the original
    // `dir` (and `path`) remain here for the diag log below, mirroring
    // `git_info`'s clone-before-`spawn_blocking`.
    let scan_dir = dir.clone();
    let show = show_ignored.unwrap_or(false);
    let res = tauri::async_runtime::spawn_blocking(move || read_dir_shallow(&scan_dir, show))
        .await
        .map_err(|e| format!("list_dir task failed: {e}"))?;
    // DIAG: file tree "not loading" was undiagnosable from the release build.
    // Log the incoming path, the normalized host path, and the outcome so the
    // diag log shows exactly where the tree breaks (bad/empty path vs wsl/fs read
    // returning nothing vs an error).
    match &res {
        Ok(v) => crate::diag::diag_log(format!(
            "{{\"t\":\"files\",\"m\":\"list_dir OK: in='{}' host='{}' -> {} entries\"}}",
            path.replace('\'', " "),
            dir.display().to_string().replace('\'', " "),
            v.len()
        )),
        Err(e) => crate::diag::diag_log(format!(
            "{{\"t\":\"files\",\"m\":\"list_dir ERR: in='{}' host='{}' -> {}\"}}",
            path.replace('\'', " "),
            dir.display().to_string().replace('\'', " "),
            e.replace('\'', " ")
        )),
    }
    res
}

/// Read a (text) file for the reader, capped at [`MAX_READ_BYTES`].
#[tauri::command]
pub async fn read_text_file(path: String) -> Result<FileContents, String> {
    let p = normalize(&path);
    // `read_text_capped` stats + reads the file over the `\\wsl.localhost\` UNC
    // bridge on Windows — blocking IO that would pin a Tokio worker if run in the
    // async body. Hop it onto a blocking thread; the owned `PathBuf` is moved into
    // the `'static + Send` closure.
    tauri::async_runtime::spawn_blocking(move || read_text_capped(&p))
        .await
        .map_err(|e| format!("read_text_file task failed: {e}"))?
}

/// Overwrite `path` with `contents` (the editor's save). Routes through the same
/// `normalize` path translation as the reader, so a WSL path saves to the right
/// place. Refuses to clobber a directory; otherwise creates/overwrites the file.
/// The frontend only enables editing for non-truncated text files, so this never
/// writes back a capped/partial buffer.
#[tauri::command]
pub async fn write_text_file(path: String, contents: String) -> Result<(), String> {
    let p = normalize(&path);
    if let Ok(meta) = std::fs::metadata(&p) {
        if meta.is_dir() {
            return Err(format!("is a directory: {}", p.display()));
        }
    }
    std::fs::write(&p, contents.as_bytes()).map_err(|e| format!("write failed: {e}"))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    /// Build a small fixture tree in a unique temp dir and return its root.
    fn make_fixture() -> PathBuf {
        let mut root = std::env::temp_dir();
        root.push(format!("t-hub-files-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();

        // A gitignore that hides `secret.txt`, the `ignored/` dir, `*.log`, and
        // `.env` (the conventional secret-config case the directory-only rule must
        // still SHOW in the tree).
        fs::write(
            root.join(".gitignore"),
            "secret.txt\nignored/\n*.log\n.env\n",
        )
        .unwrap();

        fs::write(root.join("README.md"), "# Title\n\nhello").unwrap();
        fs::write(root.join("package.json"), "{}").unwrap();
        // A gitignored config FILE — must still appear in the tree (directory-only
        // gitignore rule), even though the search index legitimately omits it.
        fs::write(root.join(".env"), "SECRET=1").unwrap();

        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
        fs::write(root.join("src/lib.rs"), "// lib").unwrap();
        fs::write(root.join("src/utils.ts"), "export {}").unwrap();

        // Should be ignored by .gitignore.
        fs::write(root.join("secret.txt"), "shh").unwrap();
        fs::write(root.join("debug.log"), "noise").unwrap();
        fs::create_dir_all(root.join("ignored")).unwrap();
        fs::write(root.join("ignored/x.txt"), "x").unwrap();

        // Should be pruned as a dependency dir even without .gitignore listing it.
        fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        fs::write(root.join("node_modules/pkg/index.js"), "module.exports={}").unwrap();

        // Should be pruned by the explicit .git skip.
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join(".git/config"), "[core]").unwrap();

        // A binary blob (contains NUL) should be skipped by the sniff.
        let mut bin = fs::File::create(root.join("blob.bin")).unwrap();
        bin.write_all(&[0u8, 1, 2, 3, 0, 9]).unwrap();

        root
    }

    fn cleanup(root: &Path) {
        let _ = fs::remove_dir_all(root);
    }

    fn rel_set(index: &ProjectIndex) -> Vec<String> {
        index.entries.iter().map(|e| e.rel_path.clone()).collect()
    }

    #[test]
    fn index_respects_gitignore_and_prunes() {
        let root = make_fixture();
        let index = build_index(&root, None).unwrap();
        let rels = rel_set(&index);

        // Included.
        assert!(rels.contains(&"README.md".to_string()));
        assert!(rels.contains(&"package.json".to_string()));
        assert!(rels.contains(&"src/main.rs".to_string()));
        assert!(rels.contains(&"src/lib.rs".to_string()));
        assert!(rels.contains(&"src/utils.ts".to_string()));

        // Excluded by .gitignore. (The SEARCH INDEX still honors gitignore fully —
        // only the TREE relaxes it to a directory-only rule; see
        // `list_dir_is_shallow_dirs_first_and_prunes`. So a gitignored `.env`
        // is correctly absent here while still showing in the tree.)
        assert!(
            !rels.contains(&"secret.txt".to_string()),
            "gitignored file leaked"
        );
        assert!(!rels.contains(&"debug.log".to_string()), "*.log leaked");
        assert!(
            !rels.contains(&".env".to_string()),
            "gitignored .env leaked into index"
        );
        assert!(
            !rels.iter().any(|r| r.starts_with("ignored/")),
            "gitignored dir leaked"
        );

        // Pruned dirs.
        assert!(
            !rels.iter().any(|r| r.starts_with("node_modules")),
            "node_modules leaked"
        );
        assert!(!rels.iter().any(|r| r.starts_with(".git/")), ".git leaked");

        // Binary blob skipped.
        assert!(
            !rels.contains(&"blob.bin".to_string()),
            "binary blob leaked"
        );

        cleanup(&root);
    }

    #[test]
    fn control_index_cap_bounds_the_walk() {
        let root = make_fixture();
        // A tiny cap trips on the multi-entry fixture: the walk stops early and
        // errors with a "scope to a subdirectory" message (the M2b DoS bound for
        // a peer pointing index_project at `/`), rather than indexing everything.
        let err = build_index(&root, Some(1)).unwrap_err();
        assert!(
            err.contains("refusing to index") && err.contains("control channel"),
            "expected an index-too-large error, got: {err}"
        );
        // A generous cap (and the uncapped local Tauri path) index the fixture fine.
        assert!(build_index(&root, Some(100_000)).is_ok());
        assert!(build_index(&root, None).is_ok());
        cleanup(&root);
    }

    #[test]
    fn parse_file_roots_splits_trims_and_drops_empties() {
        assert!(parse_file_roots("").is_empty());
        assert!(parse_file_roots("  :  : ").is_empty());
        // Three non-empty entries (existing dirs are canonicalized; all still count).
        assert_eq!(parse_file_roots("/tmp: /var :/usr").len(), 3);
    }

    #[test]
    fn control_reads_are_scoped_to_the_operator_allowlist_for_remote() {
        let root = make_fixture();
        let root_str = root.to_string_lossy().into_owned();
        let parent_str = root.parent().unwrap().to_string_lossy().into_owned();
        let readme = root.join("README.md").to_string_lossy().into_owned();
        let allow = vec![normalize(&root_str)]; // operator allows THIS root
        let empty: Vec<PathBuf> = Vec::new();

        // REMOTE + EMPTY allowlist (the default) => everything is denied.
        assert!(control_list_dir(&root_str, false, true, &empty).is_err());
        assert!(control_read_text(&readme, true, &empty).is_err());

        // REMOTE + allowlist[root] => the root + a file inside it are allowed...
        assert!(control_list_dir(&root_str, false, true, &allow).is_ok());
        assert!(control_read_text(&readme, true, &allow).is_ok());
        // ...but the PARENT (outside the allowed root) is refused.
        assert!(control_list_dir(&parent_str, false, true, &allow).is_err());

        // A `..` traversal is refused outright (target a NON-existent leaf so
        // normalize can't canonicalize the `..` away — mirrors the WSL-UNC fast path).
        let traversal = format!("{root_str}/../../no_such_dir_scopetest_xyz");
        let terr = control_list_dir(&traversal, false, true, &allow).unwrap_err();
        assert!(
            terr.contains("'..'"),
            "expected a '..' rejection, got: {terr}"
        );

        // A symlink INSIDE the allowed root that points OUT is refused — canonicalize
        // resolves it to the parent, which isn't under the allowed root. (unix: where
        // canonicalize is authoritative — the native-WSL/Linux daemon endgame.)
        #[cfg(unix)]
        {
            let link = root.join("escape_link");
            let _ = std::os::unix::fs::symlink(root.parent().unwrap(), &link);
            assert!(
                control_list_dir(&link.to_string_lossy(), false, true, &allow).is_err(),
                "a symlink out of the allowed root must be rejected"
            );
        }

        // LOOPBACK (enforce=false) bypasses the scope entirely — the parent lists fine.
        assert!(control_list_dir(&parent_str, false, false, &empty).is_ok());

        cleanup(&root);
    }

    #[test]
    fn scoped_create_path_confines_new_paths_to_the_allowlist() {
        let root = make_fixture();
        let root_str = root.to_string_lossy().into_owned();
        let allow = vec![normalize(&root_str)];
        let empty: Vec<PathBuf> = Vec::new();

        // A NEW path under an allowed root (the leaf doesn't exist yet) is accepted —
        // its deepest existing ancestor (the root) canonicalizes under the allowlist.
        let new_under = format!("{root_str}/wt-new/sub");
        assert!(scoped_create_path(&new_under, true, &allow).is_ok());

        // Empty allowlist => denied (the default); outside the root => denied.
        assert!(scoped_create_path(&new_under, true, &empty).is_err());
        let outside = root.parent().unwrap().join("wt-elsewhere");
        let outside_str = outside.to_string_lossy().into_owned();
        assert!(scoped_create_path(&outside_str, true, &allow).is_err());
        // `..` is refused outright.
        assert!(scoped_create_path(&format!("{root_str}/../escape"), true, &allow).is_err());

        // Loopback (enforce=false) is unrestricted.
        assert!(scoped_create_path(&outside_str, false, &empty).is_ok());

        cleanup(&root);
    }

    #[test]
    fn entry_metadata_is_correct() {
        let root = make_fixture();
        let index = build_index(&root, None).unwrap();
        let main = index
            .entries
            .iter()
            .find(|e| e.rel_path == "src/main.rs")
            .expect("main.rs indexed");
        assert_eq!(main.basename, "main.rs");
        assert_eq!(main.ext, "rs");
        assert!(!main.is_key_file);

        let pkg = index
            .entries
            .iter()
            .find(|e| e.rel_path == "package.json")
            .unwrap();
        assert!(pkg.is_key_file, "package.json should be a key file");

        let readme = index
            .entries
            .iter()
            .find(|e| e.rel_path == "README.md")
            .unwrap();
        assert!(readme.is_key_file, "README should be a key file");

        cleanup(&root);
    }

    #[test]
    fn fuzzy_basename_ranks_above_path() {
        let root = make_fixture();
        let index = build_index(&root, None).unwrap();

        // "main" should surface src/main.rs as the top hit.
        let hits = search_index(&index, "main", 10);
        assert!(!hits.is_empty(), "expected hits for 'main'");
        assert_eq!(hits[0].rel_path, "src/main.rs");

        cleanup(&root);
    }

    #[test]
    fn fuzzy_subsequence_matches() {
        let root = make_fixture();
        let index = build_index(&root, None).unwrap();

        // "srlib" is a subsequence of "src/lib.rs" but not of others meaningfully.
        let hits = search_index(&index, "srlib", 10);
        assert!(
            hits.iter().any(|h| h.rel_path == "src/lib.rs"),
            "subsequence match failed: {:?}",
            hits.iter().map(|h| &h.rel_path).collect::<Vec<_>>()
        );

        cleanup(&root);
    }

    #[test]
    fn extension_query_ranks_matching_ext() {
        let root = make_fixture();
        let index = build_index(&root, None).unwrap();

        let hits = search_index(&index, ".ts", 10);
        assert!(!hits.is_empty(), "expected .ts hits");
        assert_eq!(hits[0].ext, "ts", "top hit should be the .ts file");
        assert_eq!(hits[0].rel_path, "src/utils.ts");

        cleanup(&root);
    }

    #[test]
    fn empty_query_returns_key_files_first() {
        let root = make_fixture();
        let index = build_index(&root, None).unwrap();
        let hits = search_index(&index, "", 10);
        assert!(!hits.is_empty());
        // The first results should be key files (README/package.json).
        assert!(
            hits[0].is_key_file,
            "empty query should lead with key files, got {:?}",
            hits[0].rel_path
        );
        cleanup(&root);
    }

    #[test]
    fn no_match_returns_empty() {
        let root = make_fixture();
        let index = build_index(&root, None).unwrap();
        let hits = search_index(&index, "zzzzzqqqqq", 10);
        assert!(hits.is_empty(), "garbage query should not match");
        cleanup(&root);
    }

    #[test]
    fn search_respects_limit() {
        let root = make_fixture();
        let index = build_index(&root, None).unwrap();
        let hits = search_index(&index, "", 2);
        assert_eq!(hits.len(), 2, "limit should cap results");
        cleanup(&root);
    }

    #[test]
    fn list_dir_is_shallow_dirs_first_and_prunes() {
        let root = make_fixture();
        let entries = read_dir_shallow(&root, false).unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();

        // Shallow: src is listed as a dir, but src/main.rs is not present.
        assert!(names.contains(&"src"));
        assert!(!names.contains(&"main.rs"));

        // node_modules is pruned from the tree (an always-prune dir).
        assert!(!names.contains(&"node_modules"));

        // DIRECTORY-only gitignore rule: ignored *directories* stay hidden …
        assert!(
            !names.contains(&"ignored"),
            "gitignored dir leaked into tree"
        );
        // … but ignored *files* are SHOWN. The headline case: a gitignored `.env`
        // (and other gitignored files like secret.txt / *.log) must appear while
        // browsing, even though the search index legitimately omits them.
        assert!(
            names.contains(&".env"),
            "gitignored .env must show in the tree by default"
        );
        assert!(
            names.contains(&"secret.txt"),
            "gitignored file should show (files are never filtered)"
        );
        assert!(
            names.contains(&"debug.log"),
            "gitignored *.log file should show (files are never filtered)"
        );
        // Tracked files are still listed.
        assert!(names.contains(&"README.md"));
        assert!(names.contains(&"package.json"));
        // .git is never browsable — pruned regardless.
        assert!(
            !names.contains(&".git"),
            ".git must never appear in the tree"
        );

        // Dirs come before files: the first non-pruned entry should be a dir.
        let first_dir_idx = entries.iter().position(|e| e.is_dir);
        let first_file_idx = entries.iter().position(|e| !e.is_dir);
        if let (Some(d), Some(f)) = (first_dir_idx, first_file_idx) {
            assert!(d < f, "directories should sort before files");
        }

        // src must be a directory with size 0.
        let src = entries.iter().find(|e| e.name == "src").unwrap();
        assert!(src.is_dir);
        assert_eq!(src.size, 0);
        // The .env we added back via the raw pass must report a real (non-dir)
        // size, not 0 — i.e. it's classified as a file.
        let env = entries.iter().find(|e| e.name == ".env").unwrap();
        assert!(!env.is_dir);
        assert!(env.size > 0, "added-back .env should carry its byte size");

        cleanup(&root);
    }

    #[test]
    fn list_dir_show_ignored_reveals_ignored_dirs() {
        let root = make_fixture();
        // With show_ignored = true, ignored DIRECTORIES come back too: the
        // gitignored `ignored/` dir and the pruned `node_modules` both appear,
        // alongside the files. Only `.git` stays hidden.
        let entries = read_dir_shallow(&root, true).unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();

        assert!(
            names.contains(&"ignored"),
            "show_ignored should reveal ignored dirs"
        );
        assert!(
            names.contains(&"node_modules"),
            "show_ignored should reveal node_modules"
        );
        assert!(
            names.contains(&".env"),
            "show_ignored still shows ignored files"
        );
        assert!(names.contains(&"src"));
        assert!(names.contains(&"README.md"));
        // .git is never browsable, even with show_ignored on.
        assert!(
            !names.contains(&".git"),
            ".git stays hidden even with show_ignored"
        );

        cleanup(&root);
    }

    #[test]
    fn read_text_file_reads_and_reports_ext() {
        let root = make_fixture();
        let readme = root.join("README.md");
        let contents = read_text_capped(&readme).unwrap();
        assert_eq!(contents.ext, "md");
        assert!(contents.text.contains("# Title"));
        assert!(!contents.truncated);
        cleanup(&root);
    }

    #[test]
    fn read_text_file_rejects_binary() {
        let root = make_fixture();
        let blob = root.join("blob.bin");
        let err = read_text_capped(&blob).unwrap_err();
        assert!(err.contains("not a text file"), "got: {err}");
        cleanup(&root);
    }

    #[test]
    fn read_text_file_truncates_oversize() {
        let root = make_fixture();
        let big = root.join("big.txt");
        // Write just over the cap of printable ASCII (no NULs → counts as text).
        let chunk = "a".repeat(1024);
        let mut f = fs::File::create(&big).unwrap();
        let mut written = 0u64;
        while written <= MAX_READ_BYTES {
            f.write_all(chunk.as_bytes()).unwrap();
            written += chunk.len() as u64;
        }
        drop(f);

        let contents = read_text_capped(&big).unwrap();
        assert!(
            contents.truncated,
            "oversize file should be marked truncated"
        );
        assert!(
            contents.text.len() as u64 <= MAX_READ_BYTES,
            "returned text exceeds cap"
        );
        cleanup(&root);
    }

    /// Evidence harness (ignored by default): index this very repo and print a
    /// few real search results. Run with:
    ///   cargo test --manifest-path src-tauri/Cargo.toml files::tests::evidence_index_this_repo -- --ignored --nocapture
    #[test]
    #[ignore]
    fn evidence_index_this_repo() {
        // src-tauri/ is CARGO_MANIFEST_DIR; the repo root is its parent.
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let repo_root = manifest.parent().unwrap().to_path_buf();
        let index = build_index(&repo_root, None).unwrap();
        println!(
            "\n=== indexed {} : {} files ===",
            repo_root.display(),
            index.entries.len()
        );
        // Sanity: node_modules / target / .git must not be present.
        let leaked: Vec<_> = index
            .entries
            .iter()
            .filter(|e| {
                e.rel_path.contains("node_modules")
                    || e.rel_path.starts_with(".git/")
                    || e.rel_path.contains("/target/")
                    || e.rel_path.starts_with("target/")
            })
            .collect();
        assert!(leaked.is_empty(), "ignored dirs leaked: {leaked:?}");

        for q in ["files", "ipc", "lib.rs", ".rs", "term"] {
            let hits = search_index(&index, q, 5);
            println!("query {:?} -> {} hits:", q, hits.len());
            for h in &hits {
                println!("    {:>5}  {}", h.score, h.rel_path);
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn to_host_path_is_identity_on_unix() {
        // On unix a native POSIX path is already the Linux path: no rewrite.
        assert_eq!(
            to_host_path("/home/natkins/proj"),
            PathBuf::from("/home/natkins/proj")
        );
        assert_eq!(to_host_path("relative/dir"), PathBuf::from("relative/dir"));
    }

    #[cfg(windows)]
    #[test]
    fn to_host_path_maps_wsl_to_unc_on_windows() {
        std::env::set_var("T_HUB_DISTRO", "Ubuntu-24.04");
        // A POSIX-absolute WSL path is mapped onto the \\wsl.localhost\ share.
        assert_eq!(
            to_host_path("/home/natkins/proj"),
            PathBuf::from("\\\\wsl.localhost\\Ubuntu-24.04\\home\\natkins\\proj"),
        );
        // Already-Windows paths pass through untouched.
        assert_eq!(
            to_host_path("C:\\Users\\natha"),
            PathBuf::from("C:\\Users\\natha")
        );
        assert_eq!(
            to_host_path("\\\\wsl.localhost\\Ubuntu-24.04\\home\\x"),
            PathBuf::from("\\\\wsl.localhost\\Ubuntu-24.04\\home\\x"),
        );
    }

    #[test]
    fn build_index_errors_on_nonexistent_root() {
        let mut root = std::env::temp_dir();
        root.push(format!("t-hub-does-not-exist-{}", uuid::Uuid::new_v4()));
        let err = build_index(&root, None).unwrap_err();
        assert!(err.contains("not a directory"), "got: {err}");
    }
}
