use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{BlockId, ChapterId, DocumentId};

/// Bumping this invalidates every stored artifact and every translation keyed
/// against one, so it changes only when the shape below changes incompatibly.
pub const CANONICAL_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename_all = "snake_case")]
pub enum BlockKind {
    Heading,
    Paragraph,
    List,
    Equation,
    Table,
    Figure,
    Caption,
}

impl BlockKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Heading => "heading",
            Self::Paragraph => "paragraph",
            Self::List => "list",
            Self::Equation => "equation",
            Self::Table => "table",
            Self::Figure => "figure",
            Self::Caption => "caption",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "heading" => Some(Self::Heading),
            "paragraph" => Some(Self::Paragraph),
            "list" => Some(Self::List),
            "equation" => Some(Self::Equation),
            "table" => Some(Self::Table),
            "figure" => Some(Self::Figure),
            "caption" => Some(Self::Caption),
            _ => None,
        }
    }

    /// Whether the block carries prose a translator should see. Equations and
    /// bare figures do not, so the translation planner skips them outright
    /// rather than spending budget confirming there is nothing to do.
    #[must_use]
    pub fn is_translatable(self) -> bool {
        matches!(
            self,
            Self::Heading | Self::Paragraph | Self::List | Self::Table | Self::Caption
        )
    }
}

/// Chapters are a flat list, so this records why a chapter exists rather than
/// where it sits in a tree. Prefetch uses it to stay out of the bibliography.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename_all = "snake_case")]
pub enum ChapterRole {
    FrontMatter,
    Body,
    References,
}

impl ChapterRole {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FrontMatter => "front_matter",
            Self::Body => "body",
            Self::References => "references",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "front_matter" => Some(Self::FrontMatter),
            "body" => Some(Self::Body),
            "references" => Some(Self::References),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename_all = "snake_case")]
pub enum CoordinateSpace {
    /// The only space the reader overlays against. Providers that normalise
    /// coordinates differently are converted during normalisation, never here.
    #[default]
    PdfPoints,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, rename_all = "camelCase")]
pub struct PageBoundingBox {
    pub page: u32,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub coordinate_space: CoordinateSpace,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, rename_all = "snake_case")]
pub enum AssetMimeType {
    #[serde(rename = "image/png")]
    ImagePng,
    #[serde(rename = "image/jpeg")]
    ImageJpeg,
    #[serde(rename = "image/webp")]
    ImageWebp,
}

impl AssetMimeType {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ImagePng => "image/png",
            Self::ImageJpeg => "image/jpeg",
            Self::ImageWebp => "image/webp",
        }
    }

    /// Derived from the file extension rather than sniffed content because the
    /// unpacker has already verified the magic bytes before an asset gets here.
    #[must_use]
    pub fn from_extension(extension: &str) -> Option<Self> {
        match extension.to_ascii_lowercase().as_str() {
            "png" => Some(Self::ImagePng),
            "jpg" | "jpeg" => Some(Self::ImageJpeg),
            "webp" => Some(Self::ImageWebp),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, rename_all = "camelCase")]
pub struct TableCell {
    pub row: u32,
    pub column: u32,
    pub row_span: u32,
    pub column_span: u32,
    pub content: Vec<ContentAtom>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export, rename_all = "snake_case", tag = "type")]
pub enum ContentAtom {
    Text {
        value: String,
    },
    Formula {
        id: String,
        latex: String,
        display: bool,
    },
    Citation {
        id: String,
        label: String,
    },
    LineBreak,
    Table {
        rows: Vec<Vec<TableCell>>,
    },
    Asset {
        #[serde(rename = "assetId")]
        #[ts(rename = "assetId")]
        asset_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        alt: Option<String>,
    },
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, rename_all = "camelCase")]
pub struct StructuredContent {
    /// The flattened reading-order text. Kept alongside the atoms so search and
    /// digests never have to walk the tree, and so a renderer that cannot draw
    /// an atom still has something to show.
    pub plain_text: String,
    pub atoms: Vec<ContentAtom>,
}

