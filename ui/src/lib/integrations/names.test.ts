import { describe, expect, it } from "vitest";
import { sourceLabel } from "./names";
import type { LiteratureProviderInfo } from "../types";

const provider = (id: string, name: string): LiteratureProviderInfo => ({
  provider: id,
  name,
  enabled: true,
  builtin: id !== "custom:crossref",
  base_url: "https://example.test",
  default_base_url: null,
  capabilities: { browse: null, lookup: true, references: false, resolve_download: false },
  secrets: [],
});

describe("sourceLabel", () => {
  const providers = [provider("openalex", "OpenAlex"), provider("custom:crossref", "Crossref")];

  it("names the two sources that are not providers", () => {
    expect(sourceLabel("file", providers)).toBe("File");
    expect(sourceLabel("zotero", providers)).toBe("Zotero");
  });

  it("names a provider by what the registry says it is called", () => {
    expect(sourceLabel("openalex", providers)).toBe("OpenAlex");
    expect(sourceLabel("custom:crossref", providers)).toBe("Crossref");
  });

  it("shows a source the registry does not know as the id it was written under", () => {
    expect(sourceLabel("custom:removed", providers)).toBe("custom:removed");
    expect(sourceLabel("openalex", [])).toBe("openalex");
  });
});
