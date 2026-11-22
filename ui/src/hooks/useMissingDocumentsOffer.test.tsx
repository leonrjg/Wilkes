import { render, screen, fireEvent, waitFor, act } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import { ToastProvider } from "../components/Toast";
import { useMissingDocumentsOffer } from "./useMissingDocumentsOffer";
import { useSettingsStore } from "../stores/useSettingsStore";
import { useSemanticStore } from "../stores/useSemanticStore";
import { api } from "../services";

// The stores this hook reads react to their own changes (listing files,
// reading index status), so every other method answers too, with nothing.
vi.mock("../services", () => {
  const named: Record<string, any> = {
    getIndexStatus: vi.fn(),
    indexCoverage: vi.fn(),
    indexMissingDocuments: vi.fn(),
  };
  const api = new Proxy(named, {
    get: (target, key: string) => (target[key] ??= vi.fn().mockResolvedValue(undefined)),
  });
  return { api };
});

const selected = { engine: "Candle", model: "mock-model", dimension: 384 };

const indexedWith = (model: string) => ({
  indexed_files: 97,
  total_chunks: 500,
  built_at: 0,
  build_duration_ms: 0,
  engine: "Candle",
  model_id: model,
  dimension: 384,
  root_path: "/library/papers",
  db_size_bytes: null,
});

function Host() {
  useMissingDocumentsOffer();
  return null;
}

const mount = () =>
  render(
    <ToastProvider>
      <Host />
    </ToastProvider>,
  );

describe("useMissingDocumentsOffer", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useSemanticStore.setState({ coverage: {}, coverageRoots: [] });
    useSettingsStore.setState({
      directory: "/library/papers",
      preferSemantic: true,
      semantic: { selected } as any,
    });
    (api.getIndexStatus as any).mockResolvedValue(indexedWith("mock-model"));
    (api.indexCoverage as any).mockResolvedValue([
      { root: "/library/papers", indexable: 100, covered: 97, complete: false },
    ]);
    (api.indexMissingDocuments as any).mockResolvedValue(undefined);
  });

  it("asks once when the active folder is missing documents, and indexes them on yes", async () => {
    mount();

    const offer = await screen.findByText("3 documents in papers aren't in the semantic index yet.");
    expect(offer).toBeInTheDocument();
    expect(api.indexMissingDocuments).not.toHaveBeenCalled();
    // The fresh reading is the one the folder strip shows too.
    expect(useSemanticStore.getState().coverage["/library/papers"].covered).toBe(97);

    fireEvent.click(screen.getByRole("button", { name: "Index them" }));

    expect(api.indexMissingDocuments).toHaveBeenCalledWith("/library/papers", selected);
    expect(screen.queryByText(/aren't in the semantic index yet/)).not.toBeInTheDocument();
  });

  it("withdraws the question when the folder changes", async () => {
    mount();
    await screen.findByText(/aren't in the semantic index yet/);
    (api.indexCoverage as any).mockResolvedValue([
      { root: "/library/books", indexable: 4, covered: 4, complete: true },
    ]);

    await act(async () => {
      useSettingsStore.setState({ directory: "/library/books" });
    });

    await waitFor(() =>
      expect(screen.queryByText(/aren't in the semantic index yet/)).not.toBeInTheDocument(),
    );
  });

  it("says why when indexing cannot start", async () => {
    (api.indexMissingDocuments as any).mockRejectedValue("A build is already in progress.");
    mount();
    fireEvent.click(await screen.findByRole("button", { name: "Index them" }));

    expect(
      await screen.findByText(/Could not index the missing documents: A build is already in progress/),
    ).toBeInTheDocument();
  });

  it.each([
    ["the index was built with another model", () =>
      (api.getIndexStatus as any).mockResolvedValue(indexedWith("other-model"))],
    ["the folder has no index of its own", () =>
      (api.getIndexStatus as any).mockResolvedValue({ ...indexedWith("mock-model"), indexed_files: 0, total_chunks: 0 })],
    ["semantic search is off", () => useSettingsStore.setState({ preferSemantic: false })],
    ["nothing is missing", () =>
      (api.indexCoverage as any).mockResolvedValue([
        { root: "/library/papers", indexable: 97, covered: 97, complete: true },
      ])],
  ])("does not ask when %s", async (_why, arrange) => {
    arrange();
    mount();

    await act(async () => {});
    await act(async () => {});
    expect(screen.queryByText(/in the semantic index yet/)).not.toBeInTheDocument();
    expect(api.indexMissingDocuments).not.toHaveBeenCalled();
  });
});
