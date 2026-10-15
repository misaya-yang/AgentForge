//! PostgreSQL task persistence from `DatabaseStorage` and `DatabaseTaskStorage`.
//!
//! Production SQL is pinned to canonical `gateway.tasks`; the supplied pool must
//! already be configured by the trusted host. This adapter opens no connection,
//! creates no schema, mounts no route, and runs no worker or callback transport.
//! ID-only reads and writes are an internal authority boundary: callers must
//! authenticate and enforce ownership before calling them. The source upsert
//! retains an existing row's identity, but does not authorize its mutable update.
//!
//! Source quirks are intentional: save omits created/updated timestamps, clears
//! falsy JSON results to SQL NULL. Native typed UnifiedResponse results serialize
//! through the canonical protocol with a private metadata discriminator; legacy
//! dictionaries remain JSON. Lists order by created_at only; pending tasks
//! order by priority DESC, created_at ASC. Direct status update sets lifecycle
//! timestamps, while TaskManager's get/save update and cancel do not. Missing-row
//! direct updates are successful no-ops. There is no source count operation.
//! The native manager/worker use compare_status instead: one conditional UPDATE
//! preserves immutable identity and terminal outcomes; no retry/reclaim is added.
//!
//! Scope/differences: this is cache-free durable storage, not the Python wrapper's
//! Redis/process cache. Request JSONB is decoded rather than exposing asyncpg's
//! undecoded string. Database times retain UTC awareness; newly created in-memory
//! tasks retain the source's naive timestamps. Public aware UTC times serialize Z.
//! Unrepresentable rows (including NULL owners), serialization/overflow failures,
//! and a closed/unavailable pool fail closed with a redacted storage error. The
//! source's disabled-pool successful no-op is deliberately not reproduced.
//! Caller-owned mutations use supervised transactions, server cancellation and
//! rollback on caller drop. Abort and join callers, then drain_cancelled before
//! shutdown. This does not undo a COMMIT already sent or make worker.stop join.
//! Read methods retain their original unsupervised, read-only behavior.
use crate::postgres_operation::PgOperationStorage;
use crate::protocol::{ContentData, ProtocolTimestamp, UnifiedResponse};
use crate::task_manager::{
    SystemTaskRuntime, Task, TaskError, TaskFuture, TaskListFilter, TaskResult, TaskRuntime,
    TaskStatus, TaskStorage,
};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use sqlx::{FromRow, PgPool};

const TABLE: &str = "gateway.tasks";
// Kept outside public result JSON: shape alone must never promote legacy JSON
// into a typed response. User request metadata is not task bookkeeping metadata.
const RESULT_FORMAT_KEY: &str = "gateway.native_task_result.v1";
const SAVE: &str = r#"
INSERT INTO gateway.tasks (
    task_id, request_id, service_id, user_id, tenant_id,
    status, progress, request_data, result, error,
    callback_url, callback_sent, priority, retry_count, max_retries,
    metadata, started_at, completed_at
) VALUES (
    $1, $2, $3, $4, $5, $6, $7::text::numeric, $8, $9, $10,
    $11, $12, $13, $14, $15, $16, $17, $18
)
ON CONFLICT (task_id) DO UPDATE SET
    status = EXCLUDED.status,
    progress = EXCLUDED.progress,
    result = EXCLUDED.result,
    error = EXCLUDED.error,
    callback_sent = EXCLUDED.callback_sent,
    retry_count = EXCLUDED.retry_count,
    metadata = EXCLUDED.metadata,
    started_at = EXCLUDED.started_at,
    completed_at = EXCLUDED.completed_at,
    updated_at = NOW()
"#;
// Typed time projections avoid dependence on the connection's TimeZone setting.
// JSON projection also preserves NUMERIC without a new SQLx Decimal dependency.
const SELECT: &str = "SELECT to_jsonb(t) AS document, created_at, updated_at, started_at, completed_at FROM gateway.tasks t";
const MARK_CALLBACK: &str =
    "UPDATE gateway.tasks SET callback_sent = TRUE, updated_at = NOW() WHERE task_id = $1";

