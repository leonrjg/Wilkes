import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { ReadingOrder } from "../lib/types";

const { mockReadingOrder } = vi.hoisted(() => ({ mockReadingOrder: vi.fn() }));

vi.mock("../services", () => ({ api: { readingOrder: mockReadingOrder } }));

import ReadingOrderStrip from "./ReadingOrderStrip";
import { useReadingOrderStore } from "../stores/useReadingOrderStore";

function order(overrides: Partial<ReadingOrder> = {}): ReadingOrder {
  return {
    root: "/lib",
    theme: null,
    subjects: [
      { name: "algebra", documents: 3 },
      { name: "calculus", documents: 1 },
    ],
    documents: [
      { path: "/lib/a.pdf", placement: "foundation", subjects: ["algebra"], after: [], needed_by: 1, theme_share: null },
      { path: "/lib/b.pdf", placement: "specialised", subjects: ["calculus"], after: [], needed_by: 0, theme_share: null },
      { path: "/lib/c.pdf", placement: "standalone", subjects: ["topology"], after: [], needed_by: 0, theme_share: null },
      { path: "/lib/d.pdf", placement: "no_text", subjects: [], after: [], needed_by: 0, theme_share: null },
    ],
    ...overrides,
  };
}

describe("ReadingOrderStrip", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useReadingOrderStore.getState().resetForWorkspace();
  });

  it("says how many documents were ordered and how many were not", async () => {
    mockReadingOrder.mockResolvedValue(order());
    render(<ReadingOrderStrip />);
    await useReadingOrderStore.getState().load("/lib", "listing");

    expect(await screen.findByText("2 in order · 1 at any time · 1 not placed")).toBeInTheDocument();
    expect(mockReadingOrder).toHaveBeenCalledWith({ root: "/lib", theme: null });
  });

  it("says so when nothing could be ordered", async () => {
    mockReadingOrder.mockResolvedValue(
      order({ documents: order().documents.slice(2), subjects: [] }),
    );
    render(<ReadingOrderStrip />);
    await useReadingOrderStore.getState().load("/lib", "listing");

    expect(
      await screen.findByText("No document here names another's subject, so there is no order to give."),
    ).toBeInTheDocument();
  });

  it("asks again with a subject as the theme, and without it when the subject is pressed again", async () => {
    mockReadingOrder.mockResolvedValue(order());
    render(<ReadingOrderStrip />);
    await useReadingOrderStore.getState().load("/lib", "listing");

    const themed = order({ theme: "algebra" });
    themed.documents[0].theme_share = 0.5;
    mockReadingOrder.mockResolvedValue(themed);
    fireEvent.click(await screen.findByRole("button", { name: "Rank by algebra" }));

    await waitFor(() =>
      expect(mockReadingOrder).toHaveBeenLastCalledWith({ root: "/lib", theme: "algebra" }),
    );
    expect(await screen.findByText("1 of 4 discuss algebra")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Rank by algebra" })).toHaveAttribute("aria-pressed", "true");

    mockReadingOrder.mockResolvedValue(order());
    fireEvent.click(screen.getByRole("button", { name: "Rank by algebra" }));
    await waitFor(() =>
      expect(mockReadingOrder).toHaveBeenLastCalledWith({ root: "/lib", theme: null }),
    );
  });

  it("takes a typed theme and clears it", async () => {
    mockReadingOrder.mockResolvedValue(order());
    render(<ReadingOrderStrip />);
    await useReadingOrderStore.getState().load("/lib", "listing");

    const input = screen.getByRole("textbox", { name: "Theme to rank the documents by" });
    fireEvent.change(input, { target: { value: "  eigenvalues " } });
    fireEvent.submit(input);
    await waitFor(() =>
      expect(mockReadingOrder).toHaveBeenLastCalledWith({ root: "/lib", theme: "eigenvalues" }),
    );

    fireEvent.click(await screen.findByRole("button", { name: "Clear theme" }));
    await waitFor(() =>
      expect(mockReadingOrder).toHaveBeenLastCalledWith({ root: "/lib", theme: null }),
    );
  });

  it("shows why an order could not be worked out", async () => {
    mockReadingOrder.mockRejectedValue(new Error("The document text index is unavailable"));
    vi.spyOn(console, "error").mockImplementation(() => undefined);
    render(<ReadingOrderStrip />);
    await useReadingOrderStore.getState().load("/lib", "listing");

    expect(await screen.findByRole("alert")).toHaveTextContent("The document text index is unavailable");
  });

  it("does not ask twice for the same listing, and asks again on refresh", async () => {
    mockReadingOrder.mockResolvedValue(order());
    render(<ReadingOrderStrip />);
    await useReadingOrderStore.getState().load("/lib", "listing");
    await useReadingOrderStore.getState().load("/lib", "listing");
    expect(mockReadingOrder).toHaveBeenCalledTimes(1);

    fireEvent.click(screen.getByRole("button", { name: "Work out the reading order again" }));
    await waitFor(() => expect(mockReadingOrder).toHaveBeenCalledTimes(2));
  });
});
