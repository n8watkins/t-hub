import { describe, it, expect } from "vitest";
import {
  posixDirname,
  posixBasename,
  sanitizeBranchToDir,
} from "./worktreeTarget";

// NOTE: only the PURE helpers are tested here. `resolveWorktreeTarget` calls the
// gitWorktreeList IPC and is intentionally skipped this batch (would need an
// invoke mock). Importing this module is still safe under vitest: the IPC import
// (@tauri-apps/api/core) has no module-load side effects and these helpers never
// touch it.

describe("posixDirname", () => {
  it("returns the parent directory of a nested path", () => {
    expect(posixDirname("/a/b/c")).toBe("/a/b");
  });

  it("ignores a trailing slash", () => {
    expect(posixDirname("/a/b/")).toBe("/a");
  });

  it("collapses multiple trailing slashes", () => {
    expect(posixDirname("/a/b///")).toBe("/a");
  });

  it("yields '' for a root-level path (parent is the fs root)", () => {
    expect(posixDirname("/foo")).toBe("");
  });

  it("yields '' for a bare root", () => {
    expect(posixDirname("/")).toBe("");
  });

  it("yields '' for a relative path with no slash", () => {
    expect(posixDirname("foo")).toBe("");
  });

  it("handles a deep absolute repo path", () => {
    expect(posixDirname("/home/me/projects/repo")).toBe("/home/me/projects");
  });
});

describe("posixBasename", () => {
  it("returns the final component of a nested path", () => {
    expect(posixBasename("/a/b/c")).toBe("c");
  });

  it("ignores a trailing slash", () => {
    expect(posixBasename("/a/b/")).toBe("b");
  });

  it("collapses multiple trailing slashes", () => {
    expect(posixBasename("/a/b///")).toBe("b");
  });

  it("yields '' for a bare root", () => {
    expect(posixBasename("/")).toBe("");
  });

  it("yields '' for an empty string", () => {
    expect(posixBasename("")).toBe("");
  });

  it("returns the whole string when there is no slash", () => {
    expect(posixBasename("repo")).toBe("repo");
  });

  it("returns the repo name from an absolute path", () => {
    expect(posixBasename("/home/me/projects/repo")).toBe("repo");
  });
});

describe("posixDirname + posixBasename compose to the original path", () => {
  it("dirname + '/' + basename reconstructs a nested path", () => {
    const p = "/home/me/projects/repo";
    expect(`${posixDirname(p)}/${posixBasename(p)}`).toBe(p);
  });
});

describe("sanitizeBranchToDir", () => {
  it("flattens a slash into a hyphen (feat/x -> feat-x)", () => {
    expect(sanitizeBranchToDir("feat/x")).toBe("feat-x");
  });

  it("keeps allowed chars [A-Za-z0-9._-] intact", () => {
    expect(sanitizeBranchToDir("v1.2.3_rc-final")).toBe("v1.2.3_rc-final");
  });

  it("strips leading and trailing slashes", () => {
    expect(sanitizeBranchToDir("/a/b/")).toBe("a-b");
  });

  it("trims surrounding whitespace and inner slashes (' /a//b/ ' -> a-b)", () => {
    expect(sanitizeBranchToDir("  /a//b/  ")).toBe("a-b");
  });

  it("collapses runs of disallowed chars into a single hyphen", () => {
    expect(sanitizeBranchToDir("a@@@b")).toBe("a-b");
  });

  it("trims stray leading/trailing hyphens produced by sanitizing", () => {
    expect(sanitizeBranchToDir("!!!a!!!")).toBe("a");
  });

  it("falls back to 'work' when nothing usable remains", () => {
    expect(sanitizeBranchToDir("!!!")).toBe("work");
    expect(sanitizeBranchToDir("")).toBe("work");
    expect(sanitizeBranchToDir("   ")).toBe("work");
    expect(sanitizeBranchToDir("///")).toBe("work");
  });

  it("DOCUMENTED collision: 'feat/x' and 'feat-x' both sanitize to 'feat-x'", () => {
    // The slash->hyphen flattening is many-to-one: distinct git branches can map
    // to the SAME directory component. Callers anchor the real branch separately
    // (sanitizeBranchToDir only names the on-disk folder), so this is a known,
    // accepted property — pinned here so a future change to the rule is noticed.
    expect(sanitizeBranchToDir("feat/x")).toBe("feat-x");
    expect(sanitizeBranchToDir("feat-x")).toBe("feat-x");
    expect(sanitizeBranchToDir("feat/x")).toBe(sanitizeBranchToDir("feat-x"));
  });
});
