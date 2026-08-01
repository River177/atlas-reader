use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::Arc,
};

use atlas_document_reader::ReaderSourceRegistry;
use atlas_domain::{AtlasError, DocumentId, DocumentSummary, ImportPdfResult};
use atlas_library::{DocumentStore, LibraryModule};
use axum::extract::multipart::Field;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::{
    fs,
    io::{AsyncReadExt, AsyncWriteExt, BufReader, BufWriter},
    sync::Mutex,
};
use uuid::Uuid;

const MAX_UPLOAD_BYTES: u64 = 200 * 1024 * 1024;

struct TemporaryUpload {
    path: PathBuf,
    committed: bool,
}

impl TemporaryUpload {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            committed: false,
        }
    }

    fn commit(&mut self) {
        self.committed = true;
    }
}

impl Drop for TemporaryUpload {
    fn drop(&mut self) {
        if !self.committed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

struct QuarantineGuard {
    path: PathBuf,
    armed: bool,
}

impl QuarantineGuard {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for QuarantineGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

struct PersistedUpload {
    path: PathBuf,
    digest: String,
    created: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct MovedPath {
    original: PathBuf,
    moved: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct DeletionManifest {
    document_id: DocumentId,
    pdf: Option<MovedPath>,
    artifacts: Option<MovedPath>,
    committed: bool,
}

#[derive(Clone)]
pub struct ManagedImports {
    library: Arc<dyn LibraryModule>,
    documents: Arc<dyn DocumentStore>,
    root: PathBuf,
    artifact_root: PathBuf,
    sources: Arc<ReaderSourceRegistry>,
    transitions: Arc<Mutex<()>>,
}

impl std::fmt::Debug for ManagedImports {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ManagedImports")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

impl ManagedImports {
    pub fn new(
        library: Arc<dyn LibraryModule>,
        documents: Arc<dyn DocumentStore>,
        root: PathBuf,
        artifact_root: PathBuf,
        sources: Arc<ReaderSourceRegistry>,
    ) -> Self {
        Self {
            library,
            documents,
            root,
            artifact_root,
            sources,
            transitions: Arc::new(Mutex::new(())),
        }
    }

    pub async fn prepare(&self) -> Result<(), AtlasError> {
        let staging = self.root.join(".staging");
        if fs::try_exists(&staging)
            .await
            .map_err(|error| AtlasError::storage(error.to_string()))?
        {
            fs::remove_dir_all(&staging)
                .await
                .map_err(|error| AtlasError::storage(error.to_string()))?;
        }
        fs::create_dir_all(&staging)
            .await
            .map_err(|error| AtlasError::storage(error.to_string()))?;
        if let Some(data_dir) = self.root.parent() {
            self.reconcile_trash(&data_dir.join(".trash")).await?;
        }
        self.cleanup_unreferenced().await?;
        Ok(())
    }

    pub async fn import(&self, field: Field<'_>) -> Result<ImportPdfResult, AtlasError> {
        let _transition = self.transitions.lock().await;
        let upload = self.persist(field).await?;
        let previous = self
            .documents
            .list_sources()
            .await?
            .into_iter()
            .find(|document| document.sha256 == upload.digest);
        match self
            .library
            .import_pdf(upload.path.to_string_lossy().into_owned())
            .await
        {
            Ok(result) => {
                if let Some(previous) = previous {
                    self.remove_superseded_if_idle(Path::new(&previous.file_path), &upload.path)
                        .await?;
                }
                Ok(result)
            }
            Err(error) => {
                if upload.created {
                    self.remove_managed_path(&upload.path).await?;
                }
                Err(error)
            }
        }
    }

    pub async fn relocate(
        &self,
        document_id: DocumentId,
        field: Field<'_>,
    ) -> Result<DocumentSummary, AtlasError> {
        let _transition = self.transitions.lock().await;
        let previous = self
            .documents
            .get(&document_id)
            .await?
            .ok_or_else(|| AtlasError::not_found("document was not found"))?;
        let upload = self.persist(field).await?;
        match self
            .library
            .relocate(document_id, upload.path.to_string_lossy().into_owned())
            .await
        {
            Ok(document) => {
                self.remove_superseded_if_idle(Path::new(&previous.file_path), &upload.path)
                    .await?;
                Ok(document)
            }
            Err(error) => {
                if upload.created {
                    self.remove_managed_path(&upload.path).await?;
                }
                Err(error)
            }
        }
    }

    pub async fn remove(&self, document_id: DocumentId) -> Result<(), AtlasError> {
        let _transition = self.transitions.lock().await;
        let record = self.documents.get(&document_id).await?;
        let artifacts = self.artifact_root.join(document_id.as_str());
        let data_dir = self
            .root
            .parent()
            .ok_or_else(|| AtlasError::storage("managed PDF root has no parent"))?;
        let trash = data_dir.join(".trash").join(Uuid::new_v4().to_string());
        fs::create_dir_all(&trash)
            .await
            .map_err(|error| AtlasError::storage(error.to_string()))?;
        let mut quarantine = QuarantineGuard::new(trash.clone());
        let pdf = record
            .as_ref()
            .map(|record| self.managed_path(Path::new(&record.file_path)))
            .transpose()?
            .flatten()
            .map(|original| MovedPath {
                original,
                moved: trash.join("paper.pdf"),
            });
        let artifact_move = if fs::try_exists(&artifacts)
            .await
            .map_err(|error| AtlasError::storage(error.to_string()))?
        {
            Some(MovedPath {
                original: artifacts,
                moved: trash.join("parse-artifacts"),
            })
        } else {
            None
        };
        let mut manifest = DeletionManifest {
            document_id: document_id.clone(),
            pdf,
            artifacts: artifact_move,
            committed: false,
        };
        self.write_deletion_manifest(&trash, &manifest).await?;
        quarantine.disarm();

        if let Some(pdf) = &manifest.pdf
            && let Err(error) = fs::rename(&pdf.original, &pdf.moved).await
        {
            let _ = fs::remove_dir_all(&trash).await;
            return Err(AtlasError::storage(error.to_string()));
        }
        if let Some(artifacts) = &manifest.artifacts
            && let Err(error) = fs::rename(&artifacts.original, &artifacts.moved).await
        {
            let rollback = self.restore_deletion(&manifest).await;
            if rollback.is_ok() {
                let _ = fs::remove_dir_all(&trash).await;
            }
            return Err(match rollback {
                Ok(()) => AtlasError::storage(error.to_string()),
                Err(rollback) => AtlasError::storage(format!(
                    "Could not quarantine parse artifacts: {error}; PDF rollback also failed: {rollback}"
                )),
            });
        }
        if let Err(error) = self.library.remove(document_id).await {
            return match self.restore_deletion(&manifest).await {
                Ok(()) => {
                    let _ = fs::remove_dir_all(&trash).await;
                    Err(error)
                }
                Err(rollback) => Err(AtlasError::storage(format!(
                    "{error}; quarantined files could not be restored: {rollback}"
                ))),
            };
        }
        manifest.committed = true;
        if let Err(error) = self.write_deletion_manifest(&trash, &manifest).await {
            eprintln!(
                "Atlas could not mark deletion quarantine committed at {}: {error}",
                trash.display()
            );
        }
        if let Err(error) = fs::remove_dir_all(&trash).await {
            eprintln!(
                "Atlas deferred deletion of quarantined document files at {}: {error}",
                trash.display()
            );
        }
        Ok(())
    }

    async fn persist(&self, mut field: Field<'_>) -> Result<PersistedUpload, AtlasError> {
        let file_name = sanitize_file_name(field.file_name().unwrap_or_default())?;
        let staging = self.root.join(".staging");
        let mut temporary =
            TemporaryUpload::new(staging.join(format!("{}.partial", Uuid::new_v4())));
        let file = fs::File::create(&temporary.path)
            .await
            .map_err(|error| AtlasError::storage(error.to_string()))?;
        let mut writer = BufWriter::new(file);
        let mut hasher = Sha256::new();
        let mut size = 0_u64;

        while let Some(chunk) = field
            .chunk()
            .await
            .map_err(|error| AtlasError::source_unreadable(error.to_string()))?
        {
            size = size
                .checked_add(chunk.len() as u64)
                .ok_or_else(|| AtlasError::pdf_too_large(MAX_UPLOAD_BYTES / 1024 / 1024))?;
            if size > MAX_UPLOAD_BYTES {
                return Err(AtlasError::pdf_too_large(MAX_UPLOAD_BYTES / 1024 / 1024));
            }
            hasher.update(&chunk);
            writer
                .write_all(&chunk)
                .await
                .map_err(|error| AtlasError::storage(error.to_string()))?;
        }
        writer
            .flush()
            .await
            .map_err(|error| AtlasError::storage(error.to_string()))?;
        writer
            .get_ref()
            .sync_all()
            .await
            .map_err(|error| AtlasError::storage(error.to_string()))?;
        drop(writer);

        let digest = hex::encode(hasher.finalize());
        let directory = self.root.join(&digest);
        fs::create_dir_all(&directory)
            .await
            .map_err(|error| AtlasError::storage(error.to_string()))?;
        let final_path = directory.join(file_name);
        let created = if fs::try_exists(&final_path)
            .await
            .map_err(|error| AtlasError::storage(error.to_string()))?
        {
            if file_digest(&final_path).await? == digest {
                false
            } else {
                fs::remove_file(&final_path)
                    .await
                    .map_err(|error| AtlasError::storage(error.to_string()))?;
                fs::rename(&temporary.path, &final_path)
                    .await
                    .map_err(|error| AtlasError::storage(error.to_string()))?;
                temporary.commit();
                true
            }
        } else {
            fs::rename(&temporary.path, &final_path)
                .await
                .map_err(|error| AtlasError::storage(error.to_string()))?;
            temporary.commit();
            true
        };
        let path = final_path
            .canonicalize()
            .map_err(|error| AtlasError::storage(error.to_string()))?;
        Ok(PersistedUpload {
            path,
            digest,
            created,
        })
    }

    async fn remove_managed_path(&self, path: &Path) -> Result<(), AtlasError> {
        let Some(path) = self.managed_path(path)? else {
            return Ok(());
        };
        match fs::remove_file(&path).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(AtlasError::storage(error.to_string())),
        }
        if let Some(parent) = path.parent() {
            match fs::remove_dir(parent).await {
                Ok(()) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
                    ) => {}
                Err(error) => return Err(AtlasError::storage(error.to_string())),
            }
        }
        Ok(())
    }

    fn managed_path(&self, path: &Path) -> Result<Option<PathBuf>, AtlasError> {
        let root = self
            .root
            .canonicalize()
            .map_err(|error| AtlasError::storage(error.to_string()))?;
        let path = match path.canonicalize() {
            Ok(path) => path,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(AtlasError::storage(error.to_string())),
        };
        let Ok(relative) = path.strip_prefix(&root) else {
            return Ok(None);
        };
        Ok((relative.components().count() == 2).then_some(path))
    }

    async fn remove_superseded_if_idle(
        &self,
        previous: &Path,
        replacement: &Path,
    ) -> Result<(), AtlasError> {
        let Some(previous) = self.managed_path(previous)? else {
            return Ok(());
        };
        let replacement = replacement
            .canonicalize()
            .map_err(|error| AtlasError::storage(error.to_string()))?;
        if previous == replacement || self.sources.path_is_active(&previous)? {
            return Ok(());
        }
        self.remove_managed_path(&previous).await
    }

    async fn write_deletion_manifest(
        &self,
        trash: &Path,
        manifest: &DeletionManifest,
    ) -> Result<(), AtlasError> {
        let bytes = serde_json::to_vec(manifest)
            .map_err(|error| AtlasError::internal(error.to_string()))?;
        let path = trash.join("manifest.json");
        let temporary = trash.join("manifest.next");
        let mut file = fs::File::create(&temporary)
            .await
            .map_err(|error| AtlasError::storage(error.to_string()))?;
        file.write_all(&bytes)
            .await
            .map_err(|error| AtlasError::storage(error.to_string()))?;
        file.sync_all()
            .await
            .map_err(|error| AtlasError::storage(error.to_string()))?;
        drop(file);
        fs::rename(temporary, path)
            .await
            .map_err(|error| AtlasError::storage(error.to_string()))
    }

    async fn restore_deletion(&self, manifest: &DeletionManifest) -> Result<(), AtlasError> {
        if let Some(artifacts) = &manifest.artifacts {
            restore_moved_path(artifacts).await?;
        }
        if let Some(pdf) = &manifest.pdf {
            restore_moved_path(pdf).await?;
        }
        Ok(())
    }

    async fn reconcile_trash(&self, trash_root: &Path) -> Result<(), AtlasError> {
        let mut entries = match fs::read_dir(trash_root).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(AtlasError::storage(error.to_string())),
        };
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|error| AtlasError::storage(error.to_string()))?
        {
            if !entry
                .file_type()
                .await
                .map_err(|error| AtlasError::storage(error.to_string()))?
                .is_dir()
            {
                return Err(AtlasError::storage(
                    "Atlas deletion quarantine contains an unexpected file",
                ));
            }
            let manifest_path = entry.path().join("manifest.json");
            let bytes = match fs::read(&manifest_path).await {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    fs::remove_dir_all(entry.path())
                        .await
                        .map_err(|error| AtlasError::storage(error.to_string()))?;
                    continue;
                }
                Err(error) => {
                    return Err(AtlasError::storage(format!(
                        "Atlas deletion quarantine is unreadable at {}: {error}",
                        entry.path().display()
                    )));
                }
            };
            let manifest: DeletionManifest = serde_json::from_slice(&bytes).map_err(|error| {
                AtlasError::storage(format!(
                    "Atlas deletion quarantine manifest is invalid at {}: {error}",
                    entry.path().display()
                ))
            })?;
            if self.documents.get(&manifest.document_id).await?.is_some() {
                self.restore_deletion(&manifest).await?;
            }
            fs::remove_dir_all(entry.path())
                .await
                .map_err(|error| AtlasError::storage(error.to_string()))?;
        }
        Ok(())
    }

    async fn cleanup_unreferenced(&self) -> Result<(), AtlasError> {
        let referenced = self
            .documents
            .list_sources()
            .await?
            .into_iter()
            .filter_map(|document| PathBuf::from(document.file_path).canonicalize().ok())
            .collect::<HashSet<_>>();
        let mut directories = match fs::read_dir(&self.root).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(AtlasError::storage(error.to_string())),
        };
        while let Some(directory) = directories
            .next_entry()
            .await
            .map_err(|error| AtlasError::storage(error.to_string()))?
        {
            if !directory
                .file_type()
                .await
                .map_err(|error| AtlasError::storage(error.to_string()))?
                .is_dir()
                || directory.file_name() == ".staging"
            {
                continue;
            }

            let mut files = fs::read_dir(directory.path())
                .await
                .map_err(|error| AtlasError::storage(error.to_string()))?;
            while let Some(file) = files
                .next_entry()
                .await
                .map_err(|error| AtlasError::storage(error.to_string()))?
            {
                let path = file.path().canonicalize().map_err(|error| {
                    AtlasError::storage(format!("managed PDF path is invalid: {error}"))
                })?;
                if !referenced.contains(&path) {
                    fs::remove_file(path)
                        .await
                        .map_err(|error| AtlasError::storage(error.to_string()))?;
                }
            }
            match fs::remove_dir(directory.path()).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::DirectoryNotEmpty => {}
                Err(error) => return Err(AtlasError::storage(error.to_string())),
            }
        }
        Ok(())
    }
}

