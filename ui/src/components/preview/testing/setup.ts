/**
 * Test-environment gaps the readers need filled, wherever they are tested.
 *
 * Kept beside the readers rather than in an application's test setup: any host
 * that runs these components under jsdom hits the same gaps, and discovering
 * them one confusing import-time crash at a time is a poor welcome.
 */

// pdf.js constructs `new DOMMatrix()` at module-evaluation time (its canvas
// module's SCALE_MATRIX), and jsdom does not implement DOMMatrix. Any test that
// imports the readers therefore loads pdf.js and dies on import, before a
// single assertion runs. No test renders a PDF canvas for real, so an
// identity-matrix stand-in is enough to let the module evaluate; a test that
// genuinely needs matrix maths should mock pdf.js rather than lean on this.
if (!("DOMMatrix" in globalThis)) {
  class DOMMatrixStub {
    a = 1; b = 0; c = 0; d = 1; e = 0; f = 0;
    constructor(_init?: unknown) {}
  }
  Object.defineProperty(globalThis, "DOMMatrix", {
    configurable: true,
    writable: true,
    value: DOMMatrixStub,
  });
}
