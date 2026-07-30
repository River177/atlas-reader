use std::sync::Arc;

use atlas_document_reader::DocumentReaderModule;
use atlas_library::LibraryModule;
use atlas_reading_session::ReadingSessionModule;
pub struct AppState {
    pub library: Arc<dyn LibraryModule>,
    pub document_reader: Arc<dyn DocumentReaderModule>,
    pub reading_session: Arc<dyn ReadingSessionModule>,
}

impl AppState {
    #[must_use]
    pub fn new(
        library: Arc<dyn LibraryModule>,
        document_reader: Arc<dyn DocumentReaderModule>,
        reading_session: Arc<dyn ReadingSessionModule>,
    ) -> Self {
        Self {
            library,
            document_reader,
            reading_session,
        }
    }
}
