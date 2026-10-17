//! Run unchanged before and after the serde_yaml -> serde_yaml_ng package alias switch.
//! This checks behavior/API; dependency-policy separately proves which crate resolved.
use serde::{Deserialize, Serialize};
use serde_json::json;
#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct Document {
    label: String,
    enabled: bool,
    values: Vec<i64>,
}
#[test]
fn yaml12_scalar_and_github_on_key_semantics_do_not_silently_switch_to_yaml11() {
    let value:serde_json::Value=serde_yaml::from_str("on: [push]\nflag: on\nlegacy_yes: yes\nleading_zero: 0123\nquoted: 'true'\ntruth: true\nscientific: 1e3\n").unwrap();
    assert_eq!(value["on"], json!(["push"]));
    assert_eq!(value["flag"], "on");
    assert_eq!(value["legacy_yes"], "yes");
    assert_eq!(value["leading_zero"], "0123");
    assert_eq!(value["quoted"], "true");
    assert_eq!(value["truth"], true);
    assert_eq!(value["scientific"], 1000.0);
}
#[test]
fn reader_slice_value_writer_and_mapping_apis_remain_compatible() {
    let input = "label: '中文: ✅'\nenabled: true\nvalues: [1, 2]\n";
    let a: Document = serde_yaml::from_reader(input.as_bytes()).unwrap();
    let b: Document = serde_yaml::from_slice(input.as_bytes()).unwrap();
    assert_eq!(a, b);
    let value = serde_yaml::to_value(&a).unwrap();
    let c: Document = serde_yaml::from_value(value).unwrap();
    assert_eq!(a, c);
    let mut bytes = vec![];
    serde_yaml::to_writer(&mut bytes, &a).unwrap();
    assert_eq!(serde_yaml::from_slice::<Document>(&bytes).unwrap(), a);
    let mut mapping = serde_yaml::Mapping::new();
    mapping.insert(
        serde_yaml::Value::String("name".into()),
        serde_yaml::Value::String("工具".into()),
    );
    assert_eq!(mapping.len(), 1);
}
#[test]
fn explicit_merge_preserves_override_precedence_and_tags_round_trip() {
    let mut value: serde_yaml::Value = serde_yaml::from_str(
        "defaults: &defaults {api: v1, enabled: true}\nselected:\n  <<: *defaults\n  api: v2\n",
    )
    .unwrap();
    value.apply_merge().unwrap();
    assert_eq!(value["selected"]["api"].as_str(), Some("v2"));
    assert_eq!(value["selected"]["enabled"].as_bool(), Some(true));
    let tagged: serde_yaml::Value = serde_yaml::from_str("!Mode {enabled: true}").unwrap();
    assert!(matches!(tagged, serde_yaml::Value::Tagged(_)));
    let serialized = serde_yaml::to_string(&tagged).unwrap();
    assert_eq!(
        serde_yaml::from_str::<serde_yaml::Value>(&serialized).unwrap(),
        tagged
    );
}
#[test]
fn duplicate_fields_bad_types_recursive_aliases_and_complex_json_keys_are_rejected() {
    assert!(
        serde_yaml::from_str::<Document>("label: x\nlabel: y\nenabled: true\nvalues: []").is_err()
    );
    assert!(serde_yaml::from_str::<Document>("label: x\nenabled: on\nvalues: []").is_err());
    assert!(serde_yaml::from_str::<serde_yaml::Value>("&self [*self]").is_err());
    assert!(serde_yaml::from_str::<serde_json::Value>("? [a, b]\n: value").is_err());
    let error = serde_yaml::from_str::<Document>("label: x\nenabled: [").unwrap_err();
    let location = error.location().unwrap();
    assert!(location.line() >= 2);
    assert!(location.column() > 0);
}
#[test]
fn stream_deserializer_is_available_but_single_document_api_rejects_extra_documents() {
    let source = "---\nname: first\n---\nname: second\n";
    let values: Vec<serde_json::Value> = serde_yaml::Deserializer::from_str(source)
        .map(|doc| serde_json::Value::deserialize(doc).unwrap())
        .collect();
    assert_eq!(values.len(), 2);
    assert_eq!(values[1]["name"], "second");
    assert!(serde_yaml::from_str::<serde_json::Value>(source).is_err());
}
