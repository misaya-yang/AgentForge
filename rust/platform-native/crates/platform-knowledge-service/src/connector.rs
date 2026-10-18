//! Connector delta admission shared by source adapters. Transport verifies the
//! Gateway actor first; external event bodies never grant dataset permissions.
use serde::Deserialize;
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeltaEvent {
    pub request_id: uuid::Uuid,
    pub connector: String,
    pub source_id: String,
}
impl DeltaEvent {
    pub fn validate(&self, connector: &str) -> bool {
        self.connector == connector
            && !self.source_id.is_empty()
            && self.source_id.len() <= 255
            && self.source_id.bytes().all(|b| b.is_ascii_digit())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn delta_identity_is_bounded_and_adapter_specific() {
        let e = DeltaEvent {
            request_id: uuid::Uuid::new_v4(),
            connector: "confluence".into(),
            source_id: "123".into(),
        };
        assert!(e.validate("confluence"));
        assert!(!e.validate("sharepoint"));
        let mut e = e;
        e.source_id = "../../other".into();
        assert!(!e.validate("confluence"));
    }
}
