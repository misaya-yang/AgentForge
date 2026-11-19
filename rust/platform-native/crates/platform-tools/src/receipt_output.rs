//! Persist the exact evaluated gate receipt before dispatch reports its outcome.
use crate::{Result, require};
use serde_json::Value;
use std::{fs, io::Write, path::Path};

pub fn write(path: &Path, receipt: &Value) -> Result<()> {
    require(!path.as_os_str().is_empty(), "receipt destination is empty")?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    if let Ok(metadata) = fs::symlink_metadata(path) {
        require(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "receipt destination must be a regular file",
        )?;
    }
    let temporary = parent.join(format!(".gate-receipt-{}", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        serde_json::to_writer_pretty(&mut file, receipt)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn persists_exact_pass_and_failure_receipts_on_repeat_runs() {
        let root = std::env::temp_dir().join(format!("gate-receipt-test-{}", uuid::Uuid::new_v4()));
        let path = root.join("nested/receipt.json");
        for result in ["pass", "blocked", "fail"] {
            let receipt = serde_json::json!({"result": result, "findings": ["original"]});
            write(&path, &receipt).unwrap();
            assert_eq!(
                serde_json::from_slice::<Value>(&fs::read(&path).unwrap()).unwrap(),
                receipt
            );
        }
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
        fs::remove_dir_all(root).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn refuses_symlink_without_touching_the_target() {
        let root = std::env::temp_dir().join(format!("gate-receipt-link-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let target = root.join("target");
        let link = root.join("link");
        fs::write(&target, "preserved").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(write(&link, &serde_json::json!({"result":"pass"})).is_err());
        assert_eq!(fs::read_to_string(target).unwrap(), "preserved");
        fs::remove_dir_all(root).unwrap();
    }
}
