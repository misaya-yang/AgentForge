use super::*;
use std::{collections::BTreeMap, io::Read};
#[derive(Clone)]
pub(super) struct Entry {
    pub relative: PathBuf,
    pub source: PathBuf,
    pub directory: bool,
    pub link: bool,
}
fn table_map(c: &Config) -> Result<BTreeMap<String, PathBuf>> {
    c.v["tablespaces"]
        .as_object()
        .ok_or("tablespace map missing")?
        .iter()
        .map(|(k, v)| {
            Ok((
                k.clone(),
                fs::canonicalize(v.as_str().ok_or("invalid tablespace")?)
                    .map_err(|_| "preserved tablespace copy unavailable")?,
            ))
        })
        .collect()
}
fn walk(
    root: &Path,
    relative: &Path,
    maps: &BTreeMap<String, PathBuf>,
    out: &mut Vec<Entry>,
) -> Result<()> {
    need(
        out.len() < 100000,
        "copy tree exceeds bounded file inventory",
    )?;
    let path = root.join(relative);
    let meta = fs::symlink_metadata(&path).map_err(|_| "copy metadata unavailable")?;
    if meta.file_type().is_symlink() {
        let parts: Vec<_> = relative.components().collect();
        need(
            parts.len() == 2 && parts[0].as_os_str() == "pg_tblspc",
            "non-tablespace symlink refused; materialize preserved copy first",
        )?;
        let oid = parts[1]
            .as_os_str()
            .to_str()
            .ok_or("invalid tablespace OID")?;
        let target = fs::canonicalize(&path).map_err(|_| "tablespace target unavailable")?;
        need(
            maps.get(oid) == Some(&target),
            "tablespace link is not the exact allowlisted preserved copy",
        )?;
        out.push(Entry {
            relative: relative.into(),
            source: target.clone(),
            directory: true,
            link: true,
        });
        let mut children: Vec<_> = fs::read_dir(&target)
            .map_err(|_| "tablespace unreadable")?
            .collect::<std::io::Result<Vec<_>>>()
            .map_err(|_| "tablespace enumeration failed")?;
        children.sort_by_key(|e| e.file_name());
        for child in children {
            let child_relative = PathBuf::from(child.file_name());
            walk_target(&target, &child_relative, relative, maps, out)?;
        }
        return Ok(());
    }
    need(
        meta.is_file() || meta.is_dir(),
        "special filesystem entries refused",
    )?;
    if !relative.as_os_str().is_empty() {
        out.push(Entry {
            relative: relative.into(),
            source: path.clone(),
            directory: meta.is_dir(),
            link: false,
        });
    }
    if meta.is_dir() {
        let mut children: Vec<_> = fs::read_dir(&path)
            .map_err(|_| "copy directory unreadable")?
            .collect::<std::io::Result<Vec<_>>>()
            .map_err(|_| "copy enumeration failed")?;
        children.sort_by_key(|e| e.file_name());
        for child in children {
            walk(root, &relative.join(child.file_name()), maps, out)?;
        }
    }
    Ok(())
}
fn walk_target(
    base: &Path,
    rel: &Path,
    virtual_root: &Path,
    _maps: &BTreeMap<String, PathBuf>,
    out: &mut Vec<Entry>,
) -> Result<()> {
    need(
        out.len() < 100000,
        "tablespace tree exceeds bounded file inventory",
    )?;
    let p = base.join(rel);
    let m = fs::symlink_metadata(&p).map_err(|_| "tablespace metadata unavailable")?;
    need(
        !m.file_type().is_symlink() && (m.is_file() || m.is_dir()),
        "nested tablespace symlink/special file refused",
    )?;
    out.push(Entry {
        relative: virtual_root.join(rel),
        source: p.clone(),
        directory: m.is_dir(),
        link: false,
    });
    if m.is_dir() {
        let mut children: Vec<_> = fs::read_dir(p)
            .map_err(|_| "tablespace enumeration failed")?
            .collect::<std::io::Result<Vec<_>>>()
            .map_err(|_| "tablespace enumeration failed")?;
        children.sort_by_key(|e| e.file_name());
        for child in children {
            walk_target(base, &rel.join(child.file_name()), virtual_root, _maps, out)?;
        }
    }
    Ok(())
}
fn entries(root: &Path, maps: &BTreeMap<String, PathBuf>) -> Result<Vec<Entry>> {
    let mut out = vec![];
    walk(root, Path::new(""), maps, &mut out)?;
    out.sort_by(|a, b| a.relative.cmp(&b.relative));
    Ok(out)
}
fn fingerprint(entries: &[Entry]) -> Result<Value> {
    let mut h = Sha256::new();
    let mut files = 0u64;
    let mut bytes = 0u64;
    for e in entries {
        let rel = e.relative.to_str().ok_or("non UTF-8 database file name")?;
        let m = fs::metadata(&e.source).map_err(|_| "copy changed during hash")?;
        let mode = {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                m.mode() & 0o777
            }
            #[cfg(not(unix))]
            {
                0u32
            }
        };
        let kind = if e.link {
            "link"
        } else if e.directory {
            "dir"
        } else {
            "file"
        };
        h.update(serde_json::to_vec(&(rel, kind, mode)).map_err(|_| "tree descriptor failed")?);
        h.update([0]);
        if !e.directory {
            files += 1;
            bytes += m.len();
            h.update(m.len().to_le_bytes());
            let mut f = fs::File::open(&e.source).map_err(|_| "copy file unreadable")?;
            let mut b = [0; 65536];
            loop {
                let n = f.read(&mut b).map_err(|_| "copy read failed")?;
                if n == 0 {
                    break;
                }
                h.update(&b[..n]);
            }
        }
        h.update([0]);
    }
    Ok(json!({"sha256":format!("{:x}",h.finalize()),"files":files,"bytes":bytes}))
}
pub(super) fn inspect(c: &Config) -> Result<Value> {
    need(
        !c.source.join("postmaster.pid").exists(),
        "preserved copy must be stopped; source is never stopped by this tool",
    )?;
    need(
        fs::read_to_string(c.source.join("PG_VERSION"))
            .map_err(|_| "source PG_VERSION unavailable")?
            .trim()
            == "16",
        "source must be PostgreSQL 16",
    )?;
    fingerprint(&entries(&c.source, &table_map(c)?)?)
}
pub(super) struct Cloned {
    pub data: PathBuf,
    pub maps: BTreeMap<String, PathBuf>,
    pub tree: Value,
}
pub(super) fn clone(c: &Config, dest: &Path, tables: &Path) -> Result<Cloned> {
    let source = entries(&c.source, &table_map(c)?)?;
    private_dir(dest)?;
    private_dir(tables)?;
    let mut maps = BTreeMap::new();
    for e in &source {
        let parts: Vec<_> = e.relative.components().collect();
        let target = if parts.len() >= 2 && parts[0].as_os_str() == "pg_tblspc" {
            let oid = parts[1]
                .as_os_str()
                .to_str()
                .ok_or("invalid tablespace OID")?;
            if e.link {
                let path = tables.join(oid);
                private_dir(&path)?;
                fs::set_permissions(
                    &path,
                    fs::metadata(&e.source)
                        .map_err(|_| "tablespace source metadata unavailable")?
                        .permissions(),
                )
                .map_err(|_| "tablespace permissions copy failed")?;
                #[cfg(unix)]
                std::os::unix::fs::symlink(&path, dest.join(&e.relative))
                    .map_err(|_| "isolated tablespace link creation failed")?;
                maps.insert(oid.into(), path);
                continue;
            }
            if let Some(base) = maps.get(oid) {
                let mut p = base.clone();
                for component in &parts[2..] {
                    p.push(component.as_os_str());
                }
                p
            } else {
                dest.join(&e.relative)
            }
        } else {
            dest.join(&e.relative)
        };
        if e.directory {
            fs::create_dir(&target).map_err(|_| "isolated directory creation failed")?;
            fs::set_permissions(
                &target,
                fs::metadata(&e.source)
                    .map_err(|_| "source metadata failed")?
                    .permissions(),
            )
            .map_err(|_| "directory permission copy failed")?;
        } else {
            fs::copy(&e.source, &target).map_err(|_| "independent data copy failed")?;
            fs::File::open(&target)
                .and_then(|f| f.sync_all())
                .map_err(|_| "independent copy sync failed")?;
            independent(&e.source, &target)?;
        }
    }
    let tree = fingerprint(&entries(dest, &maps)?)?;
    need(
        tree == fingerprint(&source)?,
        "independent copy tree checksum mismatch",
    )?;
    Ok(Cloned {
        data: dest.into(),
        maps,
        tree,
    })
}
fn independent(a: &Path, b: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let a = fs::metadata(a).map_err(|_| "source inode unavailable")?;
        let b = fs::metadata(b).map_err(|_| "copy inode unavailable")?;
        need(
            (a.dev(), a.ino()) != (b.dev(), b.ino()),
            "copy must not hardlink preserved source",
        )?;
    }
    Ok(())
}
pub(super) fn assert_no_source_links(c: &Config, copy: &Cloned) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let source = entries(&c.source, &table_map(c)?)?;
        let mut inodes = std::collections::BTreeSet::new();
        for e in source.iter().filter(|e| !e.directory) {
            let m = fs::metadata(&e.source).map_err(|_| "source inode unavailable")?;
            inodes.insert((m.dev(), m.ino()));
        }
        for e in entries(&copy.data, &copy.maps)?
            .iter()
            .filter(|e| !e.directory)
        {
            let m = fs::metadata(&e.source).map_err(|_| "new copy inode unavailable")?;
            need(
                !inodes.contains(&(m.dev(), m.ino())),
                "working copy shares a preserved-source inode",
            )?;
        }
    }
    Ok(())
}
pub(super) fn linked_files(old: &Cloned, new: &Cloned) -> Result<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let mut inodes = std::collections::BTreeSet::new();
        for e in entries(&old.data, &old.maps)?
            .iter()
            .filter(|e| !e.directory)
        {
            let m = fs::metadata(&e.source).map_err(|_| "old link-proof metadata failed")?;
            inodes.insert((m.dev(), m.ino()));
        }
        let mut linked = 0;
        for e in entries(&new.data, &new.maps)?
            .iter()
            .filter(|e| !e.directory)
        {
            let m = fs::metadata(&e.source).map_err(|_| "new link-proof metadata failed")?;
            if inodes.contains(&(m.dev(), m.ino())) {
                linked += 1;
            }
        }
        Ok(linked)
    }
    #[cfg(not(unix))]
    {
        let _ = (old, new);
        Err("inode link proof requires Unix".into())
    }
}
pub(super) fn authority_binding(c: &Config) -> Result<Value> {
    let mut out = serde_json::Map::new();
    for (kind, path) in [
        (
            "baseline",
            c.repo.join("database/baselines").join(c.get("baseline")?),
        ),
        (
            "epoch",
            c.repo.join("database/migrations").join(c.get("baseline")?),
        ),
    ] {
        out.insert(
            kind.into(),
            fingerprint(&entries(&path, &BTreeMap::new())?)?,
        );
    }
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    #[test]
    fn hardlinks_to_source_are_refused() {
        let root = std::env::temp_dir().join(format!("af-link-test-{}", uuid::Uuid::new_v4()));
        private_dir(&root).unwrap();
        let a = root.join("source");
        let b = root.join("link");
        fs::write(&a, b"synthetic").unwrap();
        fs::hard_link(&a, &b).unwrap();
        assert!(independent(&a, &b).is_err());
        fs::remove_file(&b).unwrap();
        fs::copy(&a, &b).unwrap();
        assert!(independent(&a, &b).is_ok());
        fs::remove_file(a).unwrap();
        fs::remove_file(b).unwrap();
        fs::remove_dir(root).unwrap();
    }
    #[test]
    fn byte_mutation_changes_tree_evidence() {
        let root = std::env::temp_dir().join(format!("af-tree-test-{}", uuid::Uuid::new_v4()));
        private_dir(&root).unwrap();
        let file = root.join("page");
        fs::write(&file, b"before").unwrap();
        let a = fingerprint(&entries(&root, &BTreeMap::new()).unwrap()).unwrap();
        fs::write(&file, b"after!").unwrap();
        let b = fingerprint(&entries(&root, &BTreeMap::new()).unwrap()).unwrap();
        assert_ne!(a["sha256"], b["sha256"]);
        fs::remove_file(file).unwrap();
        fs::remove_dir(root).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn non_tablespace_symlink_is_not_followed() {
        let root = std::env::temp_dir().join(format!("af-symlink-test-{}", uuid::Uuid::new_v4()));
        private_dir(&root).unwrap();
        let link = root.join("pg_wal");
        std::os::unix::fs::symlink("/not-an-allowlisted-source", &link).unwrap();
        assert!(entries(&root, &BTreeMap::new()).is_err());
        fs::remove_file(link).unwrap();
        fs::remove_dir(root).unwrap();
    }
}
