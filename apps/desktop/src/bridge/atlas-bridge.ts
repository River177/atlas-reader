import type {
  CommandId,
  CommandReceipt,
  DocumentId,
  DocumentSummary,
  ImportPdfResult,
  LibraryPage,
  LibraryQuery,
  OpenSessionInput,
  OpenSessionResult,
  ReadingCommand,
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

export interface AtlasBridge {
  pickPdfPaths(multiple: boolean): Promise<string[]>;
  subscribePdfDrops(listener: (event: PdfDropEvent) => void): Promise<Unsubscribe>;
  confirmDocumentRemoval(title: string): Promise<boolean>;
  importPdf(path: string): Promise<ImportPdfResult>;
  queryLibrary(input: LibraryQuery): Promise<LibraryPage>;
  refreshLibrarySources(): Promise<RefreshSourcesResult>;
  relocateDocument(documentId: DocumentId, newPath: string): Promise<DocumentSummary>;
  removeDocument(documentId: DocumentId): Promise<void>;
  openReadingSession(input: OpenSessionInput): Promise<OpenSessionResult>;
  dispatchReadingCommand(input: DispatchReadingCommandInput): Promise<CommandReceipt>;
  closeReadingSession(sessionId: SessionId): Promise<void>;
}
