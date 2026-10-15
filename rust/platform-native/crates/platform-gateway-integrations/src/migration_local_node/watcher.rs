use super::{
    Result,
    files::LocalFileService,
    grants::identity,
    models::{now, sha256},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    io::Read,
    sync::Arc,
};
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WatchEvent {
    pub sequence: u64,
    pub kind: String,
    pub relative_path: String,
    pub previous_path: Option<String>,
    pub sha256: Option<String>,
    pub size: Option<u64>,
    pub observed_at: f64,
}
#[derive(Clone, PartialEq, Eq)]
struct Snapshot {
    device: u64,
    inode: u64,
    size: u64,
    digest: String,
}
pub struct DirectoryWatcher {
    files: Arc<LocalFileService>,
    grant_id: String,
    snapshot: Option<BTreeMap<String, Snapshot>>,
    sequence: u64,
}
impl DirectoryWatcher {
    pub fn new(files: Arc<LocalFileService>, grant_id: String) -> Result<Self> {
        files.grants.get(&grant_id, "watch", None)?;
        Ok(Self {
            files,
            grant_id,
            snapshot: None,
            sequence: 0,
        })
    }
    fn capture(&self) -> Result<BTreeMap<String, Snapshot>> {
        let workspace = self.files.workspace(&self.grant_id, "watch", None)?;
        let mut result = BTreeMap::new();
        for entry in self.files.list_files(&self.grant_id, ".", true)? {
            if entry.kind != "file" {
                continue;
            }
            let capture = (|| {
                let file = workspace.open_read(&entry.relative_path)?;
                let info = file.metadata()?;
                let mut bytes = Vec::new();
                file.take(self.files.max_read_bytes as u64 + 1)
                    .read_to_end(&mut bytes)?;
                if bytes.len() > self.files.max_read_bytes {
                    return Ok(None);
                }
                let (device, inode) = identity(&info)?;
                Ok::<_, super::LocalNodeError>(Some(Snapshot {
                    device,
                    inode,
                    size: info.len(),
                    digest: sha256(&bytes),
                }))
            })();
            if let Ok(Some(snapshot)) = capture {
                result.insert(entry.relative_path, snapshot);
            }
        }
        Ok(result)
    }
    fn event(
        &mut self,
        kind: &str,
        path: &str,
        previous: Option<String>,
        item: Option<&Snapshot>,
    ) -> WatchEvent {
        self.sequence += 1;
        WatchEvent {
            sequence: self.sequence,
            kind: kind.into(),
            relative_path: path.into(),
            previous_path: previous,
            sha256: item.map(|v| v.digest.clone()),
            size: item.map(|v| v.size),
            observed_at: now(),
        }
    }
    pub fn scan_once(&mut self) -> Result<Vec<WatchEvent>> {
        let current = self.capture()?;
        let Some(previous) = self.snapshot.take() else {
            self.snapshot = Some(current);
            return Ok(Vec::new());
        };
        let mut deleted: BTreeSet<_> = previous
            .keys()
            .filter(|p| !current.contains_key(*p))
            .cloned()
            .collect();
        let created: Vec<_> = current
            .keys()
            .filter(|p| !previous.contains_key(*p))
            .cloned()
            .collect();
        let mut deleted_by_inode: HashMap<_, _> = deleted
            .iter()
            .map(|p| ((previous[p].device, previous[p].inode), p.clone()))
            .collect();
        let mut events = Vec::new();
        for path in created {
            let item = &current[&path];
            if let Some(old) = deleted_by_inode.remove(&(item.device, item.inode)) {
                deleted.remove(&old);
                events.push(self.event("rename", &path, Some(old), Some(item)));
            } else {
                events.push(self.event("create", &path, None, Some(item)));
            }
        }
        for path in deleted {
            events.push(self.event("delete", &path, None, None));
        }
        for (path, item) in &current {
            if previous.get(path).is_some_and(|p| p != item) {
                events.push(self.event("modify", path, None, Some(item)));
            }
        }
        self.snapshot = Some(current);
        Ok(events)
    }
}
