//! Private persistence metadata. The upstream/public RolloutItem wire is unchanged.
use codex_rollout::RolloutItem;
use serde::de::Error as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const METADATA_KEY: &str = "ai_platform_history";
pub const FORMAT: &str = "ai-platform/rollout-writer/v1";

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WriterIdentity {
    pub format: String,
    pub upstream_revision: String,
    pub schema_sha256: String,
}

pub fn valid_digest(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl WriterIdentity {
    pub fn validate(&self) -> serde_json::Result<()> {
        if self.format != FORMAT
            || !valid_digest(&self.upstream_revision, 40)
            || !valid_digest(&self.schema_sha256, 64)
        {
            return Err(serde_json::Error::custom(
                "invalid persisted rollout writer identity",
            ));
        }
        Ok(())
    }
}

/// Compiled source identity, never an actor-controlled environment value.
/// Release gate compares it with the admitted source lock before promotion.
pub fn writer_identity() -> serde_json::Result<WriterIdentity> {
    let writer: WriterIdentity =
        serde_json::from_str(include_str!("history_writer_identity.json"))?;
    writer.validate()?;
    Ok(writer)
}

pub fn persisted_writer(value: &Value) -> serde_json::Result<Option<WriterIdentity>> {
    value
        .get(METADATA_KEY)
        .map(|metadata| {
            let writer: WriterIdentity = serde_json::from_value(metadata.clone())?;
            writer.validate()?;
            Ok(writer)
        })
        .transpose()
}

pub fn encode_rollout(item: &RolloutItem) -> serde_json::Result<Value> {
    let mut value = serde_json::to_value(item)?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| serde_json::Error::custom("rollout item must be an object"))?;
    if object.contains_key(METADATA_KEY) {
        return Err(serde_json::Error::custom(
            "upstream rollout metadata key collision",
        ));
    }
    object.insert(
        METADATA_KEY.to_owned(),
        serde_json::to_value(writer_identity()?)?,
    );
    Ok(value)
}

/// Legacy rows stay readable; a present malformed/future metadata envelope is
/// never silently treated as legacy. Unknown native variants fail upstream decode.
pub fn decode_rollout(mut value: Value) -> serde_json::Result<RolloutItem> {
    persisted_writer(&value)?;
    if let Some(object) = value.as_object_mut() {
        object.remove(METADATA_KEY);
    }
    serde_json::from_value(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn legacy() -> Value {
        serde_json::from_str(include_str!(
            "../../tests/history-fixtures/upstream-legacy-response-line.json"
        ))
        .unwrap()
    }

    #[test]
    fn existing_upstream_legacy_record_decodes_and_new_stamp_preserves_native_wire() {
        let original = legacy();
        assert!(persisted_writer(&original).unwrap().is_none());
        let native = decode_rollout(original).unwrap();
        let stamped = encode_rollout(&native).unwrap();
        assert_eq!(
            persisted_writer(&stamped).unwrap(),
            Some(writer_identity().unwrap())
        );
        // Existing upstream decoder ignores the extra top-level private key.
        // This is current-decoder compatibility, not proof about a frozen N-1 binary.
        let direct: RolloutItem = serde_json::from_value(stamped.clone()).unwrap();
        assert_eq!(
            serde_json::to_value(direct).unwrap(),
            serde_json::to_value(&native).unwrap()
        );
        assert_eq!(
            serde_json::to_value(decode_rollout(stamped).unwrap()).unwrap(),
            serde_json::to_value(native).unwrap()
        );
    }

    #[test]
    fn legacy_fixture_mutations_reject_unknown_native_variant_and_bad_payload() {
        let mut value = legacy();
        value["type"] = serde_json::json!("future_unsupported_native_variant");
        assert!(decode_rollout(value).is_err());
        value = legacy();
        value["payload"] = serde_json::json!({"type":"message","content":"invalid"});
        assert!(decode_rollout(value).is_err());
    }

    #[test]
    fn present_bad_or_unknown_writer_metadata_is_not_legacy() {
        let mut value = legacy();
        for metadata in [
            Value::Null,
            serde_json::json!({}),
            serde_json::json!({"format":"future","upstream_revision":"a".repeat(40),"schema_sha256":"b".repeat(64)}),
            serde_json::json!({"format":FORMAT,"upstream_revision":"a".repeat(39),"schema_sha256":"b".repeat(64)}),
            serde_json::json!({"format":FORMAT,"upstream_revision":"a".repeat(40),"schema_sha256":"G".repeat(64)}),
        ] {
            value[METADATA_KEY] = metadata;
            assert!(decode_rollout(value.clone()).is_err());
        }
    }
}
