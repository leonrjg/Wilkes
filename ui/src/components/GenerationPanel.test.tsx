import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { GeneratorDescriptor, Settings } from "../lib/types";
import { useGenerationStore } from "../stores/useGenerationStore";
import GenerationPanel from "./GenerationPanel";

const MODELS: GeneratorDescriptor[] = [
  {
    model_id: "org/default-model",
    display_name: "Default Model",
    description: "Small default generation model",
    weights_file: "default.gguf",
    weights_revision: "revision-a",
    tokenizer_repo: "org/default-model",
    tokenizer_revision: "revision-a",
    context_tokens: 4096,
    is_cached: true,
    is_default: true,
    is_recommended: true,
    size_bytes: 1_073_741_824,
  },
  {
    model_id: "org/reasoning-model",
    display_name: "Reasoning Model",
    description: "Larger model for careful explanations",
    weights_file: "reasoning.gguf",
    weights_revision: "revision-b",
    tokenizer_repo: "org/reasoning-model",
    tokenizer_revision: "revision-b",
    context_tokens: 8192,
    is_cached: false,
    is_default: false,
    is_recommended: true,
    size_bytes: null,
  },
];

const SETTINGS = {
  generation: {
    enabled: false,
    model: null,
    device: null,
    sampling_overrides: {},
  },
} as Settings;

describe("GenerationPanel", () => {
  const refreshReady = vi.fn().mockResolvedValue(false);
  const api = {
    listGenerationModels: vi.fn().mockResolvedValue(MODELS),
    getGenerationModelSize: vi.fn().mockResolvedValue(2_147_483_648),
    loadGenerationModel: vi.fn().mockResolvedValue(true),
    onGenerationProgress: vi.fn().mockResolvedValue(() => {}),
    onGenerationDone: vi.fn().mockResolvedValue(() => {}),
    onGenerationError: vi.fn().mockResolvedValue(() => {}),
  } as any;
  const onUpdateSettings = vi.fn().mockResolvedValue(undefined);

  beforeEach(() => {
    vi.clearAllMocks();
    api.listGenerationModels.mockResolvedValue(MODELS);
    api.getGenerationModelSize.mockResolvedValue(2_147_483_648);
    api.loadGenerationModel.mockResolvedValue(true);
    api.onGenerationProgress.mockResolvedValue(() => {});
    api.onGenerationDone.mockResolvedValue(() => {});
    api.onGenerationError.mockResolvedValue(() => {});
    onUpdateSettings.mockResolvedValue(undefined);
    refreshReady.mockResolvedValue(false);
    useGenerationStore.setState({ ready: false, refreshReady } as any);
  });

  it("uses the shared searchable model catalog", async () => {
    render(
      <GenerationPanel
        api={api}
        settings={SETTINGS}
        onUpdateSettings={onUpdateSettings}
      />,
    );

    expect(await screen.findByText("Default Model")).toBeInTheDocument();
    expect(screen.getByText("Reasoning Model")).toBeInTheDocument();
    expect(screen.getByText("2 available")).toBeInTheDocument();

    fireEvent.change(screen.getByLabelText("Search generation model"), {
      target: { value: "reasoning" },
    });

    expect(screen.queryByText("Default Model")).not.toBeInTheDocument();
    expect(screen.getByText("Reasoning Model")).toBeInTheDocument();
    expect(screen.getByText("1 match")).toBeInTheDocument();
  });

  it("fetches an unknown download size when a model is selected", async () => {
    render(
      <GenerationPanel
        api={api}
        settings={SETTINGS}
        onUpdateSettings={onUpdateSettings}
      />,
    );

    fireEvent.click(await screen.findByRole("button", { name: /Reasoning Model/ }));

    await waitFor(() => {
      expect(api.getGenerationModelSize).toHaveBeenCalledWith("org/reasoning-model");
    });
    expect(await screen.findByText("Estimated download: 2.0 GB")).toBeInTheDocument();
  });

  it("persists a selected model once and lets the settings change trigger its load", async () => {
    render(
      <GenerationPanel
        api={api}
        settings={SETTINGS}
        onUpdateSettings={onUpdateSettings}
      />,
    );

    fireEvent.click(await screen.findByRole("button", { name: /Reasoning Model/ }));
    await act(async () => {
      fireEvent.click(
        screen.getByRole("button", { name: "Download model and enable" }),
      );
    });

    expect(onUpdateSettings).toHaveBeenCalledWith({
      generation: {
        ...SETTINGS.generation,
        enabled: true,
        model: "org/reasoning-model",
      },
    });
    expect(api.loadGenerationModel).not.toHaveBeenCalled();
  });

  it("reloads the already configured model without rewriting settings", async () => {
    const enabledSettings = {
      ...SETTINGS,
      generation: {
        ...SETTINGS.generation,
        enabled: true,
        model: "org/default-model",
      },
    } as Settings;

    render(
      <GenerationPanel
        api={api}
        settings={enabledSettings}
        onUpdateSettings={onUpdateSettings}
      />,
    );

    await screen.findByText("Default Model");
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Reload model" }));
    });

    expect(api.loadGenerationModel).toHaveBeenCalledTimes(1);
    expect(onUpdateSettings).not.toHaveBeenCalled();
  });
});
