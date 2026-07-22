import {
  expect,
  test,
  type Browser,
  type Page,
  type TestInfo,
} from "@playwright/test";

const TILE_SELECTOR = "[data-tile-id]";
const HEADER_SELECTOR = `${TILE_SELECTOR} .th-tile-header`;
const TAB_NAMES = ["Terminal", "Files", "Preview"];
const STATUS_SEQUENCE = ["working", "needsPermission", "completed", "failed"];
const mockedPages = new WeakSet<Page>();

type MeasuredItem = {
  kind: string;
  name: string;
  width: number;
  height: number;
  contained: boolean;
  control: boolean;
  left: number;
  right: number;
  top: number;
  bottom: number;
};

type HeaderMetrics = {
  tileId: string;
  width: number;
  labels: string[];
  tabNames: string[];
  items: MeasuredItem[];
  overlaps: string[][];
};

async function installGitMock(page: Page): Promise<void> {
  if (mockedPages.has(page)) return;
  mockedPages.add(page);
  await page.addInitScript(() => {
    let callbackId = 1;
    const host = window as typeof window & Record<string, unknown>;
    host.__TAURI_INTERNALS__ = {
      invoke: async (command: string, args?: Record<string, unknown>) => {
        if (command === "control_request") {
          if (args?.command === "git_info") {
            return {
              isRepo: true,
              branch: "feature/an-exceptionally-long-dirty-worktree-branch-name",
              worktreeRoot: "/home/natkins/projects/repository",
              isLinkedWorktree: true,
              dirtyCount: 27,
              headCommit: "0123456789abcdef",
            };
          }
          if (args?.command === "reconcile_cortana") {
            const request = args.args as Record<string, unknown> | undefined;
            return {
              operationId: String(request?.operationId ?? "browser-fixture"),
              action: "keep",
              healthy: true,
              terminalId: "browser-cortana",
              identityId: "browser-cortana-identity",
              generation: 1,
              degradedReason: null,
            };
          }
          if (args?.command === "history_list") {
            return {
              schemaVersion: 1,
              generatedAt: new Date(0).toISOString(),
              revision: "browser-fixture",
              entries: [],
              count: 0,
              total: 0,
              truncated: false,
              sources: [],
            };
          }
          if (args?.command === "recent_sessions") return [];
        }
        if (command === "plugin:event|listen") return callbackId;
        throw {
          message: `Browser fixture does not implement ${command}`,
          retryable: false,
        };
      },
      transformCallback: (callback: (value: unknown) => void, once = false) => {
        const id = callbackId;
        callbackId += 1;
        host[`_${id}`] = (value: unknown) => {
          callback(value);
          if (once) delete host[`_${id}`];
        };
        return id;
      },
      unregisterCallback: (id: number) => {
        delete host[`_${id}`];
      },
      convertFileSrc: (path: string) => path,
      metadata: {
        currentWindow: { label: "main" },
        currentWebview: { windowLabel: "main", label: "main" },
      },
    };
  });
}

