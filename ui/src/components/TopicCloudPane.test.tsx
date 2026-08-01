import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { useSearchStore } from "../stores/useSearchStore";
import { useSettingsStore } from "../stores/useSettingsStore";
import { useTopicsStore } from "../stores/useTopicsStore";
import TopicCloudPane from "./TopicCloudPane";
import { ToastProvider } from "./Toast";

vi.mock("../services", () => ({
  api: {
    chunkTopics: vi.fn(),
    updateSettings: vi.fn().mockResolvedValue(undefined),
    listFiles: vi.fn().mockResolvedValue({ files: [], omitted: [] }),
    cancelSearch: vi.fn().mockResolvedValue(undefined),
  },
}));

import { api } from "../services";

const result = {
  topics: [
    {
      cluster_key: "topic-a",
      chunks: [
        {
          chunk_id: 11,
          file_path: "/library/a.pdf",
          chunk_text: "Graph indexes speed up neighborhood traversal.",
          extraction_byte_range: { start: 10, end: 55 },
          origin: { PdfPage: { page: 3, bbox: null } },
        },
        {
          chunk_id: 12,
          file_path: "/library/b.txt",
          chunk_text: "Edges connect nodes in graph databases.",
          extraction_byte_range: { start: 20, end: 61 },
          origin: { TextFile: { line: 2, col: 1 } },
        },
      ],
      representative_chunk_id: 11,
      chunk_count: 2,
      distinct_document_count: 2,
      cohesion: 0.9,
      label: "Graph Database Indexes",
    },
  ],
  total_chunk_count: 2400,
  sampled_chunk_count: 1500,
  total_document_count: 100,
  sampled_document_count: 100,
  input_cap: 1500,
};

