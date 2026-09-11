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

test("source onboarding clears credentials and selects the source for a business question", async ({ page }) => {
  await page.route("**/relay/source-admin", (route) => route.fulfill({ json: { postgres: true } }));
  let added = false;
  await page.route("**/relay/sources", async (route) => {
    if (route.request().method() === "POST") {
      const body = route.request().postDataJSON();
      expect(body.tables).toEqual(["orders", "customers"]);
      expect(body.password).toBe("browser-secret-canary");
      expect(body).not.toHaveProperty("principalId");
      expect(body).not.toHaveProperty("workspaceId");
      added = true;
      await new Promise((resolve) => setTimeout(resolve, 100));
      return route.fulfill({ json: { id: mapping } });
    }
    return route.fulfill({ json: { sources: added ? [{ mappingId: mapping, displayName: "Sales", alias: "sales", entities: [] }] : [] } });
  });
  let ask: any;
  await page.route("**/relay/ask", (route) => {
    ask = route.request().postDataJSON();
    return route.fulfill({ json: { run: { id: run }, outcome: { kind: "succeeded" } } });
  });
  await page.goto("/");
  await page.locator("#tab-data").click();
  await page.locator("#data-source-admin summary").click();
  const form = page.locator("#data-source-form");
  for (const [name, value] of Object.entries({ name: "Sales", alias: "sales", host: "localhost", database: "sales", tables: "orders, customers", username: "reader", password: "browser-secret-canary" })) {
    await form.locator(`[name=${name}]`).fill(value);
  }
  await form.locator("button[type=submit]").click();
  await expect(form.locator("[name=password]")).toHaveValue("");
  await expect(page.locator("#data-source-status")).toContainText("Source ready");
  await expect(page.locator("#data-source-list input")).toBeChecked();
  expect(await page.evaluate(() => JSON.stringify({ local: localStorage, session: sessionStorage }))).not.toContain("browser-secret-canary");
  await page.locator("#data-ask-input").fill("Total sales by customer for the last quarter");
  await page.locator("#data-ask-submit").click();
  await expect.poll(() => ask?.mappingIds).toEqual([mapping]);
  expect(JSON.stringify(ask)).not.toContain("browser-secret-canary");
});

test("source administration stays hidden without its capability", async ({ page }) => {
  await page.route("**/relay/source-admin", (route) => route.fulfill({ status: 403, json: {} }));
  await page.goto("/");
  await page.locator("#tab-data").click();
  await expect(page.locator("#data-source-admin")).toBeHidden();
});


test("switching workspace clears the source registration draft", async ({ page }) => {
  await page.route("**/relay/source-admin", (route) => route.fulfill({ json: { postgres: true } }));
  await page.goto("/");
  await page.locator("#tab-data").click();
  await page.locator("#data-source-admin summary").click();
  const form = page.locator("#data-source-form");
  await form.locator("[name=host]").fill("finance.internal");
  await form.locator("[name=password]").fill("workspace-secret-canary");
  await page.evaluate(() => (window as any).setActiveWorkspace({ id: "dddddddd-dddd-dddd-dddd-dddddddddddd", name: "Other", lifecycle: "continuous", closedAt: null }));
  await expect(form.locator("[name=password]")).toHaveValue("");
  await expect(form.locator("[name=host]")).toHaveValue("");
});

