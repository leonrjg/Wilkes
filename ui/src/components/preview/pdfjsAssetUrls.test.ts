import { describe, expect, it } from "vitest";
import { pdfjsAssetUrls } from "./pdfjsAssetUrls";

describe("pdfjsAssetUrls", () => {
  it("names every directory pdf.js fetches from at runtime", () => {
    expect(pdfjsAssetUrls()).toEqual({
      wasmUrl: expect.stringMatching(/\/pdfjs\/wasm\/$/),
      standardFontDataUrl: expect.stringMatching(/\/pdfjs\/standard_fonts\/$/),
      cMapUrl: expect.stringMatching(/\/pdfjs\/cmaps\/$/),
      cMapPacked: true,
      iccUrl: expect.stringMatching(/\/pdfjs\/iccs\/$/),
    });
  });

  it("keeps the trailing slash pdf.js appends a file name to", () => {
    // pdf.js builds `${wasmUrl}jbig2.wasm` by concatenation, so a missing
    // slash silently produces a sibling path that 404s.
    for (const value of Object.values(pdfjsAssetUrls())) {
      if (typeof value === "string") expect(value.endsWith("/")).toBe(true);
    }
  });

  it("resolves against the document base, so a sub-path deployment still finds them", () => {
    const base = document.createElement("base");
    base.href = "https://example.test/app/";
    document.head.append(base);
    try {
      expect(pdfjsAssetUrls().wasmUrl).toBe("https://example.test/app/pdfjs/wasm/");
    } finally {
      base.remove();
    }
  });
});
