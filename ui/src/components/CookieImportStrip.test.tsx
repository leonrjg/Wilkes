import { act, fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../services", () => ({
  isTauri: true,
  api: {
    browserCookieSources: vi.fn(() =>
      Promise.resolve([
        { id: "chrome", name: "Chrome" },
        { id: "firefox", name: "Firefox" },
      ]),
    ),
    browserPaneImportCookies: vi.fn(() => Promise.resolve(3)),
  },
}));

import { api } from "../services";
import CookieImportStrip, { siteOf } from "./CookieImportStrip";

async function renderStrip(onImported = vi.fn()) {
  await act(async () => {
    render(
      <CookieImportStrip url="https://www.sciencedirect.com/science/article/1" onImported={onImported} onClose={vi.fn()} />,
    );
  });
  return onImported;
}

describe("CookieImportStrip", () => {
  beforeEach(() => vi.clearAllMocks());

  it("starts from the page's site and the first browser", async () => {
    await renderStrip();
    expect(screen.getByLabelText("Site to import cookies for")).toHaveValue("sciencedirect.com");
    expect(screen.getByLabelText("Browser to import cookies from")).toHaveValue("chrome");
  });

  it("imports the chosen browser's cookies for the named site, then reloads the page", async () => {
    const onImported = await renderStrip();
    fireEvent.change(screen.getByLabelText("Browser to import cookies from"), { target: { value: "firefox" } });
    fireEvent.change(screen.getByLabelText("Site to import cookies for"), { target: { value: "elsevier.com" } });
    await act(async () => {
      fireEvent.click(screen.getByText("Import"));
    });
    expect(api.browserPaneImportCookies).toHaveBeenCalledWith("firefox", "elsevier.com");
    expect(screen.getByRole("status")).toHaveTextContent("Imported 3 cookies for elsevier.com from Firefox.");
    expect(onImported).toHaveBeenCalled();
  });

  it("says so when the browser has none, without reloading", async () => {
    vi.mocked(api.browserPaneImportCookies!).mockResolvedValueOnce(0);
    const onImported = await renderStrip();
    await act(async () => {
      fireEvent.click(screen.getByText("Import"));
    });
    expect(screen.getByRole("status")).toHaveTextContent("Chrome has no cookies for sciencedirect.com.");
    expect(onImported).not.toHaveBeenCalled();
  });

  it("shows why an import failed", async () => {
    vi.mocked(api.browserPaneImportCookies!).mockRejectedValueOnce("Could not read Chrome's cookies: denied");
    await renderStrip();
    await act(async () => {
      fireEvent.click(screen.getByText("Import"));
    });
    expect(screen.getByRole("alert")).toHaveTextContent("Could not read Chrome's cookies: denied");
  });

  it("reads a page's site from its address", () => {
    expect(siteOf("https://www.example.org/a?b")).toBe("example.org");
    expect(siteOf("https://id.elsevier.com/x")).toBe("id.elsevier.com");
    expect(siteOf(null)).toBe("");
  });
});
