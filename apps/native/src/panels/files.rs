//! Files panel state (T11): lazy directory tree + fuzzy search + read-only
//! viewer, backed entirely by the read-tier socket commands `list_dir`,
//! `index_project`, `search_files` and `open_file`/`read_text_file`.
//!
//! HARD CONSTRAINT (§3 T11): arbitrary-path read/WRITE stays M4-gated
//! server-side. This panel only consumes the read surface that already exists;
//! there is no edit/save path (the webview's `write_text_file` is Tauri-only
//! and stays that way).
//!
//! gpui-free: reducers + view-models only; `panels::view` paints them.

use std::collections::{HashMap, HashSet};

use serde::Deserialize;

/// Webview parity: search keystrokes debounce 120ms before hitting the socket.
pub const SEARCH_DEBOUNCE_MS: u64 = 120;
/// Webview parity: ranked search results are capped at 50.
pub const SEARCH_LIMIT: u64 = 50;
/// The viewer renders at most this many lines (a 2 MiB `read_text_file` cap can
/// still be tens of thousands of lines; the view rebuilds its element tree per
/// frame, so the line count has to stay sane).
pub const VIEWER_MAX_LINES: usize = 2000;

// ---------------------------------------------------------------------------
// Wire payloads (all camelCase per the server's serde rename_all)
// ---------------------------------------------------------------------------

/// One `list_dir` entry.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DirEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    #[serde(default)]
    pub size: u64,
}

/// One `search_files` hit.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FileHit {
    pub rel_path: String,
    pub basename: String,
    #[serde(default)]
    pub ext: String,
    #[serde(default)]
    pub is_key_file: bool,
    #[serde(default)]
    pub score: i64,
}

/// The `search_files` response envelope.
#[derive(Debug, Clone, Deserialize)]
pub struct SearchResponse {
    pub root: String,
    pub query: String,
    pub hits: Vec<FileHit>,
}

/// The `index_project` response.
#[derive(Debug, Clone, Deserialize)]
pub struct IndexSummary {
    pub root: String,
    pub count: u64,
}

/// The `open_file` / `read_text_file` response.
#[derive(Debug, Clone, Deserialize)]
pub struct FileContents {
    pub path: String,
    #[serde(default)]
    pub ext: String,
    pub text: String,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub size: u64,
}

/// The `git_info` response (branch header parity with the webview Files panel).
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GitInfo {
    #[serde(default)]
    pub is_repo: bool,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub worktree_root: Option<String>,
    #[serde(default)]
    pub is_linked_worktree: bool,
    #[serde(default)]
    pub dirty_count: u32,
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// One fetched (or in-flight) directory listing.
#[derive(Debug, Default, Clone)]
pub struct DirNode {
    pub entries: Option<Vec<DirEntry>>,
    pub loading: bool,
    pub error: Option<String>,
}

/// The open-file viewer.
#[derive(Debug, Clone)]
pub struct Viewer {
    pub path: String,
    pub loading: bool,
    pub error: Option<String>,
    pub lines: Vec<String>,
    /// Server-side 2 MiB truncation flag.
    pub truncated: bool,
    /// Render-side line cap kicked in (`VIEWER_MAX_LINES`).
    pub clipped: bool,
    pub size: u64,
}

/// A directory fetch the feed should issue (`list_dir` on `path`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchDir {
    pub path: String,
    pub show_ignored: bool,
}

/// The Files panel state machine. Every mutation is a plain reducer; the feed
/// owns the socket I/O and the view paints `tree_rows()` / `hit_rows()`.
#[derive(Debug, Default)]
pub struct FilesState {
    /// The project root the tree + search operate on.
    pub root: Option<String>,
    dirs: HashMap<String, DirNode>,
    expanded: HashSet<String>,
    /// Hide dot-prefixed entries client-side (webview `hideDotfiles`, default on).
    pub hide_dotfiles: bool,
    /// Passed through to `list_dir` (server hides gitignored DIRS when false;
    /// ignored FILES are always listed - server behavior).
    pub show_ignored: bool,

