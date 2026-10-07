import type { LiteratureProviderInfo } from "../types";

/**
 * What to call the source of a cached metadata value.
 *
 * `file` and `zotero` are the two that are not literature providers; any other
 * source is a provider's id, and the name that provider gave itself is in the
 * list the backend reports. An id with no entry (a provider that has since
 * been removed) is shown as the id, which is at least what the stored value
 * was written under.
 */
export function sourceLabel(
  source: string,
  providers: readonly LiteratureProviderInfo[],
): string {
  if (source === "file") return "File";
  if (source === "zotero") return "Zotero";
  return providers.find((provider) => provider.provider === source)?.name ?? source;
}
