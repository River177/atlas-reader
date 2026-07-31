use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

use crate::identity::{digest, stable_id};
use atlas_domain::{
    AssetMimeType, AtlasError, BlockId, BlockKind, CANONICAL_SCHEMA_VERSION, CanonicalAsset,
    CanonicalBlock, CanonicalChapter, CanonicalDocument, ChapterId, ChapterRole, ContentAtom,
    CoordinateSpace, DocumentId, PageBoundingBox, ParserIdentity, StructuredContent, TableCell,
};
use scraper::{ElementRef, Html, Selector};
use serde::Deserialize;

pub const NORMALIZER_VERSION: &str = "mineru-v1";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MineruAssetInput {
    pub relative_path: String,
    pub sha256: String,
    pub mime_type: AssetMimeType,
    pub size_bytes: u64,
}

#[derive(Clone, Debug)]
pub struct MineruDocumentInput<'a> {
    pub document_id: &'a DocumentId,
    pub artifact_id: &'a str,
    pub source_sha256: &'a str,
    pub parser_version: &'a str,
    pub content_list_json: &'a [u8],
    pub layout_json: &'a [u8],
    pub assets: &'a [MineruAssetInput],
}

#[derive(Clone, Debug, Default)]
pub struct MineruNormalizer;

impl MineruNormalizer {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    pub fn normalize(
        &self,
        input: MineruDocumentInput<'_>,
    ) -> Result<CanonicalDocument, AtlasError> {
        validate_sha256(input.source_sha256, "source SHA-256")?;
        if input.artifact_id.trim().is_empty() {
            return Err(AtlasError::invalid_input("artifact id cannot be empty"));
        }
        let page_sizes = parse_page_sizes(input.layout_json)?;
        let raw_blocks: Vec<RawBlock> =
            serde_json::from_slice(input.content_list_json).map_err(|error| {
                AtlasError::invalid_input(format!("invalid content_list.json: {error}"))
            })?;
        if page_sizes.is_empty() {
            return Err(AtlasError::invalid_input(
                "layout.json does not contain any PDF pages",
            ));
        }

        let assets = validate_assets(input.assets)?;
        let asset_by_path: HashMap<&str, &CanonicalAsset> = assets
            .iter()
            .map(|asset| (asset.relative_path.as_str(), asset))
            .collect();
        let mut builder = DocumentBuilder::new(
            input.artifact_id,
            page_sizes.len() as u32,
            &page_sizes,
            &asset_by_path,
        );

        for raw in raw_blocks {
            builder.accept(raw)?;
        }
        let (title, chapters) = builder.finish()?;
        if chapters.iter().all(|chapter| chapter.blocks.is_empty()) {
            return Err(AtlasError::invalid_input(
                "Cloud MinerU returned no usable document content",
            ));
        }

        Ok(CanonicalDocument {
            schema_version: CANONICAL_SCHEMA_VERSION,
            artifact_id: input.artifact_id.to_owned(),
            document_id: input.document_id.clone(),
            source_sha256: input.source_sha256.to_owned(),
            parser: ParserIdentity {
                name: "Cloud MinerU".to_owned(),
                version: input.parser_version.to_owned(),
                backend: "cloud_mineru".to_owned(),
            },
            normalizer_version: NORMALIZER_VERSION.to_owned(),
            page_count: page_sizes.len() as u32,
            title,
            chapters,
            assets,
        })
    }
}

#[derive(Clone, Copy, Debug)]
struct PageSize {
    width: f64,
    height: f64,
}

#[derive(Debug, Deserialize)]
struct RawLayout {
    pdf_info: Vec<RawPage>,
}

#[derive(Debug, Deserialize)]
struct RawPage {
    page_idx: Option<u32>,
    page_size: [f64; 2],
}

