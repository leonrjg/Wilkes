import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import TidyUpDialog, { destinationLabels } from "./TidyUpDialog";
import { ToastProvider } from "./Toast";
import { useTidyStore } from "../stores/useTidyStore";
import { useSettingsStore } from "../stores/useSettingsStore";
import { useSearchStore } from "../stores/useSearchStore";
import type { FileEntry, TidyPlan } from "../lib/types";

const { mockTidyFolder, mockMoveFile } = vi.hoisted(() => ({
  mockTidyFolder: vi.fn(),
  mockMoveFile: vi.fn(),
}));

vi.mock("../services", () => ({
  api: {},
  source: { tidyFolder: mockTidyFolder, moveFile: mockMoveFile },
  isTauri: true,
}));

const entry = (path: string, title?: string): FileEntry => ({
  path,
  size_bytes: 1,
  file_type: "Pdf",
  extension: "pdf",
  title,
});

const RECSYS = "/lib/Literature/Recommendation systems";
const CONTENT = "/lib/Literature/Content analysis";
const LEGAL = "/lib/Literature/Legal precedent retrieval";

const plan: TidyPlan = {
  folder: "/lib/Reference",
  destinations: [
    { path: CONTENT, documents: 45, indexed_documents: 40 },
    { path: LEGAL, documents: 45, indexed_documents: 45 },
    { path: RECSYS, documents: 0, indexed_documents: 0 },
  ],
  suggestions: [
    {
      entry: entry("/lib/Reference/koren-2009.pdf", "Matrix factorization"),
      ranking: [
        { destination: RECSYS, score: 1.7 },
        { destination: CONTENT, score: 1.1 },
        { destination: LEGAL, score: -1 },
      ],
      close_call: false,
    },
    {
      entry: entry("/lib/Reference/linden-2003.pdf"),
      ranking: [
        { destination: RECSYS, score: 1.8 },
        { destination: CONTENT, score: 0.8 },
      ],
      close_call: false,
    },
    {
      entry: entry("/lib/Reference/esmeli-2026.pdf"),
      ranking: [
        { destination: CONTENT, score: 1.29 },
        { destination: RECSYS, score: 1.28 },
      ],
      close_call: true,
    },
  ],
  unindexed: [entry("/lib/Reference/scan.pdf")],
};

const refreshFileList = vi.fn().mockResolvedValue(undefined);

function renderDialog() {
  return render(
    <ToastProvider>
      <TidyUpDialog />
    </ToastProvider>,
  );
}

beforeEach(() => {
  vi.clearAllMocks();
  mockTidyFolder.mockResolvedValue(plan);
  mockMoveFile.mockResolvedValue("moved");
  useSettingsStore.setState({ refreshFileList });
  useSearchStore.setState({ hasQuery: false });
  useTidyStore.setState({ folder: null });
});

