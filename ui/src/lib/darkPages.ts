import type { Settings } from "./types";

type DarkPageSettings = Partial<
  Pick<Settings, "dark_mode_reading" | "dark_mode_inversion" | "dark_mode_dimming">
>;

/**
 * The CSS filter a light page is darkened with in dark mode: the reader's pages
 * (`--pdf-dark-filter`) and the browser pane's pages alike, so both darken the
 * same way. Inverting turns white to near-black; dimming keeps the page's
 * colours and takes away the share of its brightness the user chose.
 *
 * Each value is optional because a settings file written before it existed
 * does not carry it; the defaults are the ones the host fills in on read.
 */
export function darkPageFilter(settings: DarkPageSettings): string {
  if ((settings.dark_mode_reading ?? "Invert") === "Dim") {
    return `brightness(${(100 - (settings.dark_mode_dimming ?? 50)) / 100})`;
  }
  return `invert(${(settings.dark_mode_inversion ?? 90) / 100}) hue-rotate(180deg) brightness(1.1)`;
}
