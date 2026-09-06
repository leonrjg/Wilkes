import { render, screen, waitFor, fireEvent } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import IndexActivityPanel, { describeJob } from "./IndexActivityPanel";
import { useSettingsStore } from "../stores/useSettingsStore";
import type { IndexActivity, JobSummary, Settings } from "../lib/types";

const ROOT = "/corpus";

const settings = {
  last_directory: ROOT,
  semantic: { worker_timeout_secs: 300, selected: { engine: "Candle" } },
} as unknown as Settings;

function job(overrides: Partial<JobSummary> = {}): JobSummary {
  return {
    id: 1,
    root: ROOT,
    started_at_ms: 1_700_000_000_000,
    ended_at_ms: 1_700_000_060_000,
    state: "interrupted",
    detail: null,
    total_documents: 4,
    counts: { pending: 1, reused: 1, indexed: 1, empty: 0, failed: 1 },
    ...overrides,
  };
}

function activity(overrides: Partial<IndexActivity> = {}): IndexActivity {
  return {
    root: ROOT,
    job: job(),
    document_limit: 500,
    documents: [
      {
        path: "/corpus/broken.pdf",
        stage: "extracting",
        outcome: "failed",
        error: "mupdf: broken xref table",
        chunks: null,
        updated_at_ms: 1_700_000_030_000,
      },
      {
        path: "/corpus/unread.pdf",
        stage: "queued",
        outcome: "pending",
        error: null,
        chunks: null,
        updated_at_ms: 1_700_000_000_000,
      },
      {
        path: "/corpus/done.pdf",
        stage: "embedding",
        outcome: "indexed",
        error: null,
        chunks: 12,
        updated_at_ms: 1_700_000_020_000,
      },
    ],
    history: [job()],
    ...overrides,
  };
}

function makeApi(over: Record<string, any> = {}) {
  return {
    indexActivity: vi.fn().mockResolvedValue(activity()),
    continueIndexJob: vi.fn().mockResolvedValue(undefined),
    retryFailedDocuments: vi.fn().mockResolvedValue(undefined),
    onEmbedProgress: vi.fn().mockResolvedValue(() => {}),
    getWorkerStatuses: vi.fn().mockResolvedValue([]),
    killWorker: vi.fn(),
    setWorkerTimeout: vi.fn(),
    ...over,
  } as any;
}

function renderPanel(api: any) {
  return render(
    <IndexActivityPanel
      api={api}
      settings={settings}
      onUpdateSettings={vi.fn().mockResolvedValue(undefined)}
      isActive
    />,
  );
}

