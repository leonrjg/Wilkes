import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { CopyButton } from "./CopyButton";

describe("CopyButton", () => {
  afterEach(() => vi.useRealTimers());

  it("shows copied content after a successful copy and then resets", async () => {
    vi.useFakeTimers();
    const copy = vi.fn().mockResolvedValue(undefined);
    render(<CopyButton copy={copy} copiedChildren="Copied">Copy</CopyButton>);

    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Copy" }));
    });
    expect(copy).toHaveBeenCalledOnce();
    expect(screen.getByRole("button", { name: "Copied" })).toHaveTextContent("Copied");

    act(() => vi.advanceTimersByTime(2_000));
    expect(screen.getByRole("button", { name: "Copy" })).toHaveTextContent("Copy");
  });

  it("does not show copied content when copying fails", async () => {
    const copy = vi.fn().mockRejectedValue(new Error("unavailable"));
    const onCopyError = vi.fn();
    render(<CopyButton copy={copy} onCopyError={onCopyError}>Copy</CopyButton>);

    fireEvent.click(screen.getByRole("button", { name: "Copy" }));

    await waitFor(() => expect(onCopyError).toHaveBeenCalledOnce());
    expect(screen.getByRole("button", { name: "Copy" })).toHaveTextContent("Copy");
  });
});
