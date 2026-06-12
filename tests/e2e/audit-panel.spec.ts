import { test, expect, type Page } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";

const WS_ID = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";

function auditEvent(overrides: Record<string, unknown> = {}) {
  return {
    id: "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb",
    workspaceId: WS_ID,
    actorKind: "peer",
    actorRef: "peer:1234",
    verb: "peer_tool_executed",
    objectKind: "pending_peer_tool_call",
    objectId: "cccccccc-cccc-cccc-cccc-cccccccccccc",
    payload: {},
    foreignTenantId: null,
    createdAt: "2026-06-11T10:00:00Z",
    ...overrides
  };
}

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
          id: WS_ID,
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

/** Non-admin default: the stat-strip probe gets 403 unless a test overrides it. */
async function stubAggregates403(page: Page) {
  await page.route("**/api/v1/workspaces/*/audit-aggregates*", (route) =>
    route.fulfill({ status: 403, contentType: "application/json", body: JSON.stringify({ error: "forbidden" }) })
  );
  await page.route("**/api/v1/workspaces/*/pipeline-aggregates*", (route) =>
    route.fulfill({ status: 403, contentType: "application/json", body: JSON.stringify({ error: "forbidden" }) })
  );
}

test("filters drive refetch with actor_kind/verb/since query params", async ({ page }) => {
  await stubAggregates403(page);
  const urls: URL[] = [];
  await page.route("**/api/v1/workspaces/*/audit_events*", (route) => {
    urls.push(new URL(route.request().url()));
    route.fulfill({
      contentType: "application/json",
      body: JSON.stringify({ items: [auditEvent()], next_cursor: null })
    });
  });

  await page.goto("/");
  await page.locator("#tab-audit").click();
  await expect(page.locator("#audit-rows tr").first()).toContainText("peer_tool_executed");
  expect(urls[0].searchParams.has("actor_kind")).toBe(false);
  expect(urls[0].searchParams.has("since")).toBe(false);

  await page.locator("#audit-filter-actor-kind").selectOption("peer");
  await expect.poll(() => urls.length).toBe(2);
  expect(urls[1].searchParams.get("actor_kind")).toBe("peer");

  await page.locator("#audit-filter-verb").fill("peer_tool_executed");
  await page.locator("#audit-filter-verb").blur();
  await expect.poll(() => urls.length).toBe(3);
  expect(urls[2].searchParams.get("verb")).toBe("peer_tool_executed");

  await page.locator("#audit-filter-window").selectOption("24h");
  await expect.poll(() => urls.length).toBe(4);
  const since = urls[3].searchParams.get("since");
  expect(since).not.toBeNull();
  expect(Date.now() - new Date(since!).getTime()).toBeLessThan(25 * 3600e3);
});

test("Load more appends the cursor page and hides when exhausted", async ({ page }) => {
  await stubAggregates403(page);
  const urls: URL[] = [];
  await page.route("**/api/v1/workspaces/*/audit_events*", (route) => {
    urls.push(new URL(route.request().url()));
    const firstPage = urls.length === 1;
    route.fulfill({
      contentType: "application/json",
      body: JSON.stringify(
        firstPage
          ? {
              items: [auditEvent({ verb: "page_one_a" }), auditEvent({ verb: "page_one_b" })],
              next_cursor: "cursor-1"
            }
          : { items: [auditEvent({ verb: "page_two_a" })], next_cursor: null }
      )
    });
  });

  await page.goto("/");
  await page.locator("#tab-audit").click();
  await expect(page.locator("#audit-rows tr")).toHaveCount(2);
  await expect(page.locator("#audit-load-more")).toBeVisible();

  await page.locator("#audit-load-more").click();
  await expect(page.locator("#audit-rows tr")).toHaveCount(3); // appended, not replaced
  await expect(page.locator("#audit-rows tr").first()).toContainText("page_one_a");
  await expect(page.locator("#audit-rows tr").last()).toContainText("page_two_a");
  expect(urls[1].searchParams.get("cursor")).toBe("cursor-1");
  await expect(page.locator("#audit-load-more")).toBeHidden();
});

test("admin: stat strip, bar list, and export button render from aggregates", async ({ page }) => {
  await page.route("**/api/v1/workspaces/*/audit_events*", (route) =>
    route.fulfill({ contentType: "application/json", body: JSON.stringify({ items: [auditEvent()], next_cursor: null }) })
  );
  await page.route("**/api/v1/workspaces/*/audit-aggregates*", (route) =>
    route.fulfill({
      contentType: "application/json",
      body: JSON.stringify({
        op: "count_by_bucket",
        bucket: "hour",
        groups: [
          { key: "user", bucket_start: "2026-06-11T10:00:00Z", count: 5 },
          { key: "peer", bucket_start: "2026-06-11T11:00:00Z", count: 3 }
        ]
      })
    })
  );
  await page.route("**/api/v1/workspaces/*/pipeline-aggregates*", (route) =>
    route.fulfill({
      contentType: "application/json",
      body: JSON.stringify({
        op: "recovery_gap",
        items: [{ connector_id: "dddddddd-dddd-dddd-dddd-dddddddddddd", gap_seconds: 90, from_stage: "error", occurred_at: "2026-06-11T10:00:00Z" }],
        summary: { count: 1, p50: 90, p90: 90, max: 90 }
      })
    })
  );

  await page.goto("/");
  await page.locator("#tab-audit").click();

  await expect(page.locator("#audit-stats")).toBeVisible();
  await expect(page.locator("#audit-stat-total")).toContainText("Interactions (24 h): 8");
  await expect(page.locator("#audit-stat-recovery")).toContainText("90s / 90s (1 faults)");
  await expect(page.locator("#audit-chart li")).toHaveCount(2);
  await expect(page.locator("#audit-export-btn")).toBeVisible();

  const axe = await new AxeBuilder({ page }).include("#panel-audit").analyze();
  expect(axe.violations).toEqual([]);
});

