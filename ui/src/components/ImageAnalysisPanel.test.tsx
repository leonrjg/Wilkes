import { useState } from "react";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { RecognizerCatalogue, Settings } from "../lib/types";
import ImageAnalysisPanel from "./ImageAnalysisPanel";

const SETTINGS = {
  generation: { ollama_url: "http://127.0.0.1:11434" },
  image_analysis: {
    enabled: false,
    engine: "Onnx",
    model: null,
    device: null,
    describer_model: "",
    scope: "typeset_only",
  },
} as unknown as Settings;

const GRANITE = {
  engine: "Onnx" as const,
  model_id: "ibm-granite/granite-docling-258M",
  role: "page" as const,
  display_name: "Granite-Docling 258M",
  description: "Reads a page in one pass.",
  is_default: true,
  is_engine_default: true,
  is_role_default: true,
  is_cached: false,
  footprint_bytes: 560_000_000,
  admission_threshold: 0.4,
  emits: ["text", "formula", "table"],
};

const PADDLE = {
  engine: "Candle" as const,
  model_id: "paddleocr-vl-1.6",
  role: "page" as const,
  display_name: "PaddleOCR-VL paddleocr-vl-1.6",
  description: "Transcribes with per-region geometry.",
  is_default: false,
  is_engine_default: true,
  is_role_default: true,
  is_cached: false,
  footprint_bytes: 1_928_447_087,
  admission_threshold: 0.6,
  emits: ["text"],
};

const DETECTOR = {
  inventory: {
    name: "PP-DocLayoutV2",
    repo: "alex-dinh/PP-DocLayoutV2-ONNX",
    revision: "5e30a2650d087e23af3a8084d42bd30d135af771",
    license: "Apache-2.0",
    license_url: "https://huggingface.co/PaddlePaddle/PP-DocLayoutV2",
    derived_from: ["PP-DocLayoutV2 (Apache-2.0, PaddlePaddle)"],
    artifacts: [],
    footprint_bytes: 213_968_247,
  },
  is_installed: false,
};

/// A row of the catalogue like the page readers, distinguished by its role.
/// It used to be a field of its own on the catalogue, which made a second
/// formula model a second field rather than a second row.
const TEXIFY = {
  engine: "Onnx" as const,
  model_id: "texify",
  role: "formula" as const,
  display_name: "Texify",
  description: "Reads one cropped expression back as LaTeX.",
  is_default: false,
  is_engine_default: false,
  is_role_default: true,
  is_cached: false,
  footprint_bytes: 320_847_936,
  admission_threshold: 0,
  emits: ["formula"],
};

/// A second row for the same role, and the reason `is_role_default` exists:
/// which model reads formulas is now a choice, so "the formula reader" can no
/// longer be "the one formula row" the catalogue happens to list first.
const PP_FORMULANET = {
  ...TEXIFY,
  model_id: "pp-formulanet-plus-s",
  display_name: "PP-FormulaNet_plus-S",
  description: "Reads cropped formulas as LaTeX using PP-FormulaNet_plus-S.",
  is_role_default: false,
};

/// The third role. It transcribes nothing at all — it answers the grid and the
/// cells are filled from the page's own glyphs — which is why it can no more be
/// the page recognizer than the formula reader can.
const SLANET = {
  engine: "Onnx" as const,
  model_id: "slanet-plus",
  role: "table" as const,
  display_name: "SLANet-plus",
  description: "Reads the grid of a ruled table the page typesets.",
  is_default: false,
  is_engine_default: false,
  is_role_default: true,
  is_cached: false,
  footprint_bytes: 7_758_305,
  admission_threshold: 0,
  emits: ["table"],
};

const CATALOGUE: RecognizerCatalogue = {
  engines: ["Onnx", "Candle"],
  models: [GRANITE, PADDLE, TEXIFY, SLANET],
  detector: DETECTOR,
};

