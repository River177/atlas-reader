import type { ReadingPosition, ReadingPositionUpdate } from "@atlas/contracts";

export interface PdfViewerState {
  page: number;
  pageCount: number;
  scaleValue: string;
  searchCurrent: number;
  searchTotal: number;
  loadingProgress: number | undefined;
}

export interface OpenPdfViewerInput {
  sourceUrl: string;
  initialPosition: ReadingPosition;
  onPositionChange(position: ReadingPositionUpdate): void;
  onStateChange(state: PdfViewerState): void;
}

export interface PdfViewerModule {
  open(input: OpenPdfViewerInput): Promise<void>;
  setPage(page: number): void;
  zoomIn(): void;
  zoomOut(): void;
  setScale(scaleValue: string): void;
  search(query: string): void;
  findNext(): void;
  findPrevious(): void;
  currentPosition(): ReadingPositionUpdate;
  close(): Promise<void>;
}

export type PdfViewerFactory = (container: HTMLDivElement) => Promise<PdfViewerModule>;

export const defaultPdfViewerFactory: PdfViewerFactory = async (container) => {
  const { PdfJsViewer } = await import("./pdfjs-viewer");
  return new PdfJsViewer(container);
};