describe("TidyUpDialog", () => {
  it("renders nothing until a folder is chosen", () => {
    renderDialog();
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(mockTidyFolder).not.toHaveBeenCalled();
  });

  it("groups suggestions by destination and leaves close calls unselected", async () => {
    renderDialog();
    act(() => useTidyStore.getState().open("/lib/Reference"));

    const recsys = await screen.findByRole("region", {
      name: "Suggested for Recommendation systems",
    });
    expect(mockTidyFolder).toHaveBeenCalledWith("/lib/Reference");
    expect(within(recsys).getByText("koren-2009.pdf")).toBeInTheDocument();
    expect(within(recsys).getByText("Matrix factorization")).toBeInTheDocument();
    expect(within(recsys).getByText("linden-2003.pdf")).toBeInTheDocument();
    expect(screen.getByRole("checkbox", { name: "Move koren-2009.pdf" })).toBeChecked();

    const content = screen.getByRole("region", { name: "Suggested for Content analysis" });
    expect(within(content).getByText("Close call")).toHaveAttribute(
      "title",
      "Fits Recommendation systems nearly as well",
    );
    expect(screen.getByRole("checkbox", { name: "Move esmeli-2026.pdf" })).not.toBeChecked();
    expect(screen.getByRole("button", { name: "Move 2 documents" })).toBeEnabled();

    expect(screen.getByText(/1 document is not in the semantic index/)).toBeInTheDocument();
    expect(screen.getByText(/Content analysis \(40 of 45\)/)).toBeInTheDocument();
    expect(screen.getByText(/as they are empty: Recommendation systems/)).toBeInTheDocument();
  });

  it("moves the selected documents to the destinations chosen", async () => {
    renderDialog();
    act(() => useTidyStore.getState().open("/lib/Reference"));
    await screen.findByRole("region", { name: "Suggested for Recommendation systems" });

    fireEvent.change(screen.getByRole("combobox", { name: "Destination for koren-2009.pdf" }), {
      target: { value: CONTENT },
    });
    fireEvent.click(screen.getByRole("checkbox", { name: "Move linden-2003.pdf" }));
    fireEvent.change(screen.getByRole("combobox", { name: "Destination for esmeli-2026.pdf" }), {
      target: { value: RECSYS },
    });
    fireEvent.click(screen.getByRole("button", { name: "Move 2 documents" }));

    await waitFor(() => expect(useTidyStore.getState().folder).toBeNull());
    expect(mockMoveFile.mock.calls).toEqual([
      ["/lib/Reference/koren-2009.pdf", CONTENT],
      ["/lib/Reference/esmeli-2026.pdf", RECSYS],
    ]);
    expect(refreshFileList).toHaveBeenCalled();
    expect(await screen.findByText("Moved 2 documents")).toBeInTheDocument();
  });

  it("toggles a whole group from its header", async () => {
    renderDialog();
    act(() => useTidyStore.getState().open("/lib/Reference"));
    const group = await screen.findByRole("checkbox", {
      name: "Move all suggested for Recommendation systems",
    });

    fireEvent.click(group);

    expect(screen.getByRole("checkbox", { name: "Move koren-2009.pdf" })).not.toBeChecked();
    expect(screen.getByRole("checkbox", { name: "Move linden-2003.pdf" })).not.toBeChecked();
    expect(screen.getByRole("button", { name: "Move 0 documents" })).toBeDisabled();
  });

  it("keeps the dialog open on what is left when a move fails", async () => {
    mockMoveFile
      .mockResolvedValueOnce("moved")
      .mockRejectedValueOnce(new Error("A file or folder with that name already exists"));
    renderDialog();
    act(() => useTidyStore.getState().open("/lib/Reference"));
    await screen.findByRole("region", { name: "Suggested for Recommendation systems" });

    fireEvent.click(screen.getByRole("button", { name: "Move 2 documents" }));

    expect(
      await screen.findByText(/Moved 1; 1 could not be moved\. linden-2003\.pdf: A file/),
    ).toBeInTheDocument();
    await waitFor(() => expect(mockTidyFolder).toHaveBeenCalledTimes(2));
    expect(useTidyStore.getState().folder).toBe("/lib/Reference");
  });

  it("shows why no plan could be made", async () => {
    mockTidyFolder.mockRejectedValue(new Error("No semantic index found. Build the index first."));
    renderDialog();
    act(() => useTidyStore.getState().open("/lib/Reference"));

    expect(await screen.findByRole("alert")).toHaveTextContent("No semantic index found");
    expect(screen.getByRole("button", { name: "Move 0 documents" })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(useTidyStore.getState().folder).toBeNull();
  });

  it("names destinations that share a name by their parent too", () => {
    const labels = destinationLabels([
      { path: "/a/Papers/2024", documents: 1, indexed_documents: 1 },
      { path: "/a/Books/2024", documents: 1, indexed_documents: 1 },
      { path: "/a/Notes", documents: 1, indexed_documents: 1 },
    ]);
    expect([...labels.values()]).toEqual(["Papers/2024", "Books/2024", "Notes"]);
  });
});