fn parse_page_sizes(bytes: &[u8]) -> Result<Vec<PageSize>, AtlasError> {
    let layout: RawLayout = serde_json::from_slice(bytes)
        .map_err(|error| AtlasError::invalid_input(format!("invalid layout.json: {error}")))?;
    let mut pages = Vec::with_capacity(layout.pdf_info.len());
    for (index, page) in layout.pdf_info.into_iter().enumerate() {
        if page.page_idx.is_some_and(|value| value != index as u32) {
            return Err(AtlasError::invalid_input(
                "layout.json pages are not in physical page order",
            ));
        }
        let [width, height] = page.page_size;
        if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
            return Err(AtlasError::invalid_input(
                "layout.json contains an invalid PDF page size",
            ));
        }
        pages.push(PageSize { width, height });
    }
    Ok(pages)
}

fn validate_assets(inputs: &[MineruAssetInput]) -> Result<Vec<CanonicalAsset>, AtlasError> {
    let mut paths = HashSet::new();
    let mut ids = HashSet::new();
    inputs
        .iter()
        .map(|input| {
            validate_sha256(&input.sha256, "asset SHA-256")?;
            let path = Path::new(&input.relative_path);
            if !is_safe_relative_path(path)
                || path.components().count() != 2
                || path
                    .components()
                    .next()
                    .and_then(|part| part.as_os_str().to_str())
                    != Some("images")
            {
                return Err(AtlasError::invalid_input(
                    "asset path must be a file directly inside images/",
                ));
            }
            if input.size_bytes == 0 {
                return Err(AtlasError::invalid_input("asset cannot be empty"));
            }
            if !paths.insert(input.relative_path.clone()) || !ids.insert(input.sha256.clone()) {
                return Err(AtlasError::invalid_input(
                    "Cloud MinerU returned duplicate assets",
                ));
            }
            Ok(CanonicalAsset {
                id: input.sha256.clone(),
                mime_type: input.mime_type,
                relative_path: input.relative_path.clone(),
                sha256: input.sha256.clone(),
                size_bytes: input.size_bytes,
            })
        })
        .collect()
}

fn validate_sha256(value: &str, label: &str) -> Result<(), AtlasError> {
    if value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(AtlasError::invalid_input(format!("{label} is invalid")))
    }
}

fn is_safe_relative_path(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && !path.is_absolute()
        && path.components().all(|part| {
            matches!(
                part,
                std::path::Component::Normal(_) | std::path::Component::CurDir
            )
        })
}

#[derive(Debug, Deserialize)]
struct RawBlock {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    text_level: Option<u32>,
    #[serde(default)]
    bbox: Option<[f64; 4]>,
    page_idx: u32,
    #[serde(default)]
    img_path: Option<String>,
    #[serde(default)]
    image_caption: Vec<String>,
    #[serde(default)]
    image_footnote: Vec<String>,
    #[serde(default)]
    chart_caption: Vec<String>,
    #[serde(default)]
    chart_footnote: Vec<String>,
    #[serde(default)]
    table_caption: Vec<String>,
    #[serde(default)]
    table_footnote: Vec<String>,
    #[serde(default)]
    table_body: String,
}

struct DocumentBuilder<'a> {
    artifact_id: &'a str,
    page_count: u32,
    page_sizes: &'a [PageSize],
    assets: &'a HashMap<&'a str, &'a CanonicalAsset>,
    title: Option<String>,
    chapters: Vec<ChapterDraft>,
    current: Option<ChapterDraft>,
    references_started: bool,
}

impl<'a> DocumentBuilder<'a> {
    fn new(
        artifact_id: &'a str,
        page_count: u32,
        page_sizes: &'a [PageSize],
        assets: &'a HashMap<&'a str, &'a CanonicalAsset>,
    ) -> Self {
        Self {
            artifact_id,
            page_count,
            page_sizes,
            assets,
            title: None,
            chapters: Vec::new(),
            current: None,
            references_started: false,
        }
    }

