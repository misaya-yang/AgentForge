use super::*;
fn source() -> SourceIdentity {
    SourceIdentity {
        tenant_id: "t".into(),
        dataset_id: "d".into(),
        document_id: "doc".into(),
        document_version: "version-2".into(),
        publication_generation: "generation-3".into(),
        source_sha256: "a".repeat(64),
        parser_profile: "layout-fixture/v2".into(),
        execution_id: Some("execution-4".into()),
    }
}
fn native() -> DocIR {
    serde_json::from_value(json!({"doc_id":"doc","content_hash":"a".repeat(64),"filename":"table.pdf","mime":"application/pdf","schema_version":"1","metadata":{"native":true},"pages":[
        {"page_number":1,"page_width":600,"page_height":800,"parser":"native","parser_version":"pinned","blocks":[{"block_id":"table-a","type":"table","order":0,"text":"A | B\n1 | 2","bbox":[10,20,300,100],"table":{"html":"<table><tr><th>A</th><th>B</th></tr><tr><td>1</td><td>2</td></tr></table>","markdown":"| A | B |\n|---|---|\n|1|2|","n_rows":2,"n_cols":2,"continuation":"table-1"}}]},
        {"page_number":2,"page_width":600,"page_height":800,"parser":"native","parser_version":"pinned","blocks":[{"block_id":"formula-b","type":"formula","order":0,"text":"E=mc^2","bbox":[10,20,300,100],"formula":{"latex":"E=mc^2","display":true}}]}
    ]})).unwrap()
}
fn profile() -> LayoutProfile {
    LayoutProfile {
        mode: LayoutMode::Thresholds,
        fingerprint: "layout-fixture/v2".into(),
        provider: "registered".into(),
        model: "layout".into(),
        revision: "immutable-v1".into(),
        min_text_coverage: 0.7,
        image_ratio_threshold: 0.8,
        table_density_threshold: 0.4,
        allow_known_failure_native_fallback: true,
        budget: Budgets::default(),
    }
}
fn image() -> PageImage {
    PageImage {
        png: b"\x89PNG\r\n\x1a\nfixture".to_vec(),
        width: 1200,
        height: 1600,
    }
}
fn wire(blocks: Vec<BlockV2>, page: usize) -> Value {
    json!({"choices":[{"finish_reason":"stop","message":{"content":serde_json::to_string(&LayoutOutput{schema:"docir-layout/v2".into(),page_number:page,blocks}).unwrap()}}]})
}
fn block() -> BlockV2 {
    BlockV2 {
        id: "region-1".into(),
        reading_order: 0,
        text: "E=mc^2".into(),
        region: Some(Region {
            x0: 10.,
            y0: 10.,
            x1: 200.,
            y1: 100.,
        }),
        structure: Structure::Formula {
            latex: "E=mc^2".into(),
            display: true,
        },
        provenance: json!({}),
    }
}
#[tokio::test]
async fn actual_native_docir_upgrades_without_model_io_preserving_cross_page_citations() {
    let original = native();
    let doc = upgrade(&original, source(), &BTreeMap::new(), None)
        .await
        .unwrap();
    assert_eq!(doc.schema, "docir/v2");
    assert_eq!(doc.pages.len(), 2);
    assert_eq!(
        doc.pages[0].blocks[0].text,
        original.pages[0].blocks[0].text
    );
    assert!(matches!(
        &doc.pages[0].blocks[0].structure,
        Structure::Table {
            rows: 2,
            columns: 2,
            ..
        }
    ));
    assert!(
        matches!(&doc.pages[1].blocks[0].structure,Structure::Formula{latex,..} if latex=="E=mc^2")
    );
    let a = doc.citation(1, "table-a").unwrap();
    let b = doc.citation(2, "formula-b").unwrap();
    assert_eq!(a.page_number, 1);
    assert_eq!(b.page_number, 2);
    assert_eq!(a.source.publication_generation, "generation-3");
    assert_eq!(a.source.document_version, "version-2");
    assert_eq!(a.source, b.source);
    assert_eq!(a.region.unwrap().x1, 300.);
    assert!(doc.citation(1, "formula-b").is_none());
    assert_eq!(doc.atomic_chunks().len(), 2);
}
#[test]
fn measured_routing_keeps_digital_text_fast_and_selects_scan_table_complex_pages() {
    let p = profile();
    let mut e = PageEvidence {
        measurements: Some(PageMeasurements {
            text_coverage: 0.95,
            image_ratio: 0.1,
            table_density: 0.05,
        }),
        image: None,
    };
    assert!(!p.route(&e).unwrap());
    e.measurements.as_mut().unwrap().text_coverage = 0.1;
    assert!(p.route(&e).unwrap());
    e.measurements.as_mut().unwrap().text_coverage = 0.95;
    e.measurements.as_mut().unwrap().table_density = 0.7;
    assert!(p.route(&e).unwrap());
    e.measurements.as_mut().unwrap().table_density = f64::NAN;
    assert!(p.route(&e).is_err());
}
#[test]
fn layout_schema_preserves_pixels_formula_and_rejects_cross_page_duplicate_and_bad_region() {
    let page = schema::upgrade_page(&native().pages[0]);
    let p = profile();
    let parsed = layout_page(wire(vec![block()], 1), &page, &image(), &p).unwrap();
    assert_eq!(parsed.coordinate_space, "render_pixels");
    assert_eq!(parsed.width, Some(1200.));
    assert!(layout_page(wire(vec![block()], 2), &page, &image(), &p).is_err());
    assert!(layout_page(wire(vec![block(), block()], 1), &page, &image(), &p).is_err());
    let mut outside = block();
    outside.region.as_mut().unwrap().x1 = 1201.;
    assert!(layout_page(wire(vec![outside], 1), &page, &image(), &p).is_err());
    let mut missing = block();
    missing.region = None;
    assert!(layout_page(wire(vec![missing], 1), &page, &image(), &p).is_err());
    assert!(
        layout_page(
            json!({"choices":[{"message":{"content":"```json\n{}\n```"}}]}),
            &page,
            &image(),
            &p
        )
        .is_err()
    );
}
#[test]
fn table_output_requires_both_representations_and_rejects_active_html() {
    let page = schema::upgrade_page(&native().pages[0]);
    let p = profile();
    let mut b = block();
    b.structure = Structure::Table {
        html: Some("<table><tr><td colspan=\"2\">x</td></tr></table>".into()),
        markdown: "|x|".into(),
        rows: 1,
        columns: 2,
        continuation: Some("continued-table".into()),
    };
    assert!(layout_page(wire(vec![b.clone()], 1), &page, &image(), &p).is_ok());
    for html in [
        None,
        Some("<table><tr><td onclick=\"steal()\">x</td></tr></table>".into()),
        Some("<table><tr><td><img src=\"https://evil\"></td></tr></table>".into()),
    ] {
        if let Structure::Table { html: field, .. } = &mut b.structure {
            *field = html;
        }
        assert!(layout_page(wire(vec![b.clone()], 1), &page, &image(), &p).is_err());
    }
}
#[test]
fn actual_gateway_payload_has_explicit_cap_and_no_credentials() {
    let input = payload(&profile(), 1, &image(), "native untrusted text").unwrap();
    assert_eq!(input["max_tokens"], 8192);
    assert_eq!(input["response_format"]["type"], "json_object");
    assert!(
        input["messages"][1]["content"][0]["image_url"]["url"]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,")
    );
    assert!(!input.to_string().contains("api_key"));
}
#[tokio::test]
async fn source_generation_geometry_and_page_budgets_cannot_be_forged() {
    let mut s = source();
    s.document_id = "other".into();
    assert!(upgrade(&native(), s, &BTreeMap::new(), None).await.is_err());
    let mut n = native();
    n.pages[1].page_number = 1;
    assert!(upgrade(&n, source(), &BTreeMap::new(), None).await.is_err());
    let mut p = profile();
    p.budget.max_pages = 1;
    assert!(
        upgrade(&native(), source(), &BTreeMap::new(), Some(&p))
            .await
            .is_err()
    );
    let mut s = source();
    s.parser_profile = "other-profile".into();
    assert!(
        upgrade(&native(), s, &BTreeMap::new(), Some(&profile()))
            .await
            .is_err()
    );
}
#[tokio::test]
async fn missing_layout_image_has_explicit_native_fallback_but_never_fake_layout_success() {
    let evidence = BTreeMap::from([(
        1,
        PageEvidence {
            measurements: Some(PageMeasurements {
                text_coverage: 0.,
                image_ratio: 1.,
                table_density: 0.,
            }),
            image: None,
        },
    )]);
    let doc = upgrade(&native(), source(), &evidence, Some(&profile()))
        .await
        .unwrap();
    assert_eq!(doc.pages[0].route["selected"], "native");
    assert_eq!(doc.pages[0].route["reason"], "rendered_page_unavailable");
    let mut strict = profile();
    strict.allow_known_failure_native_fallback = false;
    assert!(
        upgrade(&native(), source(), &evidence, Some(&strict))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn native_plaintext_parser_runs_real_decode_into_v2_without_model_or_daemon() {
    let bytes = b"Native UTF-8 paragraph with formula E=mc^2.".to_vec();
    let mut identity = source();
    identity.source_sha256 = super::super::sha256(&bytes);
    let parser = super::super::parser::DocumentParser::new(
        super::super::parser::ParserConfig::new(std::env::temp_dir()),
    );
    let doc = parse_native(
        parser,
        bytes,
        "fixture.txt".into(),
        "text/plain".into(),
        identity,
        &BTreeMap::new(),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        doc.pages[0].blocks[0].text,
        "Native UTF-8 paragraph with formula E=mc^2."
    );
    assert_eq!(doc.pages[0].coordinate_space, "unknown");
    assert!(doc.pages[0].blocks[0].region.is_none());
}
#[test]
fn truncated_completion_cannot_be_accepted_even_if_its_json_is_valid() {
    let page = schema::upgrade_page(&native().pages[0]);
    let mut output = wire(vec![block()], 1);
    output["choices"][0]["finish_reason"] = json!("length");
    assert!(layout_page(output, &page, &image(), &profile()).is_err());
}
#[tokio::test]
async fn missing_verified_actor_never_turns_model_dispatch_failure_into_native_success() {
    let evidence = BTreeMap::from([(
        1,
        PageEvidence {
            measurements: Some(PageMeasurements {
                text_coverage: 0.,
                image_ratio: 1.,
                table_density: 0.,
            }),
            image: Some(image()),
        },
    )]);
    assert!(
        upgrade(&native(), source(), &evidence, Some(&profile()))
            .await
            .is_err()
    );
}
#[test]
fn explicit_profile_selects_real_image_without_fabricating_measurements() {
    let mut profile = profile();
    let evidence = PageEvidence {
        measurements: None,
        image: Some(image()),
    };
    assert!(!profile.route(&evidence).unwrap());
    profile.mode = LayoutMode::Explicit;
    assert!(profile.route(&evidence).unwrap());
    assert!(evidence.measurements.is_none());
    let page = schema::upgrade_page(&native().pages[0]);
    assert!(
        layout_page(
            wire(vec![block()], 1),
            &page,
            evidence.image.as_ref().unwrap(),
            &profile
        )
        .is_ok()
    );
}
#[tokio::test]
async fn explicit_strict_profile_requires_raster_and_never_uses_anonymous_dispatch() {
    let mut profile = profile();
    profile.mode = LayoutMode::Explicit;
    profile.allow_known_failure_native_fallback = false;
    assert!(
        upgrade(&native(), source(), &BTreeMap::new(), Some(&profile))
            .await
            .is_err()
    );
    let evidence = BTreeMap::from([(
        1,
        PageEvidence {
            measurements: None,
            image: Some(image()),
        },
    )]);
    assert!(
        upgrade(&native(), source(), &evidence, Some(&profile))
            .await
            .is_err()
    );
}
