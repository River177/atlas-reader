use std::{
    collections::HashSet,
    fs::{self, File},
    io::{Read, Seek},
    path::{Component, Path, PathBuf},
};

use atlas_domain::{AssetMimeType, AtlasError};
use sha2::{Digest, Sha256};
use zip::ZipArchive;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ArchiveLimits {
    pub max_entries: usize,
    pub max_expanded_bytes: u64,
    pub max_single_file_bytes: u64,
}

impl ArchiveLimits {
    #[must_use]
    pub fn for_source_size(source_size_bytes: u64) -> Self {
        const ABSOLUTE_LIMIT: u64 = 1_000_000_000;
        const MINIMUM_LIMIT: u64 = 16 * 1024 * 1024;
        let proportional = source_size_bytes.saturating_mul(10).max(MINIMUM_LIMIT);
        Self {
            max_entries: 4_096,
            max_expanded_bytes: proportional.min(ABSOLUTE_LIMIT),
            max_single_file_bytes: proportional.min(256 * 1024 * 1024),
        }
    }
}

impl Default for ArchiveLimits {
    fn default() -> Self {
        Self::for_source_size(20 * 1024 * 1024)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExtractedMineruAsset {
    pub relative_path: String,
    pub sha256: String,
    pub mime_type: AssetMimeType,
    pub size_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExtractedMineruArtifact {
    pub content_list_path: PathBuf,
    pub layout_path: PathBuf,
    pub assets: Vec<ExtractedMineruAsset>,
    pub expanded_bytes: u64,
}

#[derive(Clone, Debug)]
pub struct MineruArchiveUnpacker {
    limits: ArchiveLimits,
}

impl MineruArchiveUnpacker {
    #[must_use]
    pub fn new(limits: ArchiveLimits) -> Self {
        Self { limits }
    }

    pub fn unpack_file(
        &self,
        archive_path: &Path,
        destination: &Path,
    ) -> Result<ExtractedMineruArtifact, AtlasError> {
        let file = File::open(archive_path)
            .map_err(|error| AtlasError::source_unreadable(error.to_string()))?;
        self.unpack(file, destination)
    }

    pub fn unpack<R: Read + Seek>(
        &self,
        reader: R,
        destination: &Path,
    ) -> Result<ExtractedMineruArtifact, AtlasError> {
        let mut archive = ZipArchive::new(reader)
            .map_err(|error| invalid_archive(format!("invalid ZIP container: {error}")))?;
        if archive.len() > self.limits.max_entries {
            return Err(invalid_archive("archive contains too many entries"));
        }
        let plans = self.preflight(&mut archive)?;
        fs::create_dir_all(destination).map_err(|error| AtlasError::storage(error.to_string()))?;

        let mut content_list_path = None;
        let mut layout_path = None;
        let mut assets = Vec::new();
        let mut expanded_bytes = 0_u64;

        for plan in plans {
            let mut entry = archive
                .by_index(plan.index)
                .map_err(|error| invalid_archive(error.to_string()))?;
            let target = destination.join(&plan.relative_path);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)
                    .map_err(|error| AtlasError::storage(error.to_string()))?;
            }
            let temporary = target.with_extension(format!(
                "{}.partial",
                target
                    .extension()
                    .and_then(|value| value.to_str())
                    .unwrap_or_default()
            ));
            let mut output =
                File::create(&temporary).map_err(|error| AtlasError::storage(error.to_string()))?;
            let copied = std::io::copy(&mut entry, &mut output)
                .map_err(|error| invalid_archive(format!("could not expand entry: {error}")))?;
            if copied != plan.size_bytes {
                let _ = fs::remove_file(&temporary);
                return Err(invalid_archive(
                    "archive entry ended before its declared size",
                ));
            }
            drop(output);

            let asset = match plan.kind {
                EntryKind::ContentList => {
                    content_list_path = Some(target.clone());
                    None
                }
                EntryKind::Layout => {
                    layout_path = Some(target.clone());
                    None
                }
                EntryKind::Asset { expected_sha256 } => Some(verify_asset(
                    &temporary,
                    &plan.relative_path,
                    expected_sha256,
                    copied,
                )?),
            };
            fs::rename(&temporary, &target)
                .map_err(|error| AtlasError::storage(error.to_string()))?;
            if let Some(asset) = asset {
                assets.push(asset);
            }
            expanded_bytes = expanded_bytes
                .checked_add(copied)
                .ok_or_else(|| invalid_archive("expanded size overflow"))?;
        }

        let content_list_path = content_list_path
            .ok_or_else(|| invalid_archive("archive does not contain content_list.json"))?;
        let layout_path =
            layout_path.ok_or_else(|| invalid_archive("archive does not contain layout.json"))?;
        assets.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
        Ok(ExtractedMineruArtifact {
            content_list_path,
            layout_path,
            assets,
            expanded_bytes,
        })
    }

    fn preflight<R: Read + Seek>(
        &self,
        archive: &mut ZipArchive<R>,
    ) -> Result<Vec<EntryPlan>, AtlasError> {
        let mut total = 0_u64;
        let mut paths = HashSet::new();
        let mut plans = Vec::new();
        let mut content_lists = 0_u32;
        let mut layouts = 0_u32;

        for index in 0..archive.len() {
            let entry = archive
                .by_index(index)
                .map_err(|error| invalid_archive(error.to_string()))?;
            let relative_path = safe_entry_path(&entry)?;
            let mode = entry.unix_mode().unwrap_or(0);
            let file_type = mode & 0o170_000;
            if file_type != 0 && file_type != 0o100_000 && file_type != 0o040_000 {
                return Err(invalid_archive(
                    "archive contains a link or special filesystem entry",
                ));
            }
            if entry.is_dir() {
                continue;
            }
            if entry.size() > self.limits.max_single_file_bytes {
                return Err(invalid_archive("archive entry exceeds the per-file limit"));
            }
            total = total
                .checked_add(entry.size())
                .ok_or_else(|| invalid_archive("expanded size overflow"))?;
            if total > self.limits.max_expanded_bytes {
                return Err(invalid_archive("archive exceeds the expanded-size limit"));
            }

            let Some(kind) = classify_entry(&relative_path)? else {
                continue;
            };
            if !paths.insert(relative_path.clone()) {
                return Err(invalid_archive("archive contains a duplicate output path"));
            }
            match kind {
                EntryKind::ContentList => content_lists += 1,
                EntryKind::Layout => layouts += 1,
                EntryKind::Asset { .. } => {}
            }
            plans.push(EntryPlan {
                index,
                relative_path,
                size_bytes: entry.size(),
                kind,
            });
        }
        if content_lists != 1 {
            return Err(invalid_archive(
                "archive must contain exactly one content_list.json",
            ));
        }
        if layouts != 1 {
            return Err(invalid_archive(
                "archive must contain exactly one layout.json",
            ));
        }
        Ok(plans)
    }
}

struct EntryPlan {
    index: usize,
    relative_path: PathBuf,
    size_bytes: u64,
    kind: EntryKind,
}

enum EntryKind {
    ContentList,
    Layout,
    Asset { expected_sha256: String },
}

fn safe_entry_path(entry: &zip::read::ZipFile<'_, impl Read>) -> Result<PathBuf, AtlasError> {
    let path = entry
        .enclosed_name()
        .ok_or_else(|| invalid_archive("archive contains an unsafe path"))?;
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
    {
        return Err(invalid_archive("archive contains an unsafe path"));
    }
    Ok(path.to_path_buf())
}

fn classify_entry(path: &Path) -> Result<Option<EntryKind>, AtlasError> {
    let text = path
        .to_str()
        .ok_or_else(|| invalid_archive("archive path is not valid UTF-8"))?;
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("");
    if path.components().count() == 1 && file_name == "layout.json" {
        return Ok(Some(EntryKind::Layout));
    }
    if path.components().count() == 1
        && file_name.ends_with("_content_list.json")
        && !file_name.ends_with("_content_list_v2.json")
    {
        return Ok(Some(EntryKind::ContentList));
    }
    if path.components().count() == 2 && text.starts_with("images/") {
        let stem = path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("");
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("");
        if stem.len() != 64 || !stem.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(invalid_archive(
                "asset filename is not a SHA-256 content address",
            ));
        }
        if AssetMimeType::from_extension(extension).is_none() {
            return Err(invalid_archive("asset has an unsupported image type"));
        }
        return Ok(Some(EntryKind::Asset {
            expected_sha256: stem.to_ascii_lowercase(),
        }));
    }
    Ok(None)
}

