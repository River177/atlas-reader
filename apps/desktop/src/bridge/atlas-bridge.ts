import type {
  CommandId,
  CommandReceipt,
  LibraryPage,
  LibraryQuery,
  OpenSessionInput,
  OpenSessionResult,
  ReadingCommand,
  SessionId,
} from "@atlas/contracts";

export interface DispatchReadingCommandInput {
  sessionId: SessionId;
  commandId: CommandId;
  expectedRevision?: number;
  command: ReadingCommand;
}

export interface AtlasBridge {
  queryLibrary(input: LibraryQuery): Promise<LibraryPage>;
  openReadingSession(input: OpenSessionInput): Promise<OpenSessionResult>;
  dispatchReadingCommand(input: DispatchReadingCommandInput): Promise<CommandReceipt>;
  closeReadingSession(sessionId: SessionId): Promise<void>;
}
