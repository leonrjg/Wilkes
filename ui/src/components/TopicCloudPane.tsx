import { useEffect, useMemo } from "react";
import { Loader, Sidebar, X } from "react-feather";
import type { BookmarkClusterGranularity, ChunkTopic } from "../lib/types";
import { useSearchStore } from "../stores/useSearchStore";
import { useSettingsStore } from "../stores/useSettingsStore";
import { useTopicsStore } from "../stores/useTopicsStore";
import { Tooltip } from "./Tooltip";
import { useToasts } from "./Toast";

const GRANULARITIES: readonly BookmarkClusterGranularity[] = [
  "much_fewer",
  "fewer",
  "balanced",
  "more",
  "much_more",
];

const GRANULARITY_LABELS: Record<BookmarkClusterGranularity, string> = {
  much_fewer: "Much fewer",
  fewer: "Fewer",
  balanced: "Balanced",
  more: "More",
  much_more: "Much more",
};

const DEFAULT_INPUT_CAP = 1500;

interface InputCapOption {
  label: "Minimal" | "Raised" | "Maximum";
  value: number;
}

function inputCapOptions(totalChunkCount: number | undefined, configuredCap: number) {
  const dataMaximum = totalChunkCount ?? Math.max(DEFAULT_INPUT_CAP, configuredCap);
  if (dataMaximum <= DEFAULT_INPUT_CAP) {
    return [{ label: "Maximum", value: dataMaximum }] satisfies InputCapOption[];
  }

  const options: InputCapOption[] = [
    { label: "Minimal", value: DEFAULT_INPUT_CAP },
  ];
  const raised = Math.round((DEFAULT_INPUT_CAP + dataMaximum) / 2);
  if (raised < dataMaximum) options.push({ label: "Raised", value: raised });
  options.push({ label: "Maximum", value: dataMaximum });
  return options;
}

function topicSearchResults(topic: ChunkTopic) {
  const byPath = new Map<string, ChunkTopic["chunks"]>();
  for (const chunk of topic.chunks) {
    const members = byPath.get(chunk.file_path);
    if (members) members.push(chunk);
    else byPath.set(chunk.file_path, [chunk]);
  }
  return [...byPath.entries()].map(([path, chunks]) => ({
    path,
    file_type: path.toLowerCase().endsWith(".pdf")
      ? ("Pdf" as const)
      : ("PlainText" as const),
    matches: chunks.map((chunk) => ({
      text_range:
        "TextFile" in chunk.origin ? chunk.extraction_byte_range : null,
      matched_text: chunk.chunk_text,
      context_before: "",
      context_after: "",
      origin: chunk.origin,
    })),
  }));
}