const INVENTORY = {
  name: "paddleocr-vl-1.6",
  repo: "PaddlePaddle/PaddleOCR-VL-1.6",
  revision: "c5630abae1d940eafe0697512a0325494b02ab42",
  license: "Apache-2.0",
  license_url: "https://huggingface.co/PaddlePaddle/PaddleOCR-VL-1.6",
  derived_from: ["NaViT-style dynamic-resolution vision encoder"],
  artifacts: [{ filename: "model.safetensors", size_bytes: 1_917_255_968, sha256: "85a4".repeat(16) }],
  footprint_bytes: 1_928_447_087,
};

const READING = {
  ...SETTINGS,
  image_analysis: { ...SETTINGS.image_analysis, enabled: true, model: GRANITE.model_id, disabled_roles: [] },
} as unknown as Settings;

const configured = (patch: Partial<Settings["image_analysis"]> = {}, enabled = true): Settings => ({
  ...READING, image_analysis: { ...READING.image_analysis, enabled, ...patch },
});
const installed = (ids = [GRANITE.model_id, TEXIFY.model_id, SLANET.model_id], layout = true): RecognizerCatalogue => ({
  ...CATALOGUE,
  models: CATALOGUE.models.map((model) => ({ ...model, is_cached: ids.includes(model.model_id) })),
  detector: { ...DETECTOR, is_installed: layout },
});

