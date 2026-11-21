import { fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import ApiLoginGate from "./ApiLoginGate";
import { checkApiSession, loginToApi } from "../services/http";

vi.mock("../services", () => ({ isTauri: false }));
vi.mock("../services/http", () => ({
  checkApiSession: vi.fn(),
  loginToApi: vi.fn(),
}));

describe("ApiLoginGate", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("mounts the application at once when no token is required", async () => {
    vi.mocked(checkApiSession).mockResolvedValue(true);

    render(<ApiLoginGate><div>Full application</div></ApiLoginGate>);

    expect(await screen.findByText("Full application")).toBeInTheDocument();
  });

  it("asks for the token, refuses a wrong one, and opens on the right one", async () => {
    vi.mocked(checkApiSession).mockResolvedValue(false);
    vi.mocked(loginToApi)
      .mockRejectedValueOnce(new Error("Sign in failed: That is not this server's token"))
      .mockResolvedValueOnce(undefined);

    render(<ApiLoginGate><div>Full application</div></ApiLoginGate>);

    const input = await screen.findByLabelText("API token");
    expect(screen.queryByText("Full application")).not.toBeInTheDocument();

    fireEvent.change(input, { target: { value: "guess" } });
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("not this server's token");
    expect(screen.queryByText("Full application")).not.toBeInTheDocument();

    fireEvent.change(input, { target: { value: " secret " } });
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    expect(await screen.findByText("Full application")).toBeInTheDocument();
    expect(loginToApi).toHaveBeenLastCalledWith("secret");
  });

  it("says why when the server cannot be asked", async () => {
    vi.mocked(checkApiSession).mockRejectedValue(new Error("Check the session failed: 502"));

    render(<ApiLoginGate><div>Full application</div></ApiLoginGate>);

    expect(await screen.findByRole("alert")).toHaveTextContent("502");
    expect(screen.queryByText("Full application")).not.toBeInTheDocument();
  });
});
