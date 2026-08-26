import { useSyncExternalStore, type ReactElement, type ReactNode } from "react";
import { act, render as rtlRender, type RenderOptions } from "@testing-library/react";
import { vi } from "vitest";
import { ReaderHostProvider, type ReaderHostServices } from "../ReaderHost";
import type { DocumentSelection } from "../selection";
import type { SelectionActionsSlot } from "../slots";

/** The one capability the readers ask a host to perform. */
export const openExternalSpy = vi.fn();

const DEFAULT_HOST: ReaderHostServices = {
  openExternal: openExternalSpy,
  colorScheme: "light",
};

/**
 * Render a reader with a host, and let the test change that host afterwards --
 * appearance is something a real host changes at runtime, so a reader's
 * response to it has to be testable without re-mounting.
 */
export function renderWithReaderHost(
  ui: ReactElement,
  options: Omit<RenderOptions, "wrapper"> & { host?: Partial<ReaderHostServices> } = {},
) {
  const { host: initialHost, ...renderOptions } = options;
  const listeners = new Set<() => void>();
  let value: ReaderHostServices = { ...DEFAULT_HOST, ...initialHost };

  function Wrapper({ children }: { children: ReactNode }) {
    const host = useSyncExternalStore(
      (onChange) => {
        listeners.add(onChange);
        return () => listeners.delete(onChange);
      },
      () => value,
    );
    return <ReaderHostProvider value={host}>{children}</ReaderHostProvider>;
  }

  const result = rtlRender(ui, { wrapper: Wrapper, ...renderOptions });
  return {
    ...result,
    setHost(patch: Partial<ReaderHostServices>) {
      value = { ...value, ...patch };
      act(() => {
        listeners.forEach((onChange) => onChange());
      });
    },
  };
}

/**
 * Stand-in host chrome for the selection slot. Deliberately not any real
 * application's menu: what a reader owes the slot is a position, a selection
 * and a working pin, and those are what this exercises.
 */
export function stubSelectionSlot(
  handlers: { onAction?: (selection: DocumentSelection) => void } = {},
): SelectionActionsSlot {
  return (selection, api) => (
    <div>
      <button
        type="button"
        onClick={() => {
          handlers.onAction?.(selection);
          api.clear();
          api.dismiss();
        }}
      >
        Stub action
      </button>
      <input
        placeholder="Stub input"
        onFocus={() => api.setPinned(true)}
        onBlur={() => api.setPinned(false)}
      />
    </div>
  );
}
