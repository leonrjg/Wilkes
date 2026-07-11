import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { afterEach, describe, it, expect, vi } from "vitest";
import { ContextMenu, type ContextMenuItem } from "./ContextMenu";

function makeMenu(items: Pick<ContextMenuItem, "id" | "label" | "dividerBefore" | "run">[]) {
  return { position: { x: 10, y: 10 }, target: null, items };
}

describe("ContextMenu", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("shows a spinner and blocks the menu while an async action is in flight", async () => {
    let resolveRun!: () => void;
    const pending = new Promise<void>((resolve) => {
      resolveRun = resolve;
    });
    const asyncRun = vi.fn(() => pending);
    const onClose = vi.fn();

    render(
      <ContextMenu
        menu={makeMenu([
          { id: "async", label: "Async Action", run: asyncRun },
          { id: "other", label: "Other Action", run: vi.fn() },
        ])}
        onClose={onClose}
      />,
    );

    fireEvent.click(screen.getByRole("menuitem", { name: "Async Action" }));

    // Immediate feedback: spinner on the clicked item, menu stays open, all
    // items disabled to prevent a second action.
    expect(screen.getByTestId("context-menu-spinner")).toBeInTheDocument();
    expect(screen.getByRole("menuitem", { name: /Async Action/ })).toBeDisabled();
    expect(screen.getByRole("menuitem", { name: "Other Action" })).toBeDisabled();
    expect(onClose).not.toHaveBeenCalled();

    resolveRun();
    await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
  });

  it("closes immediately for synchronous actions", () => {
    const onClose = vi.fn();
    render(
      <ContextMenu menu={makeMenu([{ id: "sync", label: "Sync Action", run: vi.fn() }])} onClose={onClose} />,
    );

    fireEvent.click(screen.getByRole("menuitem", { name: "Sync Action" }));

    expect(screen.queryByTestId("context-menu-spinner")).not.toBeInTheDocument();
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("renders dividers before grouped actions", () => {
    render(
      <ContextMenu
        menu={makeMenu([
          { id: "open", label: "Open", run: vi.fn() },
          { id: "rename", label: "Rename", dividerBefore: true, run: vi.fn() },
        ])}
        onClose={vi.fn()}
      />,
    );

    expect(screen.getAllByRole("separator")).toHaveLength(1);
  });

  it("closes and handles rejected async actions", async () => {
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => {});
    const onClose = vi.fn();
    render(
      <ContextMenu
        menu={makeMenu([
          { id: "reject", label: "Rejected Action", run: vi.fn().mockRejectedValue(new Error("boom")) },
        ])}
        onClose={onClose}
      />,
    );

    fireEvent.click(screen.getByRole("menuitem", { name: "Rejected Action" }));

    expect(screen.getByTestId("context-menu-spinner")).toBeInTheDocument();
    await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
    expect(consoleError).toHaveBeenCalledWith("context menu action failed", expect.any(Error));
  });
});
