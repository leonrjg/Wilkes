// pdfjs-dist ships no `exports` map, so `pdfjs-dist/web/pdf_viewer.mjs` resolves
// as a direct file path but TypeScript can't locate its declarations (they live
// under `types/web/`). Map the value module to the maintained type for the one
// symbol we consume.
declare module "pdfjs-dist/web/pdf_viewer.mjs" {
  export { TextLayerBuilder } from "pdfjs-dist/types/web/text_layer_builder";
}