    pub query: String,
    query_seq: u64,
    issued_seq: u64,
    edited_at_ms: u64,
    /// `index_project` has completed for this root.
    pub indexed: bool,
    pub index_count: Option<u64>,
    pub searching: bool,
    pub hits: Vec<FileHit>,
    hits_seq: u64,

    pub viewer: Option<Viewer>,
    pub git: Option<GitInfo>,
    pub error: Option<String>,
}

impl FilesState {
    pub fn new() -> Self {
        Self { hide_dotfiles: true, ..Self::default() }
    }

    /// Whether Escape still has a panel-internal target (the viewer, then the
    /// query - the Files tab's Esc stack). When false, a host embedding the
    /// panels (N5 cockpit) should let Esc escalate out - e.g. hand keyboard
    /// focus back to the tiles so the next Esc can restore a fullscreen tile.
    pub fn wants_escape(&self) -> bool {
        self.viewer.is_some() || !self.query.is_empty()
    }

    /// Point the panel at a project root, resetting tree/search/viewer state.
    /// Returns the root fetch to issue (or None if unchanged).
    pub fn set_root(&mut self, root: &str) -> Option<FetchDir> {
        if self.root.as_deref() == Some(root) {
            return None;
        }
        let show_ignored = self.show_ignored;
        let hide_dotfiles = self.hide_dotfiles;
        *self = Self {
            root: Some(root.to_string()),
            hide_dotfiles,
            show_ignored,
            ..Self::default()
        };
        self.request_dir(root)
            .map(|path| FetchDir { path, show_ignored })
    }

    /// Expand/collapse a directory. Expanding an unfetched dir returns the
    /// fetch the feed should run.
    pub fn toggle_dir(&mut self, path: &str) -> Option<FetchDir> {
        if self.expanded.contains(path) {
            self.expanded.remove(path);
            return None;
        }
        self.expanded.insert(path.to_string());
        self.request_dir(path).map(|path| FetchDir { path, show_ignored: self.show_ignored })
    }

    /// Mark `path` loading if it needs a fetch; returns it when a fetch is due.
    fn request_dir(&mut self, path: &str) -> Option<String> {
        let node = self.dirs.entry(path.to_string()).or_default();
        if node.entries.is_some() || node.loading {
            return None;
        }
        node.loading = true;
        node.error = None;
        Some(path.to_string())
    }

    /// Fold a `list_dir` result.
    pub fn fold_dir(&mut self, path: &str, result: Result<Vec<DirEntry>, String>) {
        let node = self.dirs.entry(path.to_string()).or_default();
        node.loading = false;
        match result {
            Ok(entries) => node.entries = Some(entries),
            Err(e) => node.error = Some(e),
        }
    }

    /// Drop every cached listing and refetch what is visible (root + expanded).
    pub fn refresh(&mut self) -> Vec<FetchDir> {
        self.dirs.clear();
        let mut out = Vec::new();
        let mut wanted: Vec<String> = self.root.iter().cloned().collect();
        wanted.extend(self.expanded.iter().cloned());
        for path in wanted {
            if let Some(path) = self.request_dir(&path) {
                out.push(FetchDir { path, show_ignored: self.show_ignored });
            }
        }
        out
    }

    /// Flip the show-ignored toggle: server-side filter, so refetch everything.
    pub fn set_show_ignored(&mut self, show: bool) -> Vec<FetchDir> {
        if self.show_ignored == show {
            return Vec::new();
        }
        self.show_ignored = show;
        self.refresh()
    }

    // -- search ------------------------------------------------------------

    /// Record a search-box edit (debounced; see [`take_due_search`]).
    pub fn set_query(&mut self, query: &str, now_ms: u64) {
        if self.query == query {
            return;
        }
        self.query = query.to_string();
        self.query_seq += 1;
        self.edited_at_ms = now_ms;
        if query.is_empty() {
            self.hits.clear();
            self.searching = false;
            // An empty query issues no search; fast-forward past this seq.
            self.issued_seq = self.query_seq;
        }
    }

