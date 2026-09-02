import type {
  IntegrationState,
  IntegrationStatus,
  IntegrationsSettings,
  OpenAlexSettings,
  SemanticScholarSettings,
  ZoteroSettings,
} from "../types";
import type { SearchApi } from "../../services/api";

/**
 * The built-in providers, described rather than written out.
 *
 * `custom-integrations.md` §8: fixing the Rust duplication while leaving one
 * hand-written form per provider in the UI would move the cost, not remove it.
 * A provider is a row in this table — its fields, how to check it, what to say
 * when it cannot be reached — and `ProviderForm` renders any row. The fourth
 * provider is four lines here and nothing anywhere else.
 *
 * The manifest-defined providers of `CustomIntegrations` are deliberately not
 * in this table: their fields are not known until a user writes them, which is
 * the whole point of them, so they are described by their manifest instead and
 * get their own tab.
 */

export type ProviderFieldKind = "url" | "text" | "email" | "password" | "select";

export interface ProviderField {
  /** Key within this provider's settings object. */
  key: string;
  label: string;
  kind: ProviderFieldKind;
  options?: readonly { readonly id: string; readonly label: string }[];
  /**
   * Store an emptied field as `null` rather than `""`.
   *
   * True for the optional ones — an API key, a contact address — where the
   * backend's `Option<String>` distinguishes "not set" from "set to nothing",
   * and a `""` would be sent as a header on every request.
   */
  nullable?: boolean;
}

/** Any built-in provider's settings. Every one carries `enabled` and a base URL. */
export type ProviderSettings =
  | ZoteroSettings
  | SemanticScholarSettings
  | OpenAlexSettings;

export interface BuiltInProvider {
  /** Key within `IntegrationsSettings`, and the tab's id. */
  key: "zotero" | "semantic_scholar" | "openalex";
  /** Tab label. */
  name: string;
  /**
   * Label on the enable checkbox. Spelled out per provider rather than derived
   * from `name`: it is what a screen reader announces and what the tests click.
   */
  enableLabel: string;
  defaults: ProviderSettings;
  fields: readonly ProviderField[];
  status: (api: SearchApi) => Promise<IntegrationStatus>;
  /** What to report when the status call itself throws. */
  unreachable: { state: IntegrationState; message: string };
}

const CITATION_STYLES = [
  { id: "chicago-note-bibliography", label: "Chicago notes" },
  { id: "apa", label: "APA" },
  { id: "ieee", label: "IEEE" },
  { id: "modern-language-association", label: "MLA" },
] as const;

export const BUILT_IN_PROVIDERS: readonly BuiltInProvider[] = [
  {
    key: "zotero",
    name: "Zotero",
    enableLabel: "Enable Zotero integration",
    defaults: {
      enabled: false,
      base_url: "http://127.0.0.1:23119",
      citation_style: "chicago-note-bibliography",
    },
    fields: [
      { key: "base_url", label: "Local API URL", kind: "url" },
      {
        key: "citation_style",
        label: "Citation style",
        kind: "select",
        options: CITATION_STYLES,
      },
    ],
    status: (api) => api.zoteroStatus(),
    unreachable: {
      state: "zotero_down",
      message: "Zotero local API is not reachable.",
    },
  },
  {
    key: "semantic_scholar",
    name: "Semantic Scholar",
    enableLabel: "Enable Semantic Scholar integration",
    defaults: {
      enabled: false,
      base_url: "https://api.semanticscholar.org",
      api_key: null,
    },
    fields: [
      { key: "base_url", label: "API URL", kind: "url" },
      { key: "api_key", label: "API key", kind: "password", nullable: true },
    ],
    status: (api) => api.semanticScholarStatus(),
    unreachable: {
      state: "remote_api_down",
      message: "Semantic Scholar API is not reachable.",
    },
  },
  {
    key: "openalex",
    name: "OpenAlex",
    enableLabel: "Enable OpenAlex integration",
    defaults: {
      enabled: false,
      base_url: "https://api.openalex.org",
      email: null,
    },
    fields: [
      { key: "base_url", label: "API URL", kind: "url" },
      { key: "email", label: "Email", kind: "email", nullable: true },
    ],
    status: (api) => api.openAlexStatus(),
    unreachable: {
      state: "remote_api_down",
      message: "OpenAlex API is not reachable.",
    },
  },
] as const;

/**
 * Whether a status means the provider can be used.
 *
 * One rule for every provider. Zotero previously required exactly `ready`
 * while the two remote providers accepted `rate_limited` as well; the
 * difference was accidental, and it is not a difference Zotero can express —
 * its local API has no rate limit and its client never returns that state.
 */
export function isUsableProviderStatus(status: IntegrationStatus): boolean {
  return status.state === "ready" || status.state === "rate_limited";
}

/** This provider's settings, falling back to its declared defaults. */
export function providerSettings(
  provider: BuiltInProvider,
  integrations: IntegrationsSettings | undefined,
): ProviderSettings {
  return (integrations?.[provider.key] as ProviderSettings) ?? provider.defaults;
}
