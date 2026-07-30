import type { AtlasBridge } from "./atlas-bridge";

function unavailable(method: string): never {
  throw new Error(`${method} is only available inside the Atlas Reader desktop app`);
}

export const browserBridge: AtlasBridge = {
  async pickPdfPaths() {
    return [];
  },
  async subscribePdfDrops() {
    return () => undefined;
  },
  async confirmDocumentRemoval(title) {
    return window.confirm(`Remove “${title}” from Atlas Reader? The PDF file will be kept.`);
  },
  async importPdf() {
    return unavailable("importPdf");
  },
  async queryLibrary() {
    return {
      items: [],
      nextCursor: null,
    };
  },
  async refreshLibrarySources() {
    return {
      updated: [],
    };
  },
  async relocateDocument() {
    return unavailable("relocateDocument");
  },
  async removeDocument() {
    return unavailable("removeDocument");
  },
  async openReader() {
    return unavailable("openReader");
  },
  async saveReadingPosition() {
    return unavailable("saveReadingPosition");
  },
  async closeReader() {
    return unavailable("closeReader");
  },
  async getProviderSettings() {
    return {
      mineruEndpoint: null,
      mineruHasSecret: false,
      mineruAutomaticCloudParsingEnabled: false,
      translationBaseUrl: null,
      translationModelId: null,
      translationHasSecret: false,
      contextWindowOverride: null,
    };
  },
  async saveMineruSettings() {
    return unavailable("saveMineruSettings");
  },
  async saveTranslationSettings() {
    return unavailable("saveTranslationSettings");
  },
  async testProviderConnection() {
    return unavailable("testProviderConnection");
  },
  async deleteProviderSecret() {
    return unavailable("deleteProviderSecret");
  },
  async openReadingSession() {
    return unavailable("openReadingSession");
  },
  async dispatchReadingCommand() {
    return unavailable("dispatchReadingCommand");
  },
  async closeReadingSession() {
    return unavailable("closeReadingSession");
  },
};
