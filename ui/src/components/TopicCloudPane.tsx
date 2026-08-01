import { useEffect } from "react";
import { Loader, Sidebar, X } from "react-feather";
import type { ChunkTopic } from "../lib/types";
import { useSearchStore } from "../stores/useSearchStore";
import { useSettingsStore } from "../stores/useSettingsStore";
import { useTopicsStore } from "../stores/useTopicsStore";
import {
  DEFAULT_TOPIC_INPUT_CAP,
  TopicCloudControls,
  TopicCloudTags,
  topicSearchResults,
} from "./TopicCloudShared";
import { Tooltip } from "./Tooltip";
import { useToasts } from "./Toast";

export default function TopicCloudPane() {
  const { addToast } = useToasts();
  const directory = useSettingsStore((state) => state.directory);
  const dock = useSettingsStore((state) => state.bookmarksDock);
  const setDock = useSettingsStore((state) => state.setBookmarksDock);
  const inputCap = useSettingsStore(
    (state) =>
      state.settings?.semantic.topic_cloud_input_cap ??
      DEFAULT_TOPIC_INPUT_CAP,
  );
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
            <h2 className="text-xs font-semibold text-[var(--text-main)]">
              Topic cloud
            </h2>
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
                aria-label={
                  dock === "Left"
                    ? "Dock topic cloud right"
                    : "Dock topic cloud left"
                }
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

        <TopicCloudControls
          loading={loading}
          result={result}
          granularity={granularity}
          onGranularityChange={setGranularity}
        />
      </header>

      <div className="flex-1 overflow-auto p-3 custom-scrollbar">
        {loading && !result && (
          <div className="flex items-center gap-2 text-xs text-[var(--text-dim)]">
            <Loader size={13} className="animate-spin" />
            Finding topics…
          </div>
        )}
        <TopicCloudTags
          loading={loading}
          result={result}
          selectedTopicKey={selectedTopicKey}
          onActivate={(topic) => void activateTopic(topic)}
        />
      </div>
    </aside>
  );
}
