import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { CatalogueHit } from "../lib/types";

vi.mock("../services", () => ({
  isTauri: true,
  api: {
    catalogueSearch: vi.fn(),
    catalogueAcquire: vi.fn(),
    listFiles: vi.fn(() => Promise.resolve({ files: [], omitted: [] })),
  },
  source: { type: "desktop", deletionKind: "trash", importFiles: vi.fn() },
}));

import { api } from "../services";
import CataloguePane from "./CataloguePane";
import { useCatalogueStore } from "../stores/useCatalogueStore";
import { useSettingsStore } from "../stores/useSettingsStore";
import { useWorkspaceStore } from "../stores/useWorkspaceStore";

const HIT: CatalogueHit = {
  provider: "devdocs",
  external_id: "python~3.12",
  title: "Python 3.12",
  summary: "The language reference and standard library.",
  subject: "Programming",
  authors: "PSF",
  license: "PSF",
  landing_url: "https://example.invalid/python",
  pdf_url: null,
  outline_url: null,
  grain: "reference",
  pages: null,
  recall_score: 6.1,
};

const search = api.catalogueSearch as unknown as ReturnType<typeof vi.fn>;

describe("CataloguePane", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useCatalogueStore.setState({
      paneOpen: true,
      query: "",
      grains: [],
      loading: false,
      answer: null,
      error: null,
      acquired: {},
      acquiring: null,
    });
    useSettingsStore.setState({ directory: "/library", refreshFileList: vi.fn() as never });
    useWorkspaceStore.setState({ workspaces: [], activeWorkspaceId: null } as never);
    search.mockResolvedValue({
      results: [{ key: "pane", terms: ["python", "lists"], hits: [HIT] }],
    });
  });

  const submit = (text: string) => {
    fireEvent.change(screen.getByLabelText("Search the teaching catalogues"), {
      target: { value: text },
    });
    fireEvent.submit(screen.getByLabelText("Search the teaching catalogues"));
  };

  it("searches the mirror and lists what it holds", async () => {
    render(<CataloguePane />);
    submit("python lists");
    expect(await screen.findByText("Python 3.12")).toBeTruthy();
    expect(search.mock.calls[0][0]).toEqual([
      { key: "pane", text: "python lists", grains: null },
    ]);
  });

  /// The catalogues publish at different grains and a question is rarely
  /// answerable at only one, so no selection means all of them — and a chosen
  /// grain is a filter the caller asked for, not a hint.
  it("passes only the grains the user selected", async () => {
    render(<CataloguePane />);
    submit("python lists");
    await screen.findByText("Python 3.12");
    fireEvent.click(screen.getByRole("button", { name: "Reference" }));
    await waitFor(() => {
      expect(search.mock.calls[1][0]).toEqual([
        { key: "pane", text: "python lists", grains: ["reference"] },
      ]);
    });
  });

  /// Two empty answers with different causes. Telling someone "nothing found"
  /// for a word that was never looked for would be a lie by omission.
  it("separates an unsearchable query from an empty catalogue", async () => {
    search.mockResolvedValue({ results: [{ key: "pane", terms: [], hits: [] }] });
    render(<CataloguePane />);
    submit("C");
    expect(
      await screen.findByText(/single letters and very common words are dropped/i),
    ).toBeTruthy();

    search.mockResolvedValue({
      results: [{ key: "pane", terms: ["topology"], hits: [] }],
    });
    submit("topology");
    expect(await screen.findByText(/No catalogue here holds anything matching/i)).toBeTruthy();
  });

  it("says why nothing can be added to a read-only workspace", async () => {
    useWorkspaceStore.setState({
      workspaces: [{ id: "w", name: "W", roots: [], active_root: null, read_only: true }],
      activeWorkspaceId: "w",
    } as never);
    render(<CataloguePane />);
    expect(screen.getByText(/read-only/i)).toBeTruthy();
  });
});
