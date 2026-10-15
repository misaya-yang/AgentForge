#![allow(clippy::expect_used)]
use codex_core::{
    RecoverTurnRequest, StartIfIdleSubmission, StartThreadOptions, SuspendTurnOutcome,
    TurnInputRequest,
};
use codex_history::RolloutItem;
use codex_protocol::protocol::{EventMsg, SessionSource, SubAgentSource};
use codex_protocol::user_input::UserInput;
use core_test_support::responses;
use core_test_support::responses::{
    ev_completed, ev_response_created, mount_response_once, mount_sse_once, sse, sse_response,
    start_mock_server,
};
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use std::sync::Arc;
use std::time::Duration;

#[tokio::test]
async fn recover_turn_preserves_model_lease_metadata_and_replacement_owner() {
    let server = responses::start_mock_server().await;
    let response_mock =
        responses::mount_sse_once(&server, responses::sse_completed("recovered")).await;
    let test = test_codex()
        .build_with_auto_env(&server)
        .await
        .expect("recovery session");
    let turn_id = "original-durable-turn";
    let metadata = std::collections::HashMap::from([
        (
            "ai_platform_execution_owner".to_string(),
            "replacement-owner".to_string(),
        ),
        ("ai_platform_execution_fence".to_string(), "2".to_string()),
        (
            "ai_platform_lease_id".to_string(),
            "original-lease".to_string(),
        ),
        (
            "ai_platform_lease_signature".to_string(),
            "fixture-signature".to_string(),
        ),
        ("ai_platform_scope_sha256".to_string(), "a".repeat(64)),
    ]);
    assert_eq!(
        test.codex
            .recover_turn_with_metadata_if_idle(
                RecoverTurnRequest {
                    turn_id: turn_id.to_string(),
                    thread_settings: Default::default(),
                    trace: None,
                    cyber_access_program: None,
                },
                Some(metadata.clone())
            )
            .await
            .expect("recover"),
        StartIfIdleSubmission::Started {
            turn_id: turn_id.to_string(),
            root_turn_id: turn_id.to_string(),
        }
    );
    let started = wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnStarted(_))
    })
    .await;
    let EventMsg::TurnStarted(started) = started else {
        unreachable!()
    };
    assert_eq!(started.turn_id, turn_id);
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    let request = response_mock.single_request();
    let body = request.body_json();
    let canonical: serde_json::Value = serde_json::from_str(
        body["client_metadata"]["x-codex-turn-metadata"]
            .as_str()
            .expect("canonical turn metadata"),
    )
    .expect("valid canonical turn metadata");
    for (key, value) in metadata {
        assert_eq!(canonical[&key].as_str(), Some(value.as_str()));
    }
    assert_eq!(canonical["turn_id"].as_str(), Some(turn_id));
    assert_eq!(body["client_metadata"]["turn_id"].as_str(), Some(turn_id));
    let groups = request.message_input_text_groups("user");
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].len(), 1);
    assert!(groups[0][0].starts_with("<environment_context>"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn root_turn_suspension_preserves_unfinished_turn_history() {
    let server = start_mock_server().await;
    // Waiting on the mocked response keeps the turn active on local and remote executors
    // without requiring an OS-specific command or a working sandboxed child process.
    mount_response_once(
        &server,
        sse_response(sse(vec![
            ev_response_created("suspended_response"),
            ev_completed("suspended_response"),
        ]))
        .set_delay(Duration::from_secs(60)),
    )
    .await;
    let test = test_codex()
        .with_model("gpt-5.4")
        .build_with_auto_env(&server)
        .await
        .expect("start persistent root thread");
    let codex = Arc::clone(&test.codex);
    let descendant = test
        .thread_manager
        .start_thread(StartThreadOptions {
            session_source: Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id: test.session_configured.thread_id,
                depth: 1,
                agent_path: None,
                agent_nickname: None,
                agent_role: None,
            })),
            ..StartThreadOptions::new(test.config.clone())
        })
        .await
        .expect("start a currently loaded descendant");
    let submitted = codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "preserve this exact unfinished turn".into(),
            text_elements: Vec::new(),
        }]))
        .await
        .expect("start root turn");
    let codex_core::TurnInputSubmission::Started {
        turn_id,
        root_turn_id,
    } = submitted else {
        panic!("expected a started root turn");
    };
    assert_eq!(root_turn_id, turn_id);
    wait_for_event(&codex, |event| matches!(event, EventMsg::TurnStarted(_))).await;

    assert_eq!(
        codex
            .suspend_turn_and_shutdown()
            .await
            .expect("reject handoff while a descendant remains loaded"),
        SuspendTurnOutcome::HasLiveDescendants,
    );
    descendant
        .thread
        .shutdown_and_wait()
        .await
        .expect("stop the descendant before root handoff");
    test.thread_manager
        .remove_thread(&descendant.thread_id)
        .await
        .expect("remove the stopped descendant from the live thread inventory");

    // A previously admitted descendant no longer blocks handoff once it is stopped
    // and removed; suspension only consults the current live subtree.
    assert_eq!(
        codex
            .suspend_turn_and_shutdown()
            .await
            .expect("stop and close the old writer"),
        SuspendTurnOutcome::Suspended {
            turn_id: turn_id.clone(),
        },
    );
    let rollout_path = codex.rollout_path().expect("rollout path");
    let rollout = tokio::fs::read_to_string(&rollout_path)
        .await
        .expect("read durable rollout");
    let items = rollout
        .lines()
        .map(|line| {
            codex_rollout::parse_rollout_line(line)
                .expect("parse durable rollout")
                .item
        })
        .collect::<Vec<_>>();
    assert!(items.iter().all(|item| !matches!(
        item,
        RolloutItem::EventMsg(EventMsg::TurnAborted(_) | EventMsg::TurnComplete(_))
    )));
    test.thread_manager
        .remove_thread(&test.session_configured.thread_id)
        .await
        .expect("unload the suspended root");
    let recovery_server = start_mock_server().await;
    mount_sse_once(
        &recovery_server,
        sse(vec![
            ev_response_created("recovered_response"),
            ev_completed("recovered_response"),
        ]),
    )
    .await;
    let resumed = test_codex()
        .with_model("gpt-5.4")
        .resume(&recovery_server, Arc::clone(&test.home), rollout_path)
        .await
        .expect("resume the suspended root on a replacement runtime");

    assert_eq!(
        resumed
            .codex
            .recover_turn_if_idle(codex_core::RecoverTurnRequest {
                turn_id: turn_id.clone(),
                thread_settings: Default::default(),
                trace: None,
                cyber_access_program: None,
            })
            .await
            .expect("recover the unfinished turn"),
        codex_core::StartIfIdleSubmission::Started {
            turn_id: turn_id.clone(),
            root_turn_id: turn_id.clone(),
        },
    );
    let completed = wait_for_event(&resumed.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    let EventMsg::TurnComplete(completed) = completed else {
        unreachable!("wait_for_event returned unexpected event");
    };
    assert_eq!(completed.turn_id, turn_id);
}
