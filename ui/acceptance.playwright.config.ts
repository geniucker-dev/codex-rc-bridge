import { defineConfig } from "@playwright/test";

// Loopback-only synthetic instances; never import the user's bridge state.
for (const name of [
  "http_proxy",
  "https_proxy",
  "all_proxy",
  "HTTP_PROXY",
  "HTTPS_PROXY",
  "ALL_PROXY",
])
  delete process.env[name];
delete process.env.BRIDGE_UPSTREAM_PROXY;
process.env.NO_PROXY = "localhost,127.0.0.1";
process.env.no_proxy = process.env.NO_PROXY;

export default defineConfig({
  testDir: "./acceptance-tests",
  workers: 1,
  timeout: 45_000,
  use: {
    browserName: "chromium",
    locale: "zh-CN",
    trace: "off",
    screenshot: "off",
    video: "off",
  },
});
