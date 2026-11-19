//! Native management CLI. The independent Agent CLI and its runtime are unchanged.
use ai_platform_client::{
    ApiRequest, Client, Error,
    management::{NodeEventCursor, TaskQuery, TaskStatus},
    scheduled_notifications::{NotificationCursor, NotificationFilter, NotificationQuery},
    scheduled_tasks::{ScheduleCursor, ScheduleQuery},
};
use serde_json::{Value, json};
use std::{
    io::{IsTerminal, Read, Write},
    time::Duration,
};

#[path = "platformctl/device_auth.rs"]
mod device_auth_cli;
#[path = "platformctl/sdk_services.rs"]
mod sdk_services_cli;

const HELP: &str = "platformctl --origin <https://gateway> --token-stdin <command>

Authentication: platformctl auth login --origin <https://gateway> uses device authorization
and the native OS vault. Normal commands refresh that scoped session as needed.
Use auth status|logout with the same origin. A strict --config FILE may contain
{version:1,origin:https://gateway,client_id:agentforge-cli}; it never contains secrets.
Alternatively pipe an existing Gateway access token with --token-stdin.
Tokens are never accepted in arguments, written to disk or printed by errors.
HTTP is accepted only for a literal loopback IP.

Commands:
  sdk-services get ID                  Sanitized exact SDK service settings
  sdk-services create --file SETTINGS.json --provider-key-file KEYFILE [--yes]
  sdk-services edit ID --file SETTINGS.json [--provider-key-file KEYFILE] [--yes]
  capabilities                         Tenant-visible tools
  policies                             Resolved assistant policies
  tasks list [--status STATUS] [--limit 1..100] [--offset N]
  tasks get ID                         Task details and progress
  tasks result ID                      Completed task output
  tasks watch ID [--interval 1..300]    Poll until terminal; Ctrl-C stops observation
  tasks cancel ID --yes                Submit cancellation exactly once
  runs get ID                          Read-only owner run status/usage recovery
  runs cancel ID --yes                 Cancel a run exactly once
  approvals inspect ID                 Review an SDK action and its binding
  approvals approve|reject ID --run-id ID --tool-call-id ID
                    --arguments-hash SHA256 --yes
  nodes list
  nodes status|capabilities|doctor|grants ID
  nodes events ID [--after N] [--limit 1..500]
  nodes watch ID [--after N] [--interval 1..300]
  schedules list [--limit 1..200] [--before-at RFC3339 --before-id ID]
  schedules executions ID [--limit 1..100] [--before-at RFC3339 --before-id ID]
  schedules get ID                     Read one schedule beyond the current page
  schedules execution ID EXECUTION_ID  Read one exact execution receipt
  schedules create --file PAYLOAD.json --yes
  schedules pause|resume|cancel ID --yes
  notifications list [--filter all|unread] [--limit 1..100]
                     [--before-at RFC3339 --before-id ID]
  notifications summary                Unread and total outcome counts
  notifications get ID                 Open one outcome without marking it read
  notifications read ID [ID ...] --yes  Mark exactly 1..100 supplied IDs read
  notifications unread ID --yes         Mark one outcome unread
  sessions list
  sessions history ID

Output: JSON; watch commands emit newline-delimited JSON.
SDK create/edit without --yes reviews via GET only; --yes permits one write.
Settings JSON: create name/framework/model, optional instructions/max_turns; edit
only model/instructions/max_turns. Framework: openai_agents or claude_agent_sdk.
Provider keys only from a regular UTF-8 file; omitted edit key preserves it.
Token stdin is separate. Do not put keys in settings JSON, argv, or shell history.
Create generates a fresh ID per invocation; review IDs are not reserved/reused.
Create requires atomic /api/v1/service-registrations; no legacy fallback.
Absence preflight is advisory; metadata edits have no CAS.
Exact get recovers uncertain writes, including 404; never automatically retry.
Configured-key state cannot verify a particular replacement. SDK exit codes:
0 review/readback success, 1 request or unconfirmed write, 2 input, 130 Ctrl-C.
Schedule create accepts title, message, model_id, future RFC3339 run_at, optional
interval_seconds (60..31536000) and max_retries (0..3). --yes approves the exact
payload and future executions. Use an explicit timestamp offset.
Schedule list/executions return one keyset page (default 20); omit cursor for latest.
Pass both next_cursor fields unchanged as --before-at and --before-id for older.
Exact reads never dispatch, retry, acknowledge or change a schedule.
Notification list returns one keyset page. Pass both next_cursor fields unchanged
as --before-at and --before-id to continue; no offset or automatic page traversal.
Opening/listing never acknowledges an outcome. Read/unread only changes read state.
Notification exit codes: 0 success (including failed/cancelled outcome receipts),
1 request/response failure or unknown write outcome, 2 invalid input, 130 Ctrl-C.
After an uncertain read-state change, get the supplied IDs before deciding to retry.
runs get is a read-only status/usage snapshot; refresh with another get after disconnect.
Use the genuine SDK assistant_run_id, never a worker session or managed thread ID.
Missing usage is unavailable, never zero; provider cost does not establish settled billing.
Cancellation acknowledgement does not prove remote effects stopped or final usage arrived.
SDK approvals require exact run/tool-call/hash values copied from approvals inspect.
Review the preview before --yes. Decisions recheck the binding and submit it once;
Gateway support for expected_* fields is required. Inspect after an uncertain result.
Preview content can be private. Raw argument objects and unknown fields are omitted.
Mutations are explicit. Ctrl-C never cancels server-side work.
Requests that change data are never retried. Inspect a task after an uncertain cancellation.
";

struct Arguments {
    origin: String,
    words: Vec<String>,
    options: std::collections::BTreeMap<String, String>,
    yes: bool,
    token_stdin: bool,
}
impl Arguments {
    fn parse(args: Vec<String>) -> Result<Self, &'static str> {
        let mut result = Self {
            origin: String::new(),
            words: Vec::new(),
            options: Default::default(),
            yes: false,
            token_stdin: false,
        };
        let mut token_stdin = false;
        let mut iter = args.into_iter();
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--origin" if result.origin.is_empty() => {
                    result.origin = iter.next().ok_or("--origin needs a value")?;
                }
                "--token-stdin" if !token_stdin => token_stdin = true,
                "--yes" if !result.yes => result.yes = true,
                "--status"
                | "--limit"
                | "--offset"
                | "--after"
                | "--interval"
                | "--file"
                | "--filter"
                | "--before-at"
                | "--before-id"
                | "--provider-key-file"
                | "--run-id"
                | "--tool-call-id"
                | "--arguments-hash" => {
                    let value = iter.next().ok_or("Option needs a value")?;
                    if result.options.insert(arg, value).is_some() {
                        return Err("Duplicate option");
                    }
                }
                _ if arg.starts_with('-') => return Err("Unknown or duplicate option; use --help"),
                _ => result.words.push(arg),
            }
        }
        if result.origin.is_empty() {
            return Err("Provide --origin or --config; use --help");
        }
        result.token_stdin = token_stdin;
        Ok(result)
    }
    fn number(&mut self, key: &str, default: u64, min: u64, max: u64) -> Result<u64, &'static str> {
        let value = self
            .options
            .remove(key)
            .map(|s| {
                s.parse::<u64>()
                    .map_err(|_| "Expected a nonnegative integer")
            })
            .transpose()?
            .unwrap_or(default);
        if !(min..=max).contains(&value) {
            return Err("Numeric option is outside its supported range");
        }
        Ok(value)
    }
    fn finish(&self, mutation: bool) -> Result<(), &'static str> {
        if !self.options.is_empty() {
            return Err("Option is not supported by this command");
        }
        if mutation != self.yes {
            return Err("Mutating commands require --yes; omit it for read-only commands");
        }
        Ok(())
    }
}

