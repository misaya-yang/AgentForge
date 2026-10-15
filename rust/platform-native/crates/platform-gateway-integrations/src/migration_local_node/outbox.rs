use super::{LocalNodeError as Error, Result, database, models::*};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{path::Path, sync::Mutex};
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ReceiptEvent {
    pub sequence: i64,
    pub event_id: String,
    pub action_id: Option<String>,
    pub event_type: String,
    pub status: Option<ActionStatus>,
    pub occurred_at: f64,
    pub result_digest: Option<String>,
    pub error_code: Option<String>,
    pub summary: Option<String>,
    pub result: Option<Value>,
}
impl ReceiptEvent {
    pub fn as_value(&self, include_result: bool) -> Result<Value> {
        let mut v = serde_json::to_value(self)?;
        if !include_result {
            v.as_object_mut().unwrap().remove("result");
        }
        Ok(v)
    }
}
pub struct ReceiptOutbox {
    db: Mutex<Connection>,
}
fn error(message: &str) -> Error {
    Error::new("outbox_error", message)
}
fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ReceiptEvent> {
    let status: Option<String> = r.get(4)?;
    let result: Option<String> = r.get(9)?;
    let bad = || rusqlite::Error::InvalidQuery;
    Ok(ReceiptEvent {
        sequence: r.get(0)?,
        event_id: r.get(1)?,
        action_id: r.get(2)?,
        event_type: r.get(3)?,
        status: status
            .map(|s| serde_json::from_value(serde_json::json!(s)))
            .transpose()
            .map_err(|_| bad())?,
        occurred_at: r.get(5)?,
        result_digest: r.get(6)?,
        error_code: r.get(7)?,
        summary: r.get(8)?,
        result: result
            .map(|s| serde_json::from_str(&s))
            .transpose()
            .map_err(|_| bad())?,
    })
}
const SELECT: &str = "SELECT sequence,event_id,action_id,event_type,status,occurred_at,result_digest,error_code,summary,result_json FROM receipt_events";
impl ReceiptOutbox {
    pub fn open(path: &Path) -> Result<Self> {
        let db = database::open(path)?;
        {
            let conn = database::lock(&db)?;
            conn.execute_batch("CREATE TABLE IF NOT EXISTS receipt_events(sequence INTEGER PRIMARY KEY AUTOINCREMENT,event_id TEXT UNIQUE NOT NULL,action_id TEXT,event_type TEXT NOT NULL,status TEXT,occurred_at REAL NOT NULL,result_digest TEXT,error_code TEXT,summary TEXT,result_json TEXT); CREATE TABLE IF NOT EXISTS receipt_meta(id INTEGER PRIMARY KEY CHECK(id=1),acked_through INTEGER NOT NULL); INSERT OR IGNORE INTO receipt_meta VALUES(1,0);")?;
            let columns = conn
                .prepare("PRAGMA table_info(receipt_events)")?
                .query_map([], |r| r.get::<_, String>(1))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            if !columns.iter().any(|v| v == "result_json") {
                conn.execute_batch("ALTER TABLE receipt_events ADD COLUMN result_json TEXT")?;
            }
        }
        Ok(Self { db })
    }
    pub fn append(&self, mut event: ReceiptEvent, retain_result: bool) -> Result<ReceiptEvent> {
        if !valid_identifier(&event.event_id, 512)
            || !valid_identifier(&event.event_type, 80)
            || event
                .action_id
                .as_ref()
                .is_some_and(|s| !valid_identifier(s, 512))
            || event
                .error_code
                .as_ref()
                .is_some_and(|s| !valid_identifier(s, 80))
            || !event.occurred_at.is_finite()
        {
            return Err(error("receipt identity or timestamp invalid"));
        }
        if event.summary.as_ref().is_some_and(|s| {
            s.chars().count() > 500 || s.chars().any(|c| c < '\u{20}' && c != '\t')
        }) {
            return Err(error("receipt summary invalid"));
        }
        if let Some(result) = &event.result {
            if event.result_digest.is_some() || canonical_json(result).len() > 2 * 1024 * 1024 {
                return Err(error("receipt digest budget or shape invalid"));
            }
            event.result_digest = Some(digest_payload(result));
        } else if retain_result {
            return Err(error("retained receipt result is missing"));
        }
        if event.result_digest.as_ref().is_some_and(|s| {
            s.len() != 64
                || !s
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        }) {
            return Err(error("receipt result digest is invalid"));
        }
        if !retain_result {
            event.result = None;
        }
        let mut db = database::lock(&self.db)?;
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if let Some(existing) = tx
            .query_row(
                &format!("{SELECT} WHERE event_id=?1"),
                [&event.event_id],
                row,
            )
            .optional()?
        {
            event.sequence = existing.sequence;
            let mut compare = existing.clone();
            compare.occurred_at = event.occurred_at;
            if compare != event {
                return Err(error("receipt event id was reused with different data"));
            }
            tx.commit()?;
            return Ok(existing);
        }
        tx.execute("INSERT INTO receipt_events(event_id,action_id,event_type,status,occurred_at,result_digest,error_code,summary,result_json) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",params![event.event_id,event.action_id,event.event_type,event.status.map(ActionStatus::as_str),event.occurred_at,event.result_digest,event.error_code,event.summary,event.result.as_ref().map(canonical_json)])?;
        event.sequence = tx.last_insert_rowid();
        tx.commit()?;
        Ok(event)
    }
    pub fn acked_through(&self) -> Result<i64> {
        Ok(database::lock(&self.db)?.query_row(
            "SELECT acked_through FROM receipt_meta WHERE id=1",
            [],
            |r| r.get(0),
        )?)
    }
    pub fn pending(&self, limit: usize) -> Result<Vec<ReceiptEvent>> {
        if !(1..=200).contains(&limit) {
            return Err(error("receipt batch limit invalid"));
        }
        let db = database::lock(&self.db)?;
        let mut stmt=db.prepare(&format!("{SELECT} WHERE sequence>(SELECT acked_through FROM receipt_meta WHERE id=1) ORDER BY sequence LIMIT ?1"))?;
        Ok(stmt
            .query_map([limit as i64], row)?
            .collect::<std::result::Result<Vec<_>, _>>()?)
    }
    pub fn acknowledge(&self, through: i64) -> Result<()> {
        let mut db = database::lock(&self.db)?;
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let current: i64 = tx.query_row(
            "SELECT acked_through FROM receipt_meta WHERE id=1",
            [],
            |r| r.get(0),
        )?;
        let maximum: i64 = tx.query_row(
            "SELECT COALESCE(MAX(sequence),0) FROM receipt_events",
            [],
            |r| r.get(0),
        )?;
        if through < current || through > maximum {
            return Err(error("receipt acknowledgement outside local range"));
        }
        let count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM receipt_events WHERE sequence>?1 AND sequence<=?2",
            params![current, through],
            |r| r.get(0),
        )?;
        if count != through - current {
            return Err(error("receipt acknowledgement would cross an event gap"));
        }
        tx.execute(
            "UPDATE receipt_meta SET acked_through=?1 WHERE id=1",
            [through],
        )?;
        tx.execute(
            "UPDATE receipt_events SET result_json=NULL WHERE sequence<=?1",
            [through],
        )?;
        tx.commit()?;
        db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
        Ok(())
    }
}
