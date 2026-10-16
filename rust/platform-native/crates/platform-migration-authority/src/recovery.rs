//! Non-transactional checkpoint/lease recovery with the existing immutable ledger.
use crate::{
    AuthorityError, Result, ledger,
    manifest::ChangeSpec,
    runner::{MigrationAuthority, evaluate_conditions, runner_digest},
    sql::reject_embedded_transactions,
};
use sha2::{Digest, Sha256};
use sqlx::{Connection, PgConnection, Row};
use std::{collections::BTreeSet, future::Future, path::Path, pin::Pin, time::Instant};
#[path = "recovery_sql.rs"]
mod queries;
#[derive(Clone)]
pub struct RecoveryContext {
    pub baseline_id: String,
    pub spec: ChangeSpec,
    pub attempt_id: String,
    pub checkpoint: String,
    pub fence_generation: i32,
    pub prior_state: String,
}
pub type RecoveryFuture<'a> = Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;
pub trait RecoveryHandler: Send + Sync {
    fn run<'a>(
        &'a self,
        conn: &'a mut PgConnection,
        context: RecoveryContext,
    ) -> RecoveryFuture<'a>;
}
pub fn split_checkpoints(sql: &str) -> Result<Vec<(String, String)>> {
    let pattern =
        regex::Regex::new(r"(?im)^\s*--\s*@checkpoint\s+(?P<name>[a-z0-9_]+)\s*$").unwrap();
    let mut segments = Vec::new();
    let mut name = "__preamble__".to_string();
    let mut end = 0;
    let mut seen = BTreeSet::new();
    for capture in pattern.captures_iter(sql) {
        let full = capture.get(0).unwrap();
        let part = &sql[end..full.start()];
        if !part.trim().is_empty() {
            segments.push((name, part.to_string()));
        }
        name = capture["name"].to_string();
        if name == "__preamble__" || !seen.insert(name.clone()) {
            return Err(AuthorityError::Refused(
                "non-transactional SQL has duplicate/reserved checkpoint".into(),
            ));
        }
        end = full.end();
    }
    if !sql[end..].trim().is_empty() {
        segments.push((name, sql[end..].to_string()));
    } else if !seen.is_empty() {
        return Err(AuthorityError::Refused(
            "non-transactional checkpoint has no SQL segment".into(),
        ));
    }
    if segments.is_empty() {
        return Err(AuthorityError::Refused(
            "non-transactional change contains no executable SQL".into(),
        ));
    }
    Ok(segments)
}
fn fence(value: Option<i32>) -> Result<i32> {
    value.ok_or_else(|| AuthorityError::Blocked("migration attempt lost its lease/fence".into()))
}
async fn terminal(
    conn: &mut PgConnection,
    id: &str,
    owner: &str,
    generation: i32,
    state: &str,
    error: Option<&str>,
) -> Result<i32> {
    fence(
        sqlx::query_scalar(queries::_UPDATE_ATTEMPT_TERMINAL_FENCED)
            .bind(id)
            .bind(owner)
            .bind(generation)
            .bind(state)
            .bind(error)
            .fetch_optional(conn)
            .await?,
    )
}
impl MigrationAuthority {
    pub async fn apply_change_non_transactional(
        &self,
        conn: &mut PgConnection,
        baseline_id: &str,
        spec: &ChangeSpec,
        directory: &Path,
    ) -> Result<i32> {
        let sql = std::fs::read_to_string(directory.join(&spec.file))?;
        if format!("{:x}", Sha256::digest(sql.as_bytes())) != spec.sha256 {
            return Err(AuthorityError::Refused(
                "non-transactional change checksum mismatch".into(),
            ));
        }
        reject_embedded_transactions(&sql, &spec.name)?;
        let declared = spec
            .resume_handler
            .iter()
            .chain(spec.repair_handler.iter())
            .filter(|s| !s.is_empty())
            .collect::<BTreeSet<_>>();
        if declared.is_empty() {
            return Err(AuthorityError::Blocked(
                "non-transactional change has no declared recovery handler".into(),
            ));
        }
        for name in declared {
            if !self.recovery_handlers.contains_key(name) {
                return Err(AuthorityError::Blocked(format!(
                    "non-transactional change has no registered recovery implementation for {name}"
                )));
            }
        }
        let segments = split_checkpoints(&sql)?;
        let owner = format!(
            "{}:native:{}",
            runner_digest(),
            uuid::Uuid::new_v4().simple()
        );
        let latest = sqlx::query(queries::_SELECT_LATEST_ATTEMPT)
            .bind(baseline_id)
            .bind(spec.sequence)
            .fetch_optional(&mut *conn)
            .await?;
        let (id, checkpoint, mut generation, recovery, prior_state) = if let Some(row) = latest {
            let id: String = row.try_get("attempt_id")?;
            if row.try_get::<String, _>("checksum_sha256")? != spec.sha256
                || row.try_get::<String, _>("runner_digest")? != runner_digest()
            {
                return Err(AuthorityError::Blocked(
                    "latest attempt checksum or runner digest differs".into(),
                ));
            }
            let checkpoint: String = row.try_get("checkpoint")?;
            if !checkpoint.is_empty() && !segments.iter().any(|(name, _)| name == &checkpoint) {
                return Err(AuthorityError::Blocked(
                    "migration attempt has unknown checkpoint".into(),
                ));
            }
            let mut state: String = row.try_get("state")?;
            let mut generation: i32 = row.try_get("fence_generation")?;
            if state == "running" {
                let started: chrono::DateTime<chrono::Utc> = row.try_get("started_at")?;
                if chrono::Utc::now() - started < chrono::Duration::minutes(15) {
                    return Err(AuthorityError::Blocked(
                        "migration attempt still has an active lease".into(),
                    ));
                }
                state = if spec.resume_handler.is_some() {
                    "resumable"
                } else {
                    "failed"
                }
                .into();
                let prior_owner: String = row.try_get("lease_owner")?;
                fence(
                    sqlx::query_scalar(queries::_TRANSITION_EXPIRED_ATTEMPT)
                        .bind(&id)
                        .bind(prior_owner)
                        .bind(generation)
                        .bind(&state)
                        .bind("lease_expired")
                        .fetch_optional(&mut *conn)
                        .await?,
                )?;
            }
            let kind = match state.as_str() {
                "resumable" => "resume",
                "failed" => "repair",
                _ => {
                    return Err(AuthorityError::Blocked(
                        "migration attempt has unsupported state".into(),
                    ));
                }
            };
            generation = fence(
                sqlx::query_scalar(queries::_CLAIM_ATTEMPT)
                    .bind(&id)
                    .bind(&owner)
                    .bind(generation)
                    .bind(kind)
                    .fetch_optional(&mut *conn)
                    .await?,
            )?;
            (id, checkpoint, generation, Some(kind), state)
        } else {
            let id = format!("{:03}-{}", spec.sequence, uuid::Uuid::new_v4().simple());
            sqlx::query(ledger::INSERT_ATTEMPT)
                .bind(&id)
                .bind(baseline_id)
                .bind(spec.sequence)
                .bind(&spec.sha256)
                .bind(runner_digest())
                .bind("apply")
                .bind("")
                .bind(&owner)
                .bind(0i32)
                .execute(&mut *conn)
                .await?;
            (id, String::new(), 0, None, "new".into())
        };
        let start_index = segments
            .iter()
            .position(|(name, _)| name == &checkpoint)
            .map_or(0, |i| i + 1);
        let started = Instant::now();
        sqlx::query(
            "SELECT set_config('statement_timeout',$1,false),set_config('lock_timeout',$2,false)",
        )
        .bind((spec.timeout_seconds * 1000).to_string())
        .bind((spec.lock_budget_seconds * 1000).to_string())
        .execute(&mut *conn)
        .await?;
        let outcome: Result<()> = async {
            if let Some(kind) = recovery {
                let name = if kind == "resume" {
                    spec.resume_handler.as_ref()
                } else {
                    spec.repair_handler.as_ref()
                }
                .ok_or_else(|| {
                    AuthorityError::Blocked(format!("change needs a declared {kind}_handler"))
                })?;
                let handler = self.recovery_handlers.get(name).ok_or_else(|| {
                    AuthorityError::Blocked("missing registered recovery handler".into())
                })?;
                handler
                    .run(
                        conn,
                        RecoveryContext {
                            baseline_id: baseline_id.into(),
                            spec: spec.clone(),
                            attempt_id: id.clone(),
                            checkpoint: checkpoint.clone(),
                            fence_generation: generation,
                            prior_state,
                        },
                    )
                    .await?;
            }
            evaluate_conditions(
                conn,
                &self.role_prefix,
                &spec.preconditions,
                "pre",
                &spec.name,
            )
            .await?;
            for (name, segment) in &segments[start_index..] {
                let previous: String = sqlx::query_scalar("SELECT current_setting('role')")
                    .fetch_one(&mut *conn)
                    .await?;
                sqlx::query("SELECT set_config('role',$1,false)")
                    .bind(format!("{}{}", self.role_prefix, spec.owner))
                    .execute(&mut *conn)
                    .await?;
                let execution = sqlx::raw_sql(segment).execute(&mut *conn).await;
                let restore = sqlx::query("SELECT set_config('role',$1,false)")
                    .bind(previous)
                    .execute(&mut *conn)
                    .await;
                restore?;
                execution?;
                generation = fence(
                    sqlx::query_scalar(queries::_UPDATE_ATTEMPT_CHECKPOINT_FENCED)
                        .bind(&id)
                        .bind(&owner)
                        .bind(generation)
                        .bind(name)
                        .fetch_optional(&mut *conn)
                        .await?,
                )?;
            }
            evaluate_conditions(
                conn,
                &self.role_prefix,
                &spec.postconditions,
                "post",
                &spec.name,
            )
            .await?;
            Ok(())
        }
        .await;
        let outcome = match outcome {
            Ok(()) => Ok(()),
            Err(error) => {
                let state = if spec.resume_handler.is_some() {
                    "resumable"
                } else {
                    "failed"
                };
                terminal(conn, &id, &owner, generation, state, Some("AuthorityError"))
                    .await
                    .map(|_| ())
                    .and(Err(error))
            }
        };
        sqlx::query("RESET statement_timeout")
            .execute(&mut *conn)
            .await?;
        sqlx::query("RESET lock_timeout")
            .execute(&mut *conn)
            .await?;
        outcome?;
        let duration = i32::try_from(started.elapsed().as_millis()).unwrap_or(i32::MAX);
        let mut tx = conn.begin().await?;
        sqlx::query(ledger::INSERT_CHANGE_SUCCESS)
            .bind(baseline_id)
            .bind(spec.sequence)
            .bind(&spec.name)
            .bind(&spec.sha256)
            .bind(duration)
            .bind(runner_digest())
            .execute(&mut *tx)
            .await?;
        terminal(&mut tx, &id, &owner, generation, "succeeded", None).await?;
        tx.commit().await?;
        Ok(duration)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn checkpoints_preserve_preamble_and_exact_order() {
        let segments = split_checkpoints(
            "SELECT 0;\n-- @checkpoint first\nSELECT 1;\n-- @checkpoint second\nSELECT 2;",
        )
        .unwrap();
        assert_eq!(
            segments
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            vec!["__preamble__", "first", "second"]
        );
        for sql in [
            "",
            "-- @checkpoint first",
            "-- @checkpoint a\nSELECT 1;\n-- @checkpoint a\nSELECT 2;",
            "-- @checkpoint __preamble__\nSELECT 1;",
        ] {
            assert!(split_checkpoints(sql).is_err(), "{sql}");
        }
    }
}
