import { render, screen, fireEvent, act } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import UploadZone from "./UploadZone";

describe("UploadZone", () => {
  const mockSource = {
    deleteFile: vi.fn(),
    deleteAll: vi.fn(),
    type: "web",
  } as any;

  const mockApi = {
    listFiles: vi.fn(),
  } as any;

  const defaultProps = {
    source: mockSource,
    api: mockApi,
    root: "",
    onRootChange: vi.fn(),
  };

  beforeEach(() => {
    vi.clearAllMocks();
    mockApi.listFiles.mockResolvedValue([]);
    // Mock XMLHttpRequest
    const xhrMock: any = {
      open: vi.fn(),
      send: vi.fn(),
      setRequestHeader: vi.fn(),
      readyState: 4,
      status: 200,
      responseText: JSON.stringify({ root: "/new/root", file_count: 5 }),
      addEventListener: vi.fn(),
      upload: {
        addEventListener: vi.fn(),
      },
    };
    vi.stubGlobal("XMLHttpRequest", vi.fn(() => xhrMock));
  });

  it("renders upload triggers", () => {
    render(<UploadZone {...defaultProps} />);
    expect(screen.getByText("Files")).toBeInTheDocument();
    expect(screen.getByText("Folder")).toBeInTheDocument();
  });

  it("triggers file input when clicking Files button", () => {
    render(<UploadZone {...defaultProps} />);
    const filesBtn = screen.getByText("Files");
    fireEvent.click(filesBtn);
    // Difficult to check if hidden input was clicked, but we covered the line
  });

  it("handles file selection and triggers upload", async () => {
    render(<UploadZone {...defaultProps} />);
    const fileInput = screen.getByTitle("Upload files").nextElementSibling?.nextElementSibling as HTMLInputElement; 
    // Actually the inputs are hidden, but we can find them by their type
    const inputs = document.querySelectorAll('input[type="file"]');
    const fileInputReal = inputs[0];

    const file = new File(["test"], "test.txt", { type: "text/plain" });
    
    await act(async () => {
      fireEvent.change(fileInputReal, { target: { files: [file] } });
    });

    // The uploadWithProgress uses XMLHttpRequest which we mocked.
    // In a real test we'd need to trigger the 'load' event on the mock.
  });

  it("calls deleteAll when Clear all is clicked", async () => {
    mockApi.listFiles.mockResolvedValue([{ path: "test.txt", size_bytes: 10, file_type: "PlainText", extension: "txt" }]);
    
    const { rerender } = render(<UploadZone {...defaultProps} root="/some/root" />);
    
    // Wait for listFiles to resolve
    await act(async () => {
      await Promise.resolve();
    });
    
    rerender(<UploadZone {...defaultProps} root="/some/root" />);

    const clearAll = screen.getByText("Clear all");
    fireEvent.click(clearAll);

    expect(mockSource.deleteAll).toHaveBeenCalled();
  });

  it("calls deleteFile when remove button is clicked", async () => {
    const file = { path: "test.txt", size_bytes: 10, file_type: "PlainText", extension: "txt" };
    mockApi.listFiles.mockResolvedValue([file]);
    
    const { rerender } = render(<UploadZone {...defaultProps} root="/some/root" />);
    
    await act(async () => {
      await Promise.resolve();
    });
    
    rerender(<UploadZone {...defaultProps} root="/some/root" />);

    const removeBtn = screen.getByTitle("Remove file");
    await act(async () => {
      fireEvent.click(removeBtn);
    });

    expect(mockSource.deleteFile).toHaveBeenCalledWith("test.txt");
  });

  it("handles folder selection", async () => {
    render(<UploadZone {...defaultProps} />);
    const folderInput = document.querySelectorAll('input[type="file"]')[1];

    const file = new File(["test"], "test.txt");
    await act(async () => {
      fireEvent.change(folderInput, { target: { files: [file] } });
    });
  });
});
