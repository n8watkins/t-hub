import { expect, test, type Browser, type Page } from "@playwright/test";

const TILE_SELECTOR = "[data-tile-id]";
const HEADER_SELECTOR = `${TILE_SELECTOR} .th-tile-header`;

type HeaderMetrics = {
  width: number;
  labels: string[];
  tabNames: string[];
  tabWidths: number[];
  contained: boolean;
  overlaps: string[][];
};

async function seedTiles(page: Page, count: number): Promise<void> {
  await page.goto("/");
  await expect(page.locator("body")).not.toHaveText("");

  await page.evaluate(async (tileCount) => {
    const [{ useWorkspace }, { useCaptain }, { useTheme }] = await Promise.all([
      import("/src/store/workspace.ts"),
      import("/src/store/captain.ts"),
      import("/src/store/theme.ts"),
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
        `Terminal ${index + 1} with an exceptionally long user title`,
      ]),
    );

    const workNames = Object.fromEntries(
      ids.map((_, index) => [
        `/home/natkins/projects/a-very-long-project-directory-${index + 1}`,
        `An exceptionally long current work name for terminal ${index + 1}`,
      ]),
    );
    useCaptain.setState({ orchestratorId: null, captainIds: [], claims: {} });
    useTheme.setState({ workNames });
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
  }, count);

  await expect(page.locator(TILE_SELECTOR).filter({ visible: true })).toHaveCount(count);
}

async function visibleHeaderMetrics(page: Page): Promise<HeaderMetrics[]> {
  return page.locator(HEADER_SELECTOR).evaluateAll((headers) =>
    headers
      .filter((header) => header.getBoundingClientRect().width > 0)
      .map((header) => {
        const headerRect = header.getBoundingClientRect();
        const tabs = [...header.querySelectorAll<HTMLButtonElement>(".th-tab")];
        const visibleButtons = [
          ...header.querySelectorAll<HTMLButtonElement>("button"),
        ].filter((button) => {
          const rect = button.getBoundingClientRect();
          return (
            getComputedStyle(button).display !== "none" &&
            rect.width > 0 &&
            rect.height > 0
          );
        });
        const overlaps: string[][] = [];
        for (let left = 0; left < visibleButtons.length; left += 1) {
          for (let right = left + 1; right < visibleButtons.length; right += 1) {
            const a = visibleButtons[left].getBoundingClientRect();
            const b = visibleButtons[right].getBoundingClientRect();
            const overlapX = Math.min(a.right, b.right) - Math.max(a.left, b.left);
            const overlapY = Math.min(a.bottom, b.bottom) - Math.max(a.top, b.top);
            if (overlapX > 0.2 && overlapY > 0.2) {
              overlaps.push([
                visibleButtons[left].getAttribute("aria-label") ??
                  visibleButtons[left].title,
                visibleButtons[right].getAttribute("aria-label") ??
                  visibleButtons[right].title,
              ]);
            }
          }
        }

        return {
          width: headerRect.width,
          labels: [
            ...header.querySelectorAll<HTMLElement>(
              ".th-tab-label,.th-tab-label-short",
            ),
          ]
            .filter((label) => getComputedStyle(label).display !== "none")
            .map((label) => label.textContent ?? ""),
          tabNames: tabs.map((tab) => tab.getAttribute("aria-label") ?? ""),
          tabWidths: tabs.map((tab) => tab.getBoundingClientRect().width),
          contained: tabs.every((tab) => {
            const rect = tab.getBoundingClientRect();
            return (
              rect.left >= headerRect.left - 0.1 &&
              rect.right <= headerRect.right + 0.1
            );
          }),
          overlaps,
        };
      }),
  );
}

async function assertHeadersAreUsable(page: Page, count: number): Promise<void> {
  const metrics = await visibleHeaderMetrics(page);
  expect(metrics).toHaveLength(count);
  for (const header of metrics) {
    expect(header.tabNames).toEqual(["Terminal", "Files", "Preview"]);
    expect(header.tabWidths.every((width) => width >= 24)).toBe(true);
    expect(header.contained).toBe(true);
    expect(header.overlaps).toEqual([]);
  }
}

