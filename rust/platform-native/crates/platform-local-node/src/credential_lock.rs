//! OS-released per-credential advisory lock. Lock files contain no credential.
use crate::{Error, Result};
use std::{
    fs::{File, OpenOptions},
    path::PathBuf,
    time::{Duration, Instant},
};
pub struct CredentialLock {
    file: File,
}
#[cfg(unix)]
impl Drop for CredentialLock {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd;
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}
#[cfg(unix)]
fn directory() -> Result<PathBuf> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    let path = PathBuf::from("/tmp").join(format!("agentforge-client-locks-{}", unsafe {
        libc::geteuid()
    }));
    match std::fs::DirBuilder::new().mode(0o700).create(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err(Error::Storage),
    }
    let m = std::fs::symlink_metadata(&path)?;
    if !m.is_dir()
        || m.file_type().is_symlink()
        || m.uid() != unsafe { libc::geteuid() }
        || m.mode() & 0o077 != 0
    {
        return Err(Error::Denied("unsafe client lock directory"));
    }
    Ok(path)
}
#[cfg(windows)]
fn directory() -> Result<PathBuf> {
    use std::{ffi::c_void, os::windows::ffi::OsStringExt};
    #[repr(C)]
    struct Guid {
        a: u32,
        b: u16,
        c: u16,
        d: [u8; 8],
    }
    #[link(name = "Shell32")]
    unsafe extern "system" {
        fn SHGetKnownFolderPath(
            id: *const Guid,
            flags: u32,
            token: *mut c_void,
            out: *mut *mut u16,
        ) -> i32;
    }
    #[link(name = "Ole32")]
    unsafe extern "system" {
        fn CoTaskMemFree(value: *mut c_void);
    }
    let id = Guid {
        a: 0xf1b32785,
        b: 0x6fba,
        c: 0x4fcf,
        d: [0x9d, 0x55, 0x7b, 0x8e, 0x7f, 0x15, 0x70, 0x91],
    };
    let mut raw = std::ptr::null_mut();
    if unsafe { SHGetKnownFolderPath(&id, 0, std::ptr::null_mut(), &mut raw) } < 0 || raw.is_null()
    {
        return Err(Error::Storage);
    }
    let mut len = 0;
    while len < 32768 && unsafe { *raw.add(len) } != 0 {
        len += 1;
    }
    let result = if len == 32768 {
        Err(Error::Storage)
    } else {
        Ok(PathBuf::from(std::ffi::OsString::from_wide(unsafe {
            std::slice::from_raw_parts(raw, len)
        })))
    };
    unsafe {
        CoTaskMemFree(raw.cast());
    }
    let path = result?.join("AgentForge").join("ClientLocks");
    std::fs::create_dir_all(&path)?;
    Ok(path)
}
pub fn try_acquire(reference: &str) -> Result<Option<CredentialLock>> {
    if reference.is_empty()
        || reference.len() > 128
        || !reference
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
    {
        return Err(Error::Denied("invalid client lock reference"));
    }
    #[cfg(not(any(unix, windows)))]
    return Err(Error::Denied("client lock unavailable"));
    #[cfg(any(unix, windows))]
    {
        let path = directory()?.join(format!("{reference}.lock"));
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(0);
        }
        match options.open(&path) {
            Ok(file) => {
                #[cfg(unix)]
                {
                    use std::os::{fd::AsRawFd, unix::fs::MetadataExt};
                    let m = file.metadata()?;
                    if !m.is_file()
                        || m.uid() != unsafe { libc::geteuid() }
                        || m.mode() & 0o077 != 0
                    {
                        return Err(Error::Integrity);
                    }
                    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0
                    {
                        return Ok(Some(CredentialLock { file }));
                    }
                    if !matches!(
                        std::io::Error::last_os_error().raw_os_error(),
                        Some(libc::EWOULDBLOCK)
                    ) {
                        return Err(Error::Storage);
                    }
                    Ok(None)
                }
                #[cfg(windows)]
                {
                    Ok(Some(CredentialLock { file }))
                }
            }
            Err(e) => {
                #[cfg(windows)]
                if e.raw_os_error() == Some(32) {
                    return Ok(None);
                }
                let _ = e;
                Err(Error::Storage)
            }
        }
    }
}
pub fn acquire(reference: &str) -> Result<CredentialLock> {
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        if let Some(lock) = try_acquire(reference)? {
            return Ok(lock);
        }
        if Instant::now() >= deadline {
            return Err(Error::Denied("client credential is busy"));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn independent_handles_serialize_refresh_ownership() {
        let reference = format!("test_{}", uuid::Uuid::new_v4().simple());
        let first = acquire(&reference).unwrap();
        let other = reference.clone();
        let started = std::sync::Arc::new(std::sync::Barrier::new(2));
        let barrier = started.clone();
        let worker = std::thread::spawn(move || {
            barrier.wait();
            let start = Instant::now();
            let _guard = acquire(&other).unwrap();
            start.elapsed()
        });
        started.wait();
        std::thread::sleep(Duration::from_millis(60));
        drop(first);
        assert!(worker.join().unwrap() >= Duration::from_millis(40));
        // Only this test's newly created, public lock file is removed.
        std::fs::remove_file(directory().unwrap().join(format!("{reference}.lock"))).unwrap();
        assert!(acquire("../other").is_err());
    }
}
