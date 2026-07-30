use std::{
    fs::{self, File},
    io::{BufReader, Read},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use atlas_domain::{AtlasError, DocumentFileState};
use lopdf::Document;
use sha2::{Digest, Sha256};

use crate::{DocumentRecord, DocumentSourceUpdate, LibraryLimits};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct InspectedPdf {
    pub canonical_path: String,
    pub sha256: String,
    pub title: String,
    pub authors: Vec<String>,
    pub page_count: u32,
    pub file_size_bytes: u64,
    pub file_mtime_ms: u64,
}

pub(crate) fn inspect_pdf(
    path: PathBuf,
    limits: LibraryLimits,
) -> Result<InspectedPdf, AtlasError> {
    ensure_pdf_extension(&path)?;
    let canonical_path = canonical_pdf_path(&path)?;
    let initial_metadata = readable_file_metadata(&canonical_path)?;
    if initial_metadata.len() > limits.max_file_size_bytes {
        return Err(AtlasError::pdf_too_large(
            limits.max_file_size_bytes / (1024 * 1024),
        ));
    }

    let (sha256, has_pdf_header) = hash_file(&canonical_path)?;
    if !has_pdf_header {
        return Err(AtlasError::invalid_pdf(
            "The selected file does not contain a PDF header",
        ));
    }

    let metadata = Document::load_metadata(&canonical_path)
        .map_err(|error| AtlasError::invalid_pdf(format!("The PDF cannot be read: {error}")))?;
    if metadata.page_count == 0 {
        return Err(AtlasError::invalid_pdf(
            "The PDF does not contain any pages",
        ));
    }
    if metadata.page_count > limits.max_pages {
        return Err(AtlasError::pdf_too_many_pages(limits.max_pages));
    }

    let final_metadata = readable_file_metadata(&canonical_path)?;
    let initial_mtime = modified_ms(&initial_metadata)?;
    let final_mtime = modified_ms(&final_metadata)?;
    if initial_metadata.len() != final_metadata.len() || initial_mtime != final_mtime {
        return Err(AtlasError::source_unreadable(
            "The PDF changed while Atlas Reader was importing it",
        ));
    }

    let path_string = canonical_path
        .to_str()
        .ok_or_else(|| AtlasError::source_unreadable("The PDF path is not valid UTF-8"))?
        .to_owned();
    let fallback_title = canonical_path
        .file_stem()
        .and_then(std::ffi::OsStr::to_str)
        .map(normalize_metadata_text)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "Untitled paper".to_owned());
    let title = metadata
        .title
        .map(|value| normalize_metadata_text(&value))
        .filter(|value| !value.is_empty())
        .unwrap_or(fallback_title);
    let authors = metadata
        .author
        .map(|value| normalize_metadata_text(&value))
        .filter(|value| !value.is_empty())
        .into_iter()
        .collect();

    Ok(InspectedPdf {
        canonical_path: path_string,
        sha256,
        title,
        authors,
        page_count: metadata.page_count,
        file_size_bytes: final_metadata.len(),
        file_mtime_ms: final_mtime,
    })
}

pub(crate) fn inspect_existing_source(record: &DocumentRecord) -> DocumentSourceUpdate {
    let path = Path::new(&record.file_path);
    let metadata = match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => metadata,
        Ok(_) => {
            return source_update(record, DocumentFileState::Unreadable);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return source_update(record, DocumentFileState::Missing);
        }
        Err(_) => {
            return source_update(record, DocumentFileState::Unreadable);
        }
    };
    let mtime = match modified_ms(&metadata) {
        Ok(value) => value,
        Err(_) => return source_update(record, DocumentFileState::Unreadable),
    };

    if metadata.len() == record.file_size_bytes
        && mtime == record.file_mtime_ms
        && record.file_state == DocumentFileState::Available
    {
        return source_update(record, DocumentFileState::Available);
    }

    let file_state = match hash_file(path) {
        Ok((sha256, true)) if sha256 == record.sha256 => DocumentFileState::Available,
        Ok(_) => DocumentFileState::Changed,
        Err(_) => DocumentFileState::Unreadable,
    };
    DocumentSourceUpdate {
        file_path: record.file_path.clone(),
        file_size_bytes: metadata.len(),
        file_mtime_ms: mtime,
        file_state,
    }
}

fn ensure_pdf_extension(path: &Path) -> Result<(), AtlasError> {
    let is_pdf = path
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"));
    if is_pdf {
        Ok(())
    } else {
        Err(AtlasError::unsupported_file_type())
    }
}

fn canonical_pdf_path(path: &Path) -> Result<PathBuf, AtlasError> {
    match fs::canonicalize(path) {
        Ok(path) => Ok(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(AtlasError::source_missing())
        }
        Err(error) => Err(AtlasError::source_unreadable(format!(
            "The PDF path cannot be opened: {error}"
        ))),
    }
}

fn readable_file_metadata(path: &Path) -> Result<fs::Metadata, AtlasError> {
    let metadata = fs::metadata(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            AtlasError::source_missing()
        } else {
            AtlasError::source_unreadable(format!("The PDF cannot be opened: {error}"))
        }
    })?;
    if !metadata.is_file() {
        return Err(AtlasError::source_unreadable(
            "The selected path is not a regular file",
        ));
    }
    Ok(metadata)
}

fn hash_file(path: &Path) -> Result<(String, bool), AtlasError> {
    let file = File::open(path).map_err(|error| {
        AtlasError::source_unreadable(format!("The PDF cannot be read: {error}"))
    })?;
    let mut reader = BufReader::with_capacity(64 * 1024, file);
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut header = Vec::with_capacity(1024);

    loop {
        let read = reader.read(&mut buffer).map_err(|error| {
            AtlasError::source_unreadable(format!("The PDF cannot be read: {error}"))
        })?;
        if read == 0 {
            break;
        }
        if header.len() < 1024 {
            let remaining = 1024 - header.len();
            header.extend_from_slice(&buffer[..read.min(remaining)]);
        }
        hasher.update(&buffer[..read]);
    }

    let has_pdf_header = header.windows(5).any(|window| window == b"%PDF-");
    Ok((hex::encode(hasher.finalize()), has_pdf_header))
}

fn modified_ms(metadata: &fs::Metadata) -> Result<u64, AtlasError> {
    let modified = metadata.modified().map_err(|error| {
        AtlasError::source_unreadable(format!("The PDF modification time is unavailable: {error}"))
    })?;
    let duration = modified.duration_since(UNIX_EPOCH).map_err(|_| {
        AtlasError::source_unreadable("The PDF modification time predates the Unix epoch")
    })?;
    u64::try_from(duration.as_millis())
        .map_err(|_| AtlasError::source_unreadable("The PDF modification time is out of range"))
}

fn normalize_metadata_text(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(1000)
        .collect()
}

fn source_update(record: &DocumentRecord, file_state: DocumentFileState) -> DocumentSourceUpdate {
    DocumentSourceUpdate {
        file_path: record.file_path.clone(),
        file_size_bytes: record.file_size_bytes,
        file_mtime_ms: record.file_mtime_ms,
        file_state,
    }
}
