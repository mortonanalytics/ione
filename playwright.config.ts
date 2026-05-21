import { defineConfig, devices } from "@playwright/test";

// E2E tests assume an IONe server is already running at BASE_URL in local auth
// mode (default user, no login). Bring one up with:
//   docker compose up -d postgres
//   DATABASE_URL=postgres://ione:ione@localhost:5433/ione \
//     IONE_TOKEN_KEY=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA= \
//     IONE_BIND=127.0.0.1:3007 cargo run
// Tests stub the /map-layers API at the network layer, so no peer or DB seeding
// is required — only the static shell + app.js served by IONe.
export default defineConfig({
  testDir: "./tests/e2e",
  outputDir: "./test-results",
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 1 : 0,
  workers: 1,
  reporter: "list",
  use: {
    baseURL: process.env.BASE_URL ?? "http://127.0.0.1:3007",
    screenshot: "only-on-failure",
  },
  projects: [
    {
      name: "desktop",
      use: { ...devices["Desktop Chrome"] },
    },
  ],
});