    fn accept(&mut self, raw: RawBlock) -> Result<(), AtlasError> {
        let page = raw
            .page_idx
            .checked_add(1)
            .ok_or_else(|| AtlasError::invalid_input("page index overflow"))?;
        if page > self.page_count {
            return Err(AtlasError::invalid_input(
                "content_list.json refers to a page outside the PDF",
            ));
        }
        let bounding_box = raw
            .bbox
            .map(|bbox| normalise_bbox(page, bbox, self.page_sizes[(page - 1) as usize]))
            .transpose()?;

        match raw.kind.as_str() {
            "page_number" | "footer" | "aside_text" => Ok(()),
            "text" if raw.text_level == Some(1) && self.title.is_none() => {
                let title = clean_text(&raw.text);
                if !title.is_empty() {
                    self.title = Some(title);
                }
                Ok(())
            }
            "text" if raw.text_level.is_some() => {
                let title = clean_text(&raw.text);
                if title.is_empty() {
                    return Ok(());
                }
                let role = chapter_role(&title);
                let depth = heading_depth(&title);
                self.start_chapter(title.clone(), depth, role, page);
                self.push_block(
                    BlockKind::Heading,
                    page,
                    bounding_box,
                    structured_text(&title),
                )
            }
            "text" => {
                let text = clean_text(&raw.text);
                if text.is_empty() {
                    return Ok(());
                }
                let kind = if looks_like_list_item(&text) {
                    BlockKind::List
                } else {
                    BlockKind::Paragraph
                };
                self.push_block(kind, page, bounding_box, structured_text(&text))
            }
            "ref_text" => {
                if !self.references_started {
                    self.start_chapter("References".to_owned(), 1, ChapterRole::References, page);
                    self.references_started = true;
                }
                let text = clean_text(&raw.text);
                if text.is_empty() {
                    return Ok(());
                }
                self.push_block(
                    BlockKind::Paragraph,
                    page,
                    bounding_box,
                    structured_text(&text),
                )
            }
            "equation" => {
                let latex = strip_display_math(&raw.text);
                if latex.is_empty() {
                    return Ok(());
                }
                let digest = digest(latex.as_bytes());
                self.push_block(
                    BlockKind::Equation,
                    page,
                    bounding_box,
                    StructuredContent {
                        plain_text: latex.clone(),
                        atoms: vec![ContentAtom::Formula {
                            id: format!("formula-{}", &digest[..16]),
                            latex,
                            display: true,
                        }],
                    },
                )
            }
            "table" => self.push_table(raw, page, bounding_box),
            "image" | "chart" => self.push_figure(raw, page, bounding_box),
            "page_footnote" => {
                let text = clean_text(&raw.text);
                if text.is_empty() {
                    return Ok(());
                }
                self.push_block(
                    BlockKind::Caption,
                    page,
                    bounding_box,
                    structured_text(&text),
                )
            }
            _ => Ok(()),
        }
    }

    fn push_table(
        &mut self,
        raw: RawBlock,
        page: u32,
        bounding_box: Option<PageBoundingBox>,
    ) -> Result<(), AtlasError> {
        let cells = parse_table(&raw.table_body)?;
        let mut atoms = Vec::new();
        let mut plain_parts = Vec::new();
        if !cells.is_empty() {
            for row in &cells {
                plain_parts.push(
                    row.iter()
                        .map(|cell| cell_text(&cell.content))
                        .collect::<Vec<_>>()
                        .join("\t"),
                );
            }
            atoms.push(ContentAtom::Table { rows: cells });
        }
        if let Some(asset) = self.resolve_asset(raw.img_path.as_deref())? {
            atoms.push(ContentAtom::Asset {
                asset_id: asset.id.clone(),
                alt: raw.table_caption.first().cloned(),
            });
        }
        let captions = joined(&raw.table_caption);
        let footnotes = joined(&raw.table_footnote);
        if !captions.is_empty() {
            plain_parts.insert(0, captions.clone());
        }
        if !footnotes.is_empty() {
            plain_parts.push(footnotes);
        }
        if atoms.is_empty() && plain_parts.is_empty() {
            return Ok(());
        }
        self.push_block(
            BlockKind::Table,
            page,
            bounding_box,
            StructuredContent {
                plain_text: plain_parts.join("\n"),
                atoms,
            },
        )?;
        if !captions.is_empty() {
            self.push_block(
                BlockKind::Caption,
                page,
                bounding_box,
                structured_text(&captions),
            )?;
        }
        Ok(())
    }

