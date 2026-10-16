use crate::*;
use std::{collections::BTreeSet, io::Read};
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains(['\\', ':', '\0'])
        && !name.starts_with('/')
        && name.split('/').all(|s| !matches!(s, "" | "." | ".."))
}
pub fn verify(args: &Args) -> Result<Value> {
    require(
        args.positionals
            .get(1)
            .map(String::as_str)
            .unwrap_or("verify")
            == "verify",
        "unknown recovery-archive operation",
    )?;
    let path = Path::new(args.get("--archive")?);
    require(
        std::fs::metadata(path)?.len() <= 122 * 1024 * 1024,
        "archive exceeds approved 122 MiB ceiling",
    )?;
    let mut archive = zip::ZipArchive::new(std::fs::File::open(path)?)?;
    let mut names = BTreeSet::new();
    let mut total = 0u64;
    for i in 0..archive.len() {
        let file = archive.by_index(i)?;
        let name = file.name().to_owned();
        require(
            valid_name(&name) && names.insert(name),
            "unsafe/duplicated archive member",
        )?;
        require(
            !file.is_dir() && !file.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000),
            "ZIP directory/symlink forbidden",
        )?;
        require(
            file.size() <= 256 * 1024 * 1024,
            "archive member exceeds 256 MiB bound",
        )?;
        total = total
            .checked_add(file.size())
            .ok_or("archive length overflow")?;
        require(
            total <= 1024 * 1024 * 1024,
            "expanded archive exceeds 1 GiB bound",
        )?;
    }
    let manifest: Value = {
        let file = archive.by_name("RECOVERY_MANIFEST.json")?;
        let mut bytes = vec![];
        file.take(32 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
        require(bytes.len() <= 32 * 1024 * 1024, "manifest exceeds bound")?;
        serde_json::from_slice(&bytes)?
    };
    let files = manifest["files"]
        .as_object()
        .ok_or("manifest file inventory missing")?;
    let expected: BTreeSet<_> = files
        .keys()
        .cloned()
        .chain(["RECOVERY_MANIFEST.json".to_owned()])
        .collect();
    require(expected == names, "manifest/member inventory mismatch")?;
    if manifest["schema_version"] == "ai-platform/recovery-manifest/v2" {
        let main: serde_json::Map<String, Value> = files
            .iter()
            .filter(|(name, _)| name.starts_with("AI--Platfform/"))
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        let actual = format!(
            "{:x}",
            Sha256::digest(canonical_sorted(&Value::Object(main.clone())).as_bytes())
        );
        require(
            manifest["source_identity"]["kind"] == "sha256_file_inventory"
                && manifest["source_identity"]["main_file_count"].as_u64()
                    == Some(main.len() as u64)
                && manifest["source_identity"]["main_file_inventory_sha256"] == actual,
            "source file inventory identity mismatch",
        )?;
    }
    for (name, record) in files {
        if args.flag("--require-zero-python") && name.starts_with("AI--Platfform/") {
            let lower = name.to_ascii_lowercase();
            require(
                ![".py", ".pyw", ".pyi", ".pyc", ".pyo"]
                    .iter()
                    .any(|suffix| lower.ends_with(suffix)),
                "Python source remains in project recovery",
            )?;
        }
        let mut file = archive.by_name(name)?;
        require(
            record["size"].as_u64() == Some(file.size()),
            "archive size mismatch",
        )?;
        let mut hash = Sha256::new();
        let mut bytes = [0; 65536];
        loop {
            let n = file.read(&mut bytes)?;
            if n == 0 {
                break;
            }
            hash.update(&bytes[..n]);
        }
        require(
            record["sha256"] == format!("{:x}", hash.finalize()),
            format!("archive digest mismatch: {name}"),
        )?;
    }
    // zip's streaming reader verifies CRC while consuming every member above.
    Ok(
        json!({"result":"pass","archive_sha256":digest(path)?,"file_count":files.len(),"uncompressed_bytes":total,"source_identity":manifest["source_identity"],"candidate_is_release":manifest["candidate_is_release"],"extracted":false}),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn member_names_are_safe() {
        assert!(valid_name("AI--Platfform/src/main.rs"));
        for n in ["../x", "/abs", "a//b", "a/./b", "x\\y", "C:bad", ""] {
            assert!(!valid_name(n));
        }
    }
    #[test]
    fn verifies_bytes_and_rejects_tampered_manifest_and_python() {
        use std::io::Write;
        let path =
            std::env::temp_dir().join(format!("platform-archive-test-{}.zip", std::process::id()));
        let name = "AI--Platfform/src/main.rs";
        let make = |name: &str, sha: &str| {
            let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
            let options = zip::write::SimpleFileOptions::default();
            zip.start_file(name, options).unwrap();
            zip.write_all(b"fixture").unwrap();
            zip.start_file("RECOVERY_MANIFEST.json", options).unwrap();
            zip.write_all(
                serde_json::to_string(&json!({"files":{name:{"size":7,"sha256":sha}}}))
                    .unwrap()
                    .as_bytes(),
            )
            .unwrap();
            zip.finish().unwrap();
        };
        let mut args = Args::default();
        args.values
            .insert("--archive".into(), path.to_string_lossy().into_owned());
        make(name, &hash(b"fixture"));
        assert_eq!(verify(&args).unwrap()["file_count"], 1);
        make(name, &hash(b"different"));
        assert!(verify(&args).is_err());
        make("AI--Platfform/old.py", &hash(b"fixture"));
        args.flags.push("--require-zero-python".into());
        assert!(verify(&args).is_err());
        make("AI--Platfform/old.PYW", &hash(b"fixture"));
        assert!(verify(&args).is_err());
        std::fs::remove_file(path).unwrap();
    }
}