async function seedTiles(page: Page, count: number): Promise<void> {
  await installGitMock(page);
  await page.goto("/");
  await expect(page.locator("body")).not.toHaveText("");

  await page.evaluate(
    async ({ tileCount, statuses }) => {
      const [
        { useWorkspace },
        { useCaptain },
        { useTheme },
        { usePanels },
        { useSettings },
        { useSessionContext },
        { useSupervision },
      ] = await Promise.all([
        import("/src/store/workspace.ts"),
        import("/src/store/captain.ts"),
        import("/src/store/theme.ts"),
        import("/src/store/panels.ts"),
        import("/src/store/settings.ts"),
        import("/src/store/sessionContext.ts"),
        import("/src/store/supervision.ts"),
      ]);
      const ids = Array.from(
        { length: tileCount },
        (_, index) => `header${String(index + 1).padStart(3, "0")}`,
      );
      const terminals = Object.fromEntries(
        ids.map((id, index) => [
          id,
          {
            id,
            tmuxSession: `th_${id}`,
            cwd: `/home/natkins/projects/a-very-long-project-directory-${index + 1}`,
            title: "claude",
            state: "live",
          },
        ]),
      );
      const names = Object.fromEntries(
        ids.map((id, index) => [
          id,
          `Claude terminal ${index + 1} with an exceptionally long user title`,
        ]),
      );
      const workNames = Object.fromEntries(
        ids.map((_, index) => [
          `/home/natkins/projects/a-very-long-project-directory-${index + 1}`,
          `An exceptionally long current work name for terminal ${index + 1}`,
        ]),
      );
      const sessionIdByTmux = Object.fromEntries(
        ids.map((id, index) => [`th_${id}`, `session-${index + 1}`]),
      );
      const sessionStatuses = Object.fromEntries(
        ids.map((_, index) => [
          `session-${index + 1}`,
          statuses[index % statuses.length],
        ]),
      );
      const bySession = Object.fromEntries(
        ids.map((id, index) => [
          `th_${id}`,
          { usedPct: 60 + (index % 4) * 9, ts: Date.now() },
        ]),
      );

      usePanels.setState({ tab: {}, devUrl: {}, previewUrl: {}, fullscreenId: null });
      useTheme.setState({ workNames });
      useSettings.setState({ showHeaderContextMeter: true });
      useSessionContext.setState({ bySession });
      useSupervision.setState({
        trees: {},
        statuses: sessionStatuses,
        snapshots: {},
        sessionIdByTmux,
      });
      useWorkspace.setState({
        tabs: [{ id: "header-e2e", name: "Header E2E", order: ids }],
        activeTabId: "header-e2e",
        focusedId: ids[0] ?? null,
        terminals,
        poppedOutTabs: [],
        userLabels: names,
        labels: names,
        claudeTitles: {},
      });
      useCaptain.setState({
        orchestratorId: ids[0] ?? null,
        captainIds: ids[1] ? [ids[1]] : [],
        claims: {},
      });
    },
    { tileCount: count, statuses: STATUS_SEQUENCE },
  );

  await expect(page.locator(TILE_SELECTOR).filter({ visible: true })).toHaveCount(count);
  await expect(page.locator(`${HEADER_SELECTOR} .th-git-chip`)).toHaveCount(count);
  await expect(page.locator(`${HEADER_SELECTOR} .th-context-meter`)).toHaveCount(count);
}

async function visibleHeaderMetrics(page: Page): Promise<HeaderMetrics[]> {
  return page.locator(HEADER_SELECTOR).evaluateAll((headers) =>
    headers
      .filter((header) => header.getBoundingClientRect().width > 0)
      .map((header) => {
        const headerRect = header.getBoundingClientRect();
        const tileId = header.closest<HTMLElement>("[data-tile-id]")?.dataset.tileId ?? "";
        const selectors = [
          ".th-client-icon",
          ".th-captain-marker",
          ':scope > [aria-label="Orchestrator"]',
          ".th-tile-title",
          ".th-git-chip",
          ".th-work-name",
          ".th-context-meter",
          ':scope > [role="img"]',
          ".th-tab",
          ".th-header-control",
        ].join(",");
        const elements = [...header.querySelectorAll<HTMLElement>(selectors)].filter(
          (element) => {
            const rect = element.getBoundingClientRect();
            return (
              getComputedStyle(element).display !== "none" &&
              getComputedStyle(element).visibility !== "hidden" &&
              rect.width > 0 &&
              rect.height > 0
            );
          },
        );
        const items = elements.map((element) => {
          const rect = element.getBoundingClientRect();
          const control =
            element.classList.contains("th-tab") ||
            element.classList.contains("th-header-control");
          return {
            kind: control
              ? element.classList.contains("th-tab")
                ? "panel-tab"
                : "header-control"
              : element.className || element.tagName.toLowerCase(),
            name:
              element.getAttribute("aria-label") ??
              element.getAttribute("title") ??
              element.textContent?.trim() ??
              element.tagName.toLowerCase(),
            width: rect.width,
            height: rect.height,
            contained:
              rect.left >= headerRect.left - 0.1 &&
              rect.right <= headerRect.right + 0.1 &&
              rect.top >= headerRect.top - 0.1 &&
              rect.bottom <= headerRect.bottom + 0.1,
            control,
            left: rect.left,
            right: rect.right,
            top: rect.top,
            bottom: rect.bottom,
          };
        });
        const overlaps: string[][] = [];
        for (let left = 0; left < items.length; left += 1) {
          for (let right = left + 1; right < items.length; right += 1) {
            const a = items[left];
            const b = items[right];
            const overlapX = Math.min(a.right, b.right) - Math.max(a.left, b.left);
            const overlapY = Math.min(a.bottom, b.bottom) - Math.max(a.top, b.top);
            if (overlapX > 0.2 && overlapY > 0.2) {
              overlaps.push([`${a.kind}:${a.name}`, `${b.kind}:${b.name}`]);
            }
          }
        }

        const tabs = [...header.querySelectorAll<HTMLButtonElement>(".th-tab")];
        return {
          tileId,
          width: headerRect.width,
          labels: [
            ...header.querySelectorAll<HTMLElement>(
              ".th-tab-label,.th-tab-label-short",
            ),
          ]
            .filter((label) => getComputedStyle(label).display !== "none")
            .map((label) => label.textContent ?? ""),
          tabNames: tabs.map((tab) => tab.getAttribute("aria-label") ?? ""),
          items,
          overlaps,
        };
      }),
  );
}

