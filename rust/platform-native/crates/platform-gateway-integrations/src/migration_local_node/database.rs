use super::{LocalNodeError as Error, Result};
use rusqlite::Connection;
use std::{
    fs,
    path::Path,
    sync::{Mutex, MutexGuard},
};
pub fn private_directory(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}
pub fn open(path: &Path) -> Result<Mutex<Connection>> {
    if let Some(parent) = path.parent() {
        private_directory(parent)?;
    }
    if let Ok(m) = fs::symlink_metadata(path) {
        if m.file_type().is_symlink() || !m.is_file() {
            return Err(Error::path("state database must be a regular file"));
        }
    }
    let db = Connection::open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA secure_delete=ON;")?;
    Ok(Mutex::new(db))
}
pub fn lock(db: &Mutex<Connection>) -> Result<MutexGuard<'_, Connection>> {
    db.lock()
        .map_err(|_| Error::new("ledger_integrity_error", "local database lock unavailable"))
}