describe("TopicCloudPane", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(api.chunkTopics).mockResolvedValue(result);
    useSettingsStore.setState({
      directory: "/library",
      bookmarksDock: "Right",
      settings: {
        semantic: { topic_cloud_input_cap: 1500 },
      } as any,
      refreshFileList: vi.fn().mockResolvedValue(undefined),
      setBookmarksDock: vi.fn(),
    });
    useTopicsStore.setState({
      paneOpen: true,
      loading: false,
      requestId: 0,
      result: null,
      root: null,
      granularity: "much_fewer",
      selectedTopicKey: null,
    });
    useSearchStore.setState({
      results: [],
      stats: null,
      searching: false,
      hasQuery: false,
      currentSearchId: null,
      lastQuery: null,
      resultContext: null,
    });
  });

  it("loads the capped root cloud and turns a tag into search-result chunks", async () => {
    render(
      <ToastProvider>
        <TopicCloudPane />
      </ToastProvider>,
    );

    expect(await screen.findByText("Graph Database Indexes")).toBeInTheDocument();
    expect(screen.getByText("1,500 of 2,400 chunks")).toBeInTheDocument();
    expect(api.chunkTopics).toHaveBeenCalledWith({
      root: "/library",
      granularity: "much_fewer",
    });

    fireEvent.click(screen.getByText("Graph Database Indexes"));
    await waitFor(() =>
      expect(useSearchStore.getState()).toEqual(
        expect.objectContaining({
          hasQuery: true,
          stats: expect.objectContaining({ files_scanned: 2, total_matches: 2 }),
          resultContext: {
            kind: "topic",
            topicKey: "topic-a",
            subject: "Graph Database Indexes",
          },
        }),
      ),
    );
    expect(
      useSearchStore
        .getState()
        .results.flatMap((file) => file.matches.map((match) => match.matched_text)),
    ).toEqual([
      "Graph indexes speed up neighborhood traversal.",
      "Edges connect nodes in graph databases.",
    ]);
  });

  it("maps chunk locations onto the existing search match contract", async () => {
    render(
      <ToastProvider>
        <TopicCloudPane />
      </ToastProvider>,
    );
    fireEvent.click(await screen.findByText("Graph Database Indexes"));
    const [pdf, text] = useSearchStore.getState().results;
    expect(pdf.matches[0]).toEqual(
      expect.objectContaining({
        matched_text: "Graph indexes speed up neighborhood traversal.",
        origin: { PdfPage: { page: 3, bbox: null } },
        text_range: null,
      }),
    );
    expect(text.matches[0]).toEqual(
      expect.objectContaining({
        origin: { TextFile: { line: 2, col: 1 } },
        text_range: { start: 20, end: 61 },
      }),
    );
  });

  it("defaults both controls to minimal and reloads when granularity rises", async () => {
    render(
      <ToastProvider>
        <TopicCloudPane />
      </ToastProvider>,
    );
    await screen.findByText("Graph Database Indexes");
    expect(screen.getByRole("slider", { name: "Topic input cap" })).toHaveAttribute(
      "aria-valuetext",
      "Minimal 1500",
    );
    expect(screen.getByRole("slider", { name: "Topic input cap" })).toHaveAttribute(
      "max",
      "2",
    );
    const granularity = screen.getByRole("slider", { name: "Topic granularity" });
    expect(granularity).toHaveAttribute("aria-valuetext", "Much fewer");
    fireEvent.change(granularity, { target: { value: "1" } });
    await waitFor(() =>
      expect(api.chunkTopics).toHaveBeenLastCalledWith({
        root: "/library",
        granularity: "fewer",
      }),
    );
  });

  it("debounces granularity changes and reports the completed topic count", async () => {
    let resolveAdjustment!: (value: typeof result) => void;
    vi.mocked(api.chunkTopics)
      .mockResolvedValueOnce(result)
      .mockImplementationOnce(
        () => new Promise((resolve) => { resolveAdjustment = resolve; }),
      );

    render(
      <ToastProvider>
        <TopicCloudPane />
      </ToastProvider>,
    );
    await screen.findByText("Graph Database Indexes");
    expect(screen.getByText("1 topic")).toBeInTheDocument();

    const granularity = screen.getByRole("slider", { name: "Topic granularity" });
    fireEvent.change(granularity, { target: { value: "1" } });
    fireEvent.change(granularity, { target: { value: "2" } });
    fireEvent.change(granularity, { target: { value: "4" } });

    await waitFor(() => expect(api.chunkTopics).toHaveBeenCalledTimes(2));
    expect(api.chunkTopics).toHaveBeenLastCalledWith({
      root: "/library",
      granularity: "much_more",
    });
    expect(screen.getByText("Adjusting…")).toBeInTheDocument();
    expect(granularity).toBeDisabled();
    expect(screen.getByLabelText("Chunk topic cloud")).toHaveAttribute(
      "aria-busy",
      "true",
    );

    await act(async () => {
      resolveAdjustment({
        ...result,
        topics: [
          result.topics[0],
          {
            ...result.topics[0],
            cluster_key: "topic-b",
            label: "Repository Metrics",
          },
        ],
      });
    });
    expect(await screen.findByText("2 topics")).toBeInTheDocument();
  });

  it("uses the active root's chunk count as the maximum input cap", async () => {
    render(
      <ToastProvider>
        <TopicCloudPane />
      </ToastProvider>,
    );
    await screen.findByText("Graph Database Indexes");

    const inputCap = screen.getByRole("slider", { name: "Topic input cap" });
    fireEvent.change(inputCap, { target: { value: "2" } });

    await waitFor(() =>
      expect(api.updateSettings).toHaveBeenCalledWith({
        semantic: expect.objectContaining({ topic_cloud_input_cap: 2400 }),
      }),
    );
  });

  it("collapses the cap to the data maximum for a small root", async () => {
    vi.mocked(api.chunkTopics).mockResolvedValue({
      ...result,
      total_chunk_count: 900,
      sampled_chunk_count: 900,
      input_cap: 900,
    });

    render(
      <ToastProvider>
        <TopicCloudPane />
      </ToastProvider>,
    );
    await screen.findByText("Graph Database Indexes");

    const inputCap = screen.getByRole("slider", { name: "Topic input cap" });
    expect(inputCap).toHaveAttribute("aria-valuetext", "Maximum 900");
    expect(inputCap).toBeDisabled();
  });

  it("uses a non-textual placeholder while a valid label is unavailable", async () => {
    vi.mocked(api.chunkTopics).mockResolvedValue({
      ...result,
      topics: [{ ...result.topics[0], label: null }],
    });

    render(
      <ToastProvider>
        <TopicCloudPane />
      </ToastProvider>,
    );

    expect(
      await screen.findByRole("button", {
        name: "Open topic while label loads",
      }),
    ).toHaveAttribute("aria-busy", "true");
    expect(screen.queryByText(/Topic 1/i)).not.toBeInTheDocument();
    expect(screen.getByLabelText("Chunk topic cloud")).toHaveAttribute(
      "aria-busy",
      "true",
    );
    expect(
      screen.queryByText("Graph indexes speed up neighborhood traversal."),
    ).not.toBeInTheDocument();
  });
});
