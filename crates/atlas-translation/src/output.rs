use std::collections::{HashMap, HashSet};

use atlas_domain::{AtlasError, BlockId, StructuredContent};
use serde::Deserialize;

use crate::{PreparedBlock, TranslationCompletion};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct OutputRecord {
    pub id: String,
    pub target: String,
}

#[derive(Clone, Debug, Default)]
pub struct TranslationOutputParser {
    buffer: String,
}

impl TranslationOutputParser {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, fragment: &str) -> Result<(), AtlasError> {
        if self.buffer.len().saturating_add(fragment.len()) > 2 * 1024 * 1024 {
            return Err(AtlasError::invalid_input(
                "translation output exceeded the 2 MB safety limit",
            ));
        }
        self.buffer.push_str(fragment);
        Ok(())
    }

    pub fn finish(self) -> Result<Vec<OutputRecord>, AtlasError> {
        let normalized = self
            .buffer
            .lines()
            .filter(|line| !line.trim_start().starts_with("```"))
            .collect::<Vec<_>>()
            .join("\n");
        let trimmed = normalized.trim();
        if trimmed.is_empty() {
            return Ok(Vec::new());
        }
        if let Ok(records) = serde_json::from_str::<Vec<OutputRecord>>(trimmed) {
            return Ok(records);
        }
        if let Ok(record) = serde_json::from_str::<OutputRecord>(trimmed) {
            return Ok(vec![record]);
        }

        let records = complete_json_objects(trimmed)
            .into_iter()
            .filter_map(|object| serde_json::from_str::<OutputRecord>(object).ok())
            .collect::<Vec<_>>();
        if !records.is_empty() {
            return Ok(records);
        }

        let error = trimmed
            .lines()
            .filter(|line| !line.trim().is_empty())
            .find_map(|line| serde_json::from_str::<OutputRecord>(line.trim()).err())
            .map_or_else(
                || "translation returned no complete JSON record".to_owned(),
                |error| format!("translation returned invalid JSON Lines: {error}"),
            );
        Err(AtlasError::invalid_input(error))
    }
}

fn complete_json_objects(value: &str) -> Vec<&str> {
    let mut objects = Vec::new();
    let mut start = None;
    let mut depth = 0_u32;
    let mut in_string = false;
    let mut escaped = false;
    for (index, character) in value.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
            continue;
        }
        match character {
            '"' if depth > 0 => in_string = true,
            '{' => {
                if depth == 0 {
                    start = Some(index);
                }
                depth = depth.saturating_add(1);
            }
            '}' if depth > 0 => {
                depth -= 1;
                if depth == 0
                    && let Some(start) = start.take()
                {
                    objects.push(&value[start..index + character.len_utf8()]);
                }
            }
            _ => {}
        }
    }
    objects
}

