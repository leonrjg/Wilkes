import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import SelectionActions from "./SelectionActions";
import type { DocumentSelection } from "./preview/selection";
import type { SelectionSlotApi } from "./preview/slots";

/**
 * Wilkes' own selection chrome, tested on its own. It used to be exercised
 * through PdfViewer, which meant a reader test asserting on this application's
 * menu wording -- the readers have no business knowing the word "Bookmark".
 */

const selection: DocumentSelection = {
  quote: "selected text",
  origin: { TextFile: { line: 1, col: 0 } },
  text_range: { start: 0, end: 13 },
  rects: [],
};

function makeApi(): SelectionSlotApi {
  return { dismiss: vi.fn(), clear: vi.fn(), setPinned: vi.fn() };
}

describe("SelectionActions", () => {
  it("offers nothing when the host supplied no actions", () => {
    const { container } = render(<SelectionActions selection={selection} api={makeApi()} />);
    expect(container).toBeEmptyDOMElement();
  });

  it("bookmarks the selection, then clears and dismisses", () => {
    const api = makeApi();
    const onAddBookmark = vi.fn();
    render(
      <SelectionActions selection={selection} api={api} onAddBookmark={onAddBookmark} />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Bookmark" }));

    expect(onAddBookmark).toHaveBeenCalledWith(selection);
    expect(api.clear).toHaveBeenCalled();
    expect(api.dismiss).toHaveBeenCalled();
  });

  it("hides the chat actions until a chat backend is available", () => {
    render(
      <SelectionActions
        selection={selection}
        api={makeApi()}
        onAddBookmark={vi.fn()}
        onExplain={vi.fn()}
        onAsk={vi.fn()}
      />,
    );

    expect(screen.queryByRole("button", { name: "Explain" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Ask about this" })).not.toBeInTheDocument();
  });

  it("explains the selection", () => {
    const onExplain = vi.fn();
    render(
      <SelectionActions
        selection={selection}
        api={makeApi()}
        showChatActions
        onExplain={onExplain}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Explain" }));
    expect(onExplain).toHaveBeenCalledWith(selection);
  });

  it("asks a question about the selection, pinning itself while the input has focus", () => {
    const api = makeApi();
    const onAsk = vi.fn();
    render(
      <SelectionActions selection={selection} api={api} showChatActions onAsk={onAsk} />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Ask about this" }));
    // Opening the input collapses the document selection, so the chrome must
    // tell the reader to hold it open or typing destroys what is typed into.
    expect(api.setPinned).toHaveBeenCalledWith(true);

    fireEvent.change(screen.getByPlaceholderText("Ask about this…"), {
      target: { value: "Why is this important?" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));

    expect(onAsk).toHaveBeenCalledWith(selection, "Why is this important?");
    expect(api.dismiss).toHaveBeenCalled();
  });

  it("will not send an empty question", () => {
    const onAsk = vi.fn();
    render(
      <SelectionActions selection={selection} api={makeApi()} showChatActions onAsk={onAsk} />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Ask about this" }));
    expect(screen.getByRole("button", { name: "Send" })).toBeDisabled();
    expect(onAsk).not.toHaveBeenCalled();
  });

  it("unpins when the question is cancelled", () => {
    const api = makeApi();
    render(
      <SelectionActions selection={selection} api={api} showChatActions onAsk={vi.fn()} />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Ask about this" }));
    vi.mocked(api.setPinned).mockClear();
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));

    expect(api.setPinned).toHaveBeenCalledWith(false);
  });
});
