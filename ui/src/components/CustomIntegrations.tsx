import { useEffect, useMemo, useState } from "react";
import type {
  CustomIntegrationConfig,
  IntegrationStatus,
  LiteratureProviderInfo,
  ManifestSummary,
  ProbeReport,
  Settings,
} from "../lib/types";
import {
  BUTTON_CLASS,
  CHECKBOX_CLASS,
  ERROR_TEXT_CLASS,
  FIELD_LABEL_CLASS,
  GHOST_BUTTON_CLASS,
  INPUT_CLASS,
} from "../lib/integrations/styles";
import { extendingManifest } from "../lib/integrations/extend";
import type { SearchApi } from "../services/api";
import { CopyButton } from "./CopyButton";

interface CustomIntegrationsProps {
  api: SearchApi;
  settings: Settings;
  onUpdate: (patch: Partial<Settings>) => Promise<void> | void;
  /** Every provider the backend lists, which is where a row learns whether it
   *  can be extended and what there is to take from it. */
  providers?: LiteratureProviderInfo[];
  /** A provider the user asked to extend from its own tab: the editor opens
   *  on a manifest that takes that provider's capabilities. */
  extending?: LiteratureProviderInfo | null;
  /** Sends a message to the chat. Absent when there is no chat to send to,
   *  and then a row offers no "Edit with chat". */
  onSendToChat?: (message: string) => void;
}

/** The longest run of backticks in `text`, plus one, so a fence around it
 *  cannot be closed early by the text itself. */
