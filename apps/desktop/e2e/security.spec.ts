import { test, expect } from "@playwright/test";
import { readFileSync } from "node:fs";
const config = JSON.parse(readFileSync(new URL("../src-tauri/tauri.conf.json", import.meta.url), "utf8"));
const policy = Object.entries(config.app.security.csp).map(([key, value]) => `${key} ${value}`).join("; ");

test("production CSP permits local UI and preview frames but blocks injected scripts and connections", async ({ page }) => {
  await page.route("http://policy.test/**", (route) => {
    if (route.request().url().endsWith("/app.js")) {
      return route.fulfill({ contentType: "application/javascript", body: "document.querySelector('#app').textContent='Local UI loaded';" });
    }
    return route.fulfill({ contentType: "text/html", headers: { "Content-Security-Policy": policy }, body: `
      <div id="app" style="color: rgb(12, 34, 56)"></div><script src="/app.js"></script>
      <script>window.injected = true</script><script src="https://untrusted.test/attack.js"></script>
      <iframe sandbox="allow-scripts allow-same-origin" src="https://preview.test/"></iframe>` });
  });
  await page.route("https://preview.test/", (route) => route.fulfill({ contentType: "text/html", body: "<p>Preview loaded</p>" }));
  await page.goto("http://policy.test/");
  await expect(page.locator("#app")).toHaveText("Local UI loaded");
  await expect(page.locator("#app")).toHaveCSS("color", "rgb(12, 34, 56)");
  await expect(page.frameLocator("iframe").locator("p")).toHaveText("Preview loaded");
  expect(await page.evaluate(() => (window as unknown as { injected?: boolean }).injected)).toBeUndefined();
  expect(await page.evaluate(async () => {
    try { await fetch("https://untrusted.test/exfiltrate"); return "allowed"; } catch { return "blocked"; }
  })).toBe("blocked");
  expect(await page.frameLocator("iframe").locator("body").evaluate(() => {
    try { return Boolean(parent.document); } catch { return false; }
  })).toBe(false);
});