    fn push_figure(
        &mut self,
        raw: RawBlock,
        page: u32,
        bounding_box: Option<PageBoundingBox>,
    ) -> Result<(), AtlasError> {
        let asset = self.resolve_asset(raw.img_path.as_deref())?;
        let captions = if raw.kind == "chart" {
            raw.chart_caption
        } else {
            raw.image_caption
        };
        let footnotes = if raw.kind == "chart" {
            raw.chart_footnote
        } else {
            raw.image_footnote
        };
        let caption = joined(&captions);
        let footnote = joined(&footnotes);
        let Some(asset) = asset else {
            if caption.is_empty() {
                return Ok(());
            }
            return self.push_block(
                BlockKind::Caption,
                page,
                bounding_box,
                structured_text(&caption),
            );
        };
        self.push_block(
            BlockKind::Figure,
            page,
            bounding_box,
            StructuredContent {
                plain_text: caption.clone(),
                atoms: vec![ContentAtom::Asset {
                    asset_id: asset.id.clone(),
                    alt: (!caption.is_empty()).then_some(caption.clone()),
                }],
            },
        )?;
        if !caption.is_empty() {
            self.push_block(
                BlockKind::Caption,
                page,
                bounding_box,
                structured_text(&caption),
            )?;
        }
        if !footnote.is_empty() {
            self.push_block(
                BlockKind::Caption,
                page,
                bounding_box,
                structured_text(&footnote),
            )?;
        }
        Ok(())
    }

    fn resolve_asset(
        &self,
        relative_path: Option<&str>,
    ) -> Result<Option<&'a CanonicalAsset>, AtlasError> {
        let Some(relative_path) = relative_path else {
            return Ok(None);
        };
        self.assets.get(relative_path).copied().map_or_else(
            || {
                Err(AtlasError::invalid_input(format!(
                    "content_list.json refers to missing asset {relative_path}"
                )))
            },
            |asset| Ok(Some(asset)),
        )
    }

    fn start_chapter(&mut self, title: String, depth: u32, role: ChapterRole, page: u32) {
        if let Some(chapter) = self.current.take() {
            self.chapters.push(chapter);
        }
        self.references_started = role == ChapterRole::References;
        self.current = Some(ChapterDraft {
            source_title: title,
            depth,
            role,
            page_start: page,
            page_end: page,
            blocks: Vec::new(),
        });
    }

    fn ensure_chapter(&mut self, page: u32) {
        if self.current.is_none() {
            self.current = Some(ChapterDraft {
                source_title: "Front Matter".to_owned(),
                depth: 1,
                role: ChapterRole::FrontMatter,
                page_start: page,
                page_end: page,
                blocks: Vec::new(),
            });
        }
    }

    fn push_block(
        &mut self,
        kind: BlockKind,
        page: u32,
        bounding_box: Option<PageBoundingBox>,
        content: StructuredContent,
    ) -> Result<(), AtlasError> {
        if content.is_empty() {
            return Ok(());
        }
        self.ensure_chapter(page);
        let chapter = self.current.as_mut().expect("chapter was ensured");
        chapter.page_end = chapter.page_end.max(page);
        chapter.blocks.push(BlockDraft {
            kind,
            page,
            bounding_boxes: bounding_box.into_iter().collect(),
            content,
        });
        Ok(())
    }

    fn finish(mut self) -> Result<(Option<String>, Vec<CanonicalChapter>), AtlasError> {
        if let Some(chapter) = self.current.take() {
            self.chapters.push(chapter);
        }
        let mut chapters = Vec::with_capacity(self.chapters.len());
        for (chapter_index, draft) in self.chapters.into_iter().enumerate() {
            if draft.blocks.is_empty() {
                continue;
            }
            let title_digest = digest(draft.source_title.as_bytes());
            let chapter_id = ChapterId::new(stable_id(
                "chapter",
                &[self.artifact_id, &chapter_index.to_string(), &title_digest],
            ));
            let blocks = draft
                .blocks
                .into_iter()
                .enumerate()
                .map(|(block_index, block)| {
                    let source_json = serde_json::to_vec(&block.content)
                        .map_err(|error| AtlasError::internal(error.to_string()))?;
                    let source_digest = digest(&source_json);
                    let id = BlockId::new(stable_id(
                        "block",
                        &[
                            chapter_id.as_str(),
                            &block_index.to_string(),
                            block.kind.as_str(),
                            &source_digest,
                        ],
                    ));
                    Ok(CanonicalBlock {
                        id,
                        order_index: block_index as u32,
                        kind: block.kind,
                        page_start: block.page,
                        page_end: block.page,
                        bounding_boxes: block.bounding_boxes,
                        content: block.content,
                        source_digest,
                    })
                })
                .collect::<Result<Vec<_>, AtlasError>>()?;
            chapters.push(CanonicalChapter {
                id: chapter_id,
                order_index: chapters.len() as u32,
                depth: draft.depth,
                role: draft.role,
                source_title: draft.source_title,
                page_start: draft.page_start,
                page_end: draft.page_end,
                blocks,
            });
        }
        Ok((self.title, chapters))
    }
}

