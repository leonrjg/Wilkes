import { useMemo, useState } from "react";
import type {
  CustomIntegrationConfig,
  IntegrationStatus,
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
import type { SearchApi } from "../services/api";
import { CopyButton } from "./CopyButton";

interface CustomIntegrationsProps {
  api: SearchApi;
  settings: Settings;
  onUpdate: (patch: Partial<Settings>) => Promise<void> | void;
}

const INITIAL_PROBE_QUERY = "graph neural networks";

const STARTER_MANIFEST = `manifest_version = 1
id = "anna-journals"
name = "Anna journal search"

[http]
## Replace this example host with the Anna mirror you use.
base_url = "https://annas-archive.example"

[[http.params]]
location = "header"
name = "User-Agent"
value = "Mozilla/5.0"

[capabilities.search]
path = "/search?q={query}&content=journal"
response_format = "html"
items = '''div:has(> a[href^="/md5/"][class="custom-a block mr-2 sm:mr-4 hover:opacity-80"])'''

[capabilities.search.fields]
id = { path = '''a[href^="/md5/"][class="custom-a block mr-2 sm:mr-4 hover:opacity-80"]''', attribute = "href", capture = '''^/md5/([0-9a-f]+)$''' }
title = '''div.max-w-full a[href^="/md5/"]'''
authors = { path = '''a[href^="/search"]:has(span[class="icon-[mdi--user-edit]"])''', coerce = "join" }
publisher = '''a[href^="/search"]:has(span[class="icon-[mdi--company]"])'''
language = { path = "div.text-gray-800", capture = '''^\\s*✅?\\s*([^\\[]+?)\\s*\\[''' }
file_format = { path = "div.text-gray-800", capture = '''(?i)\\b(EPUB|PDF|MOBI|AZW3|AZW|DJVU|CBZ|CBR|FB2|DOCX?|TXT)\\b''' }
file_size = { path = "div.text-gray-800", capture = '''(?i)(\\d+(?:\\.\\d+)?\\s*(?:MB|KB|GB|TB))''' }
landing_page_url = { path = '''a[href^="/md5/"][class="custom-a block mr-2 sm:mr-4 hover:opacity-80"]''', attribute = "href", coerce = "absolute_url" }

[capabilities.resolve_download]
url = "{download_url}"
filename = "{title}.{file_format}"

[[capabilities.resolve_download.steps]]
path = "/dyn/api/fast_download.json?md5={id}"
response_format = "json"

[[capabilities.resolve_download.steps.params]]
location = "query"
name = "key"
secret = "anna_key"

[capabilities.resolve_download.steps.fields]
download_url = "download_url"

## For a DOI-specific provider, a field may carry the search input:
## doi = { input = "query", coerce = "normalize_doi" }
`;

/**
 * Providers the user describes instead of ones Wilkes compiles.
 *
 * The order of the editor is the order of the decisions: read the manifest and
 * see which host it will contact, supply whatever secrets it names, run it once
 * against the real service, and only then switch it on. Enabling is gated on
 * that run — a manifest cannot be checked by reading it, because a selector is
 * only right about a response that has arrived.
 */
export default function CustomIntegrations({
  api,
  settings,
  onUpdate,
}: CustomIntegrationsProps) {
  const configured = useMemo(
    () => settings.integrations?.custom ?? [],
    [settings.integrations],
  );

  const [draft, setDraft] = useState<string | null>(null);
  const [editingId, setEditingId] = useState<string | null>(null);
  const [secrets, setSecrets] = useState<Record<string, string>>({});
  const [probeQuery, setProbeQuery] = useState(INITIAL_PROBE_QUERY);
  const [summary, setSummary] = useState<ManifestSummary | null>(null);
  const [report, setReport] = useState<ProbeReport | null>(null);
  const [statuses, setStatuses] = useState<Record<string, IntegrationStatus>>({});
  const [busy, setBusy] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [copyError, setCopyError] = useState<string | null>(null);

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
  // host and the required secrets are known before either.
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
    const config: CustomIntegrationConfig = {
      id: summary.id,
      enabled,
      manifest: draft,
      secrets,
    };
    const next = configured.filter(
      (existing) => existing.id !== config.id && existing.id !== editingId,
    );
    if (await writeCustom([...next, config])) closeEditor();
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
      const status = await api.customIntegrationStatus(config.id);
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
          Ask an LLM to translate service documentation or an existing script
          into a manifest using the exact schema this build accepts.
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
            <label className="flex items-center gap-2.5 cursor-pointer group">
              <input
                type="checkbox"
                checked={config.enabled}
                disabled={busy}
                onChange={(e) => setEnabled(config, e.target.checked)}
                className={CHECKBOX_CLASS}
              />
              <span className="text-xs text-[var(--text-main)]">{config.id}</span>
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
                onClick={() => openEditor(config)}
                className={GHOST_BUTTON_CLASS}
              >
                Edit
              </button>
              <button
                type="button"
                onClick={() => remove(config)}
                disabled={busy}
                className={GHOST_BUTTON_CLASS}
              >
                Remove
              </button>
            </div>
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

          {summary && summary.problems.length === 0 && (
            <div className="space-y-3">
              <dl className="text-xs text-[var(--text-muted)] space-y-1">
                <div className="flex gap-2">
                  <dt className="text-[var(--text-dim)]">Contacts</dt>
                  <dd className="text-[var(--text-main)]">{summary.host}</dd>
                </div>
                <div className="flex gap-2">
                  <dt className="text-[var(--text-dim)]">Capabilities</dt>
                  <dd>{summary.capabilities.join(", ") || "none"}</dd>
                </div>
              </dl>

              {summary.required_secrets.map((name) => (
                <div key={name} className="space-y-1">
                  <label className={FIELD_LABEL_CLASS}>Secret: {name}</label>
                  <input
                    type="password"
                    aria-label={`Secret ${name}`}
                    value={secrets[name] ?? ""}
                    onChange={(e) =>
                      setSecrets((current) => ({
                        ...current,
                        [name]: e.target.value,
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
                  disabled={busy || !probeIsClean}
                  title={
                    probeIsClean
                      ? undefined
                      : "Probe this manifest against the service before enabling it"
                  }
                  className={BUTTON_CLASS}
                >
                  Save and enable
                </button>
              </div>
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
