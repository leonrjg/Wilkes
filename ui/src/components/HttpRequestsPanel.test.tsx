import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { SearchApi } from "../services/api";
import type { HttpCall } from "../lib/types";
import HttpRequestsPanel from "./HttpRequestsPanel";

const calls: HttpCall[] = [
  { id: "1", started_at: "2026-10-19T12:00:00Z", source: "OpenAlex", method: "GET", url: "https://example.test/works", status: 200, duration_ms: 42, error: null },
  { id: "2", started_at: "2026-10-19T12:00:01Z", source: "Zotero", method: "POST", url: "http://localhost/items", status: 503, duration_ms: 10, error: null },
  { id: "3", started_at: "2026-10-19T12:00:02Z", source: "Download", method: "GET", url: "https://example.test/file", status: null, duration_ms: null, error: null },
];

function api() {
  return {
    getHttpCalls: vi.fn().mockResolvedValue(calls),
    clearHttpCalls: vi.fn().mockResolvedValue(undefined),
    writeClipboard: vi.fn().mockResolvedValue(undefined),
  };
}

afterEach(() => vi.useRealTimers());

describe("HTTP request history", () => {
  it("fetches only while active and filters by source and status", async () => {
    const mock = api();
    const { rerender } = render(<HttpRequestsPanel api={mock as unknown as SearchApi} isActive={false} />);
    expect(mock.getHttpCalls).not.toHaveBeenCalled();
    rerender(<HttpRequestsPanel api={mock as unknown as SearchApi} isActive />);
    await screen.findByText("3 of 3 calls");
    fireEvent.change(screen.getByLabelText("Filter HTTP requests"), { target: { value: "errors" } });
    expect(screen.getByText("1 of 3 calls")).toBeTruthy();
    expect(screen.queryByText(/OpenAlex ·/)).toBeNull();
    fireEvent.change(screen.getByLabelText("Filter HTTP requests"), { target: { value: "pending" } });
    expect(screen.getByText("Pending", { selector: "span" })).toBeTruthy();
    fireEvent.change(screen.getByLabelText("Search HTTP requests"), { target: { value: "zotero" } });
    expect(screen.getByText("No requests match these filters.")).toBeTruthy();
    fireEvent.change(screen.getByLabelText("Filter HTTP requests"), { target: { value: "all" } });
    expect(screen.getByText("1 of 3 calls")).toBeTruthy();
  });

  it("does not restore old results from a poll that completes after Clear", async () => {
    const mock = api();
    let resolve!: (value: HttpCall[]) => void;
    mock.getHttpCalls.mockReturnValueOnce(new Promise<HttpCall[]>((done) => { resolve = done; }));
    render(<HttpRequestsPanel api={mock as unknown as SearchApi} isActive />);
    fireEvent.click(screen.getByRole("button", { name: "Clear" }));
    await waitFor(() => expect(mock.clearHttpCalls).toHaveBeenCalledOnce());
    await act(async () => { resolve(calls); });
    expect(screen.getByText("No HTTP requests recorded yet.")).toBeTruthy();
  });

  it("refreshes while open and stops polling when hidden", async () => {
    vi.useFakeTimers();
    const mock = api();
    const { rerender } = render(<HttpRequestsPanel api={mock as unknown as SearchApi} isActive />);
    await act(async () => {});
    expect(mock.getHttpCalls).toHaveBeenCalledTimes(1);
    await act(async () => { await vi.advanceTimersByTimeAsync(1000); });
    expect(mock.getHttpCalls).toHaveBeenCalledTimes(2);
    rerender(<HttpRequestsPanel api={mock as unknown as SearchApi} isActive={false} />);
    await act(async () => { await vi.advanceTimersByTimeAsync(3000); });
    expect(mock.getHttpCalls).toHaveBeenCalledTimes(2);
  });
});
