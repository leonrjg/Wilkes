import { useMemo, type ReactElement, type ReactNode } from "react";
import { render as rtlRender, type RenderOptions } from "@testing-library/react";
import { vi } from "vitest";
import { ReaderHostProvider } from "../components/preview/ReaderHost";
import SelectionActions from "../components/SelectionActions";
import type { DocumentSelection } from "../components/preview/selection";
import type { SelectionActionsSlot } from "../components/preview/slots";
import { useSettingsStore } from "../stores/useSettingsStore";

/** Spy for the one capability the readers ask the host for. */
export const openExternalSpy = vi.fn();

/**
 * Mirrors what PreviewPane provides, so a reader under test sees the same host
 * the application gives it -- including reading the auto-zoom target from the
 * settings store, which several tests drive with `useSettingsStore.setState`.
 */
function ReaderTestHost({ children }: { children: ReactNode }) {
  const pdfAutoZoomTargetPx = useSettingsStore(
    (state) => state.settings?.pdf_auto_zoom_target_px,
  );
  const colorScheme = useSettingsStore((state) => state.colorScheme);
  const value = useMemo(
    () => ({ openExternal: openExternalSpy, colorScheme, pdfAutoZoomTargetPx }),
    [colorScheme, pdfAutoZoomTargetPx],
  );
  return <ReaderHostProvider value={value}>{children}</ReaderHostProvider>;
}

export function renderWithReaderHost(ui: ReactElement, options?: Omit<RenderOptions, "wrapper">) {
  return rtlRender(ui, { wrapper: ReaderTestHost, ...options });
}

/** Wilkes' selection chrome as a slot, matching how PreviewPane passes it. */
export function selectionSlot(handlers: {
  onAddBookmark?: (selection: DocumentSelection) => void;
  showChatActions?: boolean;
  onExplain?: (selection: DocumentSelection) => void;
  onAsk?: (selection: DocumentSelection, question: string) => void;
}): SelectionActionsSlot {
  return (selection, api) => (
    <SelectionActions selection={selection} api={api} {...handlers} />
  );
}