async fn restore_moved_path(entry: &MovedPath) -> Result<(), AtlasError> {
    if !fs::try_exists(&entry.moved)
        .await
        .map_err(|error| AtlasError::storage(error.to_string()))?
    {
        return Ok(());
    }
    if fs::try_exists(&entry.original)
        .await
        .map_err(|error| AtlasError::storage(error.to_string()))?
    {
        return remove_any(&entry.moved).await;
    }
    if let Some(parent) = entry.original.parent() {
        fs::create_dir_all(parent)
            .await
            .map_err(|error| AtlasError::storage(error.to_string()))?;
    }
    fs::rename(&entry.moved, &entry.original)
        .await
        .map_err(|error| AtlasError::storage(error.to_string()))
}

async fn remove_any(path: &Path) -> Result<(), AtlasError> {
    let metadata = fs::metadata(path)
        .await
        .map_err(|error| AtlasError::storage(error.to_string()))?;
    if metadata.is_dir() {
        fs::remove_dir_all(path)
            .await
            .map_err(|error| AtlasError::storage(error.to_string()))
    } else {
        fs::remove_file(path)
            .await
            .map_err(|error| AtlasError::storage(error.to_string()))
    }
}

fn sanitize_file_name(file_name: &str) -> Result<String, AtlasError> {
    let file_name = Path::new(file_name)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if !file_name.to_ascii_lowercase().ends_with(".pdf") {
        return Err(AtlasError::unsupported_file_type());
    }
    let stem = Path::new(file_name)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("paper");
    let mut sanitized = stem
        .chars()
        .take(120)
        .map(|character| {
            if character.is_alphanumeric()
                || matches!(character, ' ' | '.' | '_' | '-' | '(' | ')' | '[' | ']')
            {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    sanitized = sanitized.trim_matches([' ', '.']).to_owned();
    if sanitized.is_empty() {
        sanitized = "paper".to_owned();
    }
    Ok(format!("{sanitized}.pdf"))
}

async fn file_digest(path: &Path) -> Result<String, AtlasError> {
    let file = fs::File::open(path)
        .await
        .map_err(|error| AtlasError::storage(error.to_string()))?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .await
            .map_err(|error| AtlasError::storage(error.to_string()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn uploaded_names_are_safe_but_remain_human_readable() {
        assert_eq!(
            sanitize_file_name("../../论文 (final).pdf").expect("valid PDF"),
            "论文 (final).pdf"
        );
        assert_eq!(
            sanitize_file_name("unsafe:name?.PDF").expect("valid PDF"),
            "unsafe_name_.pdf"
        );
        assert!(sanitize_file_name("notes.txt").is_err());
    }

    #[test]
    fn pre_manifest_quarantine_is_removed_on_failure() {
        let temporary = TempDir::new().expect("temporary directory");
        let quarantine = temporary.path().join("trash-entry");
        std::fs::create_dir_all(&quarantine).expect("quarantine should exist");
        {
            let _guard = QuarantineGuard::new(quarantine.clone());
        }
        assert!(!quarantine.exists());
    }
}
