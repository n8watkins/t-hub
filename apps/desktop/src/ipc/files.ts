// Typed wrappers over the Files IPC surface (index + search + tree + reader).
// Kept separate from ./client (0.1 nucleus) and ./client05 (0.5 surface) so the
// file feature's contract lives in one place. Mirrors `CommandsFiles` and the
// payload types in ./types, which in turn mirror `src-tauri/src/files.rs`.

import { invoke } from "@tauri-apps/api/core";
import { controlRequest } from "./controlClient";
import {
  CommandsFiles,
  type DirEntry,
  type FileContents,
  type FileHit,
  type IndexSummary,
} from "./types";

/**
 * Walk `root`, (re)build the in-memory index, and return a summary.
 *
 * Server-split M3 (the file INDEX, build half): routed over the control socket
 * (`index_project` in control.rs) instead of the in-process Tauri command —
 * shape-identical (`IndexSummary`), so it's a transport swap. This warms the
 * daemon's index cache, which {@link searchFiles} (also on the socket) reuses; a
 * thin client thus indexes the REMOTE tree. NOTE: the file BROWSER/READER/EDITOR
 * (`listDir`/`readTextFile`/`writeTextFile`) stays on in-process `invoke` — those
 * read/write arbitrary paths, a security-sensitive surface to expose over the
 * (network-bindable, post-M2b) control channel, so they're deferred to the M4
 * hardening pass with proper peer-gating/path-scoping.
 */
export function indexProject(root: string): Promise<IndexSummary> {
  return controlRequest("index_project", { root }) as Promise<IndexSummary>;
}

/**
 * Fuzzy-search the index for `root`. Indexes on demand if `root` is not cached.
 * An empty `query` returns key files first, then a stable prefix of the index.
 *
 * Server-split M3 (the file INDEX, query half): routed over the control socket
 * (`search_files` in control.rs — already MCP-exposed, so no new surface) instead
 * of the in-process Tauri command. The dispatcher wraps the bare `FileHit[]` as
 * `{root, query, hits}`, so we unwrap `.hits` to keep this wrapper's contract.
 */
export async function searchFiles(
  root: string,
  query: string,
  limit = 50,
): Promise<FileHit[]> {
  const res = (await controlRequest("search_files", { root, query, limit })) as {
    hits: FileHit[];
  };
  return res.hits;
}

/**
 * Shallow directory listing for the tree (dirs first; no recursion).
 *
 * `showIgnored` (default false) toggles the backend's directory-only gitignore
 * rule: false hides ignored DIRECTORIES (`node_modules`, `dist`, …) while always
 * showing ignored FILES (`.env`, `.env.local`, …); true lists everything except
 * `.git`. Maps to the `show_ignored` arg of the `list_dir` command.
 *
 * Server-split #23 (the Files TREE over the socket): routed via `controlRequest`
 * (`list_dir` in control.rs), shape-identical, so locally it's a transport swap.
 * A thin client browses the REMOTE tree — scoped to indexed roots on the daemon
 * side (loopback is unrestricted, so the local UX is unchanged).
 */
export function listDir(path: string, showIgnored = false): Promise<DirEntry[]> {
  return controlRequest("list_dir", { path, showIgnored }) as Promise<DirEntry[]>;
}

/**
 * Read a text file for the reader (capped; rejects binary blobs).
 *
 * Server-split #23 (the Files READER over the socket): routed via `controlRequest`
 * (`read_text_file` in control.rs), shape-identical. A thin client reads REMOTE
 * files — scoped to indexed roots on the daemon (loopback unrestricted).
 */
export function readTextFile(path: string): Promise<FileContents> {
  return controlRequest("read_text_file", { path }) as Promise<FileContents>;
}

/**
 * Overwrite a text file with new contents (the editor's save).
 *
 * Stays on in-process `invoke` (NOT yet over the socket): a remote WRITE to an
 * arbitrary path is the riskiest surface, deferred until the file-read scope above
 * is proven over a real two-device run (#19) and write-side gating is designed.
 */
export function writeTextFile(path: string, contents: string): Promise<void> {
  return invoke(CommandsFiles.writeTextFile, { path, contents });
}
