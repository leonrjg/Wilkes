import { describe, expect, it } from "vitest";
import { renderedBoundaries, sourceBoundaryForDomPoint, sourceMappedMarkdown } from "./markdownSourceMap";

interface TreeNode {
  type: string;
  tagName?: string;
  value?: string;
  position?: { start: { offset: number }; end: { offset: number } };
  properties?: Record<string, unknown>;
  children?: TreeNode[];
}

/** Build the paragraph hast a soft break produces: one positioned text node holding the `\n`. */
function paragraph(content: string): TreeNode {
  return {
    type: "root",
    children: [
      {
        type: "element",
        tagName: "p",
        children: [
          { type: "text", value: content, position: { start: { offset: 0 }, end: { offset: content.length } } },
        ],
      },
    ],
  };
}

function run(content: string, tree: TreeNode): void {
  sourceMappedMarkdown(content, [])()(tree as never);
}

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

  it("renders a single newline in flowing text as <br> between addressable spans", () => {
    const content = "ab\ncd";
    const tree = paragraph(content);
    run(content, tree);

    const children = tree.children![0].children!;
    expect(children.map((child) => child.tagName ?? child.type)).toEqual(["span", "br", "span"]);
    expect(children[0].children![0].value).toBe("ab");
    expect(children[2].children![0].value).toBe("cd");
    // The span after the break stays addressable, mapping back to source byte 3 ("c").
    expect(children[2].properties!.dataSourceBoundaries).toBe("3,4,5");
  });

  it("keeps newlines literal inside <pre>/<code> instead of inserting <br>", () => {
    const content = "x\ny";
    const tree: TreeNode = {
      type: "root",
      children: [
        {
          type: "element",
          tagName: "pre",
          children: [
            {
              type: "element",
              tagName: "code",
              children: [
                { type: "text", value: content, position: { start: { offset: 0 }, end: { offset: content.length } } },
              ],
            },
          ],
        },
      ],
    };
    run(content, tree);

    const codeChildren = tree.children![0].children![0].children!;
    expect(codeChildren.map((child) => child.tagName ?? child.type)).toEqual(["span"]);
    expect(codeChildren[0].children![0].value).toBe("x\ny");
  });
});
