import { defineConfig, loadEnv } from "vite";
export default defineConfig(({ mode }) => ({
  server: {
    port: 5173,
    strictPort: true,
    proxy: {
      "/api": {
        target:
          loadEnv(mode, ".", "BRIDGE_DEV_").BRIDGE_DEV_BACKEND ??
          "http://127.0.0.1:8787",
        // Management validates the browser's fixed Host/Origin, including its port.
        changeOrigin: false,
      },
    },
  },
}));
