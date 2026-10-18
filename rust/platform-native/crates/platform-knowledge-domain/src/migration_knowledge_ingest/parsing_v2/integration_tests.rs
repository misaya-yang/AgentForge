use super::*;
#[test]
fn explicit_config_is_versioned_strict_and_changes_fingerprint_with_model_revision() {
    assert!(Config::resolve(&json!({})).unwrap().is_none());
    for value in [
        json!({"version":1,"profile_id":"p","atomic_chunks":true}),
        json!({"version":2,"profile_id":"p","atomic_chunks":false}),
        json!({"version":2,"profile_id":"p","atomic_chunks":true,"api_key":"forbidden"}),
    ] {
        assert!(Config::resolve(&json!({"parsing_v2":value})).is_err());
    }
    let a = Config::resolve(
        &json!({"parsing_v2":{"version":2,"profile_id":"native-pinned-v2","atomic_chunks":true}}),
    )
    .unwrap()
    .unwrap();
    let mut b = a.clone();
    b.profile_id = "different-promotion".into();
    assert_ne!(a.fingerprint().unwrap(), b.fingerprint().unwrap());
}
#[test]
fn publication_generation_requires_real_negative_fence_and_rejects_overflow() {
    assert_eq!(final_revision(-1_000_000_007).unwrap(), 1_000_000_008);
    for revision in [0, 1, i64::MIN, -i64::MAX] {
        assert!(final_revision(revision).is_err());
    }
}
#[test]
fn actor_proof_requires_exact_persisted_initiator_and_tenant() {
    let actor: UserContext = serde_json::from_value(
        json!({"tenant_id":"t","user_id":"real-user","roles":["editor"],"is_authenticated":true}),
    )
    .unwrap();
    assert!(actor_matches(&actor, "t", "real-user").is_ok());
    assert!(actor_matches(&actor, "other", "real-user").is_err());
    assert!(actor_matches(&actor, "t", "admin").is_err());
}
#[test]
fn streaming_adapter_keeps_true_page_ids_and_never_manufactures_geometry() {
    let pages = vec![
        ingest::streaming::PageContent::new(7, "page seven".into(), vec![]),
        ingest::streaming::PageContent::new(8, "page eight".into(), vec![]),
    ];
    let ir = streaming_ir("doc", &"a".repeat(64), "f.pdf", "application/pdf", pages).unwrap();
    assert_eq!(ir.pages[0].page_number, 7);
    assert_eq!(ir.pages[1].page_number, 8);
    assert!(ir.pages[0].page_width.is_none());
    assert!(ir.pages[0].blocks[0].bbox.is_none());
    assert_eq!(ir.pages[1].blocks[0].text, "page eight");
}
fn prepared() -> Prepared {
    let native:DocIR=serde_json::from_value(json!({"doc_id":"doc","content_hash":"a".repeat(64),"filename":"f.pdf","mime":"application/pdf","pages":[]})).unwrap();
    let blocks = vec![
        BlockV2 {
            id: "table".into(),
            reading_order: 0,
            text: "table body".into(),
            region: None,
            structure: Structure::Table {
                html: Some("<table><tr><td>large cell</td></tr></table>".into()),
                markdown: format!("|{}|", "large-cell-content".repeat(32)),
                rows: 1,
                columns: 1,
                continuation: None,
            },
            provenance: json!({}),
        },
        BlockV2 {
            id: "formula".into(),
            reading_order: 1,
            text: "formula".into(),
            region: None,
            structure: Structure::Formula {
                latex: "x+y+z".repeat(32),
                display: true,
            },
            provenance: json!({}),
        },
    ];
    Prepared {
        scope: ProcessingScope {
            tenant_id: "t".into(),
            dataset_id: "ds".into(),
            document_id: "doc".into(),
            execution_id: Some("persisted-execution".into()),
            parser_profile: "profile-hash".into(),
        },
        pages: vec![PageV2 {
            page_number: 2,
            width: None,
            height: None,
            coordinate_space: "unknown".into(),
            blocks,
            route: json!({"selected":"native"}),
        }],
        native,
        actor: serde_json::from_value(
            json!({"tenant_id":"t","user_id":"u","is_authenticated":true}),
        )
        .unwrap(),
        expected_content: "source".into(),
        pending_prefix: prefix("t", "doc", "persisted-execution"),
    }
}
#[test]
fn selected_profile_really_chunks_tables_and_formulas_atomically_with_bound_references() {
    let prepared = prepared();
    let chunker = ingest::chunking::Chunker::new().unwrap();
    let config=ChunkingConfig::from_value(&json!({"mode":"fixed_size","chunk_size":50,"chunk_overlap":0,"min_chunk_size":50,"max_chunk_size":50,"use_token_count":false})).unwrap();
    let table = format!("|{}|", "large-cell-content".repeat(32));
    let formula = format!("$$\n{}\n$$", "x+y+z".repeat(32));
    // Both real fixture bodies exceed a legal normal chunk boundary. The
    // baseline splitter would divide them, while the v2 structure stays whole.
    for text in [&table, &formula] {
        assert!(
            chunker
                .standard_chunks(text, &config, "ds", "doc", "f.pdf", "upload")
                .unwrap()
                .len()
                > 1
        );
    }
    let chunks = prepared
        .chunks(&chunker, &config, "f.pdf", "upload")
        .unwrap();
    assert_eq!(chunks.len(), 2);
    assert!(chunks[0].text.len() > 50);
    assert!(chunks[1].text.len() > 50);
    assert_eq!(chunks[0].text, table);
    assert_eq!(chunks[1].text, formula);
    assert_eq!(
        chunks[0].source_reference["execution_id"],
        "persisted-execution"
    );
    assert_eq!(chunks[1].page_number, Some(2));
    assert_eq!(
        chunks[0].source_reference["schema"],
        "docir-publication-ref/v2"
    );
    assert_eq!(chunks[0].metadata["structure"]["kind"], "table");
    assert_eq!(chunks[1].metadata["structure"]["kind"], "formula");
    assert_ne!(chunks[0].segment_id, chunks[1].segment_id);
}
#[test]
fn committed_chunk_citation_uses_real_version_and_generation_while_ref_stays_stable() {
    let prepared = prepared();
    let chunker = ingest::chunking::Chunker::new().unwrap();
    let config = ChunkingConfig::from_value(&json!({})).unwrap();
    let mut chunks = prepared
        .chunks(&chunker, &config, "title", "upload")
        .unwrap();
    let document = DocV2 {
        schema: "docir/v2".into(),
        filename: "f.pdf".into(),
        mime: Some("application/pdf".into()),
        source: SourceIdentity {
            tenant_id: "t".into(),
            dataset_id: "ds".into(),
            document_id: "doc".into(),
            document_version: "persisted-version-id".into(),
            publication_generation: "1000000008".into(),
            source_sha256: "a".repeat(64),
            parser_profile: "profile-hash".into(),
            execution_id: Some("persisted-execution".into()),
        },
        pages: prepared.pages.clone(),
        native_metadata: json!({}),
    };
    let publication = Published {
        patch: json!({}),
        document,
    };
    publication.stamp_chunks(&mut chunks).unwrap();
    assert_eq!(
        chunks[0].metadata["citation"]["source"]["document_version"],
        "persisted-version-id"
    );
    assert_eq!(
        chunks[1].metadata["citation"]["source"]["publication_generation"],
        "1000000008"
    );
    assert_eq!(
        chunks[0].source_reference["execution_id"],
        "persisted-execution"
    );
}
#[test]
fn explicit_layout_configuration_is_reachable_without_threshold_measurements() {
    let mut value = json!({"parsing_v2":{"version":2,"profile_id":"explicit-candidate","atomic_chunks":true,"layout":{"mode":"explicit","provider":"registered","model":"layout","revision":"immutable-v1","max_output_tokens":8192,"deadline_seconds":60,"allow_known_failure_native_fallback":false}}});
    let config = Config::resolve(&value).unwrap().unwrap();
    let profile = config.layout_profile().unwrap().unwrap();
    assert_eq!(profile.mode, LayoutMode::Explicit);
    assert!(
        profile
            .route(&PageEvidence {
                measurements: None,
                image: None
            })
            .unwrap()
    );
    value["parsing_v2"]["layout"]["mode"] = json!("thresholds");
    assert!(Config::resolve(&value).is_err());
    value["parsing_v2"]["layout"]["mode"] = json!("automatic-made-up");
    assert!(Config::resolve(&value).is_err());
}

#[test]
fn caption_only_figure_keeps_its_crop_section_and_contextual_title() {
    let mut prepared = prepared();
    let mut figure = prepared.pages[0].blocks[0].clone();
    figure.id = "caption-only-figure".into();
    figure.text.clear();
    figure.structure = Structure::Figure {
        caption: "Architecture diagram".into(),
        crop: Some("region/crop.png".into()),
    };
    prepared.pages[0].blocks = vec![figure];
    prepared.pages.truncate(1);
    let chunker = ingest::chunking::Chunker::new().unwrap();
    let config = ChunkingConfig::from_value(&json!({})).unwrap();
    let chunks = prepared
        .chunks(&chunker, &config, "Manual", "upload")
        .unwrap();
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].text, "Architecture diagram");
    assert_eq!(chunks[0].metadata["document_title"], "Manual");
    assert_eq!(chunks[0].metadata["caption"], "Architecture diagram");
    assert_eq!(chunks[0].metadata["region_crop"], "region/crop.png");
    assert_eq!(
        chunks[0].source_reference["block_id"],
        "caption-only-figure"
    );
}