describe("OCR configuration", () => {
  let catalogue: RecognizerCatalogue;
  const listeners: Record<string, (...args: any[]) => void> = {};
  const api = {
    imageRecognizerCatalogue: vi.fn(async () => catalogue),
    imageRecognizerInventory: vi.fn(async () => INVENTORY),
    installLayoutDetector: vi.fn(async () => {}),
    installImageRecognizer: vi.fn(async (_engine: string, _id: string) => {}),
    onImageAnalysisProgress: vi.fn(async (callback: (...args: any[]) => void) => { listeners.progress = callback; return vi.fn(); }),
    onImageAnalysisDone: vi.fn(async (callback: (...args: any[]) => void) => { listeners.done = callback; return vi.fn(); }),
    onImageAnalysisError: vi.fn(async (callback: (...args: any[]) => void) => { listeners.error = callback; return vi.fn(); }),
  };
  beforeEach(() => {
    vi.clearAllMocks();
    catalogue = installed();
    api.imageRecognizerCatalogue.mockImplementation(async () => catalogue);
    api.imageRecognizerInventory.mockImplementation(async () => INVENTORY);
    api.installImageRecognizer.mockImplementation(async (_engine, id) => {
      catalogue = { ...catalogue, models: catalogue.models.map((model) => model.model_id === id ? { ...model, is_cached: true } : model) };
      listeners.done?.();
    });
    api.installLayoutDetector.mockImplementation(async () => {
      catalogue = { ...catalogue, detector: { ...DETECTOR, is_installed: true } };
      listeners.done?.();
    });
  });
  const panel = (initial = READING, update = vi.fn()) => {
    function Harness() {
      const [settings, setSettings] = useState(initial);
      return <ImageAnalysisPanel api={api as never} settings={settings}
        onUpdateSettings={async (patch) => { update(patch); setSettings((previous) => ({ ...previous, ...patch })); }} />;
    }
    render(<Harness />);
    return update;
  };
  const region = (name: string) => within(screen.getByRole("region", { name }));
  const loaded = () => screen.findByRole("checkbox", { name: "Enable OCR" });
  const clickReady = async (name: string) => {
    const action = await screen.findByRole("button", { name });
    await waitFor(() => expect(action).toBeEnabled());
    fireEvent.click(action);
  };

  it("blocks typeset coverage without layout even with all readers installed, and permits disabling", async () => {
    catalogue = installed(undefined, false);
    panel();
    expect(await loaded()).toBeEnabled();
    expect(region("Formulas").getByText("Effective reader: Requires layout detection")).toBeInTheDocument();
    expect(region("Tables").getByText("Effective reader: Requires layout detection")).toBeInTheDocument();
    expect(screen.getAllByText("Setup required").length).toBeGreaterThan(0);
  });

  it("cannot enable default scope with no layout", async () => {
    catalogue = installed(undefined, false);
    panel(configured({}, false));
    expect(await loaded()).toBeDisabled();
  });

  it("allows specialist-only OCR without an installed general reader", async () => {
    catalogue = installed([TEXIFY.model_id, SLANET.model_id]);
    const update = panel(configured({ disabled_roles: ["page"] }, false));
    const toggle = await loaded();
    expect(toggle).toBeEnabled();
    fireEvent.click(toggle);
    await waitFor(() => expect(update).toHaveBeenCalledWith({ image_analysis: expect.objectContaining({ enabled: true, disabled_roles: ["page"] }) }));
    expect(region("Formulas").getByText(`Effective reader: ${TEXIFY.display_name}`)).toBeInTheDocument();
    expect(region("Tables").getByText(`Effective reader: ${SLANET.display_name}`)).toBeInTheDocument();
    expect(screen.getByRole("checkbox", { name: "Include embedded images" })).toBeDisabled();
  });

  it("shows general routing for missing specialists", async () => {
    catalogue = installed([GRANITE.model_id]);
    panel(); await loaded();
    expect(region("Formulas").getByText(`Effective reader: General reader: ${GRANITE.display_name}`)).toBeInTheDocument();
    expect(region("Tables").getByText(`Effective reader: General reader: ${GRANITE.display_name}`)).toBeInTheDocument();
  });

  it("discloses unsupported general output", async () => {
    catalogue = installed([PADDLE.model_id]);
    panel(configured({ engine: "Candle", model: PADDLE.model_id })); await loaded();
    expect(region("Formulas").getByText(/formula output unsupported/)).toBeInTheDocument();
  });

  it("de-selects specialists without uninstalling and shows the effective general reader", async () => {
    const update = panel(); await loaded();
    fireEvent.click(screen.getByRole("checkbox", { name: "Use dedicated table reader" }));
    await waitFor(() => expect(update).toHaveBeenCalledWith({ image_analysis: expect.objectContaining({ disabled_roles: ["table"] }) }));
    expect(region("Tables").getByText(`Effective reader: General reader: ${GRANITE.display_name}`)).toBeInTheDocument();
    expect(api.installImageRecognizer).not.toHaveBeenCalled();
  });

  it("switches off general OCR while specialists remain active", async () => {
    const update = panel(); await loaded();
    fireEvent.click(screen.getByRole("checkbox", { name: "Use general reader" }));
    await waitFor(() => expect(update).toHaveBeenCalledWith({ image_analysis: expect.objectContaining({ enabled: true, disabled_roles: ["page"] }) }));
    expect(region("Formulas").getByText(`Effective reader: ${TEXIFY.display_name}`)).toBeInTheDocument();
  });

  it("refuses to switch off the last usable reader while OCR is on", async () => {
    catalogue = installed([GRANITE.model_id]);
    const update = panel(); await loaded();
    fireEvent.click(screen.getByRole("checkbox", { name: "Use general reader" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Complete OCR setup");
    expect(update).not.toHaveBeenCalled();
  });

  it("shows None for uncovered content in specialist-only mode", async () => {
    catalogue = installed([SLANET.model_id]);
    panel(configured({ disabled_roles: ["page"] })); await loaded();
    expect(region("Formulas").getByText("Effective reader: None")).toBeInTheDocument();
  });

  it("allows embedded images without layout and labels partial coverage", async () => {
    catalogue = installed([GRANITE.model_id], false);
    const update = panel(configured({}, false)); await loaded();
    fireEvent.click(screen.getByRole("checkbox", { name: "Include embedded images" }));
    await waitFor(() => expect(update).toHaveBeenCalledWith({ image_analysis: expect.objectContaining({ scope: "typeset_and_embedded" }) }));
    fireEvent.click(screen.getByRole("checkbox", { name: "Enable OCR" }));
    await screen.findByText("Partial coverage");
    expect(region("Embedded images").getByText(`Effective reader: ${GRANITE.display_name}`)).toBeInTheDocument();
    expect(region("Tables").getByText("Effective reader: Requires layout detection")).toBeInTheDocument();
  });

  it("names a build without layout and permits its embedded-image path", async () => {
    catalogue = { ...installed([GRANITE.model_id]), detector: null };
    panel(configured({ scope: "typeset_and_embedded" })); await loaded();
    expect(region("Tables").getByText("Effective reader: Unavailable in this build")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Install layout detection" })).not.toBeInTheDocument();
    expect(screen.getByText("Partial coverage")).toBeInTheDocument();
  });

  it("holds saved coverage until the selected general reader is installed and applied", async () => {
    const update = panel(); await loaded();
    fireEvent.change(screen.getByRole("combobox", { name: "General reader model" }), { target: { value: `Candle:${PADDLE.model_id}` } });
    expect(region("General reader").getByText(`Saved reader: ${GRANITE.display_name}`)).toBeInTheDocument();
    expect(update).not.toHaveBeenCalled();
    await clickReady("Install general reader and use it");
    await waitFor(() => expect(update).toHaveBeenCalledWith({ image_analysis: expect.objectContaining({ engine: "Candle", model: PADDLE.model_id }) }));
    expect(api.installImageRecognizer).toHaveBeenCalledWith("Candle", PADDLE.model_id);
    expect(region("General reader").getByText(`Saved reader: ${PADDLE.display_name}`)).toBeInTheDocument();
  });

  it("holds the formula default against catalogue order and installs before applying", async () => {
    catalogue = { ...catalogue, models: [PP_FORMULANET, ...catalogue.models] };
    const update = panel(); await loaded();
    const selector = screen.getByRole("combobox", { name: "Formula model" });
    expect(selector).toHaveValue(TEXIFY.model_id);
    fireEvent.change(selector, { target: { value: PP_FORMULANET.model_id } });
    expect(region("Formulas").getByText(`Effective reader: ${TEXIFY.display_name}`)).toBeInTheDocument();
    expect(update).not.toHaveBeenCalled();
    await clickReady("Install formula reader and use it");
    await waitFor(() => expect(update).toHaveBeenCalledWith({ image_analysis: expect.objectContaining({ formula_model: PP_FORMULANET.model_id }) }));
    expect(api.installImageRecognizer).toHaveBeenCalledWith("Onnx", PP_FORMULANET.model_id);
  });

  it("installs missing layout and formula together without changing role selection", async () => {
    catalogue = CATALOGUE;
    const update = panel(configured({ disabled_roles: ["page", "formula"] }, false));
    await clickReady("Install required models for formula");
    await waitFor(() => expect(api.installLayoutDetector).toHaveBeenCalledOnce());
    expect(api.installImageRecognizer).toHaveBeenCalledWith("Onnx", TEXIFY.model_id);
    expect(update).not.toHaveBeenCalled();
    expect(screen.getByRole("checkbox", { name: "Use dedicated formula reader" })).not.toBeChecked();
  });

  it("installs tables through the recognizer API", async () => {
    catalogue = installed([GRANITE.model_id]); panel();
    await clickReady("Install table reader");
    await waitFor(() => expect(api.installImageRecognizer).toHaveBeenCalledWith("Onnx", SLANET.model_id));
  });

  it("discloses licenses, provenance, and combined size before downloading", async () => {
    catalogue = CATALOGUE; panel(SETTINGS);
    const action = await screen.findByRole("button", { name: "Install required models for formula" });
    await waitFor(() => expect(action).toBeEnabled());
    expect(region("Formulas").getByRole("link", { name: INVENTORY.license })).toHaveAttribute("href", INVENTORY.license_url);
    expect(screen.getByRole("region", { name: "Page regions" })).toHaveTextContent(DETECTOR.inventory.name);
    expect(region("Formulas").getByText("510 MB download")).toBeInTheDocument();
    expect(region("Formulas").getByText(INVENTORY.revision)).toBeInTheDocument();
  });

  it("does not apply a model when installation fails", async () => {
    api.installImageRecognizer.mockRejectedValueOnce(new Error("Download failed"));
    const update = panel(); await loaded();
    fireEvent.change(screen.getByRole("combobox", { name: "General reader model" }), { target: { value: `Candle:${PADDLE.model_id}` } });
    await clickReady("Install general reader and use it");
    expect(await screen.findByRole("alert")).toHaveTextContent("Download failed");
    expect(update).not.toHaveBeenCalled();
    expect(screen.getByRole("checkbox", { name: "Enable OCR" })).toBeEnabled();
  });

  it("keeps controls locked across intermediate installation completion events", async () => {
    catalogue = CATALOGUE;
    let finish!: () => void;
    api.installLayoutDetector.mockImplementationOnce(() => new Promise<void>((resolve) => { finish = resolve; }));
    panel(SETTINGS); await clickReady("Install required models for formula");
    await waitFor(() => expect(api.installLayoutDetector).toHaveBeenCalled());
    expect(screen.getByRole("combobox", { name: "General reader model" })).toBeDisabled();
    finish();
    await waitFor(() => expect(screen.getByRole("combobox", { name: "General reader model" })).toBeEnabled());
  });

  it("preserves an unavailable formula selection and blocks enabling", async () => {
    const update = panel(configured({ formula_model: "gone", disabled_roles: ["formula"] }, false)); await loaded();
    expect(screen.getByRole("combobox", { name: "Formula model" })).toHaveValue("gone");
    expect(screen.getByRole("option", { name: "Unavailable: gone" })).toBeInTheDocument();
    expect(screen.getByRole("checkbox", { name: "Enable OCR" })).toBeDisabled();
    expect(update).not.toHaveBeenCalled();
  });

  it("never offers specialists or unavailable engines as general models", async () => {
    panel(); await loaded();
    const selector = screen.getByRole("combobox", { name: "General reader model" });
    expect(within(selector).queryByRole("option", { name: /Texify|SLANet/ })).not.toBeInTheDocument();
    expect(within(selector).queryByRole("option", { name: /Apple Vision/ })).not.toBeInTheDocument();
  });

  it("reveals descriptions on request, saves a trimmed model, and explicitly disables", async () => {
    const update = panel(configured({ scope: "typeset_and_embedded" })); await loaded();
    expect(screen.queryByRole("textbox", { name: "Ollama description model" })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("checkbox", { name: "Enable image descriptions", hidden: true }));
    fireEvent.change(screen.getByRole("textbox", { name: "Ollama description model" }), { target: { value: " qwen3-vl:4b " } });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(update).toHaveBeenCalledWith({ image_analysis: expect.objectContaining({ describer_model: "qwen3-vl:4b" }) }));
    expect(screen.getByText(/Image destination:/)).toHaveTextContent("http://127.0.0.1:11434");
    fireEvent.click(screen.getByRole("checkbox", { name: "Enable image descriptions" }));
    await waitFor(() => expect(update).toHaveBeenLastCalledWith({ image_analysis: expect.objectContaining({ describer_model: "" }) }));
  });

  it("recovers from catalogue loading failure", async () => {
    api.imageRecognizerCatalogue.mockRejectedValueOnce(new Error("Catalogue unavailable")); panel();
    expect(await screen.findByRole("alert")).toHaveTextContent("Catalogue unavailable");
    fireEvent.click(screen.getByRole("button", { name: "Retry loading OCR components" }));
    await loaded();
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });
  it("keeps disabled roles when an alternative formula model is downloaded and applied", async () => {
    catalogue = { ...catalogue, models: [...catalogue.models, PP_FORMULANET] };
    const update = panel(configured({ disabled_roles: ["formula"] })); await loaded();
    fireEvent.change(screen.getByRole("combobox", { name: "Formula model" }), { target: { value: PP_FORMULANET.model_id } });
    await clickReady("Install formula reader and use it");
    await waitFor(() => expect(update).toHaveBeenCalledWith({ image_analysis: expect.objectContaining({ formula_model: PP_FORMULANET.model_id, disabled_roles: ["formula"] }) }));
    expect(region("Formulas").getByText(`Effective reader: General reader: ${GRANITE.display_name}`)).toBeInTheDocument();
  });

  it("keeps an unavailable saved general reader visible while allowing specialist-only repair", async () => {
    const update = panel(configured({ model: "gone" }, false)); await loaded();
    expect(screen.getByRole("option", { name: "Unavailable: gone" })).toBeInTheDocument();
    expect(screen.getByRole("checkbox", { name: "Enable OCR" })).toBeDisabled();
    fireEvent.click(screen.getByRole("checkbox", { name: "Use general reader" }));
    await waitFor(() => expect(update).toHaveBeenCalledWith({ image_analysis: expect.objectContaining({ model: "gone", disabled_roles: ["page"] }) }));
    expect(screen.getByRole("checkbox", { name: "Enable OCR" })).toBeEnabled();
  });

  it("requires successful license disclosure before downloading and permits a retry", async () => {
    catalogue = CATALOGUE;
    api.imageRecognizerInventory.mockRejectedValue(new Error("Inventory unavailable"));
    const log = vi.spyOn(console, "error").mockImplementation(() => {});
    panel(SETTINGS);
    const action = await screen.findByRole("button", { name: "Install required models for formula" });
    await waitFor(() => expect(log).toHaveBeenCalled());
    expect(action).toBeDisabled();
    expect(api.installImageRecognizer).not.toHaveBeenCalled();
    api.imageRecognizerInventory.mockResolvedValue(INVENTORY);
    fireEvent.click(region("Formulas").getByRole("button", { name: "Load model details" }));
    await waitFor(() => expect(action).toBeEnabled());
    log.mockRestore();
  });

  it("retains successful downloads but does not apply a draft after layout installation fails", async () => {
    catalogue = installed([GRANITE.model_id], false);
    api.installLayoutDetector.mockRejectedValueOnce(new Error("Layout download failed"));
    const update = panel(configured({}, false)); await loaded();
    fireEvent.change(screen.getByRole("combobox", { name: "General reader model" }), { target: { value: `Candle:${PADDLE.model_id}` } });
    await clickReady("Install required models for general");
    expect(await screen.findByRole("alert")).toHaveTextContent("Layout download failed");
    expect(api.installImageRecognizer).toHaveBeenCalledWith("Candle", PADDLE.model_id);
    expect(update).not.toHaveBeenCalled();
    expect(region("General reader").getByText(`Saved reader: ${GRANITE.display_name}`)).toBeInTheDocument();
  });

  it("prevents removing the only processing scope when layout is absent", async () => {
    catalogue = installed([GRANITE.model_id], false);
    const update = panel(configured({ scope: "typeset_and_embedded" })); await loaded();
    fireEvent.click(screen.getByRole("checkbox", { name: "Include embedded images" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Complete OCR setup");
    expect(update).not.toHaveBeenCalled();
  });

  it("ignores stale catalogue responses during installation refreshes", async () => {
    panel(); await loaded();
    let finish!: (value: RecognizerCatalogue) => void;
    api.imageRecognizerCatalogue.mockImplementationOnce(() => new Promise<RecognizerCatalogue>((resolve) => { finish = resolve; }));
    await act(async () => { listeners.done(); });
    await act(async () => { listeners.done(); });
    await act(async () => { finish(installed(undefined, false)); });
    expect(region("Formulas").getByText(`Effective reader: ${TEXIFY.display_name}`)).toBeInTheDocument();
    expect(screen.getByText("Ready")).toBeInTheDocument();
  });

  it("does not substitute defaults for explicit empty saved model IDs", async () => {
    panel(configured({ model: "", formula_model: "" }, false)); await loaded();
    expect(screen.getByRole("checkbox", { name: "Enable OCR" })).toBeDisabled();
    expect(screen.getByRole("combobox", { name: "General reader model" })).toHaveValue("Onnx:");
    expect(screen.getByRole("combobox", { name: "Formula model" })).toHaveValue("");
  });

});