#[derive(Clone)]
pub struct PgTaskStorage {
    pool: PgPool,
    operations: PgOperationStorage,
    // Only tests may substitute a generated, validated isolated schema name.
    table: String,
}
impl PgTaskStorage {
    /// Independent data/control pools must reach the same PostgreSQL database,
    /// server and role using direct or session-pinned connections. Keep both
    /// pools and the runtime alive until cancelled callers have been drained.
    /// Construction opens no connection and starts no background work.
    pub fn new(pool: PgPool, control_pool: PgPool) -> Result<Self, TaskError> {
        let operations = PgOperationStorage::new(pool.clone(), control_pool)
            .map_err(|_| TaskError::StorageUnavailable)?;
        Ok(Self {
            pool,
            operations,
            table: TABLE.into(),
        })
    }

    /// After aborting and joining callers, await their server cancellation and
    /// rollback before releasing external locks or shutting down the runtime.
    /// A COMMIT already sent can have succeeded; this is not a rollback promise.
    pub async fn drain_cancelled(&self) -> Result<(), TaskError> {
        self.operations
            .drain_cancelled()
            .await
            .map_err(|_| TaskError::StorageUnavailable)
    }

    fn sql(&self, statement: &str) -> String {
        statement.replace(TABLE, &self.table)
    }

    /// Mirrors DatabaseTaskStorage.update_status, not TaskManager.update_status.
    /// None leaves optional fields unchanged; explicit JSON null is Python None.
    /// Terminal states can still be changed, and missing IDs are successful no-ops.
    pub async fn update_status(
        &self,
        task_id: &str,
        status: TaskStatus,
        progress: Option<f64>,
        result: Option<Value>,
        error: Option<&str>,
    ) -> Result<(), TaskError> {
        let progress = progress.map(numeric).transpose()?;
        let result = result.filter(|value| !value.is_null());
        let sql = self.sql(&update_statement(
            status,
            progress.is_some(),
            result.is_some(),
            error.is_some(),
        ));
        let task_id = task_id.to_owned();
        let error = error.map(str::to_owned);
        let timestamp = (status != TaskStatus::Pending).then(|| SystemTaskRuntime.now().and_utc());
        let mut connection = self
            .operations
            .acquire()
            .await
            .map_err(|_| TaskError::StorageUnavailable)?;
        connection
            .execute(move |connection| {
                Box::pin(async move {
                    let mut query = sqlx::query(&sql).bind(status.as_str());
                    if let Some(progress) = progress {
                        query = query.bind(progress);
                    }
                    if let Some(result) = result {
                        query = query.bind(result);
                    }
                    if let Some(error) = error {
                        query = query.bind(error);
                    }
                    if let Some(timestamp) = timestamp {
                        query = query.bind(timestamp);
                    }
                    query.bind(task_id).execute(connection).await?;
                    Ok(())
                })
            })
            .await
            .map_err(|_| TaskError::StorageUnavailable)
    }

    pub async fn mark_callback_sent(&self, task_id: &str) -> Result<(), TaskError> {
        let sql = self.sql(MARK_CALLBACK);
        let task_id = task_id.to_owned();
        let mut connection = self
            .operations
            .acquire()
            .await
            .map_err(|_| TaskError::StorageUnavailable)?;
        connection
            .execute(move |connection| {
                Box::pin(async move {
                    sqlx::query(&sql).bind(task_id).execute(connection).await?;
                    Ok(())
                })
            })
            .await
            .map_err(|_| TaskError::StorageUnavailable)
    }

    /// Global trusted worker query. This neither claims nor locks returned tasks.
    pub async fn get_pending_tasks(&self, limit: usize) -> Result<Vec<Task>, TaskError> {
        let sql = self.sql(&pending_statement());
        let rows = sqlx::query_as::<_, TaskRow>(&sql)
            .bind(page_number(limit)?)
            .fetch_all(&self.pool)
            .await
            .map_err(unavailable)?;
        rows.into_iter().map(TaskRow::into_task).collect()
    }
}

