//! Opt-in integration coverage for the production `/v1/responses` route.
//!
//! Run only with `-- --ignored` and the explicit loopback test settings
//! documented by the support fixture. Ordinary test runs never connect to PG or
//! Redis. The Runtime HTTP fixture covers clean EOF, SSE open 500 and a real
//! truncated HTTP chunk after a projected text delta.

#[path = "responses_route_idempotency_support/mod.rs"]
mod support;

use ai_platform_gateway::migration_compat::responses::ResponsesIdempotency;
use axum::http::StatusCode;
use futures_util::FutureExt;
use serde_json::Value;
use std::{panic::AssertUnwindSafe, sync::atomic::Ordering, time::Duration};
use support::{ProjectFixture, Result, RuntimeFailureMode, RuntimeStub, post_response};
use tokio::time::{sleep, timeout};
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires explicit disposable loopback PostgreSQL and Redis; no live Runtime is used"]
async fn production_responses_route_creates_fresh_session_and_caches_post_accept_eof() -> Result<()>
{
    let fixture = ProjectFixture::new().await?;
    let outcome = AssertUnwindSafe(async {
        exercise_backend(&fixture, false, RuntimeFailureMode::CleanEof).await?;
        exercise_backend(&fixture, true, RuntimeFailureMode::CleanEof).await?;
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })
    .catch_unwind()
    .await;
    let cleanup = fixture.close().await;
    cleanup?;
    match outcome {
        Ok(result) => result,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

#[tokio::test]
#[ignore = "requires explicit disposable loopback PostgreSQL and Redis; real HTTP error framing"]
async fn production_responses_route_caches_post_accept_open_and_read_errors() -> Result<()> {
    let fixture = ProjectFixture::new().await?;
    let outcome = AssertUnwindSafe(async {
        for mode in [RuntimeFailureMode::Open500, RuntimeFailureMode::ReadError] {
            for redis in [false, true] {
                exercise_backend(&fixture, redis, mode).await?;
            }
        }
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })
    .catch_unwind()
    .await;
    fixture.close().await?;
    match outcome {
        Ok(result) => result,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

async fn exercise_backend(
    fixture: &ProjectFixture,
    use_redis: bool,
    mode: RuntimeFailureMode,
) -> Result<()> {
    let backend = if use_redis { "redis" } else { "memory" };
    let key = format!("responses-route-{backend}-{}", Uuid::new_v4().simple());
    let session_id = Uuid::new_v4().to_string();
    fixture.track_idempotency_key(
        &fixture.actor_a.actor.tenant_id,
        &fixture.actor_a.actor.user_id,
        &key,
    );

    let mut runtime = if mode == RuntimeFailureMode::CleanEof {
        RuntimeStub::start(fixture.runtime_pool.clone()).await?
    } else {
        RuntimeStub::start_with_mode(fixture.runtime_pool.clone(), mode).await?
    };
    let idempotency = if use_redis {
        ResponsesIdempotency::new(Some(fixture.redis.clone()))
    } else {
        ResponsesIdempotency::new(None)
    };
    let harness = fixture.router(runtime.url.clone(), idempotency)?;
    let body = fixture.request_body("idempotency route probe", &session_id);

    let first_router = harness.router.clone();
    let first_token = fixture.actor_a.token.clone();
    let first_key = key.clone();
    let first_body = body.clone();
    let mut first = tokio::spawn(async move {
        post_response(
            &first_router,
            Some(&first_token),
            Some(&first_key),
            first_body,
        )
        .await
    });

    tokio::select! {
        biased;
        seen = timeout(Duration::from_secs(10), async {
            if mode==RuntimeFailureMode::Open500 {runtime.stream_opened.notified().await;} else {harness.event_seen.notified().await;}
            runtime.turn_started.notified().await;
        }) => {
            seen.map_err(|_| "Runtime partial event was not projected by the Gateway")?;
        }
        response = &mut first => {
            let response = response??;
            return Err(format!(
                "Gateway returned {} before projecting the Runtime event: {}",
                response.status, response.body,
            ).into());
        }
    }
    assert_eq!(harness.lookup_count.load(Ordering::SeqCst), 1);

    let waiter_router = harness.router.clone();
    let waiter_token = fixture.actor_a.token.clone();
    let waiter_key = key.clone();
    let waiter_body = body.clone();
    let waiter = tokio::spawn(async move {
        post_response(
            &waiter_router,
            Some(&waiter_token),
            Some(&waiter_key),
            waiter_body,
        )
        .await
    });
    wait_for_lookup_count(&harness, 2).await?;
    sleep(Duration::from_millis(100)).await;
    assert!(
        !waiter.is_finished(),
        "same-key waiter should remain pending until the accepted response is stored"
    );

    runtime.release_event_stream();
    let first = timeout(Duration::from_secs(10), first)
        .await
        .map_err(|_| "first Responses request timed out")?
        .map_err(|_| "first Responses task failed")??;
    let waited = timeout(Duration::from_secs(10), waiter)
        .await
        .map_err(|_| "same-key Responses waiter timed out")?
        .map_err(|_| "same-key Responses waiter task failed")??;

    let expected_status = if mode == RuntimeFailureMode::CleanEof {
        StatusCode::BAD_GATEWAY
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    let expected_code = if mode == RuntimeFailureMode::CleanEof {
        "agent_runtime_stream_incomplete"
    } else {
        "agent_runtime_event_stream_failed"
    };
    assert_eq!(
        first.status, expected_status,
        "{backend} {mode:?}: unexpected Runtime failure projection {} ({})",
        first.status, first.body["error"]["code"]
    );
    assert_eq!(first.body["error"]["code"], expected_code);
    assert!(first.headers.get("x-idempotency-replayed").is_none());
    assert_eq!(waited.status, expected_status, "{backend} {mode:?}");
    assert_eq!(waited.body, first.body);
    assert_eq!(
        waited
            .headers
            .get("x-idempotency-replayed")
            .and_then(|v| v.to_str().ok()),
        Some("true")
    );

    let replay = post_response(
        &harness.router,
        Some(&fixture.actor_a.token),
        Some(&key),
        body.clone(),
    )
    .await?;
    assert_eq!(replay.status, expected_status, "{backend} {mode:?}");
    assert_eq!(replay.body, first.body);
    assert_eq!(
        replay
            .headers
            .get("x-idempotency-replayed")
            .and_then(|v| v.to_str().ok()),
        Some("true")
    );

    // Preserve the original EOF contract independently of the new error modes.
    if mode == RuntimeFailureMode::CleanEof {
        assert_eq!(first.status, StatusCode::BAD_GATEWAY);
        assert_eq!(waited.status, StatusCode::BAD_GATEWAY);
        assert_eq!(replay.status, StatusCode::BAD_GATEWAY);
        assert_eq!(
            first.body["error"]["code"],
            "agent_runtime_stream_incomplete"
        );
    }
    let accepted_run = runtime.accepted_run_id()?;
    for response in [&first, &waited, &replay] {
        assert_eq!(
            response
                .headers
                .get("x-run-id")
                .and_then(|v| v.to_str().ok()),
            Some(accepted_run.as_str()),
            "{backend} {mode:?}"
        );
        assert_eq!(
            response
                .headers
                .get("x-session-id")
                .and_then(|v| v.to_str().ok()),
            Some(session_id.as_str()),
            "{backend} {mode:?}"
        );
        assert_eq!(
            response
                .headers
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("application/json")
        );
        assert_eq!(
            response.raw_body, first.raw_body,
            "cached bytes must match exactly"
        );
        let mut headers = response.headers.clone();
        headers.remove("x-idempotency-replayed");
        assert_eq!(
            headers, first.headers,
            "cached headers must retain accepted identity"
        );
    }

    let mut conflicting_body = body.clone();
    conflicting_body["input"] = Value::String("different request body".into());
    let conflict = post_response(
        &harness.router,
        Some(&fixture.actor_a.token),
        Some(&key),
        conflicting_body,
    )
    .await?;
    assert_eq!(conflict.status, StatusCode::CONFLICT, "{backend}");
    assert_eq!(conflict.body["error"]["code"], "idempotency_key_conflict");

    let unauthenticated = post_response(&harness.router, None, None, body.clone()).await?;
    assert_eq!(
        unauthenticated.status,
        StatusCode::UNAUTHORIZED,
        "{backend}"
    );
    let invalid_token = post_response(
        &harness.router,
        Some("not-a-valid-session-token"),
        None,
        body.clone(),
    )
    .await?;
    assert_eq!(invalid_token.status, StatusCode::UNAUTHORIZED, "{backend}");

    // A valid identity in tenant B cannot discover tenant A's model or replay
    // tenant A's cached accepted response, even with the same idempotency key.
    let other_tenant = post_response(
        &harness.router,
        Some(&fixture.actor_b.token),
        Some(&key),
        body,
    )
    .await?;
    assert_eq!(other_tenant.status, StatusCode::BAD_REQUEST, "{backend}");
    assert_eq!(other_tenant.body["error"]["code"], "model_not_found");
    assert!(other_tenant.headers.get("x-idempotency-replayed").is_none());

    assert_eq!(runtime.turns.load(Ordering::SeqCst), 1, "{backend}");
    let persisted_runs: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM assistant.assistant_runs WHERE tenant_id=$1 AND user_id=$2 AND session_id=$3",
    )
    .bind(&fixture.actor_a.actor.tenant_id)
    .bind(&fixture.actor_a.actor.user_id)
    .bind(&session_id)
    .fetch_one(&fixture.pool)
    .await?;
    assert_eq!(persisted_runs, 1, "{backend}");

    let persisted_sessions: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM assistant.sessions WHERE tenant_id=$1 AND user_id=$2 AND session_id=$3",
    )
    .bind(&fixture.actor_a.actor.tenant_id)
    .bind(&fixture.actor_a.actor.user_id)
    .bind(&session_id)
    .fetch_one(&fixture.pool)
    .await?;
    assert_eq!(
        persisted_sessions, 1,
        "fresh session should be created once"
    );

    let persisted_threads: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM assistant_runtime_threads WHERE tenant_id=$1 AND user_id=$2 AND session_id=$3",
    )
    .bind(&fixture.actor_a.actor.tenant_id)
    .bind(&fixture.actor_a.actor.user_id)
    .bind(&session_id)
    .fetch_one(&fixture.pool)
    .await?;
    assert_eq!(
        persisted_threads, 1,
        "fresh Runtime thread should be created once"
    );

    let stored_run:String=sqlx::query_scalar("SELECT run_id::text FROM assistant.assistant_runs WHERE tenant_id=$1 AND user_id=$2 AND session_id=$3")
        .bind(&fixture.actor_a.actor.tenant_id).bind(&fixture.actor_a.actor.user_id).bind(&session_id).fetch_one(&fixture.pool).await?;
    assert_eq!(
        stored_run, accepted_run,
        "response must identify the durable scoped run"
    );
    let paths = runtime.request_paths();
    assert!(
        (3..=4).contains(&paths.len()),
        "unexpected Runtime requests: {paths:?}"
    );
    assert_eq!(paths.iter().filter(|p| p.ends_with("/turns")).count(), 1);
    assert_eq!(paths.iter().filter(|p| p.ends_with("/events")).count(), 1);
    assert_eq!(
        paths
            .iter()
            .filter(|p| p.as_str() == "POST /internal/v1/threads")
            .count(),
        1
    );
    assert_eq!(
        paths.iter().filter(|p| p.ends_with("/resume")).count(),
        paths.len() - 3
    );

    runtime.finish().await?;
    Ok(())
}

async fn wait_for_lookup_count(harness: &support::RouteHarness, expected: usize) -> Result<()> {
    timeout(Duration::from_secs(5), async {
        while harness.lookup_count.load(Ordering::SeqCst) < expected {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .map_err(|_| "second authenticated model lookup was not observed")?;
    Ok(())
}
