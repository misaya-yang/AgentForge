//! Current connection adapter around the immutable retired Python replacement.
//! Keep its historical source identity and catalog SQL intact. Only routine row
//! order is projected back to the C profile used by the frozen fingerprints.
pub use crate::frozen_fingerprint::{
    canonical_digest, canonical_value, extensions_lines, like_prefix_pattern, python_json,
    reference_data_lines,
};
use crate::{AuthorityError, Result, manifest::ReferenceDataSet};
use serde_json::Value;
use sqlx::{Connection, PgConnection};
use std::collections::BTreeMap;

#[path = "fingerprint/ordering.rs"]
mod ordering;

fn refused(message: &str) -> AuthorityError {
    AuthorityError::Refused(message.into())
}

async fn order_routines(
    conn: &mut PgConnection,
    lines: &mut [String],
    key: &str,
    marker: &str,
) -> Result<()> {
    let queries: Value = serde_json::from_str(include_str!("fingerprint/catalog_queries.json"))
        .map_err(|_| refused("invalid bundled catalog queries"))?;
    let query = queries[key]
        .as_str()
        .ok_or_else(|| refused("missing routine ordering query"))?;
    let mut rows: Vec<Value> = sqlx::query_scalar(&format!(
        "SELECT to_jsonb(fingerprint_row) FROM ({query}) AS fingerprint_row"
    ))
    .bind(vec!["gateway", "assistant", "knowledge", "public"])
    .fetch_all(&mut *conn)
    .await?;
    let positions: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| line.starts_with(marker).then_some(index))
        .collect();
    if rows.len() != positions.len() {
        return Err(refused(
            "routine catalog changed during fingerprint collection",
        ));
    }
    for (index, row) in rows.iter_mut().enumerate() {
        row["native_ordering_position"] = index.into();
    }
    ordering::canonicalize_routines(key, &mut rows)?;
    let original: Vec<String> = positions
        .iter()
        .map(|&index| lines[index].clone())
        .collect();
    for (&position, row) in positions.iter().zip(rows) {
        let index = row["native_ordering_position"]
            .as_u64()
            .ok_or_else(|| refused("invalid routine ordering position"))?
            as usize;
        lines[position] = original[index].clone();
    }
    Ok(())
}

pub async fn structural_lines(conn: &mut PgConnection) -> Result<Vec<String>> {
    let mut lines = crate::frozen_fingerprint::structural_lines(conn).await?;
    order_routines(conn, &mut lines, "structural_lines/functions", "function:").await?;
    order_routines(
        conn,
        &mut lines,
        "structural_catalog_detail_lines/aggregates",
        "aggregate:",
    )
    .await?;
    Ok(lines)
}

pub async fn acl_lines(conn: &mut PgConnection, prefix: &str) -> Result<Vec<String>> {
    let mut lines = crate::frozen_fingerprint::acl_lines(conn, prefix).await?;
    order_routines(
        conn,
        &mut lines,
        "acl_lines/function_owners",
        "function_owner:",
    )
    .await?;
    order_routines(conn, &mut lines, "acl_lines/function_acls", "function_acl:").await?;
    Ok(lines)
}

pub async fn compute_fingerprints(
    conn: &mut PgConnection,
    prefix: &str,
    reference_sets: &[ReferenceDataSet],
) -> Result<BTreeMap<String, String>> {
    let mut tx = conn.begin().await?;
    let previous: String = sqlx::query_scalar("SELECT current_setting('search_path')")
        .fetch_one(&mut *tx)
        .await?;
    sqlx::query("SELECT set_config('search_path','pg_catalog',true)")
        .execute(&mut *tx)
        .await?;
    let fingerprints = BTreeMap::from([
        (
            "structural".into(),
            canonical_digest(&structural_lines(&mut tx).await?),
        ),
        (
            "acl".into(),
            canonical_digest(&acl_lines(&mut tx, prefix).await?),
        ),
        (
            "extensions".into(),
            canonical_digest(&extensions_lines(&mut tx, prefix).await?),
        ),
        (
            "reference_data".into(),
            canonical_digest(&reference_data_lines(&mut tx, reference_sets).await?),
        ),
    ]);
    sqlx::query("SELECT set_config('search_path',$1,true)")
        .bind(previous)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(fingerprints)
}
