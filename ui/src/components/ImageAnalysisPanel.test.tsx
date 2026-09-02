import { fireEvent, render, screen, waitFor } from "@testing-library/react";
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
  is_cached: false,
  footprint_bytes: 320_847_936,
  admission_threshold: 0,
  emits: ["formula"],
};

const CATALOGUE: RecognizerCatalogue = {
  engines: ["Onnx", "Candle"],
  models: [GRANITE, PADDLE, TEXIFY],
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

describe("ImageAnalysisPanel", () => {
  const api = {
    imageRecognizerCatalogue: vi.fn(() => Promise.resolve(CATALOGUE)),
    imageRecognizerInventory: vi.fn(() => Promise.resolve(INVENTORY)),
    installImageRecognizer: vi.fn(() => Promise.resolve()),
    onImageAnalysisProgress: vi.fn(() => Promise.resolve(() => {})),
    onImageAnalysisDone: vi.fn(() => Promise.resolve(() => {})),
    onImageAnalysisError: vi.fn(() => Promise.resolve(() => {})),
  } as never;

  const withCatalogue = (catalogue: RecognizerCatalogue) => {
    (api as unknown as {
      imageRecognizerCatalogue: () => Promise<RecognizerCatalogue>;
    }).imageRecognizerCatalogue = vi.fn(() => Promise.resolve(catalogue));
  };

  beforeEach(() => {
    vi.clearAllMocks();
    withCatalogue(CATALOGUE);
    (api as unknown as { installImageRecognizer: unknown }).installImageRecognizer = vi.fn(
      () => Promise.resolve(),
    );
    (api as unknown as { installLayoutDetector: unknown }).installLayoutDetector = vi.fn(
      () => Promise.resolve(),
    );
  });

  /// The catalogue with one model replaced, addressed by id. Models live in
  /// one list now, so a test that wants an installed formula reader edits a
  /// row rather than a field.
  const withModel = (modelId: string, patch: Record<string, unknown>) =>
    withCatalogue({
      ...CATALOGUE,
      models: CATALOGUE.models.map((model) =>
        model.model_id === modelId ? { ...model, ...patch } : model,
      ),
    });

  const panel = (settings: Settings, onUpdateSettings = vi.fn(() => Promise.resolve())) => {
    render(
      <ImageAnalysisPanel
        api={api}
        settings={settings}
        onUpdateSettings={onUpdateSettings}
      />,
    );
    return onUpdateSettings;
  };

  /// The cost of the toggle is not a display preference, and the panel is the
  /// only place the user is told so before paying it.
  it("says what enabling costs before it is enabled", async () => {
    panel(SETTINGS);
    await screen.findByText(/re-reads and re-embeds/i);
    expect(screen.getByText(/four\s+minutes for a full-width one/i)).toBeTruthy();
  });

  it("offers the download when the recognizer is missing, and no toggle to enable without it", async () => {
    panel(SETTINGS);

    const download = await screen.findByRole("button", { name: /download recognizer/i });
    const toggle = screen.getByRole("checkbox");
    expect((toggle as HTMLInputElement).disabled).toBe(true);

    fireEvent.click(download);
    await waitFor(() =>
      expect(
        (api as unknown as { installImageRecognizer: { mock: { calls: unknown[][] } } })
          .installImageRecognizer.mock.calls[0],
      ).toEqual(["Onnx", GRANITE.model_id]),
    );
  });

  /// FIGURE.md requires the redistributed checkpoint to carry a license and
  /// provenance inventory. This is where a person meets it, and it has to be
  /// readable *before* the weights are fetched onto their disk — so it is
  /// rendered on the uninstalled panel, beside the button that would fetch it.
  it("discloses the licence, the size and the pinned revision before the download", async () => {
    panel(SETTINGS);

    await screen.findByRole("button", { name: /download recognizer/i });
    // The detector is disclosed under the same licence and renders first, so
    // the recognizer's link is waited for by where it points — not merely for
    // *a* link saying Apache-2.0, which is already on screen.
    await waitFor(() =>
      expect(
        screen
          .getAllByRole("link", { name: /apache-2\.0/i })
          .some((link) => link.getAttribute("href") === INVENTORY.license_url),
      ).toBe(true),
    );
    expect(screen.getByText(/PaddlePaddle\/PaddleOCR-VL-1\.6/)).toBeTruthy();
    expect(screen.getByText(/c5630abae1d9/)).toBeTruthy();
    expect(screen.getAllByText(/1\.8 GB/).length).toBeGreaterThan(0);
  });

  /// Switching engine is not a model choice by itself: the engine's own
  /// default is what it reads with, and the disclosure follows the selection
  /// rather than the commitment.
  it("selects the engine's default recognizer and discloses that one", async () => {
    panel(SETTINGS);
    await screen.findByRole("button", { name: /download recognizer/i });

    fireEvent.click(screen.getByRole("button", { name: "Candle" }));

    await waitFor(() =>
      expect(
        (api as unknown as {
          imageRecognizerInventory: { mock: { calls: unknown[][] } };
        }).imageRecognizerInventory.mock.calls.at(-1),
      ).toEqual(["Candle", PADDLE.model_id]),
    );
    expect(screen.getByText(PADDLE.display_name)).toBeTruthy();
  });

  /// `build_analyzer` refuses a recognizer that is named but not on disk, so
  /// the weights have to arrive before the choice is written down. The order
  /// is the assertion.
  it("installs a recognizer before committing it to the settings", async () => {
    const order: string[] = [];
    (api as unknown as { installImageRecognizer: unknown }).installImageRecognizer = vi.fn(
      () => {
        order.push("install");
        return Promise.resolve();
      },
    );
    const onUpdate = vi.fn(() => {
      order.push("settings");
      return Promise.resolve();
    });
    panel(SETTINGS, onUpdate);
    await screen.findByRole("button", { name: /download recognizer/i });

    fireEvent.click(screen.getByRole("button", { name: "Candle" }));
    fireEvent.click(
      await screen.findByRole("button", { name: /download recognizer and use it/i }),
    );

    await waitFor(() => expect(order).toEqual(["install", "settings"]));
    expect(onUpdate).toHaveBeenCalledWith({
      image_analysis: {
        enabled: false,
        engine: "Candle",
        model: PADDLE.model_id,
        device: null,
        describer_model: "",
        scope: "typeset_only",
      },
    });
  });

  /// An engine the build left out is shown rather than hidden: absent and
  /// merely unselected are different answers, and only one of them is a
  /// property of the build.
  it("shows an engine this build cannot recognize with, and refuses to select it", async () => {
    withCatalogue({ engines: ["Onnx"], models: [GRANITE] });
    panel(SETTINGS);

    // The tooltip lends the button its accessible name, so what a screen
    // reader is told is why it cannot be chosen. More than one engine can be
    // out of a build, so the button is picked by the engine it names.
    const absent = await screen.findAllByRole("button", {
      name: /feature disabled in this build/i,
    });
    const candle = absent.find((button) => button.textContent === "Candle");
    expect(candle).toBeTruthy();
    if (!candle) throw new Error("the Candle button is missing");
    expect((candle as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(candle);
    expect(screen.getByText(GRANITE.display_name)).toBeTruthy();
  });

  it("persists the toggle as a whole image-analysis object", async () => {
    withCatalogue({ engines: ["Onnx", "Candle"], models: [{ ...GRANITE, is_cached: true }, PADDLE] });
    const onUpdate = panel(SETTINGS);

    await waitFor(() =>
      expect((screen.getByRole("checkbox") as HTMLInputElement).disabled).toBe(false),
    );
    fireEvent.click(screen.getByRole("checkbox"));
    await waitFor(() =>
      expect(onUpdate).toHaveBeenCalledWith({
        image_analysis: {
          enabled: true,
          engine: "Onnx",
          model: null,
          device: null,
          describer_model: "",
          scope: "typeset_only",
        },
      }),
    );
  });

  /// A remote describer sends the pictures in the user's documents to another
  /// machine. Naming the server it would go to is the disclosure.
  it("names the server a description would be sent to", async () => {
    panel(SETTINGS);
    await screen.findByText(/http:\/\/127\.0\.0\.1:11434/);
    expect(screen.getByText(/sent to it/i)).toBeTruthy();
  });

  it("saves the describer model only once it has changed", async () => {
    const onUpdate = panel(SETTINGS);
    const save = await screen.findByRole("button", { name: /^save$/i });
    expect((save as HTMLButtonElement).disabled).toBe(true);

    fireEvent.change(screen.getByPlaceholderText("qwen3-vl:2b"), {
      target: { value: " qwen3-vl:4b " },
    });
    fireEvent.click(screen.getByRole("button", { name: /^save$/i }));
    await waitFor(() =>
      expect(onUpdate).toHaveBeenCalledWith({
        image_analysis: {
          enabled: false,
          engine: "Onnx",
          model: null,
          device: null,
          describer_model: "qwen3-vl:4b",
          scope: "typeset_only",
        },
      }),
    );
  });

  /// Neither download is required any more — a reading runs without them —
  /// so what the panel owes the user is what each one's absence costs, stated
  /// where the download is offered. Without the detector nothing a page
  /// typesets is marked out, and a document full of mathematics reads exactly
  /// like one with none.
  it("offers the detector and says what its absence costs", async () => {
    // The formula reader is installed here so the only absence this test can
    // see is the detector's. Both sections say what theirs costs.
    withModel(TEXIFY.model_id, { is_cached: true });
    panel(SETTINGS);

    await screen.findByText(DETECTOR.inventory.name);
    expect(
      screen.getByText(/nothing a page typesets will be marked out/i),
    ).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: /download detector/i }));
    await waitFor(() =>
      expect(
        (api as unknown as { installLayoutDetector: () => void }).installLayoutDetector,
      ).toHaveBeenCalled(),
    );
  });

  /// The formula reader is a catalogue row, so installing it is installing a
  /// recognizer — there is no second install path. It is still offered in its
  /// own section, because it runs alongside the page reader rather than
  /// instead of one.
  it("installs the formula reader through the recognizer install", async () => {
    panel(SETTINGS);

    await screen.findByText(TEXIFY.display_name);
    fireEvent.click(screen.getByRole("button", { name: /download formula reader/i }));
    await waitFor(() =>
      expect(
        (api as unknown as { installImageRecognizer: (e: string, m: string) => void })
          .installImageRecognizer,
      ).toHaveBeenCalledWith(TEXIFY.engine, TEXIFY.model_id),
    );
  });

  /// A formula reader reads one cropped expression and emits a single
  /// whole-crop region. Offered as the page recognizer it would read every
  /// page of the library as one failed expression, which afterwards is
  /// indistinguishable from a library whose pictures hold no text.
  it("never offers the formula reader as the page recognizer", async () => {
    panel(SETTINGS);

    await screen.findByText(TEXIFY.display_name);
    expect(screen.queryByText(TEXIFY.description)).toBeNull();
  });

  it("says nothing about installing a formula reader that is already here", async () => {
    withModel(TEXIFY.model_id, { is_cached: true });
    panel(SETTINGS);

    await screen.findByText(TEXIFY.display_name);
    expect(
      screen.queryByRole("button", { name: /download formula reader/i }),
    ).toBeNull();
  });

  it("says nothing about installing a detector that is already here", async () => {
    withCatalogue({
      ...CATALOGUE,
      models: CATALOGUE.models.map((model) =>
        model.model_id === TEXIFY.model_id ? { ...model, is_cached: true } : model,
      ),
      detector: { ...DETECTOR, is_installed: true },
    });
    panel(SETTINGS);

    await screen.findByText(DETECTOR.inventory.name);
    expect(screen.queryByRole("button", { name: /download detector/i })).toBeNull();
    expect(
      screen.queryByText(/nothing a page typesets will be marked out/i),
    ).toBeNull();
  });

  /// The scope is what decides whether this feature costs seconds or an
  /// overnight run, so it is offered — and it is offered only once there is
  /// something to scope, which is why it is absent while the feature is off.
  it("offers the scope only while image analysis is on, and persists it", async () => {
    withCatalogue({
      engines: ["Onnx", "Candle"],
      models: [{ ...GRANITE, is_cached: true }, PADDLE],
    });
    panel(SETTINGS);
    await waitFor(() => expect(screen.queryByRole("radio")).toBeNull());

    const enabled = {
      ...SETTINGS,
      image_analysis: { ...SETTINGS.image_analysis, enabled: true },
    } as unknown as Settings;
    const onUpdate = panel(enabled);

    const wider = await screen.findByRole("radio", { name: /every picture/i });
    expect(
      (screen.getByRole("radio", { name: /formulas and tables only/i }) as HTMLInputElement)
        .checked,
    ).toBe(true);

    fireEvent.click(wider);
    await waitFor(() =>
      expect(onUpdate).toHaveBeenCalledWith({
        image_analysis: {
          enabled: true,
          engine: "Onnx",
          model: null,
          device: null,
          describer_model: "",
          scope: "typeset_and_embedded",
        },
      }),
    );
  });
});