enum Command {
    SdkService(sdk_services_cli::Action),
    Read(ApiRequest),
    Mutation(ApiRequest),
    WatchTask(String, Duration),
    WatchNode(String, u64, Duration),
    RunGet(String),
    RunCancel(String),
    ApprovalInspect(String),
    ApprovalDecision(
        String,
        ai_platform_client::assistant_runs::ApprovalBinding,
        bool,
    ),
    ScheduleList(ScheduleQuery),
    ScheduleExecutions(String, ScheduleQuery),
    ScheduleGet(String),
    ScheduleExecution(String, String),
    NotificationList(NotificationQuery),
    NotificationSummary,
    NotificationGet(String),
    NotificationMark(Vec<String>, bool),
}
fn schedule_query(args: &mut Arguments, max: u64) -> Result<ScheduleQuery, &'static str> {
    let limit = args.number("--limit", 20, 1, max)? as u16;
    let cursor = match (
        args.options.remove("--before-at"),
        args.options.remove("--before-id"),
    ) {
        (None, None) => None,
        (Some(before_at), Some(before_id)) => Some(ScheduleCursor {
            before_at,
            before_id,
        }),
        _ => return Err("Schedule pagination requires both --before-at and --before-id"),
    };
    Ok(ScheduleQuery { limit, cursor })
}
fn command(args: &mut Arguments) -> Result<Command, &'static str> {
    let words = args.words.clone();
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    if words.first() == Some(&"sdk-services") {
        return sdk_services_cli::parse(args, &words).map(Command::SdkService);
    }
    let result = match words.as_slice() {
        ["capabilities"] => Command::Read(ai_platform_client::management::capabilities_request()),
        ["policies"] => Command::Read(ai_platform_client::management::policies_request()),
        ["tasks", "list"] => {
            let status = args
                .options
                .remove("--status")
                .map(|s| TaskStatus::parse(&s).map_err(|_| "Invalid task status"))
                .transpose()?;
            let limit = args.number("--limit", 50, 1, 100)? as u16;
            let offset = args.number("--offset", 0, 0, u64::MAX)?;
            Command::Read(
                TaskQuery {
                    status,
                    limit,
                    offset,
                }
                .request()
                .map_err(|_| "Invalid task query")?,
            )
        }
        ["tasks", "get", id] => Command::Read(ai_platform_client::management::task_request(id)),
        ["tasks", "result", id] => {
            Command::Read(ai_platform_client::management::task_result_request(id))
        }
        ["tasks", "cancel", id] => {
            Command::Mutation(ai_platform_client::management::cancel_task_request(id))
        }
        ["tasks", "watch", id] => Command::WatchTask(
            (*id).into(),
            Duration::from_secs(args.number("--interval", 2, 1, 300)?),
        ),
        ["runs", action, id] if ["get", "cancel"].contains(action) => {
            ai_platform_client::assistant_runs::validate_id(id)
                .map_err(|_| "Run ID must be a canonical UUID")?;
            if *action == "get" {
                Command::RunGet((*id).into())
            } else {
                Command::RunCancel((*id).into())
            }
        }
        ["approvals", "inspect", id] => {
            ai_platform_client::assistant_runs::validate_id(id)
                .map_err(|_| "Approval ID must be a canonical UUID")?;
            Command::ApprovalInspect((*id).into())
        }
        ["approvals", action, id] if ["approve", "reject"].contains(action) => {
            ai_platform_client::assistant_runs::validate_id(id)
                .map_err(|_| "Approval ID must be a canonical UUID")?;
            let binding = ai_platform_client::assistant_runs::ApprovalBinding {
                run_id: args
                    .options
                    .remove("--run-id")
                    .ok_or("Copy --run-id from approvals inspect")?,
                tool_call_id: args
                    .options
                    .remove("--tool-call-id")
                    .ok_or("Copy --tool-call-id from approvals inspect")?,
                arguments_hash: args
                    .options
                    .remove("--arguments-hash")
                    .ok_or("Copy --arguments-hash from approvals inspect")?,
            };
            binding
                .validate()
                .map_err(|_| "Invalid approval binding; copy the exact inspected values")?;
            Command::ApprovalDecision((*id).into(), binding, *action == "approve")
        }
        ["nodes", "list"] => Command::Read(ApiRequest::get(&["api", "v1", "local-nodes"])),
        ["nodes", view, id] if ["status", "capabilities", "doctor", "grants"].contains(view) => {
            Command::Read(ApiRequest::get(&["api", "v1", "local-nodes", id, view]))
        }
        ["nodes", "events", id] => {
            let after = args.number("--after", 0, 0, u64::MAX)?;
            let limit = args.number("--limit", 100, 1, 500)? as u16;
            Command::Read(
                ai_platform_client::management::node_events_request(id, after, limit)
                    .map_err(|_| "Invalid event query")?,
            )
        }
        ["nodes", "watch", id] => Command::WatchNode(
            (*id).into(),
            args.number("--after", 0, 0, u64::MAX)?,
            Duration::from_secs(args.number("--interval", 2, 1, 300)?),
        ),
        ["schedules", "list"] => {
            let query = schedule_query(args, 200)?;
            query
                .tasks_request()
                .map_err(|_| "Invalid schedule query or cursor")?;
            Command::ScheduleList(query)
        }
        ["schedules", "executions", id] => {
            let query = schedule_query(args, 100)?;
            query
                .executions_request(id)
                .map_err(|_| "Invalid schedule ID, query or cursor")?;
            Command::ScheduleExecutions((*id).into(), query)
        }
        ["schedules", "get", id] => {
            ai_platform_client::scheduled_tasks::get_request(id)
                .map_err(|_| "Invalid schedule ID")?;
            Command::ScheduleGet((*id).into())
        }
        ["schedules", "execution", id, execution_id] => {
            ai_platform_client::scheduled_tasks::execution_request(id, execution_id)
                .map_err(|_| "Invalid schedule or execution ID")?;
            Command::ScheduleExecution((*id).into(), (*execution_id).into())
        }
        ["schedules", "create"] => {
            let path = args
                .options
                .remove("--file")
                .ok_or("Schedule creation requires --file PAYLOAD.json")?;
            if !args.yes {
                return Err("Schedule creation requires --yes");
            }
            let file = std::fs::File::open(path).map_err(|_| "Could not open schedule payload")?;
            if !file
                .metadata()
                .map_err(|_| "Could not inspect schedule payload")?
                .is_file()
            {
                return Err("Schedule payload must be a regular file");
            }
            let mut bytes = Vec::new();
            file.take(1_048_577)
                .read_to_end(&mut bytes)
                .map_err(|_| "Could not read schedule payload")?;
            if bytes.len() > 1_048_576 {
                return Err("Schedule payload exceeds 1 MiB");
            }
            let input: ai_platform_client::scheduled_tasks::CreateScheduledTask =
                serde_json::from_slice(&bytes)
                    .map_err(|_| "Invalid schedule JSON or unsupported fields")?;
            Command::Mutation(
                input
                    .request()
                    .map_err(|_| "Invalid schedule fields; use --help")?,
            )
        }
        ["schedules", action, id] if ["pause", "resume", "cancel"].contains(action) => {
            use ai_platform_client::scheduled_tasks::ScheduleAction;
            let action = match *action {
                "pause" => ScheduleAction::Pause,
                "resume" => ScheduleAction::Resume,
                _ => ScheduleAction::Cancel,
            };
            Command::Mutation(ai_platform_client::scheduled_tasks::action_request(
                id, action,
            ))
        }
        ["notifications", "list"] => {
            let filter = args
                .options
                .remove("--filter")
                .map(|value| {
                    NotificationFilter::parse(&value).map_err(|_| "Invalid notification filter")
                })
                .transpose()?
                .unwrap_or_default();
            let limit = args.number("--limit", 50, 1, 100)? as u16;
            let cursor = match (
                args.options.remove("--before-at"),
                args.options.remove("--before-id"),
            ) {
                (None, None) => None,
                (Some(before_at), Some(before_id)) => Some(NotificationCursor {
                    before_at,
                    before_id,
                }),
                _ => {
                    return Err(
                        "Notification pagination requires both --before-at and --before-id",
                    );
                }
            };
            let query = NotificationQuery {
                filter,
                limit,
                cursor,
            };
            query
                .request()
                .map_err(|_| "Invalid notification query or cursor")?;
            Command::NotificationList(query)
        }
        ["notifications", "summary"] => Command::NotificationSummary,
        ["notifications", "get", id] => {
            ai_platform_client::scheduled_notifications::get_request(id)
                .map_err(|_| "Notification ID must be a hyphenated UUID")?;
            Command::NotificationGet((*id).into())
        }
        ["notifications", "read", ids @ ..] => {
            let ids: Vec<String> = ids.iter().map(|id| (*id).into()).collect();
            ai_platform_client::scheduled_notifications::mark_read_request(&ids)
                .map_err(|_| "Provide 1..100 distinct notification UUIDs")?;
            Command::NotificationMark(ids, true)
        }
        ["notifications", "unread", id] => {
            ai_platform_client::scheduled_notifications::mark_request(id, false)
                .map_err(|_| "Notification ID must be a hyphenated UUID")?;
            Command::NotificationMark(vec![(*id).into()], false)
        }
        ["sessions", "list"] => Command::Read(ApiRequest::get(&["api", "v1", "sessions"])),
        ["sessions", "history", id] => {
            Command::Read(ApiRequest::get(&["api", "v1", "sessions", id, "history"]))
        }
        _ => return Err("Unknown command or wrong number of arguments; use --help"),
    };
    args.finish(matches!(
        result,
        Command::Mutation(_)
            | Command::NotificationMark(_, _)
            | Command::RunCancel(_)
            | Command::ApprovalDecision(_, _, _)
    ))?;
    Ok(result)
}
fn token_from_stdin() -> Result<String, &'static str> {
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        return Err("Pipe a token on standard input; interactive token entry is disabled");
    }
    let mut token = String::new();
    stdin
        .lock()
        .take(16_385)
        .read_to_string(&mut token)
        .map_err(|_| "Could not read token")?;
    if token.len() > 16_384 {
        return Err("Invalid token input");
    }
    let token = token.trim_end_matches(['\r', '\n']).to_owned();
    if token.is_empty() || token.chars().any(char::is_whitespace) {
        return Err("Expected exactly one token on standard input");
    }
    Ok(token)
}
fn output(value: &Value) -> Result<(), &'static str> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    serde_json::to_writer(&mut out, value).map_err(|_| "Could not write output")?;
    writeln!(out)
        .and_then(|_| out.flush())
        .map_err(|_| "Could not write output")
}
// Preserve the existing once-only mutation barrier for both HTTP error forms.
fn uncertain_change(error: &Error) -> bool {
    matches!(error, Error::Transport | Error::Response)
        || matches!(error.status(), Some(408 | 500..=599))
}
async fn run(client: Client, command: Command) -> Result<u8, Box<dyn std::error::Error>> {
    match command {
        Command::SdkService(action) => sdk_services_cli::execute(&client, action).await?,
        Command::Read(request) => output(&client.execute(request).await?)?,
        Command::Mutation(request) => {
            match client.execute(request).await {
                Ok(value) => output(&value)?,
                Err(error) if uncertain_change(&error) => {
                    return Err("Change outcome unknown; inspect the affected task or schedule before retrying".into());
                }
                Err(error) => return Err(error.into()),
            }
        }
        Command::RunGet(id) => output(&client.assistant_run(&id).await?)?,
        Command::ApprovalInspect(id) => output(&client.sdk_approval(&id).await?)?,
        Command::RunCancel(id) => match client.cancel_assistant_run(&id).await {
            Ok(value) => output(&value)?,
            Err(error) if uncertain_change(&error) || matches!(error.status(), Some(300..=399)) => {
                return Err(
                    "Cancellation outcome unknown; use runs get before deciding to retry".into(),
                );
            }
            Err(error) => return Err(error.into()),
        },
        Command::ApprovalDecision(id, binding, approved) => {
            match client.decide_sdk_approval(&id, &binding, approved).await {
                Ok(value) => output(&value)?,
                Err(error) if uncertain_change(&error) => {
                    return Err(
                        "Approval outcome unknown; use approvals inspect before deciding to retry"
                            .into(),
                    );
                }
                Err(error) => return Err(error.into()),
            }
        }
        Command::ScheduleList(query) => output(&client.scheduled_tasks_page(&query).await?)?,
        Command::ScheduleExecutions(id, query) => {
            output(&client.scheduled_task_executions_page(&id, &query).await?)?
        }
        Command::ScheduleGet(id) => output(&client.scheduled_task(&id).await?)?,
        Command::ScheduleExecution(id, execution_id) => {
            output(&client.scheduled_task_execution(&id, &execution_id).await?)?
        }
        Command::NotificationList(query) => {
            output(&serde_json::to_value(
                client.scheduled_notifications(&query).await?,
            )?)?;
        }
        Command::NotificationSummary => {
            output(&serde_json::to_value(
                client.scheduled_notification_summary().await?,
            )?)?;
        }
        Command::NotificationGet(id) => {
            output(&json!({"notification": client.scheduled_notification(&id).await?}))?;
        }
        Command::NotificationMark(ids, read) => {
            // Each explicit action is submitted once, with an exact-target echo.
            let result = if read && ids.len() > 1 {
                client.mark_scheduled_notifications_read(&ids).await
            } else {
                client.mark_scheduled_notification(&ids[0], read).await
            };
            match result {
                Ok(value) => output(&serde_json::to_value(value)?)?,
                Err(error) if uncertain_change(&error) => {
                    return Err("Read-state change outcome unknown; use notifications get for the supplied IDs before retrying".into());
                }
                Err(error) => return Err(error.into()),
            }
        }
        Command::WatchTask(id, interval) => loop {
            let value = client.task(&id).await?;
            if value["task_id"].as_str() != Some(id.as_str()) {
                return Err(Error::Response.into());
            }
            let status = TaskStatus::parse(value["status"].as_str().ok_or(Error::Response)?)
                .map_err(|_| Error::Response)?;
            output(&value)?;
            if status.terminal() {
                return Ok(if status == TaskStatus::Completed {
                    0
                } else {
                    3
                });
            }
            tokio::time::sleep(interval).await;
        },
        Command::WatchNode(id, after, interval) => {
            let mut cursor = NodeEventCursor {
                after_sequence: after,
            };
            loop {
                let value = client
                    .local_node_events(&id, cursor.after_sequence, 500)
                    .await?;
                for event in cursor.accept(&id, &value)? {
                    output(&json!({"device_id":id, "event":event}))?;
                }
                tokio::time::sleep(interval).await;
            }
        }
    }
    Ok(0)
}
fn error_value(code: &str, message: &str) -> Value {
    json!({"schema_version":"agentforge.platformctl.error.v1","error":{"code":code,"message":message}})
}
fn report_error(code: &str, message: &str) {
    eprintln!("{}", error_value(code, message));
}
#[tokio::main]
async fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args == ["--credential-stdio"] {
        return match device_auth_cli::credential_stdio().await {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(_) => {
                eprintln!("Credential operation failed");
                std::process::ExitCode::FAILURE
            }
        };
    }
    let args = match device_auth_cli::expand_profile(args) {
        Ok(args) => args,
        Err(_) => {
            report_error("invalid_configuration", "Invalid client configuration");
            return std::process::ExitCode::from(2);
        }
    };
    let args = if args.len() >= 4 && args[0] == "--origin" && args[2] == "auth" {
        let mut auth = vec![
            "auth".to_owned(),
            args[3].clone(),
            args[0].clone(),
            args[1].clone(),
        ];
        auth.extend_from_slice(&args[4..]);
        auth
    } else {
        args
    };
    if args.first().map(String::as_str) == Some("auth") {
        return tokio::select! {
            result=device_auth_cli::auth(&args[1..])=>match result {Ok(code)=>std::process::ExitCode::from(code),Err(error)=>{report_error("authentication_failed", &error.to_string());std::process::ExitCode::FAILURE}},
            _=tokio::signal::ctrl_c()=>std::process::ExitCode::from(130),
        };
    }
    if args.is_empty() || args == ["--help"] || args == ["-h"] {
        print!("{HELP}");
        return std::process::ExitCode::SUCCESS;
    }
    let prepared = async {
        let mut args = Arguments::parse(args)?;
        let command = command(&mut args)?;
        let token = if args.token_stdin {
            token_from_stdin()?
        } else {
            ai_platform_client::device_auth::DeviceLogin::new(&args.origin, "agentforge-cli")?
                .access_token()
                .await?
                .token
        };
        let client = Client::new(&args.origin)?.authenticated(token);
        Ok::<_, Box<dyn std::error::Error>>((client, command))
    }
    .await;
    let (client, command) = match prepared {
        Ok(value) => value,
        Err(error) => {
            report_error("invalid_request", &error.to_string());
            return std::process::ExitCode::from(2);
        }
    };
    tokio::select! {
        result = run(client, command) => match result {
            Ok(code) => std::process::ExitCode::from(code),
            Err(error) => { report_error("request_failed", &error.to_string()); std::process::ExitCode::FAILURE }
        },
        signal = tokio::signal::ctrl_c() => {
            if signal.is_err() { report_error("signal_failed", "Could not observe Ctrl-C"); }
            report_error("observation_stopped", "Observation stopped. Any submitted change may still complete on the server.");
            std::process::ExitCode::from(130)
        }
    }
}

