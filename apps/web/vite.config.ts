import { fileURLToPath, URL } from "node:url";
import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

const coreUrl = process.env.VETRO_CORE_URL ?? "http://127.0.0.1:47600";

export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: { "~": fileURLToPath(new URL("./src", import.meta.url)) },
  },
  server: {
    host: "127.0.0.1",
    port: 5733,
    strictPort: false,
    proxy: {
      "/api": { target: coreUrl, changeOrigin: false, ws: true },
    },
  },
  preview: { host: "127.0.0.1" },
  build: {
    target: "es2022",
    chunkSizeWarningLimit: 900,
  },
});
