// adopt-harden regression: per-tile render fault isolation. A single tile that
// throws during render must NOT tear down its siblings - the incident's blank-UI /
// zero-attach failure mode was one bad tile unwinding the whole pool. Here N
// healthy tiles render (and would attach) while M throwing tiles are each
// contained to their own inline error state.
import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen } from "@testing-library/react";
import { TileErrorBoundary } from "./TileErrorBoundary";

// The boundary logs to the diag sink (Tauri/file-backed); stub it web-safe.
vi.mock("../lib/diag", () => ({ tlog: () => {} }));

/** A stand-in tile: throws on render iff its id looks like debris (`ghost*`),
 *  mirroring a bad/dead/weird session whose tile fails to materialize. */
function Tile({ id }: { id: string }): JSX.Element {
  if (id.startsWith("ghost")) throw new Error(`render boom for ${id}`);
  return <div data-testid={`tile-${id}`}>ok {id}</div>;
}

describe("TileErrorBoundary", () => {
  afterEach(() => vi.restoreAllMocks());

  it("contains a throwing tile so its siblings still render (N healthy + M garbage -> N render)", () => {
    // React logs the caught error to console.error; silence it for a clean run.
    const spy = vi.spyOn(console, "error").mockImplementation(() => {});

    const ids = ["a", "ghost1", "b", "ghost2", "c"];
    render(
      <>
        {ids.map((id) => (
          <TileErrorBoundary key={id} terminalId={id}>
            <Tile id={id} />
          </TileErrorBoundary>
        ))}
      </>,
    );

    // Every healthy tile rendered - a garbage neighbor never blanked them.
    expect(screen.getByTestId("tile-a")).toBeTruthy();
    expect(screen.getByTestId("tile-b")).toBeTruthy();
    expect(screen.getByTestId("tile-c")).toBeTruthy();
    // The failing tiles are each contained to their own inline error state...
    expect(document.querySelectorAll("[data-th-tile-error]")).toHaveLength(2);
    // ...and never rendered their (throwing) body.
    expect(screen.queryByTestId("tile-ghost1")).toBeNull();
    expect(screen.queryByTestId("tile-ghost2")).toBeNull();

    spy.mockRestore();
  });

  it("renders the child untouched when nothing throws", () => {
    render(
      <TileErrorBoundary terminalId="a">
        <Tile id="a" />
      </TileErrorBoundary>,
    );
    expect(screen.getByTestId("tile-a")).toBeTruthy();
    expect(document.querySelector("[data-th-tile-error]")).toBeNull();
  });
});
