//! Sandboxed workspace filesystem reads (fd-relative walk on Unix).

use std::io::Read;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::ffi::{CStr, OsString};
#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
#[cfg(unix)]
use std::os::unix::ffi::{OsStrExt, OsStringExt};

use globset::{Glob, GlobMatcher};
use serde_json::{Value, json};

use super::{
    MAX_FILE_BYTES, MAX_GLOB_RESULTS, MAX_GREP_RESULTS, MAX_WALKED_FILES, ReadCapabilityError,
};

pub(super) fn read_file(
    root: &Path,
    relative: &str,
    offset: usize,
    limit: usize,
) -> Result<Value, ReadCapabilityError> {
    let bytes = read_bounded_file(root, relative)?;
    let byte_truncated = bytes.len() > MAX_FILE_BYTES;
    let text = String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_FILE_BYTES)]);
    let lines: Vec<_> = text.lines().collect();
    let selected = lines
        .iter()
        .skip(offset)
        .take(limit)
        .copied()
        .collect::<Vec<_>>();
    Ok(json!({
        "path": relative,
        "content": selected.join("\n"),
        "total_lines": lines.len(),
        "returned_lines": selected.len(),
        "byte_truncated": byte_truncated
    }))
}

pub(super) fn glob_files(root: &Path, pattern: &str) -> Result<Value, ReadCapabilityError> {
    let matcher = Glob::new(pattern)
        .map_err(|_| ReadCapabilityError::Arguments)?
        .compile_matcher();
    let (paths, walked_truncated) = walk_files(root, &matcher, MAX_GLOB_RESULTS)?;
    Ok(json!({
        "paths": paths,
        "truncated": walked_truncated || paths.len() >= MAX_GLOB_RESULTS
    }))
}

pub(super) fn grep_files(
    root: &Path,
    pattern: &str,
    glob: &str,
    case_sensitive: bool,
) -> Result<Value, ReadCapabilityError> {
    let matcher = Glob::new(glob)
        .map_err(|_| ReadCapabilityError::Arguments)?
        .compile_matcher();
    let (files, walked_truncated) = walk_files(root, &matcher, MAX_WALKED_FILES)?;
    let needle = if case_sensitive {
        pattern.to_string()
    } else {
        pattern.to_lowercase()
    };
    let mut matches = Vec::new();
    for relative in files {
        if matches.len() >= MAX_GREP_RESULTS {
            break;
        }
        let Ok(bytes) = read_bounded_file(root, &relative) else {
            continue;
        };
        if bytes.len() > MAX_FILE_BYTES {
            continue;
        }
        let content = String::from_utf8_lossy(&bytes);
        for (index, line) in content.lines().enumerate() {
            let haystack = if case_sensitive {
                line.to_string()
            } else {
                line.to_lowercase()
            };
            if haystack.contains(&needle) {
                matches.push(json!({
                    "path": relative,
                    "line": index + 1,
                    "text": line.chars().take(500).collect::<String>()
                }));
                if matches.len() >= MAX_GREP_RESULTS {
                    break;
                }
            }
        }
    }
    Ok(json!({
        "matches": matches,
        "truncated": walked_truncated || matches.len() >= MAX_GREP_RESULTS
    }))
}

fn walk_files(
    root: &Path,
    matcher: &GlobMatcher,
    maximum_results: usize,
) -> Result<(Vec<String>, bool), ReadCapabilityError> {
    #[cfg(unix)]
    {
        walk_files_unix(root, matcher, maximum_results)
    }
    #[cfg(not(unix))]
    {
        walk_files_non_unix(root, matcher, maximum_results)
    }
}

