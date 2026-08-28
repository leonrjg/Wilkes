import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Settings } from "../lib/types";
import ImageAnalysisPanel from "./ImageAnalysisPanel";

const SETTINGS = {
  generation: { ollama_url: "http://127.0.0.1:11434" },
  image_analysis: { enabled: false, device: null, describer_model: "" },
} as unknown as Settings;

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
    isImageRecognizerInstalled: vi.fn(() => Promise.resolve(true)),
    imageRecognizerInventory: vi.fn(() => Promise.resolve(INVENTORY)),
    installImageRecognizer: vi.fn(() => Promise.resolve()),
    onImageAnalysisProgress: vi.fn(() => Promise.resolve(() => {})),
    onImageAnalysisDone: vi.fn(() => Promise.resolve(() => {})),
    onImageAnalysisError: vi.fn(() => Promise.resolve(() => {})),
  } as never;

  beforeEach(() => {
    vi.clearAllMocks();
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
    (api as unknown as { isImageRecognizerInstalled: () => Promise<boolean> })
      .isImageRecognizerInstalled = vi.fn(() => Promise.resolve(false));
    panel(SETTINGS);

    const download = await screen.findByRole("button", { name: /download recognizer/i });
    const toggle = screen.getByRole("checkbox");
    expect((toggle as HTMLInputElement).disabled).toBe(true);

    fireEvent.click(download);
    await waitFor(() =>
      expect(
        (api as unknown as { installImageRecognizer: { mock: { calls: unknown[] } } })
          .installImageRecognizer.mock.calls.length,
      ).toBe(1),
    );
  });

  /// FIGURE.md requires the redistributed checkpoint to carry a license and
  /// provenance inventory. This is where a person meets it, and it has to be
  /// readable *before* 1.9 GB is fetched onto their disk — so it is rendered
  /// on the uninstalled panel, beside the button that would fetch it.
  it("discloses the licence, the size and the pinned revision before the download", async () => {
    (api as unknown as { isImageRecognizerInstalled: () => Promise<boolean> })
      .isImageRecognizerInstalled = vi.fn(() => Promise.resolve(false));
    panel(SETTINGS);

    await screen.findByRole("button", { name: /download recognizer/i });
    const licence = await screen.findByRole("link", { name: /apache-2\.0/i });
    expect(licence.getAttribute("href")).toBe(INVENTORY.license_url);
    expect(screen.getByText(/PaddlePaddle\/PaddleOCR-VL-1\.6/)).toBeTruthy();
    expect(screen.getByText(/c5630abae1d9/)).toBeTruthy();
    // Once for the whole download, once for the file it is nearly all of.
    expect(screen.getAllByText(/1\.8 GB/).length).toBeGreaterThan(0);
  });

  it("persists the toggle as a whole image-analysis object", async () => {
    (api as unknown as { isImageRecognizerInstalled: () => Promise<boolean> })
      .isImageRecognizerInstalled = vi.fn(() => Promise.resolve(true));
    const onUpdate = panel(SETTINGS);

    fireEvent.click(await screen.findByRole("checkbox"));
    await waitFor(() =>
      expect(onUpdate).toHaveBeenCalledWith({
        image_analysis: { enabled: true, device: null, describer_model: "" },
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
    const save = await screen.findByRole("button", { name: /save/i });
    expect((save as HTMLButtonElement).disabled).toBe(true);

    fireEvent.change(screen.getByPlaceholderText("qwen3-vl:2b"), {
      target: { value: " qwen3-vl:4b " },
    });
    fireEvent.click(screen.getByRole("button", { name: /save/i }));
    await waitFor(() =>
      expect(onUpdate).toHaveBeenCalledWith({
        image_analysis: { enabled: false, device: null, describer_model: "qwen3-vl:4b" },
      }),
    );
  });
});
