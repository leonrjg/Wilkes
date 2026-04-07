import { render, screen, fireEvent, act } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import UploadZone from "./UploadZone";
import { useSettingsStore } from "../stores/useSettingsStore";

describe("UploadZone", () => {
  const mockSource = {
    deleteFile: vi.fn(),
    deleteAll: vi.fn(),
    type: "web",
  } as any;

  const defaultProps = {
    source: mockSource,
    onRootChange: vi.fn(),
  };

  let xhrMock: any;

  beforeEach(() => {
    vi.clearAllMocks();
    useSettingsStore.setState({ fileList: [], refreshFileList: vi.fn() } as any);
    // Mock XMLHttpRequest
    xhrMock = {
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
    vi.stubGlobal("XMLHttpRequest", vi.fn(function(this: any) {
      return xhrMock;
    }));
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
  });

  it("handles file selection and triggers upload success", async () => {
    render(<UploadZone {...defaultProps} />);
    const inputs = document.querySelectorAll('input[type="file"]');
    const fileInputReal = inputs[0];

    const file = new File(["test"], "test.txt", { type: "text/plain" });

    // Capture the load event listener
    let loadListener: Function | undefined;
    xhrMock.addEventListener.mockImplementation((event: string, listener: Function) => {
      if (event === "load") loadListener = listener;
    });

    await act(async () => {
      fireEvent.change(fileInputReal, { target: { files: [file] } });
    });

    expect(xhrMock.open).toHaveBeenCalledWith("POST", "/api/upload");
    expect(xhrMock.send).toHaveBeenCalledWith(expect.any(FormData));

    // Simulate upload success
    await act(async () => {
      if (loadListener) loadListener();
    });

    expect(defaultProps.onRootChange).toHaveBeenCalledWith("/new/root");
  });

  it("handles upload progress", async () => {
    render(<UploadZone {...defaultProps} />);
    const inputs = document.querySelectorAll('input[type="file"]');
    const fileInputReal = inputs[0];
    const file = new File(["test"], "test.txt");

    let progressListener: Function | undefined;
    xhrMock.upload.addEventListener.mockImplementation((event: string, listener: Function) => {
      if (event === "progress") progressListener = listener;
    });

    await act(async () => {
      fireEvent.change(fileInputReal, { target: { files: [file] } });
    });

    // Simulate progress
    await act(async () => {
      if (progressListener) {
        progressListener({ lengthComputable: true, loaded: 50, total: 100 });
      }
    });

    expect(screen.getByText("Uploading 50%…")).toBeInTheDocument();
  });

  it("handles upload failure", async () => {
    render(<UploadZone {...defaultProps} />);
    const inputs = document.querySelectorAll('input[type="file"]');
    const fileInputReal = inputs[0];
    const file = new File(["test"], "test.txt");

    let loadListener: Function | undefined;
    xhrMock.addEventListener.mockImplementation((event: string, listener: Function) => {
      if (event === "load") loadListener = listener;
    });

    await act(async () => {
      fireEvent.change(fileInputReal, { target: { files: [file] } });
    });

    // Simulate upload failure
    xhrMock.status = 500;
    await act(async () => {
      if (loadListener) loadListener();
    });

    expect(screen.getByText("Upload failed: 500")).toBeInTheDocument();
  });

  it("handles upload network error", async () => {
    render(<UploadZone {...defaultProps} />);
    const inputs = document.querySelectorAll('input[type="file"]');
    const fileInputReal = inputs[0];
    const file = new File(["test"], "test.txt");

    let errorListener: Function | undefined;
    xhrMock.addEventListener.mockImplementation((event: string, listener: Function) => {
      if (event === "error") errorListener = listener;
    });

    await act(async () => {
      fireEvent.change(fileInputReal, { target: { files: [file] } });
    });

    // Simulate network error
    await act(async () => {
      if (errorListener) errorListener();
    });

    expect(screen.getByText("Upload network error")).toBeInTheDocument();
  });

  it("calls deleteAll when Clear all is clicked", async () => {
    useSettingsStore.setState({
      fileList: [{ path: "test.txt", size_bytes: 10, file_type: "PlainText", extension: "txt" }],
      refreshFileList: vi.fn(),
    } as any);
    mockSource.deleteAll.mockResolvedValue(undefined);

    render(<UploadZone {...defaultProps} root="/some/root" />);

    const clearAll = screen.getByText("Clear all");
    await act(async () => { fireEvent.click(clearAll); });

    expect(mockSource.deleteAll).toHaveBeenCalled();
    expect(useSettingsStore.getState().refreshFileList).toHaveBeenCalled();
  });

  it("calls deleteFile when remove button is clicked", async () => {
    const file = { path: "test.txt", size_bytes: 10, file_type: "PlainText", extension: "txt" };
    useSettingsStore.setState({ fileList: [file], refreshFileList: vi.fn() } as any);
    mockSource.deleteFile.mockResolvedValue(undefined);

    render(<UploadZone {...defaultProps} root="/some/root" />);

    const removeBtn = screen.getByTitle("Remove file");
    await act(async () => { fireEvent.click(removeBtn); });

    expect(mockSource.deleteFile).toHaveBeenCalledWith("test.txt");
    expect(useSettingsStore.getState().refreshFileList).toHaveBeenCalled();
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
