import { useEffect } from "react";
import { Loader, X } from "react-feather";
import type { ChunkTopic } from "../lib/types";
import { useSearchStore } from "../stores/useSearchStore";
import { useSettingsStore } from "../stores/useSettingsStore";
import { useTopicsStore } from "../stores/useTopicsStore";
import { fileName } from "./DocumentEntryRow";
import {
  DEFAULT_TOPIC_INPUT_CAP,
  TopicCloudControls,
  TopicCloudTags,
  chunkSearchResults,
  topicSearchResults,
} from "./TopicCloudShared";
import { Tooltip } from "@leonrjg/wilkes-reader";
import { useToasts } from "./Toast";

interface Props {
  currentPath: string;
  onClose: () => void;
}

export default function DocumentTopicCloudPane({ currentPath, onClose }: Props) {
  const { addToast } = useToasts();
  const directory = useSettingsStore((state) => state.directory);
  const inputCap = useSettingsStore(
    (state) =>
      state.settings?.semantic.topic_cloud_input_cap ??
      DEFAULT_TOPIC_INPUT_CAP,
  );
  const document = useTopicsStore((state) => state.document);
  const loadDocument = useTopicsStore((state) => state.loadDocument);
  const cancelDocument = useTopicsStore((state) => state.cancelDocument);
  const setGranularity = useTopicsStore(
    (state) => state.setDocumentGranularity,
  );
  const selectTopic = useTopicsStore((state) => state.selectDocumentTopic);
  const showResultSet = useSearchStore((state) => state.showResultSet);

  useEffect(() => {
    if (!directory) return;
    const timeout = window.setTimeout(() => {
      loadDocument(directory, currentPath).catch((error) => {
        console.error("Failed to build document topic cloud:", error);
        addToast("Failed to build document topic cloud", { type: "error" });
      });
    }, 150);
    return () => window.clearTimeout(timeout);
  }, [
    addToast,
    currentPath,
    directory,
    document.granularity,
    inputCap,
    loadDocument,
  ]);

  useEffect(
    () => () => {
      cancelDocument();
    },
    [cancelDocument],
  );

  const activateTopic = async (topic: ChunkTopic) => {
    await showResultSet(topicSearchResults(topic), {
      kind: "topic",
      topicKey: topic.cluster_key,
      subject: topic.label ?? null,
    });
    selectTopic(topic.cluster_key);
  };

  const activateCoverage = async (topic: ChunkTopic) => {
    const coverage = topic.library_coverage;
    if (!coverage) return;
    await showResultSet(chunkSearchResults(coverage.chunks), {
      kind: "topic",
      topicKey: topic.cluster_key,
      subject: topic.label ?? null,
    });
    selectTopic(topic.cluster_key);
  };

  return (
    <aside className="hidden w-64 flex-shrink-0 border-l border-[var(--border-main)] bg-[var(--bg-sidebar)] md:flex md:flex-col">
      <header className="space-y-2 border-b border-[var(--border-main)] p-2">
        <div className="flex items-center gap-1 text-xs font-medium text-[var(--text-main)]">
          <Tooltip content={currentPath} className="break-all font-mono">
            <span className="min-w-0 flex-1 truncate">
              Topics in {fileName(currentPath)}
            </span>
          </Tooltip>
          <Tooltip content="Close document topics">
            <button
              type="button"
              onClick={() => {
                cancelDocument();
                onClose();
              }}
              aria-label="Close document topics"
              className="inline-flex flex-shrink-0 rounded p-0.5 text-[var(--text-dim)] transition-colors hover:bg-[var(--bg-active)] hover:text-[var(--text-main)]"
            >
              <X size={14} />
            </button>
          </Tooltip>
        </div>
        <p className="text-[10px] text-[var(--text-dim)]">
          {document.result
            ? `${document.result.sampled_chunk_count.toLocaleString()} of ${document.result.total_chunk_count.toLocaleString()} chunks`
            : "Indexed passages"}
        </p>
        <TopicCloudControls
          loading={document.loading}
          result={document.result}
          granularity={document.granularity}
          onGranularityChange={setGranularity}
        />
      </header>

      <div className="min-h-0 flex-1 overflow-y-auto p-3 custom-scrollbar">
        {document.loading && !document.result && (
          <div className="flex items-center gap-2 text-xs text-[var(--text-dim)]">
            <Loader size={13} className="animate-spin" />
            Finding topics…
          </div>
        )}
        <TopicCloudTags
          loading={document.loading}
          result={document.result}
          selectedTopicKey={document.selectedTopicKey}
          documentScoped
          onActivate={(topic) => void activateTopic(topic)}
          onActivateCoverage={(topic) => void activateCoverage(topic)}
        />
      </div>
    </aside>
  );
}
