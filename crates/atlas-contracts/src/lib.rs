pub use atlas_domain::{
    AssetMimeType, AssistantMessageState, AtlasError, AtlasErrorCode, BlockId, BlockKind,
    BlockTranslationState, CanonicalAsset, CanonicalBlock, CanonicalChapter, CanonicalDocument,
    ChapterId, ChapterRole, ChapterTranslationView, CitationId, CitationTarget, CommandId,
    CommandReceipt, CommandStatus, ConnectionTestCode, ConnectionTestResult, ContentAtom,
    ConversationId, CoordinateSpace, DocumentFileState, DocumentId, DocumentSummary,
    ImportPdfResult, JobId, LibraryPage, LibraryQuery, LibrarySort, MineruSettingsInput,
    OpenSessionInput, OpenSessionResult, OpenedReaderDocument, PageBoundingBox, ParseBackend,
    ParseSnapshot, ParseState, ParsedDocumentView, ParserIdentity, ProviderKind, ProviderState,
    ProviderStatusSnapshot, PublicProviderSettings, READING_ASSISTANT_SCHEMA_VERSION,
    ReaderSourceToken, ReadingAssistantCommand, ReadingAssistantSnapshot, ReadingCommand,
    ReadingMessageId, ReadingMessageView, ReadingPosition, ReadingPositionUpdate,
    RefreshSourcesResult, SelectionContext, SelectionContextInput, SessionId, SessionLifecycle,
    SessionSnapshot, StructuredContent, TableCell, TranslatedBlockView, TranslationSettingsInput,
    TranslationSnapshot, TranslationState,
};

pub const CONTRACT_SCHEMA_VERSION: u16 = 2;
