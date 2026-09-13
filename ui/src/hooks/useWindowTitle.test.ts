import { renderHook } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { composeWindowTitle, useWindowTitle } from "./useWindowTitle";

describe("composeWindowTitle", () => {
  it("leads with the document and trails with the application", () => {
    expect(
      composeWindowTitle({
        workspace: "Thesis",
        root: "/Users/me/Papers/",
        document: "/Users/me/Papers/ocr/survey.pdf",
      }),
    ).toBe("survey.pdf — Papers — Thesis — Wilkes");
  });

  it("omits the parts that are not known", () => {
    expect(composeWindowTitle({ workspace: "Thesis", root: "", document: null })).toBe(
      "Thesis — Wilkes",
    );
    expect(composeWindowTitle({ document: "C:\\docs\\notes.epub" })).toBe("notes.epub — Wilkes");
    expect(composeWindowTitle({})).toBe("Wilkes");
  });
});

describe("useWindowTitle", () => {
  it("follows the parts it is given", () => {
    const { rerender } = renderHook((parts) => useWindowTitle(parts), {
      initialProps: { workspace: "Thesis", root: "/r/Papers", document: null as string | null },
    });
    expect(document.title).toBe("Papers — Thesis — Wilkes");

    rerender({ workspace: "Thesis", root: "/r/Papers", document: "/r/Papers/a.pdf" });
    expect(document.title).toBe("a.pdf — Papers — Thesis — Wilkes");
  });
});
