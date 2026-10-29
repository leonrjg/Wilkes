import { describe, expect, it } from "vitest";
import { arrangePanes, movePane } from "./paneOrder";

const panes = (...keys: string[]) => keys.map((key) => ({ key }));
const keysOf = (arranged: { key: string }[]) => arranged.map((pane) => pane.key);

describe("arrangePanes", () => {
  it("keeps the default order when nothing was arranged", () => {
    expect(keysOf(arrangePanes(panes("bookmarks", "chat:1", "browser"), []))).toEqual([
      "bookmarks",
      "chat:1",
      "browser",
    ]);
  });

  it("follows the arrangement the user made", () => {
    expect(
      keysOf(arrangePanes(panes("bookmarks", "chat:1", "browser"), ["browser", "bookmarks", "chat:1"])),
    ).toEqual(["browser", "bookmarks", "chat:1"]);
  });

  it("opens a pane the arrangement does not name beside the pane it follows by default", () => {
    expect(
      keysOf(arrangePanes(panes("bookmarks", "chat:1", "chat:2", "browser"), ["chat:1", "browser", "bookmarks"])),
    ).toEqual(["chat:1", "chat:2", "browser", "bookmarks"]);
  });

  it("opens a pane with nothing before it by default first", () => {
    expect(keysOf(arrangePanes(panes("bookmarks", "chat:1", "browser"), ["browser", "chat:1"]))).toEqual([
      "bookmarks",
      "browser",
      "chat:1",
    ]);
  });

  it("ignores panes the arrangement names that are not open", () => {
    expect(keysOf(arrangePanes(panes("chat:1", "browser"), ["topics", "browser", "chat:1"]))).toEqual([
      "browser",
      "chat:1",
    ]);
  });
});

describe("movePane", () => {
  it("moves a pane to its index among the others", () => {
    expect(movePane(["a", "b", "c"], "a", 2)).toEqual(["b", "c", "a"]);
    expect(movePane(["a", "b", "c"], "c", 0)).toEqual(["c", "a", "b"]);
    expect(movePane(["a", "b", "c"], "b", 1)).toEqual(["a", "b", "c"]);
  });

  it("keeps a pane moved past either end at that end", () => {
    expect(movePane(["a", "b"], "a", -1)).toEqual(["a", "b"]);
    expect(movePane(["a", "b"], "b", 5)).toEqual(["a", "b"]);
  });
});
