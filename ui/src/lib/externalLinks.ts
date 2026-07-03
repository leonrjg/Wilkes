export interface ExternalLinks {
  doi: string | null;
  doiUrl: string | null;
  googleScholarUrl: string;
}

function normalizeDoi(rawDoi: string): string {
  return rawDoi
    .trim()
    .replace(/^https?:\/\/(?:dx\.)?doi\.org\//i, "")
    .replace(/^doi:\s*/i, "");
}

export function buildExternalLinks(
  rawDoi: string | null | undefined,
  rawTitle: string | null | undefined,
): ExternalLinks | null {
  const doi = rawDoi ? normalizeDoi(rawDoi) : "";
  const title = rawTitle?.trim() ?? "";
  const googleScholarQuery = doi || title;
  if (!googleScholarQuery) return null;

  return {
    doi: doi || null,
    doiUrl: doi ? `https://doi.org/${doi}` : null,
    googleScholarUrl: `https://scholar.google.com/scholar?q=${encodeURIComponent(googleScholarQuery)}`,
  };
}
