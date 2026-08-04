import { describe, expect, it } from "vitest";
import {
  configuredLibraryRoots,
  pathIsWithinRoot,
  pathsEqual,
} from "./configuredRoots";

describe("configuredLibraryRoots", () => {
  it("deduplicates roots while retaining nested roots", () => {
    expect(configuredLibraryRoots({
      directory: "/library",
      favorites: ["/library/nested", "/other"],
      recentDirs: ["/library", "/library/nested"],
    })).toEqual(["/library/nested", "/other", "/library"]);
  });

  it("ignores empty root values", () => {
    expect(configuredLibraryRoots({
      directory: "",
      favorites: [""],
      recentDirs: ["/library"],
    })).toEqual(["/library"]);
  });
});

describe("path root membership", () => {
  it("matches nested paths at separator boundaries", () => {
    expect(pathIsWithinRoot("/library/nested/file.pdf", "/library/nested")).toBe(true);
    expect(pathIsWithinRoot("/library/nested-old/file.pdf", "/library/nested")).toBe(false);
  });

  it("handles alternate and trailing separators", () => {
    expect(pathIsWithinRoot("C:\\library\\nested\\file.pdf", "C:\\library\\nested\\")).toBe(true);
    expect(pathsEqual("C:\\library\\nested\\", "C:/library/nested")).toBe(true);
  });
});