fn verify_asset(
    path: &Path,
    relative_path: &Path,
    expected_sha256: String,
    size_bytes: u64,
) -> Result<ExtractedMineruAsset, AtlasError> {
    let bytes = fs::read(path).map_err(|error| AtlasError::storage(error.to_string()))?;
    let actual = hex::encode(Sha256::digest(&bytes));
    if actual != expected_sha256 {
        let _ = fs::remove_file(path);
        return Err(invalid_archive(
            "asset content does not match its SHA-256 name",
        ));
    }
    let mime_type = sniff_image(&bytes)
        .ok_or_else(|| invalid_archive("asset bytes do not match a supported image MIME type"))?;
    let extension = relative_path
        .extension()
        .and_then(|value| value.to_str())
        .and_then(AssetMimeType::from_extension)
        .ok_or_else(|| invalid_archive("asset has an unsupported extension"))?;
    if mime_type != extension {
        return Err(invalid_archive(
            "asset extension does not match its image content",
        ));
    }
    Ok(ExtractedMineruAsset {
        relative_path: relative_path.to_string_lossy().into_owned(),
        sha256: actual,
        mime_type,
        size_bytes,
    })
}

fn sniff_image(bytes: &[u8]) -> Option<AssetMimeType> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        Some(AssetMimeType::ImagePng)
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some(AssetMimeType::ImageJpeg)
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some(AssetMimeType::ImageWebp)
    } else {
        None
    }
}

