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
const require_ = createRequire(import.meta.url);
const readerRoot = dirname(require_.resolve("@leonrjg/wilkes-reader/package.json"));
// The chat is a sibling checkout, linked rather than installed, and the same
// rule applies to reading its stylesheet off disk.
const chatRoot = dirname(require_.resolve("@leonrjg/wilkes-chat/package.json"));

export default defineConfig({
  plugins: [react(), tailwindcss(), pdfjsAssets()],
  // A linked package brings its own node_modules, and a second copy of React
  // means a second hook dispatcher -- which fails as `useCallback` of null the
  // first time a component from it renders, nowhere near the cause.
  resolve: { dedupe: ["react", "react-dom", "zustand"] },
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
    fs: { allow: [".", readerRoot, chatRoot] },
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
