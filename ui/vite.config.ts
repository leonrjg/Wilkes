/// <reference types="vitest" />
import { createRequire } from "node:module";
import { dirname } from "node:path";
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { pdfjsAssets } from "@leonrjg/wilkes-reader/vite";

// Vite resolves a linked package to its real path, which is outside this root,
// and the dev server refuses to serve pdf.js' worker from there. Derived rather
// than hard-coded so it holds wherever the package is linked from, and inert
// once it is installed from a tag and lives under node_modules.
const readerRoot = dirname(
  createRequire(import.meta.url).resolve("@leonrjg/wilkes-reader/package.json"),
);

export default defineConfig({
  plugins: [react(), tailwindcss(), pdfjsAssets()],
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
    fs: { allow: [".", readerRoot] },
    watch: {
      ignored: ["**/src-tauri/**"],
    },
  },
  envPrefix: ["VITE_", "TAURI_ENV_*"],
  build: {
    target: process.env.TAURI_ENV_PLATFORM === "windows" ? "chrome105" : "safari13",
    minify: !process.env.TAURI_ENV_DEBUG ? "esbuild" : false,
    sourcemap: !!process.env.TAURI_ENV_DEBUG,
  },
  test: {
    globals: true,
    environment: "jsdom",
    setupFiles: ["./src/test/setup.ts"],
    css: true,
    coverage: {
      provider: "v8",
      reporter: ["text", "json-summary", "html", "cobertura"],
    },
  },
});