fn invalid_archive(message: impl Into<String>) -> AtlasError {
    AtlasError::invalid_input(format!("unsafe Cloud MinerU artifact: {}", message.into()))
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Write};

    use tempfile::TempDir;
    use zip::{ZipWriter, write::SimpleFileOptions};

    use super::*;

    fn zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        for (name, bytes) in entries {
            writer
                .start_file(*name, SimpleFileOptions::default())
                .expect("file should start");
            writer.write_all(bytes).expect("file should write");
        }
        writer.finish().expect("zip should finish").into_inner()
    }

    fn valid_entries() -> Vec<(&'static str, &'static [u8])> {
        vec![
            ("result_content_list.json", br#"[]"#),
            (
                "layout.json",
                br#"{"pdf_info":[{"page_idx":0,"page_size":[612,792]}]}"#,
            ),
        ]
    }

    #[test]
    fn extracts_only_the_allowlist_and_discards_the_echoed_pdf() {
        let mut entries = valid_entries();
        entries.push(("result_origin.pdf", b"%PDF-private-source"));
        entries.push(("full.md", b"diagnostic"));
        let bytes = zip(&entries);
        let destination = TempDir::new().expect("temporary directory");

        let artifact = MineruArchiveUnpacker::new(ArchiveLimits::default())
            .unpack(Cursor::new(bytes), destination.path())
            .expect("archive should unpack");

        assert!(artifact.content_list_path.exists());
        assert!(artifact.layout_path.exists());
        assert!(!destination.path().join("result_origin.pdf").exists());
        assert!(!destination.path().join("full.md").exists());
    }

    #[test]
    fn rejects_zip_slip_before_writing_any_entry() {
        let mut entries = valid_entries();
        entries.push(("../../escaped.txt", b"escape"));
        let bytes = zip(&entries);
        let destination = TempDir::new().expect("temporary directory");

        let result = MineruArchiveUnpacker::new(ArchiveLimits::default())
            .unpack(Cursor::new(bytes), destination.path());

        assert!(result.is_err());
        assert!(!destination.path().join("result_content_list.json").exists());
        assert!(!destination.path().join("../../escaped.txt").exists());
    }

    #[test]
    fn rejects_links_before_writing_any_entry() {
        let cursor = Cursor::new(Vec::new());
        let mut writer = ZipWriter::new(cursor);
        writer
            .start_file("result_content_list.json", SimpleFileOptions::default())
            .expect("content list should start");
        writer.write_all(b"[]").expect("content list should write");
        writer
            .start_file("layout.json", SimpleFileOptions::default())
            .expect("layout should start");
        writer
            .write_all(br#"{"pdf_info":[]}"#)
            .expect("layout should write");
        writer
            .add_symlink(
                "images/link.jpg",
                "../../secret",
                SimpleFileOptions::default(),
            )
            .expect("link should write");
        let bytes = writer.finish().expect("zip should finish").into_inner();
        let destination = TempDir::new().expect("temporary directory");

        let result = MineruArchiveUnpacker::new(ArchiveLimits::default())
            .unpack(Cursor::new(bytes), destination.path());

        assert!(result.is_err());
        assert!(!destination.path().join("layout.json").exists());
    }

    #[test]
    fn rejects_an_expansion_over_the_declared_limit() {
        let bytes = zip(&valid_entries());
        let destination = TempDir::new().expect("temporary directory");
        let limits = ArchiveLimits {
            max_entries: 10,
            max_expanded_bytes: 2,
            max_single_file_bytes: 1_000,
        };

        let result =
            MineruArchiveUnpacker::new(limits).unpack(Cursor::new(bytes), destination.path());

        assert!(result.is_err());
        assert!(!destination.path().join("layout.json").exists());
    }

    #[test]
    fn verifies_asset_hash_and_magic_bytes() {
        let image: &[u8] = &[0xff, 0xd8, 0xff, 0xdb, 0x00, 0x01];
        let hash = hex::encode(Sha256::digest(image));
        let name = format!("images/{hash}.jpg");
        let mut owned = valid_entries()
            .into_iter()
            .map(|(name, bytes)| (name.to_owned(), bytes.to_vec()))
            .collect::<Vec<_>>();
        owned.push((name, image.to_vec()));
        let borrowed = owned
            .iter()
            .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
            .collect::<Vec<_>>();
        let destination = TempDir::new().expect("temporary directory");

        let artifact = MineruArchiveUnpacker::new(ArchiveLimits::default())
            .unpack(Cursor::new(zip(&borrowed)), destination.path())
            .expect("archive should unpack");

        assert_eq!(artifact.assets.len(), 1);
        assert_eq!(artifact.assets[0].sha256, hash);
        assert_eq!(artifact.assets[0].mime_type, AssetMimeType::ImageJpeg);
    }
}
