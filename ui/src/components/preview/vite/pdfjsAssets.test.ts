import { describe, expect, it, vi } from "vitest";
import { pdfjsAssets } from "./pdfjsAssets";
import { pdfjsAssetUrls } from "../pdfjsAssetUrls";

/** The emitted assets, keyed by the path they are published under. */
function emittedFiles() {
  const emitFile = vi.fn();
  const plugin = pdfjsAssets();
  const generate = plugin.generateBundle as (this: unknown) => void;
  generate.call({ emitFile });
  return emitFile.mock.calls.map(([file]) => file);
}

describe("pdfjsAssets", () => {
  it("publishes the decoders pdf.js 6 no longer bundles", () => {
    const names = emittedFiles().map((file) => file.fileName);
    // JBIG2 left the worker in pdf.js 6; without it scanned pages are blank.
    expect(names).toContain("pdfjs/wasm/jbig2.wasm");
    expect(names).toContain("pdfjs/wasm/openjpeg.wasm");
  });

  it("publishes the standard fonts, character maps and colour profiles", () => {
    const names = emittedFiles().map((file) => file.fileName);
    expect(names).toContain("pdfjs/standard_fonts/LiberationSans-Regular.ttf");
    expect(names.some((name: string) => name.endsWith(".bcmap"))).toBe(true);
    expect(names.some((name: string) => name.startsWith("pdfjs/iccs/"))).toBe(true);
  });

  it("emits under fixed names, because pdf.js builds the URL by concatenation", () => {
    for (const file of emittedFiles()) {
      expect(file.type).toBe("asset");
      // `name` would let Vite hash it; `fileName` is verbatim.
      expect(file.name).toBeUndefined();
    }
  });

  it("publishes under the prefix the reader asks for", () => {
    const { wasmUrl } = pdfjsAssetUrls();
    const prefix = new URL(wasmUrl).pathname.replace(/^\//, "").replace(/wasm\/$/, "");
    expect(emittedFiles().every((file) => file.fileName.startsWith(prefix))).toBe(true);
  });
});
