import { useEffect, useMemo, useState } from "react";
import type { LiteratureProviderInfo, Settings } from "../lib/types";
import { ZOTERO_ROW, literatureRow } from "../lib/integrations/providers";
import { ERROR_TEXT_CLASS } from "../lib/integrations/styles";
import type { SearchApi } from "../services/api";
import CustomIntegrations from "./CustomIntegrations";
import ProviderForm from "./ProviderForm";

interface IntegrationsPanelProps {
  api: SearchApi;
  settings: Settings;
  onUpdate: (patch: Partial<Settings>) => Promise<void> | void;
}

/** The custom tab is not a provider, so it is not a row. */
const CUSTOM_TAB = "custom";

/**
 * The integrations settings, one provider at a time.
 *
 * Previously every provider's form was stacked in one scroll, which made the
 * panel's length the sum of how many providers exist — a shape that only got
 * worse once a user could add their own. Tabs make it the length of one
 * provider instead, and give the manifest editor, which is the tallest thing
 * here by far, somewhere to be without pushing everything else off-screen.
 *
 * Zotero is a row written out; every provider shipped with Wilkes is a row
 * built from what the backend reports of it, so the panel names no provider
 * but Zotero.
 */
export default function IntegrationsPanel({
  api,
  settings,
  onUpdate,
}: IntegrationsPanelProps) {
  const [active, setActive] = useState<string>(ZOTERO_ROW.key);
  const [shipped, setShipped] = useState<LiteratureProviderInfo[]>([]);
  const [listError, setListError] = useState<string | null>(null);
  const custom = settings.integrations?.custom ?? [];

  useEffect(() => {
    let cancelled = false;
    api
      .literatureProviders()
      .then((providers) => {
        if (cancelled) return;
        setShipped(providers.filter((provider) => provider.builtin));
        setListError(null);
      })
      .catch((error: unknown) => {
        console.error("literature providers could not be listed:", error);
        if (!cancelled) {
          setListError(error instanceof Error ? error.message : String(error));
        }
      });
    return () => {
      cancelled = true;
    };
  }, [api]);

  const rows = useMemo(
    () => [ZOTERO_ROW, ...shipped.map((provider) => literatureRow(provider))],
    [shipped],
  );

  const tabs = [
    ...rows.map((row) => ({
      id: row.key,
      label: row.name,
      // A provider the user has switched on is marked, so the panel says which
      // ones are live without the user opening each tab to find out.
      on: Boolean(row.read(settings.integrations).enabled),
    })),
    {
      id: CUSTOM_TAB,
      label: "Custom",
      on: custom.some((config) => config.enabled),
    },
  ];

  const activeRow = rows.find((row) => row.key === active);

  return (
    <div className="space-y-4 animate-in fade-in slide-in-from-bottom-2 duration-300 p-1">
      <div
        role="tablist"
        aria-label="Integrations"
        className="flex items-center gap-1 border-b border-[var(--border-main)]"
      >
        {tabs.map((tab) => (
          <button
            key={tab.id}
            type="button"
            role="tab"
            aria-selected={active === tab.id}
            onClick={() => setActive(tab.id)}
            className={`flex items-center gap-1.5 px-3 py-1.5 -mb-px text-xs border-b-2 transition-colors ${
              active === tab.id
                ? "border-[var(--accent-blue)] text-[var(--text-main)] font-medium"
                : "border-transparent text-[var(--text-muted)] hover:text-[var(--text-main)]"
            }`}
          >
            {tab.label}
            {tab.on && (
              <span
                aria-label="enabled"
                className="w-1.5 h-1.5 rounded-full bg-[var(--accent-blue)]"
              />
            )}
          </button>
        ))}
      </div>

      {listError !== null && (
        <p className={ERROR_TEXT_CLASS}>
          The literature providers could not be listed: {listError}
        </p>
      )}

      <div role="tabpanel">
        {activeRow ? (
          <ProviderForm
            key={activeRow.key}
            provider={activeRow}
            api={api}
            settings={settings}
            onUpdate={onUpdate}
          />
        ) : (
          <CustomIntegrations api={api} settings={settings} onUpdate={onUpdate} />
        )}
      </div>
    </div>
  );
}
