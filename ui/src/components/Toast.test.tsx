import { render, screen, fireEvent, act } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { ToastProvider, useToasts } from "./Toast";
import { useEffect } from "react";

const TestComponent = ({ message, options }: { message: string; options?: any }) => {
  const { addToast } = useToasts();
  useEffect(() => {
    addToast(message, options);
  }, [addToast, message, options]);
  return <div>Test Component</div>;
};

describe("Toast", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-04-05T12:00:00Z"));
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("renders a toast message", () => {
    render(
      <ToastProvider>
        <TestComponent message="Hello World" />
      </ToastProvider>
    );

    expect(screen.getByText("Hello World")).toBeInTheDocument();
  });

  it("removes toast after duration", () => {
    render(
      <ToastProvider>
        <TestComponent message="Temporary" options={{ duration: 1000 }} />
      </ToastProvider>
    );

    expect(screen.getByText("Temporary")).toBeInTheDocument();

    act(() => {
      vi.advanceTimersByTime(1001);
    });

    expect(screen.queryByText("Temporary")).not.toBeInTheDocument();
  });

  it("removes toast when X is clicked", () => {
    render(
      <ToastProvider>
        <TestComponent message="Removable" />
      </ToastProvider>
    );

    const closeButton = screen.getByRole("button");
    fireEvent.click(closeButton);

    expect(screen.queryByText("Removable")).not.toBeInTheDocument();
  });

  it("shows different types of toasts", () => {
    const { rerender } = render(
      <ToastProvider>
        <TestComponent message="Error" options={{ type: "error" }} />
      </ToastProvider>
    );
    expect(screen.getByText("Error")).toBeInTheDocument();

    rerender(
      <ToastProvider>
        <TestComponent message="Success" options={{ type: "success" }} />
      </ToastProvider>
    );
    expect(screen.getByText("Success")).toBeInTheDocument();
  });

  it("handles elapsed time for toasts with startTime", async () => {
    const now = Date.now();
    const startTime = now - 5000;
    render(
      <ToastProvider>
        <TestComponent message="Timed" options={{ startTime }} />
      </ToastProvider>
    );

    // Initial render should show some elapsed time
    expect(screen.getByText(/Elapsed:/)).toBeInTheDocument();

    await act(async () => {
      vi.advanceTimersByTime(2000);
    });

    expect(screen.getByText(/Elapsed:/)).toBeInTheDocument();
  });

  it("renders a shimmer bar when requested", () => {
    render(
      <ToastProvider>
        <TestComponent message="Indexing" options={{ startTime: Date.now(), shimmer: true }} />
      </ToastProvider>
    );

    expect(screen.getByTestId("toast-shimmer-bar")).toBeInTheDocument();
  });
});
