import { defineConfig, devices } from "@playwright/test";

// Live end-to-end suite: drives the real console (Caddy image or Vite dev server) against a
// real Aurix Net node. Nothing is mocked; every spec goes through the same-origin API proxy.
//
//   DASHBOARD_URL                console origin under test (default: Vite dev server started here)
//   NET_API_URL                  node the dev server proxies to (default http://127.0.0.1:8090)
//   AURIX_NET_E2E_TOKEN          operator token of that node (`auth.admin_token`)
const dashboardUrl = process.env.DASHBOARD_URL;
const baseURL = dashboardUrl ?? "http://127.0.0.1:5174";

export default defineConfig({
  testDir: "./e2e",
  outputDir: "./test-results",
  fullyParallel: false,
  workers: 1,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 1 : 0,
  timeout: 45_000,
  expect: { timeout: 10_000 },
  reporter: process.env.CI ? [["list"], ["html", { open: "never" }]] : [["list"]],
  use: {
    baseURL,
    locale: "en-US",
    timezoneId: "UTC",
    viewport: { width: 1440, height: 900 },
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
  },
  projects: [
    { name: "setup", testMatch: /auth\.setup\.ts/ },
    {
      name: "chromium",
      use: { ...devices["Desktop Chrome"], storageState: "e2e/.auth/operator.json" },
      dependencies: ["setup"],
    },
  ],
  webServer: dashboardUrl
    ? undefined
    : {
        command: "npm run dev -- --host 127.0.0.1 --port 5174 --strictPort",
        url: "http://127.0.0.1:5174/login",
        reuseExistingServer: !process.env.CI,
        timeout: 60_000,
      },
});
