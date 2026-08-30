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
  },
} as unknown as Settings;

const GRANITE = {
  engine: "Onnx" as const,
  model_id: "ibm-granite/granite-docling-258M",
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
  display_name: "PaddleOCR-VL paddleocr-vl-1.6",
  description: "Transcribes with per-region geometry.",
  is_default: false,
  is_engine_default: true,
  is_cached: false,
  footprint_bytes: 1_928_447_087,
  admission_threshold: 0.6,
  emits: ["text"],
};

const CATALOGUE: RecognizerCatalogue = {
  engines: ["Onnx", "Candle"],
  models: [GRANITE, PADDLE],
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
    const licence = await screen.findByRole("link", { name: /apache-2\.0/i });
    expect(licence.getAttribute("href")).toBe(INVENTORY.license_url);
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
    // reader is told is why it cannot be chosen.
    const candle = await screen.findByRole("button", {
      name: /feature disabled in this build/i,
    });
    expect(candle.textContent).toBe("Candle");
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
        },
      }),
    );
  });
});