#[cfg(test)]
mod approval_cli_tests {
    use super::*;
    #[test]
    fn error_output_has_stable_json_schema() {
        assert_eq!(
            error_value("invalid_request", "Invalid option"),
            json!({"schema_version":"agentforge.platformctl.error.v1","error":{"code":"invalid_request","message":"Invalid option"}})
        );
    }
    fn parse(words: &str) -> Result<Command, &'static str> {
        let mut args = Arguments::parse(
            format!("--origin https://gateway.example --token-stdin {words}")
                .split_whitespace()
                .map(str::to_owned)
                .collect(),
        )?;
        command(&mut args)
    }
    const ID: &str = "00000000-0000-4000-8000-000000000001";
    #[test]
    fn explicit_acknowledgment_and_binding_required() {
        assert!(parse(&format!("runs get {ID}")).is_ok());
        assert!(parse(&format!("runs get {ID} --yes")).is_err());
        assert!(parse(&format!("runs cancel {ID}")).is_err());
        assert!(parse(&format!("runs cancel {ID} --yes")).is_ok());
        assert!(parse(&format!("approvals approve {ID} --yes")).is_err());
        for action in ["approve", "reject"] {
            let input = format!(
                "approvals {action} {ID} --run-id {ID} --tool-call-id call_1 --arguments-hash {}",
                "a".repeat(64)
            );
            assert!(parse(&input).is_err());
            assert!(parse(&format!("{input} --yes")).is_ok());
        }
        assert!(parse(&format!("approvals inspect {ID} --run-id {ID}")).is_err());
    }
}

