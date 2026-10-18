//! Native Assistant SDK workflows. Mutations are sent once; uncertain admissions are explicit.
use crate::{ApiRequest, Client, Error, sdk_stream::SdkEvent};
use reqwest::Method;
use serde_json::{Value, json};
use std::time::Duration;
use tokio::sync::{mpsc, watch};
fn id(value: &str) -> Result<(), Error> {
    if value.trim().is_empty()
        || value.chars().count() > 512
        || value.contains(['/', '\\', '?', '#', '%', '\0', '\r', '\n'])
    {
        Err(Error::Request)
    } else {
        Ok(())
    }
}
fn skill(value: &str) -> Result<(), Error> {
    if !(3..=50).contains(&value.len())
        || !value.as_bytes()[0].is_ascii_lowercase()
        || !value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        Err(Error::Request)
    } else {
        Ok(())
    }
}
fn task(value: Value, expected: Option<&str>) -> Result<Value, Error> {
    let task_id = value["task_id"].as_str().ok_or(Error::Response)?;
    id(task_id).map_err(|_| Error::Response)?;
    if expected.is_some_and(|s| s != task_id)
        || value["status"].as_str().is_none_or(|s| s.trim().is_empty())
    {
        return Err(Error::Response);
    }
    Ok(value)
}
/// A definitive client rejection keeps the original redacted typed error.
/// Admission uncertainty never supplies a task ID or permits resubmission.
fn async_image_admission(result: Result<Value, Error>) -> Result<Value, Error> {
    match result {
        Ok(value) => task(value, None).map_err(|_| Error::UnknownOutcome),
        Err(error) if matches!(error.status(), Some(400..=499)) && error.status() != Some(408) => {
            Err(error)
        }
        Err(_) => Err(Error::UnknownOutcome),
    }
}
impl Client {
    pub async fn assistant_chat(&self, mut body: Value) -> Result<Value, Error> {
        if body["message"].as_str().is_none() {
            return Err(Error::Request);
        }
        body["stream"] = json!(false);
        self.execute(ApiRequest::mutation(
            Method::POST,
            &["api", "v1", "assistant", "chat"],
            Some(body),
        ))
        .await
    }
    pub async fn assistant_image(&self, body: Value, asynchronous: bool) -> Result<Value, Error> {
        if body["prompt"].as_str().is_none() {
            return Err(Error::Request);
        }
        if asynchronous
            && body["model_id"]
                .as_str()
                .is_none_or(|s| s.trim().is_empty())
        {
            return Err(Error::Request);
        }
        let route = if asynchronous {
            "generate-image-async"
        } else {
            "generate-image"
        };
        let result = self
            .execute(ApiRequest::mutation(
                Method::POST,
                &["api", "v1", "assistant", route],
                Some(body),
            ))
            .await;
        if !asynchronous {
            return result;
        }
        async_image_admission(result)
    }
    pub async fn assistant_image_task(&self, task_id: &str) -> Result<Value, Error> {
        id(task_id)?;
        task(
            self.get(&["api", "v1", "assistant", "image-task", task_id])
                .await?,
            Some(task_id),
        )
    }
    pub async fn assistant_cancel_task(
        &self,
        task_id: &str,
        reason: Option<&str>,
    ) -> Result<Value, Error> {
        id(task_id)?;
        let mut body = json!({});
        if let Some(reason) = reason {
            body["reason"] = json!(reason)
        }
        let value = self
            .post(
                &["api", "v1", "assistant", "tasks", task_id, "cancel"],
                body,
            )
            .await?;
        if value["task_id"] != task_id || !value["cancelled"].is_boolean() {
            return Err(Error::UnknownOutcome);
        }
        Ok(value)
    }
    pub async fn instruction_skills(&self, enabled_only: bool) -> Result<Value, Error> {
        let value = self
            .execute(ApiRequest::get(&["api", "v1", "skills"]).query("enabled_only", enabled_only))
            .await?;
        let rows = value["skills"].as_array().ok_or(Error::Response)?;
        if value["total"].as_u64().is_none() {
            return Err(Error::Response);
        }
        for row in rows {
            skill(row["name"].as_str().ok_or(Error::Response)?).map_err(|_| Error::Response)?;
        }
        Ok(value)
    }
    pub async fn instruction_skill(&self, name: &str) -> Result<Value, Error> {
        skill(name)?;
        let value = self.get(&["api", "v1", "skills", name]).await?;
        if value["name"] != name {
            return Err(Error::Response);
        }
        Ok(value)
    }
    pub async fn test_instruction_skill(&self, name: &str, input: &str) -> Result<Value, Error> {
        skill(name)?;
        let value = self
            .post(
                &["api", "v1", "skills", name, "test"],
                json!({"input":input}),
            )
            .await?;
        if !value["success"].is_boolean() {
            return Err(Error::Response);
        }
        Ok(value)
    }
    pub async fn set_instruction_enabled(&self, name: &str, enabled: bool) -> Result<Value, Error> {
        skill(name)?;
        self.post(
            &[
                "api",
                "v1",
                "skills",
                name,
                if enabled { "enable" } else { "disable" },
            ],
            json!({}),
        )
        .await
    }
    pub async fn assistant_tools(&self) -> Result<Value, Error> {
        self.get(&["api", "v1", "assistant", "tools"]).await
    }
    pub async fn assistant_create_session(&self, body: Value) -> Result<Value, Error> {
        self.post(&["api", "v1", "assistant", "sessions"], body)
            .await
    }
    pub async fn assistant_session(&self, session: &str) -> Result<Value, Error> {
        id(session)?;
        self.get(&["api", "v1", "assistant", "sessions", session])
            .await
    }
    pub async fn assistant_delete_session(&self, session: &str) -> Result<(), Error> {
        id(session)?;
        self.execute(ApiRequest::mutation(
            Method::DELETE,
            &["api", "v1", "assistant", "sessions", session],
            None,
        ))
        .await
        .map(|_| ())
    }
    pub async fn assistant_artifact(&self, artifact: &str) -> Result<Value, Error> {
        id(artifact)?;
        self.get(&["api", "v1", "assistant", "artifacts", artifact])
            .await
    }
    pub async fn assistant_create_artifact(&self, body: Value) -> Result<Value, Error> {
        if !body.is_object() {
            return Err(Error::Request);
        }
        self.post(&["api", "v1", "assistant", "artifacts"], body)
            .await
    }
    pub async fn assistant_delete_artifact(&self, artifact: &str) -> Result<(), Error> {
        id(artifact)?;
        self.execute(ApiRequest::mutation(
            Method::DELETE,
            &["api", "v1", "assistant", "artifacts", artifact],
            None,
        ))
        .await
        .map(|_| ())
    }
    pub async fn assistant_session_artifacts(&self, session: &str) -> Result<Value, Error> {
        id(session)?;
        self.get(&["api", "v1", "assistant", "sessions", session, "artifacts"])
            .await
    }
    pub fn assistant_artifact_url(&self, artifact: &str) -> Result<url::Url, Error> {
        id(artifact)?;
        self.endpoint(&["api", "v1", "assistant", "artifacts", artifact, "download"])
    }
    pub async fn assistant_download_artifact(
        &self,
        artifact: &str,
        max_bytes: usize,
    ) -> Result<Vec<u8>, Error> {
        if max_bytes == 0 || max_bytes > 64 * 1024 * 1024 {
            return Err(Error::Request);
        }
        let mut response = self
            .authorize(self.http.get(self.assistant_artifact_url(artifact)?))?
            .send()
            .await
            .map_err(|_| Error::Transport)?;
        if !response.status().is_success() {
            return Err(Error::Status(response.status().as_u16()));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| Error::Transport)? {
            if bytes.len().saturating_add(chunk.len()) > max_bytes {
                return Err(Error::Response);
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }
    pub async fn assistant_thread(&self, thread: &str) -> Result<Value, Error> {
        id(thread)?;
        self.get(&["api", "v2", "agent", "threads", thread]).await
    }
    pub async fn assistant_thread_turn(&self, thread: &str, body: Value) -> Result<Value, Error> {
        id(thread)?;
        if body["message"].as_str().is_none() {
            return Err(Error::Request);
        }
        self.post(&["api", "v2", "agent", "threads", thread, "turns"], body)
            .await
    }
    pub async fn assistant_approval(&self, thread: &str, approval: &str) -> Result<Value, Error> {
        id(thread)?;
        id(approval)?;
        self.get(&[
            "api",
            "v2",
            "agent",
            "threads",
            thread,
            "approvals",
            approval,
        ])
        .await
    }
    pub async fn assistant_decide_approval(
        &self,
        thread: &str,
        approval: &str,
        approved: bool,
        reason: Option<&str>,
    ) -> Result<Value, Error> {
        id(thread)?;
        id(approval)?;
        let mut body = json!({"approved":approved});
        if let Some(reason) = reason {
            body["reason"] = json!(reason)
        }
        self.post(
            &[
                "api",
                "v2",
                "agent",
                "threads",
                thread,
                "approvals",
                approval,
                "decision",
            ],
            body,
        )
        .await
    }
    /// Dropping receiver or setting cancel closes the HTTP body. Never resubmits the POST.
    pub async fn assistant_stream(
        &self,
        mut body: Value,
        sender: mpsc::Sender<Result<SdkEvent, Error>>,
        mut cancel: watch::Receiver<bool>,
    ) -> Result<(), Error> {
        if body["message"].as_str().is_none() {
            return Err(Error::Request);
        }
        if *cancel.borrow() || sender.is_closed() {
            return Ok(());
        }
        body["stream"] = json!(true);
        let http = reqwest::Client::builder()
            .retry(reqwest::retry::never())
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|_| Error::Transport)?;
        let request = self
            .authorize(http.post(self.endpoint(&["api", "v1", "assistant", "chat", "stream"])?))?
            .header("Accept", "text/event-stream")
            .json(&body);
        let response = tokio::select! {r=tokio::time::timeout(self.stream_limits.header_timeout(),request.send())=>r.map_err(|_|Error::UnknownOutcome)?.map_err(|_|Error::UnknownOutcome)?,_ = cancel.changed()=>return Ok(()),_ = sender.closed()=>return Ok(())};
        crate::sdk_stream::receive(response, sender, cancel, Some, self.stream_limits).await
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn skill_names_and_task_receipts() {
        assert!(skill("my-skill").is_ok());
        for n in ["a", "Abc", "../skill", "a/b"] {
            assert!(skill(n).is_err())
        }
        assert!(task(json!({"task_id":"t","status":"queued"}), Some("t")).is_ok());
        assert!(task(json!({"task_id":"other","status":"done"}), Some("t")).is_err());
    }
}

impl Client {
    pub async fn send(
        &self,
        request: crate::sdk_models::ChatRequest,
    ) -> Result<crate::sdk_models::ChatResponse, Error> {
        serde_json::from_value(self.assistant_chat(request.to_value()?).await?)
            .map_err(|_| Error::Response)
    }
    pub async fn generate_image(
        &self,
        request: crate::sdk_models::ImageRequest,
    ) -> Result<crate::sdk_models::ImageResult, Error> {
        let body = serde_json::to_value(request).map_err(|_| Error::Request)?;
        serde_json::from_value(self.assistant_image(body, false).await?)
            .map_err(|_| Error::Response)
    }
    pub async fn create_session(
        &self,
        request: crate::sdk_models::SessionCreateRequest,
    ) -> Result<crate::sdk_models::Session, Error> {
        serde_json::from_value(self.assistant_create_session(request.to_value()).await?)
            .map_err(|_| Error::Response)
    }
    pub async fn list_sessions(
        &self,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<crate::sdk_models::Session>, Error> {
        let value = self
            .execute(
                ApiRequest::get(&["api", "v1", "assistant", "sessions"])
                    .query("limit", limit)
                    .query("offset", offset),
            )
            .await?;
        let rows = value.get("sessions").unwrap_or(&value);
        serde_json::from_value(rows.clone()).map_err(|_| Error::Response)
    }
    pub async fn session_history(
        &self,
        session: &str,
        limit: u64,
    ) -> Result<Vec<crate::sdk_models::Message>, Error> {
        id(session)?;
        let value = self
            .execute(
                ApiRequest::get(&["api", "v1", "assistant", "sessions", session, "history"])
                    .query("limit", limit),
            )
            .await?;
        serde_json::from_value(value.get("messages").unwrap_or(&value).clone())
            .map_err(|_| Error::Response)
    }
    pub async fn ask(
        &self,
        question: &str,
        datasets: Vec<String>,
        mut options: crate::sdk_models::ChatRequest,
    ) -> Result<crate::sdk_models::ChatResponse, Error> {
        options.message = question.into();
        options.kb_dataset_ids = Some(datasets);
        self.send(options).await
    }
    pub async fn immutable_source(
        &self,
        dataset: &str,
        document: &str,
        version: u32,
        hash: &str,
    ) -> Result<Value, Error> {
        for s in [dataset, document] {
            if s.is_empty()
                || s != s.trim()
                || s.len() > 1024
                || s == "."
                || s == ".."
                || s.contains(['/', '\\', '?', '#', '%'])
                || s.chars().any(char::is_control)
            {
                return Err(Error::Request);
            }
        }
        if version == 0
            || version > i32::MAX as u32
            || hash.len() != 64
            || !hash.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(Error::Request);
        }
        let hash = hash.to_ascii_lowercase();
        let url = self.endpoint(&[
            "api",
            "v1",
            "knowledge",
            dataset,
            "documents",
            document,
            "versions",
            &version.to_string(),
            "source",
        ])?;
        let mut response = self
            .authorize(
                self.http
                    .get(url)
                    .query(&[("hash", &hash)])
                    .header("Accept-Encoding", "identity"),
            )?
            .send()
            .await
            .map_err(|_| Error::Transport)?;
        if !response.status().is_success() {
            return Err(Error::Status(response.status().as_u16()));
        }
        if response
            .headers()
            .get("Content-Encoding")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|s| s != "identity")
        {
            return Err(Error::Response);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| Error::Transport)? {
            if bytes.len() + chunk.len() > 2 * 1024 * 1024 {
                return Err(Error::Response);
            }
            bytes.extend_from_slice(&chunk);
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| Error::Response)?;
        let content = value["content"].as_str().ok_or(Error::Response)?;
        use sha2::{Digest, Sha256};
        if value["document_id"] != document
            || value["version_number"].as_u64() != Some(version as u64)
            || value["source_hash"] != hash
            || !value["title"].is_string()
            || format!("{:x}", Sha256::digest(content.as_bytes())) != hash
        {
            return Err(Error::Response);
        }
        Ok(
            json!({"document_id":document,"version_number":version,"source_hash":hash,"title":value["title"],"content":content}),
        )
    }
}

#[cfg(test)]
mod problem_admission_tests {
    use super::*;
    fn problem(status: u16) -> Error {
        let body=serde_json::to_vec(&json!({"type":"urn:agentforge:problem:test","title":"private-marker","status":status,"detail":{"token":"private-marker"},"instance":"https://private-marker.invalid/request"})).unwrap();
        Error::Problem(Box::new(
            crate::problem::RemoteProblem::decode(status, &body).unwrap(),
        ))
    }
    #[test]
    fn problem_client_rejections_keep_status_and_typed_error() {
        for status in [400, 401, 403, 404, 409, 422, 429, 499] {
            let error = async_image_admission(Err(problem(status))).unwrap_err();
            assert_eq!(error.status(), Some(status));
            assert!(matches!(&error, Error::Problem(_)));
            assert!(!format!("{error:?} {error}").contains("private-marker"));
            assert_eq!(
                async_image_admission(Err(Error::Status(status)))
                    .unwrap_err()
                    .status(),
                Some(status)
            );
        }
    }
    #[test]
    fn problem_deadline_server_and_missing_receipts_keep_unknown_effect_barrier() {
        for status in [408, 500, 501, 502, 503, 504, 599] {
            for error in [problem(status), Error::Status(status)] {
                assert!(matches!(
                    async_image_admission(Err(error)),
                    Err(Error::UnknownOutcome)
                ));
            }
        }
        for error in [
            Error::Transport,
            Error::Response,
            Error::UnknownOutcome,
            Error::Status(302),
        ] {
            assert!(matches!(
                async_image_admission(Err(error)),
                Err(Error::UnknownOutcome)
            ));
        }
        assert!(matches!(
            async_image_admission(Ok(json!({"status":"queued"}))),
            Err(Error::UnknownOutcome)
        ));
        assert!(async_image_admission(Ok(json!({"task_id":"t","status":"queued"}))).is_ok());
    }
}
