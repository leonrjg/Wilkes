import { useMemo } from "react";
import type {
  BookmarkClusterGranularity,
  ChunkTopic,
  ChunkTopicsResult,
  FileMatches,
} from "../lib/types";
import { useSettingsStore } from "../stores/useSettingsStore";

export const DEFAULT_TOPIC_INPUT_CAP = 1500;

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

interface InputCapOption {
  label: "Minimal" | "Raised" | "Maximum";
  value: number;
}

function inputCapOptions(
  totalChunkCount: number | undefined,
  configuredCap: number,
) {
  const dataMaximum =
    totalChunkCount ?? Math.max(DEFAULT_TOPIC_INPUT_CAP, configuredCap);
  if (dataMaximum <= DEFAULT_TOPIC_INPUT_CAP) {
    return [{ label: "Maximum", value: dataMaximum }] satisfies InputCapOption[];
  }

  const options: InputCapOption[] = [
    { label: "Minimal", value: DEFAULT_TOPIC_INPUT_CAP },
  ];
  const raised = Math.round((DEFAULT_TOPIC_INPUT_CAP + dataMaximum) / 2);
  if (raised < dataMaximum) options.push({ label: "Raised", value: raised });
  options.push({ label: "Maximum", value: dataMaximum });
  return options;
}

export function topicSearchResults(topic: ChunkTopic): FileMatches[] {
  const byPath = new Map<string, ChunkTopic["chunks"]>();
  for (const chunk of topic.chunks) {
    const members = byPath.get(chunk.file_path);
    if (members) members.push(chunk);
    else byPath.set(chunk.file_path, [chunk]);
  }
  return [...byPath.entries()].map(([path, chunks]) => ({
    path,
    file_type: path.toLowerCase().endsWith(".pdf") ? "Pdf" : "PlainText",
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

interface ControlsProps {
  loading: boolean;
  result: ChunkTopicsResult | null;
  granularity: BookmarkClusterGranularity;
  onGranularityChange: (granularity: BookmarkClusterGranularity) => void;
}

export function TopicCloudControls({
  loading,
  result,
  granularity,
  onGranularityChange,
}: ControlsProps) {
  const inputCap = useSettingsStore(
    (state) =>
      state.settings?.semantic.topic_cloud_input_cap ??
      DEFAULT_TOPIC_INPUT_CAP,
  );
  const setInputCap = useSettingsStore((state) => state.setTopicCloudInputCap);
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

  return (
    <div className="space-y-2">
      <label className="block space-y-1 text-[10px] text-[var(--text-dim)]">
        <span className="flex justify-between">
          <span>Input cap</span>
          <span>
            {selectedCap.label} · {effectiveInputCap.toLocaleString()}
          </span>
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
                DEFAULT_TOPIC_INPUT_CAP,
            )
          }
          className="w-full accent-[var(--accent-blue)] disabled:opacity-50"
        />
      </label>

      <label className="block space-y-1 text-[10px] text-[var(--text-dim)]">
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
              onGranularityChange(
                GRANULARITIES[Number(event.currentTarget.value)] ??
                  "much_fewer",
              )
            }
            className="min-w-0 flex-1 accent-[var(--accent-blue)] disabled:opacity-50"
          />
          <span className="text-[9px]">More</span>
        </span>
      </label>
    </div>
  );
}

interface TagsProps {
  loading: boolean;
  result: ChunkTopicsResult | null;
  selectedTopicKey: string | null;
  documentScoped?: boolean;
  onActivate: (topic: ChunkTopic) => void;
}

export function TopicCloudTags({
  loading,
  result,
  selectedTopicKey,
  documentScoped = false,
  onActivate,
}: TagsProps) {
  const weights = result?.topics.map((topic) => topic.chunk_count) ?? [];
  const minWeight = weights.length ? Math.min(...weights) : 0;
  const maxWeight = weights.length ? Math.max(...weights) : 0;
  const tags = useMemo(
    () =>
      (result?.topics ?? []).map((topic) => {
        const scale =
          maxWeight === minWeight
            ? 0.5
            : (Math.sqrt(topic.chunk_count) - Math.sqrt(minWeight)) /
              (Math.sqrt(maxWeight) - Math.sqrt(minWeight));
        return { topic, label: topic.label, fontSize: 13 + scale * 17 };
      }),
    [maxWeight, minWeight, result],
  );
  const labelsPending = tags.some(({ label }) => !label);
  const titleFor = (topic: ChunkTopic) =>
    documentScoped
      ? `${topic.chunk_count} chunks`
      : `${topic.chunk_count} chunks across ${topic.distinct_document_count} documents`;

  if (!loading && result && result.topics.length === 0) {
    return <p className="text-xs text-[var(--text-dim)]">No topics found.</p>;
  }
  if (tags.length === 0) return null;

  return (
    <div
      aria-label={documentScoped ? "Document topic cloud" : "Chunk topic cloud"}
      aria-busy={loading || labelsPending}
      className={`flex flex-wrap items-center justify-center gap-x-3 gap-y-2 py-2 transition-opacity ${loading ? "opacity-50" : ""}`}
    >
      {tags.map(({ topic, label, fontSize }) =>
        label ? (
          <button
            type="button"
            key={topic.cluster_key}
            aria-pressed={selectedTopicKey === topic.cluster_key}
            title={titleFor(topic)}
            onClick={() => onActivate(topic)}
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
            title={titleFor(topic)}
            onClick={() => onActivate(topic)}
            style={{
              width: `${Math.round(fontSize * 4.5)}px`,
              height: `${Math.round(fontSize * 1.25)}px`,
            }}
            className="max-w-full animate-pulse rounded-full bg-[var(--bg-active)] opacity-70 transition-opacity hover:opacity-100"
          />
        ),
      )}
    </div>
  );
}
