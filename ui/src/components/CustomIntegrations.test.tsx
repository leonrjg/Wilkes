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
    customIntegrationAuthoringPrompt: vi.fn().mockResolvedValue("AUTHORING PROMPT"),
    customIntegrationSummary: vi.fn().mockResolvedValue({
      id: "crossref",
      name: "Crossref",
      origins: ["https://api.crossref.org"],
      capabilities: ["search", "health"],
      required_secrets: [],
      problems: [],
    }),
    customIntegrationProbe: vi.fn().mockResolvedValue(CLEAN_PROBE),
    customIntegrationStatus: vi.fn(),
    writeClipboard: vi.fn().mockResolvedValue(undefined),
    ...overrides,
  } as never;
}

describe("CustomIntegrations", () => {
  it("copies the core-owned manifest generation prompt", async () => {
    const customIntegrationAuthoringPrompt = vi
      .fn()
      .mockResolvedValue("GENERATE A MANIFEST");
    const writeClipboard = vi.fn().mockResolvedValue(undefined);
    const api = apiWith({ customIntegrationAuthoringPrompt, writeClipboard });

    render(
      <CustomIntegrations api={api} settings={settings()} onUpdate={vi.fn()} />,
    );

    fireEvent.click(
      screen.getByRole("button", {
        name: "Copy manifest generation prompt",
      }),
    );

    await waitFor(() => {
      expect(customIntegrationAuthoringPrompt).toHaveBeenCalledOnce();
      expect(writeClipboard).toHaveBeenCalledWith("GENERATE A MANIFEST");
    });
    expect(
      screen.getByRole("button", {
        name: "Manifest generation prompt copied",
      }),
    ).toHaveTextContent("Prompt copied");
  });

  it("shows prompt-copy failures", async () => {
    const api = apiWith({
      customIntegrationAuthoringPrompt: vi
        .fn()
        .mockRejectedValue(new Error("prompt unavailable")),
    });

    render(
      <CustomIntegrations api={api} settings={settings()} onUpdate={vi.fn()} />,
    );
    fireEvent.click(
      screen.getByRole("button", {
        name: "Copy manifest generation prompt",
      }),
    );

    expect(
      await screen.findByText(/Could not copy to clipboard: prompt unavailable/),
    ).toBeInTheDocument();
  });

  it("starts with the complete HTML and download-resolution vocabulary", () => {
    render(
      <CustomIntegrations
        api={apiWith()}
        settings={settings()}
        onUpdate={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByText("Add integration"));
    const manifest = (screen.getByLabelText("Manifest") as HTMLTextAreaElement).value;
    expect(manifest).toContain('response_format = "html"');
    expect(manifest).toContain('attribute = "href"');
    expect(manifest).toContain("capture =");
    expect(manifest).toContain('coerce = "join"');
    expect(manifest).toContain("[capabilities.resolve_download]");
    expect(manifest).toContain("[[capabilities.resolve_download.steps.params]]");
    expect(manifest).toContain('## base_url = "https://annas-archive.example"');
    expect(manifest).toContain('secret = "anna_key"');
    expect(manifest).toContain('input = "query"');
  });

  it("names every origin a manifest will contact before anything is saved", async () => {
    const onUpdate = vi.fn();
    const api = apiWith({
      customIntegrationSummary: vi.fn().mockResolvedValue({
        id: "crossref",
        name: "Crossref",
        origins: [
          "https://libgen.li",
          "https://annas-archive.example",
        ],
        capabilities: ["search", "resolve_download"],
        required_secrets: [],
        problems: [],
      }),
    });
    render(
      <CustomIntegrations
        api={api}
        settings={settings()}
        onUpdate={onUpdate}
      />,
    );

    fireEvent.click(screen.getByText("Add integration"));
    fireEvent.click(screen.getByText("Read manifest"));

    await waitFor(() => {
      expect(
        screen.getByText(
          "https://libgen.li, https://annas-archive.example",
        ),
      ).toBeInTheDocument();
    });
    expect(onUpdate).not.toHaveBeenCalled();
  });

  it("probes with the editable example query and invalidates an old verdict", async () => {
    const customIntegrationProbe = vi.fn().mockResolvedValue(CLEAN_PROBE);
    const api = apiWith({ customIntegrationProbe });
    render(
      <CustomIntegrations api={api} settings={settings()} onUpdate={vi.fn()} />,
    );

    fireEvent.click(screen.getByText("Add integration"));
    fireEvent.click(screen.getByText("Read manifest"));
    const query = await screen.findByLabelText("Example query");
    expect(query).toHaveValue("graph neural networks");

    fireEvent.change(query, { target: { value: "protein folding" } });
    fireEvent.click(screen.getByText("Probe"));

    await waitFor(() => {
      expect(customIntegrationProbe).toHaveBeenCalledWith(
        expect.stringContaining('id = "anna-journals"'),
        {},
        "protein folding",
      );
      expect(screen.getByText(/Mapped 1 record with nothing left over/)).toBeInTheDocument();
    });

    // Editing the query drops the verdict it belonged to: the report on
    // screen must never describe a question other than the one in the box.
    fireEvent.change(query, { target: { value: "different terms" } });
    expect(
      screen.queryByText(/Mapped 1 record with nothing left over/),
    ).not.toBeInTheDocument();
  });

  /// The probe is evidence, not a gate. A service that is down, rate-limiting
  /// or holding nothing for the example query says nothing about whether the
  /// manifest is worth keeping — and refusing to save until it answered made
  /// the user's work hostage to someone else's uptime.
  it("saves a manifest that has not probed clean, and says what is known", async () => {
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

    // Before any probe: available, and the row says a probe is worth running.
    expect(screen.getByText("Save and enable")).not.toBeDisabled();
    expect(screen.getByText(/Not probed/)).toBeInTheDocument();

    fireEvent.click(screen.getByText("Probe"));
    await waitFor(() => {
      expect(
        screen.getByText(/citation_count.*expected an integer/),
      ).toBeInTheDocument();
    });
    // After a probe that found unmapped values: still available, and what the
    // probe found is said rather than enforced.
    expect(screen.getByText("Save and enable")).not.toBeDisabled();
    expect(screen.getByText(/did not come back clean/)).toBeInTheDocument();

    fireEvent.click(screen.getByText("Save and enable"));
    await waitFor(() => {
      expect(onUpdate).toHaveBeenCalledWith({
        integrations: expect.objectContaining({
          custom: [
            expect.objectContaining({ id: "crossref", enabled: true }),
          ],
        }),
      });
    });
  });

  describe("renaming", () => {
    const CONFIGURED: CustomIntegrationConfig[] = [
      { id: "crossref", enabled: true, manifest: 'id = "crossref"', secrets: {} },
    ];

    /// The id is what a stored provider filter and every log line name, so it
    /// stays on screen whatever the row is called.
    it("shows the manifest's name, and the id beside it", async () => {
      render(
        <CustomIntegrations
          api={apiWith()}
          settings={settings(CONFIGURED)}
          onUpdate={vi.fn()}
        />,
      );
      expect(await screen.findByText("Crossref")).toBeInTheDocument();
      expect(screen.getByText("custom:crossref")).toBeInTheDocument();
    });

    /// A name is a label on this installation's copy. Renaming must not mean
    /// editing the manifest, and so must not mean reading or probing it again.
    it("saves a new name without reading or probing the manifest", async () => {
      const onUpdate = vi.fn();
      const api = apiWith();
      render(
        <CustomIntegrations
          api={api}
          settings={settings(CONFIGURED)}
          onUpdate={onUpdate}
        />,
      );
      await screen.findByText("Crossref");

      fireEvent.click(screen.getByText("Rename"));
      fireEvent.change(screen.getByLabelText("Name for crossref"), {
        target: { value: "  Work DOIs  " },
      });
      fireEvent.click(screen.getByText("Save name"));

      await waitFor(() => {
        expect(onUpdate).toHaveBeenCalledWith({
          integrations: expect.objectContaining({
            custom: [
              expect.objectContaining({
                id: "crossref",
                name: "Work DOIs",
                manifest: 'id = "crossref"',
              }),
            ],
          }),
        });
      });
      expect(api.customIntegrationProbe).not.toHaveBeenCalled();
    });

    /// Clearing the box is no override, not a nameless provider.
    it("clears the override back to the manifest's own name", async () => {
      const onUpdate = vi.fn();
      render(
        <CustomIntegrations
          api={apiWith()}
          settings={settings([{ ...CONFIGURED[0], name: "Work DOIs" }])}
          onUpdate={onUpdate}
        />,
      );
      expect(await screen.findByText("Work DOIs")).toBeInTheDocument();

      fireEvent.click(screen.getByText("Rename"));
      fireEvent.change(screen.getByLabelText("Name for crossref"), {
        target: { value: "   " },
      });
      fireEvent.click(screen.getByText("Save name"));

      await waitFor(() => {
        expect(onUpdate).toHaveBeenCalledWith({
          integrations: expect.objectContaining({
            custom: [expect.objectContaining({ name: undefined })],
          }),
        });
      });
    });

    /// Editing a manifest is a different act from naming one, and must not
    /// quietly undo the other.
    it("keeps a name across a manifest edit", async () => {
      const onUpdate = vi.fn();
      render(
        <CustomIntegrations
          api={apiWith()}
          settings={settings([{ ...CONFIGURED[0], name: "Work DOIs" }])}
          onUpdate={onUpdate}
        />,
      );
      await screen.findByText("Work DOIs");

      fireEvent.click(screen.getByText("Edit"));
      fireEvent.click(screen.getByText("Read manifest"));
      await screen.findByLabelText("Example query");
      fireEvent.click(screen.getByText("Save and enable"));

      await waitFor(() => {
        expect(onUpdate).toHaveBeenCalledWith({
          integrations: expect.objectContaining({
            custom: [expect.objectContaining({ name: "Work DOIs" })],
          }),
        });
      });
    });
  });

  it("labels the full request error and the redacted request URL separately", async () => {
    const api = apiWith({
      customIntegrationProbe: vi.fn().mockResolvedValue({
        ...CLEAN_PROBE,
        request_url:
          "https://annas-archive.gl/search?q=graph%20neural%20networks&key=***",
        raw_response: "",
        results: [],
        ok: false,
        error:
          "External request failed: error sending request for url (<request URL>)\nCaused by: dns error\nCaused by: host not found",
      }),
    });
    render(
      <CustomIntegrations api={api} settings={settings()} onUpdate={vi.fn()} />,
    );

    fireEvent.click(screen.getByText("Add integration"));
    fireEvent.click(screen.getByText("Read manifest"));
    await waitFor(() => expect(screen.getByText("Probe")).toBeInTheDocument());
    fireEvent.click(screen.getByText("Probe"));

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("Error");
    expect(alert).toHaveTextContent("dns error");
    expect(alert).toHaveTextContent("host not found");
    expect(screen.getByText("Request URL")).toBeInTheDocument();
    expect(
      screen.getByText(
        "https://annas-archive.gl/search?q=graph%20neural%20networks&key=***",
      ),
    ).toBeInTheDocument();
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
        origins: [],
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
        origins: ["https://api.crossref.org"],
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
