import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Settings } from "../lib/types";
import ImageAnalysisPanel from "./ImageAnalysisPanel";

const SETTINGS = {
  generation: { ollama_url: "http://127.0.0.1:11434" },
  image_analysis: { enabled: false, device: null, describer_model: "" },
} as unknown as Settings;

describe("ImageAnalysisPanel", () => {
  const api = {
    isImageRecognizerInstalled: vi.fn(() => Promise.resolve(true)),
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
    expect(screen.getByText(/minute per picture/i)).toBeTruthy();
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
