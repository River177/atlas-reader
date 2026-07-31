import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type {
  CanonicalDocument,
  CommandReceipt,
  DocumentSummary,
  ImportPdfResult,
  ParseSnapshot,
  SessionSnapshot,
} from "@atlas/contracts";

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

function canonicalDocument(): CanonicalDocument {
  const figureDigest = "a".repeat(64);
  return {
    schemaVersion: 1,
    artifactId: "artifact-operation-1",
    documentId: "document-1",
    sourceSha256: "b".repeat(64),
    parser: {
      name: "Atlas local text",
      version: "1",
      backend: "local_text",
    },
    normalizerVersion: "1",
    pageCount: 12,
    title: "Atlas Retrieval",
    assets: [
      {
        id: "asset-figure-1",
        mimeType: "image/png",
        relativePath: `images/${figureDigest}.png`,
        sha256: figureDigest,
        sizeBytes: 128,
      },
    ],
    chapters: [
      {
        id: "chapter-introduction",
        orderIndex: 0,
        depth: 1,
        role: "body",
        sourceTitle: "Introduction",
        pageStart: 1,
        pageEnd: 2,
        blocks: [
          {
            id: "block-introduction",
            orderIndex: 0,
            kind: "paragraph",
            pageStart: 1,
            pageEnd: 1,
            boundingBoxes: [],
            content: {
              plainText: "Retrieval systems connect papers.",
              atoms: [{ type: "text", value: "Retrieval systems connect papers." }],
            },
            sourceDigest: "intro-digest",
          },
        ],
      },
      {
        id: "chapter-method",
        orderIndex: 1,
        depth: 1,
        role: "body",
        sourceTitle: "Method",
        pageStart: 3,
        pageEnd: 5,
        blocks: [
          {
            id: "block-figure",
            orderIndex: 0,
            kind: "figure",
            pageStart: 4,
            pageEnd: 4,
            boundingBoxes: [],
            content: {
              plainText: "Figure 1",
              atoms: [{ type: "asset", assetId: "asset-figure-1", alt: "System overview" }],
            },
            sourceDigest: "figure-digest",
          },
        ],
      },
    ],
  };
}

function parseSnapshot(
  state: ParseSnapshot["state"],
  overrides: Partial<ParseSnapshot> = {},
): ParseSnapshot {
  return {
    state,
    backend: null,
    progress: null,
    parseOperationId: "parse-operation-1",
    automaticCloudParsingEnabled: true,
    safeMessage: null,
    ...overrides,
  };
}

function emptyReadingAssistant(): SessionSnapshot["readingAssistant"] {
  return {
    schemaVersion: 1,
    conversationId: null,
    messages: [],
    activeAssistantMessageId: null,
    latestSelection: null,
  };
}

