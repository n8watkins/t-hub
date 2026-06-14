// Typed wrappers over the Files IPC surface (index + search + tree + reader).
// Kept separate from ./client (0.1 nucleus) and ./client05 (0.5 surface) so the
// file feature's contract lives in one place. Mirrors `CommandsFiles` and the
// payload types in ./types, which in turn mirror `src-tauri/src/files.rs`.

import { invoke } from "@tauri-apps/api/core";
import {
  CommandsFiles,
  type DirEntry,
  type FileContents,
  type FileHit,
  type IndexSummary,
} from "./types";

/** Walk `root`, (re)build the in-memory index, and return a summary. */
export function indexProject(root: string): Promise<IndexSummary> {
  return invoke(CommandsFiles.indexProject, { root });
}

/**
 * Fuzzy-search the index for `root`. Indexes on demand if `root` is not cached.
 * An empty `query` returns key files first, then a stable prefix of the index.
 */
export function searchFiles(
  root: string,
  query: string,
  limit = 50,
): Promise<FileHit[]> {
  return invoke(CommandsFiles.searchFiles, { root, query, limit });
}

/** Shallow directory listing for the tree (dirs first; no recursion). */
export function listDir(path: string): Promise<DirEntry[]> {
  return invoke(CommandsFiles.listDir, { path });
}

/** Read a text file for the reader (capped; rejects binary blobs). */
export function readTextFile(path: string): Promise<FileContents> {
  return invoke(CommandsFiles.readTextFile, { path });
}
