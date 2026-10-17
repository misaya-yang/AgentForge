//! Offline native artifacts: verified architectures, real process checks, immutable materials.
use crate::*;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{
    io::{Cursor, Write},
    process::{Command, Stdio},
    time::{Duration, Instant},
};
const WS: &str = "rust/platform-native";
fn string<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key]
        .as_str()
        .ok_or_else(|| format!("missing {key}").into())
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn git(root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()?;
    require(
        output.status.success(),
        "packaging Git identity command failed",
    )?;
    Ok(String::from_utf8(output.stdout)?.trim().into())
}
fn machine(bytes: &[u8], target: &Value) -> Result<()> {
    let name = string(target, "name")?;
    let code = if name.starts_with("linux-") {
        require(
            bytes.len() >= 64 && bytes[..6] == *b"\x7fELF\x02\x01",
            "expected 64-bit little-endian ELF",
        )?;
        u16::from_le_bytes(bytes[18..20].try_into()?) as u32
    } else if name.starts_with("darwin-") {
        require(
            bytes.len() >= 32 && bytes[..4] == [0xcf, 0xfa, 0xed, 0xfe],
            "expected single-architecture 64-bit Mach-O",
        )?;
        u32::from_le_bytes(bytes[4..8].try_into()?)
    } else {
        require(
            bytes.len() >= 64 && bytes[..2] == *b"MZ",
            "expected Windows PE",
        )?;
        let offset = u32::from_le_bytes(bytes[60..64].try_into()?) as usize;
        require(
            offset <= bytes.len() - 24 && bytes[offset..offset + 4] == *b"PE\0\0",
            "invalid PE header",
        )?;
        u16::from_le_bytes(bytes[offset + 4..offset + 6].try_into()?) as u32
    };
    require(
        target["machine"].as_u64() == Some(code as u64),
        "binary architecture differs from target",
    )
}
fn materials(root: &Path, desktop: bool) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut paths = std::collections::BTreeSet::from([
        "LICENSE".to_owned(),
        format!("{WS}/Cargo.toml"),
        format!("{WS}/Cargo.lock"),
        format!("{WS}/README.md"),
        "docs/architecture/native-client-capability-matrix.md".into(),
        format!("{WS}/crates/platform-tools/src/packaging.rs"),
        format!("{WS}/crates/platform-tools/Cargo.toml"),
        format!("{WS}/crates/platform-client-packaging/Cargo.toml"),
    ]);
    if desktop {
        paths.extend([
            "RELEASE.md".into(),
            "deploy/release/desktop-targets.json".into(),
            "docs/architecture/native-desktop-distribution.md".into(),
            ".github/workflows/native-desktop.yml".into(),
        ]);
        for entry in std::fs::read_dir(root.join(format!("{WS}/crates")))? {
            let entry = entry?;
            if entry.file_type()?.is_dir() && entry.path().join("Cargo.toml").is_file() {
                paths.insert(format!(
                    "{WS}/crates/{}/Cargo.toml",
                    entry.file_name().to_string_lossy()
                ));
            }
        }
    } else {
        paths.extend([
            format!("{WS}/crates/platform-client/Cargo.toml"),
            "deploy/release/platformctl-targets.json".into(),
            ".github/workflows/publish-sdk.yml".into(),
        ]);
    }
    for name in if desktop {
        vec![
            "platform-client",
            "platform-desktop",
            "platform-tools",
            "platform-client-packaging",
        ]
    } else {
        vec![
            "platform-client",
            "platform-tools",
            "platform-client-packaging",
        ]
    } {
        let prefix = format!("{WS}/crates/{name}");
        for rel in walk(&root.join(format!("{prefix}/src")), &[], true)? {
            if rel.extension().is_some_and(|s| s == "rs") {
                paths.insert(format!(
                    "{prefix}/src/{}",
                    rel.to_string_lossy().replace('\\', "/")
                ));
            }
        }
        if root.join(format!("{prefix}/build.rs")).is_file() {
            paths.insert(format!("{prefix}/build.rs"));
        }
    }
    let mut source = BTreeMap::new();
    for path in paths {
        source.insert(path.clone(), std::fs::read(safe(root, &path)?)?);
    }
    let mut argv = vec!["status", "--porcelain", "--untracked-files=all", "--"];
    argv.extend(source.keys().map(String::as_str));
    require(
        git(root, &argv)?.is_empty(),
        "package inputs must be committed and clean",
    )?;
    let mut argv = vec!["ls-files", "--"];
    argv.extend(source.keys().map(String::as_str));
    let tracked = git(root, &argv)?;
    require(
        tracked.lines().collect::<std::collections::BTreeSet<_>>()
            == source.keys().map(String::as_str).collect(),
        "all package materials must be tracked",
    )?;
    Ok(source)
}
#[cfg(unix)]
fn smoke(binary: &Path, arg: &str, seconds: u64) -> Result<Vec<u8>> {
    use std::os::{fd::AsRawFd, unix::process::CommandExt};
    let mut child = Command::new(binary)
        .arg(arg)
        .process_group(0)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    struct Process(std::process::Child);
    impl Drop for Process {
        fn drop(&mut self) {
            // Also close inherited pipes held by descendants of the smoke process.
            unsafe {
                libc::kill(-(self.0.id() as i32), libc::SIGKILL);
            }
            let _ = self.0.wait();
        }
    }
    let mut stdout = child.stdout.take().ok_or("smoke stdout missing")?;
    let mut child = Process(child);
    let fd = stdout.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    require(
        flags >= 0 && unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } >= 0,
        "smoke pipe configuration failed",
    )?;
    let mut bytes = Vec::new();
    let mut status = None;
    let mut eof = false;
    let deadline = Instant::now() + Duration::from_secs(seconds);
    loop {
        let mut buffer = [0u8; 8192];
        match stdout.read(&mut buffer) {
            Ok(0) => eof = true,
            Ok(n) => bytes.extend_from_slice(&buffer[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        }
        require(
            bytes.len() <= 1024 * 1024,
            "offline native smoke output exceeded limit",
        )?;
        if status.is_none() {
            status = child.0.try_wait()?;
        }
        if eof && status.is_some() {
            break;
        }
        if Instant::now() >= deadline {
            return Err("offline native smoke timeout".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    require(
        status.is_some_and(|s| s.success()),
        "offline native smoke failed",
    )?;
    Ok(bytes)
}
fn desktop_smoke(binary: &Path, expected: &Value) -> Result<()> {
    let info = smoke(binary, "--package-info", 20)?;
    require(
        info.len() <= 8192 && strict_json(&info)? == *expected,
        "desktop source/target identity mismatch",
    )?;
    require(
        smoke(binary, "--help", 20)?
            .windows(b"Usage: ai-platform-desktop".len())
            .any(|w| w == b"Usage: ai-platform-desktop"),
        "desktop help smoke failed",
    )
}
fn archive(
    name: &str,
    files: &BTreeMap<String, Vec<u8>>,
    binary: &str,
    kind: &str,
) -> Result<Vec<u8>> {
    if kind == "zip" {
        let mut output = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (path, bytes) in files {
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated)
                .last_modified_time(zip::DateTime::default())
                .unix_permissions(if path == binary { 0o755 } else { 0o644 });
            output.start_file(format!("{name}/{path}"), options)?;
            output.write_all(bytes)?;
        }
        Ok(output.finish()?.into_inner())
    } else {
        require(kind == "tar.gz", "unsupported archive kind")?;
        let gzip = flate2::GzBuilder::new()
            .mtime(0)
            .write(Vec::new(), flate2::Compression::default());
        let mut tar = tar::Builder::new(gzip);
        for (path, bytes) in files {
            let mut header = tar::Header::new_ustar();
            header.set_size(bytes.len() as u64);
            header.set_mode(if path == binary { 0o755 } else { 0o644 });
            header.set_uid(0);
            header.set_gid(0);
            header.set_mtime(0);
            header.set_entry_type(tar::EntryType::Regular);
            header.set_cksum();
            tar.append_data(&mut header, format!("{name}/{path}"), Cursor::new(bytes))?;
        }
        Ok(tar.into_inner()?.finish()?)
    }
}
struct Extraction(PathBuf);
impl Drop for Extraction {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn extracted_smoke(
    payload: &[u8],
    name: &str,
    files: &BTreeMap<String, Vec<u8>>,
    binary: &str,
    kind: &str,
    expected: &Value,
    parent: &Path,
) -> Result<()> {
    let temp = Extraction(parent.join(format!("desktop-extraction-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&temp.0)?;
    let mut extracted = BTreeMap::new();
    if kind == "zip" {
        let mut zip = zip::ZipArchive::new(Cursor::new(payload))?;
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i)?;
            let mode = entry.unix_mode().ok_or("ZIP permission missing")?;
            require(
                !entry.is_dir() && mode & 0o170000 != 0o120000,
                "nonregular archive entry",
            )?;
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes)?;
            require(
                extracted
                    .insert(entry.name()?.into_owned(), (bytes, mode))
                    .is_none(),
                "duplicate archive entry",
            )?;
        }
    } else {
        let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(payload));
        for entry in tar.entries()? {
            let mut entry = entry?;
            require(
                entry.header().entry_type().is_file(),
                "nonregular archive entry",
            )?;
            let path = entry.path()?.to_string_lossy().into_owned();
            let mode = entry.header().mode()?;
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes)?;
            require(
                extracted.insert(path, (bytes, mode)).is_none(),
                "duplicate archive entry",
            )?;
        }
    }
    require(
        extracted.len() == files.len(),
        "archive member count mismatch",
    )?;
    for (path, bytes) in files {
        let full = format!("{name}/{path}");
        let (actual, mode) = extracted.remove(&full).ok_or("archive member missing")?;
        let desired = if path == binary { 0o755 } else { 0o644 };
        require(
            actual == *bytes && mode & 0o777 == desired,
            "archive bytes or permissions differ",
        )?;
        let destination = safe(&temp.0, &full)?;
        std::fs::create_dir_all(destination.parent().ok_or("missing parent")?)?;
        std::fs::write(&destination, &actual)?;
        #[cfg(unix)]
        std::fs::set_permissions(destination, std::fs::Permissions::from_mode(desired))?;
    }
    desktop_smoke(&safe(&temp.0, &format!("{name}/{binary}"))?, expected)
}
fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
pub fn run(args: &Args, desktop: bool) -> Result<Value> {
    let root = std::fs::canonicalize(args.root())?;
    let kind = if desktop { "desktop" } else { "platformctl" };
    let manifest = object(&root.join(format!("deploy/release/{kind}-targets.json")))?;
    require(
        manifest["schema_version"]
            == if desktop {
                "ai-platform-desktop/build-targets/v1"
            } else {
                "platformctl/build-targets/v1"
            },
        "unsupported target manifest",
    )?;
    let targets = manifest["targets"].as_array().ok_or("targets missing")?;
    if desktop {
        let names: std::collections::BTreeSet<_> = targets
            .iter()
            .map(|t| t["name"].as_str().unwrap_or(""))
            .collect();
        require(
            targets.len() == 5
                && names
                    == [
                        "linux-x64",
                        "linux-arm64",
                        "darwin-x64",
                        "darwin-arm64",
                        "win32-x64",
                    ]
                    .into_iter()
                    .collect(),
            "desktop requires five distinct targets",
        )?;
        for target in targets {
            let windows = target["os"] == "windows";
            require(
                target["binary"]
                    == if windows {
                        "ai-platform-desktop.exe"
                    } else {
                        "ai-platform-desktop"
                    }
                    && target["archive"] == if windows { "zip" } else { "tar.gz" },
                "unexpected desktop target binary/archive",
            )?;
        }
    }
    if args.flag("--matrix") {
        require(
            !args.flag("--build-env")
                && ["--target", "--binary", "--output"]
                    .iter()
                    .all(|k| !args.values.contains_key(*k)),
            "matrix cannot be combined with package options",
        )?;
        return Ok(json!({"include":targets}));
    }
    let target = targets
        .iter()
        .find(|t| t["name"].as_str() == args.values.get("--target").map(String::as_str))
        .ok_or("unknown native target")?;
    let source = materials(&root, desktop)?;
    let sha = git(&root, &["rev-parse", "HEAD"])?;
    require(
        regex::Regex::new("^[0-9a-f]{40,64}$")?.is_match(&sha),
        "invalid source commit",
    )?;
    let workspace: toml::Value = std::str::from_utf8(
        source
            .get(&format!("{WS}/Cargo.toml"))
            .ok_or("workspace material missing")?,
    )?
    .parse()?;
    let version = workspace["workspace"]["package"]["version"]
        .as_str()
        .ok_or("version missing")?;
    require(
        regex::Regex::new(r"^[0-9]+\.[0-9]+\.[0-9]+(?:[-+][0-9A-Za-z.-]+)?$")?.is_match(version),
        "invalid native version",
    )?;
    let license = workspace["workspace"]["package"]["license"]
        .as_str()
        .ok_or("license missing")?;
    let hashes: BTreeMap<_, _> = source.iter().map(|(k, v)| (k.clone(), hash(v))).collect();
    let fingerprint = hash(&serde_json::to_vec(&hashes)?);
    if args.flag("--build-env") {
        require(
            desktop
                && !args.values.contains_key("--binary")
                && !args.values.contains_key("--output"),
            "build-env only accepts desktop target",
        )?;
        let mut lines = vec![
            format!("AI_PLATFORM_DESKTOP_SOURCE_COMMIT={sha}"),
            format!("AI_PLATFORM_DESKTOP_SOURCE_DIGEST={fingerprint}"),
            format!(
                "AI_PLATFORM_DESKTOP_RUST_TARGET={}",
                string(target, "rust_target")?
            ),
        ];
        if target["os"] == "macos" {
            lines.push(format!(
                "MACOSX_DEPLOYMENT_TARGET={}",
                string(target, "macos_minimum")?
            ));
        }
        if target["os"] == "windows" {
            lines.push(
                "CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS=-C target-feature=+crt-static"
                    .into(),
            );
        }
        return Ok(json!({"_plain":lines.join("\n")}));
    }
    let output = crate::resolve_path(args.get("--output")?)?;
    require(
        !output.starts_with(&root),
        "package output must be outside repository",
    )?;
    let supplied = Path::new(args.get("--binary")?);
    require(
        !std::fs::symlink_metadata(supplied)?
            .file_type()
            .is_symlink(),
        "binary cannot be symlink",
    )?;
    let binary = std::fs::canonicalize(supplied)?;
    require(
        binary.is_file()
            && binary.file_name().and_then(|s| s.to_str()) == target["binary"].as_str(),
        "binary filename differs from target",
    )?;
    require(
        std::fs::metadata(&binary)?.len() <= 256 * 1024 * 1024,
        "binary exceeds size bound",
    )?;
    let content = std::fs::read(&binary)?;
    machine(&content, target)?;
    let expected = json!({"schema_version":"ai-platform-desktop/build-info/v1","package":"ai-platform-desktop","version":version,"os":target["os"],"arch":target["arch"],"source_commit":sha,"source_digest":fingerprint,"rust_target":target["rust_target"]});
    if desktop {
        desktop_smoke(&binary, &expected)?;
    } else {
        require(
            smoke(&binary, "--help", 15)?
                .windows(b"platformctl --origin".len())
                .any(|w| w == b"platformctl --origin"),
            "platformctl native help failed",
        )?;
    }
    let name = format!(
        "{}-{version}-{}",
        if desktop {
            "ai-platform-desktop"
        } else {
            "platformctl"
        },
        string(target, "name")?
    );
    let binary_name = if desktop && target["os"] == "macos" {
        format!(
            "AI Platform.app/Contents/MacOS/{}",
            string(target, "binary")?
        )
    } else {
        string(target, "binary")?.to_owned()
    };
    let mut receipt = json!({"schema_version":if desktop{"ai-platform-desktop/native-artifact/v1"}else{"platformctl/native-artifact/v1"},"version":version,"source_commit":sha,"target":target["name"],"rust_target":target["rust_target"],"binary":binary_name,"binary_sha256":hash(&content),"native_help_smoke":"passed","crate_license_expression":license,"repository_license_material":"LICENSE.repository","licensing_note":"Repository LICENSE is MIT; native workspace declares Apache-2.0. Preserved without relicensing; reconcile before public release.","materials":hashes});
    if desktop {
        let object = receipt.as_object_mut().unwrap();
        object.extend(json!({"source_digest":fingerprint,"declared_build_baseline":target["declared_build_baseline"],"native_identity_smoke":"passed","extracted_identity_smoke":"passed","extracted_help_smoke":"passed","gui_acceptance":"not_run","authenticated_acceptance":"not_run","publisher_signature":"not_provided","platform_adhoc_signature":"not_assessed","notarization":"not_performed","public_release_eligible":false}).as_object().unwrap().clone());
    } else {
        receipt.as_object_mut().unwrap().extend(json!({"live_api_acceptance":"not_run","signature":"unsigned","notarization":"not_performed"}).as_object().unwrap().clone());
    }
    let receipt_bytes = format!("{}\n", serde_json::to_string_pretty(&receipt)?).into_bytes();
    let readme = format!(
        "{} {version} ({})\nSource commit: {sha}\n\nInternal unsigned candidate. Verify companion .sha256 before extraction.\nChecksums are not publisher signatures. No notarization or publication is supplied.\nKeep extracted files together. No installer, updater, background service or stored credentials are added.\nOffline process checks do not prove GUI rendering, login, live API or provider acceptance.\nNever bypass platform security warnings. Use platformctl --origin and --token-stdin for authenticated CLI commands.\n\nLICENSE.repository preserves repository MIT text; workspace declares Apache-2.0.\nOwner reconciliation and dependency license/notice review are required before public release.\nRead the included capability matrix and desktop distribution documentation for supported surfaces.\n",
        if desktop {
            "AI Platform Desktop"
        } else {
            "platformctl"
        },
        string(target, "name")?
    );
    let mut files = BTreeMap::from([
        (binary_name.clone(), content.clone()),
        ("artifact.json".into(), receipt_bytes.clone()),
        ("README.txt".into(), readme.into_bytes()),
        ("LICENSE.repository".into(), source["LICENSE"].clone()),
        (
            "native-client-capability-matrix.md".into(),
            source["docs/architecture/native-client-capability-matrix.md"].clone(),
        ),
    ]);
    if desktop {
        files.insert(
            "native-desktop-distribution.md".into(),
            source["docs/architecture/native-desktop-distribution.md"].clone(),
        );
        if target["os"] == "macos" {
            let numeric = version.split(['-', '+']).next().unwrap();
            let properties = BTreeMap::from([
                ("CFBundleExecutable", string(target, "binary")?),
                (
                    "CFBundleIdentifier",
                    string(&manifest, "bundle_identifier")?,
                ),
                ("CFBundleName", "AI Platform"),
                ("CFBundleDisplayName", "AI Platform"),
                ("CFBundlePackageType", "APPL"),
                ("CFBundleInfoDictionaryVersion", "6.0"),
                ("CFBundleShortVersionString", numeric),
                ("CFBundleVersion", numeric),
                ("LSMinimumSystemVersion", string(target, "macos_minimum")?),
            ]);
            let mut plist = String::from(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict>\n",
            );
            for (k, v) in properties {
                plist.push_str(&format!("<key>{k}</key><string>{}</string>\n", xml(v)));
            }
            plist.push_str("<key>NSHighResolutionCapable</key><true/>\n</dict></plist>\n");
            files.insert(
                "AI Platform.app/Contents/Info.plist".into(),
                plist.into_bytes(),
            );
            files.insert(
                "AI Platform.app/Contents/Resources/LICENSE.repository".into(),
                source["LICENSE"].clone(),
            );
            files.insert(
                "AI Platform.app/Contents/Resources/artifact.json".into(),
                receipt_bytes.clone(),
            );
        } else if target["os"] == "linux" {
            files.insert("ai-platform-desktop.desktop".into(),b"[Desktop Entry]\nType=Application\nName=AI Platform\nComment=Native AI Platform Gateway client\nExec=ai-platform-desktop\nTryExec=ai-platform-desktop\nTerminal=false\nCategories=Office;Network;\nStartupNotify=false\n".to_vec());
        }
    }
    let kind = string(target, "archive")?;
    let payload = archive(&name, &files, &binary_name, kind)?;
    let filename = format!("{name}.{kind}");
    let artifacts = BTreeMap::from([
        (filename.clone(), payload.clone()),
        (
            format!("{filename}.sha256"),
            format!("{}  {filename}\n", hash(&payload)).into_bytes(),
        ),
        (format!("{name}.artifact.json"), receipt_bytes),
    ]);
    std::fs::create_dir_all(&output)?;
    for path in artifacts.keys() {
        require(!output.join(path).exists(), "output already exists")?;
    }
    if desktop {
        extracted_smoke(
            &payload,
            &name,
            &files,
            &binary_name,
            kind,
            &expected,
            &output,
        )?;
    }
    require(
        materials(&root, desktop)? == source
            && git(&root, &["rev-parse", "HEAD"])? == sha
            && std::fs::read(&binary)? == content,
        "package source or binary changed during smoke",
    )?;
    for (path, bytes) in artifacts {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(output.join(path))?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    Ok(
        json!({"archive":output.join(filename),"source_commit":sha,"target":target["name"],"gui_acceptance":"not_run","publication":"not_performed"}),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[cfg(unix)]
    fn smoke_deadline_includes_inherited_stdout() {
        let temp = Extraction(
            std::env::temp_dir().join(format!("native-package-smoke-{}", uuid::Uuid::new_v4())),
        );
        std::fs::create_dir(&temp.0).unwrap();
        let binary = temp.0.join("fixture");
        std::fs::write(
            &binary,
            "#!/bin/sh\nsleep 30 &\nprintf 'parent exited\\n'\nexit 0\n",
        )
        .unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        let start = Instant::now();
        assert_eq!(
            smoke(&binary, "--help", 1).unwrap_err().to_string(),
            "offline native smoke timeout"
        );
        assert!(start.elapsed() < Duration::from_secs(5));
    }
    #[test]
    fn machines_are_not_accepted_under_wrong_labels() {
        let mut elf = vec![0; 64];
        elf[..6].copy_from_slice(b"\x7fELF\x02\x01");
        elf[18..20].copy_from_slice(&62u16.to_le_bytes());
        assert!(machine(&elf, &json!({"name":"linux-x64","machine":62})).is_ok());
        assert!(machine(&elf, &json!({"name":"linux-arm64","machine":183})).is_err());
        assert!(machine(&elf, &json!({"name":"win32-x64","machine":34404})).is_err());
    }
    #[test]
    fn archives_are_deterministic() {
        let files = BTreeMap::from([
            ("binary".into(), b"native".to_vec()),
            ("info".into(), b"data".to_vec()),
        ]);
        for kind in ["zip", "tar.gz"] {
            assert_eq!(
                archive("package", &files, "binary", kind).unwrap(),
                archive("package", &files, "binary", kind).unwrap()
            );
        }
    }
}

/// Windows pipes cannot use Unix fcntl. Peek the anonymous stdout pipe and
/// enforce the same output/deadline bounds; terminate the smoke process job on
/// every exit so inherited descendant handles cannot keep packaging alive.
/// APIs: learn.microsoft.com/windows/win32/api/jobapi2/nf-jobapi2-assignprocesstojobobject
/// and learn.microsoft.com/windows/win32/api/namedpipeapi/nf-namedpipeapi-peeknamedpipe.
#[cfg(windows)]
fn smoke(binary: &Path, arg: &str, seconds: u64) -> Result<Vec<u8>> {
    use std::{ffi::c_void, os::windows::io::AsRawHandle, ptr};
    type Handle = *mut c_void;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateJobObjectW(attributes: *const c_void, name: *const u16) -> Handle;
        fn AssignProcessToJobObject(job: Handle, process: Handle) -> i32;
        fn TerminateJobObject(job: Handle, exit_code: u32) -> i32;
        fn CloseHandle(handle: Handle) -> i32;
        fn PeekNamedPipe(
            pipe: Handle,
            buffer: *mut c_void,
            buffer_size: u32,
            read: *mut u32,
            available: *mut u32,
            left: *mut u32,
        ) -> i32;
    }
    struct Process {
        child: std::process::Child,
        job: Handle,
    }
    impl Drop for Process {
        fn drop(&mut self) {
            if !self.job.is_null() {
                unsafe {
                    TerminateJobObject(self.job, 1);
                    CloseHandle(self.job);
                }
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
    let child = Command::new(binary)
        .arg(arg)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut process = Process {
        child,
        job: ptr::null_mut(),
    };
    process.job = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
    if process.job.is_null() {
        return Err(std::io::Error::last_os_error().into());
    }
    if unsafe { AssignProcessToJobObject(process.job, process.child.as_raw_handle()) } == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let mut stdout = process.child.stdout.take().ok_or("smoke stdout missing")?;
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut bytes = Vec::new();
    let mut status = None;
    let mut eof = false;
    loop {
        let mut available = 0;
        if unsafe {
            PeekNamedPipe(
                stdout.as_raw_handle(),
                ptr::null_mut(),
                0,
                ptr::null_mut(),
                &mut available,
                ptr::null_mut(),
            )
        } == 0
        {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(109) {
                eof = true;
            } else {
                return Err(error.into());
            }
        } else if available > 0 {
            let mut buffer = [0u8; 8192];
            let length = (available as usize).min(buffer.len());
            let read = stdout.read(&mut buffer[..length])?;
            if read == 0 {
                eof = true;
            } else {
                bytes.extend_from_slice(&buffer[..read]);
            }
        }
        require(
            bytes.len() <= 1024 * 1024,
            "offline native smoke output exceeded limit",
        )?;
        if status.is_none() {
            status = process.child.try_wait()?;
        }
        if eof && status.is_some() {
            break;
        }
        if Instant::now() >= deadline {
            return Err("offline native smoke timeout".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    require(
        status.is_some_and(|s| s.success()),
        "offline native smoke failed",
    )?;
    Ok(bytes)
}
