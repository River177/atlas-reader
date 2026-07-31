import { Fragment, useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import type {
  CanonicalAsset,
  CanonicalBlock,
  CanonicalChapter,
  CanonicalDocument,
  ContentAtom,
  DocumentSummary,
  ParseSnapshot,
  ParsedDocumentView,
  ReaderSourceToken,
  ReadingPositionUpdate,
  SessionId,
  SessionSnapshot,
  StructuredContent,
  TranslatedBlockView,
  TranslationSnapshot,
} from "@atlas/contracts";

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
const parsePollDelayMs = 750;
const translationPollDelayMs = 750;

const emptyParseView: ParsedDocumentView = {
  parse: {
    state: "not_started",
    backend: null,
    progress: null,
    parseOperationId: null,
    automaticCloudParsingEnabled: false,
    safeMessage: null,
  },
  document: null,
};

const emptyTranslationSnapshot: TranslationSnapshot = {
  targetLocale: "zh-CN",
  modelId: null,
  activeChapter: null,
  prefetchedChapterId: null,
};

export function ReaderScreen({
  bridge,
  document,
  onBack,
  viewerFactory = defaultPdfViewerFactory,
}: ReaderScreenProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const viewerRef = useRef<PdfViewerModule | undefined>(undefined);
  const focusSequenceRef = useRef(0);
  const [viewerState, setViewerState] = useState(initialViewerState);
  const [searchQuery, setSearchQuery] = useState("");
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string>();
  const [saveError, setSaveError] = useState<string>();
  const [parseView, setParseView] = useState<ParsedDocumentView>(emptyParseView);
  const [parseError, setParseError] = useState<string>();
  const [translation, setTranslation] = useState<TranslationSnapshot>(emptyTranslationSnapshot);
  const [translationError, setTranslationError] = useState<string>();
  const [sessionId, setSessionId] = useState<SessionId>();
  const [sessionRevision, setSessionRevision] = useState(0);
  const [viewMode, setViewMode] = useState<"pdf" | "structured">("pdf");
  const [activeChapterId, setActiveChapterId] = useState<string>();
  const [focusGeneration, setFocusGeneration] = useState(0);
  const [parseActionPending, setParseActionPending] = useState(false);
  const applySessionSnapshot = useCallback((snapshot: SessionSnapshot, sequence: number) => {
    if (sequence !== focusSequenceRef.current) {
      return false;
    }
    setSessionRevision(snapshot.revision);
    setTranslation(snapshot.translation);
    setActiveChapterId(snapshot.activeChapterId ?? undefined);
    setTranslationError(undefined);
    return true;
  }, []);
  const parseRunning = parseIsRunning(parseView.parse);
  const translationRunning = translation.activeChapter?.jobActive ?? false;
  const resolvedActiveChapterId = parseView.document?.chapters.some(
    (chapter) => chapter.id === activeChapterId,
  )
    ? activeChapterId
    : parseView.document?.chapters[0]?.id;

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

  useEffect(() => {
    let disposed = false;
    let localSessionId: SessionId | undefined;
    const sequence = focusSequenceRef.current;

    void (async () => {
      try {
        const opened = await bridge.openReadingSession({
          documentId: document.id,
          initialChapterId: null,
        });
        localSessionId = opened.sessionId;
        if (disposed) {
          await bridge.closeReadingSession(opened.sessionId);
          return;
        }
        setSessionId(opened.sessionId);
        applySessionSnapshot(opened.snapshot, sequence);
        const parsed = await bridge.getParsedDocument(document.id);
        if (!disposed) {
          setParseView(parsed);
          setParseError(undefined);
        }
      } catch (reason) {
        if (!disposed) {
          setParseError(errorMessage(reason));
        }
      }
    })();

    return () => {
      disposed = true;
      if (localSessionId) {
        void bridge.closeReadingSession(localSessionId).catch(() => undefined);
      }
    };
  }, [applySessionSnapshot, bridge, document.id]);

  useEffect(() => {
    if (!sessionId || !parseView.document?.artifactId) {
      return;
    }
    let disposed = false;
    const sequence = focusSequenceRef.current;
    void bridge
      .getReadingSessionSnapshot(sessionId)
      .then((snapshot) => {
        if (!disposed) {
          applySessionSnapshot(snapshot, sequence);
        }
      })
      .catch((reason: unknown) => {
        if (!disposed) {
          setTranslationError(errorMessage(reason));
        }
      });
    return () => {
      disposed = true;
    };
  }, [applySessionSnapshot, bridge, parseView.document?.artifactId, sessionId]);

  useEffect(() => {
    if (!sessionId || !translationRunning) {
      return;
    }
    let disposed = false;
    let timer: number | undefined;
    const sequence = focusSequenceRef.current;
    const poll = async () => {
      try {
        const snapshot = await bridge.getReadingSessionSnapshot(sessionId);
        if (disposed || !applySessionSnapshot(snapshot, sequence)) {
          return;
        }
        if (snapshot.translation.activeChapter?.jobActive) {
          timer = window.setTimeout(() => void poll(), translationPollDelayMs);
        }
      } catch (reason) {
        if (!disposed) {
          setTranslationError(errorMessage(reason));
          timer = window.setTimeout(() => void poll(), translationPollDelayMs);
        }
      }
    };
    timer = window.setTimeout(() => void poll(), translationPollDelayMs);
    return () => {
      disposed = true;
      if (timer !== undefined) {
        window.clearTimeout(timer);
      }
    };
  }, [
    applySessionSnapshot,
    bridge,
    focusGeneration,
    resolvedActiveChapterId,
    sessionId,
    translationRunning,
  ]);

  useEffect(() => {
    if (!parseRunning) {
      return;
    }
    let disposed = false;
    let timer: number | undefined;
    const poll = async () => {
      try {
        const parsed = await bridge.getParsedDocument(document.id);
        if (disposed) {
          return;
        }
        setParseView(parsed);
        setParseError(undefined);
        if (parseIsRunning(parsed.parse)) {
          timer = window.setTimeout(() => void poll(), parsePollDelayMs);
        }
      } catch (reason) {
        if (!disposed) {
          setParseError(errorMessage(reason));
          timer = window.setTimeout(() => void poll(), parsePollDelayMs);
        }
      }
    };
    timer = window.setTimeout(() => void poll(), parsePollDelayMs);
    return () => {
      disposed = true;
      if (timer !== undefined) {
        window.clearTimeout(timer);
      }
    };
  }, [bridge, document.id, parseRunning]);

  const changePage = (page: number) => {
    viewerRef.current?.setPage(page);
  };

  const refreshParsedDocument = async () => {
    const parsed = await bridge.getParsedDocument(document.id);
    setParseView(parsed);
    setParseError(undefined);
  };

  const retryRemoteStatus = async () => {
    setParseActionPending(true);
    try {
      const parse = await bridge.retryRemoteParse(document.id);
      setParseView((current) => ({ ...current, parse }));
      setParseError(undefined);
    } catch (reason) {
      setParseError(errorMessage(reason));
    } finally {
      setParseActionPending(false);
    }
  };

  const reupload = async () => {
    if (!sessionId || !(await bridge.confirmParseReupload())) {
      return;
    }
    setParseActionPending(true);
    try {
      const parse = await bridge.reuploadDocument(document.id, sessionId);
      setParseView((current) => ({ ...current, parse }));
      setParseError(undefined);
    } catch (reason) {
      setParseError(errorMessage(reason));
    } finally {
      setParseActionPending(false);
    }
  };

  const refreshSessionTranslation = async (activeSessionId: SessionId, sequence: number) => {
    const snapshot = await bridge.getReadingSessionSnapshot(activeSessionId);
    applySessionSnapshot(snapshot, sequence);
  };

  const focusChapter = (chapter: CanonicalChapter) => {
    const sequence = ++focusSequenceRef.current;
    setFocusGeneration(sequence);
    setActiveChapterId(chapter.id);
    documentElement(chapter.id)?.scrollIntoView?.({ behavior: "smooth", block: "start" });
    if (sessionId) {
      void bridge
        .dispatchReadingCommand({
          sessionId,
          commandId: `focus-${Date.now()}-${chapter.id}`,
          command: { type: "focus_chapter", chapterId: chapter.id },
        })
        .then(async (receipt) => {
          if (sequence !== focusSequenceRef.current) {
            return;
          }
          setSessionRevision(receipt.revision);
          if (receipt.rejection) {
            throw receipt.rejection;
          }
          await refreshSessionTranslation(sessionId, sequence);
        })
        .catch(async (reason: unknown) => {
          if (sequence !== focusSequenceRef.current) {
            return;
          }
          await refreshSessionTranslation(sessionId, sequence).catch(() => undefined);
          setTranslationError(errorMessage(reason));
        });
    }
  };

  const retryTranslation = () => {
    const chapterId = resolvedActiveChapterId;
    if (!sessionId || !chapterId) {
      return;
    }
    const sequence = focusSequenceRef.current;
    void bridge
      .dispatchReadingCommand({
        sessionId,
        commandId: `retry-translation-${Date.now()}-${chapterId}`,
        expectedRevision: sessionRevision,
        command: { type: "retry_translation", chapterId },
      })
      .then(async (receipt) => {
        setSessionRevision(receipt.revision);
        if (receipt.rejection) {
          throw receipt.rejection;
        }
        await refreshSessionTranslation(sessionId, sequence);
      })
      .catch((reason: unknown) => setTranslationError(errorMessage(reason)));
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
        <div className="reader-header-meta">
          <ParseIndicator parse={parseView.parse} error={parseError} />
          <TranslationIndicator translation={translation} error={translationError} />
          <div className="reader-persistence">
            <span
              className={saveError ? "save-indicator save-indicator--error" : "save-indicator"}
            />
            {saveError ? "Position not saved" : "Position saved locally"}
          </div>
        </div>
      </header>

      <div className="reader-toolbar" aria-label="PDF controls">
        <div className="reader-mode-switch" aria-label="Reader mode" role="group">
          <button
            aria-pressed={viewMode === "pdf"}
            className={viewMode === "pdf" ? "is-active" : undefined}
            onClick={() => setViewMode("pdf")}
            type="button"
          >
            PDF
          </button>
          <button
            aria-pressed={viewMode === "structured"}
            className={viewMode === "structured" ? "is-active" : undefined}
            disabled={!parseView.document}
            onClick={() => setViewMode("structured")}
            type="button"
          >
            Bilingual
          </button>
        </div>
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

        {parseView.parse.state === "status_unknown" ? (
          <div className="parse-actions" aria-label="Remote parse recovery">
            <button
              disabled={parseActionPending}
              onClick={() => void retryRemoteStatus()}
              type="button"
            >
              Query remote
            </button>
            <button
              disabled={parseActionPending || !sessionId}
              onClick={() => void reupload()}
              type="button"
            >
              Re-upload
            </button>
          </div>
        ) : null}
        {parseError && parseView.parse.state !== "status_unknown" ? (
          <button
            className="parse-refresh"
            disabled={parseActionPending}
            onClick={() => void refreshParsedDocument()}
            type="button"
          >
            Refresh parse
          </button>
        ) : null}
      </div>

      <section
        className={`reader-stage reader-stage--${viewMode}`}
        aria-label={viewMode === "pdf" ? "PDF document" : "Parsed document"}
      >
        <div
          aria-hidden={viewMode !== "pdf"}
          className={`pdf-viewer-container${viewMode === "pdf" ? "" : " is-hidden"}`}
          ref={containerRef}
        >
          <div className="pdfViewer" />
        </div>
        {viewMode === "structured" && parseView.document ? (
          <StructuredReader
            activeChapterId={resolvedActiveChapterId}
            bridge={bridge}
            document={parseView.document}
            onFocusChapter={focusChapter}
            onRetryTranslation={retryTranslation}
            translation={translation}
          />
        ) : null}
        {loading && viewMode === "pdf" ? (
          <div className="reader-loading" role="status">
            <span>Opening PDF</span>
            <strong>
              {viewerState.loadingProgress === undefined
                ? "Preparing pages…"
                : `${Math.round(viewerState.loadingProgress * 100)}%`}
            </strong>
          </div>
        ) : null}
        {error && viewMode === "pdf" ? (
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

function TranslationIndicator({
  translation,
  error,
}: {
  translation: TranslationSnapshot;
  error: string | undefined;
}) {
  const chapter = translation.activeChapter;
  const label = error
    ? "Translation status unavailable"
    : chapter
      ? (
          {
            not_started: "Waiting to translate",
            not_configured: "Translation not configured",
            queued: "Translation queued",
            translating: `Translating ${Math.round(chapter.progress * 100)}%`,
            readable: `Translation readable ${Math.round(chapter.progress * 100)}%`,
            complete: "Translation complete",
            failed: "Translation needs attention",
          } as const
        )[chapter.state]
      : "Waiting for a chapter";
  return (
    <div
      className={`translation-indicator translation-indicator--${error ? "error" : (chapter?.state ?? "idle")}`}
      title={error ?? chapter?.safeMessage ?? undefined}
    >
      <span />
      {label}
    </div>
  );
}

function namedScale(value: string): string {
  return ["page-width", "page-fit", "page-actual"].includes(value) ? value : "custom";
}

function scaleLabel(value: string): string {
  const parsed = Number(value);
  return Number.isFinite(parsed) ? `${Math.round(parsed * 100)}%` : "Custom";
}

function parseIsRunning(parse: ParseSnapshot): boolean {
  return ["queued", "uploading", "processing", "downloading", "normalizing"].includes(parse.state);
}

function ParseIndicator({ parse, error }: { parse: ParseSnapshot; error: string | undefined }) {
  const progress =
    parse.progress === null
      ? ""
      : ` ${Math.round(Math.max(0, Math.min(1, parse.progress)) * 100)}%`;
  const label = error
    ? "Parse status unavailable"
    : (
        {
          not_started: "Waiting to parse",
          queued: "Parse queued",
          uploading: `Uploading${progress}`,
          processing: `Cloud parsing${progress}`,
          downloading: "Downloading structure",
          normalizing: "Building document",
          ready: "Cloud structure ready",
          degraded: "Basic parsing",
          failed: "Parsing unavailable",
          status_unknown: "Remote status unknown",
        } as const
      )[parse.state];
  return (
    <div
      className={`parse-indicator parse-indicator--${error ? "error" : parse.state}`}
      title={error ?? parse.safeMessage ?? undefined}
    >
      <span />
      {label}
    </div>
  );
}

function StructuredReader({
  activeChapterId,
  bridge,
  document,
  onFocusChapter,
  onRetryTranslation,
  translation,
}: {
  activeChapterId: string | undefined;
  bridge: AtlasBridge;
  document: CanonicalDocument;
  onFocusChapter(chapter: CanonicalChapter): void;
  onRetryTranslation(): void;
  translation: TranslationSnapshot;
}) {
  const chapter =
    document.chapters.find((candidate) => candidate.id === activeChapterId) ?? document.chapters[0];
  const activeTranslation = translation.activeChapter;
  const activeTranslatedBlocks =
    activeTranslation && activeTranslation.chapterId === chapter?.id
      ? activeTranslation.blocks
      : [];
  const translatedBlocks = new Map(activeTranslatedBlocks.map((block) => [block.blockId, block]));
  const failed = activeTranslation?.blocks.some((block) => block.state === "failed");
  return (
    <div className="structured-reader">
      <aside className="chapter-outline" aria-label="Paper chapters">
        <div className="chapter-outline-heading">
          <span>Document map</span>
          <strong>{document.chapters.length} chapters</strong>
        </div>
        <nav aria-label="Paper chapters">
          {document.chapters.map((chapter) => (
            <button
              className={activeChapterId === chapter.id ? "is-active" : undefined}
              key={chapter.id}
              onClick={() => onFocusChapter(chapter)}
              style={{ paddingLeft: `${12 + Math.max(0, chapter.depth - 1) * 12}px` }}
              type="button"
            >
              <span>{String(chapter.orderIndex + 1).padStart(2, "0")}</span>
              {chapter.sourceTitle}
            </button>
          ))}
        </nav>
      </aside>

      <article className="structured-bilingual" aria-label="Bilingual paper text">
        <header>
          <div>
            <span>English · 简体中文</span>
            <h2>{document.title ?? "Parsed paper"}</h2>
          </div>
          <div className="bilingual-header-meta">
            <small>
              {document.parser.backend === "local_text" ? "Basic parsing" : document.parser.name}
            </small>
            {translation.modelId ? <small>{translation.modelId}</small> : null}
            {failed || translation.activeChapter?.state === "failed" ? (
              <button onClick={onRetryTranslation} type="button">
                Retry translation
              </button>
            ) : null}
          </div>
        </header>
        {chapter ? (
          <section className="structured-chapter" id={`chapter-${chapter.id}`} key={chapter.id}>
            <div className="structured-chapter-kicker">
              Chapter {chapter.orderIndex + 1} · pages {chapter.pageStart}–{chapter.pageEnd}
            </div>
            <h2>{chapter.sourceTitle}</h2>
            {chapter.blocks.map((block) => (
              <div className="bilingual-block" data-block-id={block.id} key={block.id}>
                <SourceBlock block={block} bridge={bridge} document={document} />
                <TargetBlock
                  block={translatedBlocks.get(block.id)}
                  bridge={bridge}
                  document={document}
                />
              </div>
            ))}
          </section>
        ) : null}
      </article>
    </div>
  );
}

function TargetBlock({
  block,
  bridge,
  document,
}: {
  block: TranslatedBlockView | undefined;
  bridge: AtlasBridge;
  document: CanonicalDocument;
}) {
  if (block?.state === "ready" && block.target) {
    return (
      <div className="target-block target-block--ready">
        <StructuredContentView bridge={bridge} content={block.target} document={document} />
      </div>
    );
  }
  const message =
    block?.state === "failed"
      ? (block.safeMessage ?? "This block could not be translated safely.")
      : block?.state === "skipped"
        ? "Structure preserved from the source."
        : "Waiting for translation…";
  return (
    <div className={`target-block target-block--${block?.state ?? "pending"}`}>
      <span>{message}</span>
    </div>
  );
}

function SourceBlock({
  block,
  bridge,
  document,
}: {
  block: CanonicalBlock;
  bridge: AtlasBridge;
  document: CanonicalDocument;
}) {
  return (
    <div
      className={`source-block source-block--${block.kind}`}
      data-block-id={block.id}
      data-page={block.pageStart}
    >
      <div className="source-block-meta">
        <span>{block.kind}</span>
        <span>p. {block.pageStart}</span>
      </div>
      <StructuredContentView bridge={bridge} content={block.content} document={document} />
    </div>
  );
}

function StructuredContentView({
  bridge,
  content,
  document,
}: {
  bridge: AtlasBridge;
  content: StructuredContent;
  document: CanonicalDocument;
}) {
  const assets = new Map(document.assets.map((asset) => [asset.id, asset]));
  if (content.atoms.length === 0) {
    return <p>{content.plainText}</p>;
  }
  return (
    <div className="structured-content">
      {content.atoms.map((atom, index) => (
        <Fragment key={`${atom.type}-${index}`}>
          {renderAtom(atom, document, assets, bridge)}
        </Fragment>
      ))}
    </div>
  );
}

function renderAtom(
  atom: ContentAtom,
  document: CanonicalDocument,
  assets: Map<string, CanonicalAsset>,
  bridge: AtlasBridge,
): ReactNode {
  switch (atom.type) {
    case "text":
      return atom.value;
    case "formula":
      return (
        <span className={atom.display ? "formula formula--display" : "formula"} role="math">
          {atom.latex}
        </span>
      );
    case "citation":
      return <span className="citation">{atom.label}</span>;
    case "line_break":
      return <br />;
    case "table":
      return (
        <div className="structured-table-wrap">
          <table>
            <tbody>
              {atom.rows.map((row, rowIndex) => (
                <tr key={rowIndex}>
                  {row.map((cell) => (
                    <td
                      colSpan={cell.columnSpan}
                      key={`${cell.row}-${cell.column}`}
                      rowSpan={cell.rowSpan}
                    >
                      {cell.content.map((cellAtom, atomIndex) => (
                        <Fragment key={atomIndex}>
                          {renderAtom(cellAtom, document, assets, bridge)}
                        </Fragment>
                      ))}
                    </td>
                  ))}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      );
    case "asset": {
      const asset = assets.get(atom.assetId);
      if (!asset) {
        return null;
      }
      const source = bridge.parseAssetUrl(
        document.documentId,
        document.artifactId,
        asset.relativePath,
      );
      return source ? (
        <figure>
          <img alt={atom.alt ?? "Paper figure"} loading="lazy" src={source} />
          {atom.alt ? <figcaption>{atom.alt}</figcaption> : null}
        </figure>
      ) : null;
    }
  }
}

function documentElement(chapterId: string): HTMLElement | null {
  return document.getElementById(`chapter-${chapterId}`);
}
