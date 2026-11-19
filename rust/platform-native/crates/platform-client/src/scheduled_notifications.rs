//! Owner-scoped scheduled-task outcome receipts, not execution or delivery control.
//! Listing and opening are read-only. Acknowledgements are explicit, single attempts.
use crate::{ApiRequest, Client, Error};
use reqwest::Method;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use std::collections::HashSet;

const MAX_PAGE: usize = 100;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NotificationFilter {
    #[default]
    All,
    Unread,
}
impl NotificationFilter {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Unread => "unread",
        }
    }
    pub fn parse(value: &str) -> Result<Self, Error> {
        match value {
            "all" => Ok(Self::All),
            "unread" => Ok(Self::Unread),
            _ => Err(Error::Request),
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct NotificationCursor {
    pub before_at: String,
    pub before_id: String,
}
#[derive(Clone, Debug)]
pub struct NotificationQuery {
    pub filter: NotificationFilter,
    pub limit: u16,
    pub cursor: Option<NotificationCursor>,
}
impl Default for NotificationQuery {
    fn default() -> Self {
        Self { filter: NotificationFilter::All, limit: 50, cursor: None }
    }
}
impl NotificationQuery {
    pub fn request(&self) -> Result<ApiRequest, Error> {
        if !(1..=MAX_PAGE as u16).contains(&self.limit) {
            return Err(Error::Request);
        }
        let mut request = ApiRequest::get(&["api", "v1", "scheduled-task-notifications"])
            .query("filter", self.filter.as_str())
            .query("limit", self.limit);
        if let Some(cursor) = &self.cursor {
            timestamp(&cursor.before_at).ok_or(Error::Request)?;
            let id = uuid(&cursor.before_id).ok_or(Error::Request)?;
            request = request.query("before_at", &cursor.before_at).query("before_id", id);
        }
        Ok(request)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScheduledOutcomeStatus {
    Succeeded,
    Failed,
    Cancelled,
}
impl ScheduledOutcomeStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ScheduledNotificationTask {
    pub task_id: String,
    pub title: String,
    pub status: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ScheduledNotificationExecution {
    pub execution_id: String,
    pub task_id: String,
    pub session_id: String,
    #[serde(deserialize_with = "nullable")]
    pub run_id: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub runtime_thread_id: Option<String>,
    pub scheduled_at: String,
    pub attempt: u8,
    pub status: ScheduledOutcomeStatus,
    pub after_sequence: u64,
    pub cancel_requested: bool,
    #[serde(deserialize_with = "nullable")]
    pub error_code: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(deserialize_with = "nullable")]
    pub finished_at: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ScheduledNotification {
    pub notification_id: String,
    pub occurred_at: String,
    #[serde(deserialize_with = "nullable")]
    pub read_at: Option<String>,
    pub task: ScheduledNotificationTask,
    pub execution: ScheduledNotificationExecution,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ScheduledNotificationPage {
    pub notifications: Vec<ScheduledNotification>,
    pub unread_count: u64,
    #[serde(deserialize_with = "nullable")]
    pub next_cursor: Option<NotificationCursor>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ScheduledNotificationSummary {
    pub unread_count: u64,
    pub total_count: u64,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct NotificationAcknowledgement {
    pub notification_id: String,
    #[serde(deserialize_with = "nullable")]
    pub read_at: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct NotificationAcknowledgements {
    pub notifications: Vec<NotificationAcknowledgement>,
}

// Nullable fields are required on the wire: an omitted read_at must not silently
// become an unread receipt, and an omitted next_cursor must not truncate history.
fn nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}
impl ScheduledNotification {
    /// Parses the detail envelope and binds it to the exact requested receipt.
    pub fn parse(value: Value, id: &str) -> Result<Self, Error> {
        let expected = uuid(id).ok_or(Error::Request)?;
        #[derive(Deserialize)]
        struct Detail { notification: ScheduledNotification }
        let detail: Detail = serde_json::from_value(value).map_err(|_| Error::Response)?;
        detail.notification.validate()?;
        if uuid(&detail.notification.notification_id).as_deref() != Some(expected.as_str()) {
            return Err(Error::Response);
        }
        Ok(detail.notification)
    }
    fn validate(&self) -> Result<(), Error> {
        let execution = &self.execution;
        let id = uuid(&self.notification_id).ok_or(Error::Response)?;
        let task_id = uuid(&self.task.task_id).ok_or(Error::Response)?;
        let occurred_at = timestamp(&self.occurred_at).ok_or(Error::Response)?;
        if uuid(&execution.execution_id).as_deref() != Some(id.as_str())
            || uuid(&execution.task_id).as_deref() != Some(task_id.as_str())
            || self.task.title.trim().is_empty()
            || self.task.title.chars().count() > 120
            || !["active", "paused", "completed", "cancelled"].contains(&self.task.status.as_str())
            || execution.session_id.is_empty()
            || execution.session_id.len() > 255
            || execution.session_id.chars().any(char::is_control)
            || execution.attempt > 3
        {
            return Err(Error::Response);
        }
        for id in [&execution.run_id, &execution.runtime_thread_id].into_iter().flatten() {
            uuid(id).ok_or(Error::Response)?;
        }
        for at in [&execution.scheduled_at, &execution.created_at, &execution.updated_at] {
            timestamp(at).ok_or(Error::Response)?;
        }
        for at in [&self.read_at, &execution.finished_at].into_iter().flatten() {
            timestamp(at).ok_or(Error::Response)?;
        }
        // The Gateway projection uses finished_at, falling back to updated_at.
        let outcome_at = execution.finished_at.as_ref().unwrap_or(&execution.updated_at);
        if timestamp(outcome_at) != Some(occurred_at) {
            return Err(Error::Response);
        }
        Ok(())
    }
}
impl ScheduledNotificationPage {
    /// Accepts a whole page atomically. Rejects malformed/replayed keysets before
    /// callers replace their visible rows or advance a cursor.
    pub fn parse(value: Value, query: &NotificationQuery) -> Result<Self, Error> {
        query.request()?;
        let page: Self = serde_json::from_value(value).map_err(|_| Error::Response)?;
        if page.notifications.len() > usize::from(query.limit) {
            return Err(Error::Response);
        }
        let mut previous = query.cursor.as_ref().map(cursor_key).transpose()?;
        let mut ids = HashSet::new();
        let mut unread = 0;
        for notification in &page.notifications {
            notification.validate()?;
            let key = (
                timestamp(&notification.occurred_at).ok_or(Error::Response)?,
                uuid(&notification.notification_id).ok_or(Error::Response)?,
            );
            if !ids.insert(key.1.clone())
                || previous.as_ref().is_some_and(|last| &key >= last)
                || (query.filter == NotificationFilter::Unread && notification.read_at.is_some())
            {
                return Err(Error::Response);
            }
            if notification.read_at.is_none() { unread += 1; }
            previous = Some(key);
        }
        if page.unread_count < unread {
            return Err(Error::Response);
        }
        if let Some(cursor) = &page.next_cursor {
            if page.notifications.len() != usize::from(query.limit)
                || previous.as_ref() != Some(&cursor_key(cursor)?)
            {
                return Err(Error::Response);
            }
        }
        Ok(page)
    }
}
impl ScheduledNotificationSummary {
    pub fn parse(value: Value) -> Result<Self, Error> {
        let summary: Self = serde_json::from_value(value).map_err(|_| Error::Response)?;
        if summary.unread_count > summary.total_count {
            return Err(Error::Response);
        }
        Ok(summary)
    }
}
impl NotificationAcknowledgements {
    /// A malformed echo after POST is an unknown outcome, never permission to retry.
    pub fn parse(value: Value, ids: &[String], read: bool) -> Result<Self, Error> {
        let expected: HashSet<_> = normalize_ids(ids)?.into_iter().collect();
        let mut result: Self = serde_json::from_value(value).map_err(|_| Error::Response)?;
        if result.notifications.len() != expected.len() {
            return Err(Error::Response);
        }
        let mut seen = HashSet::new();
        for notification in &result.notifications {
            let id = uuid(&notification.notification_id).ok_or(Error::Response)?;
            if !expected.contains(&id) || !seen.insert(id) || notification.read_at.is_some() != read {
                return Err(Error::Response);
            }
            if let Some(at) = &notification.read_at { timestamp(at).ok_or(Error::Response)?; }
        }
        // UPDATE RETURNING has no ordering guarantee; provide stable CLI JSON.
        result.notifications.sort_by(|a, b| a.notification_id.cmp(&b.notification_id));
        Ok(result)
    }
}

pub fn summary_request() -> ApiRequest {
    ApiRequest::get(&["api", "v1", "scheduled-task-notifications", "summary"])
}
pub fn get_request(id: &str) -> Result<ApiRequest, Error> {
    let id = uuid(id).ok_or(Error::Request)?;
    Ok(ApiRequest::get(&["api", "v1", "scheduled-task-notifications", &id]))
}
pub fn mark_request(id: &str, read: bool) -> Result<ApiRequest, Error> {
    let id = uuid(id).ok_or(Error::Request)?;
    Ok(ApiRequest::mutation(
        Method::POST,
        &["api", "v1", "scheduled-task-notifications", &id, if read { "read" } else { "unread" }],
        Some(serde_json::json!({})),
    ))
}
/// Only these explicitly supplied receipts are acknowledged; there is no mark-all.
pub fn mark_read_request(ids: &[String]) -> Result<ApiRequest, Error> {
    let ids = normalize_ids(ids)?;
    Ok(ApiRequest::mutation(
        Method::POST,
        &["api", "v1", "scheduled-task-notifications", "read"],
        Some(serde_json::json!({"notification_ids": ids})),
    ))
}
fn normalize_ids(ids: &[String]) -> Result<Vec<String>, Error> {
    if ids.is_empty() || ids.len() > MAX_PAGE { return Err(Error::Request); }
    let mut seen = HashSet::new();
    ids.iter().map(|id| {
        let id = uuid(id).ok_or(Error::Request)?;
        if !seen.insert(id.clone()) { return Err(Error::Request); }
        Ok(id)
    }).collect()
}
impl Client {
    pub async fn scheduled_notifications(&self, query: &NotificationQuery) -> Result<ScheduledNotificationPage, Error> {
        ScheduledNotificationPage::parse(self.execute(query.request()?).await?, query)
    }
    pub async fn scheduled_notification_summary(&self) -> Result<ScheduledNotificationSummary, Error> {
        ScheduledNotificationSummary::parse(self.execute(summary_request()).await?)
    }
    pub async fn scheduled_notification(&self, id: &str) -> Result<ScheduledNotification, Error> {
        ScheduledNotification::parse(self.execute(get_request(id)?).await?, id)
    }
    /// Single attempt, including when the acknowledgement echo cannot be validated.
    pub async fn mark_scheduled_notification(&self, id: &str, read: bool) -> Result<NotificationAcknowledgements, Error> {
        NotificationAcknowledgements::parse(self.execute(mark_request(id, read)?).await?, &[id.into()], read)
    }
    /// Explicit bounded batch; partial/malformed echoes are rejected atomically.
    pub async fn mark_scheduled_notifications_read(&self, ids: &[String]) -> Result<NotificationAcknowledgements, Error> {
        NotificationAcknowledgements::parse(self.execute(mark_read_request(ids)?).await?, ids, true)
    }
}

pub(crate) fn uuid(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    if bytes.len() != 36 || !bytes.iter().enumerate().all(|(i, b)| {
        if [8, 13, 18, 23].contains(&i) { *b == b'-' } else { b.is_ascii_hexdigit() }
    }) { return None; }
    Some(value.to_ascii_lowercase())
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Timestamp { seconds: i64, nanos: u32 }
fn cursor_key(cursor: &NotificationCursor) -> Result<(Timestamp, String), Error> {
    Ok((timestamp(&cursor.before_at).ok_or(Error::Response)?, uuid(&cursor.before_id).ok_or(Error::Response)?))
}
// Normalize RFC3339 offsets and fractional precision before keyset comparison.
// This is a bounded wire parser, not a scheduler; no clock or date dependency.
pub(crate) fn timestamp(value: &str) -> Option<Timestamp> {
    let b = value.as_bytes();
    if !(20..=35).contains(&b.len()) || !value.is_ascii()
        || b[4] != b'-' || b[7] != b'-' || ![b'T', b't'].contains(&b[10])
        || b[13] != b':' || b[16] != b':' { return None; }
    fn number(bytes: &[u8]) -> Option<i64> {
        bytes.iter().try_fold(0, |n, b| {
            if b.is_ascii_digit() { Some(n * 10 + i64::from(b - b'0')) } else { None }
        })
    }
    let year = number(&b[..4])?;
    let month = number(&b[5..7])?;
    let day = number(&b[8..10])?;
    let hour = number(&b[11..13])?;
    let minute = number(&b[14..16])?;
    let second = number(&b[17..19])?;
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let months = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    if !(1..=12).contains(&month) || !(1..=months[(month - 1) as usize]).contains(&day)
        || hour > 23 || minute > 59 || second > 59 { return None; }
    let mut end = 19;
    let mut nanos = 0;
    if b[end] == b'.' {
        end += 1;
        let start = end;
        while end < b.len() && b[end].is_ascii_digit() { end += 1; }
        let digits = end - start;
        if !(1..=9).contains(&digits) { return None; }
        nanos = number(&b[start..end])? as u32 * 10_u32.pow((9 - digits) as u32);
    }
    let zone = &b[end..];
    let offset = if zone == b"Z" || zone == b"z" { 0 } else {
        if zone.len() != 6 || ![b'+', b'-'].contains(&zone[0]) || zone[3] != b':' { return None; }
        let h = number(&zone[1..3])?;
        let m = number(&zone[4..6])?;
        if h > 23 || m > 59 { return None; }
        (h * 3600 + m * 60) * if zone[0] == b'+' { 1 } else { -1 }
    };
    let y = year - 1;
    let days = 365 * y + y.div_euclid(4) - y.div_euclid(100) + y.div_euclid(400)
        + months[..(month - 1) as usize].iter().sum::<i64>() + day - 1;
    Some(Timestamp { seconds: days * 86400 + hour * 3600 + minute * 60 + second - offset, nanos })
}
