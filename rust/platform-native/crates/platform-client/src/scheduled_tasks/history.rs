//! Bounded owner-scoped recovery reads. Unknown database fields remain intact.
use crate::{ApiRequest, Client, Error, scheduled_notifications::{timestamp, uuid}};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ScheduleCursor {
    pub before_at: String,
    pub before_id: String,
}
#[derive(Clone, Debug)]
pub struct ScheduleQuery {
    pub limit: u16,
    pub cursor: Option<ScheduleCursor>,
}
impl Default for ScheduleQuery {
    fn default() -> Self { Self { limit: 20, cursor: None } }
}
impl ScheduleQuery {
    fn apply(&self, request: ApiRequest, max: u16) -> Result<ApiRequest, Error> {
        if !(1..=max).contains(&self.limit) { return Err(Error::Request); }
        let mut request = request.query("limit", self.limit);
        if let Some(cursor) = &self.cursor {
            timestamp(&cursor.before_at).ok_or(Error::Request)?;
            let id = uuid(&cursor.before_id).ok_or(Error::Request)?;
            request = request.query("before_at", &cursor.before_at).query("before_id", id);
        }
        Ok(request)
    }
    pub fn tasks_request(&self) -> Result<ApiRequest, Error> {
        self.apply(super::list_request(), 200)
    }
    pub fn executions_request(&self, id: &str) -> Result<ApiRequest, Error> {
        let id = uuid(id).ok_or(Error::Request)?;
        self.apply(super::executions_request(&id), 100)
    }
}
pub fn get_request(id: &str) -> Result<ApiRequest, Error> {
    let id = uuid(id).ok_or(Error::Request)?;
    Ok(ApiRequest::get(&["api", "v1", "scheduled-tasks", &id]))
}
pub fn execution_request(id: &str, execution_id: &str) -> Result<ApiRequest, Error> {
    let id = uuid(id).ok_or(Error::Request)?;
    let execution_id = uuid(execution_id).ok_or(Error::Request)?;
    Ok(ApiRequest::get(&["api", "v1", "scheduled-tasks", &id, "executions", &execution_id]))
}
fn row_id(row: &Value, key: &str) -> Result<String, Error> {
    row.get(key).and_then(Value::as_str).and_then(uuid).ok_or(Error::Response)
}
pub fn parse_task(value: &Value, id: &str) -> Result<Value, Error> {
    let id = uuid(id).ok_or(Error::Request)?;
    let row = &value["task"];
    if row_id(row, "task_id")? != id { return Err(Error::Response); }
    Ok(row.clone())
}
pub fn parse_execution(value: &Value, id: &str, execution_id: &str) -> Result<Value, Error> {
    let id = uuid(id).ok_or(Error::Request)?;
    let execution_id = uuid(execution_id).ok_or(Error::Request)?;
    let row = &value["execution"];
    if row_id(row, "task_id")? != id || row_id(row, "execution_id")? != execution_id {
        return Err(Error::Response);
    }
    Ok(row.clone())
}
#[derive(Clone, Debug)]
pub struct SchedulePage {
    pub rows: Vec<Value>,
    pub next_cursor: Option<ScheduleCursor>,
}
impl SchedulePage {
    pub fn tasks(value: &Value, query: &ScheduleQuery) -> Result<Self, Error> {
        query.tasks_request()?;
        Self::parse(value, query, "tasks", "task_id", None)
    }
    pub fn executions(value: &Value, id: &str, query: &ScheduleQuery) -> Result<Self, Error> {
        query.executions_request(id)?;
        Self::parse(value, query, "executions", "execution_id", Some(id))
    }
    fn parse(value: &Value, query: &ScheduleQuery, array: &str, key: &str, task: Option<&str>) -> Result<Self, Error> {
        let rows = value[array].as_array().ok_or(Error::Response)?;
        if rows.len() > usize::from(query.limit) { return Err(Error::Response); }
        // A missing cursor is a malformed response, not the end of history.
        let next_cursor: Option<ScheduleCursor> = serde_json::from_value(value.get("next_cursor").ok_or(Error::Response)?.clone()).map_err(|_| Error::Response)?;
        let cursor_key = |c: &ScheduleCursor| -> Result<_, Error> {
            Ok((timestamp(&c.before_at).ok_or(Error::Response)?, uuid(&c.before_id).ok_or(Error::Response)?))
        };
        let mut previous = query.cursor.as_ref().map(cursor_key).transpose()?;
        let expected_task = task.map(|id| uuid(id).ok_or(Error::Request)).transpose()?;
        let mut ids = std::collections::HashSet::new();
        for row in rows {
            let id = row_id(row, key)?;
            if !ids.insert(id.clone()) { return Err(Error::Response); }
            if let Some(expected) = &expected_task {
                if row_id(row, "task_id")? != *expected { return Err(Error::Response); }
            }
            let at = row["created_at"].as_str().and_then(timestamp).ok_or(Error::Response)?;
            let current = (at, id);
            if previous.as_ref().is_some_and(|p| current >= *p) { return Err(Error::Response); }
            previous = Some(current);
        }
        if let Some(cursor) = &next_cursor {
            if rows.len() != usize::from(query.limit) || previous.as_ref() != Some(&cursor_key(cursor)?) {
                return Err(Error::Response);
            }
        }
        Ok(Self { rows: rows.clone(), next_cursor })
    }
}
impl Client {
    pub async fn scheduled_tasks_page(&self, query: &ScheduleQuery) -> Result<Value, Error> {
        let value = self.execute(query.tasks_request()?).await?;
        SchedulePage::tasks(&value, query)?;
        Ok(value)
    }
    pub async fn scheduled_task_executions_page(&self, id: &str, query: &ScheduleQuery) -> Result<Value, Error> {
        let value = self.execute(query.executions_request(id)?).await?;
        SchedulePage::executions(&value, id, query)?;
        Ok(value)
    }
    pub async fn scheduled_task(&self, id: &str) -> Result<Value, Error> {
        let value = self.execute(get_request(id)?).await?;
        parse_task(&value, id)?;
        Ok(value)
    }
    pub async fn scheduled_task_execution(&self, id: &str, execution_id: &str) -> Result<Value, Error> {
        let value = self.execute(execution_request(id, execution_id)?).await?;
        parse_execution(&value, id, execution_id)?;
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    const TASK: &str = "00000000-0000-0000-0000-000000000001";
    const EXEC: &str = "00000000-0000-0000-0000-000000000003";
    const OLDER: &str = "00000000-0000-0000-0000-000000000002";
    const AT: &str = "2026-10-13T02:00:00+00:00";
    fn row(id: &str) -> Value { json!({"task_id":TASK,"execution_id":id,"created_at":AT,"private_extension":{"kept":true}}) }
    fn query(limit: u16) -> ScheduleQuery { ScheduleQuery { limit, cursor:None } }
    #[test]
    fn requests_are_bounded_read_only_and_exact() {
        for q in [query(1), query(200)] { assert!(!q.tasks_request().unwrap().is_mutation()); }
        for q in [query(0), query(201)] { assert!(q.tasks_request().is_err()); }
        assert!(query(101).executions_request(TASK).is_err());
        for request in [get_request(TASK).unwrap(), execution_request(TASK,EXEC).unwrap(), query(100).executions_request(TASK).unwrap()] {
            assert_eq!(request.method, reqwest::Method::GET);
            assert!(request.body.is_none()); assert!(request.headers.is_empty());
        }
        assert_eq!(execution_request(TASK,EXEC).unwrap().segments, ["api","v1","scheduled-tasks",TASK,"executions",EXEC]);
        for id in ["", "../task", "bad", "00000000-0000-0000-0000-000000000001/extra"] {
            assert!(get_request(id).is_err()); assert!(execution_request(TASK,id).is_err());
            assert!(query(20).executions_request(id).is_err());
        }
        let q = ScheduleQuery { limit:20, cursor:Some(ScheduleCursor{before_at:AT.into(),before_id:EXEC.into()}) };
        assert_eq!(q.tasks_request().unwrap().query, vec![("limit".into(),"20".into()),("before_at".into(),AT.into()),("before_id".into(),EXEC.into())]);
        for (at,id) in [("bad",EXEC),(AT,"bad"),("2026-02-30T00:00:00Z",EXEC)] {
            assert!(ScheduleQuery{limit:20,cursor:Some(ScheduleCursor{before_at:at.into(),before_id:id.into()})}.tasks_request().is_err());
        }
    }
    #[test]
    fn latest_older_and_exact_receipts_preserve_unknown_fields() {
        let first = json!({"executions":[row(EXEC)],"next_cursor":{"before_at":AT,"before_id":EXEC}});
        let page = SchedulePage::executions(&first,TASK,&query(1)).unwrap();
        let q = ScheduleQuery{limit:1,cursor:page.next_cursor};
        let older = json!({"executions":[row(OLDER)],"next_cursor":null});
        assert!(SchedulePage::executions(&older,TASK,&q).unwrap().next_cursor.is_none());
        assert!(SchedulePage::executions(&first,TASK,&q).is_err());
        assert_eq!(parse_execution(&json!({"execution":row(EXEC)}),TASK,EXEC).unwrap()["private_extension"]["kept"],true);
        assert!(parse_execution(&json!({"execution":row(EXEC)}),TASK,OLDER).is_err());
        assert!(parse_execution(&json!({"execution":row(EXEC)}),OLDER,EXEC).is_err());
        assert!(parse_task(&json!({"task":row(EXEC)}),TASK).is_ok());
        assert!(parse_task(&json!({"task":row(EXEC)}),OLDER).is_err());
    }
    #[test]
    fn malformed_or_cross_task_pages_do_not_truncate_history() {
        let valid=json!({"executions":[row(EXEC),row(OLDER)],"next_cursor":null});
        assert!(SchedulePage::executions(&valid,TASK,&query(2)).is_ok());
        let mut cases=vec![json!({"executions":[]}),json!({"executions":[],"next_cursor":{}}),json!({"executions":[row(EXEC),row(EXEC)],"next_cursor":null}),json!({"executions":[row(OLDER),row(EXEC)],"next_cursor":null}),json!({"executions":[row(EXEC)],"next_cursor":{"before_at":AT,"before_id":OLDER}})];
        let mut wrong=valid.clone(); wrong["executions"][0]["task_id"]=json!(OLDER); cases.push(wrong);
        let mut bad=valid.clone(); bad["executions"][0]["created_at"]=json!("bad"); cases.push(bad);
        for value in cases { assert!(SchedulePage::executions(&value,TASK,&query(2)).is_err(),"{value}"); }
        assert!(SchedulePage::executions(&valid,TASK,&query(1)).is_err());
    }
    #[test]
    fn offset_equivalent_cursor_and_timestamp_ties_use_uuid() {
        let q=ScheduleQuery{limit:1,cursor:Some(ScheduleCursor{before_at:"2026-10-13T03:00:00+01:00".into(),before_id:EXEC.into()})};
        assert!(SchedulePage::executions(&json!({"executions":[row(OLDER)],"next_cursor":null}),TASK,&q).is_ok());
        assert!(SchedulePage::executions(&json!({"executions":[row(EXEC)],"next_cursor":null}),TASK,&q).is_err());
    }
    #[test]
    fn old_constructors_and_write_semantics_remain_unchanged() {
        assert!(super::super::list_request().query.is_empty());
        assert!(super::super::executions_request(TASK).query.is_empty());
        for action in [super::super::ScheduleAction::Pause,super::super::ScheduleAction::Resume,super::super::ScheduleAction::Cancel] {
            let request=super::super::action_request(TASK,action);
            assert_eq!(request.method,reqwest::Method::POST);assert!(request.query.is_empty());assert!(request.body.is_none());
        }
    }
}
