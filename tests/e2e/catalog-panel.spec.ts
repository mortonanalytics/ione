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

test("catalog search flow: type → fetch → render, with min-char guard", async ({ page }) => {
  await stubShell(page);
  let calls = 0;
  await page.route("**/api/v1/workspaces/*/catalog-search*", (route) => {
    calls += 1;
    route.fulfill({
      contentType: "application/json",
      body: JSON.stringify({
        items: [
          { peer_id: "11111111-1111-1111-1111-111111111111", peer_name: "Weather Peer", namespaced_name: "weatherpeer:get_flood", kind: "tool", description: "flood risk inundation outlook", sample_queries: ["flood risk forecast"], score: 0.96 },
        ],
      }),
    });
  });

  await page.goto("/");
  await expect.poll(() => page.evaluate(() => typeof (window as any).loadCatalog)).toBe("function");

  // Select a workspace and open the Catalog tab via the real wiring.
  await page.evaluate(() =>
    (window as any).setActiveWorkspace({ id: "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa", name: "Operations", domain: "test", lifecycle: "continuous", closedAt: null })
  );
  await page.click("#tab-catalog");
  await expect(page.locator("#panel-catalog")).toBeVisible();

  // Min-char guard: a single character does not fetch and shows the hint.
  await page.fill("#catalog-search-input", "f");
  await expect(page.locator("#catalog-status")).toHaveText(/at least 2 characters/i);
  expect(calls).toBe(0);

  // A real query fetches and renders the result + a count status.
  await page.fill("#catalog-search-input", "flood risk");
  await expect(page.locator("#catalog-results .catalog-result-name")).toHaveText("weatherpeer:get_flood");
  await expect(page.locator("#catalog-status")).toHaveText(/1 result/i);
  expect(calls).toBeGreaterThan(0);
});
