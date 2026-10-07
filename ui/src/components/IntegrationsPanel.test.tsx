import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import IntegrationsPanel from "./IntegrationsPanel";
import type { LiteratureProviderInfo, Settings } from "../lib/types";

function settings(
  enabled = false,
  builtin: NonNullable<Settings["integrations"]["builtin"]> = {},
): Settings {
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
        enabled,
        base_url: "http://127.0.0.1:23119",
        citation_style: "chicago-note-bibliography",
      },
      builtin,
    },
    supported_extensions: ["pdf"],
    max_results: 50,
    bookmarks_dock: "Right",
  };
}

const NO_BROWSE = { browse: null, lookup: true, references: false, resolve_download: false };

/** What the backend reports of the two providers that ship with Wilkes: their
 *  names, their origins and the secrets their manifests name. */
const SEMANTIC_SCHOLAR: LiteratureProviderInfo = {
  provider: "semantic_scholar",
  name: "Semantic Scholar",
  enabled: false,
  builtin: true,
  base_url: "https://api.semanticscholar.org",
  default_base_url: "https://api.semanticscholar.org",
  capabilities: NO_BROWSE,
  secrets: [{ name: "api_key", label: "API key", kind: "password", optional: true }],
};

const OPENALEX: LiteratureProviderInfo = {
  provider: "openalex",
  name: "OpenAlex",
  enabled: false,
  builtin: true,
  base_url: "https://api.openalex.org",
  default_base_url: "https://api.openalex.org",
  capabilities: NO_BROWSE,
  secrets: [{ name: "email", label: "Email", kind: "email", optional: true }],
};

const CUSTOM: LiteratureProviderInfo = {
  provider: "custom:crossref",
  name: "Crossref",
  enabled: true,
  builtin: false,
  base_url: "https://api.crossref.org",
  default_base_url: null,
  capabilities: NO_BROWSE,
  secrets: [],
};

function apiWith(extra: Record<string, unknown> = {}) {
  return {
    literatureProviders: vi.fn().mockResolvedValue([SEMANTIC_SCHOLAR, OPENALEX, CUSTOM]),
    ...extra,
  } as any;
}

/** The panel loads the provider list before it can name their tabs. */
async function panel(
  api: any,
  config: Settings,
  onUpdate = vi.fn(),
): Promise<ReturnType<typeof vi.fn>> {
  render(<IntegrationsPanel api={api} settings={config} onUpdate={onUpdate} />);
  await screen.findByRole("tab", { name: /OpenAlex/ });
  return onUpdate;
}

