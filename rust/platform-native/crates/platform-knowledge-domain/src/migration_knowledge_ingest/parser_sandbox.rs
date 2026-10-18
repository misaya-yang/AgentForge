//! Credential-free converter boundary. Production selects an immutable parser
//! rootfs and a trusted namespace launcher; source bytes are the only writable mount.
use super::{IngestError, Result};
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsString,
    path::{Component, Path, PathBuf},
    process::Command,
};
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SandboxPolicy {
    pub launcher: PathBuf,
    pub rootfs: PathBuf,
    pub scratch: PathBuf,
}
impl SandboxPolicy {
    pub fn validate(&self) -> Result<()> {
        if !cfg!(target_os = "linux")
            || [&self.launcher, &self.rootfs, &self.scratch]
                .iter()
                .any(|p| {
                    !p.is_absolute() || p.components().any(|c| matches!(c, Component::ParentDir))
                })
            || self.rootfs == Path::new("/")
            || self.scratch == Path::new("/")
            || self.rootfs == self.scratch
            || self.rootfs.starts_with(&self.scratch)
            || self.scratch.starts_with(&self.rootfs)
        {
            return Err(IngestError::Validation(
                "parser sandbox requires Linux, a separate rootfs and dedicated scratch".into(),
            ));
        }
        Ok(())
    }
    pub fn arguments(
        &self,
        executable: &Path,
        args: &[OsString],
        directory: &Path,
    ) -> Result<Vec<OsString>> {
        self.validate()?;
        if !executable.is_absolute()
            || executable
                .components()
                .any(|c| matches!(c, Component::ParentDir))
            || !directory.starts_with(&self.scratch)
        {
            return Err(IngestError::Validation(
                "parser operation escaped sandbox policy".into(),
            ));
        }
        let mut result: Vec<OsString> = [
            "--unshare-all",
            "--die-with-parent",
            "--new-session",
            "--cap-drop",
            "ALL",
            "--ro-bind",
        ]
        .into_iter()
        .map(Into::into)
        .collect();
        result.extend([
            self.rootfs.as_os_str().to_owned(),
            "/".into(),
            "--tmpfs".into(),
            "/tmp".into(),
            "--proc".into(),
            "/proc".into(),
            "--dev".into(),
            "/dev".into(),
            "--bind".into(),
            directory.as_os_str().to_owned(),
            "/work".into(),
            "--chdir".into(),
            "/work".into(),
            "--clearenv".into(),
            "--setenv".into(),
            "LANG".into(),
            "C.UTF-8".into(),
            "--setenv".into(),
            "HOME".into(),
            "/tmp".into(),
            "--setenv".into(),
            "TMPDIR".into(),
            "/tmp".into(),
            "--".into(),
            executable.as_os_str().to_owned(),
        ]);
        for arg in args {
            let path = Path::new(arg);
            if let Ok(relative) = path.strip_prefix(directory) {
                result.push(Path::new("/work").join(relative).into_os_string());
            } else {
                result.push(arg.clone());
            }
        }
        Ok(result)
    }
    pub fn executable(&self, path: &Path) -> Result<PathBuf> {
        self.validate()?;
        if !path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir)) {
            return Err(IngestError::Permission);
        }
        let root = self
            .rootfs
            .canonicalize()
            .map_err(|_| IngestError::Parser("parser rootfs unavailable".into()))?;
        let inner = root.join(
            path.strip_prefix("/")
                .map_err(|_| IngestError::Permission)?,
        );
        let resolved = inner
            .canonicalize()
            .map_err(|_| IngestError::Parser("parser tool missing from rootfs".into()))?;
        if !resolved.starts_with(&root) || !resolved.is_file() {
            return Err(IngestError::Permission);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if resolved
                .metadata()
                .map_err(|_| IngestError::Permission)?
                .permissions()
                .mode()
                & 0o111
                == 0
            {
                return Err(IngestError::Permission);
            }
        }
        Ok(path.to_owned())
    }
    pub fn command(
        &self,
        executable: &Path,
        args: &[OsString],
        directory: &Path,
    ) -> Result<Command> {
        let executable = self.executable(executable)?;
        let arguments = self.arguments(&executable, args, directory)?;
        // Do not follow an operator mistake into mounting an ancestor's secrets.
        let root = self
            .rootfs
            .canonicalize()
            .map_err(|_| IngestError::Parser("parser rootfs unavailable".into()))?;
        let scratch = self
            .scratch
            .canonicalize()
            .map_err(|_| IngestError::Parser("parser scratch unavailable".into()))?;
        let current = directory
            .canonicalize()
            .map_err(|_| IngestError::Parser("parser workspace unavailable".into()))?;
        if root != self.rootfs || scratch != self.scratch || !current.starts_with(&scratch) {
            return Err(IngestError::Permission);
        }
        let mut command = Command::new(&self.launcher);
        command.args(arguments).env_clear();
        Ok(command)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn namespace_plan_never_mounts_host_root_or_inherits_environment() {
        let p = SandboxPolicy {
            launcher: "/usr/bin/bwrap".into(),
            rootfs: "/opt/parser-rootfs".into(),
            scratch: "/scratch/parser".into(),
        };
        if cfg!(target_os = "linux") {
            let args = p
                .arguments(
                    Path::new("/usr/bin/pdftotext"),
                    &[],
                    Path::new("/scratch/parser/job"),
                )
                .unwrap();
            let args: Vec<_> = args.iter().map(|v| v.to_string_lossy()).collect();
            assert!(args.contains(&"--unshare-all".into()));
            assert!(args.contains(&"--clearenv".into()));
            assert!(!args.contains(&"--share-net".into()));
            assert!(!args.contains(&"/etc".into()));
            assert!(
                p.arguments(Path::new("/bin/tool"), &[], Path::new("/private"))
                    .is_err()
            );
        }
        let mut invalid = p;
        invalid.rootfs = "/".into();
        assert!(invalid.validate().is_err());
    }
}
