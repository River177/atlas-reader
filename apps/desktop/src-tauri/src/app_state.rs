use std::sync::Arc;

use atlas_document_reader::DocumentReaderModule;
use atlas_library::LibraryModule;
use atlas_parse::ParseModule;
use atlas_provider_settings::ProviderSettingsModule;
use atlas_reading_session::ReadingSessionModule;
pub struct AppState {
    pub library: Arc<dyn LibraryModule>,
    pub document_reader: Arc<dyn DocumentReaderModule>,
    pub provider_settings: Arc<dyn ProviderSettingsModule>,
    pub parse: Arc<dyn ParseModule>,
    pub reading_session: Arc<dyn ReadingSessionModule>,
}

impl AppState {
    #[must_use]
    pub fn new(
        library: Arc<dyn LibraryModule>,
        document_reader: Arc<dyn DocumentReaderModule>,
        provider_settings: Arc<dyn ProviderSettingsModule>,
        parse: Arc<dyn ParseModule>,
        reading_session: Arc<dyn ReadingSessionModule>,
    ) -> Self {
        Self {
            library,
            document_reader,
            provider_settings,
            parse,
            reading_session,
        }
    }
}
