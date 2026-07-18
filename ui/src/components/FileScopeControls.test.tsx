import { act, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useResearchStore } from "../stores/useResearchStore";
import { FileScopeControls } from "./FileScopeControls";

const { validateSmartCollection } = vi.hoisted(() => ({
  validateSmartCollection: vi.fn(),
}));

vi.mock("../services", () => ({
  api: { validateSmartCollection },
}));

describe("FileScopeControls", () => {
  const saveCollection = vi.fn();

  beforeEach(() => {
    vi.useFakeTimers();
    validateSmartCollection.mockResolvedValue({ valid: true });
    saveCollection.mockResolvedValue({ id: "saved", name: "Cited", expression: "citation_count > 1" });
    useResearchStore.setState({
      tags: [],
      collections: [],
      selectedCollectionId: null,
      selectedTagId: null,
      draftCollectionExpression: null,
      load: vi.fn().mockResolvedValue(undefined),
      saveCollection,
    } as any);
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.clearAllMocks();
  });

  it("previews a valid expression and saves that exact draft", async () => {
    render(<FileScopeControls matchCount={3} />);
    fireEvent.click(screen.getByRole("button", { name: "Create collection" }));
    fireEvent.change(screen.getByRole("textbox", { name: "Collection name" }), {
      target: { value: "Cited" },
    });
    fireEvent.change(screen.getByRole("textbox", { name: "Collection expression" }), {
      target: { value: "citation_count > 1" },
    });

    await act(async () => {
      vi.advanceTimersByTime(250);
      await Promise.resolve();
    });

    expect(validateSmartCollection).toHaveBeenCalledWith("citation_count > 1");
    expect(useResearchStore.getState().draftCollectionExpression).toBe("citation_count > 1");
    expect(screen.getByText("3 matching")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await act(async () => { await Promise.resolve(); });

    expect(saveCollection).toHaveBeenCalledWith(null, {
      name: "Cited",
      expression: "citation_count > 1",
    });
    expect(useResearchStore.getState().selectedCollectionId).toBe("saved");
    expect(useResearchStore.getState().draftCollectionExpression).toBeNull();
  });
});