    /// The debounced search to issue now, if any: `(seq, query)`. Marks the
    /// seq issued, so each edit triggers at most one socket search.
    pub fn take_due_search(&mut self, now_ms: u64) -> Option<(u64, String)> {
        if self.query.is_empty()
            || self.query_seq == self.issued_seq
            || now_ms.saturating_sub(self.edited_at_ms) < SEARCH_DEBOUNCE_MS
        {
            return None;
        }
        self.issued_seq = self.query_seq;
        self.searching = true;
        Some((self.query_seq, self.query.clone()))
    }

    /// Fold an `index_project` summary.
    pub fn fold_index(&mut self, summary: IndexSummary) {
        if self.root.as_deref() == Some(summary.root.as_str()) || self.root.is_none() {
            self.indexed = true;
            self.index_count = Some(summary.count);
        }
    }

    /// Fold a `search_files` response; stale sequences are dropped.
    pub fn fold_hits(&mut self, seq: u64, result: Result<SearchResponse, String>) {
        if seq < self.hits_seq {
            return;
        }
        if seq == self.issued_seq {
            self.searching = false;
        }
        match result {
            Ok(resp) => {
                self.hits_seq = seq;
                self.hits = resp.hits;
                self.error = None;
            }
            Err(e) => self.error = Some(e),
        }
    }

    // -- viewer ------------------------------------------------------------

    /// Open a file in the read-only viewer; returns the path to fetch via
    /// `open_file` (None if this exact file is already open/loading).
    pub fn open(&mut self, path: &str) -> Option<String> {
        if let Some(v) = &self.viewer {
            if v.path == path && (v.loading || v.error.is_none()) {
                return None;
            }
        }
        self.viewer = Some(Viewer {
            path: path.to_string(),
            loading: true,
            error: None,
            lines: Vec::new(),
            truncated: false,
            clipped: false,
            size: 0,
        });
        Some(path.to_string())
    }

    pub fn close_viewer(&mut self) {
        self.viewer = None;
    }

    /// Fold an `open_file` result into the viewer.
    pub fn fold_file(&mut self, path: &str, result: Result<FileContents, String>) {
        let Some(v) = self.viewer.as_mut() else { return };
        if v.path != path {
            return; // superseded by a newer open
        }
        v.loading = false;
        match result {
            Ok(fc) => {
                let mut lines: Vec<String> = fc.text.lines().map(|l| l.to_string()).collect();
                v.clipped = lines.len() > VIEWER_MAX_LINES;
                lines.truncate(VIEWER_MAX_LINES);
                v.lines = lines;
                v.truncated = fc.truncated;
                v.size = fc.size;
            }
            Err(e) => v.error = Some(e),
        }
    }

    pub fn fold_git(&mut self, info: GitInfo) {
        self.git = Some(info);
    }

    // -- view models ---------------------------------------------------------

    /// Flatten the expanded tree into rows for painting (DFS, server order:
    /// dirs first then files, both alphabetical).
    pub fn tree_rows(&self) -> Vec<TreeRow> {
        let mut rows = Vec::new();
        let Some(root) = &self.root else { return rows };
        self.push_children(root, 0, &mut rows);
        rows
    }

    fn push_children(&self, dir: &str, depth: usize, rows: &mut Vec<TreeRow>) {
        let Some(node) = self.dirs.get(dir) else { return };
        if node.loading {
            rows.push(TreeRow::note(depth, "loading..."));
            return;
        }
        if let Some(err) = &node.error {
            rows.push(TreeRow::note(depth, err));
            return;
        }
        let Some(entries) = &node.entries else { return };
        if entries.is_empty() {
            rows.push(TreeRow::note(depth, "(empty)"));
            return;
        }
        for e in entries {
            if self.hide_dotfiles && e.name.starts_with('.') {
                continue;
            }
            let expanded = e.is_dir && self.expanded.contains(&e.path);
            rows.push(TreeRow {
                path: e.path.clone(),
                name: e.name.clone(),
                depth,
                is_dir: e.is_dir,
                expanded,
                note: None,
            });
            if expanded {
                self.push_children(&e.path, depth + 1, rows);
            }
        }
    }

