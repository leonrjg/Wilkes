import { describe, it, expect, afterEach } from "vitest";
import { findMatchRanges } from "./useMarkdownFind";

function mount(html: string): HTMLElement {
  const root = document.createElement("div");
  root.innerHTML = html;
  document.body.appendChild(root);
  return root;
}

afterEach(() => {
  document.body.innerHTML = "";
});

describe("findMatchRanges", () => {
  it("finds every case-insensitive occurrence", () => {
    const root = mount("<p>Alpha beta ALPHA gamma alpha</p>");
    const ranges = findMatchRanges(root, "alpha");
    expect(ranges.length).toBe(3);
    expect(ranges[0].toString().toLowerCase()).toBe("alpha");
  });

  it("matches text split across sibling spans (source-map runs)", () => {
    // The source map wraps text in per-run spans, so a single word is often
    // several adjacent text nodes; a match must span them.
    const root = mount("<p><span>hel</span><span>lo</span> world</p>");
    const ranges = findMatchRanges(root, "hello");
    expect(ranges.length).toBe(1);
    expect(ranges[0].toString()).toBe("hello");
    expect(ranges[0].startContainer).not.toBe(ranges[0].endContainer);
  });

  it("does not match across block boundaries", () => {
    const root = mount("<p>foo</p><p>bar</p>");
    // Without the block separator this would read as the contiguous "foobar".
    expect(findMatchRanges(root, "foobar").length).toBe(0);
    expect(findMatchRanges(root, "foo").length).toBe(1);
  });

  it("returns no ranges when nothing matches", () => {
    const root = mount("<p>nothing here</p>");
    expect(findMatchRanges(root, "absent")).toEqual([]);
  });
});
