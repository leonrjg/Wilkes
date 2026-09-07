import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import CustomIntegrations from "./CustomIntegrations";
import type {
  CustomIntegrationConfig,
  ProbeReport,
  Settings,
} from "../lib/types";

function settings(custom: CustomIntegrationConfig[] = []): Settings {
  return {
    favorites: [],
    recent_dirs: [],
    last_directory: null,
    respect_gitignore: true,
    max_file_size: 1024,
    theme: "System",
    search_prefer_semantic: false,
    semantic: {
      enabled: false,
      selected: { engine: "Fastembed", model: "AllMiniLML6V2", dimension: 384 },
      engine_devices: {},
      index_path: null,
      custom_models: [],
      chunk_size: 600,
      chunk_overlap: 128,
      worker_timeout_secs: 300,
      embed_batch_size: 16,
    },
    integrations: {
      zotero: {
        enabled: false,
        base_url: "http://127.0.0.1:23119",
        citation_style: "chicago-note-bibliography",
      },
      semantic_scholar: {
        enabled: false,
        base_url: "https://api.semanticscholar.org",
        api_key: null,
      },
      openalex: {
        enabled: false,
        base_url: "https://api.openalex.org",
        email: null,
      },
      custom,
    },
    supported_extensions: ["pdf"],
    max_results: 50,
    bookmarks_dock: "Right",
  } as Settings;
}

const CLEAN_PROBE: ProbeReport = {
  id: "custom:crossref",
  capability: "search",
  request_url: "https://api.crossref.org/works?rows=3",
  raw_response: '{"message":{"items":[]}}',
  results: [
    {
      id: "10.1/example",
      doi: "10.1/example",
      title: "A paper",
      year: 2021,
      publication_date: null,
      venue: null,
      citation_count: 4,
      is_open_access: false,
      pdf_url: null,
      landing_page_url: null,
      open_access_status: null,
      license: null,
    },
  ],
  issues: [],
  ok: true,
  error: null,
};

function apiWith(overrides: Record<string, unknown> = {}) {
  return {
    customIntegrationSummary: vi.fn().mockResolvedValue({
      id: "crossref",
      name: "Crossref",
      host: "api.crossref.org",
      capabilities: ["search", "health"],
      required_secrets: [],
      problems: [],
    }),
    customIntegrationProbe: vi.fn().mockResolvedValue(CLEAN_PROBE),
    customIntegrationStatus: vi.fn(),
    ...overrides,
  } as never;
}

