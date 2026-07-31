import { vi } from "vitest";

import type { AtlasBridge } from "../bridge";

export function testBridge(): AtlasBridge {
  const sessionSnapshot = {
    schemaVersion: 1,
    sessionId: "session-1",
    documentId: "document-1",
    revision: 0,
    lifecycle: "ready" as const,
    parseState: "not_started" as const,
    activeChapterId: null,
    activeJobIds: [],
    providerStatus: {
      mineru: "not_configured" as const,
      translation: "not_configured" as const,
      translationModel: null,
    },
    translation: {
      targetLocale: "zh-CN",
      modelId: null,
      activeChapter: null,
      prefetchedChapterId: null,
    },
  };
  return {
    pickPdfPaths: vi.fn().mockResolvedValue([]),
    subscribePdfDrops: vi.fn().mockResolvedValue(() => undefined),
    confirmDocumentRemoval: vi.fn().mockResolvedValue(true),
    importPdf: vi.fn(),
    queryLibrary: vi.fn().mockResolvedValue({
      items: [],
      nextCursor: null,
    }),
    refreshLibrarySources: vi.fn().mockResolvedValue({
      updated: [],
    }),
    relocateDocument: vi.fn(),
    removeDocument: vi.fn(),
    openReader: vi.fn(),
    saveReadingPosition: vi.fn(),
    closeReader: vi.fn().mockResolvedValue(undefined),
    getParsedDocument: vi.fn().mockResolvedValue({
      parse: {
        state: "not_started",
        backend: null,
        progress: null,
        parseOperationId: null,
        automaticCloudParsingEnabled: false,
        safeMessage: null,
      },
      document: null,
    }),
    retryRemoteParse: vi.fn(),
    confirmParseReupload: vi.fn().mockResolvedValue(true),
    reuploadDocument: vi.fn(),
    parseAssetUrl: vi.fn(
      (documentId: string, artifactId: string, relativePath: string) =>
        `atlas-artifact://localhost/${documentId}/${artifactId}/${relativePath}`,
    ),
    getProviderSettings: vi.fn().mockResolvedValue({
      mineruEndpoint: null,
      mineruHasSecret: false,
      mineruAutomaticCloudParsingEnabled: false,
      translationBaseUrl: null,
      translationModelId: null,
      translationHasSecret: false,
      contextWindowOverride: null,
    }),
    saveMineruSettings: vi.fn(),
    saveTranslationSettings: vi.fn(),
    testProviderConnection: vi.fn(),
    deleteProviderSecret: vi.fn().mockResolvedValue(undefined),
    openReadingSession: vi.fn().mockResolvedValue({
      sessionId: "session-1",
      restored: false,
      snapshot: sessionSnapshot,
    }),
    getReadingSessionSnapshot: vi.fn().mockResolvedValue(sessionSnapshot),
    dispatchReadingCommand: vi.fn().mockImplementation(async (input) => ({
      commandId: input.commandId,
      status: "accepted",
      revision: 1,
      rejection: null,
    })),
    closeReadingSession: vi.fn().mockResolvedValue(undefined),
  };
}
