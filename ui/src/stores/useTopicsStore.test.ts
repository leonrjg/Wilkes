import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../services", () => ({
  api: {
    chunkTopics: vi.fn(),
    cancelChunkTopics: vi.fn().mockResolvedValue(undefined),
  },
}));

import { api } from "../services";
import { useTopicsStore } from "./useTopicsStore";

const emptyResult = {
  topics: [],
  total_chunk_count: 0,
  sampled_chunk_count: 0,
  total_document_count: 0,
  sampled_document_count: 0,
  input_cap: 0,
};

describe("useTopicsStore", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useTopicsStore.setState({
      paneOpen: true,
      loading: false,
      requestId: null,
      result: null,
      root: null,
      granularity: "much_fewer",
      selectedTopicKey: null,
      document: {
        loading: false,
        requestId: null,
        result: null,
        root: null,
        path: null,
        granularity: "much_fewer",
        selectedTopicKey: null,
      },
    });
  });

  it("cancels and silently ignores a root result after the pane closes", async () => {
    let resolve!: (result: typeof emptyResult) => void;
    vi.mocked(api.chunkTopics).mockImplementationOnce(
      () => new Promise((done) => { resolve = done; }),
    );

    const load = useTopicsStore.getState().load("/library");
    const requestId = useTopicsStore.getState().requestId;
    useTopicsStore.getState().closePane();
    resolve(emptyResult);
    await load;

    expect(api.cancelChunkTopics).toHaveBeenCalledWith(requestId);
    expect(useTopicsStore.getState()).toEqual(
      expect.objectContaining({
        paneOpen: false,
        loading: false,
        requestId: null,
        result: null,
      }),
    );
  });

  it("correlates late labels independently for root and document clouds", () => {
    const topic = {
      cluster_key: "shared-key",
      chunks: [],
      representative_chunk_id: 1,
      chunk_count: 2,
      distinct_document_count: 1,
      cohesion: 0.9,
      label: null,
    };
    useTopicsStore.setState((state) => ({
      requestId: "root-request",
      result: { ...emptyResult, topics: [topic] },
      document: {
        ...state.document,
        requestId: "document-request",
        result: { ...emptyResult, topics: [topic] },
      },
    }));

    useTopicsStore.getState().applyLabel({
      request_id: "document-request",
      cluster_key: "shared-key",
      label: "Document-only label",
    });

    expect(useTopicsStore.getState().result?.topics[0].label).toBeNull();
    expect(useTopicsStore.getState().document.result?.topics[0].label).toBe(
      "Document-only label",
    );
  });
});
