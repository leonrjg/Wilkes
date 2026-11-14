/** Where an edited copy of a document is offered to be saved: beside the
 *  original, under a name that says what it is. Never the original's own path
 *  -- an edited copy is written from the original, and must not replace it. */
export function editedCopyPath(source: string): string {
  const separator = Math.max(source.lastIndexOf("/"), source.lastIndexOf("\\"));
  const directory = source.slice(0, separator + 1);
  const name = source.slice(separator + 1);
  const dot = name.lastIndexOf(".");
  const stem = dot > 0 ? name.slice(0, dot) : name;
  const extension = dot > 0 ? name.slice(dot) : ".pdf";
  return `${directory}${stem} (edited)${extension}`;
}

export function fileNameOf(path: string): string {
  return path.slice(Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\")) + 1);
}
