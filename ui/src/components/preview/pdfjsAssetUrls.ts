// The runtime half of the contract in vite/pdfjsAssets.ts: the directory URLs
// pdf.js fetches its decoders, fonts and character maps from. Resolved against
// document.baseURI rather than a bundler's base variable so the readers stay
// portable, and so an app served from a sub-path still finds them.

/** pdf.js `getDocument` parameters locating its runtime assets. */
export function pdfjsAssetUrls() {
  const directory = (name: string) =>
    new URL(`pdfjs/${name}/`, document.baseURI).href;
  return {
    wasmUrl: directory("wasm"),
    standardFontDataUrl: directory("standard_fonts"),
    cMapUrl: directory("cmaps"),
    // pdf.js ships the character maps in its binary .bcmap form.
    cMapPacked: true,
    iccUrl: directory("iccs"),
  };
}
