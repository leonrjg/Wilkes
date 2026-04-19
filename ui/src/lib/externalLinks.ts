export interface ExternalLinks {
  doi: string;
  doiUrl: string;
  googleScholarUrl: string;
}

function normalizeDoi(rawDoi: string): string {
  return rawDoi
    .trim()
    .replace(/^https?:\/\/(?:dx\.)?doi\.org\//i, "")
    .replace(/^doi:\s*/i, "");
}

export function buildExternalLinks(rawDoi: string | null | undefined): ExternalLinks | null {
  if (!rawDoi) return null;

  const doi = normalizeDoi(rawDoi);
  if (!doi) return null;

  return {
    doi,
    doiUrl: `https://doi.org/${doi}`,
    googleScholarUrl: `https://scholar.google.com/scholar?q=${encodeURIComponent(doi)}`,
  };
}