#[cfg(unix)]
fn read_bounded_file(root: &Path, relative: &str) -> Result<Vec<u8>, ReadCapabilityError> {
    let file = open_relative_file(root, relative)?;
    let metadata = file.metadata().map_err(|_| ReadCapabilityError::NotFound)?;
    if !metadata.is_file() {
        return Err(ReadCapabilityError::NotFound);
    }
    let mut bytes = Vec::new();
    file.take((MAX_FILE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| ReadCapabilityError::WorkerFailed)?;
    Ok(bytes)
}

#[cfg(unix)]
fn open_relative_file(root: &Path, relative: &str) -> Result<std::fs::File, ReadCapabilityError> {
    let components = relative_components(relative)?;
    let mut directory = open_absolute_directory(root)?;
    let (file_name, parents) = components
        .split_last()
        .ok_or(ReadCapabilityError::NotFound)?;
    for component in parents {
        directory = openat_directory(&directory, component)
            .map_err(|error| map_open_error(error, ReadCapabilityError::NotFound))?;
    }
    let file = openat_file(&directory, file_name)
        .map_err(|error| map_open_error(error, ReadCapabilityError::NotFound))?;
    Ok(std::fs::File::from(file))
}

#[cfg(unix)]
fn relative_components(relative: &str) -> Result<Vec<OsString>, ReadCapabilityError> {
    let path = Path::new(relative);
    if path.is_absolute() {
        return Err(ReadCapabilityError::PathEscape);
    }
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(value) => components.push(value.to_os_string()),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir
            | std::path::Component::RootDir
            | std::path::Component::Prefix(_) => return Err(ReadCapabilityError::PathEscape),
        }
    }
    Ok(components)
}

#[cfg(unix)]
fn open_absolute_directory(path: &Path) -> Result<OwnedFd, ReadCapabilityError> {
    if !path.is_absolute() {
        return Err(ReadCapabilityError::Configuration);
    }
    // The workspace root is trusted operator configuration. Resolve it once
    // so platform-owned aliases such as macOS `/var -> /private/var` do not
    // make the worker unusable. Every user-controlled component below this
    // opened root is still resolved fd-relatively with O_NOFOLLOW.
    let path = path
        .canonicalize()
        .map_err(|_| ReadCapabilityError::Configuration)?;
    let mut directory = open_path_directory(Path::new("/"))
        .map_err(|error| map_open_error(error, ReadCapabilityError::Configuration))?;
    for component in path.components() {
        let std::path::Component::Normal(component) = component else {
            if matches!(component, std::path::Component::RootDir) {
                continue;
            }
            return Err(ReadCapabilityError::Configuration);
        };
        directory = openat_directory(&directory, component)
            .map_err(|error| map_open_error(error, ReadCapabilityError::Configuration))?;
    }
    Ok(directory)
}

#[cfg(unix)]
fn open_path_directory(path: &Path) -> std::io::Result<OwnedFd> {
    let component = path.as_os_str();
    let name = std::ffi::CString::new(component.as_bytes())
        .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;
    open_fd(
        libc::AT_FDCWD,
        &name,
        libc::O_RDONLY | libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW,
    )
}

#[cfg(unix)]
fn openat_directory(directory: &OwnedFd, component: &std::ffi::OsStr) -> std::io::Result<OwnedFd> {
    let name = std::ffi::CString::new(component.as_bytes())
        .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;
    open_fd(
        directory.as_raw_fd(),
        &name,
        libc::O_RDONLY | libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW,
    )
}

#[cfg(unix)]
fn openat_file(directory: &OwnedFd, component: &std::ffi::OsStr) -> std::io::Result<OwnedFd> {
    let name = std::ffi::CString::new(component.as_bytes())
        .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;
    open_fd(
        directory.as_raw_fd(),
        &name,
        libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
    )
}

#[cfg(unix)]
fn open_fd(
    directory: libc::c_int,
    name: &std::ffi::CStr,
    flags: libc::c_int,
) -> std::io::Result<OwnedFd> {
    // Every component is opened relative to an already-open directory and
    // O_NOFOLLOW is applied to each component. This makes replacement of a
    // path component between validation and use unable to escape the root.
    let fd = unsafe { libc::openat(directory, name.as_ptr(), flags, 0) };
    if fd < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }
}

