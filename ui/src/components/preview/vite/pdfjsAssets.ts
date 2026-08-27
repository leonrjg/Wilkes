import { createRequire } from "node:module";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, extname, join } from "node:path";
import type { Plugin } from "vite";

// pdf.js does not bundle its decoders, fonts or character maps: it fetches them
// at runtime from the URLs given to getDocument(). pdf.js 6 moved JBIG2, JPX
// and colour management into WebAssembly, so a document whose pages are scans
// now renders blank without them — the invisible OCR text layer still selects,
// which is what makes the failure so quiet.
//
// The files must keep their published names, because pdf.js appends a name to
// the directory URL it was given. That rules out Vite's normal asset pipeline,
// which hashes what it emits, so this serves them in dev and copies them
// verbatim at build. `pdfjsAssetUrls()` is the other half of the contract.

const ASSET_DIRECTORIES = ["wasm", "standard_fonts", "cmaps", "iccs"];

/** URL path the assets are published under, and that pdfjsAssetUrls() reads. */
export const PDFJS_ASSET_PREFIX = "pdfjs";

const CONTENT_TYPES: Record<string, string> = {
  ".wasm": "application/wasm",
  ".js": "text/javascript",
  ".ttf": "font/ttf",
  ".pfb": "application/x-font-type1",
  ".bcmap": "application/octet-stream",
  ".icc": "application/vnd.iccprofile",
};

interface PdfjsAsset {
  /** Served and emitted path, e.g. `pdfjs/wasm/jbig2.wasm`. */
  publicPath: string;
  sourcePath: string;
}

function collectAssets(): PdfjsAsset[] {
  const require = createRequire(import.meta.url);
  const pdfjsRoot = dirname(require.resolve("pdfjs-dist/package.json"));
  return ASSET_DIRECTORIES.flatMap((directory) => {
    const absolute = join(pdfjsRoot, directory);
    return readdirSync(absolute)
      .filter((name) => extname(name) in CONTENT_TYPES)
      .filter((name) => statSync(join(absolute, name)).isFile())
      .map((name) => ({
        publicPath: `${PDFJS_ASSET_PREFIX}/${directory}/${name}`,
        sourcePath: join(absolute, name),
      }));
  });
}

/** Publishes pdf.js' runtime assets under `/pdfjs/`, names intact. */
export function pdfjsAssets(): Plugin {
  const assets = collectAssets();
  const byUrl = new Map(assets.map((a) => [`/${a.publicPath}`, a.sourcePath]));

  return {
    name: "pdfjs-assets",
    configureServer(server) {
      server.middlewares.use((request, response, next) => {
        const path = request.url?.split("?")[0];
        const sourcePath = path ? byUrl.get(path) : undefined;
        if (!sourcePath) return next();
        response.setHeader("Content-Type", CONTENT_TYPES[extname(sourcePath)]);
        response.end(readFileSync(sourcePath));
      });
    },
    generateBundle() {
      for (const asset of assets) {
        // `fileName`, not `name`: the published name is part of the contract.
        this.emitFile({
          type: "asset",
          fileName: asset.publicPath,
          source: readFileSync(asset.sourcePath),
        });
      }
    },
  };
}
