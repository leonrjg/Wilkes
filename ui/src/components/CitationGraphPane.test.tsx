import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { api } from "../services";
import { useSettingsStore } from "../stores/useSettingsStore";
import CitationGraphPane from "./CitationGraphPane";

let metadataHandler: ((updates: Array<{ path: string }>) => void) | null = null;

vi.mock("../services", () => ({
  api: {
    citationLinks: vi.fn(),
    listFiles: vi.fn(() => Promise.resolve({ files: [], omitted: [] })),
    onFileMetadataUpdated: vi.fn((handler) => {
      metadataHandler = handler;
      return Promise.resolve(vi.fn());
    }),
  },
}));

const entry = (path: string) => ({ path, file_type: "Pdf" }) as any;

describe("CitationGraphPane", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    metadataHandler = null;
    useSettingsStore.setState({ directory: "/docs" });
    vi.mocked(api.citationLinks).mockResolvedValue({
      references: [],
      cited_by: [],
      all_references: [],
    });
  });

  it("renders references and cited-by documents and opens a selected document", async () => {
    vi.mocked(api.citationLinks).mockResolvedValue({
      references: [entry("/docs/reference.pdf")],
      cited_by: [entry("/docs/citing.pdf")],
      all_references: [
        {
          doi: "10.1000/reference",
          citation_line: "Smith (2024). A cited work. doi:10.1000/reference",
        },
        { doi: "10.1000/missing", citation_line: null },
      ],
    });
    const onOpenDocument = vi.fn();

    render(
      <CitationGraphPane
        currentPath="/docs/anchor.pdf"
        doi="10.1/anchor"
        onOpenDocument={onOpenDocument}
        onClose={vi.fn()}
      />,
    );

    expect(await screen.findByText("References in your library")).toBeInTheDocument();
    expect(screen.getByText("Cited by in your library")).toBeInTheDocument();
    const references = screen.getByText("References");
    const citedBy = screen.getByText("Cited by in your library");
    expect(
      citedBy.compareDocumentPosition(references) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
    expect(screen.getByRole("button", { name: "References, 2" })).toBeInTheDocument();
    expect(screen.getByText("Smith (2024). A cited work. doi:10.1000/reference")).toBeInTheDocument();
    expect(screen.getByText("10.1000/missing")).toBeInTheDocument();
    fireEvent.click(screen.getByText("reference.pdf"));
    expect(onOpenDocument).toHaveBeenCalledWith("/docs/reference.pdf");
  });

  it("shows an explicit empty state", async () => {
    render(
      <CitationGraphPane
        currentPath="/docs/anchor.pdf"
        doi="10.1/anchor"
        onOpenDocument={vi.fn()}
        onClose={vi.fn()}
      />,
    );

    expect(
      await screen.findByText("No citation references found"),
    ).toBeInTheDocument();
  });

  it("refetches when metadata enrichment completes for the anchor", async () => {
    vi.mocked(api.citationLinks)
      .mockResolvedValueOnce({ references: [], cited_by: [], all_references: [] })
      .mockResolvedValueOnce({
        references: [entry("/docs/new-reference.pdf")],
        cited_by: [],
        all_references: [],
      });

    render(
      <CitationGraphPane
        currentPath="/docs/anchor.pdf"
        doi="10.1/anchor"
        onOpenDocument={vi.fn()}
        onClose={vi.fn()}
      />,
    );

    await screen.findByText("No citation references found");
    await waitFor(() => expect(metadataHandler).not.toBeNull());
    act(() => metadataHandler?.([{ path: "/docs/anchor.pdf" }]));

    expect(await screen.findByText("new-reference.pdf")).toBeInTheDocument();
    expect(api.citationLinks).toHaveBeenCalledTimes(2);
  });

  it("shows an error state when citation loading fails", async () => {
    vi.mocked(api.citationLinks).mockRejectedValue(new Error("offline"));

    render(
      <CitationGraphPane
        currentPath="/docs/anchor.pdf"
        doi="10.1/anchor"
        onOpenDocument={vi.fn()}
        onClose={vi.fn()}
      />,
    );

    expect(await screen.findByText("Citation graph unavailable")).toBeInTheDocument();
  });
});
