/** What a link or image in the document points at, in the terms the host can
 *  act on. */
export type Reference =
  | { kind: "web"; url: string }
  | { kind: "data"; url: string }
  | { kind: "local"; path: string }
  | { kind: "other"; url: string };

function isAbsoluteReference(reference: string): boolean {
  return reference.startsWith("//") || /^[a-z][a-z\d+\-.]*:/i.test(reference);
}

/** Resolve a document-relative reference to a path beside the document.
 *
 *  DUPLICATED from `resolveDocumentRelativePath` in @leonrjg/wilkes-reader
 *  (htmlDocument.js), which the package does not export. Same rules: resolved
 *  through the URL parser so `..`, `.`, percent-escapes, queries and fragments
 *  behave; a reference that names a host is not a sibling file. Remove this
 *  once the reader exports its own. */
function resolveDocumentRelativePath(documentPath: string, reference: string): string | null {
  const windows = documentPath.includes("\\") && !documentPath.includes("/");
  const base = "file:///" + documentPath
    .split(/[/\\]/)
    .filter(Boolean)
    .map(encodeURIComponent)
    .join("/");
  let resolved: URL;
  try {
    resolved = new URL(reference, base);
  } catch {
    return null;
  }
  if (resolved.protocol !== "file:" || resolved.host) return null;
  let path: string;
  try {
    path = decodeURIComponent(resolved.pathname);
  } catch {
    return null;
  }
  if (!path || path === "/") return null;
  return windows ? path.replace(/^\//, "").replace(/\//g, "\\") : path;
}

export function classifyReference(documentPath: string, reference: string): Reference | null {
  const trimmed = reference.trim().replace(/^<(.*)>$/, "$1");
  if (!trimmed) return null;
  if (/^https?:\/\//i.test(trimmed)) return { kind: "web", url: trimmed };
  if (/^data:/i.test(trimmed)) return { kind: "data", url: trimmed };
  if (isAbsoluteReference(trimmed)) return { kind: "other", url: trimmed };
  const path = resolveDocumentRelativePath(documentPath, trimmed);
  return path ? { kind: "local", path } : null;
}
