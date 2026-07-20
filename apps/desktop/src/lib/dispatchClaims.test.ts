import { describe, expect, it } from "vitest";

import { normalizeRepositoryResource, parseDispatchClaims } from "./dispatchClaims";

describe("normalizeRepositoryResource", () => {
  it("normalizes repository-relative paths and directory prefixes", () => {
    expect(normalizeRepositoryResource(" ./apps\\desktop//src/App.tsx ")).toBe(
      "apps/desktop/src/App.tsx",
    );
    expect(normalizeRepositoryResource("docs")).toBe("docs");
  });

  it("rejects absolute paths, traversal, globs, and empty claims", () => {
    for (const claim of ["/repo/src", "C:\\repo\\src", "src/../secret", "src/**/*.ts", "."]) {
      expect(() => normalizeRepositoryResource(claim)).toThrow();
    }
  });
});

describe("parseDispatchClaims", () => {
  it("returns normalized, deduplicated logical claims and ordered integration contracts", () => {
    expect(
      parseDispatchClaims({
        laneId: "lane.desktop",
        dependencies: "lane.backend\nlane.backend",
        mutableFiles: "apps/desktop/src\napps\\desktop\\src",
        mutableSchemas: "captains-v18",
        mutableInterfaces: "control.dispatch",
        integrationContracts: "desktop-order | integrator.1 | lane.backend, lane.desktop",
      }),
    ).toEqual({
      laneId: "lane.desktop",
      dependencies: ["lane.backend"],
      mutableFiles: ["apps/desktop/src"],
      mutableSchemas: ["captains-v18"],
      mutableInterfaces: ["control.dispatch"],
      integrationContracts: [
        {
          contractId: "desktop-order",
          integrationOwner: "integrator.1",
          orderedLaneIds: ["lane.backend", "lane.desktop"],
        },
      ],
    });
  });

  it("requires an explicit mutable resource claim", () => {
    expect(() =>
      parseDispatchClaims({
        laneId: "lane.empty",
        dependencies: "",
        mutableFiles: "",
        mutableSchemas: "",
        mutableInterfaces: "",
        integrationContracts: "",
      }),
    ).toThrow("Claim at least one mutable");
  });

  it("rejects self-dependencies and incomplete ordering contracts", () => {
    const base = {
      laneId: "lane.ui",
      dependencies: "lane.ui",
      mutableFiles: "apps/desktop/src",
      mutableSchemas: "",
      mutableInterfaces: "",
      integrationContracts: "",
    };
    expect(() => parseDispatchClaims(base)).toThrow("cannot depend on itself");
    expect(() =>
      parseDispatchClaims({
        ...base,
        dependencies: "lane.backend",
        integrationContracts: "order | integrator | lane.backend, lane.other",
      }),
    ).toThrow("must include this lane");
  });
});
