//! File- and git-related READ-tier control handlers, split out of
//! `control.rs` to shrink that module. These are thin delegators to
//! [`crate::files`] / [`crate::git`]; the parent dispatch match routes here.

use super::*;

/// `git_info` (server-split M3 overlay source): git awareness — branch / worktree
/// root / linked-worktree flag / dirty count — for a project cwd, so a thin client
/// gets the Files-panel git header remotely. Mirrors the `git_info` Tauri command
/// (same `GitInfo` shape), reusing its per-cwd TTL cache (the freeze fix). Args:
/// `path` (or `cwd`), the same cwd string the frontend passes.
pub(super) fn git_info(ctx: &ControlContext, args: &Value) -> Result<Value, String> {
    let cwd = arg_str(args, "path")
        .or_else(|| arg_str(args, "cwd"))
        .ok_or("git_info requires a 'path' (cwd) argument")?;
    // #27 follow-up: gate the peer-controlled cwd for a REMOTE peer to the operator
    // allowlist — else it leaks whether an arbitrary host path is a git repo + its
    // branch/dirty state. Loopback is unrestricted (scoped_create_path handles the
    // existing cwd; substitute the scoped path so check and use can't diverge).
    let cwd = if ctx.peer_is_loopback {
        cwd
    } else {
        files::scoped_create_path(&cwd, true, files::remote_file_roots())?
            .to_string_lossy()
            .into_owned()
    };
    serde_json::to_value(crate::git::git_info_cached(&cwd)).map_err(|e| e.to_string())
}

/// `index_project` (server-split M3 — the file index, build half): walk `root`,
/// (re)build the control channel's file index, and return its `IndexSummary`
/// (`{root, count}`). Mirrors the `index_project` Tauri command (same shape), so
/// the frontend's warmup flips onto the wire and a thin client indexes the REMOTE
/// tree. Args: `root` (required). Paired with [`search_files`], which reuses the
/// cache this warms (and self-indexes on demand if skipped).
pub(super) fn index_project(ctx: &ControlContext, args: &Value) -> Result<Value, String> {
    let root = arg_str(args, "root").ok_or("index_project requires a 'root' argument")?;
    let summary = files::control_index(
        &ctx.files,
        &root,
        !ctx.peer_is_loopback,
        files::remote_file_roots(),
    )?;
    serde_json::to_value(summary).map_err(|e| e.to_string())
}

/// `search_files`: fuzzy basename/path/extension search over a project root,
/// using the control channel's own index cache. Args: `root` (required),
/// `query` (required), `limit` (optional, default 20).
pub(super) fn search_files(ctx: &ControlContext, args: &Value) -> Result<Value, String> {
    let root = arg_str(args, "root").ok_or("search_files requires a 'root' argument")?;
    let query = arg_str(args, "query").unwrap_or_default();
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
        .unwrap_or(20)
        .clamp(1, 1000);
    let hits = files::control_search(
        &ctx.files,
        &root,
        &query,
        limit,
        !ctx.peer_is_loopback,
        files::remote_file_roots(),
    )?;
    Ok(json!({ "root": root, "query": query, "hits": hits }))
}

/// `list_dir` (server-split #23 — the Files-panel TREE over the socket): a shallow
/// directory listing (dirs first, the directory-only gitignore rule). Mirrors the
/// `list_dir` Tauri command (same `DirEntry[]` shape). A REMOTE peer is SCOPED to
/// indexed roots (`files::control_list_dir`); loopback is unrestricted. Args: `path`
/// (required), `showIgnored` (optional, default false).
pub(super) fn list_dir(ctx: &ControlContext, args: &Value) -> Result<Value, String> {
    let path = arg_str(args, "path").ok_or("list_dir requires a 'path' argument")?;
    let show_ignored = args
        .get("showIgnored")
        .or_else(|| args.get("show_ignored"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let entries = files::control_list_dir(
        &path,
        show_ignored,
        !ctx.peer_is_loopback,
        files::remote_file_roots(),
    )?;
    serde_json::to_value(entries).map_err(|e| e.to_string())
}

/// `read_text_file` (server-split #23 — the Files-panel READER over the socket): a
/// size-capped, binary-rejecting text read. Mirrors the `read_text_file` Tauri
/// command (same `FileContents` shape). A REMOTE peer is SCOPED to indexed roots;
/// loopback is unrestricted. WRITE stays in-process (deferred). Args: `path`.
pub(super) fn read_text_file(ctx: &ControlContext, args: &Value) -> Result<Value, String> {
    let path = arg_str(args, "path").ok_or("read_text_file requires a 'path' argument")?;
    let contents =
        files::control_read_text(&path, !ctx.peer_is_loopback, files::remote_file_roots())?;
    serde_json::to_value(contents).map_err(|e| e.to_string())
}

/// `open_file`: resolve + read a capped text file for the requested path. This is
/// the one Organization-tier action that has a real, side-effect-free backing
/// implementation today (the Files reader), so the MCP "open a file" tool returns
/// the file's contents/metadata. Args: `path` (required).
pub(super) fn open_file(
    ctx: &ControlContext,
    args: &Value,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
) -> Result<Value, String> {
    let path = arg_str(args, "path").ok_or("open_file requires a 'path' argument")?;
    enforce_project_path_authority(ctx, caller, trusted_internal, &path, "open_file")?;
    // Same file-read scope as the #23 reader: a REMOTE peer may only open files
    // under the operator allowlist; loopback (the local MCP) is unrestricted.
    let contents =
        files::control_read_text(&path, !ctx.peer_is_loopback, files::remote_file_roots())?;
    serde_json::to_value(contents).map_err(|e| e.to_string())
}