impl StructuredContent {
    #[must_use]
    pub fn text(value: impl Into<String>) -> Self {
        let value = value.into();
        Self {
            atoms: vec![ContentAtom::Text {
                value: value.clone(),
            }],
            plain_text: value,
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.atoms.is_empty()
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, rename_all = "camelCase")]
pub struct CanonicalBlock {
    pub id: BlockId,
    pub order_index: u32,
    pub kind: BlockKind,
    pub page_start: u32,
    pub page_end: u32,
    pub bounding_boxes: Vec<PageBoundingBox>,
    pub content: StructuredContent,
    /// Digest of the source content only. Translation caches key against this,
    /// so it must not absorb the block's position or identity.
    pub source_digest: String,
}

impl CanonicalBlock {
    #[must_use]
    pub fn is_translatable(&self) -> bool {
        self.kind.is_translatable()
            && if self.content.atoms.is_empty() {
                !self.content.plain_text.trim().is_empty()
            } else {
                self.content.atoms.iter().any(atom_has_translatable_text)
            }
    }
}

fn atom_has_translatable_text(atom: &ContentAtom) -> bool {
    match atom {
        ContentAtom::Text { value } => !value.trim().is_empty(),
        ContentAtom::Table { rows } => rows
            .iter()
            .flatten()
            .any(|cell| cell.content.iter().any(atom_has_translatable_text)),
        ContentAtom::Formula { .. }
        | ContentAtom::Citation { .. }
        | ContentAtom::LineBreak
        | ContentAtom::Asset { .. } => false,
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, rename_all = "camelCase")]
pub struct CanonicalChapter {
    pub id: ChapterId,
    pub order_index: u32,
    /// Outline nesting, starting at 1. Cloud MinerU marks every numbered
    /// heading at the same `text_level`, so this is recovered from the title's
    /// numeric prefix rather than trusted from the provider.
    pub depth: u32,
    pub role: ChapterRole,
    pub source_title: String,
    pub page_start: u32,
    pub page_end: u32,
    pub blocks: Vec<CanonicalBlock>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, rename_all = "camelCase")]
pub struct CanonicalAsset {
    pub id: String,
    pub mime_type: AssetMimeType,
    pub relative_path: String,
    pub sha256: String,
    #[ts(type = "number")]
    pub size_bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, rename_all = "camelCase")]
pub struct ParserIdentity {
    pub name: String,
    pub version: String,
    pub backend: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, rename_all = "camelCase")]
pub struct CanonicalDocument {
    pub schema_version: u32,
    pub artifact_id: String,
    pub document_id: DocumentId,
    pub source_sha256: String,
    pub parser: ParserIdentity,
    pub normalizer_version: String,
    pub page_count: u32,
    pub title: Option<String>,
    pub chapters: Vec<CanonicalChapter>,
    pub assets: Vec<CanonicalAsset>,
}

impl CanonicalDocument {
    #[must_use]
    pub fn block_count(&self) -> usize {
        self.chapters
            .iter()
            .map(|chapter| chapter.blocks.len())
            .sum()
    }

    #[must_use]
    pub fn find_block(&self, block_id: &BlockId) -> Option<&CanonicalBlock> {
        self.chapters
            .iter()
            .flat_map(|chapter| chapter.blocks.iter())
            .find(|block| &block.id == block_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_break_atom_round_trips_as_a_tagged_object() {
        let encoded = serde_json::to_string(&ContentAtom::LineBreak).expect("atom should encode");
        assert_eq!(encoded, r#"{"type":"line_break"}"#);

        let decoded: ContentAtom = serde_json::from_str(&encoded).expect("atom should decode");
        assert_eq!(decoded, ContentAtom::LineBreak);
    }

    #[test]
    fn an_asset_atom_omits_a_missing_alt_rather_than_encoding_null() {
        let encoded = serde_json::to_string(&ContentAtom::Asset {
            asset_id: "asset-1".to_owned(),
            alt: None,
        })
        .expect("atom should encode");

        assert_eq!(encoded, r#"{"type":"asset","assetId":"asset-1"}"#);
    }

    #[test]
    fn mime_types_encode_as_their_wire_names() {
        let encoded =
            serde_json::to_string(&AssetMimeType::ImageJpeg).expect("mime type should encode");
        assert_eq!(encoded, r#""image/jpeg""#);
    }

    #[test]
    fn equations_and_figures_are_not_offered_to_the_translator() {
        assert!(!BlockKind::Equation.is_translatable());
        assert!(!BlockKind::Figure.is_translatable());
        assert!(BlockKind::Paragraph.is_translatable());
        assert!(BlockKind::Table.is_translatable());
    }

    #[test]
    fn blocks_without_prose_are_not_offered_to_the_translator() {
        let formula_only = CanonicalBlock {
            id: BlockId::from("formula-only"),
            order_index: 0,
            kind: BlockKind::Paragraph,
            page_start: 1,
            page_end: 1,
            bounding_boxes: Vec::new(),
            content: StructuredContent {
                plain_text: "x".to_owned(),
                atoms: vec![ContentAtom::Formula {
                    id: "formula-1".to_owned(),
                    latex: "x".to_owned(),
                    display: false,
                }],
            },
            source_digest: "digest".to_owned(),
        };

        assert!(!formula_only.is_translatable());
    }

    #[test]
    fn block_kinds_survive_a_round_trip_through_their_storage_form() {
        for kind in [
            BlockKind::Heading,
            BlockKind::Paragraph,
            BlockKind::List,
            BlockKind::Equation,
            BlockKind::Table,
            BlockKind::Figure,
            BlockKind::Caption,
        ] {
            assert_eq!(BlockKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(BlockKind::parse("marginalia"), None);
    }

    #[test]
    fn chapter_roles_survive_a_round_trip_through_their_storage_form() {
        for role in [
            ChapterRole::FrontMatter,
            ChapterRole::Body,
            ChapterRole::References,
        ] {
            assert_eq!(ChapterRole::parse(role.as_str()), Some(role));
        }
        assert_eq!(ChapterRole::parse("appendix"), None);
    }
}