test("saved datasets show full counts beside a truncated preview and survive reload", async ({ page }) => {
  const destination = "eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee";
  const dataset = "ffffffff-ffff-ffff-ffff-ffffffffffff";
  const version = "11111111-1111-1111-1111-111111111111";
  const saved = { dataset_id: dataset, version_id: version, dataset_name: "Sales report", row_count: 4000, column_count: 3, expires_at: "2026-10-01T00:00:00Z" };
  await page.route("**/relay/destinations", (route) => route.fulfill({ json: { canManage: true, canPublish: true, destinations: [{ id: destination, name: "Reports", can_publish: true }] } }));
  await page.route("**/relay/datasets", (route) => route.fulfill({ json: { datasets: [saved] } }));
  await page.route("**/relay/runs/*/dataset", (route) => route.fulfill({ json: { datasets: [saved] } }));
  await page.route("**/relay/runs/*/result", (route) => route.fulfill({ json: { schema: [{ name: "n", ty: "int64" }], rows: [[{ t: "int64", v: "1" }]], row_count: 1, truncated: true, omissions: [{ reason: "Preview row limit" }] } }));
  let submitted: any;
  await page.route("**/relay/ask", (route) => { submitted = route.request().postDataJSON(); return route.fulfill({ json: { run: { id: run }, outcome: { kind: "succeeded" } } }); });
  await page.goto("/");
  await page.locator("#tab-data").click();
  await page.locator("#data-source-list input").check();
  await page.locator("#data-save-dataset").check();
  await page.locator("#data-dataset-name").fill("Sales report");
  await page.locator("#data-destination").selectOption(destination);
  await page.locator("#data-ask-input").fill("Total sales by customer");
  await page.locator("#data-ask-submit").click();
  await expect(page.locator("#data-published")).toContainText("4000 complete rows, 3 columns");
  await expect(page.locator("#data-truncated")).toBeVisible();
  expect(submitted.publication).toEqual({ destinationId: destination, datasetName: "Sales report", ttlSeconds: 3600 });
  await expect(page.locator("#data-dataset-list a")).toHaveAttribute("href", `/api/v1/workspaces/${workspace}/relay/datasets/${dataset}/versions/${version}/arrow`);
  await page.reload();
  await page.locator("#tab-data").click();
  await expect(page.locator("#data-dataset-list")).toContainText("4000 complete rows");
  await expect(page.locator("#data-save-dataset")).not.toBeChecked();
});

test("storage creation uses bounded defaults and workspace changes discard drafts and stale results", async ({ page }) => {
  const other = "dddddddd-dddd-dddd-dddd-dddddddddddd";
  let created: any;
  await page.route("**/relay/destinations", async (route) => {
    if (route.request().method() === "POST") {
      created = route.request().postDataJSON();
      await new Promise((resolve) => setTimeout(resolve, 250));
      return route.fulfill({ json: { id: "eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee" } });
    }
    return route.fulfill({ json: { canManage: true, canPublish: true, destinations: [] } });
  });
  await page.route("**/relay/datasets", (route) => route.fulfill({ json: { datasets: [] } }));
  await page.goto("/");
  await page.locator("#tab-data").click();
  await page.locator("#data-destination-admin summary").click();
  await page.locator("#data-destination-form [name=name]").fill("Reports");
  await page.locator("#data-save-dataset").check();
  await page.locator("#data-dataset-name").fill("Private draft");
  await page.locator("#data-destination-form button").click();
  await expect.poll(() => created?.name).toBe("Reports");
  expect(created.policy).toEqual({ max_rows: 5000, max_columns: 64, max_cells: 320000, max_bytes: 2097152, max_ttl_seconds: 3600, max_classification: "internal" });
  expect(created).not.toHaveProperty("actorId");
  await page.evaluate((id) => (window as any).setActiveWorkspace({ id, name: "Other", lifecycle: "continuous", closedAt: null }), other);
  await expect(page.locator("#data-dataset-name")).toHaveValue("");
  await expect(page.locator("#data-destination-form [name=name]")).toHaveValue("");
  await expect(page.locator("#data-save-dataset")).not.toBeChecked();
  await page.waitForTimeout(350);
  await expect(page.locator("#data-destination-status")).toHaveText("");
  await expect(page.locator("#data-destination")).toHaveValue("");
});