fn unavailable(_: sqlx::Error) -> TaskError {
    TaskError::StorageUnavailable
}
fn numeric(value: f64) -> Result<String, TaskError> {
    if !value.is_finite() {
        return Err(TaskError::StorageUnavailable);
    }
    // SQL casts a bound decimal string to NUMERIC. Binding float8's binary
    // representation directly to an inferred NUMERIC parameter is invalid.
    Decimal::from_f64_retain(value)
        .map(|value| value.to_string())
        .ok_or(TaskError::StorageUnavailable)
}
fn page_number(value: usize) -> Result<i64, TaskError> {
    i64::try_from(value).map_err(|_| TaskError::StorageUnavailable)
}
fn integer(value: i64) -> Result<i32, TaskError> {
    i32::try_from(value).map_err(|_| TaskError::StorageUnavailable)
}
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64().is_some_and(|value| value != 0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
    }
}
pub(crate) fn saved_result(result: &Option<TaskResult>) -> Result<Option<Value>, TaskError> {
    match result {
        Some(TaskResult::Response(response)) => serde_json::to_value(response)
            .map(Some).map_err(|_| TaskError::StorageUnavailable),
        Some(TaskResult::Json(value)) if truthy(value) => Ok(Some(value.clone())),
        _ => Ok(None),
    }
}
pub(crate) fn result_format(result: &Option<TaskResult>) -> Option<Value> {
    let Some(TaskResult::Response(response)) = result else { return None; };
    // ContentData is untagged on the public wire. Retain data kinds privately so
    // canonical roundtrips preserve both binary bytes and ordinary JSON arrays.
    Some(json!({"output_data_types": response.outputs.iter().map(|output| match &output.data {
        None => "none",
        Some(ContentData::Text(_)) => "text",
        Some(ContentData::Bytes(_)) => "bytes",
        Some(ContentData::Json(_)) => "json",
    }).collect::<Vec<_>>()}))
}
fn saved_metadata(task: &Task) -> Value {
    let mut metadata = task.metadata.clone();
    if let Some(format) = result_format(&task.result) {
        metadata.get_or_insert_default().insert(RESULT_FORMAT_KEY.into(), format);
    } else if let Some(metadata) = &mut metadata {
        metadata.remove(RESULT_FORMAT_KEY);
    }
    metadata.map(Value::Object).unwrap_or(Value::Null)
}
fn restore_result(task: &mut Task) -> Result<(), TaskError> {
    let Some(format) = task.metadata.as_ref().and_then(|metadata| metadata.get(RESULT_FORMAT_KEY)) else {
        return Ok(());
    };
    let Some(TaskResult::Json(value)) = &task.result else {
        return Err(TaskError::StorageUnavailable);
    };
    let kinds = format.get("output_data_types").and_then(Value::as_array)
        .ok_or(TaskError::StorageUnavailable)?;
    let mut response: UnifiedResponse = serde_json::from_value(value.clone())
        .map_err(|_| TaskError::StorageUnavailable)?;
    if kinds.len() != response.outputs.len() { return Err(TaskError::StorageUnavailable); }
    for (index, (output, kind)) in response.outputs.iter_mut().zip(kinds).enumerate() {
        let raw = &value["outputs"][index]["data"];
        output.data = match kind.as_str() {
            Some("none") if raw.is_null() => None,
            Some("text") => Some(ContentData::Text(raw.as_str().ok_or(TaskError::StorageUnavailable)?.to_owned())),
            Some("bytes") => Some(ContentData::Bytes(serde_json::from_value(raw.clone()).map_err(|_| TaskError::StorageUnavailable)?)),
            Some("json") => Some(ContentData::Json(raw.clone())),
            _ => return Err(TaskError::StorageUnavailable),
        };
    }
    task.result = Some(TaskResult::Response(Box::new(response)));
    Ok(())
}
fn list_statement(filter: &TaskListFilter) -> (String, Vec<&str>) {
    let mut sql = format!("{SELECT} WHERE 1=1");
    let mut values = Vec::new();
    for (column, value) in [
        ("user_id", &filter.user_id),
        ("tenant_id", &filter.tenant_id),
        ("service_id", &filter.service_id),
        ("status", &filter.status),
    ] {
        if let Some(value) = value.as_deref().filter(|value| !value.is_empty()) {
            values.push(value);
            sql.push_str(&format!(" AND {column} = ${}", values.len()));
        }
    }
    sql.push_str(&format!(
        " ORDER BY created_at DESC LIMIT ${} OFFSET ${}",
        values.len() + 1,
        values.len() + 2
    ));
    (sql, values)
}
fn pending_statement() -> String {
    format!("{SELECT} WHERE status = 'pending' ORDER BY priority DESC, created_at ASC LIMIT $1")
}
fn update_statement(status: TaskStatus, progress: bool, result: bool, error: bool) -> String {
    let mut assignments = vec!["status = $1".into(), "updated_at = NOW()".into()];
    if result {
        // The compatibility operation accepts JSON, never typed responses.
        // Replacing result must remove any prior native type discriminator.
        assignments.push("metadata = CASE WHEN jsonb_typeof(metadata) = 'object' THEN metadata - 'gateway.native_task_result.v1' ELSE metadata END".into());
    }
    let mut index = 2;
    for (column, present, cast) in [
        ("progress", progress, "::text::numeric"),
        ("result", result, ""),
        ("error", error, ""),
        ("started_at", status == TaskStatus::Processing, ""),
        (
            "completed_at",
            matches!(
                status,
                TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled
            ),
            "",
        ),
    ] {
        if present {
            assignments.push(format!("{column} = ${index}{cast}"));
            index += 1;
        }
    }
    format!(
        "UPDATE {TABLE} SET {} WHERE task_id = ${index}",
        assignments.join(", ")
    )
}

