/**
 * The order of the panes docked beside the reader.
 *
 * `order` is the arrangement the user last made, as pane keys. Panes are
 * listed to these functions in their default order, which is where they sit
 * until the user moves one.
 */

/** The open panes in the order the user arranged them. A pane the
 *  arrangement does not name — one opened since, or never moved — goes beside
 *  the pane it follows by default, so a new chat opens next to the other
 *  chats, and with no arrangement at all every pane is where it always was. */
export function arrangePanes<T extends { key: string }>(panes: T[], order: string[]): T[] {
  const rank = new Map(order.map((key, index) => [key, index]));
  const arranged = panes
    .filter((pane) => rank.has(pane.key))
    .sort((a, b) => rank.get(a.key)! - rank.get(b.key)!);
  panes.forEach((pane, index) => {
    if (rank.has(pane.key)) return;
    let at = 0;
    for (let before = index - 1; before >= 0; before--) {
      const placed = arranged.indexOf(panes[before]);
      if (placed >= 0) {
        at = placed + 1;
        break;
      }
    }
    arranged.splice(at, 0, pane);
  });
  return arranged;
}

/** `keys` with `key` moved to `index` — its index once it stands there, which
 *  is also its index among the others before it moved. */
export function movePane(keys: string[], key: string, index: number): string[] {
  const others = keys.filter((other) => other !== key);
  const at = Math.max(0, Math.min(others.length, index));
  return [...others.slice(0, at), key, ...others.slice(at)];
}