test("private recipes save, reload, replay and append with workspace resets", async ({ page }) => {
  const dataset = "ffffffff-ffff-ffff-ffff-ffffffffffff";
  const version = "11111111-1111-1111-1111-111111111111";
  const recipe = "22222222-2222-2222-2222-222222222222";
  const recipeVersion = "33333333-3333-3333-3333-333333333333";
  const entries: any[] = [];
  const saves: any[] = [];
  const asks: any[] = [];
  await page.route("**/relay/datasets", (route) => route.fulfill({ json: { datasets: [{ dataset_id: dataset, version_id: version, dataset_name: "Sales", row_count: 4000, column_count: 1, expires_at: "2026-10-01T00:00:00Z" }] } }));
  await page.route("**/relay/recipes", (route) => {
    if (route.request().method() === "POST") {
      expect(route.request().headers()["content-type"]).toBe("application/json");
      saves.push(route.request().postDataJSON());
      const entry = { recipe_id: recipe, version_id: entries.length ? "44444444-4444-4444-4444-444444444444" : recipeVersion, version: entries.length + 1, name: "Sales recipe", ask: "Count sales\n", sources: [{ alias: "pg", connection_id: "connection" }], mappingIds: [mapping], output_schema: [{ name: "count", type: "Int64", nullable: false }], definition_hash: "sha256:fixture", clarifications: [{ question: "Which region?", answer: "<b>West</b>" }] };
      entries.push(entry);
      return route.fulfill({ json: entry });
    }
    return route.fulfill({ json: { recipes: entries } });
  });
  await page.route("**/relay/recipes/*/versions/*", (route) => route.fulfill({ json: entries.find((entry) => route.request().url().endsWith(entry.version_id)) }));
  await page.route("**/relay/ask", (route) => { asks.push(route.request().postDataJSON()); return route.fulfill({ json: { run: { id: run }, outcome: { kind: "succeeded" } } }); });
  await page.goto("/");
  await page.locator("#tab-data").click();
  await page.locator('[data-action="save-recipe"]').click();
  await page.locator("#data-recipe-name").fill("Sales recipe");
  await page.locator("#data-recipe-save-submit").click();
  await expect(page.locator("#data-recipe-status")).toContainText("version 1");
  expect(saves[0]).toEqual({ name: "Sales recipe", datasetId: dataset, versionId: version });
  await page.reload();
  await page.locator("#tab-data").click();
  await page.locator("#data-recipe-select").selectOption(recipeVersion);
  await expect(page.locator("#data-ask-input")).toHaveValue("Count sales\n");
  await expect(page.locator("#data-ask-input")).toHaveAttribute("readonly", "");
  await expect(page.locator("#data-source-list input")).toBeDisabled();
  await expect(page.locator("#data-recipe-details")).toContainText("<b>West</b>");
  await expect(page.locator("#data-recipe-details b")).toHaveCount(0);
  await expect(page.locator("#data-recipe-details")).toContainText("count: Int64 (required)");
  await expect(page.locator("#data-ask-hint")).toContainText("current permissions");
  await page.locator("#data-ask-submit").click();
  await expect.poll(() => asks.length).toBe(1);
  expect(asks[0]).toMatchObject({ ask: "Count sales\n", recipeId: recipe, recipeVersionId: recipeVersion, mappingIds: [mapping] });
  await page.locator('[data-action="save-recipe"]').click();
  await page.locator("#data-recipe-append").selectOption(recipe);
  await page.locator("#data-recipe-save-submit").click();
  await expect(page.locator("#data-recipe-status")).toContainText("version 2");
  expect(saves[1].recipeId).toBe(recipe);
  await page.locator("#data-recipe-new-ask").click();
  await expect(page.locator("#data-ask-input")).not.toHaveAttribute("readonly", "");
  await page.locator("#data-ask-input").fill("Different question");
  await page.locator("#data-ask-submit").click();
  await expect.poll(() => asks.length).toBe(2);
  expect(asks[1]).not.toHaveProperty("recipeVersionId");
  expect(asks[1].requestId).not.toBe(asks[0].requestId);
  await page.locator('[data-action="save-recipe"]').click();
  await page.evaluate(() => (window as any).setActiveWorkspace({ id: "dddddddd-dddd-dddd-dddd-dddddddddddd", name: "Other", lifecycle: "continuous", closedAt: null }));
  await expect(page.locator("#data-recipe-save-form")).toBeHidden();
  await expect(page.locator("#data-recipe-name")).toHaveValue("");
  await expect(page.locator("#data-recipe-select")).toHaveValue("");
  await expect(page.locator("#data-recipe-details")).toHaveText("");
});

