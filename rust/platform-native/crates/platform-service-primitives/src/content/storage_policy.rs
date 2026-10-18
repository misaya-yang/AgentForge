//! Shared storage selection is explicit and fails closed before any I/O.
use super::ContentError;
pub fn backend<'a>(mode: &str, configured: Option<&'a str>) -> Result<&'a str, ContentError> {
    let shared = match mode {
        "single-node" | "single-instance" => false,
        "multi-instance" => true,
        _ => return Err(ContentError::new(400, "Unknown deployment mode")),
    };
    let backend = match configured {
        None | Some("auto") => {
            if shared {
                "s3"
            } else {
                "local"
            }
        }
        Some(value) => value,
    };
    if !matches!(backend, "local" | "s3" | "oss") || (shared && backend != "s3") {
        return Err(ContentError::new(
            400,
            "Multi-instance deployment requires S3 shared storage",
        ));
    }
    Ok(backend)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_and_invalid_backends_fail_closed() {
        assert_eq!(backend("single-node", None).unwrap(), "local");
        assert_eq!(backend("multi-instance", None).unwrap(), "s3");
        assert_eq!(backend("multi-instance", Some("auto")).unwrap(), "s3");
        assert_eq!(backend("single-instance", Some("auto")).unwrap(), "local");
        for value in ["local", "oss", "s33", ""] {
            assert!(backend("multi-instance", Some(value)).is_err());
        }
        assert!(backend("single-node", Some("s33")).is_err());
        assert!(backend("multi-instances", None).is_err());
    }
}
