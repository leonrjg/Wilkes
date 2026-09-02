import { useState } from "react";
import type { Settings } from "../lib/types";
import { BUILT_IN_PROVIDERS } from "../lib/integrations/providers";
import type { SearchApi } from "../services/api";
import CustomIntegrations from "./CustomIntegrations";
import ProviderForm from "./ProviderForm";

interface IntegrationsPanelProps {
  api: SearchApi;
  settings: Settings;
  onUpdate: (patch: Partial<Settings>) => Promise<void> | void;
}

/** The custom tab is not a provider, so it is not a row in the provider table. */
const CUSTOM_TAB = "custom";

/**
 * The integrations settings, one provider at a time.
 *
 * Previously every provider's form was stacked in one scroll, which made the
 * panel's length the sum of how many providers exist — a shape that only got
 * worse once a user could add their own. Tabs make it the length of one
 * provider instead, and give the manifest editor, which is the tallest thing
 * here by far, somewhere to be without pushing everything else off-screen.
 */
export default function IntegrationsPanel({
  api,
  settings,
  onUpdate,
}: IntegrationsPanelProps) {
  const [active, setActive] = useState<string>(BUILT_IN_PROVIDERS[0].key);
  const custom = settings.integrations?.custom ?? [];

  const tabs = [
    ...BUILT_IN_PROVIDERS.map((provider) => ({
      id: provider.key as string,
      label: provider.name,
      // A provider the user has switched on is marked, so the panel says which
      // ones are live without the user opening each tab to find out.
      on: Boolean(
        (settings.integrations?.[provider.key] as { enabled?: boolean } | undefined)
          ?.enabled,
      ),
    })),
    {
      id: CUSTOM_TAB,
      label: "Custom",
      on: custom.some((config) => config.enabled),
    },
  ];

  const activeProvider = BUILT_IN_PROVIDERS.find(
    (provider) => provider.key === active,
  );

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

      <div role="tabpanel">
        {activeProvider ? (
          <ProviderForm
            key={activeProvider.key}
            provider={activeProvider}
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
