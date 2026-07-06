// Unit tests for the captain-chord keymap migration (captain-overlay fix
// round). A persisted keybindings blob fully replaces the shipped defaults,
// so coercePersisted seeds Ctrl+B C for pre-captain maps - but ONLY when the
// relocation has a landing spot: a binding is never deleted without one.
import { describe, it, expect } from "vitest";
import { coercePersisted } from "./keybindings";

/** A pre-captain persisted blob with the old shipped prefixed defaults. */
function preCaptainBlob(prefixed: Record<string, string>) {
  return { prefixKey: "ctrl+b", direct: {}, prefixed };
}

describe("captain keymap migration", () => {
  it("relocates newPlainWorkspace to s and seeds captain on c when s is free", () => {
    const out = coercePersisted(
      preCaptainBlob({ newPlainWorkspace: "c", newWorktreeWorkspace: "w" }),
    );
    expect(out.prefixed.toggleCaptainOverlay).toBe("c");
    expect(out.prefixed.newPlainWorkspace).toBe("s");
  });

  it("does NOT seed captain (and keeps newPlainWorkspace on c) when s is taken", () => {
    const out = coercePersisted(
      preCaptainBlob({ newPlainWorkspace: "c", openWorktreesList: "s" }),
    );
    // Never strand a binding: c stays with newPlainWorkspace, s untouched,
    // and the captain chord is simply not seeded (palette/anchor still work).
    expect(out.prefixed.newPlainWorkspace).toBe("c");
    expect(out.prefixed.openWorktreesList).toBe("s");
    expect("toggleCaptainOverlay" in out.prefixed).toBe(false);
  });

  it("leaves a custom owner of c untouched", () => {
    const out = coercePersisted(preCaptainBlob({ spawnTerminal: "c" }));
    expect(out.prefixed.spawnTerminal).toBe("c");
    expect("toggleCaptainOverlay" in out.prefixed).toBe(false);
  });

  it("respects an existing captain binding (no double migration)", () => {
    const out = coercePersisted(
      preCaptainBlob({ toggleCaptainOverlay: "a", newPlainWorkspace: "c" }),
    );
    expect(out.prefixed.toggleCaptainOverlay).toBe("a");
    expect(out.prefixed.newPlainWorkspace).toBe("c");
  });

  it("does not resurrect a deliberately cleared captain binding", () => {
    // Post-migration map where the user then unbound the captain: c is
    // unowned, newPlainWorkspace already on s - nothing to migrate.
    const out = coercePersisted(preCaptainBlob({ newPlainWorkspace: "s" }));
    expect("toggleCaptainOverlay" in out.prefixed).toBe(false);
    expect(out.prefixed.newPlainWorkspace).toBe("s");
  });
});