describe("IntegrationsPanel", () => {
  it("enables Zotero only after a ready local API check", async () => {
    const api = apiWith({
      zoteroStatus: vi.fn().mockResolvedValue({
        id: "zotero",
        enabled: false,
        state: "ready",
        message: "Zotero local API is reachable.",
        version: "7.0",
      }),
    });
    const onUpdate = await panel(api, settings(false));
    fireEvent.click(screen.getByLabelText("Enable Zotero integration"));

    await waitFor(() => {
      expect(onUpdate).toHaveBeenCalledWith({
        integrations: {
          builtin: {},
          zotero: {
            enabled: true,
            base_url: "http://127.0.0.1:23119",
            citation_style: "chicago-note-bibliography",
          },
        },
      });
    });
  });

  it("does not enable Zotero when the local API is not ready", async () => {
    const api = apiWith({
      zoteroStatus: vi.fn().mockResolvedValue({
        id: "zotero",
        enabled: false,
        state: "local_api_disabled",
        message: "Zotero is running, but the local API is disabled.",
        version: "7.0",
      }),
    });
    const onUpdate = await panel(api, settings(false));
    fireEvent.click(screen.getByLabelText("Enable Zotero integration"));

    await waitFor(() => {
      expect(
        screen.getByText("Zotero is running, but the local API is disabled."),
      ).toBeInTheDocument();
    });
    expect(onUpdate).not.toHaveBeenCalledWith(
      expect.objectContaining({
        integrations: expect.objectContaining({
          zotero: expect.objectContaining({ enabled: true }),
        }),
      }),
    );
  });

  it("allows disabling Zotero without checking the local API", async () => {
    const api = apiWith({ zoteroStatus: vi.fn() });
    const onUpdate = await panel(api, settings(true));
    fireEvent.click(screen.getByLabelText("Enable Zotero integration"));

    await waitFor(() => {
      expect(onUpdate).toHaveBeenCalledWith({
        integrations: {
          builtin: {},
          zotero: {
            enabled: false,
            base_url: "http://127.0.0.1:23119",
            citation_style: "chicago-note-bibliography",
          },
        },
      });
    });
    expect(api.zoteroStatus).not.toHaveBeenCalled();
  });

  it("enables Semantic Scholar only after a ready API check, by asking for it by id", async () => {
    const api = apiWith({
      literatureProviderStatus: vi.fn().mockResolvedValue({
        id: "semantic_scholar",
        enabled: false,
        state: "ready",
        message: "Semantic Scholar is reachable.",
        version: null,
      }),
    });
    const onUpdate = await panel(api, settings(false));
    fireEvent.click(screen.getByRole("tab", { name: /Semantic Scholar/ }));
    fireEvent.click(screen.getByLabelText("Enable Semantic Scholar integration"));

    await waitFor(() => {
      expect(api.literatureProviderStatus).toHaveBeenCalledWith("semantic_scholar");
    });
    expect(onUpdate).toHaveBeenCalledWith({
      integrations: {
        zotero: expect.objectContaining({ enabled: false }),
        // Written under the provider's id, with only what the user chose.
        builtin: { semantic_scholar: { enabled: true } },
      },
    });
  });

  it("keeps Semantic Scholar enabled when the status probe is rate limited", async () => {
    const api = apiWith({
      literatureProviderStatus: vi.fn().mockResolvedValue({
        id: "semantic_scholar",
        enabled: false,
        state: "rate_limited",
        message:
          "Semantic Scholar is reachable, but the public rate limit is currently reached.",
        version: null,
      }),
    });
    const onUpdate = await panel(api, settings(false));
    fireEvent.click(screen.getByRole("tab", { name: /Semantic Scholar/ }));
    fireEvent.click(screen.getByLabelText("Enable Semantic Scholar integration"));

    await waitFor(() => {
      expect(screen.getByText(/public rate limit/)).toBeInTheDocument();
    });
    expect(onUpdate).toHaveBeenCalledWith({
      integrations: expect.objectContaining({
        builtin: { semantic_scholar: { enabled: true } },
      }),
    });
  });

  it("does not enable a provider whose status check cannot be completed", async () => {
    const api = apiWith({
      literatureProviderStatus: vi.fn().mockRejectedValue(new Error("no route to host")),
    });
    const onUpdate = await panel(api, settings(false));
    fireEvent.click(screen.getByRole("tab", { name: /OpenAlex/ }));
    fireEvent.click(screen.getByLabelText("Enable OpenAlex integration"));
    expect(await screen.findByText("no route to host")).toBeInTheDocument();
    expect(onUpdate).not.toHaveBeenCalled();
  });

  it("shows one provider at a time, and marks the ones that are on", async () => {
    await panel(
      apiWith(),
      settings(true, { openalex: { enabled: false } }),
    );

    // Every provider is reachable, but only the selected one is rendered:
    // the panel's length is one provider, not the sum of all of them.
    expect(screen.getByRole("tab", { name: /Zotero/ })).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: /Semantic Scholar/ })).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: /OpenAlex/ })).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: /Custom/ })).toBeInTheDocument();

    expect(screen.getByLabelText("Enable Zotero integration")).toBeInTheDocument();
    expect(screen.queryByLabelText("Enable OpenAlex integration")).not.toBeInTheDocument();

    // Zotero is the enabled one in this fixture, so it alone is marked.
    expect(screen.getByRole("tab", { name: /Zotero/ })).toContainElement(
      screen.getByLabelText("enabled"),
    );
  });

  it("marks a shipped provider that is on by what is stored, not by what the list says", async () => {
    await panel(apiWith(), settings(false, { openalex: { enabled: true } }));
    expect(screen.getByRole("tab", { name: /OpenAlex/ })).toContainElement(
      screen.getByLabelText("enabled"),
    );
    expect(screen.getByRole("tab", { name: /Zotero/ })).not.toContainElement(
      screen.queryByLabelText("enabled"),
    );
  });

  it("switches to the fields of whichever provider is selected", async () => {
    await panel(apiWith(), settings(false));

    // Zotero's citation-style select belongs to Zotero alone.
    expect(screen.getByLabelText("Citation style")).toBeInTheDocument();

    // Each provider's fields are what its manifest asks for.
    fireEvent.click(screen.getByRole("tab", { name: /Semantic Scholar/ }));
    expect(screen.queryByLabelText("Citation style")).not.toBeInTheDocument();
    expect(screen.getByLabelText("API key")).toBeInTheDocument();
    expect(screen.getByLabelText("API URL")).toHaveValue("https://api.semanticscholar.org");

    fireEvent.click(screen.getByRole("tab", { name: /OpenAlex/ }));
    expect(screen.queryByLabelText("API key")).not.toBeInTheDocument();
    expect(screen.getByLabelText("Email")).toHaveAttribute("type", "email");
    expect(screen.getByLabelText("API URL")).toHaveValue("https://api.openalex.org");
  });

  it("gives a provider the manifest adds a tab and fields with no change to the panel", async () => {
    const crossref: LiteratureProviderInfo = {
      ...OPENALEX,
      provider: "crossref",
      name: "Crossref",
      base_url: "https://api.crossref.org",
      default_base_url: "https://api.crossref.org",
      secrets: [
        { name: "plus_token", label: "Plus token", kind: "password", optional: true },
        { name: "polite_email", label: "Polite pool email", kind: "email", optional: true },
      ],
    };
    const api = apiWith({
      literatureProviders: vi.fn().mockResolvedValue([crossref]),
    });
    const onUpdate = vi.fn();
    render(<IntegrationsPanel api={api} settings={settings(false)} onUpdate={onUpdate} />);
    fireEvent.click(await screen.findByRole("tab", { name: /Crossref/ }));
    expect(screen.getByLabelText("Enable Crossref integration")).toBeInTheDocument();
    fireEvent.change(screen.getByLabelText("Plus token"), { target: { value: "t0k3n" } });
    expect(onUpdate).toHaveBeenCalledWith({
      integrations: expect.objectContaining({
        builtin: { crossref: { enabled: false, secrets: { plus_token: "t0k3n" } } },
      }),
    });
  });

  it("does not make a tab of a provider the user wrote", async () => {
    await panel(apiWith(), settings(false));
    expect(screen.queryByRole("tab", { name: /Crossref/ })).not.toBeInTheDocument();
  });

  it("says when the providers could not be listed, and still offers Zotero and Custom", async () => {
    const api = {
      literatureProviders: vi.fn().mockRejectedValue(new Error("no backend")),
    } as any;
    render(<IntegrationsPanel api={api} settings={settings(false)} onUpdate={vi.fn()} />);
    expect(await screen.findByText(/providers could not be listed: no backend/)).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: /Zotero/ })).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: /Custom/ })).toBeInTheDocument();
  });

  it("gives manifest-defined providers their own tab", async () => {
    await panel(apiWith(), settings(false));

    fireEvent.click(screen.getByRole("tab", { name: /Custom/ }));
    expect(screen.getByText("Add integration")).toBeInTheDocument();
    expect(screen.queryByLabelText("Enable Zotero integration")).not.toBeInTheDocument();
  });

  it("stores a secret under the name its manifest gives it, beside what else is stored", async () => {
    const onUpdate = await panel(
      apiWith(),
      settings(false, { openalex: { enabled: true, base_url: "https://mirror.example.test" } }),
    );

    fireEvent.click(screen.getByRole("tab", { name: /OpenAlex/ }));
    fireEvent.change(screen.getByLabelText("Email"), {
      target: { value: "team@example.com" },
    });

    expect(onUpdate).toHaveBeenCalledWith({
      integrations: {
        zotero: expect.objectContaining({ enabled: false }),
        builtin: {
          openalex: {
            enabled: true,
            base_url: "https://mirror.example.test",
            secrets: { email: "team@example.com" },
          },
        },
      },
    });
  });

  it("removes an emptied secret rather than storing an empty one", async () => {
    const onUpdate = await panel(
      apiWith(),
      settings(false, { semantic_scholar: { enabled: false, secrets: { api_key: "secret" } } }),
    );

    fireEvent.click(screen.getByRole("tab", { name: /Semantic Scholar/ }));
    expect(screen.getByLabelText("API key")).toHaveValue("secret");
    fireEvent.change(screen.getByLabelText("API key"), { target: { value: "" } });

    // Absent, not `""`: a blank value is not a credential, and sending one as
    // a header on every request would be the failure.
    expect(onUpdate.mock.calls[0][0].integrations.builtin.semantic_scholar).toEqual({
      enabled: false,
    });
  });

  it("stores an origin only when it differs from the shipped one", async () => {
    const onUpdate = await panel(apiWith(), settings(false));
    fireEvent.click(screen.getByRole("tab", { name: /OpenAlex/ }));

    fireEvent.change(screen.getByLabelText("API URL"), {
      target: { value: "https://mirror.example.test" },
    });
    expect(onUpdate.mock.calls[0][0].integrations.builtin.openalex).toEqual({
      enabled: false,
      base_url: "https://mirror.example.test",
    });
  });

  it("drops the override when the origin is typed back to the shipped one", async () => {
    const onUpdate = await panel(
      apiWith(),
      settings(false, { openalex: { enabled: false, base_url: "https://mirror.example.test" } }),
    );
    fireEvent.click(screen.getByRole("tab", { name: /OpenAlex/ }));
    expect(screen.getByLabelText("API URL")).toHaveValue("https://mirror.example.test");

    fireEvent.change(screen.getByLabelText("API URL"), {
      target: { value: "https://api.openalex.org" },
    });
    expect(onUpdate.mock.calls[0][0].integrations.builtin.openalex).toEqual({
      enabled: false,
    });
  });

  it("shows the shipped origin as the placeholder of an emptied one", async () => {
    await panel(
      apiWith(),
      settings(false, { openalex: { enabled: false, base_url: "" } }),
    );
    fireEvent.click(screen.getByRole("tab", { name: /OpenAlex/ }));
    const field = screen.getByLabelText("API URL");
    expect(field).toHaveValue("");
    expect(field).toHaveAttribute("placeholder", "https://api.openalex.org");
  });

  it("enables OpenAlex only after a ready API check", async () => {
    const api = apiWith({
      literatureProviderStatus: vi.fn().mockResolvedValue({
        id: "openalex",
        enabled: false,
        state: "ready",
        message: "OpenAlex is reachable.",
        version: null,
      }),
    });
    const onUpdate = await panel(api, settings(false));
    fireEvent.click(screen.getByRole("tab", { name: /OpenAlex/ }));
    fireEvent.click(screen.getByLabelText("Enable OpenAlex integration"));

    await waitFor(() => {
      expect(api.literatureProviderStatus).toHaveBeenCalledWith("openalex");
    });
    expect(onUpdate).toHaveBeenCalledWith({
      integrations: {
        zotero: expect.objectContaining({ enabled: false }),
        builtin: { openalex: { enabled: true } },
      },
    });
  });
});