    /// Ranked search hits with match-highlight spans (server order preserved -
    /// it already sorts by score desc, then shorter path, then lexical).
    pub fn hit_rows(&self) -> Vec<HitRow> {
        self.hits
            .iter()
            .map(|h| HitRow {
                rel_path: h.rel_path.clone(),
                is_key_file: h.is_key_file,
                spans: highlight_spans(&h.rel_path, &self.query),
            })
            .collect()
    }
}

/// One painted tree row. `note` rows are inline placeholders (loading/error/
/// empty) indented under their parent.
#[derive(Debug, Clone, PartialEq)]
pub struct TreeRow {
    pub path: String,
    pub name: String,
    pub depth: usize,
    pub is_dir: bool,
    pub expanded: bool,
    pub note: Option<String>,
}

impl TreeRow {
    fn note(depth: usize, msg: &str) -> Self {
        TreeRow {
            path: String::new(),
            name: String::new(),
            depth,
            is_dir: false,
            expanded: false,
            note: Some(msg.to_string()),
        }
    }
}

/// One painted search hit: the relPath plus which char ranges matched.
#[derive(Debug, Clone, PartialEq)]
pub struct HitRow {
    pub rel_path: String,
    pub is_key_file: bool,
    /// Byte ranges of `rel_path` to paint highlighted, non-overlapping,
    /// ascending.
    pub spans: Vec<(usize, usize)>,
}

/// Greedy case-insensitive subsequence match of `query` inside `text`,
/// mirroring the server's subsequence semantics for DISPLAY (the authoritative
/// ranking already happened server-side). Returns coalesced byte ranges;
/// empty when the query does not subsequence-match.
pub fn highlight_spans(text: &str, query: &str) -> Vec<(usize, usize)> {
    if query.is_empty() {
        return Vec::new();
    }
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut qchars = query.chars().filter(|c| !c.is_whitespace()).map(|c| c.to_ascii_lowercase());
    let Some(mut want) = qchars.next() else { return Vec::new() };
    for (ix, ch) in text.char_indices() {
        if ch.to_ascii_lowercase() == want {
            let end = ix + ch.len_utf8();
            match spans.last_mut() {
                Some(last) if last.1 == ix => last.1 = end,
                _ => spans.push((ix, end)),
            }
            match qchars.next() {
                Some(next) => want = next,
                None => return spans,
            }
        }
    }
    Vec::new() // ran out of text before the query was consumed: no match
}

