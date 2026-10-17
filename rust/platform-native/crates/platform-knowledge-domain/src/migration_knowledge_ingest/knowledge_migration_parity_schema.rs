//! Exact pure assertions from test_chunking_config_schema_roundtrip.py.
//! Calls the same validators as the mounted HTTP extraction layer.
use super::*;
#[test]
fn runtime_chunking_fields_round_trip_without_loss() {
    let raw = json!({"mode":"hierarchical","chunk_size":2000,"chunk_overlap":200,"max_chunk_size":4000,"min_chunk_size":200,"normalize_whitespace":false,"strip_html":true,"extract_metadata":true,"metadata_fields":["title","author","date"],"page_marker":r"\f","strict_section_traceability":true,"preserve_images":true,"image_context_chars":512,"parent_chunk_size":4000,"parent_overlap":200,"child_chunk_size":500,"child_chunk_overlap":50,"segmentation":{"max_tokens":768}});
    let encoded = dataset_config(json!({"chunking_config":raw})).unwrap();
    let decoded = dataset_config(encoded).unwrap();
    for (key, value) in raw.as_object().unwrap() {
        assert_eq!(&decoded["chunking_config"][key], value, "{key}");
    }
}
#[test]
fn runtime_chunking_fields_remain_fail_closed() {
    for (key, value) in [
        ("page_marker", json!("(a+)+$")),
        ("metadata_fields", json!([""])),
        ("separators", json!([""])),
        ("segmentation", json!({"max_tokens":500,"separator":r"\n"})),
    ] {
        let mut raw = json!({});
        raw[key] = value;
        assert!(
            dataset_config(json!({"chunking_config":raw})).is_err(),
            "{key}"
        );
    }
}
#[test]
fn archive_reason_matches_database_column_width() {
    let accepted = legacy_body(
        DOCUMENT_ARCHIVE,
        json!({"archived":true,"reason":"x".repeat(255)}),
    )
    .unwrap();
    assert_eq!(accepted["reason"], "x".repeat(255));
    assert!(
        legacy_body(
            DOCUMENT_ARCHIVE,
            json!({"archived":true,"reason":"x".repeat(256)})
        )
        .is_err()
    );
}
