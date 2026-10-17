//! Source-only Valkey topology/config gate. Does not resolve private Compose env.
use serde_json::Value;
use std::{collections::BTreeMap, fs, path::Path};
fn ensure(ok: bool, message: &str) -> Result<(), String> {
    if ok {
        Ok(())
    } else {
        Err(message.into())
    }
}
fn config(text: &str, coord: bool) -> Result<(), String> {
    let mut values = BTreeMap::new();
    for line in text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
    {
        let (key, value) = line
            .split_once(char::is_whitespace)
            .ok_or("invalid Valkey directive")?;
        ensure(
            values.insert(key, value.trim()).is_none(),
            "duplicate Valkey directive",
        )?;
    }
    for (key, expected) in [
        ("port", "0"),
        ("tls-port", "6380"),
        ("tls-protocols", "\"TLSv1.2 TLSv1.3\""),
        ("tls-cert-file", "/run/valkey/tls.crt"),
        ("tls-key-file", "/run/valkey/tls.key"),
        ("tls-ca-cert-file", "/run/valkey/ca.pem"),
        ("protected-mode", "yes"),
        ("tls-auth-clients", "no"),
        ("aclfile", "/run/valkey/users.acl"),
        (
            "maxmemory-policy",
            if coord { "noeviction" } else { "allkeys-lru" },
        ),
        ("appendonly", if coord { "yes" } else { "no" }),
        ("save", "\"\""),
        ("maxmemory", if coord { "256mb" } else { "128mb" }),
    ] {
        ensure(
            values.get(key) == Some(&expected),
            "Valkey TLS, ACL, memory or durability policy mismatch",
        )?;
    }
    if coord {
        for (key, expected) in [
            ("appendfsync", "everysec"),
            ("no-appendfsync-on-rewrite", "no"),
            ("aof-load-truncated", "no"),
        ] {
            ensure(
                values.get(key) == Some(&expected),
                "coord AOF must fail closed",
            )?;
        }
    }
    ensure(
        !text.contains("requirepass")
            && !text.contains("tls-replication yes")
            && !text.contains("tls-auth-clients optional"),
        "shared password or unapproved transport",
    )
}
pub fn validate_image(reference: &str) -> Result<(), String> {
    let Some((tag, digest)) = reference
        .strip_prefix("docker.io/valkey/valkey:")
        .and_then(|v| v.split_once("@sha256:"))
    else {
        return Err("verified explicit Valkey9 digest reference required".into());
    };
    let parts: Vec<_> = tag.split('.').collect();
    ensure(
        parts.len() == 3
            && parts[0] == "9"
            && parts
                .iter()
                .all(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            && digest.len() == 64
            && digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "verified explicit Valkey9 digest reference required",
    )
}
pub fn validate_identity(uid: &str, gid: &str) -> Result<(), String> {
    ensure(
        [uid, gid]
            .iter()
            .all(|v| v.parse::<u32>().is_ok_and(|n| n > 0)),
        "verified nonroot Valkey UID/GID required",
    )
}
fn overlay(doc: &Value) -> Result<(), String> {
    let services = doc["services"].as_object().ok_or("services required")?;
    let expected = [
        "valkey-coord",
        "valkey-cache",
        "gateway",
        "knowledge-service",
        "knowledge-worker",
    ];
    ensure(
        services.len() == expected.len() && expected.iter().all(|k| services.contains_key(*k)),
        "Valkey overlay must not mutate existing infra or unrelated services",
    )?;
    for kind in ["coord", "cache"] {
        let unit = &services[&format!("valkey-{kind}")];
        ensure(
            unit["image"]
                == "${VALKEY9_IMAGE:?Select the verified Valkey 9 image with an exact digest}",
            "Valkey admitted image selection required",
        )?;
        ensure(
            unit["ports"].is_null()
                && unit["environment"].is_null()
                && unit["privileged"].is_null()
                && unit["read_only"] == true
                && unit["cap_drop"] == serde_json::json!(["ALL"])
                && unit["security_opt"] == serde_json::json!(["no-new-privileges:true"]),
            "Valkey private nonprivileged transport required",
        )?;
        ensure(
            unit["user"]
                .as_str()
                .is_some_and(|s| s.contains("VALKEY_UID:?") && s.contains("VALKEY_GID:?")),
            "explicit nonroot image identity required",
        )?;
        ensure(
            unit["networks"] == serde_json::json!([format!("data-valkey-{kind}")]),
            "Valkey instance network boundary",
        )?;
        for volume in unit["volumes"].as_array().ok_or("Valkey mounts required")? {
            if volume.is_object() {
                ensure(
                    volume["read_only"] == true && volume["bind"]["create_host_path"] == false,
                    "Valkey inputs require existing read-only files",
                )?;
            } else {
                ensure(
                    kind == "coord" && volume == "valkey-coord-data:/data",
                    "existing Redis volume must remain untouched",
                )?;
            }
        }
        let health = unit["healthcheck"]["test"][1]
            .as_str()
            .ok_or("Valkey local health required")?;
        ensure(
            health.contains("--tls")
                && health.contains("--cacert")
                && health.contains("-h 127.0.0.1")
                && !health.contains("--insecure")
                && !health.contains(" -a "),
            "TLS local health must not disclose or downgrade credentials",
        )?;
        ensure(
            doc["networks"][format!("data-valkey-{kind}")]["internal"] == true,
            "Valkey network must be internal",
        )?;
    }
    ensure(
        doc["volumes"]
            .as_object()
            .is_some_and(|v| v.len() == 1 && v.contains_key("valkey-coord-data")),
        "preserve existing dev Redis volumes",
    )?;
    for (unit, identity, legacy) in [
        ("gateway", "gateway", "GATEWAY_REDIS__URL"),
        ("knowledge-service", "knowledge-api", "KNOWLEDGE_REDIS__URL"),
        (
            "knowledge-worker",
            "knowledge-worker",
            "KNOWLEDGE_REDIS__URL",
        ),
    ] {
        let v = &services[unit];
        ensure(
            v["environment"]["AI_PLATFORM_VALKEY_MODE"] == "split"
                && v["environment"][legacy] == ""
                && v["environment"]["INTERNAL_COMM_REDIS_URL"] == "",
            "split profile must remove application legacy shared Redis URLs",
        )?;
        for kind in ["COORD", "CACHE"] {
            let prefix = format!("AI_PLATFORM_VALKEY_{kind}");
            ensure(
                v["environment"][format!("{prefix}_USERNAME")]
                    == format!("{identity}-{}", kind.to_ascii_lowercase()),
                "per-service ACL identity mismatch",
            )?;
            ensure(
                v["environment"][format!("{prefix}_ENDPOINT")]
                    == format!("rediss://valkey-{}:6380/0", kind.to_ascii_lowercase()),
                "coord/cache endpoint mismatch",
            )?;
        }
    }
    Ok(())
}
pub fn check(root: &Path) -> Result<(), String> {
    for (kind, coord) in [("coord", true), ("cache", false)] {
        config(
            &fs::read_to_string(root.join(format!("deploy/compose/valkey/{kind}.conf")))
                .map_err(|e| e.to_string())?,
            coord,
        )?;
    }
    let value: Value = serde_yaml::from_str(
        &fs::read_to_string(root.join("deploy/compose/docker-compose.valkey.yml"))
            .map_err(|e| e.to_string())?,
    )
    .map_err(|_| "invalid public Valkey overlay".to_owned())?;
    overlay(&value)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn root() -> std::path::PathBuf {
        let p = Path::new(env!("CARGO_MANIFEST_DIR"));
        if p.join("deploy/compose/valkey").exists() {
            p.into()
        } else {
            p.join("../../../..")
        }
    }
    #[test]
    fn repository_valkey_source_contract() {
        check(&root()).unwrap();
    }
    #[test]
    fn eviction_and_aof_negatives() {
        let text = fs::read_to_string(root().join("deploy/compose/valkey/coord.conf")).unwrap();
        for bad in [
            text.replace("noeviction", "allkeys-lru"),
            text.replace("appendfsync everysec", "appendfsync no"),
            text.replace("port 0\n", "port 6379\n"),
            text.replace("aof-load-truncated no", "aof-load-truncated yes"),
        ] {
            assert!(config(&bad, true).is_err());
        }
    }
    #[test]
    fn acl_identity_and_exposure_negatives() {
        let doc: Value = serde_yaml::from_str(
            &fs::read_to_string(root().join("deploy/compose/docker-compose.valkey.yml")).unwrap(),
        )
        .unwrap();
        let mut v = doc.clone();
        v["services"]["knowledge-service"]["environment"]["AI_PLATFORM_VALKEY_COORD_USERNAME"] =
            "gateway-coord".into();
        assert!(overlay(&v).is_err());
        let mut v = doc.clone();
        v["services"]["valkey-coord"]["ports"] = serde_json::json!(["6380:6380"]);
        assert!(overlay(&v).is_err());
        let mut v = doc;
        v["services"]["redis"] = serde_json::json!({"volumes":[]});
        assert!(overlay(&v).is_err());
    }
    #[test]
    fn only_verified_major9_digest_shapes_admitted() {
        assert!(validate_identity("999", "999").is_ok());
        assert!(validate_identity("0", "999").is_err());
        assert!(validate_identity("999", "REPLACE").is_err());
        assert!(validate_image(&format!(
            "docker.io/valkey/valkey:9.1.2@sha256:{}",
            "a".repeat(64)
        ))
        .is_ok());
        for bad in [
            "valkey/valkey:latest",
            "docker.io/valkey/valkey:9.1.2",
            "docker.io/redis:9.1.2@sha256:bad",
        ] {
            assert!(validate_image(bad).is_err());
        }
    }
}
