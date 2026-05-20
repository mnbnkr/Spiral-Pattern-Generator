import { defineConfig, devices } from "@playwright/test";

const appOrigin = "http://127.0.0.1:8081";
const appBasePath = process.env.APP_BASE_PATH ?? "/Spiral-Pattern-Generator/";
const playwrightOutputDir = "target/playwright/test-results";
const playwrightReportDir = "target/playwright/report";

export default defineConfig({
  testDir: "./tests",
  outputDir: playwrightOutputDir,
  fullyParallel: false,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 1 : 0,
  workers: 1,
  reporter: process.env.CI
    ? [["github"], ["html", { outputFolder: playwrightReportDir, open: "never" }]]
    : "list",
  use: {
    baseURL: `${appOrigin}${appBasePath}`,
    trace: "on-first-retry",
  },
  projects: [
    {
      name: "chromium",
      use: { ...devices["Desktop Chrome"] },
    },
  ],
  webServer: {
    command: `trunk serve --port 8081 --no-autoreload --public-url ${appBasePath}`,
    url: `${appOrigin}${appBasePath}`,
    reuseExistingServer: !process.env.CI,
    timeout: 120_000,
    stdout: "pipe",
    stderr: "pipe",
  },
});
