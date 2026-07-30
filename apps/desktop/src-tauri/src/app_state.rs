use std::sync::Arc;

use atlas_library::LibraryModule;
use atlas_reading_session::ReadingSessionModule;
pub struct AppState {
    pub library: Arc<dyn LibraryModule>,
    pub reading_session: Arc<dyn ReadingSessionModule>,
}

impl AppState {
    #[must_use]
    pub fn new(
        library: Arc<dyn LibraryModule>,
        reading_session: Arc<dyn ReadingSessionModule>,
    ) -> Self {
        Self {
            library,
            reading_session,
        }
    }
}
