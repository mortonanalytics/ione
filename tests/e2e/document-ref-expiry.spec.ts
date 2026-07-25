import { test, expect, type Page } from "@playwright/test";

// Issue #18 — expired signed-URL handling.
//
// A peer's `download_url` is a short-lived signed URL the peer owns. IONe never
// proxies or stores the object, so an expiry is resolved by re-requesting the
// *ref* — a fresh `GET /document-panels`, which re-runs the peer fan-out — and
// rendering whatever URL the peer signs at that moment.

const WORKSPACE_ID = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
const PEER_ID = "11111111-1111-1111-1111-111111111111";
const EXPIRED_PDF_URL = "https://docs.example.test/report.pdf?sig=expired";
const FRESH_PDF_URL = "https://docs.example.test/report.pdf?sig=fresh";
const EXPIRED_CSV_URL = "https://docs.example.test/rows.csv?sig=expired";
const FRESH_CSV_URL = "https://docs.example.test/rows.csv?sig=fresh";
const PDF_BYTES = Buffer.from(
  "%PDF-1.4\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >>\nendobj\n4 0 obj\n<< /Length 44 >>\nstream\nBT /F1 12 Tf 40 120 Td (IONe document) Tj ET\nendstream\nendobj\nxref\n0 5\n0000000000 65535 f \n0000000009 00000 n \n0000000058 00000 n \n0000000115 00000 n \n0000000205 00000 n \ntrailer\n<< /Root 1 0 R /Size 5 >>\nstartxref\n299\n%%EOF\n",
  "utf8"
);

function peerDocument(url: string, mimeType = "application/pdf") {
  return {
    id: "peer-doc-1",
    name: "Incident report",
    source: "peer",
    peerId: PEER_ID,
    peerName: "Document Peer",
    uri: "stub://document/1",
    downloadUrl: url,
    mimeType
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
  // The expired signature is rejected by the peer's object store.
  await page.route(EXPIRED_PDF_URL, (route) => route.fulfill({ status: 403, body: "expired" }));
  await page.route(FRESH_PDF_URL, (route) =>
    route.fulfill({ status: 200, contentType: "application/pdf", body: PDF_BYTES })
  );
});

/** Serve one document-panels body per call, so a re-request can return a new ref. */
async function stubDocumentRefs(page: Page, bodies: Array<Record<string, unknown>>) {
  let call = 0;
  await page.route("**/api/v1/workspaces/*/document-panels*", (route) => {
    const body = bodies[Math.min(call, bodies.length - 1)];
    call += 1;
    route.fulfill({ contentType: "application/json", body: JSON.stringify(body) });
  });
}

test("an expired embed re-requests the ref and renders the freshly-signed URL", async ({ page }) => {
  await stubDocumentRefs(page, [
    { peerDocuments: [peerDocument(EXPIRED_PDF_URL)], peerErrors: [] },
    { peerDocuments: [peerDocument(FRESH_PDF_URL)], peerErrors: [] }
  ]);
  const proxyRequests: string[] = [];
  page.on("request", (request) => {
    const url = request.url();
    if (url.includes("/proxy") || url.includes("/document-data")) proxyRequests.push(url);
  });

  await page.goto("/");
  await page.locator("#tab-document").click();
  await page.locator("#document-list .document-row").first().click();

  const iframe = page.locator("#document-frame-container iframe");
  await expect(iframe).toHaveAttribute("src", EXPIRED_PDF_URL);
  // The ref — not the object — is re-requested, and the fresh URL is embedded.
  await expect(iframe).toHaveAttribute("src", FRESH_PDF_URL, { timeout: 10000 });
  await expect(page.locator("#document-toolbar a").first()).toHaveAttribute("href", FRESH_PDF_URL);
  // Nothing was ever fetched through IONe.
  expect(proxyRequests).toEqual([]);
});

test("a ref the peer no longer lists is reported gone, not served from a cache", async ({ page }) => {
  await stubDocumentRefs(page, [
    { peerDocuments: [peerDocument(EXPIRED_PDF_URL)], peerErrors: [] },
    { peerDocuments: [], peerErrors: [] }
  ]);

  await page.goto("/");
  await page.locator("#tab-document").click();
  await page.locator("#document-list .document-row").first().click();

  await expect(page.locator("#document-notice")).toContainText("no longer offers this document", {
    timeout: 10000
  });
  await expect(page.locator("#document-frame-container iframe")).toHaveCount(0);
  await expect(page.locator("#document-link-card")).toBeHidden();
});

test("a ref older than the peer's validity window is re-requested before it is followed", async ({ page }) => {
  await stubDocumentRefs(page, [
    { peerDocuments: [peerDocument(EXPIRED_CSV_URL, "text/csv")], peerErrors: [] },
    { peerDocuments: [peerDocument(FRESH_CSV_URL, "text/csv")], peerErrors: [] }
  ]);
  await page.clock.install();

  await page.goto("/");
  await page.locator("#tab-document").click();
  await page.locator("#document-list .document-row").first().click();

  const link = page.locator("#document-link-card a.document-primary-link");
  await expect(link).toHaveAttribute("href", EXPIRED_CSV_URL);

  // Past the 4-minute presumed-expiry threshold: following the link re-requests
  // the ref first instead of navigating to a URL whose signature may be dead.
  await page.clock.fastForward("05:00");
  await link.click();

  await expect(page.locator("#document-notice")).toContainText("re-requested from the peer", {
    timeout: 10000
  });
  await expect(link).toHaveAttribute("href", FRESH_CSV_URL);
});
