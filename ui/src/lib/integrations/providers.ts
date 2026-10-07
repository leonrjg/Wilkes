import type {
  BuiltinProviderState,
  IntegrationState,
  IntegrationStatus,
  IntegrationsSettings,
  LiteratureProviderInfo,
  ZoteroSettings,
} from "../types";
import type { SearchApi } from "../../services/api";

/**
 * The providers a settings tab can configure, described rather than written
 * out.
 *
 * `custom-integrations.md` §8: fixing the Rust duplication while leaving one
 * hand-written form per provider in the UI would move the cost, not remove it.
 * A provider is a *row* — its fields, how to read and write them, how to check
 * it, what to say when it cannot be reached — and `ProviderForm` renders any
 * row.
 *
 * Zotero's row is written here, because Zotero is a local library with a
 * settings object of its own. Every literature provider's row is **built from
 * what the backend says about it** ([`literatureRow`]): its name, its origin,
 * and the secrets its manifest names (an email address, an API key) all come
 * from the manifest, so a provider added tomorrow is a tab with no change to
 * this file. The manifest-defined providers a user writes themselves are
 * edited in `CustomIntegrations`, because their fields are not known until the
 * user writes them.
 */

export type ProviderFieldKind = "url" | "text" | "email" | "password" | "select";

export interface ProviderField {
  /** Key within the row's values. */
  key: string;
  label: string;
  kind: ProviderFieldKind;
  options?: readonly { readonly id: string; readonly label: string }[];
  /** Shown in an empty field. For an origin, the one used when it is empty. */
  placeholder?: string;
  /**
   * Store an emptied field as `null` rather than `""`.
   *
   * True for the optional ones — an API key, a contact address — where "not
   * set" and "set to nothing" are different, and a `""` would be sent as a
   * header on every request.
   */
  nullable?: boolean;
}

export interface ProviderRow {
  /** The tab's id: `zotero`, or a literature provider's id. */
  key: string;
  /** Tab label. */
  name: string;
  /**
   * Label on the enable checkbox. Spelled out per provider rather than derived
   * from `name`: it is what a screen reader announces and what the tests click.
   */
  enableLabel: string;
  fields: readonly ProviderField[];
  /** The values the form shows, read from settings. Always carries `enabled`. */
  read: (integrations: IntegrationsSettings | undefined) => Record<string, unknown>;
  /** Settings after changing some of those values. */
  write: (
    integrations: IntegrationsSettings | undefined,
    changes: Record<string, unknown>,
  ) => IntegrationsSettings;
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

const ZOTERO_DEFAULTS: ZoteroSettings = {
  enabled: false,
  base_url: "http://127.0.0.1:23119",
  citation_style: "chicago-note-bibliography",
};

export const ZOTERO_ROW: ProviderRow = {
  key: "zotero",
  name: "Zotero",
  enableLabel: "Enable Zotero integration",
  fields: [
    { key: "base_url", label: "Local API URL", kind: "url" },
    {
      key: "citation_style",
      label: "Citation style",
      kind: "select",
      options: CITATION_STYLES,
    },
  ],
  read: (integrations) =>
    (integrations?.zotero ?? ZOTERO_DEFAULTS) as unknown as Record<string, unknown>,
  write: (integrations, changes) => ({
    ...(integrations ?? { zotero: ZOTERO_DEFAULTS }),
    zotero: { ...(integrations?.zotero ?? ZOTERO_DEFAULTS), ...changes },
  }),
  status: (api) => api.zoteroStatus(),
  unreachable: {
    state: "zotero_down",
    message: "Zotero local API is not reachable.",
  },
};

/**
 * The row for one literature provider, from what the backend reports of it.
 *
 * Its values live in `integrations.builtin[id]`: whether it is on, a base URL
 * when the user has pointed it somewhere else, and one entry per secret its
 * manifest names. The form edits those as flat fields; this is where they are
 * folded back into that shape.
 */
export function literatureRow(info: LiteratureProviderInfo): ProviderRow {
  const shippedOrigin = info.default_base_url ?? info.base_url;
  const secretNames = new Set(info.secrets.map((slot) => slot.name));
  return {
    key: info.provider,
    name: info.name,
    enableLabel: `Enable ${info.name} integration`,
    fields: [
      { key: "base_url", label: "API URL", kind: "url", placeholder: shippedOrigin },
      ...info.secrets.map((slot) => ({
        key: slot.name,
        label: slot.label,
        kind: slot.kind,
        // Optional or not, an emptied secret is "not set", never "set to
        // nothing": a blank value is not a credential.
        nullable: true,
      })),
    ],
    read: (integrations) => {
      const state = integrations?.builtin?.[info.provider];
      return {
        enabled: state?.enabled ?? false,
        base_url: state?.base_url ?? shippedOrigin,
        ...(state?.secrets ?? {}),
      };
    },
    write: (integrations, changes) => {
      const current: BuiltinProviderState = integrations?.builtin?.[info.provider] ?? {
        enabled: false,
      };
      const next: BuiltinProviderState = { ...current };
      const secrets: Record<string, string> = { ...(current.secrets ?? {}) };
      for (const [key, value] of Object.entries(changes)) {
        if (key === "enabled") {
          next.enabled = Boolean(value);
        } else if (key === "base_url") {
          // The shipped origin is no override. Anything else is kept as typed,
          // including empty, which the backend reads as no override too: a
          // field the user is retyping must not snap back to the default
          // between keystrokes.
          if (typeof value !== "string" || value === shippedOrigin) {
            delete next.base_url;
          } else {
            next.base_url = value;
          }
        } else if (secretNames.has(key)) {
          if (typeof value === "string" && value.length > 0) secrets[key] = value;
          else delete secrets[key];
        }
      }
      if (Object.keys(secrets).length > 0) next.secrets = secrets;
      else delete next.secrets;
      return {
        ...(integrations ?? { zotero: ZOTERO_DEFAULTS }),
        builtin: { ...(integrations?.builtin ?? {}), [info.provider]: next },
      };
    },
    status: (api) => api.literatureProviderStatus(info.provider),
    unreachable: {
      state: "remote_api_down",
      message: `${info.name} is not reachable.`,
    },
  };
}

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
