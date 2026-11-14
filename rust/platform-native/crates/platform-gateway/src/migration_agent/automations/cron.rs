//! Five-field cron. PostgreSQL's IANA timezone catalog owns DST conversion.
use super::super::AgentError;
use chrono::{DateTime, Utc};
use sqlx::PgPool;

#[derive(Debug)]
pub struct Cron {
    fields: Vec<Vec<i32>>,
    day_any: bool,
    weekday_any: bool,
}
fn invalid() -> AgentError {
    AgentError::code(422, "AUTOMATION_CRON_INVALID")
}
fn field(value: &str, min: i32, max: i32) -> Result<Vec<i32>, AgentError> {
    let mut result = std::collections::BTreeSet::new();
    for part in value.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((range, step)) => (range, step.parse::<i32>().map_err(|_| invalid())?),
            None => (part, 1),
        };
        if step < 1 || step > max - min + 1 {
            return Err(invalid());
        }
        let (from, to) = if range == "*" {
            (min, max)
        } else if let Some((a, b)) = range.split_once('-') {
            (
                a.parse().map_err(|_| invalid())?,
                b.parse().map_err(|_| invalid())?,
            )
        } else {
            let n = range.parse().map_err(|_| invalid())?;
            (n, if part.contains('/') { max } else { n })
        };
        if from < min || to > max || from > to {
            return Err(invalid());
        }
        result.extend((from..=to).step_by(step as usize));
    }
    if result.is_empty() {
        return Err(invalid());
    }
    Ok(result.into_iter().collect())
}
impl Cron {
    pub fn parse(value: &str) -> Result<Self, AgentError> {
        if value.len() > 160 {
            return Err(invalid());
        }
        let parts: Vec<_> = value.split_whitespace().collect();
        if parts.len() != 5 {
            return Err(invalid());
        }
        let mut fields = vec![
            field(parts[0], 0, 59)?,
            field(parts[1], 0, 23)?,
            field(parts[2], 1, 31)?,
            field(parts[3], 1, 12)?,
            field(parts[4], 0, 7)?,
        ];
        fields[4] = fields[4]
            .iter()
            .map(|v| if *v == 7 { 0 } else { *v })
            .collect();
        Ok(Self {
            fields,
            day_any: parts[2] == "*",
            weekday_any: parts[4] == "*",
        })
    }
    pub async fn next(
        &self,
        pool: &PgPool,
        timezone: &str,
        after: DateTime<Utc>,
    ) -> Result<DateTime<Utc>, AgentError> {
        if timezone.is_empty()
            || timezone.len() > 80
            || !sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM pg_timezone_names WHERE name=$1)",
            )
            .bind(timezone)
            .fetch_one(pool)
            .await?
        {
            return Err(invalid());
        }
        let mut tx = pool.begin().await?;
        sqlx::query("SET LOCAL statement_timeout='2500ms'")
            .execute(&mut *tx)
            .await?;
        // UTC candidates make spring gaps absent and both autumn folds explicit.
        // DOM/DOW follow crontab's OR rule when both fields are restricted.
        let next=sqlx::query_scalar("SELECT tick FROM generate_series(date_trunc('minute',$1::timestamptz)+interval '1 minute', $1::timestamptz+interval '5 years', interval '1 minute') tick WHERE extract(minute from tick AT TIME ZONE $2)::int=ANY($3) AND extract(hour from tick AT TIME ZONE $2)::int=ANY($4) AND extract(month from tick AT TIME ZONE $2)::int=ANY($6) AND CASE WHEN $8 THEN extract(dow from tick AT TIME ZONE $2)::int=ANY($7) WHEN $9 THEN extract(day from tick AT TIME ZONE $2)::int=ANY($5) ELSE extract(day from tick AT TIME ZONE $2)::int=ANY($5) OR extract(dow from tick AT TIME ZONE $2)::int=ANY($7) END LIMIT 1")
            .bind(after).bind(timezone).bind(&self.fields[0]).bind(&self.fields[1]).bind(&self.fields[2]).bind(&self.fields[3]).bind(&self.fields[4]).bind(self.day_any).bind(self.weekday_any)
            .fetch_optional(&mut *tx).await?.ok_or_else(invalid)?;
        tx.commit().await?;
        Ok(next)
    }
}
#[cfg(test)]
mod scheduled_tasks {
    use super::*;
    #[test]
    fn cron_accepts_lists_ranges_steps_and_sunday_alias_without_unbounded_input() {
        let cron = Cron::parse("*/15 9-17 * * 1-5").unwrap();
        assert_eq!(cron.fields[0], vec![0, 15, 30, 45]);
        assert!(cron.day_any && !cron.weekday_any);
        assert_eq!(Cron::parse("0 0 1 * 7").unwrap().fields[4], vec![0]);
        for invalid in [
            "* * * *",
            "* * * * * *",
            "60 * * * *",
            "*/0 * * * *",
            "* * 0 * *",
            "* * * 13 *",
            "* * * * 8",
            "1-0 * * * *",
        ] {
            assert!(Cron::parse(invalid).is_err(), "{invalid}");
        }
    }
}
