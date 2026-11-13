import { defineConfig } from "@playwright/test";
export default defineConfig({
  testDir: "./tests",
  testMatch: "*.spec.ts",
  fullyParallel: true,
  workers: 2,
  use: {
    baseURL: "http://127.0.0.1:5175",
    browserName: "chromium",
    locale: "zh-CN",
    trace: "off",
    screenshot: "only-on-failure",
  },
  webServer: {
    command: "bun --bun vite --host 127.0.0.1 --port 5175",
    url: "http://127.0.0.1:5175",
    reuseExistingServer: false,
  },
});
