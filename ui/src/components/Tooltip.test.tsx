import { act, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { Tooltip } from "./Tooltip";

describe("Tooltip", () => {
  afterEach(() => vi.useRealTimers());

  it("delays pointer opening and cancels a pending open on leave", () => {
    vi.useFakeTimers();
    render(
      <Tooltip content="Preview" delayMs={300}>
        <button>Target</button>
      </Tooltip>,
    );
    const target = screen.getByText("Target");

    fireEvent.mouseEnter(target);
    act(() => vi.advanceTimersByTime(299));
    expect(screen.queryByRole("tooltip")).not.toBeInTheDocument();

    fireEvent.mouseLeave(target);
    act(() => vi.advanceTimersByTime(1));
    expect(screen.queryByRole("tooltip")).not.toBeInTheDocument();

    fireEvent.mouseEnter(target);
    act(() => vi.advanceTimersByTime(300));
    expect(screen.getByRole("tooltip")).toHaveTextContent("Preview");
  });

  it("opens immediately for keyboard focus despite a pointer delay", () => {
    vi.useFakeTimers();
    render(
      <Tooltip content="Preview" delayMs={300}>
        <button>Target</button>
      </Tooltip>,
    );

    fireEvent.focus(screen.getByText("Target"));

    expect(screen.getByRole("tooltip")).toHaveTextContent("Preview");
  });

  it("keeps ordinary tooltips transient", () => {
    render(
      <Tooltip content="Preview">
        <button>Target</button>
      </Tooltip>,
    );
    const target = screen.getByText("Target");

    fireEvent.mouseEnter(target);
    expect(screen.getByRole("tooltip")).toBeInTheDocument();

    fireEvent.mouseLeave(target);
    expect(screen.queryByRole("tooltip")).not.toBeInTheDocument();
  });

  it("keeps an interactive tooltip open while the pointer moves onto it", () => {
    vi.useFakeTimers();
    render(
      <Tooltip content="Preview" interactive>
        <button>Target</button>
      </Tooltip>,
    );
    const target = screen.getByText("Target");

    fireEvent.mouseEnter(target);
    const tooltip = screen.getByRole("tooltip");
    expect(tooltip).toHaveClass("pointer-events-auto", "select-text");

    fireEvent.mouseLeave(target);
    act(() => vi.advanceTimersByTime(100));
    fireEvent.mouseEnter(tooltip);
    act(() => vi.advanceTimersByTime(100));
    expect(screen.getByRole("tooltip")).toBeInTheDocument();

    fireEvent.mouseLeave(tooltip);
    act(() => vi.advanceTimersByTime(149));
    expect(screen.getByRole("tooltip")).toBeInTheDocument();
    act(() => vi.advanceTimersByTime(1));
    expect(screen.queryByRole("tooltip")).not.toBeInTheDocument();
  });

  it("dismisses an interactive tooltip with Escape", () => {
    render(
      <Tooltip content="Preview" interactive>
        <button>Target</button>
      </Tooltip>,
    );

    fireEvent.focus(screen.getByText("Target"));
    expect(screen.getByRole("tooltip")).toBeInTheDocument();

    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("tooltip")).not.toBeInTheDocument();
  });
});