#[derive(Clone, Debug)]
pub struct ValidatedTranslation {
    pub block_id: BlockId,
    pub target: StructuredContent,
    pub target_plain_text: String,
    pub validation_json: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidationFailure {
    pub block_id: BlockId,
    pub code: String,
    pub safe_message: String,
}

#[derive(Clone, Debug, Default)]
pub struct OutputValidation {
    pub accepted: Vec<ValidatedTranslation>,
    pub failed: Vec<ValidationFailure>,
}

pub fn validate_output(
    blocks: &[PreparedBlock],
    records: Vec<OutputRecord>,
    completion: &TranslationCompletion,
) -> OutputValidation {
    let expected_order = blocks
        .iter()
        .map(|block| block.block_id.as_str())
        .collect::<Vec<_>>();
    let known_records = records
        .iter()
        .filter(|record| expected_order.contains(&record.id.as_str()))
        .collect::<Vec<_>>();
    let expected_positions = expected_order
        .iter()
        .enumerate()
        .map(|(index, id)| (*id, index))
        .collect::<HashMap<_, _>>();
    let mut out_of_order = HashSet::new();
    for (left_index, left) in known_records.iter().enumerate() {
        for right in known_records.iter().skip(left_index + 1) {
            if expected_positions[left.id.as_str()] > expected_positions[right.id.as_str()] {
                out_of_order.insert(left.id.clone());
                out_of_order.insert(right.id.clone());
            }
        }
    }

    let mut by_id = HashMap::<String, OutputRecord>::new();
    let mut duplicates = HashSet::new();
    for record in records {
        let id = record.id.clone();
        if by_id.insert(id.clone(), record).is_some() {
            duplicates.insert(id);
        }
    }

    let mut validation = OutputValidation::default();
    for block in blocks {
        if duplicates.contains(block.block_id.as_str()) {
            validation.failed.push(failure(
                block,
                "duplicate_block",
                "The model returned a block more than once",
            ));
            continue;
        }
        let Some(record) = by_id.remove(block.block_id.as_str()) else {
            validation.failed.push(failure(
                block,
                if completion.finish_reason.as_deref() == Some("stop") {
                    "missing_block"
                } else {
                    "truncated"
                },
                if completion.finish_reason.as_deref() == Some("stop") {
                    "The model omitted a source block"
                } else {
                    "The model response was truncated before this block"
                },
            ));
            continue;
        };
        if out_of_order.contains(block.block_id.as_str()) {
            validation.failed.push(failure(
                block,
                "block_order_changed",
                "The model changed the block order",
            ));
            continue;
        }
        if record.target.trim().is_empty() {
            validation.failed.push(failure(
                block,
                "empty_target",
                "The model returned an empty translation",
            ));
            continue;
        }
        let byte_limit = block
            .protected
            .source()
            .len()
            .saturating_mul(8)
            .min(128 * 1024);
        if record.target.len() > byte_limit {
            validation.failed.push(failure(
                block,
                "target_too_large",
                "The translation was unexpectedly large",
            ));
            continue;
        }
        if record.target.contains('\0') {
            validation.failed.push(failure(
                block,
                "unsafe_target",
                "The translation contained unsafe control data",
            ));
            continue;
        }
        match block.protected.restore(&record.target) {
            Ok(target) => validation.accepted.push(ValidatedTranslation {
                block_id: block.block_id.clone(),
                target_plain_text: target.plain_text.clone(),
                target,
                validation_json: r#"{"structure":"valid","protectedMarkers":"valid"}"#.to_owned(),
            }),
            Err(_) => validation.failed.push(failure(
                block,
                "protected_marker_changed",
                "The model changed a formula or citation marker",
            )),
        }
    }
    validation
}

fn failure(block: &PreparedBlock, code: &str, message: &str) -> ValidationFailure {
    ValidationFailure {
        block_id: block.block_id.clone(),
        code: code.to_owned(),
        safe_message: message.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atlas_domain::{BlockKind, CanonicalBlock, ContentAtom};

    use crate::{TranslationConfiguration, TranslationPlanner};

    fn configuration() -> TranslationConfiguration {
        TranslationConfiguration {
            profile_id: "test".to_owned(),
            endpoint_base_url: "http://127.0.0.1:8080/v1".to_owned(),
            endpoint_fingerprint: "endpoint".to_owned(),
            model_id: "model".to_owned(),
            context_window: 32_768,
            credential: None,
        }
    }

    fn prepared(id: &str, content: StructuredContent) -> PreparedBlock {
        TranslationPlanner::new()
            .prepare(
                &CanonicalBlock {
                    id: BlockId::from(id),
                    order_index: 0,
                    kind: BlockKind::Paragraph,
                    page_start: 1,
                    page_end: 1,
                    bounding_boxes: Vec::new(),
                    content,
                    source_digest: format!("digest-{id}"),
                },
                &configuration(),
            )
            .expect("fixture should prepare")
    }

    fn stopped() -> TranslationCompletion {
        TranslationCompletion {
            finish_reason: Some("stop".to_owned()),
        }
    }

    #[test]
    fn parser_tolerates_fences_arrays_and_a_final_line_without_newline() {
        let mut fenced = TranslationOutputParser::new();
        fenced
            .push("```json\n[{\"id\":\"a\",\"target\":\"甲\"}]\n```")
            .expect("fragment should fit");
        assert_eq!(fenced.finish().expect("array should parse").len(), 1);

        let mut jsonl = TranslationOutputParser::new();
        jsonl
            .push("{\"id\":\"a\",\"target\":\"甲\"}\n{\"id\":\"b\",\"target\":\"乙\"}")
            .expect("fragment should fit");
        assert_eq!(jsonl.finish().expect("JSON Lines should parse").len(), 2);
    }

    #[test]
    fn parser_rejects_renamed_or_extra_fields() {
        let mut parser = TranslationOutputParser::new();
        parser
            .push(r#"{"id":"a","translation":"甲"}"#)
            .expect("fragment should fit");
        assert!(parser.finish().is_err());

        let mut parser = TranslationOutputParser::new();
        parser
            .push(r#"{"id":"a","target":"甲","note":"extra"}"#)
            .expect("fragment should fit");
        assert!(parser.finish().is_err());
    }

    #[test]
    fn parser_preserves_complete_records_before_a_malformed_tail() {
        let mut parser = TranslationOutputParser::new();
        parser
            .push(
                "[{\"id\":\"a\",\"target\":\"甲\"},{\"id\":\"b\",\"target\":\"乙\"},{\"id\":\"c\"",
            )
            .expect("fragment should fit");

        let records = parser.finish().expect("complete prefix should survive");
        assert_eq!(
            records
                .iter()
                .map(|record| record.id.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );
    }

    #[test]
    fn validation_rejects_unknown_ids_and_changed_order() {
        let blocks = vec![
            prepared("a", StructuredContent::text("Alpha")),
            prepared("b", StructuredContent::text("Beta")),
        ];
        let unknown = validate_output(
            &blocks,
            vec![
                OutputRecord {
                    id: "a".to_owned(),
                    target: "甲".to_owned(),
                },
                OutputRecord {
                    id: "unknown".to_owned(),
                    target: "乙".to_owned(),
                },
            ],
            &stopped(),
        );
        assert_eq!(unknown.accepted.len(), 1);
        assert_eq!(unknown.accepted[0].block_id.as_str(), "a");
        assert_eq!(unknown.failed.len(), 1);
        assert_eq!(unknown.failed[0].block_id.as_str(), "b");
        assert_eq!(unknown.failed[0].code, "missing_block");

        let reordered = validate_output(
            &blocks,
            vec![
                OutputRecord {
                    id: "b".to_owned(),
                    target: "乙".to_owned(),
                },
                OutputRecord {
                    id: "a".to_owned(),
                    target: "甲".to_owned(),
                },
            ],
            &stopped(),
        );
        assert!(
            reordered
                .failed
                .iter()
                .all(|failure| failure.code == "block_order_changed")
        );
    }

    #[test]
    fn validation_rejects_truncation_and_changed_markers() {
        let block = prepared(
            "protected",
            StructuredContent {
                plain_text: "Let x be fixed.".to_owned(),
                atoms: vec![
                    ContentAtom::Text {
                        value: "Let ".to_owned(),
                    },
                    ContentAtom::Formula {
                        id: "formula-1".to_owned(),
                        latex: "x".to_owned(),
                        display: false,
                    },
                    ContentAtom::Text {
                        value: " be fixed.".to_owned(),
                    },
                ],
            },
        );
        let truncated = validate_output(
            std::slice::from_ref(&block),
            Vec::new(),
            &TranslationCompletion {
                finish_reason: Some("length".to_owned()),
            },
        );
        assert_eq!(truncated.failed[0].code, "truncated");

        let damaged = validate_output(
            std::slice::from_ref(&block),
            vec![OutputRecord {
                id: "protected".to_owned(),
                target: block.protected.source().replace("ATLAS", "BROKEN"),
            }],
            &stopped(),
        );
        assert_eq!(damaged.failed[0].code, "protected_marker_changed");
    }
}
