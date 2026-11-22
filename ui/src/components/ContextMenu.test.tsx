import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { afterEach, describe, it, expect, vi } from "vitest";
import { ExternalLink } from "react-feather";
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

  it("submits inline actions without opening a separate prompt", async () => {
    const submit = vi.fn().mockResolvedValue(undefined);
    const onClose = vi.fn();
    render(
      <ContextMenu
        menu={{
          position: { x: 10, y: 10 },
          target: null,
          items: [{
            id: "new-tag",
            label: "Create and add tag",
            inlineInput: { placeholder: "New tag…", submitLabel: "Add", submit },
          }],
        }}
        onClose={onClose}
      />,
    );

    fireEvent.change(screen.getByRole("textbox", { name: "Create and add tag" }), {
      target: { value: "Reviewed" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Add" }));

    expect(submit).toHaveBeenCalledWith("Reviewed");
    await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
  });

  it("runs a row's second action on its own, without the row's", () => {
    const run = vi.fn();
    const secondary = vi.fn();
    const onClose = vi.fn();
    render(
      <ContextMenu
        menu={{
          position: { x: 10, y: 10 },
          target: null,
          items: [{
            id: "row",
            label: "Second",
            run,
            secondary: { label: "Open Second in a new window", icon: ExternalLink, run: secondary },
          }],
        }}
        onClose={onClose}
      />,
    );

    fireEvent.click(screen.getByRole("menuitem", { name: "Open Second in a new window" }));

    expect(secondary).toHaveBeenCalledTimes(1);
    expect(run).not.toHaveBeenCalled();
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("announces the checked item and starts the keyboard on it", async () => {
    render(
      <ContextMenu
        menu={{
          position: { x: 10, y: 10 },
          target: null,
          items: [
            { id: "a", label: "First", checked: false, run: vi.fn() },
            { id: "b", label: "Second", checked: true, run: vi.fn() },
          ],
        }}
        onClose={vi.fn()}
      />,
    );

    const second = screen.getByRole("menuitemradio", { name: "Second" });
    expect(second).toHaveAttribute("aria-checked", "true");
    expect(screen.getByRole("menuitemradio", { name: "First" })).toHaveAttribute("aria-checked", "false");
    await waitFor(() => expect(second).toHaveFocus());
  });

  it("moves through every action, a row's second one included, with the arrow keys", async () => {
    render(
      <ContextMenu
        menu={{
          position: { x: 10, y: 10 },
          target: null,
          items: [
            {
              id: "a",
              label: "First",
              run: vi.fn(),
              secondary: { label: "First, elsewhere", icon: ExternalLink, run: vi.fn() },
            },
            { id: "b", label: "Second", run: vi.fn() },
          ],
        }}
        onClose={vi.fn()}
      />,
    );
    const first = screen.getByRole("menuitem", { name: "First" });
    await waitFor(() => expect(first).toHaveFocus());

    fireEvent.keyDown(window, { key: "ArrowDown" });
    expect(screen.getByRole("menuitem", { name: "First, elsewhere" })).toHaveFocus();
    fireEvent.keyDown(window, { key: "ArrowDown" });
    expect(screen.getByRole("menuitem", { name: "Second" })).toHaveFocus();
    // Wraps around at either end.
    fireEvent.keyDown(window, { key: "ArrowDown" });
    expect(first).toHaveFocus();
    fireEvent.keyDown(window, { key: "ArrowUp" });
    expect(screen.getByRole("menuitem", { name: "Second" })).toHaveFocus();
    fireEvent.keyDown(window, { key: "Home" });
    expect(first).toHaveFocus();
  });

  it("gives the keyboard back to what opened it when it closes", async () => {
    const opener = document.createElement("button");
    document.body.appendChild(opener);
    opener.focus();
    const menu = makeMenu([{ id: "a", label: "First", run: vi.fn() }]);
    const { rerender } = render(<ContextMenu menu={menu} onClose={vi.fn()} />);
    await waitFor(() => expect(screen.getByRole("menuitem", { name: "First" })).toHaveFocus());

    rerender(<ContextMenu menu={null} onClose={vi.fn()} />);

    expect(opener).toHaveFocus();
    opener.remove();
  });
});
