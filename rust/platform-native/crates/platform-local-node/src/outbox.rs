use crate::{Error, Result, canonical, digest};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Receipt {
    pub sequence: u64,
    pub event_id: String,
    pub action_id: Option<String>,
    pub event_type: String,
    pub status: Option<String>,
    pub occurred_at: f64,
    pub result_digest: Option<String>,
    pub error_code: Option<String>,
    pub summary: Option<String>,
    pub result: Option<Value>,
}
pub struct Outbox {
    db: Connection,
}
fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Receipt> {
    let raw: Option<String> = r.get(9)?;
    Ok(Receipt {
        sequence: r.get(0)?,
        event_id: r.get(1)?,
        action_id: r.get(2)?,
        event_type: r.get(3)?,
        status: r.get(4)?,
        occurred_at: r.get(5)?,
        result_digest: r.get(6)?,
        error_code: r.get(7)?,
        summary: r.get(8)?,
        result: raw
            .map(|s| serde_json::from_str(&s))
            .transpose()
            .map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    9,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?,
    })
}
const SELECT: &str = "SELECT sequence,event_id,action_id,event_type,status,occurred_at,result_digest,error_code,summary,result_json FROM receipt_events";
fn safe_cancel_result(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    matches!(object.len(), 2 | 3)
        && object.get("cancelled") == Some(&Value::Bool(true))
        && object.get("replay_allowed") == Some(&Value::Bool(false))
        && object
            .get("action_known")
            .map_or(true, |known| known == &Value::Bool(false))
        && object.keys().all(|key| {
            matches!(
                key.as_str(),
                "cancelled" | "replay_allowed" | "action_known"
            )
        })
}
impl Outbox {
    pub fn open(path: &Path) -> Result<Self> {
        let db = Connection::open(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA secure_delete=ON; CREATE TABLE IF NOT EXISTS receipt_events(sequence INTEGER PRIMARY KEY AUTOINCREMENT,event_id TEXT UNIQUE NOT NULL,action_id TEXT,event_type TEXT NOT NULL,status TEXT,occurred_at REAL NOT NULL,result_digest TEXT,error_code TEXT,summary TEXT,result_json TEXT); CREATE TABLE IF NOT EXISTS receipt_meta(id INTEGER PRIMARY KEY CHECK(id=1),acked_through INTEGER NOT NULL); INSERT OR IGNORE INTO receipt_meta VALUES(1,0);")?;
        Ok(Self { db })
    }
    pub fn append(&mut self, mut event: Receipt, retain_result: bool) -> Result<Receipt> {
        // Keep the small public cancellation contract even when operation
        // results are deliberately discarded. Never retain arbitrary payload
        // fields merely because a receipt is cancelled.
        let retain_result = retain_result
            || (event.status.as_deref() == Some("cancelled")
                && event.result.as_ref().is_some_and(safe_cancel_result));
        let valid = |s: &str, max: usize| {
            !s.is_empty() && s.chars().count() <= max && !s.chars().any(|c| c < ' ')
        };
        if !valid(&event.event_id, 512)
            || !valid(&event.event_type, 80)
            || !event.occurred_at.is_finite()
            || event.action_id.as_ref().is_some_and(|s| !valid(s, 512))
            || event.error_code.as_ref().is_some_and(|s| !valid(s, 80))
            || event
                .summary
                .as_ref()
                .is_some_and(|s| s.chars().count() > 500 || s.chars().any(|c| c < ' ' && c != '\t'))
        {
            return Err(Error::Protocol);
        }
        if event.status.as_ref().is_some_and(|s| {
            !crate::ledger::terminal(s)
                && ![
                    "proposed",
                    "policy_check",
                    "awaiting_approval",
                    "dispatched",
                    "running",
                    "observed",
                ]
                .contains(&s.as_str())
        }) {
            return Err(Error::Protocol);
        }
        if let Some(v) = &event.result {
            if event.result_digest.is_some() || canonical(v).len() > 2 * 1024 * 1024 {
                return Err(Error::Protocol);
            }
            event.result_digest = Some(digest(v));
        } else if retain_result {
            return Err(Error::Protocol);
        }
        if event.result_digest.as_ref().is_some_and(|d| {
            d.len() != 64
                || !d
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        }) {
            return Err(Error::Protocol);
        }
        if !retain_result {
            event.result = None;
        }
        let tx = self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if let Some(existing) = tx
            .query_row(
                &format!("{SELECT} WHERE event_id=?"),
                [&event.event_id],
                row,
            )
            .optional()?
        {
            event.sequence = existing.sequence;
            event.occurred_at = existing.occurred_at;
            if event != existing {
                return Err(Error::Conflict);
            }
            return Ok(existing);
        }
        tx.execute("INSERT INTO receipt_events(event_id,action_id,event_type,status,occurred_at,result_digest,error_code,summary,result_json) VALUES(?,?,?,?,?,?,?,?,?)",params![event.event_id,event.action_id,event.event_type,event.status,event.occurred_at,event.result_digest,event.error_code,event.summary,event.result.as_ref().map(canonical)])?;
        event.sequence = tx.last_insert_rowid() as u64;
        tx.commit()?;
        Ok(event)
    }
    pub fn acked_through(&self) -> Result<u64> {
        Ok(self.db.query_row(
            "SELECT acked_through FROM receipt_meta WHERE id=1",
            [],
            |r| r.get(0),
        )?)
    }
    pub fn has_terminal_action(&self, action_id: &str) -> Result<bool> {
        Ok(self.db.query_row("SELECT EXISTS(SELECT 1 FROM receipt_events WHERE action_id=? AND status IN ('succeeded','failed','cancelled','interrupted','unknown'))",[action_id],|r|r.get(0))?)
    }
    pub fn pending(&self, limit: u32) -> Result<Vec<Receipt>> {
        if limit == 0 || limit > 200 {
            return Err(Error::Protocol);
        }
        let mut stmt = self.db.prepare(&format!(
            "{SELECT} WHERE sequence>? ORDER BY sequence LIMIT ?"
        ))?;
        Ok(stmt
            .query_map(params![self.acked_through()?, limit], row)?
            .collect::<std::result::Result<Vec<_>, _>>()?)
    }
    pub fn acknowledge(&mut self, accepted: u64) -> Result<()> {
        let tx = self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let current: u64 = tx.query_row(
            "SELECT acked_through FROM receipt_meta WHERE id=1",
            [],
            |r| r.get(0),
        )?;
        let max: u64 = tx.query_row(
            "SELECT COALESCE(MAX(sequence),0) FROM receipt_events",
            [],
            |r| r.get(0),
        )?;
        if accepted < current || accepted > max {
            return Err(Error::Protocol);
        }
        let count: u64 = tx.query_row(
            "SELECT COUNT(*) FROM receipt_events WHERE sequence>? AND sequence<=?",
            params![current, accepted],
            |r| r.get(0),
        )?;
        if count != accepted - current {
            return Err(Error::Integrity);
        }
        tx.execute(
            "UPDATE receipt_meta SET acked_through=? WHERE id=1",
            [accepted],
        )?;
        tx.execute(
            "UPDATE receipt_events SET result_json=NULL WHERE sequence<=?",
            [accepted],
        )?;
        tx.commit()?;
        self.db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Temp(std::path::PathBuf);
    impl Temp {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("local-outbox-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn retains_only_the_literal_cancellation_result_when_operation_results_are_dropped() {
        let temp = Temp::new();
        let mut outbox = Outbox::open(&temp.0.join("outbox.sqlite")).unwrap();
        for (event_id, result) in [
            (
                "cancelled-safe",
                json!({"cancelled":true,"replay_allowed":false}),
            ),
            (
                "cancelled-private",
                json!({"cancelled":true,"replay_allowed":false,"content":"private"}),
            ),
        ] {
            outbox
                .append(
                    Receipt {
                        sequence: 0,
                        event_id: event_id.into(),
                        action_id: Some(event_id.into()),
                        event_type: "action.cancelled".into(),
                        status: Some("cancelled".into()),
                        occurred_at: crate::now(),
                        result_digest: None,
                        error_code: None,
                        summary: None,
                        result: Some(result),
                    },
                    false,
                )
                .unwrap();
        }

        let receipts = outbox.pending(10).unwrap();
        assert_eq!(
            receipts[0].result.as_ref().unwrap(),
            &json!({"cancelled":true,"replay_allowed":false})
        );
        assert!(receipts[1].result.is_none());
    }
}
