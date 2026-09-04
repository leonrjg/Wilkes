import { useState } from "react";
import type { IntegrationStatus, Settings } from "../lib/types";
import type { BuiltInProvider, ProviderField } from "../lib/integrations/providers";
import {
  isUsableProviderStatus,
  providerSettings,
} from "../lib/integrations/providers";
import {
  BUTTON_CLASS,
  CHECKBOX_CLASS,
  FIELD_LABEL_CLASS,
  INPUT_CLASS,
} from "../lib/integrations/styles";
import type { SearchApi } from "../services/api";

interface ProviderFormProps {
  provider: BuiltInProvider;
  api: SearchApi;
  settings: Settings;
  onUpdate: (patch: Partial<Settings>) => Promise<void> | void;
}

/**
 * One built-in provider, rendered from its row in `BUILT_IN_PROVIDERS`.
 *
 * The three providers this replaces were three near-identical forms with one
 * subtle disagreement between them: Zotero checked its status *before*
 * enabling, while the remote two enabled first and reverted if the check came
 * back unusable. The observable outcome was the same, so this keeps the
 * simpler of the two — check, then enable only if usable — and there is now
 * one place for that rule to live.
 */
export default function ProviderForm({
  provider,
  api,
  settings,
  onUpdate,
}: ProviderFormProps) {
  const config = providerSettings(provider, settings.integrations);
  const values = config as unknown as Record<string, unknown>;
  const [status, setStatus] = useState<IntegrationStatus | null>(null);
  const [testing, setTesting] = useState(false);

  const patch = (changes: Record<string, unknown>) =>
    onUpdate({
      integrations: {
        ...settings.integrations,
        [provider.key]: { ...config, ...changes },
      },
    } as Partial<Settings>);

  const unreachable = (error: unknown): IntegrationStatus => ({
    id: provider.key,
    enabled: false,
    state: provider.unreachable.state,
    message: error instanceof Error ? error.message : provider.unreachable.message,
    version: null,
  });

  const handleEnabledChange = async (enabled: boolean) => {
    // Switching off needs no check: the answer is known, and asking would be a
    // request the user has just said they do not want made.
    if (!enabled) {
      await patch({ enabled: false });
      return;
    }

    setTesting(true);
    try {
      const next = await provider.status(api);
      setStatus(next);
      if (isUsableProviderStatus(next)) await patch({ enabled: true });
    } catch (error) {
      setStatus(unreachable(error));
    } finally {
      setTesting(false);
    }
  };

  const testConnection = async () => {
    setTesting(true);
    try {
      setStatus(await provider.status(api));
    } catch (error) {
      setStatus(unreachable(error));
    } finally {
      setTesting(false);
    }
  };

  return (
    <div className="space-y-3">
      <label className="flex items-center gap-2.5 cursor-pointer group">
        <input
          type="checkbox"
          checked={Boolean(values.enabled)}
          disabled={testing}
          onChange={(e) => handleEnabledChange(e.target.checked)}
          className={CHECKBOX_CLASS}
        />
        <span className="text-xs text-[var(--text-main)] transition-colors">
          {provider.enableLabel}
        </span>
      </label>

      {provider.fields.map((field) => (
        <Field
          key={field.key}
          field={field}
          value={values[field.key]}
          onChange={(value) => patch({ [field.key]: value })}
        />
      ))}

      <div className="flex items-center gap-2">
        <button
          type="button"
          onClick={testConnection}
          disabled={testing}
          className={BUTTON_CLASS}
        >
          {testing ? "Testing" : "Test connection"}
        </button>
        {status && (
          <span className="text-xs text-[var(--text-muted)]">{status.message}</span>
        )}
      </div>
    </div>
  );
}

function Field({
  field,
  value,
  onChange,
}: {
  field: ProviderField;
  value: unknown;
  onChange: (value: string | null) => void;
}) {
  const text = typeof value === "string" ? value : "";
  // A nullable field emptied by the user is `null`, not `""`: the backend's
  // `Option<String>` means "no API key" and "an API key that is the empty
  // string" are different, and only one of them is what an empty box means.
  const emit = (next: string) => onChange(field.nullable ? next || null : next);

  return (
    <div className="space-y-1">
      <label className={FIELD_LABEL_CLASS} htmlFor={`provider-field-${field.key}`}>
        {field.label}
      </label>
      {field.kind === "select" ? (
        <select
          id={`provider-field-${field.key}`}
          value={text}
          onChange={(e) => emit(e.target.value)}
          className={INPUT_CLASS}
        >
          {field.options?.map((option) => (
            <option key={option.id} value={option.id}>
              {option.label}
            </option>
          ))}
        </select>
      ) : (
        <input
          id={`provider-field-${field.key}`}
          type={field.kind}
          value={text}
          onChange={(e) => emit(e.target.value)}
          className={INPUT_CLASS}
        />
      )}
    </div>
  );
}
