import { test, expect, type Page } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";

const WS_ID = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
const ROLE_ADMIN = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb";
const ROLE_VIEWER = "cccccccc-cccc-cccc-cccc-cccccccccccc";

function rolesPayload() {
  return {
    items: [
      {
        id: ROLE_ADMIN,
        workspaceId: WS_ID,
        name: "admin",
        cocLevel: 80,
        permissions: ["admin"],
        memberCount: 2
      },
      {
        id: ROLE_VIEWER,
        workspaceId: WS_ID,
        name: "viewer",
        cocLevel: 0,
        permissions: ["audit:read"],
        memberCount: 0
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
});

/** AC-14: non-roles:manage member — Roles tab absent, exactly one 403 probe. */
test("non-manager: 403 probe hides the Roles tab and no further role calls", async ({ page }) => {
  let roleCalls = 0;
  await page.route("**/api/v1/workspaces/*/roles*", (route) => {
    roleCalls += 1;
    route.fulfill({ status: 403, contentType: "application/json", body: JSON.stringify({ error: "forbidden" }) });
  });

  await page.goto("/");
  // Let the probe settle, then assert the tab never appears.
  await expect.poll(() => roleCalls).toBe(1);
  await expect(page.locator("#tab-roles")).toBeHidden();
  await expect(page.locator("#panel-roles")).toBeHidden();

  // Moving around the shell must not re-call any role endpoint.
  await page.locator("#tab-connectors").click();
  await page.locator("#tab-chat").click();
  await page.waitForTimeout(250);
  expect(roleCalls).toBe(1);
});

test("manager: probe reveals the tab; cards render chips and member counts", async ({ page }) => {
  await page.route("**/api/v1/workspaces/*/roles*", (route) =>
    route.fulfill({ contentType: "application/json", body: JSON.stringify(rolesPayload()) })
  );

  await page.goto("/");
  await expect(page.locator("#tab-roles")).toBeVisible();
  await page.locator("#tab-roles").click();

  const cards = page.locator("#roles-list .role-card");
  await expect(cards).toHaveCount(2);
  await expect(cards.first()).toContainText("admin");
  await expect(cards.first()).toContainText("CoC 80 · 2 members");
  await expect(cards.last()).toContainText("viewer");
  await expect(cards.last()).toContainText("CoC 0 · 0 members");

  // Permission chips: the viewer card has audit:read toggled on.
  const viewerOn = cards.last().locator(".permission-chip--on");
  await expect(viewerOn).toHaveCount(1);
  await expect(viewerOn).toContainText("audit:read");

  // Membership role select is populated from the same payload.
  await expect(page.locator("#roles-membership-role option")).toHaveCount(2);

  const axe = await new AxeBuilder({ page }).include("#panel-roles").analyze();
  expect(axe.violations).toEqual([]);
});

test("saving permissions PUTs the toggled set and reloads", async ({ page }) => {
  const putBodies: unknown[] = [];
  await page.route("**/api/v1/workspaces/*/roles/*/permissions", (route) => {
    putBodies.push(route.request().postDataJSON());
    route.fulfill({
      contentType: "application/json",
      body: JSON.stringify({
        id: ROLE_VIEWER,
        workspaceId: WS_ID,
        name: "viewer",
        cocLevel: 0,
        permissions: ["audit:read", "approvals:decide"]
      })
    });
  });
  await page.route("**/api/v1/workspaces/*/roles*", (route) =>
    route.fulfill({ contentType: "application/json", body: JSON.stringify(rolesPayload()) })
  );

  await page.goto("/");
  await expect(page.locator("#tab-roles")).toBeVisible();
  await page.locator("#tab-roles").click();

  const viewerCard = page.locator("#roles-list .role-card").last();
  await viewerCard.locator(".permission-chip", { hasText: "approvals:decide" }).click();
  await viewerCard.locator(".role-save-btn").click();

  await expect(page.locator("#roles-status")).toHaveText("Permissions saved.");
  expect(putBodies).toHaveLength(1);
  const body = putBodies[0] as { permissions: string[] };
  expect(body.permissions).toContain("audit:read");
  expect(body.permissions).toContain("approvals:decide");
});

test("grant and revoke wire POST and DELETE with the entered user", async ({ page }) => {
  const calls: { method: string; url: string; body?: unknown }[] = [];
  await page.route(/\/api\/v1\/workspaces\/[^/]+\/memberships(\/|$)/, (route) => {
    const req = route.request();
    calls.push({ method: req.method(), url: req.url(), body: req.postDataJSON() });
    if (req.method() === "POST") {
      route.fulfill({ contentType: "application/json", body: JSON.stringify({ id: "dddddddd-dddd-dddd-dddd-dddddddddddd" }) });
    } else {
      route.fulfill({ status: 204, body: "" });
    }
  });
  await page.route("**/api/v1/workspaces/*/roles*", (route) =>
    route.fulfill({ contentType: "application/json", body: JSON.stringify(rolesPayload()) })
  );

  await page.goto("/");
  await expect(page.locator("#tab-roles")).toBeVisible();
  await page.locator("#tab-roles").click();

  const userId = "eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee";
  await page.locator("#roles-membership-user").fill(userId);
  await page.locator("#roles-membership-role").selectOption(ROLE_VIEWER);
  await page.locator("#roles-grant-btn").click();
  await expect(page.locator("#roles-status")).toHaveText("Membership granted.");

  await page.locator("#roles-revoke-btn").click();
  await expect(page.locator("#roles-status")).toHaveText("Membership revoked.");

  const post = calls.find((c) => c.method === "POST");
  expect(post?.body).toEqual({ user_id: userId, role_id: ROLE_VIEWER });
  const del = calls.find((c) => c.method === "DELETE");
  expect(del?.url).toContain(`/memberships/${userId}/${ROLE_VIEWER}`);
});
