//! Canonical SQLite action index and redacted, hash-chained receipt ledger.
use super::{LocalNodeError as Error, Result, database, models::*};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ActionRecord {
    pub action_id: String,
    pub idempotency_key: String,
    pub arguments_digest: String,
    pub envelope_digest: String,
    pub status: ActionStatus,
    pub result: Option<Value>,
}
pub struct BeginResult {
    pub created: bool,
    pub record: ActionRecord,
}
pub struct ActionLedger {
    pub path: PathBuf,
    pub platform_signature_verifier: Option<Arc<dyn PlatformSignatureVerifier>>,
    pub trusted_local_approval_verifier: Option<Arc<dyn TrustedLocalApprovalVerifier>>,
    db: Mutex<Connection>,
}
const SELECT: &str = "SELECT action_id,idempotency_key,arguments_digest,envelope_digest,status,result_json FROM actions";
fn conflict(message: &str) -> Error {
    Error::new("idempotency_conflict", message)
}
fn integrity(message: &str) -> Error {
    Error::new("ledger_integrity_error", message)
}
pub fn sanitize_payload(value: &Value, key: &str) -> Value {
    let lower = key.to_lowercase();
    if [
        "secret",
        "token",
        "password",
        "passwd",
        "credential",
        "authorization",
        "cookie",
        "private_key",
        "error_detail",
    ]
    .iter()
    .any(|part| lower.contains(part))
        || ["stdout", "stderr", "content", "raw_content"].contains(&lower.as_str())
    {
        return json!("[REDACTED]");
    }
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), sanitize_payload(v, k)))
                .collect(),
        ),
        Value::Array(list) => Value::Array(list.iter().map(|v| sanitize_payload(v, "")).collect()),
        Value::String(s)
            if [
                "-----begin private key-----",
                "-----begin rsa private key-----",
                "bearer ",
                "sk-",
                "ghp_",
                "secret_canary",
                "secret-canary",
            ]
            .iter()
            .any(|m| s.to_lowercase().contains(m)) =>
        {
            json!("[REDACTED]")
        }
        _ => value.clone(),
    }
}
fn record(row: &rusqlite::Row<'_>) -> rusqlite::Result<ActionRecord> {
    let status: String = row.get(4)?;
    let raw: Option<String> = row.get(5)?;
    let parse_error = |index| {
        rusqlite::Error::InvalidColumnType(index, "JSON".into(), rusqlite::types::Type::Text)
    };
    Ok(ActionRecord {
        action_id: row.get(0)?,
        idempotency_key: row.get(1)?,
        arguments_digest: row.get(2)?,
        envelope_digest: row.get(3)?,
        status: serde_json::from_value(json!(status)).map_err(|_| parse_error(4))?,
        result: raw
            .map(|s| serde_json::from_str(&s))
            .transpose()
            .map_err(|_| parse_error(5))?,
    })
}
impl ActionLedger {
    pub fn open(
        path: &Path,
        platform: Option<Arc<dyn PlatformSignatureVerifier>>,
        approval: Option<Arc<dyn TrustedLocalApprovalVerifier>>,
    ) -> Result<Self> {
        let db = database::open(path)?;
        {
            let conn = database::lock(&db)?;
            conn.execute_batch("CREATE TABLE IF NOT EXISTS actions(action_id TEXT PRIMARY KEY,idempotency_key TEXT UNIQUE NOT NULL,arguments_digest TEXT NOT NULL,envelope_digest TEXT NOT NULL,status TEXT NOT NULL,result_json TEXT,created_at REAL NOT NULL,updated_at REAL NOT NULL); CREATE TABLE IF NOT EXISTS events(seq INTEGER PRIMARY KEY AUTOINCREMENT,action_id TEXT NOT NULL,event_type TEXT NOT NULL,payload_json TEXT NOT NULL,created_at REAL NOT NULL,prev_hash TEXT NOT NULL,event_hash TEXT NOT NULL); CREATE TABLE IF NOT EXISTS ledger_meta(id INTEGER PRIMARY KEY CHECK(id=1),event_count INTEGER NOT NULL,head_hash TEXT NOT NULL);")?;
            let columns = conn
                .prepare("PRAGMA table_info(actions)")?
                .query_map([], |r| r.get::<_, String>(1))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            if !columns.iter().any(|v| v == "envelope_digest") {
                conn.execute_batch("ALTER TABLE actions ADD COLUMN envelope_digest TEXT NOT NULL DEFAULT 'legacy-unbound'")?;
            }
            conn.execute("INSERT OR IGNORE INTO ledger_meta VALUES(1,(SELECT COUNT(*) FROM events),COALESCE((SELECT event_hash FROM events ORDER BY seq DESC LIMIT 1),?1))",["0".repeat(64)])?;
        }
        Ok(Self {
            path: path.to_owned(),
            platform_signature_verifier: platform,
            trusted_local_approval_verifier: approval,
            db,
        })
    }
    fn append(db: &Connection, id: &str, event: &str, payload: &Value) -> Result<()> {
        let prev: String =
            db.query_row("SELECT head_hash FROM ledger_meta WHERE id=1", [], |r| {
                r.get(0)
            })?;
        let timestamp = now();
        let encoded = canonical_json(&sanitize_payload(payload, ""));
        let hash = digest_payload(
            &json!({"action_id":id,"event_type":event,"payload":encoded,"created_at":timestamp,"prev_hash":prev}),
        );
        db.execute("INSERT INTO events(action_id,event_type,payload_json,created_at,prev_hash,event_hash) VALUES(?1,?2,?3,?4,?5,?6)",params![id,event,encoded,timestamp,prev,hash])?;
        db.execute(
            "UPDATE ledger_meta SET event_count=event_count+1,head_hash=?1 WHERE id=1",
            [hash],
        )?;
        Ok(())
    }
    pub fn begin(&self, action: &ActionContext) -> Result<BeginResult> {
        action.verify_platform_signature(self.platform_signature_verifier.as_deref())?;
        let envelope = sha256(&action.canonical_signed_payload()?);
        let mut db = database::lock(&self.db)?;
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let by_key = tx
            .query_row(
                &format!("{SELECT} WHERE idempotency_key=?1"),
                [&action.idempotency_key],
                record,
            )
            .optional()?;
        let by_id = tx
            .query_row(
                &format!("{SELECT} WHERE action_id=?1"),
                [&action.action_id],
                record,
            )
            .optional()?;
        if by_key.is_some() && by_id.is_some() && by_key != by_id {
            return Err(conflict(
                "action id and idempotency key refer to different actions",
            ));
        }
        if let Some(existing) = by_key.or(by_id) {
            if existing.action_id != action.action_id
                || existing.idempotency_key != action.idempotency_key
                || existing.arguments_digest != action.arguments_digest
                || existing.envelope_digest != envelope
            {
                return Err(conflict(
                    "idempotency key or action id reused with new intent",
                ));
            }
            tx.commit()?;
            return Ok(BeginResult {
                created: false,
                record: existing,
            });
        }
        let stamp = now();
        tx.execute("INSERT INTO actions(action_id,idempotency_key,arguments_digest,envelope_digest,status,result_json,created_at,updated_at) VALUES(?1,?2,?3,?4,'policy_check',NULL,?5,?5)",params![action.action_id,action.idempotency_key,action.arguments_digest,envelope,stamp])?;
        let mut payload: Value = serde_json::from_slice(&action.canonical_signed_payload()?)?;
        let map = payload.as_object_mut().unwrap();
        map.remove("approval");
        map.remove("resource_refs");
        map.remove("issued_at");
        map.remove("expires_at");
        map.insert("envelope_digest".into(), json!(envelope));
        map.insert(
            "approval_id".into(),
            json!(action.approval.as_ref().map(|v| &v.approval_id)),
        );
        Self::append(&tx, &action.action_id, "policy_check", &payload)?;
        tx.commit()?;
        Ok(BeginResult {
            created: true,
            record: ActionRecord {
                action_id: action.action_id.clone(),
                idempotency_key: action.idempotency_key.clone(),
                arguments_digest: action.arguments_digest.clone(),
                envelope_digest: envelope,
                status: ActionStatus::PolicyCheck,
                result: None,
            },
        })
    }
    pub fn get(&self, id: &str) -> Result<Option<ActionRecord>> {
        Ok(database::lock(&self.db)?
            .query_row(&format!("{SELECT} WHERE action_id=?1"), [id], record)
            .optional()?)
    }
    pub fn get_by_idempotency_key(&self, key: &str) -> Result<Option<ActionRecord>> {
        Ok(database::lock(&self.db)?
            .query_row(&format!("{SELECT} WHERE idempotency_key=?1"), [key], record)
            .optional()?)
    }
    pub fn transition(
        &self,
        id: &str,
        status: ActionStatus,
        result: Option<Value>,
    ) -> Result<ActionRecord> {
        let mut db = database::lock(&self.db)?;
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut before = tx
            .query_row(&format!("{SELECT} WHERE action_id=?1"), [id], record)
            .optional()?
            .ok_or_else(|| conflict("action not found"))?;
        let safe = result.as_ref().map(|v| sanitize_payload(v, ""));
        if before.status.terminal() {
            if before.status == status && before.result == safe {
                tx.commit()?;
                return Ok(before);
            }
            return Err(conflict("action already has a different terminal result"));
        }
        if !before.status.allows(status) {
            return Err(conflict("invalid action state transition"));
        }
        tx.execute(
            "UPDATE actions SET status=?1,result_json=?2,updated_at=?3 WHERE action_id=?4",
            params![
                status.as_str(),
                safe.as_ref().map(canonical_json),
                now(),
                id
            ],
        )?;
        Self::append(
            &tx,
            id,
            status.as_str(),
            safe.as_ref().unwrap_or(&json!({})),
        )?;
        tx.commit()?;
        before.status = status;
        before.result = safe;
        Ok(before)
    }
    pub fn finish(&self, id: &str, status: ActionStatus, result: Value) -> Result<ActionRecord> {
        if !status.terminal() {
            return Err(conflict("finish requires a terminal status"));
        }
        self.transition(id, status, Some(result))
    }
    pub fn mark_awaiting_approval(&self, id: &str) -> Result<()> {
        self.transition(id, ActionStatus::AwaitingApproval, None)
            .map(|_| ())
    }
    pub fn mark_dispatched(&self, id: &str) -> Result<()> {
        self.transition(id, ActionStatus::Dispatched, None)
            .map(|_| ())
    }
    pub fn mark_running(&self, id: &str) -> Result<()> {
        self.transition(id, ActionStatus::Running, None).map(|_| ())
    }
    pub fn mark_observed(&self, id: &str) -> Result<()> {
        self.transition(id, ActionStatus::Observed, None)
            .map(|_| ())
    }
    pub fn interrupt_running(&self, exclude: &HashSet<String>) -> Result<Vec<String>> {
        let records = {
            let db = database::lock(&self.db)?;
            let mut stmt = db.prepare(SELECT)?;
            stmt.query_map([], record)?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        let mut interrupted = Vec::new();
        for item in records {
            if item.status.terminal() || exclude.contains(&item.action_id) {
                continue;
            }
            let terminal = if matches!(
                item.status,
                ActionStatus::PolicyCheck | ActionStatus::AwaitingApproval
            ) {
                ActionStatus::Interrupted
            } else {
                ActionStatus::Unknown
            };
            self.finish(&item.action_id,terminal,json!({"error_code":"transport_disconnected","replay_allowed":false,"side_effect_unknown":terminal==ActionStatus::Unknown}))?;
            interrupted.push(item.action_id);
        }
        Ok(interrupted)
    }
    pub fn verify_integrity(&self) -> Result<()> {
        let db = database::lock(&self.db)?;
        let (count, head): (i64, String) = db.query_row(
            "SELECT event_count,head_hash FROM ledger_meta WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let rows=db.prepare("SELECT action_id,event_type,payload_json,created_at,prev_hash,event_hash FROM events ORDER BY seq")?.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,f64>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?)))?.collect::<std::result::Result<Vec<_>,_>>()?;
        if rows.len() as i64 != count {
            return Err(integrity("action ledger tail/count seal is invalid"));
        }
        let mut previous = "0".repeat(64);
        let mut last = HashMap::new();
        for (id, event, payload, stamp, prev, hash) in rows {
            let expected = digest_payload(
                &json!({"action_id":id,"event_type":event,"payload":payload,"created_at":stamp,"prev_hash":prev}),
            );
            if prev != previous || !constant_time_equal(expected.as_bytes(), hash.as_bytes()) {
                return Err(integrity("action ledger integrity check failed"));
            }
            previous = hash;
            last.insert(id, (event, payload));
        }
        if head != previous {
            return Err(integrity("action ledger head seal is invalid"));
        }
        let actions = db
            .prepare(SELECT)?
            .query_map([], record)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if actions.len() != last.len() {
            return Err(integrity("action index and event chain disagree"));
        }
        for action in actions {
            let (event, payload) = last
                .get(&action.action_id)
                .ok_or_else(|| integrity("action event missing"))?;
            if action.envelope_digest == "legacy-unbound" {
                continue;
            }
            if action.envelope_digest.len() != 64
                || event != action.status.as_str()
                || (action.status.terminal()
                    && action.result.as_ref().map(canonical_json).as_ref() != Some(payload))
            {
                return Err(integrity("action index and event chain disagree"));
            }
        }
        Ok(())
    }
}
