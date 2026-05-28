import { test, expect, type Page } from "@playwright/test";

// Map-panel e2e gate. Stubs GET /api/v1/workspaces/:id/map-layers at the network
// layer, so no peer or DB seeding is needed — only a running IONe server in local
// auth mode (see playwright.config.ts header). Covers AC-6, 7, 8, 12, 14, 16, 17
// from md/design/map-view.md.

const TILE_URL = "https://tile.openstreetmap.org/{z}/{x}/{y}.png";

type MapItem = {
  peerId: string;
  peerName: string;
  uri: string;
  name: string;
  meta: Record<string, unknown>;
};

function mapItem(overrides: Partial<MapItem> = {}): MapItem {
  return {
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
    ...overrides,
  };
}

async function stubMapLayers(
  page: Page,
  body: { items: MapItem[]; peersOk: string[]; peersFailed: unknown[] }
) {
  await page.route("**/api/v1/workspaces/*/map-layers*", (route) =>
    route.fulfill({ contentType: "application/json", body: JSON.stringify(body) })
  );
}

// Keep the suite hermetic: never hit a real tile server, and stub the parallel
// /event-layers fetch (these specs assert raster behavior only).
test.beforeEach(async ({ page }) => {
  await page.route("**tile.openstreetmap.org/**", (route) => route.abort());
  await page.route("**/api/v1/workspaces/*/event-layers*", (route) =>
    route.fulfill({
      contentType: "application/json",
      body: JSON.stringify({ layers: [], streamsOk: [], streamsFailed: [], truncated: false, queriedAt: "2026-05-28T00:00:00Z" }),
    })
  );
});

async function openMap(page: Page) {
  await page.goto("/");
  await page.locator("#tab-map").click();
}

async function waitForRasterLayer(page: Page) {
  await page.waitForFunction(() => {
    const m = (window as any).mapInstance;
    return !!m && m.getStyle().layers.some((l: any) => l.type === "raster");
  });
}

test("AC-6/AC-12: map renders canvas, layer row, attribution, opacity", async ({ page }) => {
  await stubMapLayers(page, { items: [mapItem()], peersOk: [mapItem().peerId], peersFailed: [] });
  await openMap(page);

  await expect(page.locator("#map-canvas-container")).toBeVisible();
  await expect(page.locator("#map-empty")).toBeHidden();
  await expect(page.locator("#map-canvas canvas.maplibregl-canvas")).toBeVisible();
  await expect(page.locator("#map-layer-list .layer-row").first()).toContainText("World tiles");

  await waitForRasterLayer(page);
  const opacity = await page.evaluate(() => {
    const m = (window as any).mapInstance;
    const lyr = m.getStyle().layers.find((l: any) => l.type === "raster");
    return m.getPaintProperty(lyr.id, "raster-opacity");
  });
  expect(opacity).toBe(0.85);

  await expect(page.locator(".maplibregl-ctrl-attrib")).toContainText("OpenStreetMap");
});

test("AC-6b: layerName overrides name in the layer control label", async ({ page }) => {
  await stubMapLayers(page, {
    items: [mapItem({ meta: { ...mapItem().meta, layerName: "Custom Label" } })],
    peersOk: [mapItem().peerId],
    peersFailed: [],
  });
  await openMap(page);
  await expect(page.locator("#map-layer-list .layer-row").first()).toContainText("Custom Label");
  await expect(page.locator("#map-layer-list .layer-row").first()).not.toContainText("World tiles");
});

test("AC-7: empty state when no map layers and no failures", async ({ page }) => {
  await stubMapLayers(page, { items: [], peersOk: [], peersFailed: [] });
  await openMap(page);
  await expect(page.locator("#map-empty")).toBeVisible();
  await expect(page.locator("#map-canvas-container")).toBeHidden();
  await expect(page.locator("#map-connect-peer")).toBeVisible();
});

test("AC-17: all-peer-failure shows error state, not empty", async ({ page }) => {
  await stubMapLayers(page, {
    items: [],
    peersOk: [],
    peersFailed: [{ peerId: "22222222-2222-2222-2222-222222222222", peerName: "Down Peer", error: "timeout" }],
  });
  await openMap(page);
  await expect(page.locator("#map-error")).toBeVisible();
  await expect(page.locator("#map-empty")).toBeHidden();
});

test("AC-8: visibility toggle hides the raster layer", async ({ page }) => {
  await stubMapLayers(page, { items: [mapItem()], peersOk: [mapItem().peerId], peersFailed: [] });
  await openMap(page);
  await waitForRasterLayer(page);

  await page.locator("#map-layer-list .layer-row input[type=checkbox]").first().uncheck();
  const vis = await page.evaluate(() => {
    const m = (window as any).mapInstance;
    const lyr = m.getStyle().layers.find((l: any) => l.type === "raster");
    return m.getLayoutProperty(lyr.id, "visibility");
  });
  expect(vis).toBe("none");
});

test("AC-16: peer attribution is escaped, no markup injected", async ({ page }) => {
  const evil = '<img src=x onerror="window.__xss=1">';
  await stubMapLayers(page, {
    items: [mapItem({ meta: { ...mapItem().meta, attribution: evil } })],
    peersOk: [mapItem().peerId],
    peersFailed: [],
  });
  await openMap(page);
  await waitForRasterLayer(page);

  expect(await page.locator(".maplibregl-ctrl-attrib img").count()).toBe(0);
  expect(await page.evaluate(() => (window as any).__xss)).toBeUndefined();
  // The escaped text renders literally.
  await expect(page.locator(".maplibregl-ctrl-attrib")).toContainText("onerror");
});

test("AC-14: chat resource chip navigates to map and highlights the layer", async ({ page }) => {
  await stubMapLayers(page, { items: [mapItem()], peersOk: [mapItem().peerId], peersFailed: [] });
  await openMap(page);
  await waitForRasterLayer(page);

  // Render a transcript message referencing the loaded layer URI, then go to Chat.
  await page.locator("#tab-chat").click();
  await page.evaluate(() => (window as any).appendMessage("assistant", "result at stub://layer/world"));

  const viewBtn = page.locator('.chip-view-map[data-uri="stub://layer/world"]');
  await expect(viewBtn).toBeVisible();
  await viewBtn.click();

  await expect(page.locator("#tab-map")).toHaveAttribute("aria-selected", "true");
  const row = page.locator('#map-layer-list .layer-row[data-uri="stub://layer/world"]');
  await expect(row).toHaveClass(/layer-row--highlight/);
  // Highlight clears after 1500 ms.
  await expect(row).not.toHaveClass(/layer-row--highlight/, { timeout: 3000 });
});
