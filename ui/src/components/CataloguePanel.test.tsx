import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { CatalogueStatus, CatalogueSyncResponse } from "../lib/types";
import CataloguePanel from "./CataloguePanel";

const UNSYNCED: CatalogueStatus = {
  total_records: 0,
  providers: [
    { provider: "libretexts", grain: "textbook", records: 0, synced_at_ms: null },
    { provider: "mit_ocw", grain: "course", records: 0, synced_at_ms: null },
  ],
};

const SYNCED: CatalogueStatus = {
  total_records: 2_219,
  providers: [
    {
      provider: "libretexts",
      grain: "textbook",
      records: 2_219,
      synced_at_ms: Date.now() - 3 * 86_400_000,
    },
    { provider: "mit_ocw", grain: "course", records: 0, synced_at_ms: null },
  ],
};

function syncResponse(overrides: Partial<CatalogueSyncResponse["providers"][0]> = {}) {
  return {
    total_records: 2_219,
    providers: [
      {
        provider: "libretexts",
        grain: "textbook" as const,
        records: 2_219,
        offered: 4_100,
        duplicates: 1_881,
        unusable: 0,
        error: null,
        ...overrides,
      },
    ],
  };
}

describe("CataloguePanel", () => {
  const api = {
    catalogueStatus: vi.fn(() => Promise.resolve(UNSYNCED)),
    catalogueSync: vi.fn(() => Promise.resolve(syncResponse())),
    onCatalogueSyncProgress: vi.fn(() => Promise.resolve(() => {})),
  } as never;

  const withStatus = (status: CatalogueStatus) => {
    (api as unknown as { catalogueStatus: unknown }).catalogueStatus = vi.fn(() =>
      Promise.resolve(status),
    );
  };

  /** Hands the panel a progress report the way the backend would. */
  const emitProgress = async (progress: {
    provider: string;
    pages: number;
    records: number;
  }) => {
    const register = (
      api as unknown as { onCatalogueSyncProgress: ReturnType<typeof vi.fn> }
    ).onCatalogueSyncProgress;
    await waitFor(() => expect(register.mock.calls.length).toBeGreaterThan(0));
    act(() => register.mock.calls[0][0](progress));
  };

  beforeEach(() => {
    vi.clearAllMocks();
    (api as unknown as { onCatalogueSyncProgress: unknown }).onCatalogueSyncProgress =
      vi.fn(() => Promise.resolve(() => {}));
    withStatus(UNSYNCED);
    (api as unknown as { catalogueSync: unknown }).catalogueSync = vi.fn(() =>
      Promise.resolve(syncResponse()),
    );
  });

  /// A provider that has never been fetched has no rows, so the store's own
  /// grouping cannot see it. Listing only what had already synced would show an
  /// empty panel and no way to understand it.
  it("names a provider that has never synced", async () => {
    render(<CataloguePanel api={api} isActive />);
    await screen.findByText("LibreTexts");
    expect(screen.getByText("MIT OpenCourseWare")).toBeTruthy();
    expect(screen.getAllByText("Never synced").length).toBe(2);
  });

  it("says the mirror is empty and what filling it costs", async () => {
    render(<CataloguePanel api={api} isActive />);
    expect(await screen.findByText(/This mirror is empty/i)).toBeTruthy();
    expect(screen.getByText(/takes a few minutes/i)).toBeTruthy();
  });

  /// Fetching all four at once is minutes with nothing to show; the route takes
  /// a list precisely so a caller that wants progress can decline to use it.
  it("syncs one provider at a time so each can be reported as it lands", async () => {
    render(<CataloguePanel api={api} isActive />);
    await screen.findByText("LibreTexts");
    fireEvent.click(screen.getByText("Sync all"));
    await waitFor(() => {
      const sync = (api as unknown as { catalogueSync: ReturnType<typeof vi.fn> })
        .catalogueSync;
      expect(sync.mock.calls.length).toBe(2);
      expect(sync.mock.calls[0][0]).toEqual(["libretexts"]);
      expect(sync.mock.calls[1][0]).toEqual(["mit_ocw"]);
    });
  });

  /// Both counts, because both providers repeat ids across a paged fetch: a
  /// panel showing only what was stored would make a provider whose pagination
  /// changed look like one that simply shrank.
  it("reports what a provider offered alongside what was stored", async () => {
    render(<CataloguePanel api={api} isActive />);
    await screen.findByText("LibreTexts");
    fireEvent.click(screen.getByLabelText("Sync LibreTexts"));
    expect(await screen.findByText(/2,219 stored, 4,100 offered/)).toBeTruthy();
  });

  /// One provider being down is not the others being down.
  it("reports a failing provider without failing the panel", async () => {
    (api as unknown as { catalogueSync: unknown }).catalogueSync = vi.fn(() =>
      Promise.resolve(
        syncResponse({ records: null, offered: null, duplicates: null, error: "429 Too Many Requests" }),
      ),
    );
    render(<CataloguePanel api={api} isActive />);
    await screen.findByText("LibreTexts");
    fireEvent.click(screen.getByLabelText("Sync LibreTexts"));
    expect(await screen.findByText("429 Too Many Requests")).toBeTruthy();
    expect(screen.getByText("MIT OpenCourseWare")).toBeTruthy();
  });

  it("shows what each provider holds and when it last said so", async () => {
    withStatus(SYNCED);
    render(<CataloguePanel api={api} isActive />);
    expect(await screen.findByText(/2,219 records · 3 days ago/)).toBeTruthy();
  });

  /// The mirror is installation-wide now; a panel that read as workspace-scoped
  /// would imply these numbers change when the workspace does.
  it("says the mirror is shared across workspaces", async () => {
    render(<CataloguePanel api={api} isActive />);
    expect(await screen.findByText(/Shared by every workspace/i)).toBeTruthy();
  });

  /// A five-minute fetch that says only "Fetching…" is indistinguishable from
  /// one that has hung.
  it("shows how far a provider's fetch has got while it runs", async () => {
    let release: (value: unknown) => void = () => {};
    (api as unknown as { catalogueSync: unknown }).catalogueSync = vi.fn(
      () => new Promise((resolve) => { release = resolve; }),
    );
    render(<CataloguePanel api={api} isActive />);
    await screen.findByText("LibreTexts");
    fireEvent.click(screen.getByLabelText("Sync LibreTexts"));
    await emitProgress({ provider: "libretexts", pages: 12, records: 1_204 });
    expect(await screen.findByText(/page 12, 1,204 records/)).toBeTruthy();
    await act(async () => {
      release(syncResponse());
    });
  });

  /// Progress belongs to the provider that reported it.
  it("does not show one provider's pages against another", async () => {
    let release: (value: unknown) => void = () => {};
    (api as unknown as { catalogueSync: unknown }).catalogueSync = vi.fn(
      () => new Promise((resolve) => { release = resolve; }),
    );
    render(<CataloguePanel api={api} isActive />);
    await screen.findByText("LibreTexts");
    fireEvent.click(screen.getByLabelText("Sync LibreTexts"));
    await emitProgress({ provider: "mit_ocw", pages: 9, records: 900 });
    expect(screen.queryByText(/page 9, 900 records/)).toBeNull();
    expect(screen.getByText("Fetching…")).toBeTruthy();
    await act(async () => {
      release(syncResponse());
    });
  });
});
