import type { ImageAnalysisSettings, RecognizerCatalogue, RecognizerDescriptor, RecognizerRole } from "./types";

/** Configuration readiness, not a claim that inference has succeeded. Resolve
 * saved choices from the catalogue; browsing another model never changes this. */
export function ocrConfiguration(settings: ImageAnalysisSettings, catalogue: RecognizerCatalogue) {
  const selected = (role: RecognizerRole) => !settings.disabled_roles?.includes(role);
  const page = catalogue.models.find((model) => model.role === "page"
    && model.engine === settings.engine
    && (settings.model != null ? model.model_id === settings.model : model.is_engine_default));
  const formula = catalogue.models.find((model) => model.role === "formula"
    && (settings.formula_model != null ? model.model_id === settings.formula_model : model.is_role_default));
  // The backend's table_model resolves the first table catalogue entry.
  const table = catalogue.models.find((model) => model.role === "table");
  const pageReady = selected("page") && !!page?.is_cached;
  const layoutReady = !!catalogue.detector?.is_installed;
  const invalidFormula = settings.formula_model != null && !formula;
  const validReaders = !invalidFormula && (!selected("page") || pageReady);
  const specialistReady = (model: RecognizerDescriptor | undefined) =>
    !!model?.is_cached && selected(model.role);
  const hasReader = pageReady || specialistReady(formula) || specialistReady(table);
  const includesImages = settings.scope === "typeset_and_embedded";
  const canEnable = validReaders && hasReader && (layoutReady || (includesImages && pageReady));

  const effectiveReader = (role: "formula" | "table") => {
    if (invalidFormula) return "Unavailable formula model";
    if (!validReaders) return "Requires general reader";
    if (!layoutReady) return catalogue.detector ? "Requires layout detection" : "Unavailable in this build";
    const model = role === "formula" ? formula : table;
    if (specialistReady(model)) return model!.display_name;
    // The backend routes remaining regions to the page reader. Emitted kinds
    // describe its supported output, so disclose when that route cannot emit it.
    if (pageReady) return `General reader: ${page!.display_name}${page!.emits.includes(role) ? "" : ` · ${role} output unsupported`}`;
    return "None";
  };

  return {
    page, formula, table, pageReady, layoutReady, validReaders, invalidFormula,
    canEnable, selected, effectiveReader,
    imageReader: invalidFormula ? "Unavailable formula model"
      : !validReaders ? "Requires general reader"
      : !includesImages ? "Excluded"
      : pageReady ? page!.display_name : "Requires general reader",
    status: !settings.enabled ? "Off" : !canEnable ? "Setup required"
      : !layoutReady ? "Partial coverage" : "Ready",
  };
}