#[cfg(unix)]
fn map_open_error(error: std::io::Error, ordinary: ReadCapabilityError) -> ReadCapabilityError {
    // macOS reports ENOTDIR for `O_DIRECTORY | O_NOFOLLOW` on a symlinked
    // parent while Linux normally reports ELOOP. Both must fail closed with
    // the same stable contract error.
    if error
        .raw_os_error()
        .is_some_and(|code| code == libc::ELOOP || code == libc::ENOTDIR)
    {
        ReadCapabilityError::PathEscape
    } else {
        ordinary
    }
}

#[cfg(unix)]
fn walk_files_unix(
    root: &Path,
    matcher: &GlobMatcher,
    maximum_results: usize,
) -> Result<(Vec<String>, bool), ReadCapabilityError> {
    let root_directory =
        open_absolute_directory(root).map_err(|_| ReadCapabilityError::Configuration)?;
    let mut directories = vec![(PathBuf::new(), root_directory)];
    let mut output = Vec::new();
    let mut walked = 0_usize;
    while let Some((relative_directory, directory)) = directories.pop() {
        let mut entries = read_directory_entries(&directory)?;
        entries.sort();
        for name in entries {
            walked += 1;
            if walked > MAX_WALKED_FILES {
                output.sort();
                return Ok((output, true));
            }
            let relative = if relative_directory.as_os_str().is_empty() {
                PathBuf::from(&name)
            } else {
                relative_directory.join(&name)
            };
            if let Ok(child_directory) = openat_directory(&directory, &name) {
                directories.push((relative, child_directory));
                continue;
            }
            let Ok(file) = openat_file(&directory, &name) else {
                continue;
            };
            let Ok(metadata) = std::fs::File::from(file).metadata() else {
                continue;
            };
            if metadata.is_file() && matcher.is_match(&relative) {
                output.push(relative.to_string_lossy().replace('\\', "/"));
                if output.len() >= maximum_results {
                    output.sort();
                    return Ok((output, true));
                }
            }
        }
    }
    output.sort();
    Ok((output, false))
}

#[cfg(unix)]
fn read_directory_entries(directory: &OwnedFd) -> Result<Vec<OsString>, ReadCapabilityError> {
    let duplicate = unsafe { libc::dup(directory.as_raw_fd()) };
    if duplicate < 0 {
        return Err(ReadCapabilityError::WorkerFailed);
    }
    let stream = unsafe { libc::fdopendir(duplicate) };
    if stream.is_null() {
        unsafe { libc::close(duplicate) };
        return Err(ReadCapabilityError::WorkerFailed);
    }
    let mut entries = Vec::new();
    loop {
        clear_errno();
        let entry = unsafe { libc::readdir(stream) };
        if entry.is_null() {
            let error = std::io::Error::last_os_error();
            unsafe { libc::closedir(stream) };
            if error.raw_os_error().is_some_and(|code| code != 0) {
                return Err(ReadCapabilityError::WorkerFailed);
            }
            return Ok(entries);
        }
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
        if name.to_bytes() != b"." && name.to_bytes() != b".." {
            entries.push(OsString::from_vec(name.to_bytes().to_vec()));
        }
    }
}

#[cfg(target_os = "linux")]
fn clear_errno() {
    unsafe { *libc::__errno_location() = 0 };
}

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
fn clear_errno() {
    unsafe { *libc::__error() = 0 };
}

#[cfg(all(
    unix,
    not(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd"
    ))
))]
fn clear_errno() {}

#[cfg(not(unix))]
fn rooted_existing_path(root: &Path, relative: &str) -> Result<PathBuf, ReadCapabilityError> {
    let root = root
        .canonicalize()
        .map_err(|_| ReadCapabilityError::Configuration)?;
    let relative = Path::new(relative);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(ReadCapabilityError::PathEscape);
    }
    let resolved = root
        .join(relative)
        .canonicalize()
        .map_err(|_| ReadCapabilityError::NotFound)?;
    if !resolved.starts_with(&root) {
        return Err(ReadCapabilityError::PathEscape);
    }
    Ok(resolved)
}

