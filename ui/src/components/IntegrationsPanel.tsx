import { useState } from "react";
import type {
  IntegrationStatus,
  SemanticScholarSettings,
  Settings,
  ZoteroSettings,
} from "../lib/types";
import type { SearchApi } from "../services/api";

interface IntegrationsPanelProps {
  api: SearchApi;
  settings: Settings;
  onUpdate: (patch: Partial<Settings>) => Promise<void> | void;
}

const DEFAULT_ZOTERO: ZoteroSettings = {
  enabled: false,
  base_url: "http://127.0.0.1:23119",
  citation_style: "chicago-note-bibliography",
};

const DEFAULT_SEMANTIC_SCHOLAR: SemanticScholarSettings = {
  enabled: false,
  base_url: "https://api.semanticscholar.org",
  api_key: null,
};

const CITATION_STYLES = [
  { id: "chicago-note-bibliography", label: "Chicago notes" },
  { id: "apa", label: "APA" },
  { id: "ieee", label: "IEEE" },
  { id: "modern-language-association", label: "MLA" },
];

export default function IntegrationsPanel({ api, settings, onUpdate }: IntegrationsPanelProps) {
  const zotero = settings.integrations?.zotero ?? DEFAULT_ZOTERO;
  const semanticScholar =
    settings.integrations?.semantic_scholar ?? DEFAULT_SEMANTIC_SCHOLAR;
  const [status, setStatus] = useState<IntegrationStatus | null>(null);
  const [semanticScholarStatus, setSemanticScholarStatus] =
    useState<IntegrationStatus | null>(null);
  const [testing, setTesting] = useState(false);
  const [testingSemanticScholar, setTestingSemanticScholar] = useState(false);

  const updateZotero = (patch: Partial<ZoteroSettings>) =>
    onUpdate({
      integrations: {
        ...settings.integrations,
        zotero: {
          ...zotero,
          ...patch,
        },
      },
    });

  const updateSemanticScholar = (patch: Partial<SemanticScholarSettings>) =>
    onUpdate({
      integrations: {
        ...settings.integrations,
        semantic_scholar: {
          ...semanticScholar,
          ...patch,
        },
      },
    });

  const handleEnabledChange = async (enabled: boolean) => {
    if (!enabled) {
      await updateZotero({ enabled: false });
      return;
    }

    setTesting(true);
    try {
      const nextStatus = await api.zoteroStatus();
      setStatus(nextStatus);
      if (nextStatus.state === "ready") {
        await updateZotero({ enabled: true });
      }
    } catch (error) {
      setStatus({
        id: "zotero",
        enabled: false,
        state: "zotero_down",
        message: error instanceof Error ? error.message : "Zotero local API is not reachable.",
        version: null,
      });
    } finally {
      setTesting(false);
    }
  };

  const testConnection = async () => {
    setTesting(true);
    try {
      setStatus(await api.zoteroStatus());
    } finally {
      setTesting(false);
    }
  };

  const handleSemanticScholarEnabledChange = async (enabled: boolean) => {
    if (!enabled) {
      await updateSemanticScholar({ enabled: false });
      return;
    }

    setTestingSemanticScholar(true);
    try {
      await updateSemanticScholar({ enabled: true });
      const nextStatus = await api.semanticScholarStatus();
      setSemanticScholarStatus(nextStatus);
      if (!isUsableSemanticScholarStatus(nextStatus)) {
        await updateSemanticScholar({ enabled: false });
      }
    } catch (error) {
      setSemanticScholarStatus({
        id: "semantic_scholar",
        enabled: false,
        state: "remote_api_down",
        message:
          error instanceof Error
            ? error.message
            : "Semantic Scholar API is not reachable.",
        version: null,
      });
      await updateSemanticScholar({ enabled: false });
    } finally {
      setTestingSemanticScholar(false);
    }
  };

  const testSemanticScholarConnection = async () => {
    setTestingSemanticScholar(true);
    try {
      setSemanticScholarStatus(await api.semanticScholarStatus());
    } finally {
      setTestingSemanticScholar(false);
    }
  };

  return (
    <div className="space-y-4 animate-in fade-in slide-in-from-bottom-2 duration-300 p-1">
      <section className="space-y-3">
        <h3 className="text-[10px] font-medium text-[var(--text-dim)] mb-2.5 uppercase tracking-wider">
          Zotero
        </h3>

        <label className="flex items-center gap-2.5 cursor-pointer group">
          <input
            type="checkbox"
            checked={zotero.enabled}
            disabled={testing}
            onChange={(e) => handleEnabledChange(e.target.checked)}
            className="w-3.5 h-3.5 rounded border-[var(--border-strong)] bg-[var(--bg-input)] text-[var(--accent-blue)] focus:ring-[var(--accent-blue)] focus:ring-offset-[var(--bg-app)]"
          />
          <span className="text-xs text-[var(--text-main)] group-hover:text-[var(--text-main)] transition-colors">
            Enable Zotero integration
          </span>
        </label>

        <div className="space-y-1">
          <label className="text-xs text-[var(--text-muted)]">Local API URL</label>
          <input
            type="url"
            value={zotero.base_url}
            onChange={(e) => updateZotero({ base_url: e.target.value })}
            className="w-full bg-[var(--bg-input)] border border-[var(--border-main)] rounded px-2.5 py-1.5 text-xs text-[var(--text-main)] focus:outline-none focus:border-[var(--accent-blue)] transition-colors"
          />
        </div>

        <div className="space-y-1">
          <label className="text-xs text-[var(--text-muted)]">Citation style</label>
          <select
            value={zotero.citation_style}
            onChange={(e) => updateZotero({ citation_style: e.target.value })}
            className="w-full bg-[var(--bg-input)] border border-[var(--border-main)] rounded px-2.5 py-1.5 text-xs text-[var(--text-main)] focus:outline-none focus:border-[var(--accent-blue)] transition-colors"
          >
            {CITATION_STYLES.map((style) => (
              <option key={style.id} value={style.id}>
                {style.label}
              </option>
            ))}
          </select>
        </div>

        <div className="flex items-center gap-2">
          <button
            type="button"
            onClick={testConnection}
            disabled={testing}
            className="px-3 py-1.5 bg-[var(--accent-blue)] hover:bg-[var(--accent-blue-hover)] text-white text-[10px] font-bold uppercase tracking-wider rounded transition-colors disabled:opacity-50"
          >
            {testing ? "Testing" : "Test connection"}
          </button>
          {status && (
            <span className="text-xs text-[var(--text-muted)]">
              {status.message}
            </span>
          )}
        </div>
      </section>

      <section className="space-y-3 pt-3 border-t border-[var(--border-main)]">
        <h3 className="text-[10px] font-medium text-[var(--text-dim)] mb-2.5 uppercase tracking-wider">
          Semantic Scholar
        </h3>

        <label className="flex items-center gap-2.5 cursor-pointer group">
          <input
            type="checkbox"
            checked={semanticScholar.enabled}
            disabled={testingSemanticScholar}
            onChange={(e) => handleSemanticScholarEnabledChange(e.target.checked)}
            className="w-3.5 h-3.5 rounded border-[var(--border-strong)] bg-[var(--bg-input)] text-[var(--accent-blue)] focus:ring-[var(--accent-blue)] focus:ring-offset-[var(--bg-app)]"
          />
          <span className="text-xs text-[var(--text-main)] group-hover:text-[var(--text-main)] transition-colors">
            Enable Semantic Scholar integration
          </span>
        </label>

        <div className="space-y-1">
          <label className="text-xs text-[var(--text-muted)]">API URL</label>
          <input
            type="url"
            value={semanticScholar.base_url}
            onChange={(e) => updateSemanticScholar({ base_url: e.target.value })}
            className="w-full bg-[var(--bg-input)] border border-[var(--border-main)] rounded px-2.5 py-1.5 text-xs text-[var(--text-main)] focus:outline-none focus:border-[var(--accent-blue)] transition-colors"
          />
        </div>

        <div className="space-y-1">
          <label className="text-xs text-[var(--text-muted)]">API key</label>
          <input
            type="password"
            value={semanticScholar.api_key ?? ""}
            onChange={(e) =>
              updateSemanticScholar({ api_key: e.target.value || null })
            }
            className="w-full bg-[var(--bg-input)] border border-[var(--border-main)] rounded px-2.5 py-1.5 text-xs text-[var(--text-main)] focus:outline-none focus:border-[var(--accent-blue)] transition-colors"
          />
        </div>

        <div className="flex items-center gap-2">
          <button
            type="button"
            onClick={testSemanticScholarConnection}
            disabled={testingSemanticScholar}
            className="px-3 py-1.5 bg-[var(--accent-blue)] hover:bg-[var(--accent-blue-hover)] text-white text-[10px] font-bold uppercase tracking-wider rounded transition-colors disabled:opacity-50"
          >
            {testingSemanticScholar ? "Testing" : "Test connection"}
          </button>
          {semanticScholarStatus && (
            <span className="text-xs text-[var(--text-muted)]">
              {semanticScholarStatus.message}
            </span>
          )}
        </div>
      </section>
    </div>
  );
}

function isUsableSemanticScholarStatus(status: IntegrationStatus): boolean {
  return status.state === "ready" || status.state === "rate_limited";
}
