import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import IntegrationsPanel from "./IntegrationsPanel";
import type { Settings } from "../lib/types";

function settings(enabled = false): Settings {
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
    },
    supported_extensions: ["pdf"],
    max_results: 50,
    bookmarks_dock: "Right",
  };
}

describe("IntegrationsPanel", () => {
  it("enables Zotero only after a ready local API check", async () => {
    const api = {
      zoteroStatus: vi.fn().mockResolvedValue({
        id: "zotero",
        enabled: false,
        state: "ready",
        message: "Zotero local API is reachable.",
        version: "7.0",
      }),
    } as any;
    const onUpdate = vi.fn();

    render(<IntegrationsPanel api={api} settings={settings(false)} onUpdate={onUpdate} />);
    fireEvent.click(screen.getByLabelText("Enable Zotero integration"));

    await waitFor(() => {
      expect(onUpdate).toHaveBeenCalledWith({
        integrations: {
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
    const api = {
      zoteroStatus: vi.fn().mockResolvedValue({
        id: "zotero",
        enabled: false,
        state: "local_api_disabled",
        message: "Zotero is running, but the local API is disabled.",
        version: "7.0",
      }),
    } as any;
    const onUpdate = vi.fn();

    render(<IntegrationsPanel api={api} settings={settings(false)} onUpdate={onUpdate} />);
    fireEvent.click(screen.getByLabelText("Enable Zotero integration"));

    await waitFor(() => {
      expect(screen.getByText("Zotero is running, but the local API is disabled.")).toBeInTheDocument();
    });
    expect(onUpdate).not.toHaveBeenCalledWith(expect.objectContaining({
      integrations: expect.objectContaining({
        zotero: expect.objectContaining({ enabled: true }),
      }),
    }));
  });

  it("allows disabling Zotero without checking the local API", async () => {
    const api = { zoteroStatus: vi.fn() } as any;
    const onUpdate = vi.fn();

    render(<IntegrationsPanel api={api} settings={settings(true)} onUpdate={onUpdate} />);
    fireEvent.click(screen.getByLabelText("Enable Zotero integration"));

    await waitFor(() => {
      expect(onUpdate).toHaveBeenCalledWith({
        integrations: {
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

  it("enables Semantic Scholar only after a ready API check", async () => {
    const api = {
      semanticScholarStatus: vi.fn().mockResolvedValue({
        id: "semantic_scholar",
        enabled: true,
        state: "ready",
        message: "Semantic Scholar API is reachable.",
        version: null,
      }),
    } as any;
    const onUpdate = vi.fn();

    render(<IntegrationsPanel api={api} settings={settings(false)} onUpdate={onUpdate} />);
    fireEvent.click(screen.getByRole("tab", { name: /Semantic Scholar/ }));
    fireEvent.click(screen.getByLabelText("Enable Semantic Scholar integration"));

    await waitFor(() => {
      expect(api.semanticScholarStatus).toHaveBeenCalled();
    });
    expect(onUpdate).toHaveBeenCalledWith({
      integrations: {
        zotero: {
          enabled: false,
          base_url: "http://127.0.0.1:23119",
          citation_style: "chicago-note-bibliography",
        },
        semantic_scholar: {
          enabled: true,
          base_url: "https://api.semanticscholar.org",
          api_key: null,
        },
        openalex: {
          enabled: false,
          base_url: "https://api.openalex.org",
          email: null,
        },
      },
    });
  });

  it("keeps Semantic Scholar enabled when the status probe is rate limited", async () => {
    const api = {
      semanticScholarStatus: vi.fn().mockResolvedValue({
        id: "semantic_scholar",
        enabled: true,
        state: "rate_limited",
        message:
          "Semantic Scholar API is reachable, but the public rate limit is currently reached.",
        version: null,
      }),
    } as any;
    const onUpdate = vi.fn();

    render(<IntegrationsPanel api={api} settings={settings(false)} onUpdate={onUpdate} />);
    fireEvent.click(screen.getByRole("tab", { name: /Semantic Scholar/ }));
    fireEvent.click(screen.getByLabelText("Enable Semantic Scholar integration"));

    await waitFor(() => {
      expect(screen.getByText(/public rate limit/)).toBeInTheDocument();
    });
    expect(onUpdate).toHaveBeenCalledWith({
      integrations: {
        zotero: {
          enabled: false,
          base_url: "http://127.0.0.1:23119",
          citation_style: "chicago-note-bibliography",
        },
        semantic_scholar: {
          enabled: true,
          base_url: "https://api.semanticscholar.org",
          api_key: null,
        },
        openalex: {
          enabled: false,
          base_url: "https://api.openalex.org",
          email: null,
        },
      },
    });
    expect(onUpdate).not.toHaveBeenCalledWith(expect.objectContaining({
      integrations: expect.objectContaining({
        semantic_scholar: expect.objectContaining({ enabled: false }),
      }),
    }));
  });

  it("shows one provider at a time, and marks the ones that are on", () => {
    const enabled = settings(true);
    render(
      <IntegrationsPanel api={{} as never} settings={enabled} onUpdate={vi.fn()} />,
    );

    // Every provider is reachable, but only the selected one is rendered:
    // the panel's length is one provider, not the sum of all of them.
    expect(screen.getByRole("tab", { name: /Zotero/ })).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: /Semantic Scholar/ })).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: /OpenAlex/ })).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: /Custom/ })).toBeInTheDocument();

    expect(screen.getByLabelText("Enable Zotero integration")).toBeInTheDocument();
    expect(
      screen.queryByLabelText("Enable OpenAlex integration"),
    ).not.toBeInTheDocument();

    // Zotero is the enabled one in this fixture, so it alone is marked.
    expect(screen.getByRole("tab", { name: /Zotero/ })).toContainElement(
      screen.getByLabelText("enabled"),
    );
  });

  it("switches to the fields of whichever provider is selected", () => {
    render(
      <IntegrationsPanel
        api={{} as never}
        settings={settings(false)}
        onUpdate={vi.fn()}
      />,
    );

    // Zotero's citation-style select belongs to Zotero alone.
    expect(screen.getByLabelText("Citation style")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("tab", { name: /Semantic Scholar/ }));
    expect(screen.queryByLabelText("Citation style")).not.toBeInTheDocument();
    expect(screen.getByLabelText("API key")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("tab", { name: /OpenAlex/ }));
    expect(screen.queryByLabelText("API key")).not.toBeInTheDocument();
    expect(screen.getByLabelText("Email")).toBeInTheDocument();
  });

  it("gives manifest-defined providers their own tab", () => {
    render(
      <IntegrationsPanel
        api={{} as never}
        settings={settings(false)}
        onUpdate={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByRole("tab", { name: /Custom/ }));
    expect(screen.getByText("Add integration")).toBeInTheDocument();
    expect(
      screen.queryByLabelText("Enable Zotero integration"),
    ).not.toBeInTheDocument();
  });

  it("edits a field of the selected provider without touching the others", () => {
    const onUpdate = vi.fn();
    render(
      <IntegrationsPanel
        api={{} as never}
        settings={settings(false)}
        onUpdate={onUpdate}
      />,
    );

    fireEvent.click(screen.getByRole("tab", { name: /OpenAlex/ }));
    fireEvent.change(screen.getByLabelText("Email"), {
      target: { value: "team@example.com" },
    });

    expect(onUpdate).toHaveBeenCalledWith({
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
          email: "team@example.com",
        },
      },
    });
  });

  it("stores an emptied optional field as null, not an empty string", () => {
    const onUpdate = vi.fn();
    const withKey = settings(false);
    withKey.integrations.semantic_scholar.api_key = "secret";
    render(
      <IntegrationsPanel api={{} as never} settings={withKey} onUpdate={onUpdate} />,
    );

    fireEvent.click(screen.getByRole("tab", { name: /Semantic Scholar/ }));
    fireEvent.change(screen.getByLabelText("API key"), { target: { value: "" } });

    // `null`, not `""`: the backend's Option<String> means "no API key" and
    // "an API key that is the empty string" are different, and an empty box is
    // only ever the first.
    expect(
      onUpdate.mock.calls[0][0].integrations.semantic_scholar.api_key,
    ).toBeNull();
  });

  it("enables OpenAlex only after a ready API check", async () => {
    const api = {
      openAlexStatus: vi.fn().mockResolvedValue({
        id: "openalex",
        enabled: true,
        state: "ready",
        message: "OpenAlex API is reachable.",
        version: null,
      }),
    } as any;
    const onUpdate = vi.fn();

    render(<IntegrationsPanel api={api} settings={settings(false)} onUpdate={onUpdate} />);
    fireEvent.click(screen.getByRole("tab", { name: /OpenAlex/ }));
    fireEvent.click(screen.getByLabelText("Enable OpenAlex integration"));

    await waitFor(() => {
      expect(api.openAlexStatus).toHaveBeenCalled();
    });
    expect(onUpdate).toHaveBeenCalledWith({
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
          enabled: true,
          base_url: "https://api.openalex.org",
          email: null,
        },
      },
    });
  });
});
