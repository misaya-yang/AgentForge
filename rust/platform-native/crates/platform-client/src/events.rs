//! V2 event transport. Only GET is retried; turn admission is never repeated.
use crate::{Client, Error, sse::Decoder};
use serde_json::Value;
use std::time::Duration;
use tokio::sync::{mpsc, watch};
#[derive(Debug)]
pub struct Event {
    pub sequence: u64,
    pub payload: Value,
    pub terminal: bool,
}
fn parse(raw: &str, thread: &str, turn: &str, cursor: u64) -> Result<Option<Event>, Error> {
    let v: Value = serde_json::from_str(raw).map_err(|_| Error::Response)?;
    if v["schema_version"] != "agent-event/v2" {
        return Err(Error::Contract(
            "Agent event schema mismatch: this client requires agent-event/v2; verify the deployed Gateway and Runtime release unit",
        ));
    }
    if v["thread_id"] != thread {
        return Err(Error::Response);
    }
    let sequence = v["sequence"].as_u64().ok_or(Error::Response)?;
    if sequence <= cursor {
        return Ok(None);
    }
    let event = v["event"].as_object().ok_or(Error::Response)?;
    match event.get("turn_id") {
        Some(Value::Null) => {}
        Some(Value::String(t)) if t == turn => {}
        _ => return Err(Error::Response),
    }
    let payload = event
        .get("payload")
        .filter(|p| p.is_object())
        .ok_or(Error::Response)?
        .clone();
    let kind = payload["event_type"].as_str().unwrap_or_default();
    let terminal = matches!(kind, "run_finished" | "run_error" | "cancelled");
    Ok(Some(Event {
        sequence,
        payload,
        terminal,
    }))
}
impl Client {
    pub async fn watch_turn(
        &self,
        thread: &str,
        turn: &str,
        relative: &str,
        sender: mpsc::Sender<Result<Event, Error>>,
        mut cancel: watch::Receiver<bool>,
    ) -> Result<(), Error> {
        let mut url = self.events_url(thread, relative)?;
        let pairs: Vec<_> = url
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        if pairs.iter().filter(|(k, _)| k == "turn_id").count() != 1
            || !pairs.iter().any(|(k, v)| k == "turn_id" && v == turn)
            || pairs.iter().filter(|(k, _)| k == "after_sequence").count() > 1
        {
            return Err(Error::Response);
        }
        let mut cursor = match pairs.iter().find(|(k, _)| k == "after_sequence") {
            Some((_, v)) => v.parse::<u64>().map_err(|_| Error::Response)?,
            None => 0,
        };
        let http = reqwest::Client::builder()
            .retry(reqwest::retry::never())
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .build()
            .map_err(|_| Error::Transport)?;
        loop {
            if *cancel.borrow() || sender.is_closed() {
                return Ok(());
            }
            {
                let mut query = url.query_pairs_mut();
                query.clear();
                for (k, v) in &pairs {
                    if k != "after_sequence" {
                        query.append_pair(k, v);
                    }
                }
                query.append_pair("after_sequence", &cursor.to_string());
            }
            let response = tokio::select! {
                _=cancel.changed()=>return Ok(()),
                _=sender.closed()=>return Ok(()),
                r=tokio::time::timeout(self.stream_limits.header_timeout(),self.authorize(http.get(url.clone()))?.header("accept","text/event-stream").send())=>r,
            };
            let mut retry = Duration::from_secs(2);
            if let Ok(Ok(mut response)) = response {
                let status = response.status().as_u16();
                if response.status().is_success() {
                    let content = response
                        .headers()
                        .get("content-type")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("");
                    if !content
                        .split(';')
                        .next()
                        .is_some_and(|s| s.trim() == "text/event-stream")
                    {
                        return Err(Error::Response);
                    }
                    let mut decoder = Decoder::default();
                    loop {
                        let chunk = tokio::select! {
                            _=cancel.changed()=>return Ok(()),
                            _=sender.closed()=>return Ok(()),
                            chunk=tokio::time::timeout(self.stream_limits.idle_timeout(),response.chunk())=>chunk,
                        };
                        let (frames, eof) = match chunk {
                            Ok(Ok(Some(bytes))) => (decoder.push(&bytes)?, false),
                            Ok(Ok(None)) => (
                                std::mem::take(&mut decoder).finish().unwrap_or_default(),
                                true,
                            ),
                            _ => break,
                        };
                        for frame in frames {
                            let parsed = match parse(&frame, thread, turn, cursor) {
                                Err(_) if eof && serde_json::from_str::<Value>(&frame).is_err() => {
                                    continue;
                                }
                                other => other?,
                            };
                            if let Some(event) = parsed {
                                cursor = event.sequence;
                                let terminal = event.terminal;
                                tokio::select! {_=cancel.changed()=>return Ok(()),
                                _=sender.closed()=>return Ok(()),
                                r=sender.send(Ok(event))=>if r.is_err(){return Ok(())}}
                                if terminal {
                                    return Ok(());
                                }
                            }
                        }
                        if eof {
                            break;
                        }
                    }
                } else if !matches!(status, 408 | 425 | 429 | 500..=599) {
                    return Err(Error::Status(status));
                } else if let Some(seconds) = response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok())
                {
                    retry = Duration::from_secs(seconds.min(3600));
                }
            }
            tokio::select! {_=cancel.changed()=>return Ok(()),_=sender.closed()=>return Ok(()),_=tokio::time::sleep(retry)=>{}}
        }
    }
}
