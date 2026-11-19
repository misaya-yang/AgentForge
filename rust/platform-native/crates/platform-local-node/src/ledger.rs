use crate::{
    Error, Result, canonical, digest,
    envelope::{Action, PlatformVerifier},
    sanitize,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::Path;
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Record {
    pub action_id: String,
    pub idempotency_key: String,
    pub arguments_digest: String,
    pub envelope_digest: String,
    pub status: String,
    pub result: Option<Value>,
}
pub struct Ledger {
    db: Connection,
    path: std::path::PathBuf,
}
pub fn terminal(s: &str) -> bool {
    matches!(
        s,
        "succeeded" | "failed" | "cancelled" | "interrupted" | "unknown"
    )
}
fn transition_allowed(before: &str, after: &str) -> bool {
    match before {
        "policy_check" | "awaiting_approval" => {
            matches!(after, "dispatched" | "failed" | "cancelled" | "interrupted")
                || (before == "policy_check" && after == "awaiting_approval")
        }
        "dispatched" => after == "running" || after == "observed" || terminal(after),
        "running" => after == "observed" || terminal(after),
        "observed" => terminal(after),
        _ => false,
    }
}
fn record(row: &rusqlite::Row<'_>) -> rusqlite::Result<Record> {
    let raw: Option<String> = row.get(5)?;
    Ok(Record {
        action_id: row.get(0)?,
        idempotency_key: row.get(1)?,
        arguments_digest: row.get(2)?,
        envelope_digest: row.get(3)?,
        status: row.get(4)?,
        result: raw
            .map(|s| serde_json::from_str(&s))
            .transpose()
            .map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    5,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?,
    })
}
const SELECT: &str = "SELECT action_id,idempotency_key,arguments_digest,envelope_digest,status,result_json FROM actions";
fn append(db: &Connection, id: &str, status: &str, payload: &Value) -> Result<()> {
    let prev: String = db.query_row("SELECT head_hash FROM ledger_meta WHERE id=1", [], |r| {
        r.get(0)
    })?;
    let at = crate::now();
    let payload = canonical(&sanitize(payload));
    let hash = digest(
        &json!({"action_id":id,"event_type":status,"payload":payload,"created_at":at,"prev_hash":prev}),
    );
    db.execute("INSERT INTO events(action_id,event_type,payload_json,created_at,prev_hash,event_hash) VALUES(?,?,?,?,?,?)",params![id,status,payload,at,prev,hash])?;
    db.execute(
        "UPDATE ledger_meta SET event_count=event_count+1,head_hash=? WHERE id=1",
        [hash],
    )?;
    Ok(())
}
impl Ledger {
    pub fn open(path: &Path) -> Result<Self> {
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()?.join(path)
        };
        let db = Connection::open(&path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        }
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
CREATE TABLE IF NOT EXISTS actions(action_id TEXT PRIMARY KEY,idempotency_key TEXT UNIQUE NOT NULL,arguments_digest TEXT NOT NULL,envelope_digest TEXT NOT NULL,status TEXT NOT NULL,result_json TEXT,created_at REAL NOT NULL,updated_at REAL NOT NULL);
CREATE TABLE IF NOT EXISTS events(seq INTEGER PRIMARY KEY AUTOINCREMENT,action_id TEXT NOT NULL,event_type TEXT NOT NULL,payload_json TEXT NOT NULL,created_at REAL NOT NULL,prev_hash TEXT NOT NULL,event_hash TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS ledger_meta(id INTEGER PRIMARY KEY CHECK(id=1),event_count INTEGER NOT NULL,head_hash TEXT NOT NULL);")?;
        let columns = db
            .prepare("PRAGMA table_info(actions)")?
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if !columns.iter().any(|v| v == "envelope_digest") {
            db.execute_batch("ALTER TABLE actions ADD COLUMN envelope_digest TEXT NOT NULL DEFAULT 'legacy-unbound'")?;
        }
        db.execute(
            "INSERT OR IGNORE INTO ledger_meta VALUES(1,0,?)",
            ["0".repeat(64)],
        )?;
        let ledger = Self { db, path };
        ledger.verify()?;
        Ok(ledger)
    }
    /// Open an independently serialized connection to the same private ledger.
    /// This is used by the control poller so it can durably cancel an action
    /// while the single ordinary dispatcher is awaiting owner approval.
    pub fn fork(&self) -> Result<Self> {
        Self::open(&self.path)
    }
    pub fn get(&self, id: &str) -> Result<Option<Record>> {
        Ok(self
            .db
            .query_row(&format!("{SELECT} WHERE action_id=?"), [id], record)
            .optional()?)
    }
    pub fn begin(&mut self, a: &Action, verifier: &dyn PlatformVerifier) -> Result<(bool, Record)> {
        a.verify(verifier, crate::now())?;
        let envelope = digest(&a.signed_payload());
        let tx = self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut q = tx.prepare(&format!("{SELECT} WHERE action_id=? OR idempotency_key=?"))?;
        let rows = q
            .query_map(params![a.action_id, a.idempotency_key], record)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(q);
        if !rows.is_empty() {
            if rows.len() != 1 {
                return Err(Error::Conflict);
            }
            let r = rows.into_iter().next().unwrap();
            if r.action_id != a.action_id
                || r.idempotency_key != a.idempotency_key
                || r.arguments_digest != a.arguments_digest
                || r.envelope_digest != envelope
            {
                return Err(Error::Conflict);
            }
            tx.commit()?;
            return Ok((false, r));
        }
        let at = crate::now();
        tx.execute("INSERT INTO actions(action_id,idempotency_key,arguments_digest,envelope_digest,status,result_json,created_at,updated_at) VALUES(?,?,?,?,?,NULL,?,?)",params![a.action_id,a.idempotency_key,a.arguments_digest,envelope,"policy_check",at,at])?;
        append(
            &tx,
            &a.action_id,
            "policy_check",
            &json!({"envelope_digest":envelope,"arguments_digest":a.arguments_digest,"tenant_id":a.tenant_id,"user_id":a.user_id,"device_id":a.device_id}),
        )?;
        tx.commit()?;
        Ok((true, self.get(&a.action_id)?.ok_or(Error::Integrity)?))
    }
    pub fn transition(&mut self, id: &str, status: &str, result: Option<Value>) -> Result<Record> {
        let tx = self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut before = tx
            .query_row(&format!("{SELECT} WHERE action_id=?"), [id], record)
            .optional()?
            .ok_or(Error::Conflict)?;
        let result = result.map(|v| sanitize(&v));
        if terminal(&before.status) {
            if before.status == status && before.result == result {
                return Ok(before);
            }
            return Err(Error::Conflict);
        }
        if !transition_allowed(&before.status, status) {
            return Err(Error::Conflict);
        }
        tx.execute(
            "UPDATE actions SET status=?,result_json=?,updated_at=? WHERE action_id=?",
            params![status, result.as_ref().map(canonical), crate::now(), id],
        )?;
        append(&tx, id, status, &result.clone().unwrap_or(json!({})))?;
        tx.commit()?;
        before.status = status.into();
        before.result = result;
        Ok(before)
    }
    pub fn disconnect(&mut self) -> Result<Vec<String>> {
        self.disconnect_except(&std::collections::HashSet::new())
    }
    pub fn disconnect_except(
        &mut self,
        exclude: &std::collections::HashSet<String>,
    ) -> Result<Vec<String>> {
        let mut q=self.db.prepare("SELECT action_id,status FROM actions WHERE status IN ('policy_check','awaiting_approval','dispatched','running','observed')")?;
        let rows = q
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(q);
        let mut ids = Vec::new();
        for (id, status) in rows {
            if exclude.contains(&id) {
                continue;
            }
            let unknown = !matches!(status.as_str(), "policy_check" | "awaiting_approval");
            self.transition(&id,if unknown {"unknown"} else {"interrupted"},Some(json!({"error_code":"transport_disconnected","replay_allowed":false,"side_effect_unknown":unknown})))?;
            ids.push(id);
        }
        Ok(ids)
    }
    pub fn verify(&self) -> Result<()> {
        let mut prev = "0".repeat(64);
        let mut count = 0;
        let mut latest = std::collections::HashMap::new();
        let mut bindings = std::collections::HashMap::new();
        let mut stmt=self.db.prepare("SELECT action_id,event_type,payload_json,created_at,prev_hash,event_hash FROM events ORDER BY seq")?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            let id: String = r.get(0)?;
            let status: String = r.get(1)?;
            let payload: String = r.get(2)?;
            let at: f64 = r.get(3)?;
            let p: String = r.get(4)?;
            let hash: String = r.get(5)?;
            if p != prev
                || hash
                    != digest(
                        &json!({"action_id":id,"event_type":status,"payload":payload,"created_at":at,"prev_hash":p}),
                    )
            {
                return Err(Error::Integrity);
            }
            let v: Value = serde_json::from_str(&payload).map_err(|_| Error::Integrity)?;
            if status == "policy_check" {
                if bindings
                    .insert(
                        id.clone(),
                        (v["envelope_digest"].clone(), v["arguments_digest"].clone()),
                    )
                    .is_some()
                {
                    return Err(Error::Integrity);
                }
            }
            latest.insert(id, (status, v));
            prev = hash;
            count += 1;
        }
        let (sealed, head): (i64, String) = self.db.query_row(
            "SELECT event_count,head_hash FROM ledger_meta WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if sealed != count || head != prev {
            return Err(Error::Integrity);
        }
        let mut actions = self.db.prepare(SELECT)?;
        for r in actions.query_map([], record)? {
            let r = r?;
            // Legacy receipts remain readable but can never match a signed envelope in begin.
            // Their historic records predate envelope binding; the event chain is still verified above.
            if r.envelope_digest == "legacy-unbound" {
                latest.remove(&r.action_id);
                bindings.remove(&r.action_id);
                continue;
            }
            let (status, payload) = latest.remove(&r.action_id).ok_or(Error::Integrity)?;
            let binding = bindings.remove(&r.action_id).ok_or(Error::Integrity)?;
            if binding != (json!(r.envelope_digest), json!(r.arguments_digest)) {
                return Err(Error::Integrity);
            }
            if status != r.status || r.result.as_ref().is_some_and(|v| v != &payload) {
                return Err(Error::Integrity);
            }
        }
        if !latest.is_empty() {
            return Err(Error::Integrity);
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transitions_fail_closed() {
        assert!(!transition_allowed("unknown", "running"));
        assert!(!transition_allowed("policy_check", "succeeded"));
        assert!(transition_allowed("running", "unknown"));
    }
    #[test]
    fn redacts_before_storage() {
        assert_eq!(
            sanitize(&json!({"stdout":"hello","nested":{"token":"secret"}})),
            json!({"stdout":"[REDACTED]","nested":{"token":"[REDACTED]"}})
        );
    }
}
