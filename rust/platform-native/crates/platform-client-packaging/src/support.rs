fn resolve_path(input: &str) -> Result<PathBuf> {
    let expanded = if input == "~" || input.starts_with("~/") {
        PathBuf::from(
            std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .ok_or("home directory missing")?,
        )
        .join(input.strip_prefix("~/").unwrap_or(""))
    } else if let Some(rest) = input.strip_prefix('~') {
        let (user, suffix) = rest.split_once('/').unwrap_or((rest, ""));
        let passwd = std::fs::read_to_string("/etc/passwd")?;
        let home = passwd
            .lines()
            .find_map(|line| {
                let fields: Vec<_> = line.split(':').collect();
                (fields.len() >= 7 && fields[0] == user).then(|| fields[5])
            })
            .ok_or("unknown home directory")?;
        PathBuf::from(home).join(suffix)
    } else {
        PathBuf::from(input)
    };
    let absolute = if expanded.is_absolute() {
        expanded
    } else {
        std::env::current_dir()?.join(expanded)
    };
    fn resolve(path: &Path, links: &mut usize) -> Result<PathBuf> {
        use std::path::Component;
        let mut out = PathBuf::new();
        for component in path.components() {
            match component {
                Component::RootDir => out.push("/"),
                Component::CurDir => {}
                Component::ParentDir => {
                    out.pop();
                }
                Component::Normal(part) => {
                    out.push(part);
                    match std::fs::symlink_metadata(&out) {
                        Ok(metadata) if metadata.file_type().is_symlink() => {
                            *links += 1;
                            require(*links <= 40, "symlink loop or excessive depth")?;
                            let target = std::fs::read_link(&out)?;
                            out.pop();
                            out = resolve(&out.join(target), links)?;
                        }
                        Ok(_) => {}
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(e) => return Err(e.into()),
                    }
                }
                Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            }
        }
        Ok(out)
    }
    let resolved = resolve(&absolute, &mut 0)?;
    // On Windows canonicalization also normalizes drive/UNC prefixes; compare
    // output containment using the same representation as the repository root.
    let mut parent = resolved.as_path();
    let mut missing = Vec::new();
    while !parent.exists() {
        missing.push(
            parent
                .file_name()
                .ok_or("output has no existing ancestor")?
                .to_owned(),
        );
        parent = parent.parent().ok_or("output has no existing ancestor")?;
    }
    let mut normalized = parent.canonicalize()?;
    for part in missing.into_iter().rev() {
        normalized.push(part);
    }
    Ok(normalized)
}
fn object(path: &Path) -> Result<Value> {
    let v = strict_json(&std::fs::read(path)?)?;
    require(v.is_object(), "JSON must be an object")?;
    Ok(v)
}
fn strict_json(bytes: &[u8]) -> Result<Value> {
    struct Strict(Value);
    impl<'de> serde::Deserialize<'de> for Strict {
        fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
            struct Visitor;
            impl<'de> serde::de::Visitor<'de> for Visitor {
                type Value = Strict;
                fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                    f.write_str("JSON without duplicate object keys")
                }
                fn visit_bool<E: serde::de::Error>(
                    self,
                    v: bool,
                ) -> std::result::Result<Strict, E> {
                    Ok(Strict(json!(v)))
                }
                fn visit_i64<E: serde::de::Error>(self, v: i64) -> std::result::Result<Strict, E> {
                    Ok(Strict(json!(v)))
                }
                fn visit_u64<E: serde::de::Error>(self, v: u64) -> std::result::Result<Strict, E> {
                    Ok(Strict(json!(v)))
                }
                fn visit_f64<E: serde::de::Error>(self, v: f64) -> std::result::Result<Strict, E> {
                    serde_json::Number::from_f64(v)
                        .map(|n| Strict(Value::Number(n)))
                        .ok_or_else(|| E::custom("non-finite JSON number"))
                }
                fn visit_str<E: serde::de::Error>(self, v: &str) -> std::result::Result<Strict, E> {
                    Ok(Strict(json!(v)))
                }
                fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<Strict, E> {
                    Ok(Strict(Value::Null))
                }
                fn visit_seq<A: serde::de::SeqAccess<'de>>(
                    self,
                    mut a: A,
                ) -> std::result::Result<Strict, A::Error> {
                    let mut values = vec![];
                    while let Some(Strict(v)) = a.next_element()? {
                        values.push(v);
                    }
                    Ok(Strict(Value::Array(values)))
                }
                fn visit_map<A: serde::de::MapAccess<'de>>(
                    self,
                    mut a: A,
                ) -> std::result::Result<Strict, A::Error> {
                    let mut values = serde_json::Map::new();
                    while let Some((k, Strict(v))) = a.next_entry::<String, Strict>()? {
                        if values.insert(k, v).is_some() {
                            return Err(serde::de::Error::custom("duplicate JSON object key"));
                        }
                    }
                    Ok(Strict(Value::Object(values)))
                }
            }
            d.deserialize_any(Visitor)
        }
    }
    Ok(serde_json::from_slice::<Strict>(bytes)?.0)
}
fn safe(root: &Path, relative: &str) -> Result<PathBuf> {
    require(
        !relative.is_empty()
            && !relative.contains(['\\', '\0', ':'])
            && relative.split('/').all(|s| !matches!(s, "" | "." | "..")),
        "unsafe relative path",
    )?;
    let root = root.canonicalize()?;
    let mut path = root.clone();
    for part in relative.split('/') {
        path.push(part);
        if let Ok(m) = std::fs::symlink_metadata(&path) {
            require(!m.file_type().is_symlink(), "source symlink forbidden")?;
        }
    }
    require(path.starts_with(root), "escaping path")?;
    Ok(path)
}
fn walk(root: &Path, prune: &[&str], symlinks: bool) -> Result<Vec<PathBuf>> {
    fn visit(
        base: &Path,
        path: &Path,
        prune: &[&str],
        links: bool,
        out: &mut Vec<PathBuf>,
    ) -> Result<()> {
        for e in std::fs::read_dir(path)? {
            let e = e?;
            let p = e.path();
            let m = std::fs::symlink_metadata(&p)?;
            if m.file_type().is_symlink() {
                if links {
                    out.push(p.strip_prefix(base)?.into());
                }
                continue;
            }
            if m.is_dir() {
                if !prune.contains(&e.file_name().to_string_lossy().as_ref()) {
                    visit(base, &p, prune, links, out)?;
                }
            } else {
                require(m.is_file(), "nonregular filesystem entry")?;
                out.push(p.strip_prefix(base)?.into());
            }
        }
        Ok(())
    }
    let mut out = vec![];
    visit(root, root, prune, symlinks, &mut out)?;
    out.sort();
    Ok(out)
}