test("non-admin: 403 probe hides stats, chart, and export without an error", async ({ page }) => {
  await stubAggregates403(page);
  await page.route("**/api/v1/workspaces/*/audit_events*", (route) =>
    route.fulfill({ contentType: "application/json", body: JSON.stringify({ items: [auditEvent()], next_cursor: null }) })
  );

  await page.goto("/");
  await page.locator("#tab-audit").click();
  await expect(page.locator("#audit-rows tr").first()).toContainText("peer_tool_executed");

  await expect(page.locator("#audit-stats")).toBeHidden();
  await expect(page.locator("#audit-chart")).toBeHidden();
  await expect(page.locator("#audit-export-btn")).toBeHidden();
  await expect(page.locator("#audit-status")).toHaveText("");
});

test("export follows X-Next-Cursor with a frozen window and downloads NDJSON", async ({ page }) => {
  await page.route("**/api/v1/workspaces/*/audit_events*", (route) =>
    route.fulfill({ contentType: "application/json", body: JSON.stringify({ items: [auditEvent()], next_cursor: null }) })
  );
  await page.route("**/api/v1/workspaces/*/audit-aggregates*", (route) =>
    route.fulfill({ contentType: "application/json", body: JSON.stringify({ op: "count_by_bucket", bucket: "hour", groups: [] }) })
  );
  await page.route("**/api/v1/workspaces/*/pipeline-aggregates*", (route) =>
    route.fulfill({ contentType: "application/json", body: JSON.stringify({ op: "recovery_gap", items: [], summary: { count: 0, p50: null, p90: null, max: null } }) })
  );

  const exportUrls: URL[] = [];
  await page.route("**/api/v1/workspaces/*/audit-export*", (route) => {
    exportUrls.push(new URL(route.request().url()));
    const firstPage = exportUrls.length === 1;
    route.fulfill({
      contentType: "application/x-ndjson",
      headers: firstPage ? { "X-Next-Cursor": "export-cursor-1" } : {},
      body: JSON.stringify(auditEvent()) + "\n"
    });
  });

  await page.goto("/");
  await page.locator("#tab-audit").click();
  await expect(page.locator("#audit-export-btn")).toBeVisible();

  const downloadPromise = page.waitForEvent("download");
  await page.locator("#audit-export-btn").click();
  const download = await downloadPromise;

  expect(download.suggestedFilename()).toBe(`audit-events-${WS_ID}.ndjson`);
  await expect.poll(() => exportUrls.length).toBe(2);
  expect(exportUrls[0].searchParams.has("cursor")).toBe(false);
  expect(exportUrls[1].searchParams.get("cursor")).toBe("export-cursor-1");
  // Frozen window: both pages carry identical since/until.
  expect(exportUrls[1].searchParams.get("since")).toBe(exportUrls[0].searchParams.get("since"));
  expect(exportUrls[1].searchParams.get("until")).toBe(exportUrls[0].searchParams.get("until"));
  expect(exportUrls[0].searchParams.get("until")).not.toBeNull();
  await expect(page.locator("#audit-status")).toHaveText("");
  await expect(page.locator("#audit-export-btn")).toBeEnabled();
});

test("export failure surfaces status text and re-enables the button", async ({ page }) => {
  await page.route("**/api/v1/workspaces/*/audit_events*", (route) =>
    route.fulfill({ contentType: "application/json", body: JSON.stringify({ items: [], next_cursor: null }) })
  );
  await page.route("**/api/v1/workspaces/*/audit-aggregates*", (route) =>
    route.fulfill({ contentType: "application/json", body: JSON.stringify({ op: "count_by_bucket", bucket: "hour", groups: [] }) })
  );
  await page.route("**/api/v1/workspaces/*/pipeline-aggregates*", (route) =>
    route.fulfill({ contentType: "application/json", body: JSON.stringify({ op: "recovery_gap", items: [], summary: { count: 0, p50: null, p90: null, max: null } }) })
  );
  await page.route("**/api/v1/workspaces/*/audit-export*", (route) =>
    route.fulfill({ status: 500, contentType: "application/json", body: JSON.stringify({ error: "internal" }) })
  );

  await page.goto("/");
  await page.locator("#tab-audit").click();
  await expect(page.locator("#audit-export-btn")).toBeVisible();
  await page.locator("#audit-export-btn").click();

  await expect(page.locator("#audit-status")).toContainText("Export failed");
  await expect(page.locator("#audit-export-btn")).toBeEnabled();
});
