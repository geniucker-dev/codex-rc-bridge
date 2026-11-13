import { defineConfig } from "@playwright/test";

// All HTTP targets belong to this test's loopback-only Compose project.
// Bun's HTTP implementation can inherit shell proxy settings even for localhost.
for (const name of [
  "http_proxy",
  "https_proxy",
  "all_proxy",
  "HTTP_PROXY",
  "HTTPS_PROXY",
  "ALL_PROXY",
])
  delete process.env[name];
process.env.NO_PROXY = "localhost,127.0.0.1";
process.env.no_proxy = process.env.NO_PROXY;

export default defineConfig({
  testDir: "./container-tests",
  testMatch: "*.spec.ts",
  workers: 1,
  timeout: 180_000,
  use: {
    browserName: "chromium",
    // The UI follows the browser language; the assertions use the Chinese labels.
    locale: "zh-CN",
    // A stuck page step fails early instead of using up the whole test budget.
    actionTimeout: 30_000,
    navigationTimeout: 30_000,
    trace: "off",
    screenshot: "off",
    video: "off",
  },
});
