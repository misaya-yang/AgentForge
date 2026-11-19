//! Optional binding to the exact SDK action the caller reviewed.
use serde_json::Value;

pub(super) struct ExpectedBinding<'a> {
    pub run_id: &'a str,
    pub tool_call_id: &'a str,
    pub arguments_hash: &'a str,
}

impl<'a> ExpectedBinding<'a> {
    pub fn parse(payload: &'a Value) -> Result<Option<Self>, &'static str> {
        let keys = ["expected_run_id", "expected_tool_call_id", "expected_arguments_hash"];
        if keys.iter().all(|key| payload.get(*key).is_none()) {
            return Ok(None);
        }
        let field = |key: &str, max: usize| {
            payload.get(key).and_then(Value::as_str)
                .filter(|value| !value.is_empty() && value.len() <= max)
                .ok_or("INVALID_APPROVAL_BINDING")
        };
        let binding = Self {
            run_id: field(keys[0], 36)?,
            tool_call_id: field(keys[1], 160)?,
            arguments_hash: field(keys[2], 64)?,
        };
        if binding.arguments_hash.len() != 64
            || !binding.arguments_hash.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err("INVALID_APPROVAL_BINDING");
        }
        Ok(Some(binding))
    }

    pub fn matches(&self, approval: &Value) -> bool {
        approval["run_id"].as_str() == Some(self.run_id)
            && approval["tool_call_id"].as_str() == Some(self.tool_call_id)
            && approval["arguments_hash"].as_str().map(str::trim) == Some(self.arguments_hash)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn payload() -> Value {
        json!({"expected_run_id":"00000000-0000-0000-0000-000000000001",
            "expected_tool_call_id":"call_1","expected_arguments_hash":"a".repeat(64)})
    }
    #[test]
    fn legacy_request_remains_supported() {
        assert!(ExpectedBinding::parse(&json!({"approved":true})).unwrap().is_none());
    }
    #[test]
    fn partial_null_and_invalid_hash_are_rejected() {
        for key in ["expected_run_id", "expected_tool_call_id", "expected_arguments_hash"] {
            let mut p = payload(); p.as_object_mut().unwrap().remove(key);
            assert!(ExpectedBinding::parse(&p).is_err());
            let mut p = payload(); p[key] = Value::Null;
            assert!(ExpectedBinding::parse(&p).is_err());
        }
        for hash in ["a".repeat(63), "G".repeat(64), "A".repeat(64)] {
            let mut p = payload(); p["expected_arguments_hash"] = json!(hash);
            assert!(ExpectedBinding::parse(&p).is_err());
        }
    }
    #[test]
    fn every_reviewed_identity_field_must_match() {
        let p = payload(); let binding = ExpectedBinding::parse(&p).unwrap().unwrap();
        let row = json!({"run_id":binding.run_id,"tool_call_id":binding.tool_call_id,
            "arguments_hash":binding.arguments_hash});
        assert!(binding.matches(&row));
        for key in ["run_id", "tool_call_id", "arguments_hash"] {
            let mut mismatch = row.clone(); mismatch[key] = json!("different");
            assert!(!binding.matches(&mismatch));
        }
    }
}
