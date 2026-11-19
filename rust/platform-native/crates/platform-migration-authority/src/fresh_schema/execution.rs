//! Bounded, redacted native SQL execution on the one owned transaction.
use super::*;
use sqlx::{Connection, PgConnection};
use std::{future::Future, time::Duration};

pub(super) const INIT_LIMIT: Duration = Duration::from_secs(120);
pub(super) const VERIFY_LIMIT: Duration = Duration::from_secs(45);
pub(super) const OPERATOR_LIMIT: Duration = Duration::from_secs(150);
pub(super) const SQL_LIMIT: Duration = Duration::from_secs(30);

/// Phase names come only from source literals. Never format SQL, DSNs, driver
/// Display text, credentials or caller-provided strings into diagnostics.
pub(super) async fn stage<T>(
    phase: &'static str,
    limit: Duration,
    operation: impl Future<Output = Result<T>>,
) -> Result<T> {
    eprintln!("fresh-schema phase={phase} state=started");
    match tokio::time::timeout(limit, operation).await {
        Ok(Ok(value)) => {
            eprintln!("fresh-schema phase={phase} state=complete");
            Ok(value)
        }
        Ok(Err(error)) => {
            let code = match &error {
                AuthorityError::Database(sqlx::Error::Database(db)) => db
                    .code()
                    .filter(|s| {
                        s.len() == 5
                            && s.bytes()
                                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
                    })
                    .map(|s| s.into_owned())
                    .unwrap_or_else(|| "unavailable".into()),
                _ => "unavailable".into(),
            };
            eprintln!("fresh-schema phase={phase} state=failed sqlstate={code}");
            Err(error)
        }
        Err(_) => {
            eprintln!("fresh-schema phase={phase} state=deadline_exceeded");
            Err(refused(&format!(
                "fixed phase {phase} exceeded its client deadline; the owned connection is discarded"
            )))
        }
    }
}

/// Exact original SQL slices, divided only at unquoted top-level semicolons.
/// Dollar-quoted routines, nested comments and E strings use the existing native
/// lexer. Concatenating the returned slices preserves every executable byte.
pub(super) fn statements(sql: &str) -> Result<Vec<&str>> {
    let masked = crate::sql::strip_sql_bodies(sql)?;
    let mut result = Vec::new();
    let mut start = 0;
    for (index, byte) in masked.bytes().enumerate() {
        if byte == b';' {
            if !masked[start..index].trim().is_empty() {
                result.push(&sql[start..=index]);
            }
            start = index + 1;
        }
    }
    if !masked[start..].trim().is_empty() {
        result.push(&sql[start..]);
    }
    Ok(result)
}

/// Do not send a 1737-command GRANT script as one simple-query result stream.
/// Each exact statement is fully drained through native RawSql, then an explicit
/// Sync barrier establishes readiness before the next phase. No commits occur.
pub(super) async fn execute(conn: &mut PgConnection, sql: &str) -> Result<()> {
    for statement in statements(sql)? {
        tokio::time::timeout(SQL_LIMIT, sqlx::raw_sql(statement).execute(&mut *conn))
            .await
            .map_err(|_| refused("one source SQL statement exceeded its client deadline"))??;
    }
    tokio::time::timeout(SQL_LIMIT, conn.ping())
        .await
        .map_err(|_| refused("source SQL protocol readiness barrier exceeded its deadline"))??;
    Ok(())
}

/// Closing a failed/cancelled protocol must not start another unbounded query.
/// SQLx close_hard itself performs TLS shutdown; bound it, then dropping its
/// future/connection closes the owned socket even if TLS readiness is stuck.
pub(super) async fn discard(conn: PgConnection) {
    let _ = tokio::time::timeout(Duration::from_secs(2), conn.close_hard()).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn slices_preserve_complete_grants_and_never_split_routines_or_literals() {
        let source = r#"-- ;
SET LOCAL x='a;b'; DO $body$ BEGIN PERFORM ';'; END $body$;
/* ; /* ; */ */ SELECT E'a\';b'; SELECT "a;b"; -- tail
"#;
        let split = statements(source).unwrap();
        assert_eq!(split.len(), 4);
        assert!(split[1].contains("END $body$;"));
        let defs = source::compiled_definitions().unwrap();
        let grants = &defs.baseline_sql[2];
        let split = statements(grants).unwrap();
        assert_eq!(split.len(), 1737);
        assert_eq!(split.concat().trim(), grants.trim());
        for (_, sql) in &defs.changes {
            let split = statements(sql).unwrap();
            assert!(!split.is_empty());
            for part in split {
                crate::sql::strip_sql_bodies(part).unwrap();
            }
        }
    }
    #[tokio::test]
    async fn pending_work_is_cancelled_and_error_values_never_enter_phase_diagnostics() {
        let result: Result<()> = stage(
            "test.pending",
            Duration::from_millis(1),
            std::future::pending(),
        )
        .await;
        let error = result.unwrap_err().to_string();
        assert!(error.contains("test.pending"));
        assert!(error.contains("client deadline"));
    }
}