#[cfg(not(unix))]
fn read_bounded_file(root: &Path, relative: &str) -> Result<Vec<u8>, ReadCapabilityError> {
    let path = rooted_existing_path(root, relative)?;
    let metadata = path
        .symlink_metadata()
        .map_err(|_| ReadCapabilityError::NotFound)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(ReadCapabilityError::NotFound);
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|_| ReadCapabilityError::NotFound)?
        .take((MAX_FILE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| ReadCapabilityError::WorkerFailed)?;
    Ok(bytes)
}

#[cfg(not(unix))]
fn walk_files_non_unix(
    root: &Path,
    matcher: &GlobMatcher,
    maximum_results: usize,
) -> Result<(Vec<String>, bool), ReadCapabilityError> {
    let root = root
        .canonicalize()
        .map_err(|_| ReadCapabilityError::Configuration)?;
    let mut directories = vec![root.clone()];
    let mut output = Vec::new();
    let mut walked = 0_usize;
    while let Some(directory) = directories.pop() {
        let entries =
            std::fs::read_dir(directory).map_err(|_| ReadCapabilityError::WorkerFailed)?;
        for entry in entries.flatten() {
            walked += 1;
            if walked > MAX_WALKED_FILES {
                return Ok((output, true));
            }
            let path = entry.path();
            let Ok(metadata) = path.symlink_metadata() else {
                continue;
            };
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                directories.push(path);
                continue;
            }
            if metadata.is_file() {
                let Ok(relative) = path.strip_prefix(&root) else {
                    continue;
                };
                if matcher.is_match(relative) {
                    output.push(relative.to_string_lossy().replace('\\', "/"));
                    if output.len() >= maximum_results {
                        return Ok((output, true));
                    }
                }
            }
        }
    }
    output.sort();
    Ok((output, false))
}

#[cfg(test)]
mod tests {
    use super::super::web::{is_public_ip, strip_html};
    use super::*;
    use uuid::Uuid;

    #[test]
    fn private_addresses_are_rejected() {
        assert!(!is_public_ip("127.0.0.1".parse().unwrap()));
        assert!(!is_public_ip("169.254.169.254".parse().unwrap()));
        assert!(!is_public_ip("::1".parse().unwrap()));
        assert!(is_public_ip("1.1.1.1".parse().unwrap()));
    }

    #[test]
    fn html_extraction_does_not_preserve_tags() {
        assert_eq!(strip_html("<p>Hello <b>world</b></p>"), "Hello world");
    }

    #[cfg(unix)]
    #[test]
    fn root_relative_open_rejects_replaced_file_symlink() {
        let base = std::env::temp_dir().join(format!("ai-platform-read-{}", Uuid::now_v7()));
        let root = base.join("root");
        let outside = base.join("outside.txt");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("document.txt"), "safe").unwrap();
        std::fs::write(&outside, "outside").unwrap();
        std::fs::remove_file(root.join("document.txt")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("document.txt")).unwrap();

        let error = read_bounded_file(&root, "document.txt").unwrap_err();
        assert_eq!(error, ReadCapabilityError::PathEscape);
        let _ = std::fs::remove_dir_all(base);
    }

    #[cfg(unix)]
    #[test]
    fn root_relative_open_rejects_symlinked_parent_escape() {
        let base = std::env::temp_dir().join(format!("ai-platform-read-{}", Uuid::now_v7()));
        let root = base.join("root");
        let outside = base.join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("document.txt"), "outside").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("nested")).unwrap();

        let error = read_bounded_file(&root, "nested/document.txt").unwrap_err();
        assert_eq!(error, ReadCapabilityError::PathEscape);
        let _ = std::fs::remove_dir_all(base);
    }
}
