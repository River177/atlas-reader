use std::path::{Path, PathBuf};

use async_trait::async_trait;
use atlas_domain::{
    AtlasError, BlockId, BlockKind, CANONICAL_SCHEMA_VERSION, CanonicalBlock, CanonicalChapter,
    CanonicalDocument, ChapterId, ChapterRole, DocumentId, ParserIdentity, StructuredContent,
};

use crate::identity::{digest, stable_id};

pub const LOCAL_PARSER_VERSION: &str = "pdf-extract-0.12";
pub const LOCAL_NORMALIZER_VERSION: &str = "local-text-v1";

#[derive(Clone, Debug)]
pub struct LocalExtractRequest {
    pub document_id: DocumentId,
    pub artifact_id: String,
    pub source_sha256: String,
    pub source_path: PathBuf,
    pub document_title: String,
}

#[async_trait]
pub trait LocalTextExtractor: Send + Sync {
    async fn extract(&self, request: LocalExtractRequest) -> Result<CanonicalDocument, AtlasError>;
}

#[derive(Clone, Debug, Default)]
pub struct LocalPdfExtractor;

impl LocalPdfExtractor {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl LocalTextExtractor for LocalPdfExtractor {
    async fn extract(&self, request: LocalExtractRequest) -> Result<CanonicalDocument, AtlasError> {
        let path = request.source_path.clone();
        let pages = tokio::task::spawn_blocking(move || extract_pages(&path))
            .await
            .map_err(|error| {
                AtlasError::internal(format!("local PDF parser stopped: {error}"))
            })??;
        normalise_pages(request, pages)
    }
}

fn extract_pages(path: &Path) -> Result<Vec<String>, AtlasError> {
    pdf_extract::extract_text_by_pages(path)
        .map_err(|error| AtlasError::invalid_pdf(format!("local text extraction failed: {error}")))
}

fn normalise_pages(
    request: LocalExtractRequest,
    pages: Vec<String>,
) -> Result<CanonicalDocument, AtlasError> {
    if pages.is_empty() {
        return Err(AtlasError::invalid_pdf("the PDF has no readable pages"));
    }
    let mut drafts = Vec::new();
    let mut current = ChapterDraft::front_matter();
    let mut has_text = false;

    for (page_index, page_text) in pages.iter().enumerate() {
        let page = page_index as u32 + 1;
        for paragraph in paragraphs(page_text) {
            has_text = true;
            if let Some((title, depth, role)) = local_heading(&paragraph) {
                if !current.blocks.is_empty() {
                    drafts.push(current);
                }
                current = ChapterDraft {
                    title: title.clone(),
                    depth,
                    role,
                    page_start: page,
                    page_end: page,
                    blocks: vec![BlockDraft {
                        kind: BlockKind::Heading,
                        page,
                        content: StructuredContent::text(title),
                    }],
                };
            } else {
                current.page_end = page;
                current.blocks.push(BlockDraft {
                    kind: if looks_like_list(&paragraph) {
                        BlockKind::List
                    } else {
                        BlockKind::Paragraph
                    },
                    page,
                    content: StructuredContent::text(paragraph),
                });
            }
        }
    }
    if !current.blocks.is_empty() {
        drafts.push(current);
    }
    if !has_text {
        return Err(AtlasError::invalid_pdf(
            "the PDF has no usable digital text layer",
        ));
    }

    let chapters = drafts
        .into_iter()
        .enumerate()
        .map(|(chapter_index, draft)| {
            let title_digest = digest(draft.title.as_bytes());
            let chapter_id = ChapterId::new(stable_id(
                "chapter",
                &[
                    &request.artifact_id,
                    &chapter_index.to_string(),
                    &title_digest,
                ],
            ));
            let blocks = draft
                .blocks
                .into_iter()
                .enumerate()
                .map(|(block_index, block)| {
                    let source_json = serde_json::to_vec(&block.content)
                        .map_err(|error| AtlasError::internal(error.to_string()))?;
                    let source_digest = digest(&source_json);
                    Ok(CanonicalBlock {
                        id: BlockId::new(stable_id(
                            "block",
                            &[
                                chapter_id.as_str(),
                                &block_index.to_string(),
                                block.kind.as_str(),
                                &source_digest,
                            ],
                        )),
                        order_index: block_index as u32,
                        kind: block.kind,
                        page_start: block.page,
                        page_end: block.page,
                        bounding_boxes: Vec::new(),
                        content: block.content,
                        source_digest,
                    })
                })
                .collect::<Result<Vec<_>, AtlasError>>()?;
            Ok(CanonicalChapter {
                id: chapter_id,
                order_index: chapter_index as u32,
                depth: draft.depth,
                role: draft.role,
                source_title: draft.title,
                page_start: draft.page_start,
                page_end: draft.page_end,
                blocks,
            })
        })
        .collect::<Result<Vec<_>, AtlasError>>()?;

    Ok(CanonicalDocument {
        schema_version: CANONICAL_SCHEMA_VERSION,
        artifact_id: request.artifact_id,
        document_id: request.document_id,
        source_sha256: request.source_sha256,
        parser: ParserIdentity {
            name: "PDF text layer".to_owned(),
            version: LOCAL_PARSER_VERSION.to_owned(),
            backend: "local_text".to_owned(),
        },
        normalizer_version: LOCAL_NORMALIZER_VERSION.to_owned(),
        page_count: pages.len() as u32,
        title: (!request.document_title.trim().is_empty()).then_some(request.document_title),
        chapters,
        assets: Vec::new(),
    })
}

struct ChapterDraft {
    title: String,
    depth: u32,
    role: ChapterRole,
    page_start: u32,
    page_end: u32,
    blocks: Vec<BlockDraft>,
}

impl ChapterDraft {
    fn front_matter() -> Self {
        Self {
            title: "Front Matter".to_owned(),
            depth: 1,
            role: ChapterRole::FrontMatter,
            page_start: 1,
            page_end: 1,
            blocks: Vec::new(),
        }
    }
}

struct BlockDraft {
    kind: BlockKind,
    page: u32,
    content: StructuredContent,
}

fn paragraphs(page: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut current = Vec::new();
    for line in page.lines() {
        let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if line.is_empty() {
            if !current.is_empty() {
                result.push(current.join(" "));
                current.clear();
            }
        } else {
            current.push(line);
        }
    }
    if !current.is_empty() {
        result.push(current.join(" "));
    }
    result
}

fn local_heading(text: &str) -> Option<(String, u32, ChapterRole)> {
    if text.len() > 160 || text.ends_with('.') {
        return None;
    }
    let trimmed = text.trim();
    let lowercase = trimmed.to_ascii_lowercase();
    let special = match lowercase.as_str() {
        "abstract" | "summary" | "acknowledgements" | "acknowledgments" => {
            Some(ChapterRole::FrontMatter)
        }
        "references" | "bibliography" => Some(ChapterRole::References),
        "conclusion" | "conclusions" => Some(ChapterRole::Body),
        _ => None,
    };
    if let Some(role) = special {
        return Some((trimmed.to_owned(), 1, role));
    }

    let prefix = trimmed
        .chars()
        .take_while(|character| character.is_ascii_digit() || *character == '.')
        .collect::<String>();
    let prefix = prefix.trim_end_matches('.');
    if prefix.is_empty()
        || prefix
            .split('.')
            .any(|part| part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return None;
    }
    Some((
        trimmed.to_owned(),
        prefix.split('.').count() as u32,
        ChapterRole::Body,
    ))
}

fn looks_like_list(text: &str) -> bool {
    let trimmed = text.trim_start();
    trimmed.starts_with("- ") || trimmed.starts_with("• ") || trimmed.starts_with("* ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> LocalExtractRequest {
        LocalExtractRequest {
            document_id: DocumentId::from("document-1"),
            artifact_id: "artifact-1".to_owned(),
            source_sha256: "a".repeat(64),
            source_path: PathBuf::from("unused.pdf"),
            document_title: "Synthetic paper".to_owned(),
        }
    }

    #[test]
    fn page_text_becomes_deterministic_chapters_and_blocks() {
        let document = normalise_pages(
            request(),
            vec![
                "Abstract\n\nA short abstract.\n\n1 Introduction\n\nFirst paragraph.".to_owned(),
                "1.1 Detail\n\n- one item\n\nSecond paragraph.".to_owned(),
            ],
        )
        .expect("local text should normalize");

        assert_eq!(document.parser.backend, "local_text");
        assert_eq!(document.page_count, 2);
        assert_eq!(document.chapters.len(), 3);
        assert_eq!(document.chapters[0].role, ChapterRole::FrontMatter);
        assert_eq!(document.chapters[1].source_title, "1 Introduction");
        assert_eq!(document.chapters[2].depth, 2);
        assert_eq!(document.chapters[2].blocks[1].kind, BlockKind::List);
    }

    #[test]
    fn empty_text_layer_is_a_recoverable_parse_failure() {
        let error = normalise_pages(request(), vec!["  \n".to_owned()])
            .expect_err("empty extraction should fail");

        assert!(error.recoverable);
        assert!(error.message.contains("text layer"));
    }
}
