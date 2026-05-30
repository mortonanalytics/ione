import { test, expect, type Page } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";

const WORKSPACE_ID = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
const PEER_ID = "11111111-1111-1111-1111-111111111111";
const PDF_URL = "https://docs.example.test/report.pdf";
const DENIED_PDF_URL = "https://docs.example.test/blocked.pdf";
const CSV_URL = "https://docs.example.test/report.csv";
const PDF_BYTES = Buffer.from(
  "%PDF-1.4\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >>\nendobj\n4 0 obj\n<< /Length 44 >>\nstream\nBT /F1 12 Tf 40 120 Td (IONe document) Tj ET\nendstream\nendobj\nxref\n0 5\n0000000000 65535 f \n0000000009 00000 n \n0000000058 00000 n \n0000000115 00000 n \n0000000205 00000 n \ntrailer\n<< /Root 1 0 R /Size 5 >>\nstartxref\n299\n%%EOF\n",
  "utf8"
);

function pdfDocument(url = PDF_URL) {
  return {
    id: "peer-doc-1",
    name: "Incident report",
    source: "peer",
    peerId: PEER_ID,
    peerName: "Document Peer",
    uri: "stub://document/1",
    downloadUrl: url,
    mimeType: "application/pdf",
    fileSizeBytes: 2048,
    lastModified: "2026-05-29T12:00:00Z"
  };
}

function csvDocument() {
  return {
    id: "peer-doc-2",
    name: "Incident rows",
    source: "peer",
    peerId: PEER_ID,
    peerName: "Document Peer",
    uri: "stub://document/2",
    downloadUrl: CSV_URL,
    mimeType: "text/csv"
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
          id: WORKSPACE_ID,
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
  await page.route(PDF_URL, (route) =>
    route.fulfill({
      status: 200,
      contentType: "application/pdf",
      body: PDF_BYTES
    })
  );
  await page.route(DENIED_PDF_URL, (route) => route.abort("blockedbyclient"));
  await page.route(CSV_URL, (route) =>
    route.fulfill({
      status: 200,
      contentType: "text/csv",
      body: "id,name\n1,Incident\n"
    })
  );
});

async function stubDocumentPanels(page: Page, body: Record<string, unknown>) {
  await page.route("**/api/v1/workspaces/*/document-panels*", (route) =>
    route.fulfill({ contentType: "application/json", body: JSON.stringify(body) })
  );
}

test("skeleton: Documents tab opens and keyboard order includes it", async ({ page }) => {
  await stubDocumentPanels(page, { peerDocuments: [], peerErrors: [] });
  await page.goto("/");

  await page.locator("#tab-document").click();
  await expect(page.locator("#tab-document")).toHaveAttribute("aria-selected", "true");
  await expect(page.locator("#panel-document")).toBeVisible();

  await page.locator("#tab-table").focus();
  await page.keyboard.press("ArrowRight");
  await expect(page.locator("#tab-document")).toBeFocused();
  await expect(page.locator("#tab-document")).toHaveAttribute("aria-selected", "true");
});

test("sandbox-spike: Chromium loads the planned PDF sandbox rung", async ({ page }) => {
  await stubDocumentPanels(page, { peerDocuments: [], peerErrors: [] });
  await page.goto("/");

  const pdfRequested = page.waitForRequest(PDF_URL);
  const rung = await page.evaluate(async (url) => {
    const iframe = document.createElement("iframe");
    iframe.setAttribute("sandbox", "allow-downloads");
    iframe.setAttribute("referrerpolicy", "no-referrer");
    iframe.src = url;
    document.body.appendChild(iframe);
    await new Promise((resolve) => window.setTimeout(resolve, 500));
    if (!iframe.contentDocument) {
      iframe.setAttribute("sandbox", "allow-downloads allow-same-origin");
    }
    const sandbox = iframe.getAttribute("sandbox") || "";
    iframe.remove();
    return sandbox;
  }, PDF_URL);

  await pdfRequested;
  expect(rung).toBe("allow-downloads allow-same-origin");
});

test("AC-5 and AC-8: PDF renders inline with sandbox and link security", async ({ page }) => {
  await stubDocumentPanels(page, { peerDocuments: [pdfDocument()], peerErrors: [] });
  const proxyRequests: string[] = [];
  page.on("request", (request) => {
    const url = request.url();
    if (url.includes("/proxy") || url.includes("/document-data")) proxyRequests.push(url);
  });

  await page.goto("/");
  await page.locator("#tab-document").click();
  await page.locator("#document-list .document-row").first().click();

  const iframe = page.locator("#document-frame-container iframe");
  await expect(iframe).toHaveAttribute("src", PDF_URL);
  await expect(iframe).toHaveAttribute("sandbox", /allow-downloads/);
  await expect(iframe).toHaveAttribute("sandbox", /allow-same-origin/);
  await expect(iframe).not.toHaveAttribute("sandbox", /allow-scripts/);
  await expect(iframe).toHaveAttribute("referrerpolicy", "no-referrer");
  await expect(iframe).toHaveAttribute("title", /Incident report - PDF document/);
  await expect(page.locator("#document-toolbar a[target='_blank']")).toHaveCount(2);
  await expect(page.locator("#document-frame-container iframe a.document-fallback-link")).toHaveCount(1);

  const rels = await page.locator("#document-toolbar a").evaluateAll((links) =>
    links.map((link) => link.getAttribute("rel") || "")
  );
  expect(rels.every((rel) => rel.includes("noopener") && rel.includes("noreferrer"))).toBe(true);
  await expect(page.locator("#document-toolbar a").first()).toHaveAttribute("aria-label", /opens in new tab/);
  expect(proxyRequests).toEqual([]);
  await page.waitForTimeout(3500);
  await expect(page.locator("#document-frame-container iframe")).toHaveCount(1);
  await expect(page.locator("#document-notice")).toBeHidden();

  const axe = await new AxeBuilder({ page }).include("#panel-document").analyze();
  expect(axe.violations).toEqual([]);
});

test("AC-6: non-PDF renders link card without iframe", async ({ page }) => {
  await stubDocumentPanels(page, { peerDocuments: [csvDocument()], peerErrors: [] });
  await page.goto("/");
  await page.locator("#tab-document").click();
  await page.locator("#document-list .document-row").first().click();

  await expect(page.locator("#document-frame-container iframe")).toHaveCount(0);
  const link = page.locator("#document-link-card a.document-primary-link");
  await expect(link).toHaveAttribute("href", CSV_URL);
  await expect(link).toHaveAttribute("target", "_blank");
  const rel = await link.getAttribute("rel");
  expect(rel).toContain("noopener");
  expect(rel).toContain("noreferrer");
});

test("AC-7: blocked PDF request falls back to link card", async ({ page }) => {
  await stubDocumentPanels(page, { peerDocuments: [pdfDocument(DENIED_PDF_URL)], peerErrors: [] });
  await page.goto("/");
  await page.locator("#tab-document").click();
  await page.locator("#document-list .document-row").first().click();

  await expect(page.locator("#document-notice")).toContainText("could not be displayed inline", { timeout: 5000 });
  await expect(page.locator("#document-frame-container iframe")).toHaveCount(0);
  const link = page.locator("#document-link-card a.document-primary-link");
  await expect(link).toHaveAttribute("href", DENIED_PDF_URL);
  await expect(link).toHaveAttribute("target", "_blank");
});
