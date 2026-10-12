import type { LiteratureProviderInfo } from "../types";

/** Capabilities a draft does not take unless the user adds them. Each makes
 *  the new provider one more that is asked about every document's DOI, beside
 *  the one being extended, for the same answer. */
const NOT_TAKEN_BY_DEFAULT = ["lookup", "references"];

/**
 * The manifest the editor starts from when a provider is extended: everything
 * that provider does for a search, taken whole, and room for what it does not.
 *
 * It names capabilities and nothing about them. What a capability *is* stays
 * in the provider's own manifest, which is the point of extending one.
 */
export function extendingManifest(
  base: Pick<LiteratureProviderInfo, "provider" | "name" | "extendable">,
  takenIds: string[],
): string {
  const offered = base.extendable ?? [];
  const taken = offered.filter((name) => !NOT_TAKEN_BY_DEFAULT.includes(name));
  const left = offered.filter((name) => NOT_TAKEN_BY_DEFAULT.includes(name));

  const stem = `${base.provider.replace(/^custom:/, "").replace(/[^a-z0-9_-]/g, "-")}-extended`;
  let id = stem;
  for (let n = 2; takenIds.includes(id); n += 1) id = `${stem}-${n}`;

  const lines = [
    "manifest_version = 2",
    `id = ${JSON.stringify(id)}`,
    `name = ${JSON.stringify(`${base.name} (extended)`)}`,
    "",
    `## Takes these capabilities, whole, from ${base.name}. Its origin, headers and`,
    `## secrets stay set on ${base.name}. Delete one to leave it behind.`,
    ...(left.length > 0
      ? [`## Also on offer, and left out so documents are not looked up twice: ${left.join(", ")}.`]
      : []),
    `extends = { provider = ${JSON.stringify(base.provider)}, capabilities = [${taken
      .map((name) => JSON.stringify(name))
      .join(", ")}] }`,
  ];
  if (!offered.includes("resolve_download")) {
    lines.push(
      "",
      `## Add what ${base.name} does not do. A download step for its results reads a`,
      "## result's fields, such as {doi}, and what earlier steps found:",
      "## [capabilities.resolve_download]",
      '## url = "{download_url}"',
      "## steps = [",
      '##   { base_url = "https://files.example", path = "/by-doi?doi={doi}", fields = { download_url = "url" } },',
      "## ]",
    );
  }
  return `${lines.join("\n")}\n`;
}
