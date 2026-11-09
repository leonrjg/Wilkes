import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const { mockFileCover } = vi.hoisted(() => ({ mockFileCover: vi.fn() }));

vi.mock("../services", () => ({ api: { fileCover: mockFileCover } }));

import { FULL_COVER_EDGE, THUMBNAIL_EDGE } from "../lib/covers";
import { CoverViewer, DocumentCover } from "./DocumentCover";

/** Every element the observer is watching. */
const observed = new Set<Element>();
let notifyObserver: IntersectionObserverCallback;

function setVisible(isIntersecting: boolean) {
  act(() => {
    notifyObserver(
      [...observed].map((target) => ({ target, isIntersecting }) as IntersectionObserverEntry),
      {} as IntersectionObserver,
    );
  });
}

/** The module keeps one observer for the whole list, so this is constructed
 *  once, by the first test, and every later test hears through that one. */
class ObserverClass {
  constructor(callback: IntersectionObserverCallback) {
    notifyObserver = callback;
  }
  observe = (element: Element) => observed.add(element);
  unobserve = (element: Element) => observed.delete(element);
  disconnect = vi.fn();
}

describe("DocumentCover", () => {
  beforeEach(() => {
    observed.clear();
    mockFileCover.mockReset();
    mockFileCover.mockResolvedValue(new ArrayBuffer(4));
    vi.stubGlobal("IntersectionObserver", ObserverClass);
    let next = 0;
    URL.createObjectURL = vi.fn(() => `blob:cover-${next++}`);
    URL.revokeObjectURL = vi.fn();
  });

  it("asks for nothing until the row is on screen", async () => {
    const { container } = render(<DocumentCover path="/library/a.pdf" version="1:1" />);

    expect(mockFileCover).not.toHaveBeenCalled();
    expect(container.querySelector("img")).toBeNull();

    setVisible(true);

    await waitFor(() => expect(container.querySelector("img")).toHaveAttribute("src", "blob:cover-0"));
    expect(mockFileCover).toHaveBeenCalledWith("/library/a.pdf", THUMBNAIL_EDGE, expect.any(AbortSignal));
  });

  it("lets go of the picture when the row leaves the screen", async () => {
    const { container } = render(<DocumentCover path="/library/a.pdf" version="1:1" />);
    setVisible(true);
    await waitFor(() => expect(container.querySelector("img")).not.toBeNull());

    setVisible(false);

    expect(container.querySelector("img")).toBeNull();
    expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:cover-0");
  });

  it("draws the new cover when the file changes", async () => {
    const { rerender } = render(<DocumentCover path="/library/a.pdf" version="1:1" />);
    setVisible(true);
    await waitFor(() => expect(mockFileCover).toHaveBeenCalledTimes(1));

    rerender(<DocumentCover path="/library/a.pdf" version="2:1" />);

    await waitFor(() => expect(mockFileCover).toHaveBeenCalledTimes(2));
  });

  it("opens on a click without the click reaching the row", async () => {
    const onOpen = vi.fn();
    const onRowClick = vi.fn();
    render(
      <div onClick={onRowClick}>
        <DocumentCover path="/library/a.pdf" version="1:1" onOpen={onOpen} />
      </div>,
    );
    setVisible(true);

    fireEvent.click(await screen.findByRole("button", { name: "Show cover of a.pdf" }));

    expect(onOpen).toHaveBeenCalledTimes(1);
    expect(onRowClick).not.toHaveBeenCalled();
  });

  it("leaves a click to the row when it cannot open", async () => {
    const onRowClick = vi.fn();
    const { container } = render(
      <div onClick={onRowClick}>
        <DocumentCover path="/library/a.pdf" version="1:1" />
      </div>,
    );
    setVisible(true);
    await waitFor(() => expect(container.querySelector("img")).not.toBeNull());

    fireEvent.click(container.querySelector("img")!);

    expect(onRowClick).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole("button")).toBeNull();
  });

  it("keeps the placeholder and reports it when a cover cannot be drawn", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    mockFileCover.mockRejectedValue(new Error("no page"));
    const { container } = render(<DocumentCover path="/library/a.pdf" version="1:1" />);

    setVisible(true);

    await waitFor(() => expect(warn).toHaveBeenCalled());
    expect(container.querySelector("img")).toBeNull();
    expect(container.querySelector("svg")).not.toBeNull();
    warn.mockRestore();
  });

  it("runs at most four loads, and the row that scrolled away gives up its turn", async () => {
    const finishers: Array<(bytes: ArrayBuffer) => void> = [];
    mockFileCover.mockImplementation(
      () => new Promise<ArrayBuffer>((resolve) => { finishers.push(resolve); }),
    );
    const paths = ["a", "b", "c", "d", "e", "f"];
    const views = paths.map((name) =>
      render(<DocumentCover path={`/library/${name}.pdf`} version="1:1" />));

    setVisible(true);
    await waitFor(() => expect(mockFileCover).toHaveBeenCalledTimes(4));

    views[5].unmount();
    await act(async () => {
      finishers[0](new ArrayBuffer(4));
    });

    await waitFor(() => expect(mockFileCover).toHaveBeenCalledTimes(5));
    expect(mockFileCover.mock.calls[4][0]).toBe("/library/e.pdf");

    await act(async () => {
      finishers.slice(1).forEach((finish) => finish(new ArrayBuffer(4)));
    });
    views.forEach((view) => view.unmount());
  });
});

describe("CoverViewer", () => {
  beforeEach(() => {
    mockFileCover.mockReset();
    mockFileCover.mockResolvedValue(new ArrayBuffer(4));
    URL.createObjectURL = vi.fn(() => "blob:full");
    URL.revokeObjectURL = vi.fn();
  });

  it("shows the document's cover at its full size and revokes it on closing", async () => {
    const { unmount } = render(<CoverViewer path="/library/a.pdf" onClose={vi.fn()} />);

    expect(await screen.findByRole("img", { name: "Cover of a.pdf" })).toHaveAttribute("src", "blob:full");
    expect(mockFileCover).toHaveBeenCalledWith("/library/a.pdf", FULL_COVER_EDGE, expect.any(AbortSignal));

    unmount();
    expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:full");
  });

  it("closes on a click and on Escape", async () => {
    const onClose = vi.fn();
    render(<CoverViewer path="/library/a.pdf" onClose={onClose} />);
    await screen.findByRole("img");

    fireEvent.click(screen.getByRole("dialog"));
    expect(onClose).toHaveBeenCalledTimes(1);

    fireEvent.keyDown(window, { key: "Escape" });
    expect(onClose).toHaveBeenCalledTimes(2);
  });

  it("says so when the cover cannot be drawn", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    mockFileCover.mockRejectedValue(new Error("no page"));

    render(<CoverViewer path="/library/a.pdf" onClose={vi.fn()} />);

    expect(await screen.findByText("This picture could not be drawn.")).toBeInTheDocument();
    warn.mockRestore();
  });
});