function readyReaderSession(
  readingAssistant: SessionSnapshot["readingAssistant"] = emptyReadingAssistant(),
): SessionSnapshot {
  return {
    schemaVersion: 3,
    sessionId: "session-1",
    documentId: "document-1",
    revision: 0,
    lifecycle: "ready",
    parseState: "ready",
    activeChapterId: "chapter-introduction",
    activeJobIds: [],
    providerStatus: {
      mineru: "ready",
      translation: "ready",
      translationModel: "test-model",
    },
    translation: {
      targetLocale: "zh-CN",
      modelId: "test-model",
      prefetchedChapterId: null,
      activeChapter: {
        chapterId: "chapter-introduction",
        state: "complete",
        progress: 1,
        jobId: "translation-job-1",
        jobActive: false,
        prefetched: false,
        safeMessage: null,
        blocks: [
          {
            blockId: "block-introduction",
            sourceDigest: "intro-digest",
            state: "ready",
            target: {
              plainText: "检索系统连接论文。",
              atoms: [{ type: "text", value: "检索系统连接论文。" }],
            },
            safeMessage: null,
          },
        ],
      },
    },
    readingAssistant,
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

  emitState(state: Partial<PdfViewerState>) {
    this.state = { ...this.state, ...state };
    this.input?.onStateChange(this.state);
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

  it("renders degraded parsed structure, chapter navigation, and content-addressed assets", async () => {
    const testBridge = bridge();
    const paper = document();
    vi.mocked(testBridge.queryLibrary).mockResolvedValue({ items: [paper], nextCursor: null });
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
    vi.mocked(testBridge.getParsedDocument).mockResolvedValue({
      parse: parseSnapshot("degraded", {
        backend: "local_text",
        automaticCloudParsingEnabled: false,
      }),
      document: canonicalDocument(),
    });
    const introductionSnapshot = {
      schemaVersion: 3,
      sessionId: "session-1",
      documentId: "document-1",
      revision: 0,
      lifecycle: "degraded",
      parseState: "degraded",
      activeChapterId: "chapter-introduction",
      activeJobIds: [],
      providerStatus: {
        mineru: "not_configured",
        translation: "ready",
        translationModel: "test-model",
      },
      translation: {
        targetLocale: "zh-CN",
        modelId: "test-model",
        prefetchedChapterId: null,
        activeChapter: {
          chapterId: "chapter-introduction",
          state: "complete",
          progress: 1,
          jobId: "translation-job-1",
          jobActive: false,
          prefetched: false,
          safeMessage: null,
          blocks: [
            {
              blockId: "block-introduction",
              sourceDigest: "intro-digest",
              state: "ready",
              target: {
                plainText: "检索系统连接论文。",
                atoms: [{ type: "text", value: "检索系统连接论文。" }],
              },
              safeMessage: null,
            },
          ],
        },
      },
      readingAssistant: emptyReadingAssistant(),
    } satisfies SessionSnapshot;
    vi.mocked(testBridge.getReadingSessionSnapshot)
      .mockResolvedValueOnce(introductionSnapshot)
      .mockResolvedValue({
        ...introductionSnapshot,
        revision: 1,
        activeChapterId: "chapter-method",
        translation: {
          ...introductionSnapshot.translation,
          activeChapter: null,
        },
      });
    render(<App bridge={testBridge} viewerFactory={async () => new FakePdfViewer()} />);
    await screen.findByText("Atlas Retrieval");
    fireEvent.click(screen.getByRole("button", { name: "Open" }));

    expect(await screen.findByText("Basic parsing")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Bilingual" }));

    expect(await screen.findByRole("navigation")).toHaveAccessibleName("Paper chapters");
    expect(screen.getByText("Retrieval systems connect papers.")).toBeInTheDocument();
    expect(await screen.findByText("检索系统连接论文。")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "02Method" }));
    await waitFor(() => {
      expect(testBridge.dispatchReadingCommand).toHaveBeenCalledWith(
        expect.objectContaining({
          sessionId: "session-1",
          command: { type: "focus_chapter", chapterId: "chapter-method" },
        }),
      );
    });
    expect(screen.getByRole("img", { name: "System overview" })).toHaveAttribute(
      "src",
      expect.stringContaining("artifact-operation-1/images/"),
    );
    expect(testBridge.parseAssetUrl).toHaveBeenCalledWith(
      "document-1",
      "artifact-operation-1",
      expect.stringMatching(/^images\/[a-f0-9]{64}\.png$/),
    );
  });

  it("turns a translated-text selection into a Reading Assistant command", async () => {
    const testBridge = bridge();
    const paper = document();
    let resolveAssistant: ((receipt: CommandReceipt) => void) | undefined;
    vi.mocked(testBridge.queryLibrary).mockResolvedValue({ items: [paper], nextCursor: null });
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
    vi.mocked(testBridge.getParsedDocument).mockResolvedValue({
      parse: parseSnapshot("ready"),
      document: canonicalDocument(),
    });
    const session = readyReaderSession();
    vi.mocked(testBridge.getReadingSessionSnapshot)
      .mockResolvedValueOnce(session)
      .mockRejectedValueOnce(new Error("Snapshot refresh unavailable"))
      .mockResolvedValue(session);
    vi.mocked(testBridge.dispatchReadingCommand).mockImplementation(
      () =>
        new Promise<CommandReceipt>((resolve) => {
          resolveAssistant = resolve;
        }),
    );
    render(<App bridge={testBridge} viewerFactory={async () => new FakePdfViewer()} />);
    await screen.findByText("Atlas Retrieval");
    fireEvent.click(screen.getByRole("button", { name: "Open" }));
    fireEvent.click(await screen.findByRole("button", { name: "Bilingual" }));
    const translated = await screen.findByText("检索系统连接论文。");
    const textNode = translated.firstChild;
    if (!textNode) {
      throw new Error("translated text node is missing");
    }
    const range = window.document.createRange();
    range.setStart(textNode, 0);
    range.setEnd(textNode, 4);
    window.getSelection()?.removeAllRanges();
    window.getSelection()?.addRange(range);
    fireEvent(window.document, new Event("selectionchange"));

    expect(await screen.findByText("Reading Assistant")).toBeInTheDocument();
    expect(screen.getAllByText("检索系统")).toHaveLength(2);
    await waitFor(() =>
      expect(window.document.querySelector(".reading-selection-highlight")).toHaveTextContent(
        "检索系统",
      ),
    );
    fireEvent.change(screen.getByLabelText("Ask about this selection"), {
      target: { value: "为什么这一点重要？" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Ask" }));

    await waitFor(() => {
      expect(testBridge.dispatchReadingCommand).toHaveBeenCalledWith(
        expect.objectContaining({
          sessionId: "session-1",
          command: {
            type: "reading_assistant",
            command: {
              type: "send_message",
              userMessageId: expect.stringMatching(/^reader-message-/),
              text: "为什么这一点重要？",
              selection: {
                blockId: "block-introduction",
                sourceDigest: "intro-digest",
                startUtf16: 0,
                endUtf16: 4,
                selectedText: "检索系统",
              },
            },
          },
        }),
      );
    });
    const nextRange = window.document.createRange();
    const remainingTextNode = window.document.querySelector(
      '[data-reading-target-block] [data-plain-start="0"]',
    )?.lastChild;
    if (!remainingTextNode) {
      throw new Error("remaining translated text node is missing");
    }
    nextRange.setStart(remainingTextNode, 0);
    nextRange.setEnd(remainingTextNode, 2);
    window.getSelection()?.removeAllRanges();
    window.getSelection()?.addRange(nextRange);
    fireEvent(window.document, new Event("selectionchange"));
    expect(await screen.findAllByText("连接")).toHaveLength(2);
    await waitFor(() =>
      expect(window.document.querySelector(".reading-selection-highlight")).toHaveTextContent(
        "连接",
      ),
    );
    act(() =>
      resolveAssistant?.({
        commandId: "assistant-command",
        status: "accepted",
        revision: 1,
        rejection: null,
      }),
    );
    await waitFor(() => {
      expect(screen.getByLabelText("Ask about this selection")).toHaveValue("");
      expect(screen.getAllByText("连接")).toHaveLength(2);
      expect(window.document.querySelector(".reading-selection-highlight")).toHaveTextContent(
        "连接",
      );
    });
  });

  it("serializes assistant commands behind chapter focus revision changes", async () => {
    const testBridge = bridge();
    const paper = document();
    let focused = false;
    let resolveFocus: (() => void) | undefined;
    vi.mocked(testBridge.queryLibrary).mockResolvedValue({ items: [paper], nextCursor: null });
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
    vi.mocked(testBridge.getParsedDocument).mockResolvedValue({
      parse: parseSnapshot("ready"),
      document: canonicalDocument(),
    });
    vi.mocked(testBridge.getReadingSessionSnapshot).mockImplementation(async () => ({
      ...readyReaderSession(),
      revision: focused ? 1 : 0,
      activeChapterId: focused ? "chapter-method" : "chapter-introduction",
      translation: focused
        ? { ...readyReaderSession().translation, activeChapter: null }
        : readyReaderSession().translation,
    }));
    vi.mocked(testBridge.dispatchReadingCommand).mockImplementation((input) => {
      if (input.command.type === "focus_chapter") {
        return new Promise<CommandReceipt>((resolve) => {
          resolveFocus = () => {
            focused = true;
            resolve({
              commandId: input.commandId,
              status: "accepted",
              revision: 1,
              rejection: null,
            });
          };
        });
      }
      return Promise.resolve({
        commandId: input.commandId,
        status: "accepted",
        revision: 2,
        rejection: null,
      });
    });
    render(<App bridge={testBridge} viewerFactory={async () => new FakePdfViewer()} />);
    await screen.findByText("Atlas Retrieval");
    fireEvent.click(screen.getByRole("button", { name: "Open" }));
    fireEvent.click(await screen.findByRole("button", { name: "Bilingual" }));
    const translated = await screen.findByText("检索系统连接论文。");
    const textNode = translated.firstChild!;
    const range = window.document.createRange();
    range.setStart(textNode, 0);
    range.setEnd(textNode, 4);
    window.getSelection()?.removeAllRanges();
    window.getSelection()?.addRange(range);
    fireEvent.mouseUp(translated.closest(".target-block")!);
    fireEvent.change(screen.getByLabelText("Ask about this selection"), {
      target: { value: "这一点与方法有什么关系？" },
    });
    fireEvent.click(screen.getByRole("tab", { name: "Outline" }));
    fireEvent.click(screen.getByRole("button", { name: "02Method" }));
    fireEvent.click(screen.getByRole("tab", { name: "Assistant" }));
    fireEvent.click(screen.getByRole("button", { name: "Ask" }));
    fireEvent.click(screen.getByRole("tab", { name: "Outline" }));
    fireEvent.click(screen.getByRole("button", { name: "01Introduction" }));

    await waitFor(() => expect(testBridge.dispatchReadingCommand).toHaveBeenCalledTimes(1));
    resolveFocus?.();
    await waitFor(() => {
      expect(testBridge.dispatchReadingCommand).toHaveBeenCalledWith(
        expect.objectContaining({
          expectedRevision: 1,
          command: expect.objectContaining({ type: "reading_assistant" }),
        }),
      );
    });
  });

  it("restores assistant messages and supports citation and cancel actions", async () => {
    const testBridge = bridge();
    const paper = document();
    const viewer = new FakePdfViewer();
    viewer.state = { ...viewer.state, pageCount: 0 };
    vi.mocked(testBridge.queryLibrary).mockResolvedValue({ items: [paper], nextCursor: null });
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
    vi.mocked(testBridge.getParsedDocument).mockResolvedValue({
      parse: parseSnapshot("ready"),
      document: canonicalDocument(),
    });
    vi.mocked(testBridge.getReadingSessionSnapshot).mockResolvedValue(
      readyReaderSession({
        schemaVersion: 1,
        conversationId: "conversation-1",
        activeAssistantMessageId: "assistant-streaming",
        latestSelection: {
          blockId: "block-introduction",
          chapterId: "chapter-introduction",
          pageStart: 1,
          pageEnd: 1,
          sourceDigest: "intro-digest",
          startUtf16: 0,
          endUtf16: 4,
          selectedText: "检索系统",
          alignedSource: "Retrieval systems connect papers.",
        },
        messages: [
          {
            role: "reader",
            id: "reader-1",
            text: "为什么重要？",
            selectionContext: null,
            createdAt: 1,
          },
          {
            role: "assistant",
            id: "assistant-failed",
            respondingTo: "reader-1",
            state: "failed",
            text: "部分回答",
            citations: [
              {
                id: "citation-1",
                blockId: "block-introduction",
                chapterId: "chapter-introduction",
                page: 1,
                label: "p. 1",
              },
            ],
            retryOfMessageId: null,
            safeMessage: "The response stopped",
            createdAt: 2,
            updatedAt: 3,
          },
          {
            role: "reader",
            id: "reader-2",
            text: "有什么限制？",
            selectionContext: null,
            createdAt: 3,
          },
          {
            role: "assistant",
            id: "assistant-complete",
            respondingTo: "reader-2",
            state: "ready",
            text: "回答没有提供论文位置。",
            citations: [],
            retryOfMessageId: null,
            safeMessage: null,
            createdAt: 3,
            updatedAt: 4,
          },
          {
            role: "assistant",
            id: "assistant-streaming",
            respondingTo: "reader-1",
            state: "streaming",
            text: "正在解释",
            citations: [],
            retryOfMessageId: "assistant-failed",
            safeMessage: null,
            createdAt: 4,
            updatedAt: 5,
          },
        ],
      }),
    );
    vi.mocked(testBridge.dispatchReadingCommand).mockRejectedValueOnce(
      new Error("Could not stop response"),
    );
    render(<App bridge={testBridge} viewerFactory={async () => viewer} />);
    await screen.findByText("Atlas Retrieval");
    fireEvent.click(screen.getByRole("button", { name: "Open" }));
    fireEvent.click(await screen.findByRole("button", { name: "Bilingual" }));
    fireEvent.click(screen.getByRole("tab", { name: "Assistant" }));

    expect(await screen.findByText("部分回答")).toBeInTheDocument();
    expect(screen.getByText("No paper location provided.")).toBeInTheDocument();
    const messageList = window.document.querySelector<HTMLElement>(".assistant-messages");
    if (!messageList) {
      throw new Error("assistant message list is missing");
    }
    Object.defineProperties(messageList, {
      clientHeight: { configurable: true, value: 200 },
      scrollHeight: { configurable: true, value: 1000 },
    });
    messageList.scrollTop = 100;
    fireEvent.scroll(messageList);
    await act(async () => {
      await new Promise((resolve) => window.setTimeout(resolve, 350));
    });
    expect(messageList.scrollTop).toBe(100);
    fireEvent.click(screen.getByRole("button", { name: "p. 1" }));
    await waitFor(() => {
      expect(
        window.document.querySelector('[data-bilingual-block-id="block-introduction"]'),
      ).toHaveClass("is-citation-target");
    });
    expect(screen.queryByRole("button", { name: "Retry" })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Stop" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Could not stop response");
    await act(async () => {
      await new Promise((resolve) => window.setTimeout(resolve, 350));
    });
    expect(screen.getByRole("alert")).toHaveTextContent("Could not stop response");

    await waitFor(() => {
      expect(testBridge.dispatchReadingCommand).toHaveBeenCalledWith(
        expect.objectContaining({
          command: {
            type: "reading_assistant",
            command: {
              type: "cancel_response",
              assistantMessageId: "assistant-streaming",
            },
          },
        }),
      );
    });
    fireEvent.click(screen.getByRole("button", { name: "Open p. 1 in PDF" }));
    expect(viewer.setPage).not.toHaveBeenCalled();
    act(() => viewer.emitState({ pageCount: 12 }));
    await waitFor(() => expect(viewer.setPage).toHaveBeenCalledWith(1));
  });

  it("offers retry only for the latest failed assistant attempt", async () => {
    const testBridge = bridge();
    const paper = document();
    vi.mocked(testBridge.queryLibrary).mockResolvedValue({ items: [paper], nextCursor: null });
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
    vi.mocked(testBridge.getParsedDocument).mockResolvedValue({
      parse: parseSnapshot("ready"),
      document: canonicalDocument(),
    });
    vi.mocked(testBridge.getReadingSessionSnapshot).mockResolvedValue(
      readyReaderSession({
        schemaVersion: 1,
        conversationId: "conversation-1",
        activeAssistantMessageId: null,
        latestSelection: null,
        messages: [
          {
            role: "reader",
            id: "reader-1",
            text: "为什么重要？",
            selectionContext: null,
            createdAt: 1,
          },
          {
            role: "assistant",
            id: "assistant-failed",
            respondingTo: "reader-1",
            state: "failed",
            text: "部分回答",
            citations: [],
            retryOfMessageId: null,
            safeMessage: "The response stopped",
            createdAt: 2,
            updatedAt: 3,
          },
        ],
      }),
    );
    render(<App bridge={testBridge} viewerFactory={async () => new FakePdfViewer()} />);
    await screen.findByText("Atlas Retrieval");
    fireEvent.click(screen.getByRole("button", { name: "Open" }));
    fireEvent.click(await screen.findByRole("button", { name: "Bilingual" }));
    fireEvent.click(screen.getByRole("tab", { name: "Assistant" }));
    fireEvent.click(await screen.findByRole("button", { name: "Retry" }));

    await waitFor(() => {
      expect(testBridge.dispatchReadingCommand).toHaveBeenCalledWith(
        expect.objectContaining({
          command: {
            type: "reading_assistant",
            command: { type: "retry_response", userMessageId: "reader-1" },
          },
        }),
      );
    });
  });

  it("restores authoritative chapter focus when a focus command is rejected", async () => {
    const testBridge = bridge();
    const paper = document();
    vi.mocked(testBridge.queryLibrary).mockResolvedValue({ items: [paper], nextCursor: null });
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
    vi.mocked(testBridge.getParsedDocument).mockResolvedValue({
      parse: parseSnapshot("ready"),
      document: canonicalDocument(),
    });
    const snapshot = {
      schemaVersion: 3,
      sessionId: "session-1",
      documentId: "document-1",
      revision: 0,
      lifecycle: "ready" as const,
      parseState: "ready" as const,
      activeChapterId: "chapter-introduction",
      activeJobIds: [],
      providerStatus: {
        mineru: "ready" as const,
        translation: "ready" as const,
        translationModel: "test-model",
      },
      translation: {
        targetLocale: "zh-CN",
        modelId: "test-model",
        activeChapter: null,
        prefetchedChapterId: null,
      },
      readingAssistant: emptyReadingAssistant(),
    };
    vi.mocked(testBridge.getReadingSessionSnapshot).mockResolvedValue(snapshot);
    vi.mocked(testBridge.dispatchReadingCommand).mockResolvedValue({
      commandId: "focus-rejected",
      status: "rejected",
      revision: 0,
      rejection: {
        code: "invalid_input",
        message: "Chapter cannot be focused",
        recoverable: true,
      },
    });
    render(<App bridge={testBridge} viewerFactory={async () => new FakePdfViewer()} />);
    await screen.findByText("Atlas Retrieval");
    fireEvent.click(screen.getByRole("button", { name: "Open" }));
    fireEvent.click(await screen.findByRole("button", { name: "Bilingual" }));

    fireEvent.click(screen.getByRole("button", { name: "02Method" }));

    await waitFor(() => {
      expect(screen.getByRole("button", { name: "01Introduction" })).toHaveClass("is-active");
    });
    expect(screen.getByRole("button", { name: "02Method" })).not.toHaveClass("is-active");
    expect(await screen.findByText("Translation status unavailable")).toHaveAttribute(
      "title",
      "Chapter cannot be focused",
    );
  });

  it("continues polling when focus changes between two active translation jobs", async () => {
    const testBridge = bridge();
    const paper = document();
    vi.mocked(testBridge.queryLibrary).mockResolvedValue({ items: [paper], nextCursor: null });
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
    vi.mocked(testBridge.getParsedDocument).mockResolvedValue({
      parse: parseSnapshot("ready"),
      document: canonicalDocument(),
    });
    const activeSnapshot = (chapterId: string, revision: number): SessionSnapshot => ({
      schemaVersion: 3,
      sessionId: "session-1",
      documentId: "document-1",
      revision,
      lifecycle: "ready",
      parseState: "ready",
      activeChapterId: chapterId,
      activeJobIds: [`job-${chapterId}`],
      providerStatus: {
        mineru: "ready",
        translation: "ready",
        translationModel: "test-model",
      },
      translation: {
        targetLocale: "zh-CN",
        modelId: "test-model",
        prefetchedChapterId: null,
        activeChapter: {
          chapterId,
          state: "translating",
          progress: 0,
          jobId: `job-${chapterId}`,
          jobActive: true,
          prefetched: false,
          safeMessage: null,
          blocks: [],
        },
      },
      readingAssistant: emptyReadingAssistant(),
    });
    const methodFocused = { current: false };
    vi.mocked(testBridge.getReadingSessionSnapshot).mockImplementation(async () =>
      methodFocused.current
        ? activeSnapshot("chapter-method", 1)
        : activeSnapshot("chapter-introduction", 0),
    );
    vi.mocked(testBridge.dispatchReadingCommand).mockImplementation(async (input) => {
      methodFocused.current = true;
      return {
        commandId: input.commandId,
        status: "accepted",
        revision: 1,
        rejection: null,
      };
    });
    render(<App bridge={testBridge} viewerFactory={async () => new FakePdfViewer()} />);
    await screen.findByText("Atlas Retrieval");
    fireEvent.click(screen.getByRole("button", { name: "Open" }));
    fireEvent.click(await screen.findByRole("button", { name: "Bilingual" }));
    await waitFor(() => {
      expect(screen.getByRole("button", { name: "01Introduction" })).toHaveClass("is-active");
    });
    await new Promise((resolve) => window.setTimeout(resolve, 900));

    fireEvent.click(screen.getByRole("button", { name: "02Method" }));
    await new Promise((resolve) => window.setTimeout(resolve, 900));
    const callsAfterFirstPollWindow = vi.mocked(testBridge.getReadingSessionSnapshot).mock.calls
      .length;
    await new Promise((resolve) => window.setTimeout(resolve, 900));

    expect(testBridge.getReadingSessionSnapshot).toHaveBeenCalledTimes(
      callsAfterFirstPollWindow + 1,
    );
  });

  it("continues polling when the already-active chapter is focused again", async () => {
    const testBridge = bridge();
    const paper = document();
    vi.mocked(testBridge.queryLibrary).mockResolvedValue({ items: [paper], nextCursor: null });
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
    vi.mocked(testBridge.getParsedDocument).mockResolvedValue({
      parse: parseSnapshot("ready"),
      document: canonicalDocument(),
    });
    const snapshot: SessionSnapshot = {
      schemaVersion: 3,
      sessionId: "session-1",
      documentId: "document-1",
      revision: 1,
      lifecycle: "ready",
      parseState: "ready",
      activeChapterId: "chapter-introduction",
      activeJobIds: ["job-introduction"],
      providerStatus: {
        mineru: "ready",
        translation: "ready",
        translationModel: "test-model",
      },
      translation: {
        targetLocale: "zh-CN",
        modelId: "test-model",
        prefetchedChapterId: null,
        activeChapter: {
          chapterId: "chapter-introduction",
          state: "translating",
          progress: 0,
          jobId: "job-introduction",
          jobActive: true,
          prefetched: false,
          safeMessage: null,
          blocks: [],
        },
      },
      readingAssistant: emptyReadingAssistant(),
    };
    vi.mocked(testBridge.getReadingSessionSnapshot).mockResolvedValue(snapshot);
    render(<App bridge={testBridge} viewerFactory={async () => new FakePdfViewer()} />);
    await screen.findByText("Atlas Retrieval");
    fireEvent.click(screen.getByRole("button", { name: "Open" }));
    fireEvent.click(await screen.findByRole("button", { name: "Bilingual" }));
    await waitFor(() => {
      expect(screen.getByRole("button", { name: "01Introduction" })).toHaveClass("is-active");
    });
    await new Promise((resolve) => window.setTimeout(resolve, 900));

    fireEvent.click(screen.getByRole("button", { name: "01Introduction" }));
    await new Promise((resolve) => window.setTimeout(resolve, 900));
    const callsAfterFirstPollWindow = vi.mocked(testBridge.getReadingSessionSnapshot).mock.calls
      .length;
    await new Promise((resolve) => window.setTimeout(resolve, 900));

    expect(testBridge.getReadingSessionSnapshot).toHaveBeenCalledTimes(
      callsAfterFirstPollWindow + 1,
    );
  });

  it("requires confirmation before recovering an unknown parse by re-upload", async () => {
    const testBridge = bridge();
    const paper = document();
    const unknown = parseSnapshot("status_unknown", {
      safeMessage: "Atlas could not confirm the remote status.",
    });
    vi.mocked(testBridge.queryLibrary).mockResolvedValue({ items: [paper], nextCursor: null });
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
    vi.mocked(testBridge.getParsedDocument).mockResolvedValue({
      parse: unknown,
      document: null,
    });
    vi.mocked(testBridge.retryRemoteParse).mockResolvedValue(unknown);
    vi.mocked(testBridge.reuploadDocument).mockResolvedValue(
      parseSnapshot("uploading", { backend: "cloud_mineru", progress: 0 }),
    );
    render(<App bridge={testBridge} viewerFactory={async () => new FakePdfViewer()} />);
    await screen.findByText("Atlas Retrieval");
    fireEvent.click(screen.getByRole("button", { name: "Open" }));

    expect(await screen.findByText("Remote status unknown")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Query remote" }));
    await waitFor(() => {
      expect(testBridge.retryRemoteParse).toHaveBeenCalledWith("document-1");
    });
    fireEvent.click(screen.getByRole("button", { name: "Re-upload" }));
    await waitFor(() => {
      expect(testBridge.confirmParseReupload).toHaveBeenCalledOnce();
      expect(testBridge.reuploadDocument).toHaveBeenCalledWith("document-1", "session-1");
    });
  });

  it("continues polling after a transient parse-status failure", async () => {
    const testBridge = bridge();
    const paper = document();
    vi.mocked(testBridge.queryLibrary).mockResolvedValue({ items: [paper], nextCursor: null });
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
    vi.mocked(testBridge.getParsedDocument)
      .mockResolvedValueOnce({
        parse: parseSnapshot("processing", {
          backend: "cloud_mineru",
          progress: 0.25,
        }),
        document: null,
      })
      .mockRejectedValueOnce(new Error("temporary storage contention"))
      .mockResolvedValue({
        parse: parseSnapshot("ready", {
          backend: "cloud_mineru",
          progress: 1,
        }),
        document: canonicalDocument(),
      });
    render(<App bridge={testBridge} viewerFactory={async () => new FakePdfViewer()} />);
    await screen.findByText("Atlas Retrieval");
    fireEvent.click(screen.getByRole("button", { name: "Open" }));

    expect(await screen.findByText("Cloud parsing 25%")).toBeInTheDocument();
    expect(
      await screen.findByText("Cloud structure ready", undefined, { timeout: 2_500 }),
    ).toBeInTheDocument();
    expect(testBridge.getParsedDocument).toHaveBeenCalledTimes(3);
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
