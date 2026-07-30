import { vi } from "vitest";

import type { AtlasBridge } from "../bridge";

export function testBridge(): AtlasBridge {
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
    closeReader: vi.fn(),
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
    openReadingSession: vi.fn(),
    dispatchReadingCommand: vi.fn(),
    closeReadingSession: vi.fn(),
  };
}
