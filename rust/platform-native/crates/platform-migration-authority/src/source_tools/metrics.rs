//! Original usage-repair SQL and transaction contract. Dry-run is the default.
use crate::Result;
use sqlx::{Connection, PgConnection, Row};
pub const SNAPSHOT_SQL: &str = include_str!("metrics_sql/snapshot_sql.sql");
pub const DEDUPE_DELETE_SQL: &str = include_str!("metrics_sql/dedupe_delete_sql.sql");
pub const REBUILD_DAILY_SQL: &str = include_str!("metrics_sql/rebuild_daily_sql.sql");
pub const REBUILD_HOURLY_SQL: &str = include_str!("metrics_sql/rebuild_hourly_sql.sql");
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub usage_records: i64,
    pub usage_daily_rows: i64,
    pub usage_hourly_rows: i64,
    pub daily_request_sum: i64,
    pub hourly_request_sum: i64,
    pub duplicated_request_ids: i64,
    pub duplicate_extra_rows: i64,
}
pub async fn snapshot(conn: &mut PgConnection) -> Result<Snapshot> {
    let Some(row) = sqlx::query(SNAPSHOT_SQL).fetch_optional(conn).await? else {
        return Ok(Snapshot::default());
    };
    Ok(Snapshot {
        usage_records: row.try_get::<Option<i64>, _>("usage_records")?.unwrap_or(0),
        usage_daily_rows: row
            .try_get::<Option<i64>, _>("usage_daily_rows")?
            .unwrap_or(0),
        usage_hourly_rows: row
            .try_get::<Option<i64>, _>("usage_hourly_rows")?
            .unwrap_or(0),
        daily_request_sum: row
            .try_get::<Option<i64>, _>("daily_request_sum")?
            .unwrap_or(0),
        hourly_request_sum: row
            .try_get::<Option<i64>, _>("hourly_request_sum")?
            .unwrap_or(0),
        duplicated_request_ids: row
            .try_get::<Option<i64>, _>("duplicated_request_ids")?
            .unwrap_or(0),
        duplicate_extra_rows: row
            .try_get::<Option<i64>, _>("duplicate_extra_rows")?
            .unwrap_or(0),
    })
}
pub async fn repair(conn: &mut PgConnection) -> Result<i64> {
    let mut tx = conn.begin().await?;
    let removed: Option<i64> = sqlx::query_scalar(DEDUPE_DELETE_SQL)
        .fetch_one(&mut *tx)
        .await?;
    sqlx::raw_sql("TRUNCATE TABLE usage_daily_aggregates, usage_hourly_aggregates")
        .execute(&mut *tx)
        .await?;
    sqlx::raw_sql(REBUILD_DAILY_SQL).execute(&mut *tx).await?;
    sqlx::raw_sql(REBUILD_HOURLY_SQL).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(removed.unwrap_or(0))
}
pub fn format_snapshot(title: &str, s: &Snapshot) -> String {
    format!(
        "\n[{title}]\nusage_records:           {}\nusage_daily_rows:        {}\nusage_hourly_rows:       {}\ndaily_request_sum:       {}\nhourly_request_sum:      {}\nduplicated_request_ids:  {}\nduplicate_extra_rows:    {}\n",
        s.usage_records,
        s.usage_daily_rows,
        s.usage_hourly_rows,
        s.daily_request_sum,
        s.hourly_request_sum,
        s.duplicated_request_ids,
        s.duplicate_extra_rows
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn output_contract() {
        assert_eq!(
            format_snapshot("Before", &Snapshot::default()),
            "\n[Before]\nusage_records:           0\nusage_daily_rows:        0\nusage_hourly_rows:       0\ndaily_request_sum:       0\nhourly_request_sum:      0\nduplicated_request_ids:  0\nduplicate_extra_rows:    0\n"
        );
    }
    #[test]
    fn immutable_sql_digests() {
        use sha2::{Digest, Sha256};
        let fixtures = include_str!("metrics_sql/digests.json");
        let expected: serde_json::Value = serde_json::from_str(fixtures).unwrap();
        for (name, sql) in [
            ("SNAPSHOT_SQL", SNAPSHOT_SQL),
            ("DEDUPE_DELETE_SQL", DEDUPE_DELETE_SQL),
            ("REBUILD_DAILY_SQL", REBUILD_DAILY_SQL),
            ("REBUILD_HOURLY_SQL", REBUILD_HOURLY_SQL),
        ] {
            assert_eq!(
                format!("{:x}", Sha256::digest(sql.as_bytes())),
                expected[name].as_str().unwrap()
            );
        }
    }
}