test("keeps panel controls usable across 1, 2, 4, 8, and 16 tile grids", async ({
  page,
}) => {
  for (const count of [1, 2, 4, 8, 16]) {
    await seedTiles(page, count);
    await assertHeadersAreUsable(page, count);
  }
});

test("keeps Captain, Cortana, and long work chrome from displacing tabs", async ({
  page,
}) => {
  await seedTiles(page, 2);
  await page.evaluate(async () => {
    const { useCaptain } = await import("/src/store/captain.ts");
    useCaptain.setState({
      orchestratorId: "header001",
      captainIds: ["header002"],
      claims: {},
    });
  });

  await expect(page.locator('[data-tile-id="header001"][data-orchestrator="1"]')).toBeVisible();
  await expect(page.locator('[data-tile-id="header002"][data-captain="1"]')).toBeVisible();
  await expect(page.locator(HEADER_SELECTOR).getByLabel("Orchestrator")).toBeVisible();
  await expect(page.locator(HEADER_SELECTOR).getByLabel("Captain session")).toBeVisible();
  await assertHeadersAreUsable(page, 2);
});

test("switches full, short, and icon-only labels at adjacent container widths", async ({
  page,
}) => {
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
  }
});

test("keeps icon-only panel tabs keyboard accessible", async ({ page }) => {
  await seedTiles(page, 1);
  await page.locator(TILE_SELECTOR).evaluate((tile) => {
    (tile as HTMLElement).style.width = "152px";
  });

  const terminal = page.getByRole("button", { name: "Terminal", exact: true });
  const files = page.getByRole("button", { name: "Files", exact: true });
  const preview = page.getByRole("button", { name: "Preview", exact: true });
  await terminal.focus();
  await page.keyboard.press("Tab");
  await expect(files).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(files).toHaveAttribute("aria-pressed", "true");
  await page.keyboard.press("Tab");
  await expect(preview).toBeFocused();
  await page.keyboard.press("Space");
  await expect(preview).toHaveAttribute("aria-pressed", "true");
  await expect(preview).toHaveAttribute("title", "Preview view");
  await expect(preview.locator(".lucide-eye")).toHaveCount(1);
});

test("resizes through every density without DOM churn or resize-loop errors", async ({
  page,
}) => {
  const resizeLoopErrors: string[] = [];
  page.on("console", (message) => {
    if (/ResizeObserver loop/i.test(message.text())) {
      resizeLoopErrors.push(message.text());
    }
  });
  page.on("pageerror", (error) => {
    if (/ResizeObserver loop/i.test(error.message)) {
      resizeLoopErrors.push(error.message);
    }
  });
  await seedTiles(page, 1);

  const mutations = await page.locator(HEADER_SELECTOR).evaluate(async (header) => {
    const records: MutationRecord[] = [];
    const observer = new MutationObserver((batch) => records.push(...batch));
    observer.observe(header, {
      attributes: true,
      childList: true,
      characterData: true,
      subtree: true,
    });
    const tile = header.closest<HTMLElement>("[data-tile-id]");
    if (!tile) throw new Error("tile host missing");
    const widths = [680, 679, 460, 459, 360, 359, 240, 239, 152];
    for (let cycle = 0; cycle < 20; cycle += 1) {
      for (const width of widths) {
        tile.style.width = `${width}px`;
        header.getBoundingClientRect();
      }
    }
    await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
    observer.disconnect();
    return records.length;
  });

  expect(mutations).toBe(0);
  expect(resizeLoopErrors).toEqual([]);
  await assertHeadersAreUsable(page, 1);
});

async function verifyScale(browser: Browser, scale: number): Promise<void> {
  const context = await browser.newContext({
    deviceScaleFactor: scale,
    viewport: { width: 1280, height: 720 },
  });
  const page = await context.newPage();
  try {
    await seedTiles(page, 16);
    expect(await page.evaluate(() => window.devicePixelRatio)).toBe(scale);
    await assertHeadersAreUsable(page, 16);
  } finally {
    await context.close();
  }
}

test("preserves the 16-tile header under emulated display scales", async ({
  browser,
}) => {
  for (const scale of [1, 1.25, 1.5, 2]) {
    await verifyScale(browser, scale);
  }
});
