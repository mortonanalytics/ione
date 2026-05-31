import { test, expect, type Page } from "@playwright/test";

// Regression for the markdown-rendering XSS hole: chat content (model /
// connector output) is untrusted, so DOMPurify must strip dangerous tags,
// attributes, and URL schemes before the rendered markdown reaches innerHTML.

async function stubShell(page: Page) {
  await page.route("**/api/v1/me", (route) =>
    route.fulfill({
      contentType: "application/json",
      body: JSON.stringify({ user: { email: "default@localhost", displayName: "Default" } }),
    })
  );
  await page.route("**/api/v1/workspaces", (route) =>
    route.fulfill({
      contentType: "application/json",
      body: JSON.stringify({
        items: [{ id: "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa", name: "Operations", domain: "test", lifecycle: "continuous", closedAt: null }],
      }),
    })
  );
  await page.route("**/api/v1/conversations", (route) =>
    route.fulfill({ contentType: "application/json", body: JSON.stringify({ items: [] }) })
  );
  await page.route("**/api/v1/health/ollama", (route) =>
    route.fulfill({ contentType: "application/json", body: JSON.stringify({ ok: true, models: { missing: [] } }) })
  );
  await page.route("**/api/v1/activation*", (route) =>
    route.fulfill({ contentType: "application/json", body: JSON.stringify({ items: [], completed: false }) })
  );
}

test("chat markdown sanitizes raw HTML and dangerous URL schemes", async ({ page }) => {
  const alerts: string[] = [];
  page.on("dialog", (d) => {
    alerts.push(d.message());
    d.dismiss().catch(() => {});
  });

  await stubShell(page);
  await page.goto("/");
  await expect.poll(() => page.evaluate(() => typeof (window as any).marked)).toBe("object");
  await expect
    .poll(() => page.evaluate(() => typeof (window as any).DOMPurify?.sanitize))
    .toBe("function");

  await page.evaluate(() => {
    const w = window as any;
    w.appendMessage("assistant", "<img src=x onerror=\"window.__xss=1\">");
    w.appendMessage("assistant", "<script>window.__xss=2</script>after");
    w.appendMessage("assistant", "[click me](javascript:window.__xss=3)");
    w.appendMessage("assistant", "Safe **bold** and `code`");
  });

  // No script executed, no dangerous DOM survived.
  expect(await page.evaluate(() => (window as any).__xss)).toBeUndefined();
  expect(alerts).toEqual([]);
  await expect(page.locator("#transcript img[onerror]")).toHaveCount(0);
  await expect(page.locator("#transcript script")).toHaveCount(0);
  const jsHrefs = await page.locator("#transcript a").evaluateAll((els) =>
    els.filter((a) => (a.getAttribute("href") || "").toLowerCase().startsWith("javascript:")).length
  );
  expect(jsHrefs).toBe(0);

  // Benign markdown still renders.
  await expect(page.locator("#transcript .message-body strong")).toHaveCount(1);
  await expect(page.locator("#transcript .message-body code")).toHaveCount(1);
});
