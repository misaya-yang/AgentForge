//! Open-directory-descriptor containment. Checked display paths never authorize I/O.
use super::{
    LocalNodeError as Error, Result,
    grants::{DirectoryGrant, identity},
};
#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd};
use std::{ffi::CString, fs::File, path::PathBuf};
use unicode_normalization::UnicodeNormalization;
pub struct SecureWorkspace {
    pub grant: DirectoryGrant,
}
impl SecureWorkspace {
    pub fn new(grant: DirectoryGrant) -> Self {
        Self { grant }
    }
    pub fn parts(relative: &str) -> Result<Vec<String>> {
        if relative.contains(['\0', '\\'])
            || relative.starts_with('/')
            || relative.nfc().collect::<String>() != relative
            || (relative.as_bytes().get(1) == Some(&b':'))
        {
            return Err(Error::path(
                "absolute, non-NFC or cross-platform path is not allowed",
            ));
        }
        let mut parts = Vec::new();
        for part in relative.split('/') {
            if part.is_empty() || part == "." {
                continue;
            }
            if part == ".." {
                return Err(Error::path("path contains an unsafe component"));
            }
            let lower = part.to_lowercase();
            if [
                ".ssh",
                ".gnupg",
                ".aws",
                ".azure",
                ".kube",
                ".docker",
                "wallet",
                "wallets",
                "keychains",
                "id_rsa",
                "id_dsa",
                "id_ecdsa",
                "id_ed25519",
                "credentials",
                "credentials.json",
                "application_default_credentials.json",
                ".npmrc",
                ".pypirc",
                ".netrc",
                ".git-credentials",
                "pip.conf",
                "nuget.config",
                "login data",
                "cookies",
                "key4.db",
                "logins.json",
                "wallet.dat",
                "seed",
                "seed.txt",
                "seed-phrase.txt",
                "seed_phrase.txt",
                "mnemonic",
                "mnemonic.txt",
            ]
            .contains(&lower.as_str())
                || lower == ".env"
                || lower.starts_with(".env.")
                || [".pem", ".p12", ".pfx", ".key"]
                    .iter()
                    .any(|s| lower.ends_with(s))
                || ["seed-phrase", "seed_phrase", "mnemonic"]
                    .iter()
                    .any(|s| lower.contains(s))
            {
                return Err(Error::path(
                    "sensitive credential material is outside file authority",
                ));
            }
            parts.push(part.into());
        }
        Ok(parts)
    }
    #[cfg(unix)]
    fn root(&self) -> Result<File> {
        use std::os::unix::ffi::OsStrExt;
        let raw = CString::new(self.grant.root.as_os_str().as_bytes())
            .map_err(|_| Error::path("invalid root"))?;
        let fd = unsafe {
            libc::open(
                raw.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let file = unsafe { File::from_raw_fd(fd) };
        if identity(&file.metadata()?)? != (self.grant.root_device, self.grant.root_inode) {
            return Err(Error::path("grant root changed"));
        }
        Ok(file)
    }
    #[cfg(not(unix))]
    fn root(&self) -> Result<File> {
        Err(Error::new(
            "driver_unavailable",
            "secure descriptor filesystem adapter unavailable",
        ))
    }
    pub fn open_dir(&self, relative: &str) -> Result<File> {
        let mut file = self.root()?;
        for part in Self::parts(relative)? {
            file = open_at(&file, &part, true, false)?;
        }
        Ok(file)
    }
    pub fn open_parent(&self, relative: &str) -> Result<(File, String)> {
        let parts = Self::parts(relative)?;
        let (name, parents) = parts
            .split_last()
            .ok_or_else(|| Error::path("a file path is required"))?;
        let mut file = self.root()?;
        for part in parents {
            file = open_at(&file, part, true, false)?;
        }
        Ok((file, name.clone()))
    }
    pub fn open_read(&self, relative: &str) -> Result<File> {
        let (parent, name) = self.open_parent(relative)?;
        let file = open_at(&parent, &name, false, false)?;
        check_regular(&file)?;
        Ok(file)
    }
    pub fn display_path(&self, relative: &str) -> Result<PathBuf> {
        let parts = Self::parts(relative)?;
        Ok(parts.iter().fold(self.grant.root.clone(), |p, s| p.join(s)))
    }
}
pub fn check_regular(file: &File) -> Result<()> {
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(Error::path("only regular files may be read"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1 {
            return Err(Error::path("hard-linked files are outside the grant"));
        }
    }
    Ok(())
}
pub fn open_at(parent: &File, name: &str, directory: bool, create: bool) -> Result<File> {
    #[cfg(unix)]
    {
        let name = CString::new(name).map_err(|_| Error::path("invalid path component"))?;
        let mut flags = libc::O_CLOEXEC
            | libc::O_NOFOLLOW
            | libc::O_NONBLOCK
            | if create {
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL
            } else {
                libc::O_RDONLY
            };
        if directory {
            flags |= libc::O_DIRECTORY;
        }
        let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags, 0o600) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(unsafe { File::from_raw_fd(fd) })
    }
    #[cfg(not(unix))]
    {
        let _ = (parent, name, directory, create);
        Err(Error::new(
            "driver_unavailable",
            "secure descriptor filesystem adapter unavailable",
        ))
    }
}
pub fn names(directory: &File) -> Result<Vec<String>> {
    #[cfg(unix)]
    {
        use std::ffi::CStr;
        let fd = unsafe { libc::dup(directory.as_raw_fd()) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let dir = unsafe { libc::fdopendir(fd) };
        if dir.is_null() {
            unsafe { libc::close(fd) };
            return Err(std::io::Error::last_os_error().into());
        }
        let mut items = Vec::new();
        loop {
            let entry = unsafe { libc::readdir(dir) };
            if entry.is_null() {
                break;
            }
            let value = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
            if let Ok(name) = value.to_str() {
                if name != "." && name != ".." {
                    items.push(name.into());
                }
            }
        }
        unsafe { libc::closedir(dir) };
        items.sort();
        Ok(items)
    }
    #[cfg(not(unix))]
    {
        let _ = directory;
        Err(Error::new(
            "driver_unavailable",
            "secure descriptor filesystem adapter unavailable",
        ))
    }
}
pub fn remove_at(parent: &File, name: &str) -> Result<()> {
    #[cfg(unix)]
    {
        let name = CString::new(name).map_err(|_| Error::path("invalid component"))?;
        if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), 0) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (parent, name);
        Err(Error::path("descriptor removal unavailable"))
    }
}
pub fn rename_at(parent: &File, source: &str, target: &str) -> Result<()> {
    #[cfg(unix)]
    {
        let source = CString::new(source).map_err(|_| Error::path("invalid component"))?;
        let target = CString::new(target).map_err(|_| Error::path("invalid component"))?;
        if unsafe {
            libc::renameat(
                parent.as_raw_fd(),
                source.as_ptr(),
                parent.as_raw_fd(),
                target.as_ptr(),
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (parent, source, target);
        Err(Error::path("descriptor replacement unavailable"))
    }
}
