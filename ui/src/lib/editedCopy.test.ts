import { describe, expect, it } from "vitest";
import { editedCopyPath, fileNameOf } from "./editedCopy";

describe("editedCopyPath", () => {
  it("names the copy beside the original", () => {
    expect(editedCopyPath("/docs/papers/attention.pdf")).toBe("/docs/papers/attention (edited).pdf");
  });

  it("keeps a Windows directory", () => {
    expect(editedCopyPath("C:\\docs\\a.b.pdf")).toBe("C:\\docs\\a.b (edited).pdf");
  });

  it("is never the original's own path", () => {
    for (const path of ["/a/b.pdf", "b.pdf", "/a/noextension"]) {
      expect(editedCopyPath(path)).not.toBe(path);
    }
  });

  it("gives the file name on its own", () => {
    expect(fileNameOf("/docs/papers/attention (edited).pdf")).toBe("attention (edited).pdf");
  });
});
