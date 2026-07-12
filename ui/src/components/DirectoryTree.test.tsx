import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { DirectoryTree } from "./DirectoryTree";

describe("DirectoryTree", () => {
  it("selects a root and lazily expands its directories", async () => {
    const onSelect = vi.fn();
    const loadChildren = vi.fn().mockResolvedValue(["/library/articles"]);
    render(
      <DirectoryTree
        roots={["/library"]}
        selected="/library"
        onSelect={onSelect}
        loadChildren={loadChildren}
      />,
    );

    expect(screen.getByRole("treeitem", { name: /library/i })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    fireEvent.click(screen.getByRole("button", { name: "Expand library" }));

    await waitFor(() => expect(loadChildren).toHaveBeenCalledWith("/library"));
    fireEvent.click(await screen.findByRole("button", { name: /^articles$/i }));
    expect(onSelect).toHaveBeenCalledWith("/library/articles");
  });

  it("shows an unreadable folder without discarding the tree", async () => {
    render(
      <DirectoryTree
        roots={["/protected"]}
        selected="/protected"
        onSelect={vi.fn()}
        loadChildren={vi.fn().mockRejectedValue(new Error("denied"))}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Expand protected" }));
    expect(await screen.findByText("Folder can’t be read")).toBeInTheDocument();
    expect(screen.getByRole("tree")).toBeInTheDocument();
  });
});
