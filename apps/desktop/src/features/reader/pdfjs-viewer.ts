import { GlobalWorkerOptions, getDocument, type PDFDocumentLoadingTask } from "pdfjs-dist";
import workerUrl from "pdfjs-dist/build/pdf.worker.min.mjs?url";
import {
  EventBus,
  PDFFindController,
  PDFLinkService,
  PDFViewer,
} from "pdfjs-dist/web/pdf_viewer.mjs";

import type { OpenPdfViewerInput, PdfViewerModule, PdfViewerState } from "./pdf-viewer-module";

GlobalWorkerOptions.workerSrc = workerUrl;

const namedScales = ["auto", "page-actual", "page-fit", "page-width"];
const minScale = 0.25;
const maxScale = 5;
const zoomStep = 1.1;

interface PageChangingEvent {
  pageNumber: number;
}

interface ScaleChangingEvent {
  scale: number;
  presetValue?: string;
}

interface FindMatchesEvent {
  matchesCount: {
    current: number;
    total: number;
  };
}

interface LoadingProgress {
  loaded: number;
  total: number;
}

interface PageView {
  div: HTMLDivElement;
}

export class PdfJsViewer implements PdfViewerModule {
  private readonly eventBus = new EventBus();
  private readonly linkService = new PDFLinkService({
    eventBus: this.eventBus,
    externalLinkTarget: 2,
  });
  private readonly findController = new PDFFindController({
    eventBus: this.eventBus,
    linkService: this.linkService,
  });
  private readonly viewer: PDFViewer;
  private loadingTask: PDFDocumentLoadingTask | undefined;
  private input: OpenPdfViewerInput | undefined;
  private state: PdfViewerState = {
    page: 1,
    pageCount: 0,
    scaleValue: "page-width",
    searchCurrent: 0,
    searchTotal: 0,
    loadingProgress: 0,
  };
  private restoring = true;
  private scrollTimer: number | undefined;
  private searchQuery = "";

  constructor(private readonly container: HTMLDivElement) {
    const viewerElement = container.querySelector<HTMLDivElement>(".pdfViewer");
    if (!viewerElement) {
      throw new Error("PDF viewer element is missing");
    }
    this.viewer = new PDFViewer({
      container,
      viewer: viewerElement,
      eventBus: this.eventBus,
      linkService: this.linkService,
      findController: this.findController,
      removePageBorders: false,
      maxCanvasPixels: 32 * 1024 * 1024,
    });
    this.linkService.setViewer(this.viewer);
    this.bindEvents();
  }

  async open(input: OpenPdfViewerInput): Promise<void> {
    this.input = input;
    this.restoring = true;
    this.state = {
      ...this.state,
      page: input.initialPosition.page,
      scaleValue: input.initialPosition.scaleValue,
      loadingProgress: 0,
    };
    this.emitState();
    this.loadingTask = getDocument({
      url: input.sourceUrl,
      rangeChunkSize: 64 * 1024,
    });
    this.loadingTask.onProgress = ({ loaded, total }: LoadingProgress) => {
      this.state.loadingProgress = total > 0 ? loaded / total : undefined;
      this.emitState();
    };
    const pdfDocument = await this.loadingTask.promise;
    this.state.pageCount = pdfDocument.numPages;
    this.findController.setDocument(pdfDocument);
    this.linkService.setDocument(pdfDocument);
    this.viewer.setDocument(pdfDocument);
  }

  setPage(page: number): void {
    if (this.state.pageCount === 0) {
      return;
    }
    this.viewer.currentPageNumber = clamp(Math.round(page), 1, this.state.pageCount);
  }

  zoomIn(): void {
    this.applyScaleFactor(zoomStep);
  }

  zoomOut(): void {
    this.applyScaleFactor(1 / zoomStep);
  }

  setScale(scaleValue: string): void {
    this.viewer.currentScaleValue = scaleValue;
  }

  search(query: string): void {
    this.searchQuery = query;
    this.eventBus.dispatch("find", {
      source: this,
      type: "",
      query,
      caseSensitive: false,
      entireWord: false,
      highlightAll: true,
      findPrevious: false,
      matchDiacritics: true,
    });
  }

  findNext(): void {
    this.findAgain(false);
  }

  findPrevious(): void {
    this.findAgain(true);
  }

