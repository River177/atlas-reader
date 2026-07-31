use std::collections::HashMap;

use atlas_domain::{
    AtlasError, BlockId, BlockTranslationState, CanonicalBlock, CanonicalChapter,
    CanonicalDocument, ChapterTranslationView, SelectionContext, SelectionContextInput,
};
use serde::Serialize;

pub const MAX_SELECTION_UTF16: usize = 4_096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContextBudget {
    pub max_utf8_bytes: usize,
    pub max_neighbors_per_side: usize,
}

impl Default for ContextBudget {
    fn default() -> Self {
        Self {
            max_utf8_bytes: 64 * 1024,
            max_neighbors_per_side: 2,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadingContextBlock {
    pub block_id: BlockId,
    pub page_start: u32,
    pub page_end: u32,
    pub source_text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub translated_text: Option<String>,
    pub selected: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssembledReadingContext {
    pub selection: SelectionContext,
    pub blocks: Vec<ReadingContextBlock>,
    pub estimated_utf8_bytes: usize,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SelectionContextAssembler;

impl SelectionContextAssembler {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    pub fn assemble(
        &self,
        document: &CanonicalDocument,
        translation: &ChapterTranslationView,
        input: &SelectionContextInput,
        budget: ContextBudget,
    ) -> Result<AssembledReadingContext, AtlasError> {
        validate_input_shape(input)?;
        let (chapter, selected_index, selected_block) =
            find_block(document, &input.block_id).ok_or_else(AtlasError::stale_selection)?;
        if chapter.id != translation.chapter_id
            || selected_block.source_digest != input.source_digest
        {
            return Err(AtlasError::stale_selection());
        }

        let translated_by_block = translation
            .blocks
            .iter()
            .map(|block| (&block.block_id, block))
            .collect::<HashMap<_, _>>();
        let selected_translation = translated_by_block
            .get(&input.block_id)
            .filter(|block| {
                block.source_digest == input.source_digest
                    && block.state == BlockTranslationState::Ready
            })
            .and_then(|block| block.target.as_ref())
            .ok_or_else(AtlasError::stale_selection)?;
        let selected_slice = utf16_slice(
            &selected_translation.plain_text,
            input.start_utf16,
            input.end_utf16,
        )
        .ok_or_else(AtlasError::stale_selection)?;
        if selected_slice != input.selected_text {
            return Err(AtlasError::stale_selection());
        }

        let selection = SelectionContext {
            block_id: input.block_id.clone(),
            chapter_id: chapter.id.clone(),
            page_start: selected_block.page_start,
            page_end: selected_block.page_end,
            source_digest: selected_block.source_digest.clone(),
            start_utf16: input.start_utf16,
            end_utf16: input.end_utf16,
            selected_text: input.selected_text.clone(),
            aligned_source: selected_block.content.plain_text.clone(),
        };
        let selected_context_block = context_block(
            selected_block,
            translated_by_block.get(&input.block_id).copied(),
            true,
        );
        let mut included = vec![(selected_index, selected_context_block)];
        let base_cost = encoded_context_len(&selection, &included)?;
        if base_cost > budget.max_utf8_bytes {
            return Err(AtlasError::invalid_input(
                "The selected block exceeds the Reading Assistant context budget",
            ));
        }

        let mut used = base_cost;
        for index in neighbor_indices(
            selected_index,
            chapter.blocks.len(),
            budget.max_neighbors_per_side,
        ) {
            let block = &chapter.blocks[index];
            let value = context_block(block, translated_by_block.get(&block.id).copied(), false);
            included.push((index, value));
            let candidate = encoded_context_len(&selection, &included)?;
            if candidate <= budget.max_utf8_bytes {
                used = candidate;
            } else {
                included.pop();
            }
        }
        included.sort_by_key(|(index, _)| *index);

        Ok(AssembledReadingContext {
            selection,
            blocks: included.into_iter().map(|(_, block)| block).collect(),
            estimated_utf8_bytes: used,
        })
    }
}

fn validate_input_shape(input: &SelectionContextInput) -> Result<(), AtlasError> {
    if input.source_digest.trim().is_empty()
        || input.start_utf16 >= input.end_utf16
        || input.selected_text.trim().is_empty()
        || input.selected_text.encode_utf16().count() > MAX_SELECTION_UTF16
    {
        return Err(AtlasError::invalid_input(
            "The translated-text selection is invalid",
        ));
    }
    Ok(())
}

fn find_block<'a>(
    document: &'a CanonicalDocument,
    block_id: &BlockId,
) -> Option<(&'a CanonicalChapter, usize, &'a CanonicalBlock)> {
    document.chapters.iter().find_map(|chapter| {
        chapter
            .blocks
            .iter()
            .enumerate()
            .find(|(_, block)| &block.id == block_id)
            .map(|(index, block)| (chapter, index, block))
    })
}

fn context_block(
    block: &CanonicalBlock,
    translation: Option<&atlas_domain::TranslatedBlockView>,
    selected: bool,
) -> ReadingContextBlock {
    let translated_text = translation
        .filter(|value| {
            value.source_digest == block.source_digest
                && value.state == BlockTranslationState::Ready
        })
        .and_then(|value| value.target.as_ref())
        .map(|target| target.plain_text.clone());
    ReadingContextBlock {
        block_id: block.id.clone(),
        page_start: block.page_start,
        page_end: block.page_end,
        source_text: block.content.plain_text.clone(),
        translated_text,
        selected,
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ContextPayload<'a> {
    selection: &'a SelectionContext,
    blocks: Vec<&'a ReadingContextBlock>,
}

fn encoded_context_len(
    selection: &SelectionContext,
    blocks: &[(usize, ReadingContextBlock)],
) -> Result<usize, AtlasError> {
    serde_json::to_vec(&ContextPayload {
        selection,
        blocks: blocks.iter().map(|(_, block)| block).collect(),
    })
    .map(|value| value.len())
    .map_err(|error| AtlasError::internal(error.to_string()))
}

fn neighbor_indices(selected: usize, block_count: usize, max_per_side: usize) -> Vec<usize> {
    let max_per_side = max_per_side.min(block_count);
    let mut indices = Vec::with_capacity(max_per_side.saturating_mul(2));
    for distance in 1..=max_per_side {
        if let Some(before) = selected.checked_sub(distance) {
            indices.push(before);
        }
        let after = selected.saturating_add(distance);
        if after < block_count {
            indices.push(after);
        }
    }
    indices
}

fn utf16_slice(value: &str, start: u32, end: u32) -> Option<&str> {
    let start = usize::try_from(start).ok()?;
    let end = usize::try_from(end).ok()?;
    let start_byte = utf16_offset_to_byte(value, start)?;
    let end_byte = utf16_offset_to_byte(value, end)?;
    value.get(start_byte..end_byte)
}

fn utf16_offset_to_byte(value: &str, offset: usize) -> Option<usize> {
    let mut utf16_index = 0;
    for (byte_index, character) in value.char_indices() {
        if utf16_index == offset {
            return Some(byte_index);
        }
        utf16_index = utf16_index.checked_add(character.len_utf16())?;
        if utf16_index > offset {
            return None;
        }
    }
    (utf16_index == offset).then_some(value.len())
}

#[cfg(test)]
mod tests {
    use atlas_domain::{
        BlockKind, BlockTranslationState, CanonicalBlock, CanonicalChapter, CanonicalDocument,
        ChapterId, ChapterRole, ChapterTranslationView, DocumentId, ParserIdentity,
        StructuredContent, TranslatedBlockView, TranslationState,
    };

    use super::*;

    fn block(id: &str, order_index: u32, source: &str) -> CanonicalBlock {
        CanonicalBlock {
            id: BlockId::from(id),
            order_index,
            kind: BlockKind::Paragraph,
            page_start: order_index.saturating_add(1),
            page_end: order_index.saturating_add(1),
            bounding_boxes: Vec::new(),
            content: StructuredContent::text(source),
            source_digest: format!("digest-{id}"),
        }
    }

    fn document() -> CanonicalDocument {
        CanonicalDocument {
            schema_version: 1,
            artifact_id: "artifact-1".to_owned(),
            document_id: DocumentId::from("document-1"),
            source_sha256: "source".to_owned(),
            parser: ParserIdentity {
                name: "test".to_owned(),
                version: "1".to_owned(),
                backend: "test".to_owned(),
            },
            normalizer_version: "1".to_owned(),
            page_count: 5,
            title: Some("Synthetic".to_owned()),
            chapters: vec![CanonicalChapter {
                id: ChapterId::from("chapter-1"),
                order_index: 0,
                depth: 1,
                role: ChapterRole::Body,
                source_title: "Method".to_owned(),
                page_start: 1,
                page_end: 5,
                blocks: vec![
                    block("block-1", 0, "Previous source."),
                    block("block-2", 1, "The model adopts this assumption."),
                    block("block-3", 2, "Next source."),
                    block("block-4", 3, "Far source."),
                ],
            }],
            assets: Vec::new(),
        }
    }

    fn translation() -> ChapterTranslationView {
        ChapterTranslationView {
            chapter_id: ChapterId::from("chapter-1"),
            state: TranslationState::Complete,
            progress: 1.0,
            job_id: None,
            job_active: false,
            blocks: [
                ("block-1", "上一段。"),
                ("block-2", "模型🙂采用该假设。"),
                ("block-3", "下一段。"),
                ("block-4", "远端段落。"),
            ]
            .into_iter()
            .map(|(id, target)| TranslatedBlockView {
                block_id: BlockId::from(id),
                source_digest: format!("digest-{id}"),
                state: BlockTranslationState::Ready,
                target: Some(StructuredContent::text(target)),
                safe_message: None,
            })
            .collect(),
            prefetched: false,
            safe_message: None,
        }
    }

    fn selection() -> SelectionContextInput {
        SelectionContextInput {
            block_id: BlockId::from("block-2"),
            source_digest: "digest-block-2".to_owned(),
            start_utf16: 4,
            end_utf16: 9,
            selected_text: "采用该假设".to_owned(),
        }
    }

    #[test]
    fn validates_utf16_selection_and_derives_source_location() {
        let context = SelectionContextAssembler::new()
            .assemble(
                &document(),
                &translation(),
                &selection(),
                ContextBudget::default(),
            )
            .expect("selection should assemble");

        assert_eq!(context.selection.chapter_id.as_str(), "chapter-1");
        assert_eq!(context.selection.page_start, 2);
        assert_eq!(
            context.selection.aligned_source,
            "The model adopts this assumption."
        );
        assert_eq!(
            context
                .blocks
                .iter()
                .map(|block| block.block_id.as_str())
                .collect::<Vec<_>>(),
            vec!["block-1", "block-2", "block-3", "block-4"]
        );
        assert!(context.blocks[1].selected);
    }

    #[test]
    fn rejects_offsets_that_split_a_surrogate_pair() {
        let mut invalid = selection();
        invalid.start_utf16 = 3;
        invalid.end_utf16 = 5;

        let error = SelectionContextAssembler::new()
            .assemble(
                &document(),
                &translation(),
                &invalid,
                ContextBudget::default(),
            )
            .expect_err("a split surrogate pair should be stale");

        assert_eq!(error.code, atlas_domain::AtlasErrorCode::StaleSelection);
    }

    #[test]
    fn rejects_stale_source_or_selected_text() {
        for invalid in [
            SelectionContextInput {
                source_digest: "old-digest".to_owned(),
                ..selection()
            },
            SelectionContextInput {
                selected_text: "错误文本".to_owned(),
                ..selection()
            },
        ] {
            let error = SelectionContextAssembler::new()
                .assemble(
                    &document(),
                    &translation(),
                    &invalid,
                    ContextBudget::default(),
                )
                .expect_err("stale evidence should be rejected");
            assert_eq!(error.code, atlas_domain::AtlasErrorCode::StaleSelection);
        }
    }

    #[test]
    fn budget_keeps_the_selected_block_and_nearest_neighbors_only() {
        let assembler = SelectionContextAssembler::new();
        let nearest = assembler
            .assemble(
                &document(),
                &translation(),
                &selection(),
                ContextBudget {
                    max_utf8_bytes: usize::MAX,
                    max_neighbors_per_side: 1,
                },
            )
            .expect("nearest neighbors should fit");
        let context = assembler
            .assemble(
                &document(),
                &translation(),
                &selection(),
                ContextBudget {
                    max_utf8_bytes: nearest.estimated_utf8_bytes,
                    max_neighbors_per_side: 2,
                },
            )
            .expect("near context should fit");

        assert!(context.blocks.iter().any(|block| block.selected));
        assert!(context.blocks.len() >= 2);
        assert_eq!(context.estimated_utf8_bytes, nearest.estimated_utf8_bytes);
        assert!(
            !context
                .blocks
                .iter()
                .any(|block| block.block_id.as_str() == "block-4")
        );
    }

    #[test]
    fn budget_includes_the_complete_payload_framing() {
        let assembler = SelectionContextAssembler::new();
        let selected_only = assembler
            .assemble(
                &document(),
                &translation(),
                &selection(),
                ContextBudget {
                    max_utf8_bytes: usize::MAX,
                    max_neighbors_per_side: 0,
                },
            )
            .expect("selected payload should assemble");

        assembler
            .assemble(
                &document(),
                &translation(),
                &selection(),
                ContextBudget {
                    max_utf8_bytes: selected_only.estimated_utf8_bytes,
                    max_neighbors_per_side: 0,
                },
            )
            .expect("the exact full-payload budget should fit");
        let error = assembler
            .assemble(
                &document(),
                &translation(),
                &selection(),
                ContextBudget {
                    max_utf8_bytes: selected_only.estimated_utf8_bytes.saturating_sub(1),
                    max_neighbors_per_side: 0,
                },
            )
            .expect_err("one byte less than the full payload should fail");
        assert_eq!(error.code, atlas_domain::AtlasErrorCode::InvalidInput);
    }

    #[test]
    fn malformed_or_oversized_selection_is_invalid_input() {
        let invalid = SelectionContextInput {
            start_utf16: 9,
            end_utf16: 9,
            ..selection()
        };
        let error = SelectionContextAssembler::new()
            .assemble(
                &document(),
                &translation(),
                &invalid,
                ContextBudget::default(),
            )
            .expect_err("empty selection should fail");
        assert_eq!(error.code, atlas_domain::AtlasErrorCode::InvalidInput);

        let oversized = SelectionContextInput {
            selected_text: "a".repeat(MAX_SELECTION_UTF16 + 1),
            end_utf16: u32::try_from(MAX_SELECTION_UTF16 + 1).expect("limit should fit"),
            ..selection()
        };
        let error = SelectionContextAssembler::new()
            .assemble(
                &document(),
                &translation(),
                &oversized,
                ContextBudget::default(),
            )
            .expect_err("oversized selection should fail");
        assert_eq!(error.code, atlas_domain::AtlasErrorCode::InvalidInput);
    }
}
