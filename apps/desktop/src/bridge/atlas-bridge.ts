import type {
  CommandId,
  CommandReceipt,
  DocumentId,
  DocumentSummary,
  ImportPdfResult,
  LibraryPage,
  LibraryQuery,
  OpenedReaderDocument,
  OpenSessionInput,
  OpenSessionResult,
  ReadingCommand,
  ReadingPosition,
  ReadingPositionUpdate,
  ReaderSourceToken,
  RefreshSourcesResult,
  SessionId,
} from "@atlas/contracts";

export interface DispatchReadingCommandInput {
  sessionId: SessionId;
  commandId: CommandId;
  expectedRevision?: number;
  command: ReadingCommand;
}

export type PdfDropEvent =
  { type: "enter"; paths: string[] } | { type: "drop"; paths: string[] } | { type: "leave" };

export type Unsubscribe = () => void;

export interface OpenedReaderView extends OpenedReaderDocument {
  sourceUrl: string;
}

export interface AtlasBridge {
  pickPdfPaths(multiple: boolean): Promise<string[]>;
  subscribePdfDrops(listener: (event: PdfDropEvent) => void): Promise<Unsubscribe>;
  confirmDocumentRemoval(title: string): Promise<boolean>;
  importPdf(path: string): Promise<ImportPdfResult>;
  queryLibrary(input: LibraryQuery): Promise<LibraryPage>;
  refreshLibrarySources(): Promise<RefreshSourcesResult>;
  relocateDocument(documentId: DocumentId, newPath: string): Promise<DocumentSummary>;
  removeDocument(documentId: DocumentId): Promise<void>;
  openReader(documentId: DocumentId): Promise<OpenedReaderView>;
  saveReadingPosition(
    sourceToken: ReaderSourceToken,
    position: ReadingPositionUpdate,
  ): Promise<ReadingPosition>;
  closeReader(
    sourceToken: ReaderSourceToken,
    finalPosition?: ReadingPositionUpdate,
  ): Promise<void>;
  openReadingSession(input: OpenSessionInput): Promise<OpenSessionResult>;
  dispatchReadingCommand(input: DispatchReadingCommandInput): Promise<CommandReceipt>;
  closeReadingSession(sessionId: SessionId): Promise<void>;
}
