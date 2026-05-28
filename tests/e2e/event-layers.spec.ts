import { test, expect } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";

// Event-layer e2e gate (AC-8 from md/design/event-point-layer.md). Stubs both
// /map-layers and /event-layers at the network layer, so no DB seeding is needed
// — only a running IONe server in local auth mode (see playwright.config.ts).

const TILE_URL = "https://tile.openstreetmap.org/{z}/{x}/{y}.png";
const STREAM_ID = "33333333-3333-3333-3333-333333333333";

function rasterBody() {
  return {
    items: [
      {
        peerId: "11111111-1111-1111-1111-111111111111",
        peerName: "Stub Peer",
        uri: "stub://layer/world",
        name: "World tiles",
        meta: {
          tileUrl: TILE_URL,
          bounds: [-180, -85, 180, 85],
          attribution: "© OpenStreetMap contributors",
          layerName: null,
          opacity: 0.85,
          vectorUrl: null,
        },
      },
    ],
    peersOk: ["11111111-1111-1111-1111-111111111111"],
    peersFailed: [],
  };
}

function eventBody() {
  const features = Array.from({ length: 5 }, (_, i) => ({
    type: "Feature",
    geometry: { type: "Point", coordinates: [-122 + i, 37 + i] },
    properties: { mag: 3 + i, _event_id: `evt-${i}`, _observed_at: "2026-05-28T00:00:00Z" },
  }));
  return {
    layers: [
      {
        streamId: STREAM_ID,
        streamName: "Earthquakes",
        attribution: "USGS",
        featuresSkipped: 0,
        collection: { type: "FeatureCollection", features },
        style: {
          sizeField: "mag",
          sizeDomain: [2.5, 7.5],
          sizeRange: [4, 22],
          colorField: "mag",
          colorDomain: [3, 5, 7],
          colorRange: ["#f5d76e", "#d9534f", "#3a0ca3"],
          labelField: null,
        },
      },
    ],
    streamsOk: [STREAM_ID],
    streamsFailed: [],
    truncated: false,
    queriedAt: "2026-05-28T00:00:00Z",
  };
}

test.beforeEach(async ({ page }) => {
  await page.route("**tile.openstreetmap.org/**", (route) => route.abort());
  await page.route("**/api/v1/workspaces/*/map-layers*", (route) =>
    route.fulfill({ contentType: "application/json", body: JSON.stringify(rasterBody()) })
  );
  await page.route("**/api/v1/workspaces/*/event-layers*", (route) =>
    route.fulfill({ contentType: "application/json", body: JSON.stringify(eventBody()) })
  );
});

test("raster and event circles coexist with correct z-order and Events badge", async ({ page }) => {
  await page.goto("/");
  await page.locator("#tab-map").click();

  await expect(page.locator("#map-canvas-container")).toBeVisible();

  // Both a raster and a circle layer end up on the map.
  await page.waitForFunction(() => {
    const m = (window as any).mapInstance;
    if (!m) return false;
    const layers = m.getStyle().layers;
    return layers.some((l: any) => l.type === "raster") && layers.some((l: any) => l.type === "circle");
  });

  // Z-order: the circle layer is added after (draws above) the raster layer.
  const zorder = await page.evaluate(() => {
    const layers = (window as any).mapInstance.getStyle().layers;
    const raster = layers.findIndex((l: any) => l.type === "raster");
    const circle = layers.findIndex((l: any) => l.type === "circle");
    return { raster, circle };
  });
  expect(zorder.circle).toBeGreaterThan(zorder.raster);

  // Layer control lists both rows; raster row unchanged, event row badged.
  await expect(page.locator("#map-layer-list .layer-row").filter({ hasText: "World tiles" })).toBeVisible();
  const eventRow = page.locator("#map-layer-list .layer-row--event");
  await expect(eventRow).toContainText("Earthquakes");
  await expect(eventRow.locator(".layer-type-badge")).toHaveText("Events");
});

test("unchecking the event row hides the circle layer", async ({ page }) => {
  await page.goto("/");
  await page.locator("#tab-map").click();

  await page.waitForFunction(() => {
    const m = (window as any).mapInstance;
    return !!m && m.getStyle().layers.some((l: any) => l.type === "circle");
  });

  await page.locator("#map-layer-list .layer-row--event input[type=checkbox]").uncheck();
  const vis = await page.evaluate(() => {
    const m = (window as any).mapInstance;
    const lyr = m.getStyle().layers.find((l: any) => l.type === "circle");
    return m.getLayoutProperty(lyr.id, "visibility");
  });
  expect(vis).toBe("none");
});

