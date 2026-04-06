import { render, screen, fireEvent } from "@testing-library/react";
import { describe, it, expect, vi } from "vitest";
import ExtensionFilter from "./ExtensionFilter";

describe("ExtensionFilter", () => {
  const fileList = [
    { extension: "ts" },
    { extension: "ts" },
    { extension: "js" },
  ] as any[];

  it("renders extension buttons with counts", () => {
    render(<ExtensionFilter fileList={fileList} excluded={new Set()} onChange={vi.fn()} />);
    expect(screen.getByText(".ts (2)")).toBeInTheDocument();
    expect(screen.getByText(".js (1)")).toBeInTheDocument();
  });

  it("calls onChange when an extension is toggled", () => {
    const onChange = vi.fn();
    render(<ExtensionFilter fileList={fileList} excluded={new Set()} onChange={onChange} />);
    
    fireEvent.click(screen.getByText(".ts (2)"));
    expect(onChange).toHaveBeenCalledWith(new Set(["ts"]));
  });

  it("removes from excluded when toggled back", () => {
    const onChange = vi.fn();
    render(<ExtensionFilter fileList={fileList} excluded={new Set(["ts"])} onChange={onChange} />);
    
    fireEvent.click(screen.getByText(".ts (2)"));
    expect(onChange).toHaveBeenCalledWith(new Set([]));
  });

  it("renders nothing when no extensions", () => {
    const { container } = render(<ExtensionFilter fileList={[]} excluded={new Set()} onChange={vi.fn()} />);
    expect(container.firstChild).toBeNull();
  });
});
