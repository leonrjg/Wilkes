import { fireEvent } from "@testing-library/react";
import { vi } from "vitest";

/** jsdom has no pointer capture or layout hit testing. Event coordinates and
 * pointer identity are explicit so missing browser fields cannot pass a test. */
export function pointerEvent(
  element: Element | Window,
  type: string,
  fields: Partial<PointerEvent> = {},
) {
  const event = new MouseEvent(type, { bubbles: true, cancelable: true });
  Object.defineProperties(event, Object.fromEntries(Object.entries({
    pointerId: 1, isPrimary: true, button: 0, buttons: 1,
    clientX: 20, clientY: 20, ...fields,
  }).map(([key, value]) => [key, { value }])));
  fireEvent(element, event);
}

export function capturePointer(source: HTMLElement) {
  const held = new Set<number>();
  source.setPointerCapture = vi.fn((id: number) => { held.add(id); });
  source.hasPointerCapture = vi.fn((id: number) => held.has(id));
  source.releasePointerCapture = vi.fn((id: number) => { held.delete(id); });
}
