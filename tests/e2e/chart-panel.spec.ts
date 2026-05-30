import { test, expect, type Page } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";

const STREAM_ID = "33333333-3333-3333-3333-333333333333";
const PEER_ID = "11111111-1111-1111-1111-111111111111";

function ioneChart() {
  return {
    id: "ione-chart-1",
    name: "Earthquake magnitude",
    source: "ione",
    spec: {
      chartType: "line",
      xAxis: "bucket_start",
      yAxis: "Magnitude",
      series: ["avg"]
    },
    descriptor: {
      streamId: STREAM_ID,
      op: "avg",
      bucket: "day",
      valuePointer: "/properties/mag"
    }
  };
}

function peerChart() {
  return {
    id: "peer-chart-1",
    name: "Peer magnitude",
    source: "peer",
    peerId: PEER_ID,
    peerName: "Chart Peer",
    uri: "stub://chart/1",
    spec: {
      chartType: "line",
      xAxis: "bucket_start",
      yAxis: "Magnitude",
      series: ["value"]
    }
  };
}

const rows = [
  { bucketStart: "2026-05-27T00:00:00Z", bucketStartMs: 1780012800000, avg: 4.2, value: 4.2 },
  { bucketStart: "2026-05-28T00:00:00Z", bucketStartMs: 1780099200000, avg: 4.4, value: 4.4 }
];

async function stubChartPanels(page: Page, body: Record<string, unknown>) {
  await page.route("**/api/v1/workspaces/*/chart-panels*", (route) =>
    route.fulfill({ contentType: "application/json", body: JSON.stringify(body) })
  );
}

test("skeleton: Charts tab loads myIO and opens panel", async ({ page }) => {
  await stubChartPanels(page, { ioneCharts: [], peerCharts: [], peerErrors: [] });
  await page.goto("/");

  await expect.poll(() => page.evaluate(() => typeof (window as any).myIOchart)).toBe("function");
  await page.locator("#tab-chart").click();

  await expect(page.locator("#tab-chart")).toHaveAttribute("aria-selected", "true");
  await expect(page.locator("#panel-chart")).toBeVisible();
});

test("spike: myIOchart renders a static fixture and destroys cleanly", async ({ page }) => {
  await stubChartPanels(page, { ioneCharts: [], peerCharts: [], peerErrors: [] });
  await page.goto("/");
  await page.locator("#tab-chart").click();

  const rendered = await page.evaluate(() => {
    const target = document.getElementById("chart-myio-target")!;
    const config = (window as any).IoneChartAdapter.ioneToMyio(
      { chartType: "line", xAxis: "bucket_start", yAxis: "Magnitude", series: ["value"] },
      [
        { bucketStart: "2026-05-27T00:00:00Z", bucketStartMs: 1780012800000, value: 4.2 },
        { bucketStart: "2026-05-28T00:00:00Z", bucketStartMs: 1780099200000, value: 4.4 }
      ]
    );
    const chart = new (window as any).myIOchart({
      element: target,
      config,
      width: 640,
      height: 360
    });
    const hasChild = target.childElementCount > 0;
    chart.destroy();
    return hasChild && target.childElementCount === 0;
  });
  expect(rendered).toBe(true);
});

test("AC-13: selecting an IONe chart renders chart and data table", async ({ page }) => {
  await stubChartPanels(page, { ioneCharts: [ioneChart()], peerCharts: [], peerErrors: [] });
  await page.route("**/api/v1/workspaces/*/event-aggregates*", (route) =>
    route.fulfill({
      contentType: "application/json",
      body: JSON.stringify({ op: "avg", bucket: "day", rows, truncated: false })
    })
  );
  await page.goto("/");
  await page.locator("#tab-chart").click();
  await page.locator("#chart-list .chart-row").first().click();

  await expect(page.locator("#chart-myio-target").locator("svg, canvas").first()).toBeVisible();
  await expect(page.locator("#chart-data-disclosure")).toBeVisible();
  await expect(page.locator("#chart-data-disclosure tbody tr")).toHaveCount(rows.length);

  const axe = await new AxeBuilder({ page }).include("#panel-chart").analyze();
  expect(axe.violations).toEqual([]);
});

test("peer chart selection reads chart-data and renders", async ({ page }) => {
  await stubChartPanels(page, { ioneCharts: [], peerCharts: [peerChart()], peerErrors: [] });
  await page.route("**/api/v1/workspaces/*/chart-data*", (route) => {
    expect(route.request().url()).toContain(`peer_id=${PEER_ID}`);
    route.fulfill({
      contentType: "application/json",
      body: JSON.stringify({
        spec: peerChart().spec,
        rows
      })
    });
  });
  await page.goto("/");
  await page.locator("#tab-chart").click();
  await page.locator("#chart-list .chart-row").first().click();

  await expect(page.locator("#chart-myio-target").locator("svg, canvas").first()).toBeVisible();
  await expect(page.locator("#chart-data-disclosure tbody tr")).toHaveCount(rows.length);
});