#[derive(FromRow)]
struct TaskRow {
    document: Value,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    started_at: Option<DateTime<Utc>>,
    completed_at: Option<DateTime<Utc>>,
}
impl TaskRow {
    fn into_task(mut self) -> Result<Task, TaskError> {
        let object = self
            .document
            .as_object_mut()
            .ok_or(TaskError::StorageUnavailable)?;
        for owner in ["user_id", "tenant_id"] {
            if object.get(owner).and_then(Value::as_str).is_none() {
                return Err(TaskError::StorageUnavailable);
            }
        }
        for (key, time) in [
            ("created_at", Some(self.created_at)),
            ("updated_at", Some(self.updated_at)),
            ("started_at", self.started_at),
            ("completed_at", self.completed_at),
        ] {
            object.insert(
                key.into(),
                serde_json::to_value(time.map(|time| ProtocolTimestamp::from(time.fixed_offset())))
                    .map_err(|_| TaskError::StorageUnavailable)?,
            );
        }
        // _dict_to_task defaults unrecognized persisted status strings to pending.
        if object
            .get("status")
            .and_then(Value::as_str)
            .is_some_and(|status| {
                !matches!(
                    status,
                    "pending" | "processing" | "completed" | "failed" | "cancelled"
                )
            })
        {
            object.insert("status".into(), Value::String("pending".into()));
        }
        // DatabaseStorage decodes JSONB once, then the wrapper makes one more
        // json.loads attempt when the resulting value itself is a string.
        if let Some(value) = object.get("result").and_then(Value::as_str)
            && let Ok(value) = serde_json::from_str(value)
        {
            object.insert("result".into(), value);
        }
        let mut task = serde_json::from_value(self.document).map_err(|_| TaskError::StorageUnavailable)?;
        restore_result(&mut task)?;
        Ok(task)
    }
}

impl TaskStorage for PgTaskStorage {
    fn save<'a>(&'a self, task: &'a Task) -> TaskFuture<'a, ()> {
        Box::pin(async move {
            let result = saved_result(&task.result)?;
            let request_data = task
                .request_data
                .as_ref()
                .filter(|value| !value.is_empty())
                .map(|value| Value::Object(value.clone()));
            // Preserve unrelated private metadata and mark only actual typed
            // results. Response-shaped legacy dictionaries receive no marker.
            let metadata = saved_metadata(task);
            // Validate and serialize before acquiring a backend or spawning an
            // owned mutation.
            let progress = numeric(task.progress)?;
            let priority = integer(task.priority)?;
            let retry_count = integer(task.retry_count)?;
            let max_retries = integer(task.max_retries)?;
            let task = task.clone();
            let sql = self.sql(SAVE);
            let mut connection = self
                .operations
                .acquire()
                .await
                .map_err(|_| TaskError::StorageUnavailable)?;
            connection
                .execute(move |connection| {
                    Box::pin(async move {
                        sqlx::query(&sql)
                            .bind(&task.task_id)
                            .bind(&task.request_id)
                            .bind(&task.service_id)
                            .bind(&task.user_id)
                            .bind(&task.tenant_id)
                            .bind(task.status.as_str())
                            .bind(progress)
                            .bind(request_data)
                            .bind(result)
                            .bind(&task.error)
                            .bind(&task.callback_url)
                            .bind(task.callback_sent)
                            .bind(priority)
                            .bind(retry_count)
                            .bind(max_retries)
                            .bind(metadata)
                            .bind(task.started_at.map(|time| time.naive_utc().and_utc()))
                            .bind(task.completed_at.map(|time| time.naive_utc().and_utc()))
                            .execute(connection)
                            .await?;
                        Ok(())
                    })
                })
                .await
                .map_err(|_| TaskError::StorageUnavailable)
        })
    }

