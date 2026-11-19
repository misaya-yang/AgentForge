use super::*;
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SourceIdentity {
    pub tenant_id: String,
    pub dataset_id: String,
    pub document_id: String,
    pub document_version: String,
    pub publication_generation: String,
    pub source_sha256: String,
    pub parser_profile: String,
    pub execution_id: Option<String>,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Region {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Structure {
    Heading {
        level: u8,
    },
    Paragraph,
    List {
        ordered: bool,
    },
    Table {
        html: Option<String>,
        markdown: String,
        rows: usize,
        columns: usize,
        continuation: Option<String>,
    },
    Formula {
        latex: String,
        display: bool,
    },
    Figure {
        caption: String,
        crop: Option<String>,
    },
    Code {
        language: Option<String>,
    },
    Unknown {
        original_kind: String,
        original_payload: Value,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BlockV2 {
    pub id: String,
    pub reading_order: usize,
    pub text: String,
    pub region: Option<Region>,
    pub structure: Structure,
    pub provenance: Value,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageV2 {
    pub page_number: usize,
    pub width: Option<f64>,
    pub height: Option<f64>,
    pub coordinate_space: String,
    pub blocks: Vec<BlockV2>,
    pub route: Value,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocV2 {
    pub schema: String,
    pub filename: String,
    pub mime: Option<String>,
    pub source: SourceIdentity,
    pub pages: Vec<PageV2>,
    pub native_metadata: Value,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Citation {
    pub source: SourceIdentity,
    pub page_number: usize,
    pub block_id: String,
    pub region: Option<Region>,
    pub coordinate_space: String,
}
impl DocV2 {
    pub fn citation(&self, page: usize, block: &str) -> Option<Citation> {
        let page = self.pages.iter().find(|p| p.page_number == page)?;
        let block = page.blocks.iter().find(|b| b.id == block)?;
        Some(Citation {
            source: self.source.clone(),
            page_number: page.page_number,
            block_id: block.id.clone(),
            region: block.region,
            coordinate_space: page.coordinate_space.clone(),
        })
    }
    /// Table/formula blocks remain atomic. No cross-page merge invents geometry;
    /// repeated continuation IDs preserve the relation while retaining citations.
    pub fn atomic_chunks(&self) -> Vec<Value> {
        let mut chunks = Vec::new();
        let mut sections: Vec<String> = Vec::new();
        for page in &self.pages {
            for block in &page.blocks {
                if let Structure::Heading { level } = block.structure {
                    sections.truncate(level.saturating_sub(1) as usize);
                    sections.push(block.text.clone());
                }
                chunks.push(json!({"text":block.text,"structure":block.structure,"section_path":sections,"citation":self.citation(page.page_number,&block.id)}));
            }
        }
        chunks
    }
}
fn nonempty(value: &str) -> bool {
    !value.trim().is_empty()
}
fn valid_dimension(value: Option<f64>) -> bool {
    value.is_none_or(|v| v.is_finite() && v > 0. && v <= 1_000_000.)
}
pub(super) fn validate_page(page: &PageV2, budget: &Budgets) -> Result<()> {
    if page.page_number == 0
        || !valid_dimension(page.width)
        || !valid_dimension(page.height)
        || !matches!(
            page.coordinate_space.as_str(),
            "page_points" | "render_pixels" | "native_page_units" | "unknown"
        )
        || page.blocks.len() > budget.max_blocks_per_page
    {
        return Err(invalid("invalid page geometry/block budget"));
    }
    let mut ids = std::collections::BTreeSet::new();
    let mut order = std::collections::BTreeSet::new();
    let mut chars = 0usize;
    for block in &page.blocks {
        chars = chars.saturating_add(block.text.len());
        if !nonempty(&block.id)
            || !ids.insert(&block.id)
            || !order.insert(block.reading_order)
            || chars > budget.max_page_text_bytes
        {
            return Err(invalid(
                "duplicate block identity/order or page text budget exceeded",
            ));
        }
        super::super::parser::require_text_budget(&block.text)?;
        if let Some(r) = block.region {
            if [r.x0, r.y0, r.x1, r.y1].iter().any(|v| !v.is_finite())
                || r.x0 < 0.
                || r.y0 < 0.
                || r.x1 <= r.x0
                || r.y1 <= r.y0
                || page.coordinate_space == "unknown"
                || page.width.is_none_or(|w| r.x1 > w)
                || page.height.is_none_or(|h| r.y1 > h)
            {
                return Err(invalid("invalid/out-of-page block region"));
            }
        }
        match &block.structure {
            Structure::Heading { level } if !(1..=6).contains(level) => {
                return Err(invalid("heading level outside 1..6"));
            }
            Structure::Table {
                html,
                markdown,
                rows,
                columns,
                ..
            } => {
                if *rows == 0
                    || *columns == 0
                    || rows.saturating_mul(*columns) > budget.max_table_cells
                    || markdown.len() > budget.max_page_text_bytes
                    || html
                        .as_ref()
                        .is_some_and(|s| s.len() > budget.max_page_text_bytes)
                    || (!nonempty(markdown) && html.as_ref().is_none_or(|s| !nonempty(s)))
                {
                    return Err(invalid("table structure/budget invalid"));
                }
                if let Some(html) = html {
                    validate_table_html(html)?;
                }
            }
            Structure::Formula { latex, .. }
                if !nonempty(latex) || latex.len() > budget.max_formula_bytes =>
            {
                return Err(invalid("invalid formula/budget"));
            }
            Structure::Figure {
                crop: Some(crop), ..
            } if crop.contains("://")
                || crop.starts_with('/')
                || crop.split('/').any(|s| s == "..") =>
            {
                return Err(invalid(
                    "figure crop must be a relative source asset reference",
                ));
            }
            _ => {}
        }
    }
    Ok(())
}
fn validate_table_html(value: &str) -> Result<()> {
    if !value.to_ascii_lowercase().contains("<table")
        || !value.to_ascii_lowercase().contains("</table>")
    {
        return Err(invalid("incomplete table HTML"));
    }
    let html = scraper::Html::parse_fragment(value);
    let all = scraper::Selector::parse("*").map_err(|_| invalid("HTML selector unavailable"))?;
    for element in html.select(&all) {
        if ![
            "html", "table", "thead", "tbody", "tfoot", "tr", "th", "td", "caption", "colgroup",
            "col", "br", "p", "span", "b", "strong", "em", "i", "sub", "sup",
        ]
        .contains(&element.value().name())
        {
            return Err(invalid("unsafe table HTML element"));
        }
        for (name, value) in element.value().attrs() {
            if !["rowspan", "colspan"].contains(&name)
                || value
                    .parse::<usize>()
                    .ok()
                    .is_none_or(|n| n == 0 || n > 1024)
            {
                return Err(invalid("unsafe table HTML attribute"));
            }
        }
    }
    Ok(())
}
pub(super) fn validate_source(source: &SourceIdentity, native: &DocIR) -> Result<()> {
    if [
        &source.tenant_id,
        &source.dataset_id,
        &source.document_id,
        &source.document_version,
        &source.publication_generation,
        &source.parser_profile,
    ]
    .iter()
    .any(|s| !nonempty(s))
        || source.document_id != native.doc_id
        || source.source_sha256 != native.content_hash
        || source.source_sha256.len() != 64
        || !source.source_sha256.bytes().all(|c| c.is_ascii_hexdigit())
    {
        return Err(invalid("source/version/generation binding invalid"));
    }
    Ok(())
}
fn native_structure(block: &super::super::parser::Block) -> Structure {
    match block.kind.as_str() {
        "heading" => Structure::Heading {
            level: block.metadata["level"].as_u64().unwrap_or(1).clamp(1, 6) as u8,
        },
        "text" | "paragraph" | "caption" => Structure::Paragraph,
        "list" => Structure::List {
            ordered: block.metadata["ordered"].as_bool().unwrap_or(false),
        },
        "code" => Structure::Code {
            language: block.metadata["language"].as_str().map(str::to_owned),
        },
        "formula"
            if block
                .formula
                .as_ref()
                .and_then(|v| v["latex"].as_str())
                .is_some() =>
        {
            Structure::Formula {
                latex: block.formula.as_ref().unwrap()["latex"]
                    .as_str()
                    .unwrap()
                    .into(),
                display: block.formula.as_ref().unwrap()["display"]
                    .as_bool()
                    .unwrap_or(true),
            }
        }
        "table"
            if block.table.as_ref().is_some_and(|v| {
                v["n_rows"].as_u64().unwrap_or(0) > 0 && v["n_cols"].as_u64().unwrap_or(0) > 0
            }) =>
        {
            let t = block.table.as_ref().unwrap();
            Structure::Table {
                html: t["html"].as_str().map(str::to_owned),
                markdown: t["markdown"].as_str().unwrap_or("").into(),
                rows: t["n_rows"].as_u64().unwrap() as usize,
                columns: t["n_cols"].as_u64().unwrap() as usize,
                continuation: t["continuation"].as_str().map(str::to_owned),
            }
        }
        "figure" => Structure::Figure {
            caption: block
                .figure
                .as_ref()
                .and_then(|f| f["caption"].as_str())
                .unwrap_or(&block.text)
                .into(),
            crop: block
                .figure
                .as_ref()
                .and_then(|figure| figure["crop"].as_str())
                .map(str::to_owned),
        },
        _ => Structure::Unknown {
            original_kind: block.kind.clone(),
            original_payload: json!({"table":block.table,"formula":block.formula,"figure":block.figure}),
        },
    }
}
pub(super) fn upgrade_page(page: &super::super::parser::PageIR) -> PageV2 {
    PageV2{page_number:page.page_number,width:page.page_width,height:page.page_height,coordinate_space:if page.page_width.is_some()&&page.page_height.is_some(){page.metadata["coordinate_space"].as_str().filter(|space|matches!(*space,"render_pixels"|"page_points")).unwrap_or("native_page_units")}else{"unknown"}.into(),blocks:page.sorted_blocks().into_iter().map(|b|BlockV2{id:b.block_id.clone(),reading_order:b.order,text:b.text.clone(),region:if page.page_width.is_some()&&page.page_height.is_some(){b.bbox.as_ref().and_then(super::super::parser::BBox::from_list).map(|r|Region{x0:r.x0,y0:r.y0,x1:r.x1,y1:r.y1})}else{None},structure:native_structure(b),provenance:json!({"parser":b.parser,"parser_version":b.parser_version,"confidence":b.confidence,"metadata":b.metadata,"original_bbox":b.bbox,"original_table":b.table,"original_formula":b.formula,"original_figure":b.figure})}).collect(),route:json!({"selected":"native","native_parser":page.parser,"native_version":page.parser_version})}
}
