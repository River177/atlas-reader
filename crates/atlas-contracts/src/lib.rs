pub use atlas_domain::{
    AtlasError, AtlasErrorCode, BlockId, ChapterId, CommandId, CommandReceipt, CommandStatus,
    DocumentId, DocumentSummary, JobId, LibraryPage, LibraryQuery, LibrarySort, OpenSessionInput,
    OpenSessionResult, ParseState, ProviderState, ProviderStatusSnapshot, ReadingCommand,
    SessionId, SessionLifecycle, SessionSnapshot,
};

pub const CONTRACT_SCHEMA_VERSION: u16 = 1;