    fn compare_status<'a>(&'a self, task: &'a Task, expected: TaskStatus) -> TaskFuture<'a, bool> {
        Box::pin(async move {
            if !expected.can_transition_to(task.status) {
                return Err(TaskError::StateConflict);
            }
            let result = saved_result(&task.result)?;
            let format = result_format(&task.result);
            let task = task.clone();
            let sql = self.sql(
                r#"
UPDATE gateway.tasks
SET status = $1, result = $2, error = $3, updated_at = NOW(),
    started_at = $4, completed_at = $5,
    metadata = CASE
        WHEN $12::jsonb IS NULL AND jsonb_typeof(metadata) = 'object'
            THEN metadata - 'gateway.native_task_result.v1'
        WHEN $12::jsonb IS NULL THEN metadata
        ELSE (CASE WHEN jsonb_typeof(metadata) = 'object' THEN metadata ELSE '{}'::jsonb END)
            || jsonb_build_object('gateway.native_task_result.v1', $12::jsonb)
        END
WHERE task_id = $6 AND status = $7 AND request_id = $8
    AND service_id = $9 AND user_id = $10 AND tenant_id = $11
"#,
            );
            let mut connection = self
                .operations
                .acquire()
                .await
                .map_err(|_| TaskError::StorageUnavailable)?;
            connection
                .execute(move |connection| {
                    Box::pin(async move {
                        let changed = sqlx::query(&sql)
                            .bind(task.status.as_str())
                            .bind(result)
                            .bind(&task.error)
                            .bind(task.started_at.map(|time| time.naive_utc().and_utc()))
                            .bind(task.completed_at.map(|time| time.naive_utc().and_utc()))
                            .bind(&task.task_id)
                            .bind(expected.as_str())
                            .bind(&task.request_id)
                            .bind(&task.service_id)
                            .bind(&task.user_id)
                            .bind(&task.tenant_id)
                            .bind(format)
                            .execute(connection)
                            .await?
                            .rows_affected();
                        Ok(changed == 1)
                    })
                })
                .await
                .map_err(|_| TaskError::StorageUnavailable)
        })
    }

    fn get<'a>(&'a self, task_id: &'a str) -> TaskFuture<'a, Task> {
        Box::pin(async move {
            sqlx::query_as::<_, TaskRow>(&self.sql(&format!("{SELECT} WHERE task_id = $1")))
                .bind(task_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(unavailable)?
                .ok_or_else(|| TaskError::NotFound(task_id.into()))?
                .into_task()
        })
    }

    fn list<'a>(&'a self, filter: &'a TaskListFilter) -> TaskFuture<'a, Vec<Task>> {
        Box::pin(async move {
            let (statement, values) = list_statement(filter);
            let sql = self.sql(&statement);
            let mut query = sqlx::query_as::<_, TaskRow>(&sql);
            for value in values {
                query = query.bind(value);
            }
            let rows = query
                .bind(page_number(filter.limit)?)
                .bind(page_number(filter.offset)?)
                .fetch_all(&self.pool)
                .await
                .map_err(unavailable)?;
            rows.into_iter().map(TaskRow::into_task).collect()
        })
    }
}

#[cfg(test)]
mod result_codec_tests {
    use super::*;
    use crate::protocol::{ContentItem, UnifiedRequest};