function fenceFor(text: string): string {
  const longest = Math.max(0, ...(text.match(/`+/g) ?? []).map((run) => run.length));
  return "`".repeat(Math.max(3, longest + 1));
}

/**
 * The message that hands one integration's manifest to the chat.
 *
 * This is the only way an agent sees a stored manifest's text: the MCP tools
 * list integrations without it. The user decides that a manifest is to be
 * changed, and the text reaches the conversation because they sent it.
 */
export function chatEditMessage(
  config: CustomIntegrationConfig,
  name: string,
  change: string,
): string {
  const fence = fenceFor(config.manifest);
  return [
    `Change the custom integration "${name}" (id \`${config.id}\`): ${change}`,
    "",
    "Its current manifest:",
    "",
    fence,
    config.manifest,
    fence,
    "",
    "Read the manifest format with read_custom_integrations (authoring_guide=true), check the change with probe_custom_integration, and save it with save_custom_integration under the same id.",
  ].join("\n");
}

const INITIAL_PROBE_QUERY = "graph neural networks";

const STARTER_MANIFEST = `manifest_version = 2
id = "example-library"
name = "Example library search"

[http]
## Replace this example host with the service you search.
base_url = "https://library.example"

[http.headers]
User-Agent = "Mozilla/5.0"

[capabilities.search]
path = "/search?q={query}"
response_format = "html"
items = "article.result"

[capabilities.search.fields]
id = { path = "a.details", attribute = "href", capture = '''^/items/([0-9a-z]+)$''' }
title = "h2"
authors = { path = "a.author", all = true, then = "join(', ', @)" }
publisher = ".publisher"
language = { path = ".metadata", capture = '''^\\s*([^\\[]+?)\\s*\\[''' }
file_format = { path = ".metadata", capture = '''(?i)\\b(EPUB|PDF|DJVU)\\b''' }
file_size = { path = ".metadata", capture = '''(?i)(\\d+(?:\\.\\d+)?\\s*(?:MB|KB|GB))''' }
landing_page_url = { path = "a.details", attribute = "href", then = "absolute_url(@)" }

[capabilities.resolve_download]
url = "{download_url}"
filename = "{title}.{file_format}"

## One inline table per step. To resolve on a different origin, add
## base_url = "https://downloads.library.example" to the step. Primary-origin
## headers and query parameters are not forwarded across that boundary.
steps = [
  { path = "/api/download?id={id}", response_format = "json", query = { key = { secret = "member_key" } }, fields = { download_url = "download_url" } },
]

## For a DOI-specific provider, a field may carry the search input:
## doi = { input = "query", then = "normalize_doi(@)" }
`;

/**
 * Providers the user describes instead of ones Wilkes compiles.
 *
 * The order of the editor is the order of the decisions: read the manifest and
 * see every origin it will contact, supply whatever secrets it names, run it
 * once against the real service, and switch it on.
 *
 * The probe is evidence, not a gate. A manifest cannot be checked by reading
 * it — a selector is only right about a response that has arrived — so running
 * one is worth doing and its report is shown in full. But a service that is
 * down, rate-limiting, or simply holding nothing for the example query says
 * nothing about whether the manifest is worth keeping, and refusing to save
 * until it answers cleanly made the user's work hostage to someone else's
 * uptime. What is saved is what the user wrote; what the probe found is shown
 * beside it.
 *
 * Renaming is deliberately not part of any of that. A name is a label on this
 * installation's copy of a provider, so it is edited in place on the row and
 * saved on its own, without the manifest being re-read or re-probed.
 */
export default function CustomIntegrations({
  api,
  settings,
  onUpdate,
  providers = [],
  extending = null,
  onSendToChat,
}: CustomIntegrationsProps) {
  const configured = useMemo(
    () => settings.integrations?.custom ?? [],
    [settings.integrations],
  );

  const [draft, setDraft] = useState<string | null>(() =>
    extending
      ? extendingManifest(
          extending,
          configured.map((config) => config.id),
        )
      : null,
  );
  const [editingId, setEditingId] = useState<string | null>(null);
  const [secrets, setSecrets] = useState<Record<string, string>>({});
  const [probeQuery, setProbeQuery] = useState(INITIAL_PROBE_QUERY);
  const [summary, setSummary] = useState<ManifestSummary | null>(null);
  const [report, setReport] = useState<ProbeReport | null>(null);
  const [statuses, setStatuses] = useState<Record<string, IntegrationStatus>>({});
  const [busy, setBusy] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [copyError, setCopyError] = useState<string | null>(null);
  /** The name each manifest declares, by id. Read from the manifest text,
   *  which is a local parse and no request, so that a row can show what it is
   *  called rather than its id — and so a rename box starts from the name
   *  being replaced. */
  const [declaredNames, setDeclaredNames] = useState<Record<string, string>>({});
  /** Which row is being renamed, and to what. */
  const [renaming, setRenaming] = useState<{ id: string; value: string } | null>(null);
  /** Which row's manifest is being handed to the chat, and the change asked
   *  for. */
  const [asking, setAsking] = useState<{ id: string; value: string } | null>(null);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      const named: Record<string, string> = {};
      for (const config of configured) {
        try {
          const summary = await api.customIntegrationSummary(config.manifest);
          if (summary.name) named[config.id] = summary.name;
        } catch (error) {
          // A manifest that cannot be read still has an id to show, and the
          // row's other controls still work. It must not blank the list.
          console.error(`custom integration ${config.id} could not be read:`, error);
        }
      }
      if (!cancelled) setDeclaredNames(named);
    })();
    return () => {
      cancelled = true;
    };
  }, [api, configured]);

  /** What to call a provider: the user's name for it, else the manifest's,
   *  else the id — which is always there and is what every stored selection
   *  and log line names it by. */
  const displayName = (config: CustomIntegrationConfig): string =>
    config.name?.trim() || declaredNames[config.id] || config.id;

  const reportCopyError = (error: unknown) => {
    setCopyError(
      `Could not copy to clipboard: ${
        error instanceof Error ? error.message : String(error)
      }`,
    );
  };

  const copyAuthoringPrompt = async () => {
    setCopyError(null);
    const prompt = await api.customIntegrationAuthoringPrompt();
    await api.writeClipboard(prompt);
  };

  const openEditor = (config?: CustomIntegrationConfig) => {
    setDraft(config?.manifest ?? STARTER_MANIFEST);
    setEditingId(config?.id ?? null);
    setSecrets(config?.secrets ?? {});
    setProbeQuery(INITIAL_PROBE_QUERY);
    setSummary(null);
    setReport(null);
    setSaveError(null);
  };

  /** Opens the editor on a new manifest that takes `base`'s capabilities. The
   *  capabilities themselves stay where they are written: in `base`. */
  const extend = (base: LiteratureProviderInfo) => {
    openEditor();
    setDraft(
      extendingManifest(
        base,
        configured.map((config) => config.id),
      ),
    );
  };

  const extendable = (config: CustomIntegrationConfig) =>
    providers.find(
      (provider) =>
        provider.provider === `custom:${config.id}` &&
        (provider.extendable?.length ?? 0) > 0,
    );

  const closeEditor = () => {
    setDraft(null);
    setEditingId(null);
    setSecrets({});
    setProbeQuery(INITIAL_PROBE_QUERY);
    setSummary(null);
    setReport(null);
    setSaveError(null);
  };

  // Reading a manifest touches nothing: no request, no save. It exists so the
  // origins and the required secrets are known before either.
  const readManifest = async () => {
    if (draft === null) return;
    setBusy(true);
    try {
      const next = await api.customIntegrationSummary(draft);
      setSummary(next);
      // A manifest edited after a probe invalidates that probe's verdict.
      setReport(null);
    } finally {
      setBusy(false);
    }
  };

  const probe = async () => {
    if (draft === null) return;
    setBusy(true);
    try {
      setReport(await api.customIntegrationProbe(draft, secrets, probeQuery));
    } catch (error) {
      setReport({
        id: summary?.id ?? "",
        capability: "search",
        request_url: "",
        raw_response: "",
        results: [],
        issues: [],
        ok: false,
        error: error instanceof Error ? error.message : String(error),
      });
    } finally {
      setBusy(false);
    }
  };

  // Allowing a program is the user's act, apart from importing the manifest
  // that names it: saving a manifest never allows anything. The summary is
  // read again so what it says is allowed is the backend's answer.
  const setProgramAllowed = async (program: string, allowed: boolean) => {
    const current = settings.integrations.allowed_programs ?? [];
    const next = allowed
      ? [...current.filter((name) => name !== program), program]
      : current.filter((name) => name !== program);
    setSaveError(null);
    setBusy(true);
    try {
      await onUpdate({ integrations: { ...settings.integrations, allowed_programs: next } });
      if (draft !== null) setSummary(await api.customIntegrationSummary(draft));
    } catch (error) {
      setSaveError(error instanceof Error ? error.message : String(error));
    } finally {
      setBusy(false);
    }
  };

  const writeCustom = async (next: CustomIntegrationConfig[]) => {
    setSaveError(null);
    try {
      await onUpdate({
        integrations: { ...settings.integrations, custom: next },
      });
      return true;
    } catch (error) {
      // The backend refuses a manifest it could not load back. Surfacing that
      // here is the difference between a rejected save and a silent one.
      setSaveError(error instanceof Error ? error.message : String(error));
      return false;
    }
  };

  const save = async (enabled: boolean) => {
    if (draft === null || !summary || summary.problems.length > 0) return;
    // A rename is a separate act, so editing the manifest must not silently
    // undo one. The name the user gave this provider survives the edit.
    const previous = configured.find(
      (existing) => existing.id === (editingId ?? summary.id),
    );
    const config: CustomIntegrationConfig = {
      id: summary.id,
      name: previous?.name,
      enabled,
      manifest: draft,
      secrets,
    };
    const next = configured.filter(
      (existing) => existing.id !== config.id && existing.id !== editingId,
    );
    if (await writeCustom([...next, config])) closeEditor();
  };

  /** Writes a row's name and nothing else. Blank clears the override, which
   *  restores whatever the manifest calls itself. */
  const commitRename = async () => {
    if (renaming === null) return;
    const name = renaming.value.trim();
    const ok = await writeCustom(
      configured.map((existing) =>
        existing.id === renaming.id
          ? { ...existing, name: name.length > 0 ? name : undefined }
          : existing,
      ),
    );
    if (ok) setRenaming(null);
  };

  const sendToChat = (config: CustomIntegrationConfig) => {
    if (asking === null || !onSendToChat) return;
    const change = asking.value.trim();
    if (!change) return;
    onSendToChat(chatEditMessage(config, displayName(config), change));
    setAsking(null);
  };

  const setEnabled = async (config: CustomIntegrationConfig, enabled: boolean) => {
    await writeCustom(
      configured.map((existing) =>
        existing.id === config.id ? { ...existing, enabled } : existing,
      ),
    );
  };

  const remove = async (config: CustomIntegrationConfig) => {
    await writeCustom(configured.filter((existing) => existing.id !== config.id));
  };

  const checkStatus = async (config: CustomIntegrationConfig) => {
    setBusy(true);
    try {
      const status = await api.literatureProviderStatus(`custom:${config.id}`);
      setStatuses((current) => ({ ...current, [config.id]: status }));
    } catch (error) {
      setStatuses((current) => ({
        ...current,
        [config.id]: {
          id: config.id,
          enabled: config.enabled,
          state: "remote_api_down",
          message: error instanceof Error ? error.message : String(error),
          version: null,
        },
      }));
    } finally {
      setBusy(false);
    }
  };

  const probeIsClean = report?.ok === true;

  return (
    <div className="space-y-3">
      <div className="flex items-start justify-between gap-3">
        <p className="text-xs text-[var(--text-muted)]">
          Copy the prompt to generate a provider manifest from service documentation.
        </p>
        <CopyButton
          copy={copyAuthoringPrompt}
          onCopyError={reportCopyError}
          aria-label="Copy manifest generation prompt"
          copiedAriaLabel="Manifest generation prompt copied"
          copiedChildren="Prompt copied"
          className={`${GHOST_BUTTON_CLASS} shrink-0`}
        >
          Copy generation prompt
        </CopyButton>
      </div>

      {copyError && <p className={ERROR_TEXT_CLASS}>{copyError}</p>}

      {configured.map((config) => (
        <div
          key={config.id}
          className="space-y-2 border border-[var(--border-main)] rounded p-2.5"
        >
          <div className="flex items-center justify-between gap-2">
            {renaming?.id === config.id ? (
              <form
                className="flex flex-1 items-center gap-1.5"
                onSubmit={(e) => {
                  e.preventDefault();
                  void commitRename();
                }}
              >
                <input
                  type="text"
                  autoFocus
                  aria-label={`Name for ${config.id}`}
                  placeholder={declaredNames[config.id] ?? config.id}
                  value={renaming.value}
                  onChange={(e) => setRenaming({ id: config.id, value: e.target.value })}
                  onKeyDown={(e) => e.key === "Escape" && setRenaming(null)}
                  className={INPUT_CLASS}
                />
                <button type="submit" disabled={busy} className={GHOST_BUTTON_CLASS}>
                  Save name
                </button>
                <button
                  type="button"
                  onClick={() => setRenaming(null)}
                  className={GHOST_BUTTON_CLASS}
                >
                  Cancel
                </button>
              </form>
            ) : asking?.id === config.id ? (
              <form
                className="flex flex-1 items-center gap-1.5"
                onSubmit={(e) => {
                  e.preventDefault();
                  sendToChat(config);
                }}
              >
                <input
                  type="text"
                  autoFocus
                  aria-label={`Change to ask for in ${config.id}`}
                  placeholder="What should change?"
                  value={asking.value}
                  onChange={(e) => setAsking({ id: config.id, value: e.target.value })}
                  onKeyDown={(e) => e.key === "Escape" && setAsking(null)}
                  className={INPUT_CLASS}
                />
                <button
                  type="submit"
                  disabled={!asking.value.trim()}
                  className={GHOST_BUTTON_CLASS}
                >
                  Send to chat
                </button>
                <button
                  type="button"
                  onClick={() => setAsking(null)}
                  className={GHOST_BUTTON_CLASS}
                >
                  Cancel
                </button>
              </form>
            ) : (
              <>
                <label className="flex items-center gap-2.5 cursor-pointer group">
                  <input
                    type="checkbox"
                    checked={config.enabled}
                    disabled={busy}
                    onChange={(e) => setEnabled(config, e.target.checked)}
                    className={CHECKBOX_CLASS}
                  />
                  <span className="text-xs text-[var(--text-main)]">
                    {displayName(config)}
                  </span>
                  {/* The id stays visible whatever the row is called: it is
                      what a stored provider filter and every log line name. */}
                  <span className="text-[10px] text-[var(--text-dim)]">
                    custom:{config.id}
                  </span>
                </label>
                <div className="flex items-center gap-1.5">
                  <button
                    type="button"
                    onClick={() => checkStatus(config)}
                    disabled={busy}
                    className={GHOST_BUTTON_CLASS}
                  >
                    Test
                  </button>
                  <button
                    type="button"
                    onClick={() => {
                      setAsking(null);
                      setRenaming({ id: config.id, value: config.name ?? "" });
                    }}
                    className={GHOST_BUTTON_CLASS}
                  >
                    Rename
                  </button>
                  <button
                    type="button"
                    onClick={() => openEditor(config)}
                    className={GHOST_BUTTON_CLASS}
                  >
                    Edit
                  </button>
                  {onSendToChat && (
                    <button
                      type="button"
                      onClick={() => {
                        setRenaming(null);
                        setAsking({ id: config.id, value: "" });
                      }}
                      className={GHOST_BUTTON_CLASS}
                    >
                      Edit with chat
                    </button>
                  )}
                  {extendable(config) && (
                    <button
                      type="button"
                      onClick={() => extend(extendable(config)!)}
                      className={GHOST_BUTTON_CLASS}
                    >
                      Extend
                    </button>
                  )}
                  <button
                    type="button"
                    onClick={() => remove(config)}
                    disabled={busy}
                    className={GHOST_BUTTON_CLASS}
                  >
                    Remove
                  </button>
                </div>
              </>
            )}
          </div>
          {statuses[config.id] && (
            <p className="text-xs text-[var(--text-muted)]">
              {statuses[config.id].message}
            </p>
          )}
        </div>
      ))}

      {saveError && (
        <p className={ERROR_TEXT_CLASS}>{saveError}</p>
      )}

      {draft === null ? (
        <button type="button" onClick={() => openEditor()} className={BUTTON_CLASS}>
          Add integration
        </button>
      ) : (
        <div className="space-y-3 border border-[var(--border-main)] rounded p-2.5">
          <div className="space-y-1">
            <label className={FIELD_LABEL_CLASS} htmlFor="manifest">
              Manifest
            </label>
            <textarea
              id="manifest"
              value={draft}
              spellCheck={false}
              onChange={(e) => {
                setDraft(e.target.value);
                setSummary(null);
                setReport(null);
              }}
              rows={16}
              className={`${INPUT_CLASS} font-mono leading-relaxed`}
            />
          </div>

          <div className="flex items-center gap-2">
            <button
              type="button"
              onClick={readManifest}
              disabled={busy}
              className={BUTTON_CLASS}
            >
              {busy ? "Working" : "Read manifest"}
            </button>
            <CopyButton
              copy={() => api.writeClipboard(draft)}
              onCopyError={reportCopyError}
              aria-label="Copy manifest"
              copiedAriaLabel="Manifest copied"
              copiedChildren="Copied"
              className={GHOST_BUTTON_CLASS}
            >
              Copy manifest
            </CopyButton>
            <button type="button" onClick={closeEditor} className={GHOST_BUTTON_CLASS}>
              Cancel
            </button>
          </div>

          {summary && summary.problems.length > 0 && (
            <ul className="space-y-1">
              {summary.problems.map((problem) => (
                <li
                  key={problem}
                  className={ERROR_TEXT_CLASS}
                >
                  {problem}
                </li>
              ))}
            </ul>
          )}

          {/* A manifest written before version 2 is refused, and what it would
              be as version 2 is on offer — replacing the text is the user's
              act, and nothing is saved by it. */}
          {summary?.upgraded && (
            <button
              type="button"
              onClick={() => {
                setDraft(summary.upgraded ?? null);
                setSummary(null);
                setReport(null);
              }}
              className={BUTTON_CLASS}
            >
              Convert to version 2
            </button>
          )}

          {summary && summary.problems.length === 0 && (
            <div className="space-y-3">
              <dl className="text-xs text-[var(--text-muted)] space-y-1">
                <div className="flex gap-2">
                  <dt className="text-[var(--text-dim)]">Origins</dt>
                  <dd className="text-[var(--text-main)]">
                    {/* A manifest whose every request is a program reaches none. */}
                    {summary.origins.join(", ") || "none"}
                  </dd>
                </div>
                {/* A program is code this machine runs, so it is named as
                    plainly as an origin, and runs only once allowed by name. */}
                {(summary.programs ?? []).map(({ program, allowed }) => (
                  <div key={program} className="flex gap-2">
                    <dt className="text-[var(--text-dim)]">Runs</dt>
                    <dd className="space-y-1">
                      <div className="font-mono text-[var(--text-main)]">{program}</div>
                      {summary.programs_editable ? (
                        <label className="flex items-center gap-2 cursor-pointer">
                          <input
                            type="checkbox"
                            checked={allowed}
                            disabled={busy}
                            aria-label={`Allow running ${program}`}
                            onChange={(e) => setProgramAllowed(program, e.target.checked)}
                            className={CHECKBOX_CLASS}
                          />
                          <span>
                            Allow this program to run on this computer when a result
                            is added. Any integration naming it may then run it.
                          </span>
                        </label>
                      ) : (
                        <div>
                          {allowed
                            ? "Allowed by this server's WILKES_ALLOWED_PROGRAMS."
                            : "Not allowed on this server. Its operator allows programs with WILKES_ALLOWED_PROGRAMS."}
                        </div>
                      )}
                    </dd>
                  </div>
                ))}
                <div className="flex gap-2">
                  <dt className="text-[var(--text-dim)]">Capabilities</dt>
                  <dd>{summary.capabilities.join(", ") || "none"}</dd>
                </div>
                {/* What is another provider's is said apart from what is this
                    manifest's own, and its secrets are not asked for here. */}
                {summary.extends && (
                  <>
                    <div className="flex gap-2">
                      <dt className="text-[var(--text-dim)]">Extends</dt>
                      <dd>
                        {summary.extends.name} ({summary.extends.provider}), taking{" "}
                        {summary.extends.capabilities.join(", ")}. Its origin and
                        secrets are set on {summary.extends.name}.
                      </dd>
                    </div>
                    {(summary.result_fields?.length ?? 0) > 0 && (
                      <div className="flex gap-2">
                        <dt className="text-[var(--text-dim)]">Result fields</dt>
                        <dd className="font-mono">
                          {summary.result_fields?.join(", ")}
                        </dd>
                      </div>
                    )}
                  </>
                )}
              </dl>

              {summary.secrets.map((slot) => (
                <div key={slot.name} className="space-y-1">
                  <label className={FIELD_LABEL_CLASS}>
                    Secret: {slot.name}
                    {slot.optional ? " (optional)" : ""}
                  </label>
                  <input
                    type={slot.kind}
                    aria-label={`Secret ${slot.name}`}
                    value={secrets[slot.name] ?? ""}
                    onChange={(e) =>
                      setSecrets((current) => ({
                        ...current,
                        [slot.name]: e.target.value,
                      }))
                    }
                    className={INPUT_CLASS}
                  />
                </div>
              ))}

              <div className="space-y-1">
                <label className={FIELD_LABEL_CLASS} htmlFor="custom-probe-query">
                  Example query
                </label>
                <input
                  id="custom-probe-query"
                  type="text"
                  value={probeQuery}
                  onChange={(event) => {
                    setProbeQuery(event.target.value);
                    setReport(null);
                  }}
                  className={INPUT_CLASS}
                />
                <p className="text-[10px] text-[var(--text-dim)]">
                  Choose terms that should return at least one representative result.
                </p>
              </div>

              <div className="flex items-center gap-2">
                <button
                  type="button"
                  onClick={probe}
                  disabled={busy || probeQuery.trim().length === 0}
                  className={BUTTON_CLASS}
                >
                  {busy ? "Probing" : "Probe"}
                </button>
                <button
                  type="button"
                  onClick={() => save(false)}
                  disabled={busy}
                  className={GHOST_BUTTON_CLASS}
                >
                  Save disabled
                </button>
                <button
                  type="button"
                  onClick={() => save(true)}
                  disabled={busy}
                  className={BUTTON_CLASS}
                >
                  Save and enable
                </button>
              </div>
              {/* Said, not enforced. A provider that is down or rate-limiting
                  says nothing about whether the manifest is worth keeping, so
                  this is a note about what is known — not a locked button. */}
              {!probeIsClean && (
                <p className="text-[10px] text-[var(--text-dim)]">
                  {report === null
                    ? "Not tested. You can save without testing."
                    : "Test found issues. You can still save."}
                </p>
              )}
            </div>
          )}

          {report && <ProbeReportView report={report} />}
        </div>
      )}
    </div>
  );
}