#[cfg(test)]
mod schedule_history_tests {
    use super::*;
    const ID: &str = "00000000-0000-0000-0000-000000000001";
    fn parse(words: &[&str]) -> Result<Command, &'static str> {
        let mut args = vec![
            "--origin".into(),
            "https://gateway.example".into(),
            "--token-stdin".into(),
        ];
        args.extend(words.iter().map(|s| s.to_string()));
        command(&mut Arguments::parse(args)?)
    }
    #[test]
    fn schedules_default_latest_and_exact_reads() {
        assert!(matches!(
            parse(&["schedules", "list"]).unwrap(),
            Command::ScheduleList(ScheduleQuery {
                limit: 20,
                cursor: None
            })
        ));
        assert!(matches!(
            parse(&["schedules", "executions", ID]).unwrap(),
            Command::ScheduleExecutions(
                _,
                ScheduleQuery {
                    limit: 20,
                    cursor: None
                }
            )
        ));
        assert!(matches!(
            parse(&["schedules", "get", ID]).unwrap(),
            Command::ScheduleGet(_)
        ));
        assert!(matches!(
            parse(&["schedules", "execution", ID, ID]).unwrap(),
            Command::ScheduleExecution(_, _)
        ));
    }
    #[test]
    fn schedules_cursor_and_limit_boundaries() {
        for words in [
            vec!["schedules", "list", "--limit", "200"],
            vec!["schedules", "executions", ID, "--limit", "100"],
            vec![
                "schedules",
                "list",
                "--before-at",
                "2026-10-13T00:00:00Z",
                "--before-id",
                ID,
            ],
        ] {
            assert!(parse(&words).is_ok());
        }
        for words in [
            vec!["schedules", "list", "--limit", "201"],
            vec!["schedules", "list", "--limit", "0"],
            vec!["schedules", "executions", ID, "--limit", "101"],
            vec!["schedules", "list", "--before-id", ID],
            vec!["schedules", "list", "--before-at", "2026-10-13T00:00:00Z"],
            vec!["schedules", "list", "--before-at", "bad", "--before-id", ID],
            vec!["schedules", "list", "--offset", "20"],
            vec!["schedules", "list", "--limit", "1", "--limit", "2"],
            vec!["schedules", "get", "bad"],
            vec!["schedules", "execution", ID, "bad"],
            vec!["schedules", "get", ID, "--limit", "1"],
            vec!["schedules", "list", "--yes"],
        ] {
            assert!(parse(&words).is_err(), "{words:?}");
        }
    }
    #[test]
    fn schedule_actions_still_require_explicit_approval() {
        assert!(parse(&["schedules", "pause", ID]).is_err());
        assert!(matches!(
            parse(&["schedules", "pause", ID, "--yes"]).unwrap(),
            Command::Mutation(_)
        ));
        assert!(parse(&["schedules", "pause", ID, "--yes", "--before-id", ID]).is_err());
    }
}