function assertMeasuredHeaders(metrics: HeaderMetrics[], count: number): void {
  expect(metrics).toHaveLength(count);
  for (const header of metrics) {
    expect(header.tabNames).toEqual(TAB_NAMES);
    expect(header.items.length).toBeGreaterThanOrEqual(6);
    expect(header.items.every((item) => item.contained)).toBe(true);
    expect(
      header.items
        .filter((item) => item.control)
        .every((item) => item.width >= 24 && item.height >= 24),
    ).toBe(true);
    expect(header.overlaps).toEqual([]);
  }
}

async function assertHeadersAreUsable(page: Page, count: number): Promise<void> {
  assertMeasuredHeaders(await visibleHeaderMetrics(page), count);
}

async function screenshot(page: Page, testInfo: TestInfo, name: string): Promise<void> {
  await page.screenshot({ path: testInfo.outputPath(name), animations: "disabled" });
}

test("measures all visible chrome across 1, 2, 4, 8, and 16 tile grids", async ({
  page,
}, testInfo) => {
  for (const count of [1, 2, 4, 8, 16]) {
    await seedTiles(page, count);
    await assertHeadersAreUsable(page, count);
    await screenshot(page, testInfo, `grid-${count}.png`);
  }
});

test("renders deterministic identity, Git, status, and context variants", async ({
  page,
}) => {
  await seedTiles(page, 4);
  await expect(page.locator('[data-tile-id="header001"][data-orchestrator="1"]')).toBeVisible();
  await expect(page.locator('[data-tile-id="header002"][data-captain="1"]')).toBeVisible();
  await expect(page.locator(HEADER_SELECTOR).getByLabel("Orchestrator")).toBeVisible();
  await expect(page.locator(HEADER_SELECTOR).getByLabel("Captain session")).toBeVisible();
  await expect(page.locator(`${HEADER_SELECTOR} .th-git-chip`).filter({ visible: true })).toHaveCount(4);
  await expect(page.locator(`${HEADER_SELECTOR} .th-work-name`).filter({ visible: true })).toHaveCount(4);
  await expect(page.locator('[data-tile-id="header001"] .th-ind-spin')).toHaveCount(1);
  await expect(page.locator('[data-tile-id="header002"] .th-ind-pulse')).toHaveCount(1);
  await expect(page.locator('[data-tile-id="header003"] .th-tile-header > [role="img"]')).toHaveCSS(
    "background-color",
    "rgb(34, 197, 94)",
  );
  await expect(page.locator('[data-tile-id="header004"] .th-tile-header > [role="img"]')).toHaveCSS(
    "background-color",
    "rgb(239, 68, 68)",
  );
  await expect(page.locator(`${HEADER_SELECTOR} > [role="img"]`)).toHaveCount(4);
  await assertHeadersAreUsable(page, 4);

  await seedTiles(page, 1);
  await expect(page.locator(`${HEADER_SELECTOR} .th-context-meter`)).toBeVisible();
  await expect(page.getByLabel("Context window 60 percent full")).toBeVisible();
  await assertHeadersAreUsable(page, 1);
});

