import type { IndexStatus, SearchMode } from "./types";

export function isUsableSemanticIndex(
  indexStatus: IndexStatus | null,
  directory?: string,
): boolean {
  if (!indexStatus || directory === "") return false;
  if (indexStatus.indexed_files === 0 || indexStatus.total_chunks === 0) return false;
  return true;
}

/** True for the modes whose retrieval reaches the semantic index. Both the
 *  combined mode and semantic-only need a built index to contribute related
 *  passages, so readiness checks and invalidation ask this rather than
 *  comparing against one mode name. */
export function usesSemanticIndex(mode: SearchMode | undefined): boolean {
  return mode === "Semantic" || mode === "Hybrid";
}
