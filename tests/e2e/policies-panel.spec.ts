import { test, expect } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";

const WS_ID = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
const POLICY_ID = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb";
const CONNECTOR_ID = "cccccccc-cccc-cccc-cccc-cccccccccccc";

function policiesPayload() {
  return {
    items: [
      {
        id: POLICY_ID,
        workspace_id: WS_ID,
        name: "auto-file spot weather",
        trigger: { signal_title_prefix: "Spot weather update", severity_at_most: "flagged" },
        connector_id: CONNECTOR_ID,
        op: "send",
        args_template: { text: "{{signal.title}}: {{signal.body}}" },
        rate_limit_per_min: 5,
        severity_cap: "flagged",
        authorized_by_permission: "approvals:decide",
        enabled: true,
        created_by: "dddddddd-dddd-dddd-dddd-dddddddddddd",
        created_at: "2026-06-01T00:00:00Z",
        updated_at: "2026-06-01T00:00:00Z"
      }
    ]
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
  // The roles probe also fires on workspace activation; keep it denied so it
  // never interferes with the policies assertions.
  await page.route("**/api/v1/workspaces/*/roles*", (route) =>
    route.fulfill({ status: 403, contentType: "application/json", body: JSON.stringify({ error: "forbidden" }) })
  );
  await page.route("**/api/v1/workspaces/*/connectors", (route) =>
    route.fulfill({
      contentType: "application/json",
      body: JSON.stringify({ items: [{ id: CONNECTOR_ID, name: "slack", status: "active" }] })
    })
  );
});

/** AC-11: non-approvals:decide member — Policies tab absent, exactly one 403 probe. */
test("non-holder: 403 probe hides the Policies tab and no further policy calls", async ({ page }) => {
  let policyCalls = 0;
  await page.route("**/api/v1/workspaces/*/auto-exec-policies*", (route) => {
    policyCalls += 1;
    route.fulfill({ status: 403, contentType: "application/json", body: JSON.stringify({ error: "forbidden" }) });
  });

  await page.goto("/");
  // Let the probe settle, then assert the tab never appears.
  await expect.poll(() => policyCalls).toBe(1);
  await expect(page.locator("#tab-policies")).toBeHidden();
  await expect(page.locator("#panel-policies")).toBeHidden();

  // Moving around the shell must not re-call any policy endpoint.
  await page.locator("#tab-connectors").click();
  await page.locator("#tab-chat").click();
  await page.waitForTimeout(250);
  expect(policyCalls).toBe(1);
});

test("holder: probe reveals the tab; policy cards render trigger, cap, and rate", async ({ page }) => {
  await page.route("**/api/v1/workspaces/*/auto-exec-policies*", (route) =>
    route.fulfill({ contentType: "application/json", body: JSON.stringify(policiesPayload()) })
  );

  await page.goto("/");
  await expect(page.locator("#tab-policies")).toBeVisible();
  await page.locator("#tab-policies").click();

  const cards = page.locator("#policies-list .policy-card");
  await expect(cards).toHaveCount(1);
  await expect(cards.first()).toContainText("auto-file spot weather");
  await expect(cards.first()).toContainText('"Spot weather update…" ≤ flagged');
  await expect(cards.first()).toContainText("cap flagged");
  await expect(cards.first()).toContainText("5/min");
  await expect(cards.first()).toContainText("enabled");

  const axe = await new AxeBuilder({ page }).include("#panel-policies").analyze();
  expect(axe.violations).toEqual([]);
});

test("creating a policy POSTs the form body and reloads the list", async ({ page }) => {
  const postBodies: unknown[] = [];
  await page.route("**/api/v1/workspaces/*/auto-exec-policies*", (route) => {
    const req = route.request();
    if (req.method() === "POST") {
      postBodies.push(req.postDataJSON());
      route.fulfill({
        contentType: "application/json",
        body: JSON.stringify(policiesPayload().items[0])
      });
      return;
    }
    route.fulfill({ contentType: "application/json", body: JSON.stringify(policiesPayload()) });
  });

  await page.goto("/");
  await expect(page.locator("#tab-policies")).toBeVisible();
  await page.locator("#tab-policies").click();

  await page.locator("#policy-name").fill("auto-file spot weather");
  await page.locator("#policy-trigger-prefix").fill("Spot weather update");
  await page.locator("#policy-trigger-severity").selectOption("flagged");
  await page.locator("#policy-connector").selectOption(CONNECTOR_ID);
  await page.locator("#policy-cap").selectOption("flagged");
  await page.locator("#policy-permission").fill("approvals:decide");
  await page.locator("#policy-save-btn").click();

  await expect(page.locator("#policies-status")).toHaveText("Policy created.");
  expect(postBodies).toHaveLength(1);
  const body = postBodies[0] as {
    name: string;
    trigger: { signal_title_prefix: string; severity_at_most: string };
    connector_id: string;
    rate_limit_per_min: number;
    severity_cap: string;
    authorized_by_permission: string;
  };
  expect(body.name).toBe("auto-file spot weather");
  expect(body.trigger.signal_title_prefix).toBe("Spot weather update");
  expect(body.trigger.severity_at_most).toBe("flagged");
  expect(body.connector_id).toBe(CONNECTOR_ID);
  expect(body.rate_limit_per_min).toBe(5);
  expect(body.severity_cap).toBe("flagged");
  expect(body.authorized_by_permission).toBe("approvals:decide");
});
