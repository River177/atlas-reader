use async_trait::async_trait;
use atlas_domain::{
    AssistantMessageState, AtlasError, CitationTarget, ConversationId, DocumentId,
    ReadingAssistantSnapshot, ReadingMessageId, SelectionContext,
};

#[derive(Clone, Debug)]
pub struct NewReaderMessage {
    pub id: ReadingMessageId,
    pub text: String,
    pub selection: Option<SelectionContext>,
    pub created_at: u64,
}

#[derive(Clone, Debug)]
pub struct NewAssistantResponse {
    pub id: ReadingMessageId,
    pub responding_to: ReadingMessageId,
    pub retry_of_message_id: Option<ReadingMessageId>,
    pub endpoint_fingerprint: String,
    pub model_id: String,
    pub created_at: u64,
}

#[derive(Clone, Debug)]
pub struct QueuedReadingResponse {
    pub conversation_id: ConversationId,
    pub document_id: DocumentId,
    pub reader: Option<NewReaderMessage>,
    pub assistant: NewAssistantResponse,
}

#[derive(Clone, Debug)]
pub struct AssistantResponseCheckpoint {
    pub conversation_id: ConversationId,
    pub assistant_message_id: ReadingMessageId,
    pub state: AssistantMessageState,
    pub text: String,
    pub citations: Vec<CitationTarget>,
    pub error_code: Option<String>,
    pub safe_message: Option<String>,
    pub updated_at: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoverableReadingResponse {
    pub conversation_id: ConversationId,
    pub document_id: DocumentId,
    pub assistant_message_id: ReadingMessageId,
    pub responding_to: ReadingMessageId,
}

#[async_trait]
pub trait ReadingAssistantStore: Send + Sync {
    async fn view(&self, document_id: &DocumentId) -> Result<ReadingAssistantSnapshot, AtlasError>;

    /// Atomically creates the document conversation if needed, optionally
    /// inserts one reader message, and queues its assistant response.
    async fn queue_response(
        &self,
        response: &QueuedReadingResponse,
    ) -> Result<ReadingAssistantSnapshot, AtlasError>;

    /// Atomically checkpoints response text, state, error and the complete
    /// validated citation set.
    async fn checkpoint_response(
        &self,
        checkpoint: &AssistantResponseCheckpoint,
    ) -> Result<(), AtlasError>;

    async fn clear(&self, document_id: &DocumentId) -> Result<bool, AtlasError>;

    async fn recoverable_responses(&self) -> Result<Vec<RecoverableReadingResponse>, AtlasError>;
}