struct ChapterDraft {
    source_title: String,
    depth: u32,
    role: ChapterRole,
    page_start: u32,
    page_end: u32,
    blocks: Vec<BlockDraft>,
}

struct BlockDraft {
    kind: BlockKind,
    page: u32,
    bounding_boxes: Vec<PageBoundingBox>,
    content: StructuredContent,
}

fn normalise_bbox(
    page: u32,
    [x0, y0, x1, y1]: [f64; 4],
    page_size: PageSize,
) -> Result<PageBoundingBox, AtlasError> {
    if [x0, y0, x1, y1]
        .into_iter()
        .any(|value| !value.is_finite() || !(0.0..=1000.0).contains(&value))
        || x1 < x0
        || y1 < y0
    {
        return Err(AtlasError::invalid_input(
            "content_list.json contains an invalid bounding box",
        ));
    }
    Ok(PageBoundingBox {
        page,
        x: x0 / 1000.0 * page_size.width,
        y: y0 / 1000.0 * page_size.height,
        width: (x1 - x0) / 1000.0 * page_size.width,
        height: (y1 - y0) / 1000.0 * page_size.height,
        coordinate_space: CoordinateSpace::PdfPoints,
    })
}

fn chapter_role(title: &str) -> ChapterRole {
    let normalised = title
        .trim_matches(|character: char| !character.is_alphanumeric())
        .to_ascii_lowercase();
    if matches!(
        normalised.as_str(),
        "references" | "bibliography" | "reference"
    ) {
        ChapterRole::References
    } else if numeric_heading_depth(title).is_some() {
        ChapterRole::Body
    } else if matches!(
        normalised.as_str(),
        "abstract" | "summary" | "acknowledgements" | "acknowledgments"
    ) {
        ChapterRole::FrontMatter
    } else {
        ChapterRole::Body
    }
}

fn heading_depth(title: &str) -> u32 {
    numeric_heading_depth(title).unwrap_or(1)
}

fn numeric_heading_depth(title: &str) -> Option<u32> {
    let prefix = title
        .trim_start()
        .chars()
        .take_while(|character| character.is_ascii_digit() || *character == '.')
        .collect::<String>();
    let prefix = prefix.trim_end_matches('.');
    if prefix.is_empty()
        || prefix
            .split('.')
            .any(|segment| segment.is_empty() || !segment.bytes().all(|byte| byte.is_ascii_digit()))
    {
        None
    } else {
        Some(prefix.split('.').count() as u32)
    }
}

fn looks_like_list_item(text: &str) -> bool {
    let text = text.trim_start();
    text.starts_with("- ")
        || text.starts_with("• ")
        || text.starts_with("* ")
        || ordered_list_prefix(text)
}

fn ordered_list_prefix(text: &str) -> bool {
    let digits = text.bytes().take_while(u8::is_ascii_digit).count();
    digits > 0 && matches!(text.as_bytes().get(digits), Some(b'.' | b')'))
}

