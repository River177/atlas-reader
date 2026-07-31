import type {
  CommandId,
  CommandReceipt,
  ConnectionTestResult,
  DocumentId,
  DocumentSummary,
  ImportPdfResult,
  LibraryPage,
  LibraryQuery,
  MineruSettingsInput,
  OpenedReaderDocument,
  OpenSessionInput,
  OpenSessionResult,
  ParseSnapshot,
  ParsedDocumentView,
  ProviderKind,
  PublicProviderSettings,
  ReadingCommand,
  ReadingPosition,
  ReadingPositionUpdate,
  ReaderSourceToken,
  RefreshSourcesResult,
  SessionId,
  SessionSnapshot,
  TranslationSettingsInput,
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
  closeReader(sourceToken: ReaderSourceToken, finalPosition?: ReadingPositionUpdate): Promise<void>;
  getParsedDocument(documentId: DocumentId): Promise<ParsedDocumentView>;
  retryRemoteParse(documentId: DocumentId): Promise<ParseSnapshot>;
  confirmParseReupload(): Promise<boolean>;
  reuploadDocument(documentId: DocumentId, sessionId: SessionId): Promise<ParseSnapshot>;
  parseAssetUrl(documentId: DocumentId, artifactId: string, relativePath: string): string;
  getProviderSettings(): Promise<PublicProviderSettings>;
  saveMineruSettings(input: MineruSettingsInput): Promise<ConnectionTestResult>;
  saveTranslationSettings(input: TranslationSettingsInput): Promise<ConnectionTestResult>;
  testProviderConnection(provider: ProviderKind): Promise<ConnectionTestResult>;
  deleteProviderSecret(provider: ProviderKind): Promise<void>;
  openReadingSession(input: OpenSessionInput): Promise<OpenSessionResult>;
  getReadingSessionSnapshot(sessionId: SessionId): Promise<SessionSnapshot>;
  dispatchReadingCommand(input: DispatchReadingCommandInput): Promise<CommandReceipt>;
  closeReadingSession(sessionId: SessionId): Promise<void>;
}