#[cfg(test)]
mod problem_effect_tests {
    use super::*;
    fn problem(status: u16) -> Error {
        let bytes=serde_json::to_vec(&json!({"type":"about:blank","title":"private-marker","status":status,"detail":{"password":"private-marker"},"instance":"https://private-marker.invalid"})).unwrap();
        Error::Problem(Box::new(
            ai_platform_client::problem::RemoteProblem::decode(status, &bytes).unwrap(),
        ))
    }
    #[test]
    fn problem_errors_keep_cli_reconciliation_classification_and_privacy() {
        for status in [401, 403, 404, 409, 422, 429, 408, 500, 502, 503, 504, 599] {
            let error = problem(status);
            assert_eq!(uncertain_change(&error), matches!(status, 408 | 500..=599));
            assert_eq!(
                uncertain_change(&error),
                uncertain_change(&Error::Status(status))
            );
            assert!(
                !error_value("request_failed", &error.to_string())
                    .to_string()
                    .contains("private-marker")
            );
            assert!(!format!("{error:?}").contains("private-marker"));
        }
        assert!(uncertain_change(&Error::Transport));
        assert!(uncertain_change(&Error::Response));
        // Redirect uncertainty remains specific to cancellation, as before.
        assert!(!uncertain_change(&Error::Status(302)));
        assert!(matches!(Error::Status(302).status(), Some(300..=399)));
        assert!(
            Error::UnknownOutcome
                .to_string()
                .contains("do not automatically resubmit")
        );
    }
}
