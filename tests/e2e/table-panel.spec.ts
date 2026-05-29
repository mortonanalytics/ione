import { test, expect, type Page } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";

const STREAM_ID = "33333333-3333-3333-3333-333333333333";
const PEER_ID = "11111111-1111-1111-1111-111111111111";

function ioneTable() {
  return {
    id: "ione-table-1",
    name: "Earthquake events",
    source: "ione",
    streamId: STREAM_ID
  };
}

function peerTable() {
  return {
    id: "peer-table-1",
    name: "Peer ledger",
    source: "peer",
    peerId: PEER_ID,
    peerName: "Table Peer",
    uri: "stub://table/1"
  };
}

const columns = [
  { name: "_observed_at", label: "Observed At", type: "datetime", pointer: null },
  { name: "mag", label: "mag", type: "string", pointer: "/properties/mag" },
  { name: "type", label: "type", type: "string", pointer: "/properties/type" }
];

const rows = [
  { _observed_at: "2026-05-28T00:00:00Z", mag: "4.2", type: "earthquake" },
  { _observed_at: "2026-05-28T01:00:00Z", mag: "5.1", type: "quarry blast" }
];

test.beforeEach(async ({ page }) => {
  await page.route("**/api/v1/me", (route) =>
    route.fulfill({
      contentType: "application/json",
      body: JSON.stringify({ user: { email: "default@localhost", displayName: "Default" } })
    })
  );
  await page.route("**/api/v1/workspaces", (route) =>
    route.fulfill({
      contentType: "application/json",
      body: JSON.stringify({
        items: [{
          id: "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
          name: "Operations",
          domain: "test",
          lifecycle: "continuous",
          closedAt: null
        }]
      })
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
  await page.route("**/api/v1/workspaces/*/approvals*", (route) =>
    route.fulfill({ contentType: "application/json", body: JSON.stringify({ items: [] }) })
  );
});

async function stubTablePanels(page: Page, body: Record<string, unknown>) {
  await page.route("**/api/v1/workspaces/*/table-panels*", (route) =>
    route.fulfill({ contentType: "application/json", body: JSON.stringify(body) })
  );
}

test("skeleton: Tables tab opens and keyboard order includes it", async ({ page }) => {
  await stubTablePanels(page, { ioneTables: [], peerTables: [], peerErrors: [] });
  await page.goto("/");

  await page.locator("#tab-chart").focus();
  await page.keyboard.press("ArrowRight");
  await expect(page.locator("#tab-table")).toBeFocused();
  await expect(page.locator("#tab-table")).toHaveAttribute("aria-selected", "true");
  await expect(page.locator("#panel-table")).toBeVisible();
});

test("AC-11: selecting an IONe table renders semantic table and refetches on sort", async ({ page }) => {
  await stubTablePanels(page, { ioneTables: [ioneTable()], peerTables: [], peerErrors: [] });
  let requests = 0;
  await page.route("**/api/v1/workspaces/*/event-table*", (route) => {
    requests += 1;
    const url = new URL(route.request().url());
    if (requests > 1) {
      expect(url.searchParams.get("sort_by")).toBe("mag");
      expect(url.searchParams.get("sort_dir")).toBe("asc");
    }
    route.fulfill({
      contentType: "application/json",
      body: JSON.stringify({
        streamId: STREAM_ID,
        columns,
        rows: requests > 1 ? [...rows].reverse() : rows,
        totalCount: rows.length,
        page: 1,
        perPage: 25,
        truncated: false
      })
    });
  });

  await page.goto("/");
  await page.locator("#tab-table").click();
  await page.locator("#table-list .table-row").first().click();

  await expect(page.locator("#table-render-region table")).toBeVisible();
  await expect(page.locator("#table-render-region caption")).toContainText("2 rows");
  await expect(page.locator("#table-render-region th[scope=col]")).toHaveCount(columns.length);
  await expect(page.locator("#table-render-region tbody tr")).toHaveCount(rows.length);

  await page.locator("#table-render-region th", { hasText: "mag" }).locator("button").click();
  await expect.poll(() => requests).toBe(2);
  await expect(page.locator("#table-render-region tbody tr").first()).toContainText("5.1");

  const axe = await new AxeBuilder({ page }).include("#panel-table").analyze();
  expect(axe.violations).toEqual([]);
});

test("AC-12: peer table sorts and filters client-side without refetch", async ({ page }) => {
  await stubTablePanels(page, { ioneTables: [], peerTables: [peerTable()], peerErrors: [] });
  let tableDataRequests = 0;
  await page.route("**/api/v1/workspaces/*/table-data*", (route) => {
    tableDataRequests += 1;
    route.fulfill({
      contentType: "application/json",
      body: JSON.stringify({
        schema: [
          { name: "amount", label: "Amount", type: "number" },
          { name: "label", label: "Label", type: "string" }
        ],
        rows: [
          { amount: 2, label: "south" },
          { amount: 10, label: "north" },
          { amount: 4, label: "northwest" }
        ]
      })
    });
  });

  await page.goto("/");
  await page.locator("#tab-table").click();
  await page.locator("#table-list .table-row").first().click();
  await expect(page.locator("#table-render-region tbody tr")).toHaveCount(3);
  expect(tableDataRequests).toBe(1);

  await page.locator("#table-render-region th", { hasText: "Amount" }).locator("button").click();
  await expect(page.locator("#table-render-region tbody tr").first()).toContainText("10");
  expect(tableDataRequests).toBe(1);

  await page.locator("#table-render-region input[aria-label='Filter Label']").fill("north");
  await expect(page.locator("#table-render-region tbody tr")).toHaveCount(2);
  await expect(page.locator("#table-render-region tbody tr").first()).toContainText("north");
  expect(tableDataRequests).toBe(1);
});