/// Split `text` into `(segment, highlighted)` runs from byte-range `spans`
/// (as produced by [`highlight_spans`]: non-overlapping, ascending).
pub fn segment_text(text: &str, spans: &[(usize, usize)]) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    let mut at = 0;
    for &(s, e) in spans {
        if s > at {
            out.push((text[at..s].to_string(), false));
        }
        out.push((text[s..e].to_string(), true));
        at = e;
    }
    if at < text.len() {
        out.push((text[at..].to_string(), false));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, path: &str, is_dir: bool) -> DirEntry {
        DirEntry { name: name.into(), path: path.into(), is_dir, size: 0 }
    }

    #[test]
    fn wants_escape_follows_the_viewer_then_query_stack() {
        let mut st = FilesState::new();
        st.set_root("/proj");
        assert!(!st.wants_escape(), "idle Files tab: Esc escalates to the host");
        st.set_query("cargo", 1000);
        assert!(st.wants_escape(), "a live query is an Esc target");
        st.open("/proj/README.md");
        assert!(st.wants_escape(), "an open viewer is an Esc target");
        st.close_viewer();
        st.set_query("", 1100);
        assert!(!st.wants_escape(), "cleared: Esc escalates again");
    }

    #[test]
    fn set_root_requests_the_root_listing_once() {
        let mut st = FilesState::new();
        let fetch = st.set_root("/proj").expect("first set_root fetches");
        assert_eq!(fetch.path, "/proj");
        assert!(st.set_root("/proj").is_none(), "same root: no refetch");
        assert!(st.set_root("/other").is_some(), "new root resets + fetches");
    }

    #[test]
    fn tree_flattens_expanded_dirs_and_filters_dotfiles() {
        let mut st = FilesState::new();
        st.set_root("/p");
        st.fold_dir(
            "/p",
            Ok(vec![
                entry("src", "/p/src", true),
                entry(".git", "/p/.git", true),
                entry(".env", "/p/.env", false),
                entry("main.rs", "/p/main.rs", false),
            ]),
        );
        let names: Vec<String> = st.tree_rows().into_iter().map(|r| r.name).collect();
        assert_eq!(names, vec!["src", "main.rs"], "dotfiles hidden by default");

        st.hide_dotfiles = false;
        let names: Vec<String> = st.tree_rows().into_iter().map(|r| r.name).collect();
        assert_eq!(names, vec!["src", ".git", ".env", "main.rs"]);

        // Expanding src requests its listing, then nests its children.
        let fetch = st.toggle_dir("/p/src").expect("unfetched dir needs a fetch");
        assert_eq!(fetch.path, "/p/src");
        st.fold_dir("/p/src", Ok(vec![entry("lib.rs", "/p/src/lib.rs", false)]));
        let rows = st.tree_rows();
        let lib = rows.iter().find(|r| r.name == "lib.rs").expect("nested child");
        assert_eq!(lib.depth, 1);
        // Collapse: no fetch, child gone, cache kept (re-expand is instant).
        assert!(st.toggle_dir("/p/src").is_none());
        assert!(!st.tree_rows().iter().any(|r| r.name == "lib.rs"));
        assert!(st.toggle_dir("/p/src").is_none(), "cached: re-expand needs no fetch");
    }

    #[test]
    fn dir_errors_and_loading_render_as_note_rows() {
        let mut st = FilesState::new();
        st.set_root("/p");
        assert_eq!(st.tree_rows()[0].note.as_deref(), Some("loading..."));
        st.fold_dir("/p", Err("permission denied".into()));
        assert_eq!(st.tree_rows()[0].note.as_deref(), Some("permission denied"));
    }

    #[test]
    fn search_debounces_and_drops_stale_responses() {
        let mut st = FilesState::new();
        st.set_root("/p");
        st.set_query("ca", 1000);
        assert!(st.take_due_search(1050).is_none(), "within the 120ms debounce");
        st.set_query("cargo", 1100);
        let (seq, q) = st.take_due_search(1300).expect("debounce elapsed");
        assert_eq!(q, "cargo");
        assert!(st.take_due_search(1400).is_none(), "issued once per edit");

        // A stale response (from the earlier "ca" seq) must not clobber.
        let hit = FileHit {
            rel_path: "Cargo.toml".into(),
            basename: "Cargo.toml".into(),
            ext: "toml".into(),
            is_key_file: true,
            score: 100,
        };
        st.fold_hits(
            seq,
            Ok(SearchResponse { root: "/p".into(), query: q, hits: vec![hit] }),
        );
        assert_eq!(st.hits.len(), 1);
        assert!(!st.searching);
        st.fold_hits(
            seq - 1,
            Ok(SearchResponse { root: "/p".into(), query: "ca".into(), hits: vec![] }),
        );
        assert_eq!(st.hits.len(), 1, "stale response dropped");
    }

    #[test]
    fn clearing_the_query_clears_hits_without_a_search() {
        let mut st = FilesState::new();
        st.set_query("x", 0);
        let (seq, _) = st.take_due_search(500).unwrap();
        st.fold_hits(
            seq,
            Ok(SearchResponse {
                root: "/".into(),
                query: "x".into(),
                hits: vec![FileHit {
                    rel_path: "x.rs".into(),
                    basename: "x.rs".into(),
                    ext: "rs".into(),
                    is_key_file: false,
                    score: 1,
                }],
            }),
        );
        st.set_query("", 600);
        assert!(st.hits.is_empty());
        assert!(st.take_due_search(10_000).is_none());
    }

    #[test]
    fn highlight_spans_subsequence_semantics() {
        // Contiguous match coalesces into one span.
        assert_eq!(highlight_spans("Cargo.toml", "cargo"), vec![(0, 5)]);
        // Subsequence across separators produces multiple spans.
        let spans = highlight_spans("src/main.rs", "smr");
        assert_eq!(spans, vec![(0, 1), (4, 5), (9, 10)]);
        // No match -> empty (never a partial highlight).
        assert!(highlight_spans("readme.md", "xyz").is_empty());
        // Whitespace in the query is ignored (webview typing habits).
        assert_eq!(highlight_spans("Cargo.toml", "car go"), vec![(0, 5)]);
        // Case-insensitive.
        assert_eq!(highlight_spans("README.md", "readme"), vec![(0, 6)]);
    }

    #[test]
    fn viewer_folds_clip_and_supersede() {
        let mut st = FilesState::new();
        let p = st.open("/p/a.txt").expect("fetch issued");
        assert_eq!(p, "/p/a.txt");
        assert!(st.open("/p/a.txt").is_none(), "same file already loading");
        // A newer open supersedes; the stale fold is ignored.
        assert_eq!(st.open("/p/b.txt").as_deref(), Some("/p/b.txt"));
        st.fold_file(
            "/p/a.txt",
            Ok(FileContents {
                path: "/p/a.txt".into(),
                ext: "txt".into(),
                text: "old".into(),
                truncated: false,
                size: 3,
            }),
        );
        assert!(st.viewer.as_ref().unwrap().loading, "stale fold ignored");
        let big = vec!["l"; VIEWER_MAX_LINES + 10].join("\n");
        st.fold_file(
            "/p/b.txt",
            Ok(FileContents {
                path: "/p/b.txt".into(),
                ext: "txt".into(),
                text: big,
                truncated: true,
                size: 999,
            }),
        );
        let v = st.viewer.as_ref().unwrap();
        assert!(!v.loading);
        assert!(v.truncated, "server cap surfaced");
        assert!(v.clipped, "render cap surfaced");
        assert_eq!(v.lines.len(), VIEWER_MAX_LINES);
    }

    #[test]
    fn segment_text_splits_around_spans() {
        let spans = highlight_spans("src/main.rs", "smr");
        let segs = segment_text("src/main.rs", &spans);
        let flat: String = segs.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(flat, "src/main.rs", "segments reassemble the text");
        assert_eq!(
            segs,
            vec![
                ("s".to_string(), true),
                ("rc/".to_string(), false),
                ("m".to_string(), true),
                ("ain.".to_string(), false),
                ("r".to_string(), true),
                ("s".to_string(), false),
            ]
        );
        assert_eq!(segment_text("abc", &[]), vec![("abc".to_string(), false)]);
    }

    #[test]
    fn refresh_refetches_root_and_expanded_only() {
        let mut st = FilesState::new();
        st.set_root("/p");
        st.fold_dir("/p", Ok(vec![entry("src", "/p/src", true), entry("t", "/p/t", true)]));
        st.toggle_dir("/p/src");
        st.fold_dir("/p/src", Ok(vec![]));
        let mut fetched: Vec<String> = st.refresh().into_iter().map(|f| f.path).collect();
        fetched.sort();
        assert_eq!(fetched, vec!["/p", "/p/src"], "collapsed /p/t not refetched");
    }
}
