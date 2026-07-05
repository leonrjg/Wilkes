import { act, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import GlobalErrorReporter from "./GlobalErrorReporter";
import { ToastProvider } from "./Toast";
import { resetErrorReportingForTests } from "../lib/errorReporting";

function renderReporter() {
  return render(
    <ToastProvider>
      <GlobalErrorReporter />
    </ToastProvider>,
  );
}

describe("GlobalErrorReporter", () => {
  const originalConsoleError = console.error;

  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-04-05T12:00:00Z"));
    resetErrorReportingForTests();
    console.error = vi.fn();
  });

  afterEach(() => {
    console.error = originalConsoleError;
    vi.useRealTimers();
  });

  it("surfaces window errors as timed error toasts", () => {
    renderReporter();

    act(() => {
      window.dispatchEvent(new ErrorEvent("error", { error: new Error("Window exploded") }));
    });

    expect(screen.getByText("Window exploded")).toBeInTheDocument();

    act(() => {
      vi.advanceTimersByTime(10_001);
    });

    expect(screen.queryByText("Window exploded")).not.toBeInTheDocument();
  });

  it("surfaces unhandled promise rejections", () => {
    renderReporter();

    act(() => {
      const event = new Event("unhandledrejection") as PromiseRejectionEvent;
      Object.defineProperties(event, {
        promise: { value: Promise.resolve() },
        reason: { value: new Error("Async exploded") },
      });
      window.dispatchEvent(
        event,
      );
    });

    expect(screen.getByText("Async exploded")).toBeInTheDocument();
  });

  it("surfaces caught errors that are only logged to console.error", () => {
    renderReporter();

    act(() => {
      console.error("PDF document load failed:", new Error("Bad PDF"));
    });

    expect(screen.getByText("PDF document load failed: Bad PDF")).toBeInTheDocument();
  });

  it("dedupes repeated global errors briefly", () => {
    renderReporter();

    act(() => {
      console.error("Repeated failure");
      console.error("Repeated failure");
    });

    expect(screen.getAllByText("Repeated failure")).toHaveLength(1);
  });
});