export default function TopicCloudPane() {
  const { addToast } = useToasts();
  const directory = useSettingsStore((state) => state.directory);
  const dock = useSettingsStore((state) => state.bookmarksDock);
  const setDock = useSettingsStore((state) => state.setBookmarksDock);
  const inputCap = useSettingsStore(
    (state) => state.settings?.semantic.topic_cloud_input_cap ?? DEFAULT_INPUT_CAP,
  );
  const setInputCap = useSettingsStore((state) => state.setTopicCloudInputCap);
  const loading = useTopicsStore((state) => state.loading);
  const result = useTopicsStore((state) => state.result);
  const granularity = useTopicsStore((state) => state.granularity);
  const selectedTopicKey = useTopicsStore((state) => state.selectedTopicKey);
  const closePane = useTopicsStore((state) => state.closePane);
  const setGranularity = useTopicsStore((state) => state.setGranularity);
  const selectTopic = useTopicsStore((state) => state.selectTopic);
  const load = useTopicsStore((state) => state.load);
  const showResultSet = useSearchStore((state) => state.showResultSet);

  useEffect(() => {
    if (!directory) return;
    const timeout = window.setTimeout(() => {
      load(directory).catch((error) => {
        console.error("Failed to build topic cloud:", error);
        addToast("Failed to build topic cloud", { type: "error" });
      });
    }, 150);
    return () => window.clearTimeout(timeout);
  }, [addToast, directory, granularity, inputCap, load]);

  const weights = result?.topics.map((topic) => topic.chunk_count) ?? [];
  const minWeight = weights.length ? Math.min(...weights) : 0;
  const maxWeight = weights.length ? Math.max(...weights) : 0;
  const capOptions = inputCapOptions(result?.total_chunk_count, inputCap);
  const effectiveInputCap = result
    ? Math.min(inputCap, result.total_chunk_count)
    : inputCap;
  const capIndex = capOptions.reduce(
    (best, option, index) =>
      Math.abs(option.value - effectiveInputCap) <
      Math.abs(capOptions[best].value - effectiveInputCap)
        ? index
        : best,
    0,
  );
  const selectedCap = capOptions[capIndex];
  const granularityIndex = GRANULARITIES.indexOf(granularity);
  const granularityStatus = loading
    ? result
      ? "Adjusting…"
      : "Finding…"
    : result
      ? `${result.topics.length} ${result.topics.length === 1 ? "topic" : "topics"}`
      : "";

  const tags = useMemo(
    () =>
      (result?.topics ?? []).map((topic) => {
        const scale =
          maxWeight === minWeight
            ? 0.5
            : (Math.sqrt(topic.chunk_count) - Math.sqrt(minWeight)) /
              (Math.sqrt(maxWeight) - Math.sqrt(minWeight));
        return {
          topic,
          label: topic.label,
          fontSize: 13 + scale * 17,
        };
      }),
    [maxWeight, minWeight, result],
  );
  const labelsPending = tags.some(({ label }) => !label);

  const activateTopic = async (topic: ChunkTopic) => {
    await showResultSet(topicSearchResults(topic), {
      kind: "topic",
      topicKey: topic.cluster_key,
      subject: topic.label ?? null,
    });
    selectTopic(topic.cluster_key);
  };

  return (
    <aside className="flex h-full flex-col border-l border-[var(--border-main)] bg-[var(--bg-sidebar)]">
      <header className="flex flex-col gap-2 border-b border-[var(--border-main)] p-2">
        <div className="flex items-center justify-between gap-2">
          <div>
            <h2 className="text-xs font-semibold text-[var(--text-main)]">Topic cloud</h2>
            <p className="text-[10px] text-[var(--text-dim)]">
              {result
                ? `${result.sampled_chunk_count.toLocaleString()} of ${result.total_chunk_count.toLocaleString()} chunks`
                : "Indexed passages"}
            </p>
          </div>
          <div className="flex items-center gap-1">
            <Tooltip content={dock === "Left" ? "Dock right" : "Dock left"}>
              <button
                type="button"
                aria-label={dock === "Left" ? "Dock topic cloud right" : "Dock topic cloud left"}
                onClick={() => setDock(dock === "Left" ? "Right" : "Left")}
                className="flex h-7 w-7 items-center justify-center rounded border border-[var(--border-main)] bg-[var(--bg-active)] text-[var(--text-muted)] hover:text-[var(--text-main)]"
              >
                <Sidebar size={13} />
              </button>
            </Tooltip>
            <Tooltip content="Close topic cloud">
              <button
                type="button"
                aria-label="Close topic cloud"
                onClick={closePane}
                className="flex h-7 w-7 items-center justify-center rounded border border-[var(--border-main)] bg-[var(--bg-active)] text-[var(--text-muted)] hover:text-[var(--text-main)]"
              >
                <X size={14} />
              </button>
            </Tooltip>
          </div>
        </div>

        <label className="space-y-1 text-[10px] text-[var(--text-dim)]">
          <span className="flex justify-between">
            <span>Input cap</span>
            <span>{selectedCap.label} · {effectiveInputCap.toLocaleString()}</span>
          </span>
          <input
            type="range"
            min={0}
            max={capOptions.length - 1}
            step={1}
            value={capIndex}
            disabled={loading || capOptions.length === 1}
            aria-label="Topic input cap"
            aria-valuetext={`${selectedCap.label} ${effectiveInputCap}`}
            onChange={(event) =>
              setInputCap(
                capOptions[Number(event.currentTarget.value)]?.value ??
                  DEFAULT_INPUT_CAP,
              )
            }
            className="w-full accent-[var(--accent-blue)] disabled:opacity-50"
          />
        </label>

        <label className="space-y-1 text-[10px] text-[var(--text-dim)]">
          <span className="flex justify-between">
            <span>{GRANULARITY_LABELS[granularity]}</span>
            <span aria-live="polite">{granularityStatus}</span>
          </span>
          <span className="flex items-center gap-2">
            <span className="text-[9px]">Fewer</span>
            <input
              type="range"
              min={0}
              max={GRANULARITIES.length - 1}
              step={1}
              value={granularityIndex}
              disabled={loading}
              aria-label="Topic granularity"
              aria-valuetext={GRANULARITY_LABELS[granularity]}
              onChange={(event) =>
                setGranularity(
                  GRANULARITIES[Number(event.currentTarget.value)] ??
                    "much_fewer",
                )
              }
              className="min-w-0 flex-1 accent-[var(--accent-blue)] disabled:opacity-50"
            />
            <span className="text-[9px]">More</span>
          </span>
        </label>
      </header>

      <div className="flex-1 overflow-auto p-3 custom-scrollbar">
        {loading && !result && (
          <div className="flex items-center gap-2 text-xs text-[var(--text-dim)]">
            <Loader size={13} className="animate-spin" />
            Finding topics…
          </div>
        )}
        {!loading && result && result.topics.length === 0 && (
          <p className="text-xs text-[var(--text-dim)]">No topics found.</p>
        )}
        {tags.length > 0 && (
          <div
            aria-label="Chunk topic cloud"
            aria-busy={loading || labelsPending}
            className={`flex flex-wrap items-center justify-center gap-x-3 gap-y-2 py-2 transition-opacity ${loading ? "opacity-50" : ""}`}
          >
            {tags.map(({ topic, label, fontSize }) =>
              label ? (
                <button
                  type="button"
                  key={topic.cluster_key}
                  aria-pressed={selectedTopicKey === topic.cluster_key}
                  title={`${topic.chunk_count} chunks across ${topic.distinct_document_count} documents`}
                  onClick={() => void activateTopic(topic)}
                  style={{ fontSize: `${fontSize}px` }}
                  className={`max-w-full rounded px-1.5 py-0.5 leading-tight transition-colors ${
                    selectedTopicKey === topic.cluster_key
                      ? "bg-[var(--accent-blue-muted)] text-[var(--accent-blue)]"
                      : "text-[var(--text-muted)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-main)]"
                  }`}
                >
                  {label}
                </button>
              ) : (
                <button
                  type="button"
                  key={topic.cluster_key}
                  aria-label="Open topic while label loads"
                  aria-busy="true"
                  title={`${topic.chunk_count} chunks across ${topic.distinct_document_count} documents`}
                  onClick={() => void activateTopic(topic)}
                  style={{
                    width: `${Math.round(fontSize * 4.5)}px`,
                    height: `${Math.round(fontSize * 1.25)}px`,
                  }}
                  className="max-w-full animate-pulse rounded-full bg-[var(--bg-active)] opacity-70 transition-opacity hover:opacity-100"
                />
              ),
            )}
          </div>
        )}

      </div>
    </aside>
  );
}