describe("IndexActivityPanel", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useSettingsStore.setState({ directory: ROOT } as any);
  });

  it("reports what a stopped job saved, what failed, and what is left", async () => {
    renderPanel(makeApi());

    expect(await screen.findByText("Interrupted")).toBeInTheDocument();
    // The saved work is stated, because "interrupted" alone reads as "nothing
    // happened" and that is what is no longer true.
    expect(
      await screen.findByText(/2 were saved and do not need reading again/),
    ).toBeInTheDocument();
    expect(await screen.findByText("broken.pdf")).toBeInTheDocument();
    expect(await screen.findByText("mupdf: broken xref table")).toBeInTheDocument();
    expect(await screen.findByText(/12 passages/)).toBeInTheDocument();
  });

  it("offers continuing and retrying as two separate actions", async () => {
    const api = makeApi();
    renderPanel(api);

    const cont = await screen.findByRole("button", {
      name: /Continue with 1 unread document/,
    });
    const retry = await screen.findByRole("button", { name: /Retry 1 failed document/ });

    fireEvent.click(cont);
    await waitFor(() => expect(api.continueIndexJob).toHaveBeenCalledWith(ROOT, settings.semantic.selected));
    expect(api.retryFailedDocuments).not.toHaveBeenCalled();

    fireEvent.click(retry);
    await waitFor(() =>
      expect(api.retryFailedDocuments).toHaveBeenCalledWith(ROOT, settings.semantic.selected),
    );
    expect(api.continueIndexJob).toHaveBeenCalledTimes(1);
  });

  it("does not offer to continue a job that is still running", async () => {
    const api = makeApi({
      indexActivity: vi.fn().mockResolvedValue(
        activity({
          job: job({ state: "running", ended_at_ms: null }),
        }),
      ),
    });
    renderPanel(api);

    expect(await screen.findByText("Running")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Continue with/ })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Retry/ })).not.toBeInTheDocument();
  });

  it("offers nothing when a completed job left nothing to do", async () => {
    const api = makeApi({
      indexActivity: vi.fn().mockResolvedValue(
        activity({
          job: job({
            state: "completed",
            counts: { pending: 0, reused: 0, indexed: 4, empty: 0, failed: 0 },
          }),
          documents: [],
        }),
      ),
    });
    renderPanel(api);

    expect(await screen.findByText("Completed")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Continue with/ })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Retry/ })).not.toBeInTheDocument();
  });

  it("says so plainly when a directory has never been indexed", async () => {
    const api = makeApi({
      indexActivity: vi.fn().mockResolvedValue({
        root: ROOT,
        job: null,
        documents: [],
        document_limit: 500,
        history: [],
      }),
    });
    renderPanel(api);

    expect(
      await screen.findByText(/No indexing job has been recorded/),
    ).toBeInTheDocument();
  });

  it("surfaces a failure to read the activity rather than showing an empty view", async () => {
    const api = makeApi({
      indexActivity: vi.fn().mockRejectedValue(new Error("journal unreadable")),
    });
    renderPanel(api);

    expect(await screen.findByRole("alert")).toHaveTextContent("journal unreadable");
  });

  it("shows the workers beneath the job without being asked", async () => {
    const api = makeApi();
    renderPanel(api);

    expect(await screen.findByText(/Worker Status/i)).toBeInTheDocument();
    await waitFor(() => expect(api.getWorkerStatuses).toHaveBeenCalled());
  });

  it("re-reads the journal when the progress stream reports movement", async () => {
    let emit: ((p: any) => void) | undefined;
    const api = makeApi({
      onEmbedProgress: vi.fn().mockImplementation((handler: any) => {
        emit = handler;
        return Promise.resolve(() => {});
      }),
    });
    renderPanel(api);
    await waitFor(() => expect(api.indexActivity).toHaveBeenCalledTimes(1));

    // Past the throttle that keeps a fast build from refetching per document.
    const real = Date.now();
    const clock = vi.spyOn(Date, "now").mockReturnValue(real + 5000);
    try {
      // The event is a signal, not a fact: the view re-reads rather than
      // accumulating, so it is identical whether or not it saw the stream.
      emit?.({ Build: { files_processed: 2, total_files: 4, done: false } });
      await waitFor(() => expect(api.indexActivity).toHaveBeenCalledTimes(2));

      // A download event is not this view's business.
      emit?.({ Download: { bytes_received: 1, total_bytes: 2, done: false } });
      expect(api.indexActivity).toHaveBeenCalledTimes(2);
    } finally {
      clock.mockRestore();
    }
  });
});

describe("describeJob", () => {
  it("names the saved work for every way a job can stop", () => {
    const counts = { pending: 2, reused: 1, indexed: 2, empty: 0, failed: 1 };
    for (const state of ["cancelled", "interrupted", "failed"] as const) {
      expect(describeJob(job({ state, counts }))).toContain("3 were saved");
    }
  });

  it("does not mention failures a completed job did not have", () => {
    const clean = describeJob(
      job({ state: "completed", counts: { pending: 0, reused: 0, indexed: 4, empty: 0, failed: 0 } }),
    );
    expect(clean).not.toContain("failed");
    const dirty = describeJob(
      job({ state: "completed", counts: { pending: 0, reused: 0, indexed: 3, empty: 0, failed: 1 } }),
    );
    expect(dirty).toContain("1 failed");
  });
});