test("event-only workspace renders circles when there are no raster layers", async ({ page }) => {
  // Override the raster stub with an empty body — this is the Phase 1 goal:
  // the map must render even when /map-layers returns nothing.
  await page.route("**/api/v1/workspaces/*/map-layers*", (route) =>
    route.fulfill({
      contentType: "application/json",
      body: JSON.stringify({ items: [], peersOk: [], peersFailed: [] }),
    })
  );

  await page.goto("/");
  await page.locator("#tab-map").click();

  await expect(page.locator("#map-canvas-container")).toBeVisible();
  await expect(page.locator("#map-empty")).toBeHidden();

  await page.waitForFunction(() => {
    const m = (window as any).mapInstance;
    if (!m) return false;
    const layers = m.getStyle().layers;
    return layers.some((l: any) => l.type === "circle") && !layers.some((l: any) => l.type === "raster");
  });

  await expect(page.locator("#map-layer-list .layer-row--event")).toContainText("Earthquakes");
});

test("raster stays alive when event layers fail and error row has retry", async ({ page }) => {
  await page.unroute("**/api/v1/workspaces/*/event-layers*");
  await page.route("**/api/v1/workspaces/*/event-layers*", (route) =>
    route.fulfill({ status: 500, contentType: "application/json", body: JSON.stringify({ error: "boom" }) })
  );

  await page.goto("/");
  await page.locator("#tab-map").click();

  await expect(page.locator("#map-canvas-container")).toBeVisible();
  await page.waitForFunction(() => {
    const m = (window as any).mapInstance;
    return !!m && m.getStyle().layers.some((l: any) => l.type === "raster");
  });
  const errorRow = page.locator("#map-layer-list .layer-row--error").filter({ hasText: "Event layers unavailable" });
  await expect(errorRow).toBeVisible();
  await expect(errorRow).not.toHaveAttribute("role", /.+/);
  await expect(errorRow.locator("button")).toHaveText("Retry");
  await expect(page.locator("#event-layer-status")).toHaveText("Event layers could not be loaded.");
});

test("event list supports keyboard popup flow and map panel has no axe violations", async ({ page }) => {
  await page.goto("/");
  await page.locator("#tab-map").click();

  await page.waitForFunction(() => {
    const m = (window as any).mapInstance;
    return !!m && m.getStyle().layers.some((l: any) => l.type === "circle");
  });

  await expect(page.locator("#event-layer-legend")).toBeVisible();
  await expect(page.locator("#event-list-disclosure")).toBeVisible();
  await page.locator("#event-list-disclosure summary").click();
  const showButton = page.locator("#event-list-disclosure button", { hasText: "Show on map" }).first();
  await showButton.focus();
  await page.keyboard.press("Enter");

  const closeButton = page.locator(".event-popup-close");
  await expect(closeButton).toBeVisible();
  await expect(closeButton).toBeFocused();

  const accessibilityScanResults = await new AxeBuilder({ page })
    .include("#panel-map")
    .analyze();
  expect(accessibilityScanResults.violations).toEqual([]);
});

test("empty states distinguish no geo streams, quiet geo streams, and event-only data", async ({ page }) => {
  await page.route("**/api/v1/workspaces/*/map-layers*", (route) =>
    route.fulfill({
      contentType: "application/json",
      body: JSON.stringify({ items: [], peersOk: [], peersFailed: [] }),
    })
  );
  await page.unroute("**/api/v1/workspaces/*/event-layers*");
  await page.route("**/api/v1/workspaces/*/event-layers*", (route) =>
    route.fulfill({
      contentType: "application/json",
      body: JSON.stringify({ layers: [], streamsOk: [], streamsFailed: [], truncated: false, queriedAt: "2026-05-28T00:00:00Z" }),
    })
  );

  await page.goto("/");
  await page.locator("#tab-map").click();
  await expect(page.locator("#map-empty")).toBeVisible();
  await expect(page.locator("body")).not.toContainText("No events in last 24 h.");

  await page.unroute("**/api/v1/workspaces/*/event-layers*");
  await page.route("**/api/v1/workspaces/*/event-layers*", (route) =>
    route.fulfill({
      contentType: "application/json",
      body: JSON.stringify({
        ...eventBody(),
        layers: [{ ...eventBody().layers[0], collection: { type: "FeatureCollection", features: [] } }],
      }),
    })
  );
  await page.reload();
  await page.locator("#tab-map").click();
  await expect(page.locator("#map-canvas-container")).toBeVisible();
  await expect(page.locator("#event-layer-status")).toHaveText("No events in last 24 h.");

  await page.unroute("**/api/v1/workspaces/*/event-layers*");
  await page.route("**/api/v1/workspaces/*/event-layers*", (route) =>
    route.fulfill({ contentType: "application/json", body: JSON.stringify(eventBody()) })
  );
  await page.reload();
  await page.locator("#tab-map").click();
  await expect(page.locator("#map-canvas-container")).toBeVisible();
  await page.waitForFunction(() => {
    const m = (window as any).mapInstance;
    if (!m) return false;
    const layers = m.getStyle().layers;
    return layers.some((l: any) => l.type === "circle") && !layers.some((l: any) => l.type === "raster");
  });
});
