import { act, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { AnnotationMode } from "pdfjs-dist";
import PdfPageCanvas from "./PdfPageCanvas";

/**
 * These tests exist because this component replaced react-pdf's `<Page>`, and
 * the bar for that replacement was "the reader renders exactly as it did".
 * Each case pins one thing that would be silently lost in a tidy-up.
 */

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  // An unhandled rejection here is the component's job to catch, not the test's.
  promise.catch(() => {});
  return { promise, resolve, reject };
}

function makePage({ width = 600, height = 800, rotate = 0 } = {}) {
  const render = deferred<void>();
  const cancel = vi.fn();
  const page = {
    rotate,
    cleanup: vi.fn(),
    getViewport: vi.fn(({ scale, rotation }: { scale: number; rotation?: number }) => ({
      width: width * scale,
      height: height * scale,
      scale,
      rotation,
    })),
    render: vi.fn(() => ({ promise: render.promise, cancel })),
  };
  return { page, render, cancel };
}

function makePdf(page: unknown) {
  return { getPage: vi.fn(async () => page) } as never;
}

const originalGetContext = HTMLCanvasElement.prototype.getContext;
const originalRatio = window.devicePixelRatio;
let contextOptions: unknown;

beforeEach(() => {
  contextOptions = undefined;
  HTMLCanvasElement.prototype.getContext = vi.fn((_type: string, options?: unknown) => {
    contextOptions = options;
    return {} as never;
  }) as never;
  Object.defineProperty(window, "devicePixelRatio", { configurable: true, value: 2 });
});

afterEach(() => {
  HTMLCanvasElement.prototype.getContext = originalGetContext;
  Object.defineProperty(window, "devicePixelRatio", { configurable: true, value: originalRatio });
});

