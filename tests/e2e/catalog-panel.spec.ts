import { test, expect, type Page } from "@playwright/test";

// AC-11: peer-supplied catalog strings are untrusted. The Catalog panel must
// render a peer description / name / sample query as literal escaped text via
// escapeHtml — never parse it as HTML or markdown (XSS, FCS-M1).

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

test("catalog panel renders peer <script> description as escaped text", async ({ page }) => {
  const alerts: string[] = [];
  page.on("dialog", (d) => {
    alerts.push(d.message());
    d.dismiss().catch(() => {});
  });

  await stubShell(page);
  await page.goto("/");
  await expect
    .poll(() => page.evaluate(() => typeof (window as any).renderCatalogResults))
    .toBe("function");

  await page.evaluate(() => {
    (window as any).renderCatalogResults([
      {
        namespaced_name: "evilpeer:get_thing",
        kind: "tool",
        peer_name: "Evil Peer",
        description: "<script>window.__xss=1</script><img src=x onerror=window.__xss=2>",
        sample_queries: ["<script>window.__xss=3</script>"],
        score: 1.0,
      },
    ]);
  });

  // No script executed, no dangerous DOM survived.
  expect(await page.evaluate(() => (window as any).__xss)).toBeUndefined();
  expect(alerts).toEqual([]);
  await expect(page.locator("#catalog-results script")).toHaveCount(0);
  await expect(page.locator("#catalog-results img[onerror]")).toHaveCount(0);

  // The peer string appears as literal, escaped text.
  const desc = await page.locator("#catalog-results .catalog-result-desc").textContent();
  expect(desc).toContain("<script>window.__xss=1</script>");
});