test("switches densities at adjacent container widths without losing chrome", async ({
  page,
}, testInfo) => {
  await seedTiles(page, 1);
  const cases = [
    { outerWidth: 680, labels: ["Terminal", "Files", "Preview"] },
    { outerWidth: 679, labels: ["Term", "Files", "Preview"] },
    { outerWidth: 460, labels: ["Term", "Files", "Preview"] },
    { outerWidth: 459, labels: [] },
    { outerWidth: 360, labels: [] },
    { outerWidth: 359, labels: [] },
    { outerWidth: 240, labels: [] },
    { outerWidth: 239, labels: [] },
    { outerWidth: 152, labels: [] },
  ];

  for (const entry of cases) {
    await page.locator(TILE_SELECTOR).evaluate((tile, width) => {
      (tile as HTMLElement).style.width = `${width}px`;
    }, entry.outerWidth);
    await expect
      .poll(async () => (await visibleHeaderMetrics(page))[0]?.labels)
      .toEqual(entry.labels);
    await assertHeadersAreUsable(page, 1);
    await screenshot(page, testInfo, `breakpoint-${entry.outerWidth}.png`);
  }
});

async function selectedPanel(page: Page): Promise<string> {
  return page.evaluate(async () => {
    const { usePanels } = await import("/src/store/panels.ts");
    return usePanels.getState().tab.header001 ?? "terminal";
  });
}

async function focusPrecedingControl(page: Page): Promise<void> {
  const precedingName = await page.evaluate(() => {
    for (const marked of document.querySelectorAll<HTMLElement>(
      "[data-e2e-preceding]",
    )) {
      delete marked.dataset.e2ePreceding;
    }
    const selector = [
      "button:not([disabled])",
      "a[href]",
      "input:not([disabled])",
      "[tabindex]:not([tabindex='-1'])",
    ].join(",");
    const focusable = [...document.querySelectorAll<HTMLElement>(selector)].filter(
      (element) => {
        const rect = element.getBoundingClientRect();
        return (
          getComputedStyle(element).display !== "none" &&
          getComputedStyle(element).visibility !== "hidden" &&
          rect.width > 0 &&
          rect.height > 0
        );
      },
    );
    const terminal = focusable.find(
      (element) =>
        element.classList.contains("th-tab") &&
        element.getAttribute("aria-label") === "Terminal",
    );
    const index = terminal ? focusable.indexOf(terminal) : -1;
    if (index < 1) throw new Error("No real focusable control precedes Terminal");
    const preceding = focusable[index - 1];
    preceding.dataset.e2ePreceding = "true";
    return preceding.getAttribute("aria-label") ?? preceding.title ?? preceding.textContent ?? "";
  });
  expect(precedingName.trim()).not.toBe("");
  await page.locator('[data-e2e-preceding="true"]').focus();
  await page.keyboard.press("Tab");
  await expect(page.getByRole("button", { name: "Terminal", exact: true })).toBeFocused();
}

async function assertKeyboardFlow(page: Page, outerWidth: number): Promise<void> {
  await page.locator(TILE_SELECTOR).evaluate((tile, width) => {
    (tile as HTMLElement).style.width = `${width}px`;
  }, outerWidth);
  await assertHeadersAreUsable(page, 1);
  await focusPrecedingControl(page);

  const tile = page.locator('[data-tile-id="header001"]');
  const terminal = tile.getByRole("button", { name: "Terminal", exact: true });
  const files = tile.getByRole("button", { name: "Files", exact: true });
  const preview = tile.getByRole("button", { name: "Preview", exact: true });

  await page.keyboard.press("Space");
  await expect(terminal).toHaveAttribute("aria-pressed", "true");
  expect(await selectedPanel(page)).toBe("terminal");
  await expect(tile.locator(".th-panel-pane")).toHaveCount(0);

  await page.keyboard.press("Tab");
  await expect(files).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(files).toHaveAttribute("aria-pressed", "true");
  expect(await selectedPanel(page)).toBe("files");
  await expect(tile.locator(".th-panel-title")).toHaveText("Files");

  await page.keyboard.press("Tab");
  await expect(preview).toBeFocused();
  await page.keyboard.press("Space");
  await expect(preview).toHaveAttribute("aria-pressed", "true");
  expect(await selectedPanel(page)).toBe("preview");
  await expect(tile.locator(".th-panel-title")).toHaveText("Preview");
  await expect(tile.getByRole("region", { name: "Preview" })).toBeVisible();
  await expect(preview).toHaveAttribute("title", "Preview");
  await expect(preview.locator(".lucide-eye")).toHaveCount(1);
}

