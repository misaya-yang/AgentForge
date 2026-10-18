use super::*;
use crate::application_guard::{self, ApplicationPrincipal};
use sqlx::PgConnection;
pub(super) async fn present(conn: &mut PgConnection) -> Result<bool> {
    Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname='public' AND c.relname='platform_fresh_schema_state')").fetch_one(conn).await?)
}
/// The static contract contains no guessed catalog hash. Each real initializer
/// measures the final database; the reader checks its complete live catalog.
pub fn validate_snapshot(snapshot: &Value) -> Result<()> {
    let mut expected = admission_contract();
    let fingerprints = snapshot["fingerprints"]
        .as_object()
        .ok_or_else(|| refused("actual final fingerprint classes absent"))?;
    if fingerprints.len() != 4
        || ["structural", "acl", "extensions", "reference_data"]
            .iter()
            .any(|key| {
                fingerprints
                    .get(*key)
                    .and_then(Value::as_str)
                    .is_none_or(|h| {
                        h.len() != 64
                            || !h
                                .bytes()
                                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                    })
            })
    {
        return Err(refused("actual final fingerprints malformed"));
    }
    expected["fingerprints"] = json!(fingerprints);
    if snapshot != &expected {
        return Err(refused(
            "fresh source, target, mode or real final catalog mismatch",
        ));
    }
    Ok(())
}
pub async fn admit_if_fresh(
    conn: &mut PgConnection,
    principal: ApplicationPrincipal,
) -> Result<bool> {
    if !present(conn).await? {
        return Ok(false);
    }
    let schema = if matches!(
        principal,
        ApplicationPrincipal::Runtime | ApplicationPrincipal::CapabilityWorker
    ) {
        "assistant"
    } else {
        "gateway"
    };
    let identity_sql = application_guard::PRINCIPAL_IDENTITY_SQL.replace(
        "platform_schema_admission_snapshot_v1",
        "platform_fresh_schema_snapshot_v1",
    );
    let identity: Value = sqlx::query_scalar(&identity_sql)
        .bind(schema)
        .fetch_one(&mut *conn)
        .await
        .map_err(|_| refused("fresh canonical reader identity unavailable"))?;
    application_guard::validate_principal_identity(&identity, principal)?;
    let snapshot: Value = sqlx::query_scalar(&format!(
        "SELECT {schema}.platform_fresh_schema_snapshot_v1()"
    ))
    .fetch_one(&mut *conn)
    .await
    .map_err(|_| refused("fresh reader denied or unavailable"))?;
    validate_snapshot(&snapshot)?;
    for query in crate::source_tools::service_successors::admission_checks(
        EPOCH,
        application_guard::principal_role_suffix(principal),
    )? {
        if !sqlx::query_scalar::<_, bool>(&query)
            .fetch_one(&mut *conn)
            .await
            .map_err(|_| refused("fresh exact service privilege probe unavailable"))?
        {
            return Err(refused("fresh exact service privilege contract refused"));
        }
    }
    Ok(true)
}
