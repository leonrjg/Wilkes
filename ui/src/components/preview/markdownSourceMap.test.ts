import { describe, expect, it } from "vitest";
import { renderedBoundaries, sourceBoundaryForDomPoint } from "./markdownSourceMap";

describe("rendered Markdown source mapping", () => {
  it("maps entities, escapes, and non-BMP characters without byte slicing", () => {
    const source = "A &amp; \\* emoji🙂";
    const rendered = "A & * emoji🙂";
    const boundaries = renderedBoundaries(source, rendered, 0, source.length);

    expect(boundaries[2]).toBe(2);
    expect(boundaries[3]).toBe(7);
    expect(boundaries[4]).toBe(8);
    expect(boundaries[5]).toBe(10);
    expect(boundaries.at(-1)).toBe(source.length);
  });

  it("maps element endpoints to the start or end of their source run", () => {
    const span = document.createElement("span");
    span.className = "markdown-source-run";
    span.dataset.sourceBoundaries = "4,5,6,7";
    span.textContent = "abc";

    expect(sourceBoundaryForDomPoint(span, 0)).toBe(4);
    expect(sourceBoundaryForDomPoint(span, 1)).toBe(7);
    expect(sourceBoundaryForDomPoint(span.firstChild!, 2)).toBe(6);
  });
});