test("enters and activates every panel tab by keyboard at full and icon widths", async ({
  page,
}, testInfo) => {
  await seedTiles(page, 1);
  await assertKeyboardFlow(page, 680);
  await screenshot(page, testInfo, "keyboard-full.png");
  await page.evaluate(async () => {
    const { usePanels } = await import("/src/store/panels.ts");
    usePanels.getState().setTab("header001", "terminal");
  });
  await assertKeyboardFlow(page, 152);
  await screenshot(page, testInfo, "keyboard-icon.png");
});

test("settles 20 rendered resize cycles during active output without churn", async ({
  page,
}, testInfo) => {
  await seedTiles(page, 1);
  const consoleErrors: string[] = [];
  page.on("console", (message) => {
    if (message.type() === "error") consoleErrors.push(message.text());
  });
  page.on("pageerror", (error) => consoleErrors.push(error.message));

  await page.locator('[data-tile-id="header001"]').evaluate((tile) => {
    const output = document.createElement("div");
    output.dataset.e2eTerminalOutput = "true";
    output.setAttribute("aria-label", "Active terminal output fixture");
    output.style.cssText = "position:absolute;left:0;bottom:0;width:1px;height:1px;overflow:hidden";
    tile.appendChild(output);
    const header = tile.querySelector(".th-tile-header");
    if (!header) throw new Error("header missing");
    const host = window as typeof window & {
      __headerMutationCount?: number;
      __outputMutationCount?: number;
    };
    host.__headerMutationCount = 0;
    host.__outputMutationCount = 0;
    new MutationObserver((records) => {
      host.__headerMutationCount = (host.__headerMutationCount ?? 0) + records.length;
    }).observe(header, { attributes: true, childList: true, characterData: true, subtree: true });
    new MutationObserver((records) => {
      host.__outputMutationCount = (host.__outputMutationCount ?? 0) + records.length;
    }).observe(output, { childList: true, characterData: true, subtree: true });
  });

  const widths = [680, 679, 460, 459, 360, 359, 240, 239, 152];
  let step = 0;
  for (let cycle = 0; cycle < 20; cycle += 1) {
    for (const width of widths) {
      step += 1;
      await page.locator('[data-tile-id="header001"]').evaluate(
        async (tile, value) => {
          tile.style.width = `${value.width}px`;
          const output = tile.querySelector<HTMLElement>("[data-e2e-terminal-output]");
          if (!output) throw new Error("output fixture missing");
          output.textContent = `active output frame ${value.step}`;
          await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
          await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
        },
        { width, step },
      );
      const metrics = await visibleHeaderMetrics(page);
      assertMeasuredHeaders(metrics, 1);
      const expectedLabels = width >= 680
        ? ["Terminal", "Files", "Preview"]
        : width >= 460
          ? ["Term", "Files", "Preview"]
          : [];
      expect(metrics[0].labels).toEqual(expectedLabels);
    }
  }

  const mutationCounts = await page.evaluate(() => {
    const host = window as typeof window & {
      __headerMutationCount?: number;
      __outputMutationCount?: number;
    };
    return {
      header: host.__headerMutationCount ?? -1,
      output: host.__outputMutationCount ?? -1,
    };
  });
  expect(mutationCounts.header).toBe(0);
  expect(mutationCounts.output).toBeGreaterThanOrEqual(step);
  expect(consoleErrors).toEqual([]);
  await screenshot(page, testInfo, "resize-final.png");
});

async function verifyScale(
  browser: Browser,
  scale: number,
  testInfo: TestInfo,
): Promise<void> {
  const context = await browser.newContext({
    deviceScaleFactor: scale,
    viewport: { width: 1280, height: 720 },
  });
  const page = await context.newPage();
  try {
    await seedTiles(page, 16);
    expect(await page.evaluate(() => window.devicePixelRatio)).toBe(scale);
    await assertHeadersAreUsable(page, 16);
    await screenshot(page, testInfo, `scale-${String(scale).replace(".", "-")}.png`);
  } finally {
    await context.close();
  }
}

test("preserves the full 16-tile chrome under emulated display scales", async ({
  browser,
}, testInfo) => {
  for (const scale of [1, 1.25, 1.5, 2]) {
    await verifyScale(browser, scale, testInfo);
  }
});
