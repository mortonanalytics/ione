import { test, expect } from "@playwright/test";

const workspace = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
const mapping = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb";
const run = "cccccccc-cccc-cccc-cccc-cccccccccccc";

test.beforeEach(async ({ page }) => {
  await page.route("**/api/**", (route) => route.fulfill({ json: { items: [] } }));
  await page.route("**/api/v1/me", (route) => route.fulfill({ json: { user: { email: "default@localhost", displayName: "Default" } } }));
  await page.route("**/api/v1/workspaces", (route) => route.fulfill({ json: { items: [{ id: workspace, name: "Operations", domain: "test", lifecycle: "continuous", closedAt: null }] } }));
  await page.route("**/relay/sources", (route) => route.fulfill({ json: { sources: [{ mappingId: mapping, displayName: "Fixture", alias: "pg", entities: [] }] } }));
  await page.route("**/relay/runs/*/receipts", (route) => route.fulfill({ json: { sources: [{ alias: "pg", receipt: {} }], usage: {} } }));
  await page.route("**/relay/runs/*/result", (route) => route.fulfill({ json: { schema: [{ name: "total", ty: "uint64" }], rows: [[{ t: "uint64", v: "18446744073709551615" }]], row_count: 1, truncated: false } }));
});

test("retry keeps identity, new ask gets a new identity, and missing receipts remain unknown", async ({ page }) => {
  const requests: { requestId: string }[] = [];
  await page.route("**/relay/ask", (route) => {
    requests.push(route.request().postDataJSON());
    return requests.length === 1
      ? route.fulfill({ status: 502, json: { error: "temporary failure" } })
      : route.fulfill({ json: { run: { id: run }, outcome: { kind: "succeeded" } } });
  });
  await page.goto("/");
  await page.locator("#tab-data").click();
  await page.locator("#data-source-list input").check();
  await page.locator("#data-ask-input").fill("Count rows");
  await page.locator("#data-ask-submit").click();
  await expect(page.locator("#data-error")).toBeVisible();
  await page.locator("#data-ask-submit").click();
  await expect(page.locator("#data-table tbody")).toHaveText("18446744073709551615");
  await expect(page.locator("#data-table tbody td")).toHaveClass("data-cell-numeric");
  await page.locator("#data-details summary").click();
  await expect(page.locator("#data-receipts")).toContainText("unknown rows");
  await expect(page.locator("#data-usage")).toContainText("unknown prompt tokens");
  await page.locator("#data-ask-submit").click();
  await expect.poll(() => requests.length).toBe(3);
  expect(requests[0].requestId).toBe(requests[1].requestId);
  expect(requests[1].requestId).not.toBe(requests[2].requestId);
});

test("clarification continues the same run and SSE success retrieves its typed result", async ({ page }) => {
  await page.route("**/relay/ask", (route) => route.fulfill({ json: { run: { id: run }, outcome: { kind: "clarification_required", seq: 1, question: "Which rows?", options: ["All rows"] } } }));
  let answered = false;
  await page.route("**/relay/runs/*/clarification", (route) => {
    expect(route.request().url()).toContain(run);
    expect(route.request().postDataJSON()).toEqual({ seq: 1, answer: "All rows" });
    answered = true;
    return route.fulfill({ json: { run: { id: run }, outcome: { kind: "started" } } });
  });
  await page.route("**/relay/runs/*/events", (route) => route.fulfill({ contentType: "text/event-stream", body: "id: 1\nevent: succeeded\ndata: {}\n\n" }));
  await page.goto("/");
  await page.locator("#tab-data").click();
  await page.locator("#data-source-list input").check();
  await page.locator("#data-ask-input").fill("Count rows");
  await page.locator("#data-ask-submit").click();
  await expect(page.locator("#data-clarification")).toBeVisible();
  await page.getByRole("button", { name: "All rows", exact: true }).click();
  await page.locator("#data-clarification button[type=submit]").click();
  await expect(page.locator("#data-table tbody")).toHaveText("18446744073709551615");
  await expect(page.locator("#data-table tbody td")).toHaveClass("data-cell-numeric");
  expect(answered).toBe(true);
});
