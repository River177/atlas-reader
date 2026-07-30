import type {
  CommandReceipt,
  DocumentSummary,
  ImportPdfResult,
  LibraryPage,
  OpenedReaderDocument,
  OpenSessionResult,
  ReadingPosition,
  RefreshSourcesResult,
} from "@atlas/contracts";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { confirm, open } from "@tauri-apps/plugin-dialog";

import type { AtlasBridge, DispatchReadingCommandInput, PdfDropEvent } from "./atlas-bridge";

const readerProtocol = "atlas-reader";

export const tauriBridge: AtlasBridge = {
  async pickPdfPaths(multiple) {
    const selected = await open({
      title: multiple ? "Import papers" : "Locate paper",
      multiple,
      directory: false,
      filters: [
        {
          name: "PDF documents",
          extensions: ["pdf"],
        },
      ],
      fileAccessMode: "scoped",
    });
    if (selected === null) {
      return [];
    }
    return Array.isArray(selected) ? selected : [selected];
  },
  subscribePdfDrops(listener) {
    return getCurrentWebview().onDragDropEvent((event) => {
      const payload = event.payload;
      let libraryEvent: PdfDropEvent | undefined;
      if (payload.type === "enter") {
        libraryEvent = { type: "enter", paths: payload.paths };
      } else if (payload.type === "drop") {
        libraryEvent = { type: "drop", paths: payload.paths };
      } else if (payload.type === "leave") {
        libraryEvent = { type: "leave" };
      }
      if (libraryEvent) {
        listener(libraryEvent);
      }
    });
  },
  confirmDocumentRemoval(title) {
    return confirm(`Remove “${title}” from Atlas Reader? The original PDF will not be deleted.`, {
      title: "Remove paper",
      kind: "warning",
    });
  },
  importPdf(path) {
    return invoke<ImportPdfResult>("library_import_pdf", { path });
  },
  queryLibrary(input) {
    return invoke<LibraryPage>("library_query", { input });
  },
  refreshLibrarySources() {
    return invoke<RefreshSourcesResult>("library_refresh_sources");
  },
  relocateDocument(documentId, newPath) {
    return invoke<DocumentSummary>("library_relocate", {
      input: { documentId, newPath },
    });
  },
  removeDocument(documentId) {
    return invoke<void>("library_remove", {
      input: { documentId },
    });
  },
  async openReader(documentId) {
    const opened = await invoke<OpenedReaderDocument>("reader_open", {
      input: { documentId },
    });
    return {
      ...opened,
      sourceUrl: convertFileSrc(opened.sourceToken, readerProtocol),
    };
  },
  saveReadingPosition(sourceToken, position) {
    return invoke<ReadingPosition>("reader_save_position", {
      input: { sourceToken, position },
    });
  },
  closeReader(sourceToken, finalPosition) {
    return invoke<void>("reader_close", {
      input: {
        sourceToken,
        finalPosition: finalPosition ?? null,
      },
    });
  },
  openReadingSession(input) {
    return invoke<OpenSessionResult>("reading_session_open", { input });
  },
  dispatchReadingCommand(input: DispatchReadingCommandInput) {
    return invoke<CommandReceipt>("reading_session_dispatch", { input });
  },
  closeReadingSession(sessionId) {
    return invoke<void>("reading_session_close", { sessionId });
  },
};
