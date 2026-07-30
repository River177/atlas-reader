import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { DocumentSummary, ImportPdfResult } from "@atlas/contracts";

import type { PdfDropEvent } from "../bridge";
import type {
  OpenPdfViewerInput,
  PdfViewerModule,
  PdfViewerState,
} from "../features/reader/pdf-viewer-module";
import { testBridge as bridge } from "../test/bridge";
import { App } from "./App";

function document(overrides: Partial<DocumentSummary> = {}): DocumentSummary {
  return {
    id: "document-1",
    title: "Atlas Retrieval",
    authors: ["Ada Researcher"],
    pageCount: 12,
    fileName: "atlas-retrieval.pdf",
    sourceState: "available",
    lastOpenedAt: 1,
    ...overrides,
  };
}

class FakePdfViewer implements PdfViewerModule {
  input: OpenPdfViewerInput | undefined;
  state: PdfViewerState = {
    page: 1,
    pageCount: 12,
    scaleValue: "page-width",
    searchCurrent: 0,
    searchTotal: 0,
    loadingProgress: undefined,
  };
  position = {
    page: 1,
    pageOffsetRatio: 0,
    scaleValue: "page-width",
  };

  open = vi.fn(async (input: OpenPdfViewerInput) => {
    this.input = input;
    this.position = {
      page: input.initialPosition.page,
      pageOffsetRatio: input.initialPosition.pageOffsetRatio,
      scaleValue: input.initialPosition.scaleValue,
    };
    this.state = {
      ...this.state,
      page: input.initialPosition.page,
      scaleValue: input.initialPosition.scaleValue,
    };
    input.onStateChange(this.state);
  });

  setPage = vi.fn((page: number) => {
    this.state = { ...this.state, page };
    this.position = { ...this.position, page };
    this.input?.onStateChange(this.state);
  });

  zoomIn = vi.fn();
  zoomOut = vi.fn();
  setScale = vi.fn((scaleValue: string) => {
    this.state = { ...this.state, scaleValue };
    this.position = { ...this.position, scaleValue };
    this.input?.onStateChange(this.state);
  });
  search = vi.fn();
  findNext = vi.fn();
  findPrevious = vi.fn();
  currentPosition = vi.fn(() => this.position);
  close = vi.fn(async () => undefined);

  emitPosition(position: typeof this.position) {
    this.position = position;
    this.input?.onPositionChange(position);
  }
}