test("file onboarding submits typed formats with JSON headers and clears both keys", async ({ page }) => {
  await page.route("**/relay/source-admin", (route) => route.fulfill({ json: { postgres: true, fileFormats: ["json", "ndjson", "ipc_file", "ipc_stream", "csv", "parquet"] } }));
  const requests: any[] = [];
  await page.route("**/relay/file-sources", async (route) => {
    expect(route.request().headers()["content-type"]).toBe("application/json");
    requests.push(route.request().postDataJSON());
    await new Promise((resolve) => setTimeout(resolve, 100));
    return route.fulfill({ json: { id: mapping } });
  });
  await page.goto("/");
  await page.locator("#tab-data").click();
  await page.locator("#data-file-admin summary").click();
  const form = page.locator("#data-file-form");
  const columns = [{ name: "amount", ty: { type: "decimal", precision: 25, scale: 4 }, nullable: true }];
  const receipt = { attested_by: "fixture operator", attested_at: "2026-09-10T00:00:00Z", expires_at: "2026-10-01T00:00:00Z", actions: ["s3:GetObject", "s3:ListBucket"], bucket: "reports", prefix: "approved", signature: "fixture operator receipt" };
  for (const format of ["json", "ndjson", "ipc_file", "ipc_stream", "csv", "parquet"]) {
    await form.locator("[name=name]").fill("File source");
    await form.locator("[name=alias]").fill(`files_${format}`);
    await form.locator("[name=endpoint]").fill("http://127.0.0.1:59000");
    await form.locator("[name=bucket]").fill("reports");
    await form.locator("[name=prefix]").fill("approved");
    await form.locator("[name=path]").fill(`sales.${format}`);
    await form.locator("[name=format]").selectOption(format);
    if (format !== "parquet") await form.locator("[name=columns]").fill(JSON.stringify(columns));
    else await expect(page.locator("#data-file-columns")).toBeHidden();
    await form.locator("[name=policyReceipt]").fill(JSON.stringify(receipt));
    await form.locator("[name=accessKeyId]").fill("file-access-canary");
    await form.locator("[name=secretAccessKey]").fill("file-secret-canary");
    await form.locator("button[type=submit]").click();
    await expect(form.locator("[name=accessKeyId]")).toHaveValue("");
    await expect(form.locator("[name=secretAccessKey]")).toHaveValue("");
    await expect(page.locator("#data-file-status")).toContainText("operator-attested");
    const sent = requests[requests.length - 1];
    expect(sent.format).toBe(format);
    expect(sent.classification).toBe("restricted");
    expect(sent.columns).toEqual(format === "parquet" ? [] : columns);
    expect(sent.policyReceipt).toEqual(receipt);
    expect(sent.accessKeyId).toBe("file-access-canary");
    expect(sent.secretAccessKey).toBe("file-secret-canary");
    if (format === "csv") expect(sent.csv).toEqual({ delimiter: ",", quote: '"', escape: null, header: true, nullValue: "NULL" });
    else expect(sent).not.toHaveProperty("csv");
    expect(sent).not.toHaveProperty("public_config");
    expect(sent).not.toHaveProperty("workspaceId");
  }
  await expect(page.locator("#data-source-list input")).toBeChecked();
  await expect(page.locator("body")).not.toContainText("file-secret-canary");
});

test("file onboarding discards workspace drafts and late responses", async ({ page }) => {
  await page.route("**/relay/source-admin", (route) => route.fulfill({ json: { fileFormats: ["json", "parquet"] } }));
  let submitted = false;
  await page.route("**/relay/file-sources", async (route) => {
    expect(route.request().headers()["content-type"]).toBe("application/json");
    submitted = true;
    await new Promise((resolve) => setTimeout(resolve, 300));
    return route.fulfill({ json: { id: mapping } });
  });
  await page.goto("/");
  await page.locator("#tab-data").click();
  await page.locator("#data-file-admin summary").click();
  const form = page.locator("#data-file-form");
  for (const [name, value] of Object.entries({ name: "Draft", alias: "draft", endpoint: "http://127.0.0.1:59000", bucket: "reports", prefix: "approved", path: "data.parquet", policyReceipt: "{}", accessKeyId: "access-canary", secretAccessKey: "secret-canary" })) await form.locator(`[name=${name}]`).fill(value);
  await form.locator("[name=format]").selectOption("parquet");
  await form.locator("button[type=submit]").click();
  await expect.poll(() => submitted).toBe(true);
  await page.evaluate(() => (window as any).setActiveWorkspace({ id: "dddddddd-dddd-dddd-dddd-dddddddddddd", name: "Other", lifecycle: "continuous", closedAt: null }));
  await expect(form.locator("[name=accessKeyId]")).toHaveValue("");
  await expect(form.locator("[name=secretAccessKey]")).toHaveValue("");
  await expect(form.locator("[name=policyReceipt]")).toHaveValue("");
  await expect(form.locator("[name=endpoint]")).toHaveValue("");
  await page.waitForTimeout(350);
  await expect(page.locator("#data-file-status")).toHaveText("");
  await expect(page.locator("#data-source-list input")).not.toBeChecked();
});
