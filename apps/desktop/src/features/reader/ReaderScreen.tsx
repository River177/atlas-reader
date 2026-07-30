import { useEffect, useRef, useState } from "react";
import type { DocumentSummary, ReaderSourceToken, ReadingPositionUpdate } from "@atlas/contracts";

import type { AtlasBridge } from "../../bridge";
import { errorMessage } from "../../bridge/error-message";
import {
  defaultPdfViewerFactory,
  type PdfViewerFactory,
  type PdfViewerModule,
  type PdfViewerState,
} from "./pdf-viewer-module";
import "./reader.css";

interface ReaderScreenProps {
  bridge: AtlasBridge;
  document: DocumentSummary;
  onBack(): void;
  viewerFactory?: PdfViewerFactory;
}

const initialViewerState: PdfViewerState = {
  page: 1,
  pageCount: 0,
  scaleValue: "page-width",
  searchCurrent: 0,
  searchTotal: 0,
  loadingProgress: 0,
};

const positionSaveDelayMs = 750;

export function ReaderScreen({
  bridge,
  document,
  onBack,
  viewerFactory = defaultPdfViewerFactory,
}: ReaderScreenProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const viewerRef = useRef<PdfViewerModule | undefined>(undefined);
  const [viewerState, setViewerState] = useState(initialViewerState);
  const [searchQuery, setSearchQuery] = useState("");
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string>();
  const [saveError, setSaveError] = useState<string>();

  useEffect(() => {
    let disposed = false;
    let localViewer: PdfViewerModule | undefined;
    let localToken: ReaderSourceToken | undefined;
    let latestPosition: ReadingPositionUpdate | undefined;
    let saveTimer: number | undefined;
    let saveChain: Promise<void> = Promise.resolve();

    const schedulePositionSave = (
      sourceToken: ReaderSourceToken,
      position: ReadingPositionUpdate,
    ) => {
      if (saveTimer !== undefined) {
        window.clearTimeout(saveTimer);
      }
      saveTimer = window.setTimeout(() => {
        saveTimer = undefined;
        saveChain = saveChain
          .catch(() => undefined)
          .then(async () => {
            await bridge.saveReadingPosition(sourceToken, position);
            if (!disposed) {
              setSaveError(undefined);
            }
          })
          .catch((reason: unknown) => {
            if (!disposed) {
              setSaveError(errorMessage(reason));
            }
          });
      }, positionSaveDelayMs);
    };

    void (async () => {
      try {
        const container = containerRef.current;
        if (!container) {
          throw new Error("PDF viewer container is unavailable");
        }
        const opened = await bridge.openReader(document.id);
        localToken = opened.sourceToken;
        if (disposed) {
          await bridge.closeReader(opened.sourceToken);
          return;
        }
        latestPosition = {
          page: opened.position.page,
          pageOffsetRatio: opened.position.pageOffsetRatio,
          scaleValue: opened.position.scaleValue,
        };
        localViewer = await viewerFactory(container);
        if (disposed) {
          await localViewer.close();
          await bridge.closeReader(opened.sourceToken);
          return;
        }
        viewerRef.current = localViewer;
        await localViewer.open({
          sourceUrl: opened.sourceUrl,
          initialPosition: opened.position,
          onStateChange: (state) => {
            if (!disposed) {
              setViewerState(state);
            }
          },
          onPositionChange: (position) => {
            if (disposed) {
              return;
            }
            latestPosition = position;
            schedulePositionSave(opened.sourceToken, position);
          },
        });
        if (!disposed) {
          setLoading(false);
        }
      } catch (reason) {
        await localViewer?.close().catch(() => undefined);
        if (localToken) {
          await bridge.closeReader(localToken).catch(() => undefined);
          localToken = undefined;
        }
        if (!disposed) {
          setError(errorMessage(reason));
          setLoading(false);
        }
      }
    })();

    return () => {
      disposed = true;
      if (saveTimer !== undefined) {
        window.clearTimeout(saveTimer);
        saveTimer = undefined;
      }
      const finalPosition = localViewer?.currentPosition() ?? latestPosition;
      void localViewer?.close().catch(() => undefined);
      if (localToken) {
        const sourceToken = localToken;
        void saveChain
          .catch(() => undefined)
          .then(() =>
            finalPosition
              ? bridge.closeReader(sourceToken, finalPosition)
              : bridge.closeReader(sourceToken),
          )
          .catch(() => undefined);
      }
      viewerRef.current = undefined;
    };
  }, [bridge, document.id, viewerFactory]);

  const changePage = (page: number) => {
    viewerRef.current?.setPage(page);
  };

  return (
    <main className="reader-screen">
      <header className="reader-header">
        <button className="reader-back" onClick={onBack} type="button">
          ← Library
        </button>
        <div className="reader-title">
          <span>{document.fileName}</span>
          <h1>{document.title}</h1>
        </div>
        <div className="reader-persistence">
          <span className={saveError ? "save-indicator save-indicator--error" : "save-indicator"} />
          {saveError ? "Position not saved" : "Position saved locally"}
        </div>
      </header>

      <div className="reader-toolbar" aria-label="PDF controls">
        <div className="page-controls">
          <button
            aria-label="Previous page"
            disabled={viewerState.page <= 1}
            onClick={() => changePage(viewerState.page - 1)}
            type="button"
          >
            ←
          </button>
          <label htmlFor="reader-page">Page</label>
          <input
            id="reader-page"
            max={Math.max(viewerState.pageCount, 1)}
            min={1}
            onChange={(event) => changePage(Number(event.currentTarget.value))}
            type="number"
            value={viewerState.page}
          />
          <span>of {viewerState.pageCount || "—"}</span>
          <button
            aria-label="Next page"
            disabled={viewerState.pageCount === 0 || viewerState.page >= viewerState.pageCount}
            onClick={() => changePage(viewerState.page + 1)}
            type="button"
          >
            →
          </button>
        </div>

        <div className="zoom-controls">
          <button aria-label="Zoom out" onClick={() => viewerRef.current?.zoomOut()} type="button">
            −
          </button>
          <select
            aria-label="Zoom level"
            onChange={(event) => viewerRef.current?.setScale(event.currentTarget.value)}
            value={namedScale(viewerState.scaleValue)}
          >
            <option value="page-width">Page width</option>
            <option value="page-fit">Fit page</option>
            <option value="page-actual">Actual size</option>
            <option value="custom" disabled>
              {scaleLabel(viewerState.scaleValue)}
            </option>
          </select>
          <button aria-label="Zoom in" onClick={() => viewerRef.current?.zoomIn()} type="button">
            +
          </button>
        </div>

        <form
          className="reader-search"
          onSubmit={(event) => {
            event.preventDefault();
            viewerRef.current?.search(searchQuery);
          }}
        >
          <label htmlFor="reader-search">Find in paper</label>
          <input
            id="reader-search"
            onChange={(event) => setSearchQuery(event.currentTarget.value)}
            placeholder="Search text"
            type="search"
            value={searchQuery}
          />
          <button type="submit">Find</button>
          <button
            aria-label="Previous search result"
            onClick={() => viewerRef.current?.findPrevious()}
            type="button"
          >
            ↑
          </button>
          <button
            aria-label="Next search result"
            onClick={() => viewerRef.current?.findNext()}
            type="button"
          >
            ↓
          </button>
          <span>
            {viewerState.searchTotal
              ? `${viewerState.searchCurrent}/${viewerState.searchTotal}`
              : "0 results"}
          </span>
        </form>
      </div>

      <section className="reader-stage" aria-label="PDF document">
        <div className="pdf-viewer-container" ref={containerRef}>
          <div className="pdfViewer" />
        </div>
        {loading ? (
          <div className="reader-loading" role="status">
            <span>Opening PDF</span>
            <strong>
              {viewerState.loadingProgress === undefined
                ? "Preparing pages…"
                : `${Math.round(viewerState.loadingProgress * 100)}%`}
            </strong>
          </div>
        ) : null}
        {error ? (
          <div className="reader-error" role="alert">
            <span>Reader unavailable</span>
            <strong>{error}</strong>
            <button onClick={onBack} type="button">
              Return to library
            </button>
          </div>
        ) : null}
      </section>
    </main>
  );
}

function namedScale(value: string): string {
  return ["page-width", "page-fit", "page-actual"].includes(value) ? value : "custom";
}

function scaleLabel(value: string): string {
  const parsed = Number(value);
  return Number.isFinite(parsed) ? `${Math.round(parsed * 100)}%` : "Custom";
}
