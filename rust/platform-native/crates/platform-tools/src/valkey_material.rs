//! Explicit local preparation only. No running services, certificate issuance,
//! ownership changes, existing-file replacement or credential output.
use ai_platform_service_primitives::valkey::{acl, Instance};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::Path,
};
fn write_new(path: &Path, contents: &[u8]) -> Result<(), String> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut f = options
        .open(path)
        .map_err(|_| "valkey material file creation refused".to_owned())?;
    f.write_all(contents)
        .and_then(|_| f.sync_all())
        .map_err(|_| "valkey material write failed".to_owned())
}
/// Generate once into a new explicitly selected ignored/private directory.
/// Return only a count; callers must not serialize intermediate passwords/hashes.
pub fn generate(repository: &Path, directory: &Path) -> Result<usize, String> {
    if !directory.is_absolute() || directory.exists() {
        return Err("new absolute private directory required".into());
    }
    let root = repository
        .canonicalize()
        .map_err(|_| "repository root unavailable".to_owned())?;
    let parent = directory
        .parent()
        .and_then(|p| p.canonicalize().ok())
        .ok_or("existing private parent required")?;
    if parent.starts_with(root) {
        return Err("credential material must remain outside repository".into());
    }
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(directory)
        .map_err(|_| "valkey private directory creation failed".to_owned())?;
    let mut hashes = BTreeMap::new();
    for &user in acl::USERS {
        let mut bytes = [0u8; 32];
        fs::File::open("/dev/urandom")
            .and_then(|mut f| f.read_exact(&mut bytes))
            .map_err(|_| "OS randomness unavailable".to_owned())?;
        let password: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        hashes.insert(
            user.to_owned(),
            format!("{:x}", Sha256::digest(password.as_bytes())),
        );
        write_new(
            &directory.join(format!("{user}.password")),
            password.as_bytes(),
        )?;
    }
    for instance in [Instance::Coord, Instance::Cache] {
        let value =
            acl::render(instance, &hashes).map_err(|_| "invalid valkey ACL material".to_owned())?;
        write_new(
            &directory.join(format!("{}.acl", instance.name())),
            value.as_bytes(),
        )?;
    }
    Ok(acl::USERS.len() + 2)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generation_requires_new_absolute_destination() {
        assert!(generate(Path::new("/tmp"), Path::new("relative")).is_err());
        assert!(generate(Path::new("/tmp"), Path::new("/tmp")).is_err());
    }
    #[test]
    fn file_creation_never_overwrites() {
        let p =
            std::env::temp_dir().join(format!("valkey-material-contract-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        write_new(&p, b"public-test").unwrap();
        assert!(write_new(&p, b"replaced").is_err());
        assert_eq!(fs::read(&p).unwrap(), b"public-test");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&p).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        fs::remove_file(p).unwrap();
    }
}
