import { render } from "@testing-library/react";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";
import { createMemoryPositionStore, HtmlViewer, ReaderHostProvider } from "@leonrjg/wilkes-reader";

/**
 * What the viewer shows of an HTML file and what search reads of it must be
 * the same text: a match search reports has to be a passage the reader can
 * see, and a passage the reader sees has to be searchable. The Rust side
 * reads the same fixture in `extract::html` and compares it with the same
 * expected text, so a sanitizer and a masker that drift apart fail one of the
 * two tests.
 *
 * Compared without whitespace: the viewer's DOM keeps the source's spacing
 * and the reading collapses it, and neither is the other's business.
 */
describe("the HTML reading", () => {
  // Vitest runs from the UI package's own directory.
  const fixtures = resolve(process.cwd(), "../crates/core/tests/fixtures/html");
  const source = readFileSync(resolve(fixtures, "visible-text.html"), "utf8");
  const expected = readFileSync(resolve(fixtures, "visible-text.txt"), "utf8");
  const squeeze = (text: string) => text.replace(/\s+/g, "");

  it("is the text the viewer shows", () => {
    const { container } = render(
      <ReaderHostProvider
        value={{
          openExternal: () => {},
          colorScheme: "light",
          positions: createMemoryPositionStore(),
        }}
      >
        <HtmlViewer
          documentPath="/library/visible-text.html"
          content={source}
          highlightRange={{ start: 0, end: 0 }}
        />
      </ReaderHostProvider>,
    );
    const shown = container.querySelector("article")?.textContent ?? "";
    expect(squeeze(shown)).toBe(squeeze(expected));
  });
});