describe("CustomIntegrations", () => {
  it("names the host a manifest will contact before anything is saved", async () => {
    const onUpdate = vi.fn();
    render(
      <CustomIntegrations
        api={apiWith()}
        settings={settings()}
        onUpdate={onUpdate}
      />,
    );

    fireEvent.click(screen.getByText("Add integration"));
    fireEvent.click(screen.getByText("Read manifest"));

    await waitFor(() => {
      expect(screen.getByText("api.crossref.org")).toBeInTheDocument();
    });
    expect(onUpdate).not.toHaveBeenCalled();
  });

  it("refuses to enable a manifest that has not probed clean", async () => {
    const api = apiWith({
      customIntegrationProbe: vi.fn().mockResolvedValue({
        ...CLEAN_PROBE,
        ok: false,
        issues: [
          {
            record: 0,
            field: "citation_count",
            selector: "is-referenced-by-count",
            problem: "expected an integer, found a string",
          },
        ],
      }),
    });
    const onUpdate = vi.fn();
    render(
      <CustomIntegrations api={api} settings={settings()} onUpdate={onUpdate} />,
    );

    fireEvent.click(screen.getByText("Add integration"));
    fireEvent.click(screen.getByText("Read manifest"));
    await waitFor(() =>
      expect(screen.getByText("Probe")).toBeInTheDocument(),
    );

    // Enabling is unavailable until a probe has come back clean.
    expect(screen.getByText("Save and enable")).toBeDisabled();

    fireEvent.click(screen.getByText("Probe"));
    await waitFor(() => {
      expect(
        screen.getByText(/citation_count.*expected an integer/),
      ).toBeInTheDocument();
    });
    // A probe that reported unmapped values leaves it unavailable.
    expect(screen.getByText("Save and enable")).toBeDisabled();
  });

  it("saves and enables once the probe is clean", async () => {
    const onUpdate = vi.fn().mockResolvedValue(undefined);
    render(
      <CustomIntegrations
        api={apiWith()}
        settings={settings()}
        onUpdate={onUpdate}
      />,
    );

    fireEvent.click(screen.getByText("Add integration"));
    fireEvent.click(screen.getByText("Read manifest"));
    await waitFor(() => expect(screen.getByText("Probe")).toBeInTheDocument());
    fireEvent.click(screen.getByText("Probe"));
    await waitFor(() =>
      expect(screen.getByText("Save and enable")).not.toBeDisabled(),
    );

    fireEvent.click(screen.getByText("Save and enable"));
    await waitFor(() => expect(onUpdate).toHaveBeenCalled());
    const patch = onUpdate.mock.calls[0][0];
    expect(patch.integrations.custom).toHaveLength(1);
    expect(patch.integrations.custom[0]).toMatchObject({
      id: "crossref",
      enabled: true,
    });
  });

  it("shows a refused save rather than swallowing it", async () => {
    const onUpdate = vi
      .fn()
      .mockRejectedValue(new Error("custom integration 'crossref' cannot be saved"));
    render(
      <CustomIntegrations
        api={apiWith()}
        settings={settings()}
        onUpdate={onUpdate}
      />,
    );

    fireEvent.click(screen.getByText("Add integration"));
    fireEvent.click(screen.getByText("Read manifest"));
    await waitFor(() =>
      expect(screen.getByText("Save disabled")).toBeInTheDocument(),
    );
    fireEvent.click(screen.getByText("Save disabled"));

    await waitFor(() => {
      expect(screen.getByText(/cannot be saved/)).toBeInTheDocument();
    });
  });

  it("reports a manifest's problems instead of an empty summary", async () => {
    const api = apiWith({
      customIntegrationSummary: vi.fn().mockResolvedValue({
        id: "",
        name: "",
        host: null,
        capabilities: [],
        required_secrets: [],
        problems: ["search.fields.titel is not a result field"],
      }),
    });
    render(
      <CustomIntegrations api={api} settings={settings()} onUpdate={vi.fn()} />,
    );

    fireEvent.click(screen.getByText("Add integration"));
    fireEvent.click(screen.getByText("Read manifest"));

    await waitFor(() => {
      expect(
        screen.getByText("search.fields.titel is not a result field"),
      ).toBeInTheDocument();
    });
    expect(screen.queryByText("Probe")).not.toBeInTheDocument();
  });

  it("asks for the secrets a manifest names, and never puts them in it", async () => {
    const api = apiWith({
      customIntegrationSummary: vi.fn().mockResolvedValue({
        id: "crossref",
        name: "Crossref",
        host: "api.crossref.org",
        capabilities: ["search"],
        required_secrets: ["crossref_token"],
        problems: [],
      }),
    });
    const onUpdate = vi.fn().mockResolvedValue(undefined);
    render(
      <CustomIntegrations api={api} settings={settings()} onUpdate={onUpdate} />,
    );

    fireEvent.click(screen.getByText("Add integration"));
    fireEvent.click(screen.getByText("Read manifest"));
    await waitFor(() =>
      expect(screen.getByLabelText("Secret crossref_token")).toBeInTheDocument(),
    );
    fireEvent.change(screen.getByLabelText("Secret crossref_token"), {
      target: { value: "hunter2" },
    });
    fireEvent.click(screen.getByText("Save disabled"));

    await waitFor(() => expect(onUpdate).toHaveBeenCalled());
    const saved = onUpdate.mock.calls[0][0].integrations.custom[0];
    expect(saved.secrets).toEqual({ crossref_token: "hunter2" });
    expect(saved.manifest).not.toContain("hunter2");
  });

  it("lists a configured integration and toggles it without reopening the editor", async () => {
    const onUpdate = vi.fn().mockResolvedValue(undefined);
    const configured: CustomIntegrationConfig = {
      id: "crossref",
      enabled: false,
      manifest: "manifest_version = 1",
      secrets: {},
    };
    render(
      <CustomIntegrations
        api={apiWith()}
        settings={settings([configured])}
        onUpdate={onUpdate}
      />,
    );

    expect(screen.getByText("custom:crossref")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("checkbox"));

    await waitFor(() => expect(onUpdate).toHaveBeenCalled());
    expect(onUpdate.mock.calls[0][0].integrations.custom[0].enabled).toBe(true);
  });
});
