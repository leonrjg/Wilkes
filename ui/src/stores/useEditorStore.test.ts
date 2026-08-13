import { beforeEach, describe, expect, it } from "vitest";
import { useEditorStore } from "./useEditorStore";

describe("useEditorStore", () => {
  beforeEach(() => {
    localStorage.clear();
    useEditorStore.setState({ buffers: {}, activeEditorPath: null });
  });

  it("drops stale completion events and keeps provenance on the current id", () => {
    const store = useEditorStore.getState();
    store.ensureBuffer("/library/draft.md", "Draft");
    store.beginCompletion("/library/draft.md", "new");
    store.applyCompletionEvent("/library/draft.md", "old", {
      kind: "shown",
      text: "stale",
      mode: "append",
    });
    expect(useEditorStore.getState().buffers["/library/draft.md"].completion?.text).toBe("");

    store.applyCompletionEvent("/library/draft.md", "new", {
      kind: "retrieval",
      hyde_query: "A likely continuation",
      sources: [{
        path: "/library/source.pdf",
        title: "Source",
        page: 3,
        chunkIds: ["7"],
        score: 0.81,
        pinned: true,
      }],
    });
    store.applyCompletionEvent("/library/draft.md", "new", {
      kind: "shown",
      text: "grounded text",
      mode: "append",
    });
    const buffer = useEditorStore.getState().buffers["/library/draft.md"];
    expect(buffer.completion?.text).toBe("grounded text");
    expect(buffer.completion?.sources[0].page).toBe(3);
    expect(buffer.suggestionHistory).toEqual(["grounded text"]);
    expect(buffer.status).toBe("idle");
  });

  it("retains unique suggestions for regeneration until the document position changes", () => {
    const store = useEditorStore.getState();
    store.ensureBuffer("/library/draft.md", "Draft");
    for (const [id, text] of [["one", "First"], ["two", "Second"], ["three", "First"]]) {
      store.beginCompletion("/library/draft.md", id);
      store.applyCompletionEvent("/library/draft.md", id, {
        kind: "shown",
        text,
        mode: "append",
      });
    }
    expect(useEditorStore.getState().buffers["/library/draft.md"].suggestionHistory)
      .toEqual(["Second", "First"]);

    store.setCursor("/library/draft.md", 2);
    expect(useEditorStore.getState().buffers["/library/draft.md"].suggestionHistory).toEqual([]);
  });

  it("moves from library to prefer on the first pin and back when it is removed", () => {
    const store = useEditorStore.getState();
    store.ensureBuffer("/library/draft.md", "Draft");
    store.togglePin("/library/draft.md", "/library/source.md");
    expect(useEditorStore.getState().buffers["/library/draft.md"].scope).toEqual({
      mode: "prefer",
      pinned: ["/library/source.md"],
      excluded: [],
    });
    store.setScopeMode("/library/draft.md", "only");
    expect(useEditorStore.getState().buffers["/library/draft.md"].scope.mode).toBe("only");
    store.togglePin("/library/draft.md", "/library/source.md");
    expect(useEditorStore.getState().buffers["/library/draft.md"].scope).toEqual({
      mode: "library",
      pinned: [],
      excluded: [],
    });
  });

  it("keeps pinned and excluded files mutually exclusive and allows restoration", () => {
    const path = "/library/draft.md";
    const source = "/library/source.md";
    const store = useEditorStore.getState();
    store.ensureBuffer(path, "Draft");
    store.togglePin(path, source);
    store.excludeFromContext(path, source);

    expect(useEditorStore.getState().buffers[path].scope).toEqual({
      mode: "library",
      pinned: [],
      excluded: [source],
    });

    useEditorStore.getState().togglePin(path, source);
    expect(useEditorStore.getState().buffers[path].scope).toEqual({
      mode: "prefer",
      pinned: [source],
      excluded: [],
    });

    useEditorStore.getState().excludeFromContext(path, source);
    useEditorStore.getState().restoreToContext(path, source);
    expect(useEditorStore.getState().buffers[path].scope).toEqual({
      mode: "library",
      pinned: [],
      excluded: [],
    });
  });
});