/**
 * The raw response beside what Wilkes made of it.
 *
 * Both halves are shown because either one alone is unactionable: the results
 * cannot say which key a selector missed, and the response cannot say which
 * field was looking for it.
 */
function ProbeReportView({ report }: { report: ProbeReport }) {
  return (
    <div className="space-y-2 border-t border-[var(--border-main)] pt-2.5">
      {report.error ? (
        <div role="alert" className="space-y-1">
          <p className="text-[10px] uppercase tracking-wider text-[var(--text-dim)]">
            Error
          </p>
          <pre className={`${ERROR_TEXT_CLASS} font-mono whitespace-pre-wrap break-words`}>
            {report.error}
          </pre>
        </div>
      ) : (
        <p
          className={report.ok ? "text-xs text-[var(--text-main)]" : ERROR_TEXT_CLASS}
        >
          {report.ok
            ? `Mapped ${report.results.length} record${report.results.length === 1 ? "" : "s"} with nothing left over.`
            : `Mapped ${report.results.length} record${report.results.length === 1 ? "" : "s"}, with ${report.issues.length} value${report.issues.length === 1 ? "" : "s"} it could not use.`}
        </p>
      )}

      {report.request_url && (
        <div className="space-y-1">
          <p className="text-[10px] uppercase tracking-wider text-[var(--text-dim)]">
            Request URL
          </p>
          <p className="text-[10px] font-mono text-[var(--text-muted)] break-all">
            {report.request_url}
          </p>
        </div>
      )}

      {report.issues.length > 0 && (
        <ul className="space-y-1">
          {report.issues.map((issue, index) => (
            <li
              key={`${issue.record}-${issue.field}-${index}`}
              className={ERROR_TEXT_CLASS}
            >
              record {issue.record}, {issue.field}
              {issue.selector ? ` (${issue.selector})` : ""}: {issue.problem}
            </li>
          ))}
        </ul>
      )}

      {report.results.length > 0 && (
        <div className="overflow-x-auto">
          <table className="text-xs text-[var(--text-muted)] w-full">
            <thead>
              <tr className="text-left text-[var(--text-dim)]">
                <th className="pr-3 font-normal">id</th>
                <th className="pr-3 font-normal">title</th>
                <th className="pr-3 font-normal">year</th>
                <th className="pr-3 font-normal">doi</th>
              </tr>
            </thead>
            <tbody>
              {report.results.map((result) => (
                <tr key={result.id}>
                  <td className="pr-3 font-mono">{result.id}</td>
                  <td className="pr-3 text-[var(--text-main)]">{result.title}</td>
                  <td className="pr-3">{result.year ?? "—"}</td>
                  <td className="pr-3 font-mono">{result.doi ?? "—"}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}

      {report.raw_response && (
        <details>
          <summary className="text-[10px] uppercase tracking-wider text-[var(--text-dim)] cursor-pointer">
            Raw response
          </summary>
          <pre className="mt-1.5 max-h-64 overflow-auto text-[10px] font-mono text-[var(--text-muted)] whitespace-pre-wrap break-all">
            {report.raw_response}
          </pre>
        </details>
      )}
    </div>
  );
}
