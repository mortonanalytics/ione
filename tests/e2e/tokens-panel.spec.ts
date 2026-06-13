import { test, expect } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";

const WS_ID = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
const TOKEN_ID = "ffffffff-ffff-ffff-ffff-ffffffffffff";

function tokensPayload() {
  return {
    items: [
      {
        id: TOKEN_ID,
        orgId: "00000000-0000-0000-0000-000000000001",
        name: "mission-launcher",
        permissions: ["provisioning:apply", "workspace:write"],
        provisionableMaxCoc: 50,
        createdBy: null,
        expiresAt: null,
        revokedAt: null,
        lastUsedAt: null,
        createdAt: "2026-06-12T00:00:00Z",
        updatedAt: "2026-06-12T00:00:00Z"
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
        items: [{ id: WS_ID, name: "Operations", domain: "test", lifecycle: "continuous", closedAt: null }]
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
  // Roles/policies probes (other panels) — keep them denied to avoid noise.
  await page.route("**/api/v1/workspaces/*/roles*", (route) =>
    route.fulfill({ status: 403, contentType: "application/json", body: JSON.stringify({ error: "forbidden" }) })
  );
});

/** AC-12: non-service_accounts:manage member — Tokens tab absent, exactly one 403 probe. */
test("non-manager: 403 probe hides the Tokens tab and no further token calls", async ({ page }) => {
  let tokenCalls = 0;
  await page.route("**/api/v1/service-account-tokens", (route) => {
    tokenCalls += 1;
    route.fulfill({ status: 403, contentType: "application/json", body: JSON.stringify({ error: "forbidden" }) });
  });

  await page.goto("/");
  await expect.poll(() => tokenCalls).toBe(1);
  await expect(page.locator("#tab-tokens")).toBeHidden();
  await expect(page.locator("#panel-tokens")).toBeHidden();

  // Moving around the shell must not re-call the token endpoint.
  await page.locator("#tab-connectors").click();
  await page.locator("#tab-chat").click();
  await page.waitForTimeout(250);
  expect(tokenCalls).toBe(1);
});

test("manager: probe reveals the tab; cards render permissions", async ({ page }) => {
  await page.route("**/api/v1/service-account-tokens", (route) =>
    route.fulfill({ contentType: "application/json", body: JSON.stringify(tokensPayload()) })
  );

  await page.goto("/");
  await expect(page.locator("#tab-tokens")).toBeVisible();
  await page.locator("#tab-tokens").click();

  const cards = page.locator("#tokens-list .token-card");
  await expect(cards).toHaveCount(1);
  await expect(cards.first()).toContainText("mission-launcher");
  await expect(cards.first()).toContainText("provisioning:apply");

  const axe = await new AxeBuilder({ page }).include("#panel-tokens").analyze();
  expect(axe.violations).toEqual([]);
});

test("issue surfaces the plaintext once in a copy-once modal", async ({ page }) => {
  let posted = false;
  await page.route("**/api/v1/service-account-tokens", (route) => {
    const req = route.request();
    if (req.method() === "POST") {
      posted = true;
      route.fulfill({
        status: 201,
        contentType: "application/json",
        body: JSON.stringify({
          id: TOKEN_ID,
          token: "ione_sat_PLAINTEXTSHOWNONCE",
          name: "new-token",
          permissions: ["provisioning:apply"],
          provisionableMaxCoc: 0,
          expiresAt: null
        })
      });
    } else {
      route.fulfill({ contentType: "application/json", body: JSON.stringify(tokensPayload()) });
    }
  });

  await page.goto("/");
  await expect(page.locator("#tab-tokens")).toBeVisible();
  await page.locator("#tab-tokens").click();

  await page.locator("#token-name").fill("new-token");
  await page.locator('#token-permissions input[data-perm="provisioning:apply"]').check();
  await page.locator("#token-issue-btn").click();

  await expect.poll(() => posted).toBe(true);
  await expect(page.locator("#token-secret-modal")).toBeVisible();
  await expect(page.locator("#token-secret-value")).toHaveText("ione_sat_PLAINTEXTSHOWNONCE");

  await page.locator("#token-secret-close-btn").click();
  await expect(page.locator("#token-secret-modal")).toBeHidden();
});

test("revoke wires DELETE and refreshes the list", async ({ page }) => {
  let deleted: string | null = null;
  let revoked = false;
  await page.route("**/api/v1/service-account-tokens/*", (route) => {
    deleted = route.request().url();
    revoked = true;
    route.fulfill({ status: 204, body: "" });
  });
  await page.route("**/api/v1/service-account-tokens", (route) => {
    const body = revoked ? { items: [] } : tokensPayload();
    route.fulfill({ contentType: "application/json", body: JSON.stringify(body) });
  });

  await page.goto("/");
  await expect(page.locator("#tab-tokens")).toBeVisible();
  await page.locator("#tab-tokens").click();
  await expect(page.locator("#tokens-list .token-card")).toHaveCount(1);

  await page.locator(".token-revoke-btn").click();
  await expect(page.locator("#tokens-status")).toHaveText("Token revoked.");
  await expect(page.locator("#tokens-list .token-card")).toHaveCount(0);
  expect(deleted).toContain(`/service-account-tokens/${TOKEN_ID}`);
});