describe("App", () => {
  it("refreshes local sources before loading the library", async () => {
    const testBridge = bridge();

    render(<App bridge={testBridge} />);

    expect(await screen.findByText("Your research library is empty")).toBeInTheDocument();
    expect(testBridge.refreshLibrarySources).toHaveBeenCalledOnce();
    expect(testBridge.queryLibrary).toHaveBeenCalledWith({
      sort: "recent",
      limit: 100,
    });
  });

  it("imports PDFs selected by the native picker and refreshes the list", async () => {
    const testBridge = bridge();
    const imported = document();
    vi.mocked(testBridge.pickPdfPaths).mockResolvedValue(["/papers/atlas.pdf"]);
    vi.mocked(testBridge.importPdf).mockResolvedValue({
      document: imported,
      duplicate: false,
    });
    vi.mocked(testBridge.queryLibrary)
      .mockResolvedValueOnce({ items: [], nextCursor: null })
      .mockResolvedValueOnce({ items: [imported], nextCursor: null });
    render(<App bridge={testBridge} />);
    await screen.findByText("Your research library is empty");

    fireEvent.click(screen.getByRole("button", { name: "Import PDF" }));

    expect(await screen.findByText("Atlas Retrieval")).toBeInTheDocument();
    expect(testBridge.pickPdfPaths).toHaveBeenCalledWith(true);
    expect(testBridge.importPdf).toHaveBeenCalledWith("/papers/atlas.pdf");
    expect(screen.getByRole("status")).toHaveTextContent("1 imported");
  });

  it("keeps search disabled while an import owns the operation lock", async () => {
    const testBridge = bridge();
    let finishImport: ((result: ImportPdfResult) => void) | undefined;
    vi.mocked(testBridge.pickPdfPaths).mockResolvedValue(["/papers/atlas.pdf"]);
    vi.mocked(testBridge.importPdf).mockImplementation(
      () =>
        new Promise((resolve) => {
          finishImport = resolve;
        }),
    );
    render(<App bridge={testBridge} />);
    await screen.findByText("Your research library is empty");

    fireEvent.click(screen.getByRole("button", { name: "Import PDF" }));

    expect(await screen.findByRole("button", { name: "Importing…" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Search" })).toBeDisabled();
    await act(async () => {
      finishImport?.({
        document: document(),
        duplicate: false,
      });
    });
    await waitFor(() => {
      expect(screen.getByRole("button", { name: "Import PDF" })).toBeEnabled();
    });
  });

  it("imports dropped PDF paths and shows drag feedback", async () => {
    const testBridge = bridge();
    const imported = document();
    let dropListener: ((event: PdfDropEvent) => void) | undefined;
    vi.mocked(testBridge.subscribePdfDrops).mockImplementation(async (listener) => {
      dropListener = listener;
      return () => undefined;
    });
    vi.mocked(testBridge.importPdf).mockResolvedValue({
      document: imported,
      duplicate: false,
    });
    vi.mocked(testBridge.queryLibrary)
      .mockResolvedValueOnce({ items: [], nextCursor: null })
      .mockResolvedValueOnce({ items: [imported], nextCursor: null });
    render(<App bridge={testBridge} />);
    await screen.findByText("Your research library is empty");
    expect(dropListener).toBeDefined();

    act(() => dropListener?.({ type: "enter", paths: ["/papers/drop.pdf"] }));
    expect(screen.getByText("Drop to import")).toBeInTheDocument();
    act(() => dropListener?.({ type: "drop", paths: ["/papers/drop.pdf"] }));

    await waitFor(() => {
      expect(testBridge.importPdf).toHaveBeenCalledWith("/papers/drop.pdf");
    });
    expect(await screen.findByText("Atlas Retrieval")).toBeInTheDocument();
  });

  it("removes only the library record after confirmation", async () => {
    const testBridge = bridge();
    const paper = document();
    vi.mocked(testBridge.queryLibrary)
      .mockResolvedValueOnce({ items: [paper], nextCursor: null })
      .mockResolvedValueOnce({ items: [], nextCursor: null });
    render(<App bridge={testBridge} />);
    await screen.findByText("Atlas Retrieval");

    fireEvent.click(screen.getByRole("button", { name: "Remove" }));

    expect(await screen.findByText("Your research library is empty")).toBeInTheDocument();
    expect(testBridge.confirmDocumentRemoval).toHaveBeenCalledWith("Atlas Retrieval");
    expect(testBridge.removeDocument).toHaveBeenCalledWith("document-1");
    expect(screen.getByRole("status")).toHaveTextContent("The PDF was kept");
  });

  it("relocates a missing source after verifying the selected PDF", async () => {
    const testBridge = bridge();
    const missing = document({ sourceState: "missing" });
    const restored = document({ fileName: "restored.pdf" });
    vi.mocked(testBridge.pickPdfPaths).mockResolvedValue(["/papers/restored.pdf"]);
    vi.mocked(testBridge.relocateDocument).mockResolvedValue(restored);
    vi.mocked(testBridge.queryLibrary)
      .mockResolvedValueOnce({ items: [missing], nextCursor: null })
      .mockResolvedValueOnce({ items: [restored], nextCursor: null });
    render(<App bridge={testBridge} />);
    expect(await screen.findByText("Source missing")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Locate" }));

    expect(await screen.findByText(/restored\.pdf/)).toBeInTheDocument();
    expect(testBridge.pickPdfPaths).toHaveBeenCalledWith(false);
    expect(testBridge.relocateDocument).toHaveBeenCalledWith("document-1", "/papers/restored.pdf");
  });

  it("opens the PDF reader, navigates, searches, and saves the final position", async () => {
    const testBridge = bridge();
    const paper = document();
    const viewer = new FakePdfViewer();
    const viewerFactory = vi.fn(async () => viewer);
    vi.mocked(testBridge.queryLibrary).mockResolvedValue({
      items: [paper],
      nextCursor: null,
    });
    vi.mocked(testBridge.openReader).mockResolvedValue({
      document: paper,
      sourceToken: "reader-token",
      sourceUrl: "atlas-reader://localhost/pdf/reader-token",
      position: {
        page: 1,
        pageOffsetRatio: 0.2,
        scaleValue: "page-width",
        updatedAt: 1,
      },
    });
    render(<App bridge={testBridge} viewerFactory={viewerFactory} />);
    await screen.findByText("Atlas Retrieval");

    fireEvent.click(screen.getByRole("button", { name: "Open" }));

    expect(await screen.findByLabelText("Find in paper")).toBeInTheDocument();
    expect(testBridge.openReader).toHaveBeenCalledWith("document-1");
    expect(viewer.open).toHaveBeenCalledWith(
      expect.objectContaining({
        sourceUrl: "atlas-reader://localhost/pdf/reader-token",
      }),
    );
    fireEvent.click(screen.getByRole("button", { name: "Next page" }));
    expect(viewer.setPage).toHaveBeenCalledWith(2);
    fireEvent.change(screen.getByLabelText("Find in paper"), {
      target: { value: "retrieval" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Find" }));
    expect(viewer.search).toHaveBeenCalledWith("retrieval");
    act(() => {
      viewer.emitPosition({
        page: 4,
        pageOffsetRatio: 0.65,
        scaleValue: "1.25",
      });
    });

    fireEvent.click(screen.getByRole("button", { name: "← Library" }));

    await waitFor(() => {
      expect(testBridge.closeReader).toHaveBeenCalledWith("reader-token", {
        page: 4,
        pageOffsetRatio: 0.65,
        scaleValue: "1.25",
      });
    });
    expect(viewer.close).toHaveBeenCalledOnce();
  });

  it("throttles periodic reading-position persistence", async () => {
    const testBridge = bridge();
    const paper = document();
    const viewer = new FakePdfViewer();
    vi.mocked(testBridge.queryLibrary).mockResolvedValue({
      items: [paper],
      nextCursor: null,
    });
    vi.mocked(testBridge.openReader).mockResolvedValue({
      document: paper,
      sourceToken: "reader-token",
      sourceUrl: "atlas-reader://localhost/pdf/reader-token",
      position: {
        page: 1,
        pageOffsetRatio: 0,
        scaleValue: "page-width",
        updatedAt: 1,
      },
    });
    render(<App bridge={testBridge} viewerFactory={async () => viewer} />);
    await screen.findByText("Atlas Retrieval");
    fireEvent.click(screen.getByRole("button", { name: "Open" }));
    await screen.findByLabelText("Find in paper");
    vi.useFakeTimers();
    try {
      act(() => {
        viewer.emitPosition({
          page: 2,
          pageOffsetRatio: 0.4,
          scaleValue: "page-fit",
        });
        vi.advanceTimersByTime(749);
      });
      expect(testBridge.saveReadingPosition).not.toHaveBeenCalled();
      await act(async () => {
        vi.advanceTimersByTime(1);
        await Promise.resolve();
      });
      expect(testBridge.saveReadingPosition).toHaveBeenCalledWith("reader-token", {
        page: 2,
        pageOffsetRatio: 0.4,
        scaleValue: "page-fit",
      });
    } finally {
      vi.useRealTimers();
    }
  });

  it("shows reader-open failures and returns to the library", async () => {
    const testBridge = bridge();
    const paper = document();
    vi.mocked(testBridge.queryLibrary).mockResolvedValue({
      items: [paper],
      nextCursor: null,
    });
    vi.mocked(testBridge.openReader).mockRejectedValue({
      code: "source_missing",
      message: "The selected PDF no longer exists",
      recoverable: true,
    });
    render(<App bridge={testBridge} viewerFactory={async () => new FakePdfViewer()} />);
    await screen.findByText("Atlas Retrieval");

    fireEvent.click(screen.getByRole("button", { name: "Open" }));

    expect(await screen.findByRole("alert")).toHaveTextContent("The selected PDF no longer exists");
    fireEvent.click(screen.getByRole("button", { name: "Return to library" }));
    expect(await screen.findByText("Atlas Retrieval")).toBeInTheDocument();
  });

  it("reports an import failure without discarding the current library", async () => {
    const testBridge = bridge();
    vi.mocked(testBridge.pickPdfPaths).mockResolvedValue(["/papers/not-a-pdf.txt"]);
    vi.mocked(testBridge.importPdf).mockRejectedValue({
      code: "unsupported_file_type",
      message: "Atlas Reader only imports PDF files",
      recoverable: true,
    });
    render(<App bridge={testBridge} />);
    await screen.findByText("Your research library is empty");

    fireEvent.click(screen.getByRole("button", { name: "Import PDF" }));

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Atlas Reader only imports PDF files",
    );
    expect(screen.getByText("Your research library is empty")).toBeInTheDocument();
  });

  it("surfaces library startup failures without hiding the workspace", async () => {
    const testBridge = bridge();
    vi.mocked(testBridge.queryLibrary).mockRejectedValue(new Error("database unavailable"));

    render(<App bridge={testBridge} />);

    expect(await screen.findByRole("alert")).toHaveTextContent("database unavailable");
    expect(screen.getByText("Local core connected")).toBeInTheDocument();
  });

  it("moves between the library and the provider settings", async () => {
    const testBridge = bridge();

    render(<App bridge={testBridge} />);

    await screen.findByRole("heading", {
      name: "Read difficult papers without losing the thread.",
    });

    fireEvent.click(screen.getByRole("button", { name: "Settings" }));

    expect(
      await screen.findByRole("heading", {
        name: "Connect the services Atlas is allowed to call.",
      }),
    ).toBeVisible();
    expect(screen.getByRole("button", { name: "Settings" })).toHaveAttribute(
      "aria-current",
      "page",
    );
    expect(testBridge.getProviderSettings).toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: /^Library/ }));

    expect(
      await screen.findByRole("heading", {
        name: "Read difficult papers without losing the thread.",
      }),
    ).toBeVisible();
  });
});
