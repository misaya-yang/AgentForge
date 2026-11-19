//! Source assertions from test_parsing_ir, test_parsing_table_policy,
//! test_parsing_backends and test_parsing_cascade. Pure native adapter calls.
use ai_platform_knowledge_service::migration_knowledge_ingest::{
    parser::{Block, DocIR, PageIR},
    structured::*,
};
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
fn block(id: &str, kind: &str, text: &str, order: usize) -> Block {
    Block {
        block_id: id.into(),
        kind: kind.into(),
        text: text.into(),
        order,
        bbox: None,
        parser: "mineru".into(),
        parser_version: "3.0".into(),
        confidence: None,
        table: None,
        formula: None,
        figure: None,
        metadata: json!({}),
    }
}
fn page(number: usize, blocks: Vec<Block>) -> PageIR {
    PageIR {
        page_number: number,
        blocks,
        parser: "mineru".into(),
        parser_version: "3.0".into(),
        page_width: Some(595.),
        page_height: Some(842.),
        confidence: None,
        hard_page: false,
        metadata: json!({}),
    }
}
fn doc() -> DocIR {
    let mut heading = block("p1b1", "heading", "第一章 总则", 1);
    heading.metadata = json!({"level":2});
    heading.bbox = Some(json!({"x0":0.,"y0":0.,"x1":100.,"y1":20.}));
    let mut table = block("p1b2", "table", "| a | b |\n|---|---|\n| 1 | 2 |", 2);
    table.bbox = Some(json!({"x0":0.,"y0":300.,"x1":400.,"y1":500.}));
    table.table = Some(
        json!({"markdown":table.text,"html":"<table><td colspan='2'>1</td></table>","n_rows":1,"n_cols":2,"column_semantics":["名称","数量"],"has_merged_cells":true,"summary":"一个两列表"}),
    );
    let mut formula = block("p2b0", "formula", "E=mc^2", 0);
    formula.formula = Some(json!({"latex":"E=mc^2","display":true}));
    let mut figure = block("p2b1", "figure", "流程图", 1);
    figure.figure = Some(
        json!({"image_ref":"assets/doc-1/p2/fig1.png","mime":"image/png","description":"流程图"}),
    );
    let mut p2 = page(2, vec![formula, figure]);
    p2.hard_page = true;
    p2.parser = "general_vlm_fallback".into();
    DocIR {
        doc_id: "doc-1".into(),
        content_hash: "sha256:abc".into(),
        filename: "annual.pdf".into(),
        mime: Some("application/pdf".into()),
        schema_version: "1".into(),
        pages: vec![
            page(
                1,
                vec![heading, block("p1b0", "text", "正文内容。", 0), table],
            ),
            p2,
        ],
        metadata: json!({"notes":"x"}),
    }
}
#[test]
fn ir_roundtrip_is_exact_deterministic_and_preserves_version() {
    let d = doc();
    let encoded = serde_json::to_string(&d).unwrap();
    let clone: DocIR = serde_json::from_str(&encoded).unwrap();
    assert_eq!(serde_json::to_string(&clone).unwrap(), encoded);
    assert_eq!(
        clone.pages[0].blocks[2].table.as_ref().unwrap()["has_merged_cells"],
        true
    );
    assert_eq!(clone.pages[0].blocks[0].bbox, d.pages[0].blocks[0].bbox);
    assert!(clone.pages[1].hard_page);
    let mut v = serde_json::to_value(&d).unwrap();
    v["schema_version"] = json!("99");
    let changed: DocIR = serde_json::from_value(v.clone()).unwrap();
    assert_eq!(changed.schema_version, "99");
    assert_eq!(serde_json::to_value(changed).unwrap(), v);
}
#[test]
fn ir_render_order_markers_formula_figure_and_citations() {
    let d = doc();
    let plain = render_document_markdown(&d, false);
    assert!(plain.find("正文内容。").unwrap() < plain.find("## 第一章 总则").unwrap());
    assert!(plain.find("## 第一章 总则").unwrap() < plain.find("| a | b |").unwrap());
    assert!(plain.contains("| 1 | 2 |"));
    assert!(plain.contains("$$E=mc^2$$"));
    assert!(plain.contains("![流程图](assets/doc-1/p2/fig1.png)"));
    assert!(!plain.contains("[Page"));
    let marked = render_document_markdown(&d, true);
    assert_eq!(marked.matches("[Page").count(), 2);
    let b = &d.pages[0].blocks[2];
    let a = citation_anchor(b, 1);
    assert_eq!(a["block_id"], "p1b2");
    assert_eq!(a["page_number"], 1);
    assert_eq!(a["bbox"], json!({"x0":0.,"y0":300.,"x1":400.,"y1":500.}));
    assert_eq!(a["parser"], "mineru");
    let p = derive_chunk_payload(b, 1);
    assert_eq!(p["block_type"], "table");
    assert!(p["table_html"].as_str().unwrap().starts_with("<table>"));
    assert_eq!(p["table_summary"], "一个两列表");
    assert_eq!(p["citation"]["page_number"], 1);
    assert_eq!(
        derive_chunk_payload(&d.pages[1].blocks[0], 2)["formula_latex"],
        "E=mc^2"
    );
}
#[test]
fn ir_heading_clamp_and_html_only_summary() {
    for (level, prefix) in [(1, "# "), (3, "### "), (9, "###### ")] {
        let mut b = block("h", "heading", "T", 0);
        b.metadata = json!({"level":level});
        assert!(render_block_markdown(&b).starts_with(prefix));
    }
    let mut b = block("b", "table", "", 0);
    b.table = Some(json!({"markdown":"","html":"<table>...</table>","summary":"季度财务汇总表"}));
    assert_eq!(render_block_markdown(&b), "季度财务汇总表");
}
fn table(rows: usize) -> Block {
    let md = format!(
        "| 列1 | 列2 |\n|---|---|\n{}",
        (0..rows)
            .map(|i| format!("| c1-{i} | c2-{i} |"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let mut b = block("tbl", "table", &md, 3);
    b.table = Some(
        json!({"markdown":md,"html":"<table/>","n_rows":rows,"n_cols":2,"column_semantics":["名称","数量"],"summary":"季度表"}),
    );
    b
}
#[test]
fn table_tier_boundaries_small_and_html_only_are_preserved() {
    for rows in [0, 20] {
        let b = table(rows);
        assert_eq!(
            serde_json::to_value(split_table_block(&b, 20, 2, 20, 100)).unwrap(),
            json!([b])
        );
    }
    for (rows, count) in [(21, 2), (100, 6), (101, 101), (120, 120)] {
        assert_eq!(split_table_block(&table(rows), 20, 2, 20, 100).len(), count);
    }
    let mut b = table(200);
    b.table.as_mut().unwrap()["markdown"] = json!("");
    assert_eq!(
        serde_json::to_value(split_table_block(&b, 20, 2, 20, 100)).unwrap(),
        json!([b])
    );
}
#[test]
fn table_segments_repeat_headers_overlap_cover_rows_and_keep_provenance() {
    let b = table(50);
    let before = serde_json::to_value(&b).unwrap();
    let out = split_table_block(&b, 20, 2, 20, 100);
    assert_eq!(out.len(), 3);
    let mut covered = BTreeSet::new();
    let mut ids = BTreeSet::new();
    for seg in &out {
        let t = seg.table.as_ref().unwrap();
        let lines = t["markdown"].as_str().unwrap().lines().collect::<Vec<_>>();
        assert_eq!(&lines[..2], &["| 列1 | 列2 |", "|---|---|"]);
        assert!(t["html"].is_null());
        assert_eq!(t["summary"], "季度表");
        assert_eq!(t["column_semantics"], json!(["名称", "数量"]));
        covered.extend(lines[2..].iter().map(|s| s.to_string()));
        ids.insert(seg.block_id.clone());
        assert_eq!(seg.order, 3);
        assert_eq!(seg.parser, "mineru");
    }
    assert_eq!(covered.len(), 50);
    assert_eq!(ids.len(), 3);
    assert!(out[1].text.contains(out[0].text.lines().last().unwrap()));
    assert_eq!(serde_json::to_value(&b).unwrap(), before);
    let large = split_table_block(&table(120), 20, 2, 20, 100);
    assert_eq!(large[0].text, "| 列1 | 列2 |\n|---|---|\n| c1-0 | c2-0 |");
}
#[test]
fn native_payload_adapters_preserve_types_bbox_and_dual_store() {
    let job = PageJob::text("d", "", None);
    let p=payload_to_page(&json!({"blocks":[{"type":"title","text":"标题","bbox":[10,10,200,30]},{"type":"table","text":"|a|\n|-|\n|b|","html":"<table><td colspan='2'>x</td></table>"},{"type":"formula","latex":"x^2","display":false},{"type":"figure","image_ref":"assets/p1/fig.png"},{"type":"sideways_chart"}]}),&job,"mineru","3.1").unwrap();
    assert_eq!(
        p.blocks.iter().map(|b| b.kind.as_str()).collect::<Vec<_>>(),
        vec!["heading", "table", "formula", "figure", "unknown"]
    );
    assert_eq!(
        p.blocks[0].bbox,
        Some(json!({"x0":10,"y0":10,"x1":200,"y1":30}))
    );
    assert_eq!(
        p.blocks[1].table.as_ref().unwrap()["has_merged_cells"],
        true
    );
    assert!(
        !p.blocks[1].table.as_ref().unwrap()["markdown"]
            .as_str()
            .unwrap()
            .is_empty()
    );
    assert_eq!(p.blocks[2].formula.as_ref().unwrap()["display"], false);
    assert_eq!(
        p.blocks[3].figure.as_ref().unwrap()["image_ref"],
        "assets/p1/fig.png"
    );
    let paddle=payload_to_page(&json!({"regions":[{"label":"paragraph_title","text":"标题","confidence":0.98},{"label":"table","text":"| a |\n|---|\n| b |","rows":1,"cols":1},{"label":"inline_formula","latex":"x^2"},{"label":"chart"},{"label":"handwritten_note"}]}),&job,"paddle_ppstructure_v3","ppstructurev3-1").unwrap();
    assert_eq!(
        paddle
            .blocks
            .iter()
            .map(|b| b.kind.as_str())
            .collect::<Vec<_>>(),
        vec!["heading", "table", "formula", "figure", "unknown"]
    );
    assert_eq!(paddle.blocks[2].formula.as_ref().unwrap()["display"], false);
}
fn config(names: &[(&str, f64)]) -> CascadeConfig {
    CascadeConfig {
        stages: names
            .iter()
            .map(|(name, min)| CascadeStage {
                backend: (*name).into(),
                min_confidence: *min,
                require_image: false,
                require_text_layer: false,
            })
            .collect(),
        backend_options: BTreeMap::new(),
        parallelism: 1,
        allow_partial: false,
    }
}
#[tokio::test]
async fn cascade_first_acceptable_cache_copy_and_version_invalidation() {
    let count = Arc::new(AtomicUsize::new(0));
    let mut registry = BackendRegistry::default();
    let calls = count.clone();
    registry.register(
        "mineru",
        "3.0",
        Arc::new(move |_| {
            let calls = calls.clone();
            Box::pin(async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(json!({"score":0.95,"blocks":[{"type":"text","text":"accepted"}]}))
            })
        }),
    );
    registry.register(
        "paddleocr_vl",
        "1",
        Arc::new(|_| Box::pin(async { panic!("later stage must not run") })),
    );
    let cascade = ParserCascade::new(
        config(&[("mineru", 0.8), ("paddleocr_vl", 0.8)]),
        registry.clone(),
    )
    .unwrap();
    let job = PageJob::text("d", "", None);
    let mut first = cascade.parse_page(job.clone()).await.unwrap();
    assert_eq!(first.backend, "mineru");
    assert!(!first.from_cache);
    first.page.blocks[0].text = "mutated".into();
    let cached = cascade.parse_page(job.clone()).await.unwrap();
    assert!(cached.from_cache);
    assert_eq!(cached.page.blocks[0].text, "accepted");
    assert_eq!(count.load(Ordering::SeqCst), 1);
    registry.versions.insert("mineru".into(), "3.1".into());
    let newer = ParserCascade::new(config(&[("mineru", 0.8)]), registry).unwrap();
    assert_ne!(cascade.bundle_version(), newer.bundle_version());
    assert_ne!(
        page_cache_key("hash", 1, "mineru", "3.0", ""),
        page_cache_key("hash", 1, "mineru", "3.1", "")
    );
}
#[tokio::test]
async fn cascade_escalates_exceptions_low_confidence_and_skips_unavailable() {
    let mut registry = BackendRegistry::default();
    registry.register(
        "mineru",
        "1",
        Arc::new(|_| {
            Box::pin(async { Ok(json!({"score":0.2,"blocks":[{"type":"text","text":"low"}]})) })
        }),
    );
    registry.register(
        "paddleocr_vl",
        "1",
        Arc::new(|_| {
            Box::pin(async {
                Ok(json!({"regions":[{"label":"text","text":"best","confidence":0.7}]}))
            })
        }),
    );
    let cascade = ParserCascade::new(
        config(&[("unavailable", 0.), ("mineru", 0.9), ("paddleocr_vl", 0.9)]),
        registry,
    )
    .unwrap();
    let result = cascade
        .parse_page(PageJob::text("d", "", None))
        .await
        .unwrap();
    assert_eq!(result.backend, "paddleocr_vl");
    assert!(result.page.hard_page);
    assert_eq!(result.page.blocks[0].text, "best");
    let mut registry = BackendRegistry::default();
    registry.register(
        "mineru",
        "1",
        Arc::new(|_| {
            Box::pin(async {
                Err(
                    ai_platform_knowledge_service::migration_knowledge_ingest::IngestError::Parser(
                        "synthetic failure".into(),
                    ),
                )
            })
        }),
    );
    let cascade =
        ParserCascade::new(config(&[("mineru", 0.), ("text_layer", 0.)]), registry).unwrap();
    let result = cascade
        .parse_page(PageJob::text("d", "first\n\nsecond", None))
        .await
        .unwrap();
    assert_eq!(result.backend, "text_layer");
    assert_eq!(result.page.blocks.len(), 2);
    assert_eq!(result.page.confidence, Some(1.));
    assert_eq!(result.attempts, vec!["mineru"]);
    assert!(
        cascade
            .parse_page(PageJob::text("empty", "", None))
            .await
            .is_err()
    );
    assert!(ParserCascade::new(config(&[]), BackendRegistry::default()).is_err());
}
#[test]
fn cascade_default_config_roundtrip_and_confidence_semantics() {
    let c = CascadeConfig::default();
    let value = serde_json::to_value(&c).unwrap();
    assert_eq!(
        serde_json::to_value(serde_json::from_value::<CascadeConfig>(value.clone()).unwrap())
            .unwrap(),
        value
    );
    let mut p = page(1, vec![]);
    assert_eq!(page_confidence(&p), 0.);
    p.blocks.push(block("b", "text", "text", 0));
    assert_eq!(page_confidence(&p), 1.);
    p.confidence = Some(0.3);
    assert_eq!(page_confidence(&p), 0.3);
}

#[test]
fn ir_defaults_unknown_kind_reading_order_stats_and_bbox_source_contract() {
    use ai_platform_knowledge_service::migration_knowledge_ingest::parser::BBox;
    let sparse:DocIR=serde_json::from_value(json!({"doc_id":"d","pages":[{"page_number":1,"blocks":[{"block_id":"x","type":"sideways_chart"}]}]})).unwrap();
    assert_eq!(sparse.schema_version, "1");
    assert_eq!(sparse.pages[0].blocks[0].kind, "unknown");
    let d = doc();
    assert_eq!(
        d.page(1)
            .unwrap()
            .sorted_blocks()
            .iter()
            .map(|b| b.block_id.as_str())
            .collect::<Vec<_>>(),
        vec!["p1b0", "p1b1", "p1b2"]
    );
    assert!(d.page(99).is_none());
    assert_eq!(
        d.iter_blocks()
            .iter()
            .map(|b| b.block_id.as_str())
            .collect::<Vec<_>>(),
        vec!["p1b0", "p1b1", "p1b2", "p2b0", "p2b1"]
    );
    let stats = d.stats();
    assert_eq!(stats["pages"], 2);
    assert_eq!(stats["blocks"], 5);
    assert_eq!(stats["tables"], 1);
    assert_eq!(stats["formulas"], 1);
    assert_eq!(stats["figures"], 1);
    assert_eq!(stats["hard_pages"], 1);
    assert_eq!(stats["by_type"]["heading"], 1);
    assert_eq!(stats["parsers"], json!(["general_vlm_fallback", "mineru"]));
    let payloads = iter_chunk_payloads(&d);
    assert_eq!(
        payloads
            .iter()
            .map(|p| p["citation"]["page_number"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![1, 1, 1, 2, 2]
    );
    assert!(
        payloads
            .iter()
            .all(|p| !p["citation"]["block_id"].as_str().unwrap().is_empty())
    );
    assert_eq!(
        BBox::from_list(&json!([1, 2, 3, 4])),
        Some(BBox {
            x0: 1.,
            y0: 2.,
            x1: 3.,
            y1: 4.
        })
    );
    assert!(BBox::from_list(&json!([1, 2])).is_none());
    assert!(BBox::from_list(&serde_json::Value::Null).is_none());
}
#[test]
fn table_classification_header_parser_and_pure_document_rewrite() {
    for (rows, tier) in [
        (20, TableTier::Whole),
        (21, TableTier::Segmented),
        (100, TableTier::Segmented),
        (101, TableTier::RowLevel),
    ] {
        assert_eq!(classify_table(rows, 20, 100), tier);
    }
    assert_eq!(
        parse_markdown_rows("| h1 | h2 |\n|---|---|\n| a | b |"),
        (vec!["| h1 | h2 |", "|---|---|"], vec!["| a | b |"])
    );
    assert_eq!(parse_markdown_rows("no pipes here"), (vec![], vec![]));
    let mut d = doc();
    let mut large = table(50);
    large.block_id = "tbl-big".into();
    let mut small = table(5);
    small.block_id = "tbl-small".into();
    d.pages = vec![page(
        1,
        vec![block("intro", "text", "前言", 0), large, small],
    )];
    let before = serde_json::to_value(&d).unwrap();
    let out = apply_table_policy(&d, 20, 2, 20, 100);
    assert_eq!(serde_json::to_value(&d).unwrap(), before);
    let blocks = out.iter_blocks();
    let ids = blocks
        .iter()
        .map(|b| b.block_id.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(ids.len(), blocks.len());
    assert_eq!(blocks[0].block_id, "intro");
    assert_eq!(blocks.last().unwrap().block_id, "tbl-small");
    assert!(
        blocks[1..blocks.len() - 1]
            .iter()
            .all(|b| b.block_id.starts_with("tbl-big#t"))
    );
    assert_eq!(
        blocks.last().unwrap().table.as_ref().unwrap()["html"],
        "<table/>"
    );
}