fn clean_text(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_owned()
}

fn strip_display_math(value: &str) -> String {
    value
        .trim()
        .strip_prefix("$$")
        .and_then(|inner| inner.strip_suffix("$$"))
        .unwrap_or(value.trim())
        .trim()
        .to_owned()
}

fn joined(values: &[String]) -> String {
    values
        .iter()
        .map(|value| clean_text(value))
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn structured_text(text: &str) -> StructuredContent {
    let text = clean_text(text);
    StructuredContent {
        atoms: parse_inline_atoms(&text),
        plain_text: text,
    }
}

/// Splits only unambiguous display-neutral formulas and numeric citations. It
/// deliberately leaves malformed delimiters as text rather than inventing a
/// structure the provider did not supply.
fn parse_inline_atoms(text: &str) -> Vec<ContentAtom> {
    let mut atoms = Vec::new();
    let mut cursor = 0;
    while cursor < text.len() {
        let formula = find_formula(text, cursor);
        let citation = find_citation(text, cursor);
        let next = match (formula, citation) {
            (Some(formula), Some(citation)) => {
                if formula.0 <= citation.0 {
                    InlineMatch::Formula(formula)
                } else {
                    InlineMatch::Citation(citation)
                }
            }
            (Some(formula), None) => InlineMatch::Formula(formula),
            (None, Some(citation)) => InlineMatch::Citation(citation),
            (None, None) => {
                push_text(&mut atoms, &text[cursor..]);
                break;
            }
        };
        let (start, end, value) = match &next {
            InlineMatch::Formula((start, end, value))
            | InlineMatch::Citation((start, end, value)) => (*start, *end, value),
        };
        push_text(&mut atoms, &text[cursor..start]);
        match next {
            InlineMatch::Formula(_) => atoms.push(ContentAtom::Formula {
                id: format!("formula-{}", &digest(value.as_bytes())[..16]),
                latex: value.clone(),
                display: false,
            }),
            InlineMatch::Citation(_) => atoms.push(ContentAtom::Citation {
                id: format!("citation-{}", &digest(value.as_bytes())[..16]),
                label: value.clone(),
            }),
        }
        cursor = end;
    }
    if atoms.is_empty() && !text.is_empty() {
        push_text(&mut atoms, text);
    }
    atoms
}

enum InlineMatch {
    Formula((usize, usize, String)),
    Citation((usize, usize, String)),
}

fn find_formula(text: &str, from: usize) -> Option<(usize, usize, String)> {
    let rest = &text[from..];
    let start = rest.find('$')? + from;
    if start > 0 && text.as_bytes()[start - 1] == b'\\' {
        return find_formula(text, start + 1);
    }
    let after = start + 1;
    let end = text[after..].find('$')? + after;
    if end == after || (end > 0 && text.as_bytes()[end - 1] == b'\\') {
        return find_formula(text, end + 1);
    }
    Some((start, end + 1, text[after..end].to_owned()))
}

fn find_citation(text: &str, from: usize) -> Option<(usize, usize, String)> {
    let rest = &text[from..];
    let start = rest.find('[')? + from;
    let end = text[start + 1..].find(']')? + start + 1;
    let inner = &text[start + 1..end];
    let valid = !inner.is_empty()
        && inner.chars().all(|character| {
            character.is_ascii_digit()
                || character.is_ascii_whitespace()
                || matches!(character, ',' | ';' | '-' | '–')
        });
    if valid {
        Some((start, end + 1, text[start..=end].to_owned()))
    } else {
        find_citation(text, end + 1)
    }
}

fn push_text(atoms: &mut Vec<ContentAtom>, value: &str) {
    if !value.is_empty() {
        atoms.push(ContentAtom::Text {
            value: value.to_owned(),
        });
    }
}

fn parse_table(html: &str) -> Result<Vec<Vec<TableCell>>, AtlasError> {
    if html.trim().is_empty() {
        return Ok(Vec::new());
    }
    let document = Html::parse_fragment(html);
    let row_selector =
        Selector::parse("tr").map_err(|error| AtlasError::internal(error.to_string()))?;
    let cell_selector =
        Selector::parse("th, td").map_err(|error| AtlasError::internal(error.to_string()))?;
    let mut rows = Vec::new();
    for (row_index, row) in document.select(&row_selector).enumerate() {
        let mut cells = Vec::new();
        let mut column = 0_u32;
        for cell in row.select(&cell_selector) {
            let row_span = span(&cell, "rowspan")?;
            let column_span = span(&cell, "colspan")?;
            let text = clean_text(&cell.text().collect::<Vec<_>>().join(" "));
            cells.push(TableCell {
                row: row_index as u32,
                column,
                row_span,
                column_span,
                content: parse_inline_atoms(&text),
            });
            column = column
                .checked_add(column_span)
                .ok_or_else(|| AtlasError::invalid_input("table column count overflow"))?;
        }
        if !cells.is_empty() {
            rows.push(cells);
        }
    }
    Ok(rows)
}

fn span(cell: &ElementRef<'_>, name: &str) -> Result<u32, AtlasError> {
    let Some(value) = cell.value().attr(name) else {
        return Ok(1);
    };
    let span = value
        .parse::<u32>()
        .map_err(|_| AtlasError::invalid_input(format!("table has an invalid {name}")))?;
    if (1..=1_000).contains(&span) {
        Ok(span)
    } else {
        Err(AtlasError::invalid_input(format!(
            "table {name} is outside the supported range"
        )))
    }
}

fn cell_text(atoms: &[ContentAtom]) -> String {
    atoms
        .iter()
        .filter_map(|atom| match atom {
            ContentAtom::Text { value } => Some(value.clone()),
            ContentAtom::Formula { latex, .. } => Some(latex.clone()),
            ContentAtom::Citation { label, .. } => Some(label.clone()),
            ContentAtom::LineBreak => Some("\n".to_owned()),
            ContentAtom::Table { .. } | ContentAtom::Asset { .. } => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input<'a>(
        content: &'a [u8],
        layout: &'a [u8],
        assets: &'a [MineruAssetInput],
    ) -> MineruDocumentInput<'a> {
        static DOCUMENT: std::sync::LazyLock<DocumentId> =
            std::sync::LazyLock::new(|| DocumentId::from("document-1"));
        MineruDocumentInput {
            document_id: &DOCUMENT,
            artifact_id: "artifact-1",
            source_sha256: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            parser_version: "vlm",
            content_list_json: content,
            layout_json: layout,
            assets,
        }
    }

    fn layout() -> &'static [u8] {
        br#"{"pdf_info":[{"page_idx":0,"page_size":[612,792]},{"page_idx":1,"page_size":[720,1000]}]}"#
    }

    #[test]
    fn normalizes_axes_independently_and_recovers_heading_depth() {
        let content = br#"[
          {"type":"text","text":"A Paper","text_level":1,"bbox":[100,100,900,150],"page_idx":0},
          {"type":"text","text":"3.2.1 Scaled Attention","text_level":2,"bbox":[100,200,600,250],"page_idx":0},
          {"type":"text","text":"See $x+y$ in [3, 4].","bbox":[100,300,600,400],"page_idx":0}
        ]"#;

        let document = MineruNormalizer::new()
            .normalize(input(content, layout(), &[]))
            .expect("document should normalize");

        assert_eq!(document.title.as_deref(), Some("A Paper"));
        assert_eq!(document.chapters.len(), 1);
        let chapter = &document.chapters[0];
        assert_eq!(chapter.depth, 3);
        assert_eq!(chapter.source_title, "3.2.1 Scaled Attention");
        let bbox = chapter.blocks[1].bounding_boxes[0];
        assert_eq!(bbox.x, 61.2);
        assert_eq!(bbox.y, 237.6);
        assert_eq!(bbox.width, 306.0);
        assert_eq!(bbox.height, 79.2);
        assert!(matches!(
            chapter.blocks[1].content.atoms[1],
            ContentAtom::Formula { .. }
        ));
        assert!(matches!(
            chapter.blocks[1].content.atoms[3],
            ContentAtom::Citation { .. }
        ));
    }

    #[test]
    fn drops_layout_noise_and_keeps_references_out_of_body_chapters() {
        let content = br#"[
          {"type":"aside_text","text":"arXiv banner","bbox":[1,1,2,2],"page_idx":0},
          {"type":"text","text":"Abstract","text_level":2,"bbox":[1,1,2,2],"page_idx":0},
          {"type":"text","text":"Summary","bbox":[1,1,2,2],"page_idx":0},
          {"type":"page_number","text":"1","bbox":[1,1,2,2],"page_idx":0},
          {"type":"ref_text","text":"[1] Reference","bbox":[1,1,2,2],"page_idx":1}
        ]"#;

        let document = MineruNormalizer::new()
            .normalize(input(content, layout(), &[]))
            .expect("document should normalize");

        assert_eq!(document.block_count(), 3);
        assert_eq!(document.chapters[0].role, ChapterRole::FrontMatter);
        assert_eq!(document.chapters[1].role, ChapterRole::References);
        assert_eq!(document.chapters[1].source_title, "References");
    }

    #[test]
    fn maps_a_table_and_its_content_addressed_fallback_asset() {
        let content = br#"[
          {"type":"table","bbox":[0,0,1000,1000],"page_idx":0,
           "img_path":"images/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.jpg",
           "table_caption":["Table 1: Scores"],
           "table_footnote":[],
           "table_body":"<table><tr><th>A</th><th>B</th></tr><tr><td rowspan='2'>1</td><td>$x$</td></tr></table>"}
        ]"#;
        let assets = vec![MineruAssetInput {
            relative_path:
                "images/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.jpg"
                    .to_owned(),
            sha256: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
            mime_type: AssetMimeType::ImageJpeg,
            size_bytes: 10,
        }];

        let document = MineruNormalizer::new()
            .normalize(input(content, layout(), &assets))
            .expect("document should normalize");

        assert_eq!(document.assets.len(), 1);
        let table = &document.chapters[0].blocks[0];
        assert_eq!(table.kind, BlockKind::Table);
        assert!(matches!(table.content.atoms[0], ContentAtom::Table { .. }));
        assert!(matches!(table.content.atoms[1], ContentAtom::Asset { .. }));
        assert_eq!(document.chapters[0].blocks[1].kind, BlockKind::Caption);
    }

    #[test]
    fn ids_are_stable_within_an_artifact_and_change_with_the_artifact() {
        let content = br#"[{"type":"text","text":"Hello","bbox":[0,0,100,100],"page_idx":0}]"#;
        let first = MineruNormalizer::new()
            .normalize(input(content, layout(), &[]))
            .expect("first document should normalize");
        let second = MineruNormalizer::new()
            .normalize(input(content, layout(), &[]))
            .expect("second document should normalize");
        let mut changed = input(content, layout(), &[]);
        changed.artifact_id = "artifact-2";
        let third = MineruNormalizer::new()
            .normalize(changed)
            .expect("third document should normalize");

        assert_eq!(first.chapters[0].id, second.chapters[0].id);
        assert_eq!(
            first.chapters[0].blocks[0].id,
            second.chapters[0].blocks[0].id
        );
        assert_ne!(first.chapters[0].id, third.chapters[0].id);
        assert_ne!(
            first.chapters[0].blocks[0].id,
            third.chapters[0].blocks[0].id
        );
    }

    #[test]
    fn rejects_out_of_range_boxes_and_missing_assets() {
        let bad_box = br#"[{"type":"text","text":"Hello","bbox":[0,0,1001,100],"page_idx":0}]"#;
        let missing_asset = br#"[{"type":"image","img_path":"images/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.jpg","bbox":[0,0,10,10],"page_idx":0}]"#;

        assert!(
            MineruNormalizer::new()
                .normalize(input(bad_box, layout(), &[]))
                .is_err()
        );
        assert!(
            MineruNormalizer::new()
                .normalize(input(missing_asset, layout(), &[]))
                .is_err()
        );
    }
}