describe("PdfPageCanvas", () => {
  it("draws at device resolution and sizes the CSS box at the requested width", async () => {
    const { page } = makePage();
    render(<PdfPageCanvas pdf={makePdf(page)} pageNumber={1} width={300} />);

    await waitFor(() => expect(page.render).toHaveBeenCalled());

    const element = document.querySelector("canvas")!;
    // scale = 300/600 = 0.5, so the CSS box is the requested 300px wide; the
    // backing store is that again times the 2x ratio.
    expect(element.style.width).toBe("300px");
    expect(element.style.height).toBe("400px");
    expect(element.width).toBe(600);
    expect(element.height).toBe(800);
  });

  it("does not cap the device pixel ratio", async () => {
    Object.defineProperty(window, "devicePixelRatio", { configurable: true, value: 3 });
    const { page } = makePage();
    render(<PdfPageCanvas pdf={makePdf(page)} pageNumber={1} width={600} />);

    await waitFor(() => expect(page.render).toHaveBeenCalled());
    // A cap at 2 would soften every page on a 3x display.
    expect(document.querySelector("canvas")!.width).toBe(1800);
  });

  it("renders opaque, with annotations composited onto the canvas", async () => {
    const { page } = makePage();
    render(
      <PdfPageCanvas pdf={makePdf(page)} pageNumber={1} width={600} canvasBackground="white" />,
    );

    await waitFor(() => expect(page.render).toHaveBeenCalled());
    expect(contextOptions).toEqual({ alpha: false });
    const renderArgs = page.render.mock.calls[0][0] as Record<string, unknown>;
    expect(renderArgs.background).toBe("white");
    // The reader draws its own link layer, but annotation *appearances* must
    // still be painted; DISABLE would drop stamps and highlights.
    expect(renderArgs.annotationMode).toBe(AnnotationMode.ENABLE);
    expect(page.cleanup).toHaveBeenCalled();
  });

  it("keeps the canvas hidden until the page has painted", async () => {
    const { page, render: renderTask } = makePage();
    const onRenderSuccess = vi.fn();
    render(
      <PdfPageCanvas
        pdf={makePdf(page)}
        pageNumber={1}
        width={600}
        onRenderSuccess={onRenderSuccess}
      />,
    );

    await waitFor(() => expect(page.render).toHaveBeenCalled());
    expect(document.querySelector("canvas")!.style.visibility).toBe("hidden");
    expect(onRenderSuccess).not.toHaveBeenCalled();

    await act(async () => {
      renderTask.resolve();
    });
    expect(document.querySelector("canvas")!.style.visibility).toBe("");
    expect(onRenderSuccess).toHaveBeenCalledTimes(1);
  });

  it("cancels an in-flight render when the scale changes", async () => {
    const { page, cancel } = makePage();
    const pdf = makePdf(page);
    const { rerender } = render(<PdfPageCanvas pdf={pdf} pageNumber={1} width={600} />);
    await waitFor(() => expect(page.render).toHaveBeenCalledTimes(1));

    rerender(<PdfPageCanvas pdf={pdf} pageNumber={1} width={900} />);
    await waitFor(() => expect(page.render).toHaveBeenCalledTimes(2));
    expect(cancel).toHaveBeenCalled();
  });

  it("treats a cancelled render as normal, not as a failure", async () => {
    const { page, render: renderTask } = makePage();
    const onRenderError = vi.fn();
    const errorSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    render(
      <PdfPageCanvas
        pdf={makePdf(page)}
        pageNumber={1}
        width={600}
        onRenderError={onRenderError}
      />,
    );
    await waitFor(() => expect(page.render).toHaveBeenCalled());

    await act(async () => {
      renderTask.reject(Object.assign(new Error("cancelled"), {
        name: "RenderingCancelledException",
      }));
    });

    expect(onRenderError).not.toHaveBeenCalled();
    expect(errorSpy).not.toHaveBeenCalled();
    errorSpy.mockRestore();
  });

  it("releases the backing surface when the canvas goes away", async () => {
    const { page } = makePage();
    const { unmount } = render(<PdfPageCanvas pdf={makePdf(page)} pageNumber={1} width={600} />);
    await waitFor(() => expect(page.render).toHaveBeenCalled());
    const element = document.querySelector("canvas")!;
    expect(element.width).toBeGreaterThan(0);

    unmount();

    // Without this a document scrolled end to end holds every canvas it ever
    // painted. Reading the ref in the cleanup instead of capturing the element
    // here would find null under React 19 and quietly release nothing.
    expect(element.width).toBe(0);
    expect(element.height).toBe(0);
  });

  it("cancels an in-flight render when the page is unmounted", async () => {
    const { page, cancel } = makePage();
    const { unmount } = render(<PdfPageCanvas pdf={makePdf(page)} pageNumber={1} width={600} />);
    await waitFor(() => expect(page.render).toHaveBeenCalled());

    unmount();

    // Scrolling a half-painted page out of view must stop the work, not leave
    // the worker rendering into a detached canvas.
    expect(cancel).toHaveBeenCalled();
  });

  it("carries the class the page styling hooks onto", async () => {
    const { page } = makePage();
    render(<PdfPageCanvas pdf={makePdf(page)} pageNumber={1} width={600} />);
    await waitFor(() => expect(page.render).toHaveBeenCalled());

    // `.pdf-page canvas` carries the sheet-of-paper white and drop shadow, and
    // `.pdf-dark-mode .pdf-page canvas` the dark-mode inversion. Renaming or
    // dropping this class silently removes both, which no canvas-level
    // assertion would notice.
    const wrapper = document.querySelector(".pdf-page")!;
    expect(wrapper.querySelector("canvas")).toBeInTheDocument();
    // Identity belongs to the reader's virtualized wrapper, which is where
    // every geometry lookup resolves; a second copy here would give
    // `querySelectorAll("[data-page-number]")` two candidates per page.
    expect(wrapper).not.toHaveAttribute("data-page-number");
    // The wrapper itself stays transparent; the white belongs to the canvas.
    expect((wrapper as HTMLElement).style.backgroundColor).toBe("");
  });

  it("shows the loading message until the page proxy resolves", async () => {
    const pageProxy = deferred<unknown>();
    const pdf = { getPage: vi.fn(() => pageProxy.promise) } as never;
    render(<PdfPageCanvas pdf={pdf} pageNumber={1} width={600} />);

    expect(screen.getByText("Loading page…")).toBeInTheDocument();

    const { page } = makePage();
    await act(async () => {
      pageProxy.resolve(page);
    });
    expect(screen.queryByText("Loading page…")).not.toBeInTheDocument();
  });
});
