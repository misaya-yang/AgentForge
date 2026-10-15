//! Active-document version reads and Python difflib-compatible grouped diff.
use super::{Actor, IngestError, IngestionService, Result, object, sha256, string};
use serde_json::{Value, json};
use std::collections::HashMap;
impl IngestionService {
    pub(super) async fn require_active_document(&self, dataset: &Value, id: &str) -> Result<Value> {
        let ds = string(dataset, "dataset_id").trim();
        let tenant = string(dataset, "tenant_id").trim();
        let id = id.trim();
        if ds.is_empty()
            || tenant.is_empty()
            || id.is_empty()
            || !self
                .store
                .filter_active_document_ids(ds, tenant, &[id.to_owned()])
                .await?
                .contains(id)
        {
            return Err(IngestError::Validation("Document not found".into()));
        }
        self.store.get_document(ds, id).await
    }
    pub async fn list_document_versions(
        &self,
        actor: &Actor,
        dataset: &str,
        document: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Value> {
        let ds = self
            .store
            .require_dataset_access(actor, dataset, "viewer")
            .await?;
        let generation = super::document_reads::generation(&ds)?;
        let doc = self.require_active_document(&ds, document).await?;
        let versions = self.store.list_versions(document, limit, offset).await?;
        let total:i64=sqlx::query_scalar("SELECT COUNT(*) FROM knowledge.document_versions WHERE document_id=$1 AND change_type NOT IN ('pending_restore','pending_before_restore')").bind(document).fetch_one(&self.store.pool).await?;
        self.require_read_generation(actor, dataset, &generation)
            .await?;
        Ok(
            json!({"versions":versions,"total":total,"current_version":doc.get("current_version").cloned().unwrap_or(json!(1))}),
        )
    }
    pub async fn get_document_version(
        &self,
        actor: &Actor,
        dataset: &str,
        document: &str,
        version: i32,
    ) -> Result<Value> {
        let ds = self
            .store
            .require_dataset_access(actor, dataset, "viewer")
            .await?;
        let generation = super::document_reads::generation(&ds)?;
        self.require_active_document(&ds, document).await?;
        let version = self
            .store
            .get_version(document, version, false)
            .await
            .map_err(|e| {
                if e == IngestError::NotFound {
                    IngestError::Validation(format!("Version {version} not found"))
                } else {
                    e
                }
            })?;
        self.require_read_generation(actor, dataset, &generation)
            .await?;
        Ok(version)
    }
    pub async fn historical_document_source(
        &self,
        actor: &Actor,
        dataset: &str,
        document: &str,
        version: i32,
        hash: &str,
    ) -> Result<Value> {
        let hash = hash.trim().to_lowercase();
        if version <= 0 || hash.len() != 64 || !hash.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err(IngestError::NotFound);
        }
        let ds = self
            .store
            .require_dataset_access(actor, dataset, "viewer")
            .await?;
        let generation = super::document_reads::generation(&ds)?;
        self.require_active_document(&ds, document).await?;
        let row = self.store.get_version(document, version, false).await?;
        let content = row["content"].as_str().ok_or(IngestError::NotFound)?;
        if string(&row, "change_type").starts_with("pending_")
            || string(&row, "content_hash").to_lowercase() != hash
            || sha256(content.as_bytes()) != hash
        {
            return Err(IngestError::NotFound);
        }
        self.require_read_generation(actor, dataset, &generation)
            .await?;
        self.require_active_document(&ds, document).await?;
        Ok(
            json!({"document_id":document,"version_number":version,"source_hash":hash,"title":string(&row,"title"),"content":content}),
        )
    }
    pub async fn compare_document_versions(
        &self,
        actor: &Actor,
        dataset: &str,
        document: &str,
        from: i32,
        to: i32,
    ) -> Result<Value> {
        let ds = self
            .store
            .require_dataset_access(actor, dataset, "viewer")
            .await?;
        let generation = super::document_reads::generation(&ds)?;
        self.require_active_document(&ds, document).await?;
        let a = self
            .store
            .get_version(document, from, false)
            .await
            .map_err(|e| {
                if e == IngestError::NotFound {
                    IngestError::Validation(format!("Version {from} not found"))
                } else {
                    e
                }
            })?;
        let b = self
            .store
            .get_version(document, to, false)
            .await
            .map_err(|e| {
                if e == IngestError::NotFound {
                    IngestError::Validation(format!("Version {to} not found"))
                } else {
                    e
                }
            })?;
        let a = string(&a, "content").to_owned();
        let b = string(&b, "content").to_owned();
        let (diff, additions, deletions) =
            tokio::task::spawn_blocking(move || version_diff(&a, &b))
                .await
                .map_err(|_| IngestError::Storage)?;
        self.require_read_generation(actor, dataset, &generation)
            .await?;
        Ok(
            json!({"from_version":from,"to_version":to,"diff":diff,"stats":{"additions":additions,"deletions":deletions,"changes":additions+deletions}}),
        )
    }
}
fn split_lines(text: &str) -> Vec<String> {
    let mut out = vec![];
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if matches!(
            c,
            '\n' | '\r'
                | '\u{b}'
                | '\u{c}'
                | '\u{1c}'
                | '\u{1d}'
                | '\u{1e}'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
        ) {
            let mut end = i + c.len_utf8();
            if c == '\r' && chars.peek().is_some_and(|(_, c)| *c == '\n') {
                end = chars.next().unwrap().0 + 1;
            }
            out.push(text[start..end].to_owned());
            start = end;
        }
    }
    if start < text.len() {
        out.push(text[start..].to_owned())
    }
    out
}
#[derive(Clone, Copy)]
struct Op {
    tag: &'static str,
    a: usize,
    ae: usize,
    b: usize,
    be: usize,
}
/// Ratcliff/Obershelp matching uses the same earliest-match tie break and
/// automatic popular-line removal as SequenceMatcher(autojunk=True).
pub fn version_diff(before: &str, after: &str) -> (Vec<Value>, usize, usize) {
    let a = split_lines(before);
    let b = split_lines(after);
    let mut positions: HashMap<&str, Vec<usize>> = HashMap::new();
    for (j, line) in b.iter().enumerate() {
        positions.entry(line).or_default().push(j);
    }
    if b.len() >= 200 {
        let cap = b.len() / 100 + 1;
        positions.retain(|_, indices| indices.len() <= cap);
    }
    let mut queue = vec![(0, a.len(), 0, b.len())];
    let mut blocks = vec![];
    while let Some((alo, ahi, blo, bhi)) = queue.pop() {
        let (mut ai, mut bj, mut size) = (alo, blo, 0);
        let mut previous = HashMap::new();
        for i in alo..ahi {
            let mut current = HashMap::new();
            if let Some(indices) = positions.get(a[i].as_str()) {
                for &j in indices {
                    if j < blo {
                        continue;
                    }
                    if j >= bhi {
                        break;
                    }
                    let k = 1 + j
                        .checked_sub(1)
                        .and_then(|j| previous.get(&j))
                        .copied()
                        .unwrap_or(0usize);
                    current.insert(j, k);
                    if k > size {
                        ai = i + 1 - k;
                        bj = j + 1 - k;
                        size = k;
                    }
                }
            }
            previous = current;
        }
        while ai > alo && bj > blo && a[ai - 1] == b[bj - 1] {
            ai -= 1;
            bj -= 1;
            size += 1;
        }
        while ai + size < ahi && bj + size < bhi && a[ai + size] == b[bj + size] {
            size += 1;
        }
        if size > 0 {
            blocks.push((ai, bj, size));
            if alo < ai && blo < bj {
                queue.push((alo, ai, blo, bj));
            }
            if ai + size < ahi && bj + size < bhi {
                queue.push((ai + size, ahi, bj + size, bhi));
            }
        }
    }
    blocks.sort_unstable();
    let mut merged: Vec<(usize, usize, usize)> = vec![];
    for block in blocks {
        if let Some(last) = merged.last_mut() {
            if last.0 + last.2 == block.0 && last.1 + last.2 == block.1 {
                last.2 += block.2;
                continue;
            }
        }
        merged.push(block)
    }
    merged.push((a.len(), b.len(), 0));
    let mut ops = vec![];
    let (mut i, mut j) = (0, 0);
    for (ai, bj, n) in merged {
        let tag = if i < ai && j < bj {
            "replace"
        } else if i < ai {
            "delete"
        } else if j < bj {
            "insert"
        } else {
            ""
        };
        if !tag.is_empty() {
            ops.push(Op {
                tag,
                a: i,
                ae: ai,
                b: j,
                be: bj,
            })
        }
        if n > 0 {
            ops.push(Op {
                tag: "equal",
                a: ai,
                ae: ai + n,
                b: bj,
                be: bj + n,
            })
        }
        i = ai + n;
        j = bj + n;
    }
    if let Some(first) = ops.first_mut() {
        if first.tag == "equal" {
            first.a = first.a.max(first.ae.saturating_sub(3));
            first.b = first.b.max(first.be.saturating_sub(3));
        }
    }
    if let Some(last) = ops.last_mut() {
        if last.tag == "equal" {
            last.ae = last.ae.min(last.a + 3);
            last.be = last.be.min(last.b + 3);
        }
    }
    let mut groups = vec![];
    let mut group = vec![];
    for mut op in ops {
        if op.tag == "equal" && op.ae - op.a > 6 {
            group.push(Op {
                ae: op.a + 3,
                be: op.b + 3,
                ..op
            });
            groups.push(std::mem::take(&mut group));
            op.a = op.ae - 3;
            op.b = op.be - 3;
        }
        group.push(op)
    }
    if group.iter().any(|op| op.tag != "equal") {
        groups.push(group)
    }
    let (mut out, mut additions, mut deletions) = (vec![], 0, 0);
    let mut emit = |tag: &str, line: &str| {
        let prefix = match tag {
            "insert" => '+',
            "delete" => '-',
            _ => ' ',
        };
        let formatted = format!("{prefix}{line}");
        if formatted.starts_with("+++")
            || formatted.starts_with("---")
            || formatted.starts_with("@@")
        {
            return;
        }
        if tag == "insert" {
            additions += 1;
        } else if tag == "delete" {
            deletions += 1;
        }
        if out.len() < 500 {
            out.push(json!({"type":tag,"content":if tag=="equal"{formatted}else{line.to_owned()}}));
        }
    };
    for group in groups {
        for op in group {
            if op.tag == "equal" {
                for line in &a[op.a..op.ae] {
                    emit("equal", line)
                }
            } else {
                if ["replace", "delete"].contains(&op.tag) {
                    for line in &a[op.a..op.ae] {
                        emit("delete", line)
                    }
                }
                if ["replace", "insert"].contains(&op.tag) {
                    for line in &b[op.b..op.be] {
                        emit("insert", line)
                    }
                }
            }
        }
    }
    (out, additions, deletions)
}
