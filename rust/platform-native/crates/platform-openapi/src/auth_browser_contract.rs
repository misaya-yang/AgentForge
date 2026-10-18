//! Shared pure authentication schemas, without constructing authentication I/O.
#[path = "../../platform-auth/src/browser_contract.rs"]
mod browser_contract;
#[allow(dead_code)]
#[path = "../../platform-auth/src/contract.rs"]
mod contract;
#[path = "../../platform-auth/src/device_contract.rs"]
mod device_contract;
#[path = "../../platform-auth/src/enterprise_contract.rs"]
mod enterprise_contract;

pub(super) fn extend_document(document: &mut serde_json::Value) {
    browser_contract::extend_document(document);
    enterprise_contract::extend(document);
    device_contract::extend(document);
    use sha2::{Digest, Sha256};
    for (name, source) in [
        (
            "../../platform-auth/src/device_contract.rs",
            include_bytes!("../../platform-auth/src/device_contract.rs").as_slice(),
        ),
        (
            "../../platform-auth/src/device_flow.rs",
            include_bytes!("../../platform-auth/src/device_flow.rs").as_slice(),
        ),
        (
            "../../platform-auth/src/browser_contract.rs",
            include_bytes!("../../platform-auth/src/browser_contract.rs").as_slice(),
        ),
        (
            "../../platform-auth/src/browser.rs",
            include_bytes!("../../platform-auth/src/browser.rs").as_slice(),
        ),
        (
            "../../platform-auth/src/enterprise_contract.rs",
            include_bytes!("../../platform-auth/src/enterprise_contract.rs").as_slice(),
        ),
        (
            "../../platform-auth/src/enterprise/http.rs",
            include_bytes!("../../platform-auth/src/enterprise/http.rs").as_slice(),
        ),
    ] {
        document["x-native-contract"]["common_source_sha256"][name] =
            serde_json::json!(format!("{:x}", Sha256::digest(source)));
    }
}
