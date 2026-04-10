import type { IndexStatus } from "./types";

export function isUsableSemanticIndex(
  indexStatus: IndexStatus | null,
  directory: string,
): boolean {
  if (!indexStatus || !directory) return false;
  if (indexStatus.indexed_files === 0 || indexStatus.total_chunks === 0) return false;
  if (indexStatus.root_path && indexStatus.root_path !== directory) return false;
  return true;
}