    fn sample() -> Task {
        let request = UnifiedRequest::new("request", "service", vec![]);
        let now = ProtocolTimestamp::parse_isoformat("2026-10-12T09:00:00.123456Z").unwrap();
        let mut task = Task::new("task".into(), &request, now.naive_utc());
        let mut response = UnifiedResponse::new("request", "completed", vec![
            ContentItem::text("hello"), ContentItem::text("bytes"),
            ContentItem::text("array"), ContentItem::text("json string"),
        ]);
        response.timestamp = now;
        response.outputs[1].data = Some(ContentData::Bytes(vec![0, 255, 128]));
        response.outputs[2].data = Some(ContentData::Json(json!([0, 255, 128])));
        response.outputs[3].data = Some(ContentData::Json(json!("still json")));
        task.status = TaskStatus::Completed;
        task.result = Some(response.into());
        task.metadata.as_mut().unwrap().insert("unrelated".into(), json!("retained"));
        task
    }
    fn roundtrip(task: &Task) -> Result<Task, TaskError> {
        let mut document = serde_json::to_value(task).unwrap();
        document["result"] = saved_result(&task.result)?.unwrap_or(Value::Null);
        document["metadata"] = saved_metadata(task);
        let now = task.created_at.naive_utc().and_utc();
        TaskRow {document, created_at: now, updated_at: now, started_at: None, completed_at: Some(now)}.into_task()
    }
    #[test]
    fn typed_response_survives_real_row_decoder_with_binary_and_json_kinds() {
        let task = sample();
        let restored = roundtrip(&task).unwrap();
        assert_eq!(restored.result, task.result);
        assert_eq!(restored.metadata.as_ref().unwrap()["unrelated"], "retained");
        let Some(TaskResult::Response(response)) = restored.result else { panic!("typed response lost"); };
        assert_eq!(response.to_wire_value()["outputs"][1]["data"], "AP+A");
        assert_eq!(response.to_wire_value()["outputs"][2]["data"], json!([0,255,128]));
    }
    #[test]
    fn response_shaped_legacy_dictionary_stays_json() {
        let mut task = sample();
        let Some(TaskResult::Response(response)) = task.result.take() else { unreachable!() };
        task.result = Some(TaskResult::Json(serde_json::to_value(response).unwrap()));
        let restored = roundtrip(&task).unwrap();
        assert_eq!(restored.result, task.result);
        assert!(!restored.metadata.as_ref().unwrap().contains_key(RESULT_FORMAT_KEY));
    }
    #[test]
    fn json_save_removes_stale_marker_without_changing_legacy_dictionary() {
        let mut task = sample();
        task.metadata.as_mut().unwrap().insert(RESULT_FORMAT_KEY.into(), result_format(&task.result).unwrap());
        task.result = Some(TaskResult::Json(json!({"request_id":"legacy","outputs":[]})));
        assert!(!saved_metadata(&task).as_object().unwrap().contains_key(RESULT_FORMAT_KEY));
        assert_eq!(saved_result(&task.result).unwrap(), Some(json!({"request_id":"legacy","outputs":[]})));
    }
    #[test]
    fn invalid_native_marker_fails_closed() {
        let mut task = sample();
        task.result = Some(TaskResult::Json(saved_result(&task.result).unwrap().unwrap()));
        task.metadata.as_mut().unwrap().insert(RESULT_FORMAT_KEY.into(), json!({"output_data_types":[]}));
        assert_eq!(restore_result(&mut task), Err(TaskError::StorageUnavailable));
    }
    #[test]
    fn legacy_falsey_and_null_metadata_storage_remain_compatible() {
        let mut task = sample();
        task.metadata = None;
        task.result = Some(TaskResult::Json(json!({})));
        assert_eq!(saved_result(&task.result).unwrap(), None);
        assert_eq!(saved_metadata(&task), Value::Null);
    }
    #[test]
    fn compatibility_json_update_clears_only_native_codec_metadata_key() {
        let statement = update_statement(TaskStatus::Completed, false, true, false);
        assert!(statement.contains("metadata - 'gateway.native_task_result.v1'"));
        assert!(!update_statement(TaskStatus::Completed, false, false, false).contains("metadata ="));
    }
}