  currentPosition() {
    const page = this.viewer.currentPageNumber || this.state.page || 1;
    const pageView = this.viewer.getPageView(page - 1) as PageView | undefined;
    let pageOffsetRatio = 0;
    if (pageView?.div.clientHeight) {
      pageOffsetRatio = clamp(
        (this.container.scrollTop - pageView.div.offsetTop) / pageView.div.clientHeight,
        0,
        1,
      );
    }
    return {
      page,
      pageOffsetRatio,
      scaleValue: normalizeScaleValue(this.viewer.currentScaleValue || this.state.scaleValue),
    };
  }

  async close(): Promise<void> {
    if (this.scrollTimer !== undefined) {
      window.clearTimeout(this.scrollTimer);
      this.scrollTimer = undefined;
    }
    this.container.removeEventListener("scroll", this.handleScroll);
    await this.loadingTask?.destroy();
    this.loadingTask = undefined;
    this.input = undefined;
  }

  private bindEvents(): void {
    this.eventBus.on("pagesinit", () => {
      const initial = this.input?.initialPosition;
      if (!initial) {
        return;
      }
      this.viewer.currentScaleValue = initial.scaleValue;
      this.viewer.currentPageNumber = clamp(initial.page, 1, Math.max(this.state.pageCount, 1));
      window.requestAnimationFrame(() => {
        const pageView = this.viewer.getPageView(this.viewer.currentPageNumber - 1) as
          PageView | undefined;
        if (pageView) {
          this.container.scrollTop =
            pageView.div.offsetTop + initial.pageOffsetRatio * pageView.div.clientHeight;
        }
        this.restoring = false;
        this.state.loadingProgress = undefined;
        this.emitState();
        this.emitPosition();
      });
    });
    this.eventBus.on("pagechanging", (event: PageChangingEvent) => {
      this.state.page = event.pageNumber;
      this.emitState();
      this.schedulePosition();
    });
    this.eventBus.on("scalechanging", (event: ScaleChangingEvent) => {
      this.state.scaleValue = normalizeScaleValue(event.presetValue ?? String(event.scale));
      this.emitState();
      this.schedulePosition();
    });
    this.eventBus.on("updatefindmatchescount", (event: FindMatchesEvent) => {
      this.state.searchCurrent = event.matchesCount.current;
      this.state.searchTotal = event.matchesCount.total;
      this.emitState();
    });
    this.container.addEventListener("scroll", this.handleScroll, { passive: true });
  }

  private readonly handleScroll = () => {
    this.schedulePosition();
  };

  private applyScaleFactor(factor: number): void {
    const current = this.viewer.currentScale;
    if (!Number.isFinite(current) || current <= 0) {
      return;
    }
    const next = roundScale(clamp(current * factor, minScale, maxScale));
    if (next === roundScale(current)) {
      return;
    }
    this.viewer.currentScaleValue = String(next);
  }

  private schedulePosition(): void {
    if (this.restoring) {
      return;
    }
    if (this.scrollTimer !== undefined) {
      window.clearTimeout(this.scrollTimer);
    }
    this.scrollTimer = window.setTimeout(() => {
      this.scrollTimer = undefined;
      this.emitPosition();
    }, 200);
  }

  private emitPosition(): void {
    this.input?.onPositionChange(this.currentPosition());
  }

  private emitState(): void {
    this.input?.onStateChange({ ...this.state });
  }

  private findAgain(findPrevious: boolean): void {
    if (!this.searchQuery) {
      return;
    }
    this.eventBus.dispatch("find", {
      source: this,
      type: "again",
      query: this.searchQuery,
      caseSensitive: false,
      entireWord: false,
      highlightAll: true,
      findPrevious,
      matchDiacritics: true,
    });
  }
}

function clamp(value: number, minimum: number, maximum: number): number {
  return Math.min(Math.max(value, minimum), maximum);
}

function roundScale(value: number): number {
  return Math.round(value * 10_000) / 10_000;
}

/**
 * Keeps viewer zoom inside the range the reading-position store accepts, so
 * repeated zooming can never produce a position the backend rejects.
 */
function normalizeScaleValue(value: string): string {
  if (namedScales.includes(value)) {
    return value;
  }
  const parsed = Number(value);
  if (!Number.isFinite(parsed) || parsed <= 0) {
    return "page-width";
  }
  return String(roundScale(clamp(parsed, minScale, maxScale)));
}
