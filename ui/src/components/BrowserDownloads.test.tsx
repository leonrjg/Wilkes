import { fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { useBrowserStore } from "../stores/useBrowserStore";
import { useViewerStore } from "../stores/useViewerStore";
import BrowserDownloads from "./BrowserDownloads";

describe("BrowserDownloads", () => {
  beforeEach(() => useBrowserStore.setState({ downloads: [] }));

  it("shows nothing without downloads", () => {
    const { container } = render(<BrowserDownloads />);
    expect(container).toBeEmptyDOMElement();
  });

  it("opens a saved download in the viewer", () => {
    const openFile = vi.fn();
    useViewerStore.setState({ openFile } as never);
    useBrowserStore.setState({
      downloads: [{ url: "u", name: "a.epub", path: "/lib/a.epub", state: "saved" }],
    });
    render(<BrowserDownloads />);
    expect(screen.getByText("Saved a.epub to the library")).toBeTruthy();
    fireEvent.click(screen.getByText("Open"));
    expect(openFile).toHaveBeenCalledWith("/lib/a.epub");
  });

  it("says why a download was not saved", () => {
    useBrowserStore.setState({
      downloads: [{ url: "u", name: "b.zip", path: null, state: "refused", reason: "No library folder is open to save it in" }],
    });
    render(<BrowserDownloads />);
    expect(screen.getByText("Did not save b.zip: No library folder is open to save it in")).toBeTruthy();
  });
});
