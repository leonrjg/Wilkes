interface ConfiguredRootSources {
  directory: string;
  favorites: string[];
  recentDirs: string[];
}

/**
 * Return every user-configured library root once, preserving nested roots and
 * the UI's established priority (favorites, recent roots, then active root).
 * Search coverage has different semantics and may collapse nested roots.
 */
export function configuredLibraryRoots({
  directory,
  favorites,
  recentDirs,
}: ConfiguredRootSources): string[] {
  const roots: string[] = [];
  const seen = new Set<string>();

  for (const root of [...favorites, ...recentDirs, directory]) {
    if (!root) continue;
    if (seen.has(root)) continue;
    seen.add(root);
    roots.push(root);
  }

  return roots;
}

/** Compare paths without treating a shared string prefix as containment. */
export function pathIsWithinRoot(path: string, root: string): boolean {
  const candidate = comparablePath(path);
  const container = comparablePath(root);
  if (candidate === container) return true;
  if (container.endsWith("/")) return candidate.startsWith(container);
  return candidate.startsWith(`${container}/`);
}

export function pathsEqual(left: string, right: string): boolean {
  return comparablePath(left) === comparablePath(right);
}

function comparablePath(path: string): string {
  let normalized = path.replace(/\\/g, "/");
  while (
    normalized.length > 1 &&
    normalized.endsWith("/") &&
    !/^[A-Za-z]:\/$/.test(normalized)
  ) {
    normalized = normalized.slice(0, -1);
  }
  return normalized;
}
