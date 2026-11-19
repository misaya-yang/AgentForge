//! PostgreSQL `RegistryStorage`, migrated from `DatabaseRegistryStorage` and
//! `ai_gateway_core.persistence.database.DatabaseStorage`.
//!
//! Production queries use `gateway.services`, its owner after the canonical
//! per-service schema migration. The caller supplies a pool; this module does
//! not connect, migrate, mount routes, instantiate connectors or cache records.
//! Save replaces the 27 legacy fields, retaining creation time and advancing
//! updated_at. Reads include inactive services. Lists order by created_at DESC
//! with no invented tie-breaker, and optional tags use overlap, not containment.
//!
//! Database conversion is distinct from configuration-file conversion: unknown
//! enums fall back individually, JSON text is parsed, invalid schema values
//! become {}, and omitted input/output types default to [text]. Source errors
//! propagate; backend diagnostics are redacted. Explicit typed boundaries reject
//! annotation-violating object/scalar/config fields, nonfinite floats and integer
//! overflow. A closed pool errors instead of the Python disabled-store no-op.
//! SQLx decodes rate_limit/async_config JSONB instead of preserving asyncpg's
//! accidentally undecoded JSON text. Other JSON fields preserve the source's
//! exact number of decoding passes, including nested JSON-string quirks.
use crate::{
    protocol::{ContentType, InvocationMode, ServiceDefinition, ServiceType, TransportType},
    service_ownership::ServiceHandle,
    service_registry::{
        RegistryError, RegistryFuture, RegistryStorage, python_truthy, service_from_value,
    },
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, QueryBuilder};

const TABLE: &str = "gateway.services";
const SAVE: &str = r#"
INSERT INTO gateway.services (
    service_id, name, description, version, service_type,
    connector_type, connector_config, supported_modes,
    accepted_content_types, output_content_types,
    input_schema, output_schema, session_enabled, session_adapter,
    timeout, max_retries, retry_delay, circuit_breaker_enabled,
    failure_threshold, recovery_timeout, rate_limit, concurrency_limit,
    service_config, async_config, status, tags, metadata
) VALUES (
    $1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
    $11, $12, $13, $14, $15, $16, $17::text::numeric, $18, $19, $20,
    $21, $22, $23, $24, $25, $26, $27
)
ON CONFLICT (service_id) DO UPDATE SET
    name = EXCLUDED.name,
    description = EXCLUDED.description,
    version = EXCLUDED.version,
    service_type = EXCLUDED.service_type,
    connector_type = EXCLUDED.connector_type,
    connector_config = EXCLUDED.connector_config,
    supported_modes = EXCLUDED.supported_modes,
    accepted_content_types = EXCLUDED.accepted_content_types,
    output_content_types = EXCLUDED.output_content_types,
    input_schema = EXCLUDED.input_schema,
    output_schema = EXCLUDED.output_schema,
    session_enabled = EXCLUDED.session_enabled,
    session_adapter = EXCLUDED.session_adapter,
    timeout = EXCLUDED.timeout,
    max_retries = EXCLUDED.max_retries,
    retry_delay = EXCLUDED.retry_delay,
    circuit_breaker_enabled = EXCLUDED.circuit_breaker_enabled,
    failure_threshold = EXCLUDED.failure_threshold,
    recovery_timeout = EXCLUDED.recovery_timeout,
    rate_limit = EXCLUDED.rate_limit,
    concurrency_limit = EXCLUDED.concurrency_limit,
    service_config = EXCLUDED.service_config,
    async_config = EXCLUDED.async_config,
    status = EXCLUDED.status,
    tags = EXCLUDED.tags,
    metadata = EXCLUDED.metadata,
    updated_at = NOW()
"#;
const GET: &str = "SELECT to_jsonb(s) FROM gateway.services s WHERE service_id = $1";
const DELETE: &str = "DELETE FROM gateway.services WHERE service_id = $1";

/// Empty strings and an empty tag slice are omitted, matching Python truthiness.
#[derive(Clone, Copy, Debug, Default)]
pub struct RegistryFilter<'a> {
    pub status: Option<&'a str>,
    pub service_type: Option<&'a str>,
    pub tags: Option<&'a [String]>,
}

#[derive(Clone)]
pub struct PgRegistryStorage {
    pool: PgPool,
    // Production construction fixes the table. Only module-local tests may
    // substitute a validated, unique fixture schema; no user SQL is accepted.
    table: String,
}
impl PgRegistryStorage {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            table: TABLE.into(),
        }
    }

    fn sql(&self, statement: &str) -> String {
        statement.replace(TABLE, &self.table)
    }

    pub async fn list_filtered(
        &self,
        filter: RegistryFilter<'_>,
    ) -> Result<Vec<ServiceHandle>, RegistryError> {
        let mut query = list_query(&self.table, filter);
        let rows = query
            .build_query_scalar::<Value>()
            .fetch_all(&self.pool)
            .await
            .map_err(unavailable)?;
        rows.into_iter().map(database_service).collect()
    }
}

fn list_query(table: &str, filter: RegistryFilter<'_>) -> QueryBuilder<'static, Postgres> {
    let mut query = QueryBuilder::new(format!("SELECT to_jsonb(s) FROM {table} s WHERE 1=1"));
    if let Some(status) = filter.status.filter(|value| !value.is_empty()) {
        query.push(" AND status = ").push_bind(status.to_owned());
    }
    if let Some(service_type) = filter.service_type.filter(|value| !value.is_empty()) {
        query
            .push(" AND service_type = ")
            .push_bind(service_type.to_owned());
    }
    if let Some(tags) = filter.tags.filter(|value| !value.is_empty()) {
        // SQLx advertises Vec<String> as text[], whereas asyncpg infers the
        // parameter as varchar[] from this column. Unlike assignment, the
        // polymorphic overlap operator requires matching array element types.
        query
            .push(" AND tags && ")
            .push_bind(tags.to_vec())
            .push("::varchar[]");
    }
    query.push(" ORDER BY created_at DESC");
    query
}

fn fallback_enum<E: DeserializeOwned>(value: &mut Value, fallback: &str) {
    // Python str-enum construction does not accept serde's externally tagged
    // map form (for example {"embedding": null}). Only exact strings qualify.
    if !value.is_string() || serde_json::from_value::<E>(value.clone()).is_err() {
        *value = json!(fallback);
    }
}
fn decode_json_text(value: &mut Value) {
    if let Value::String(text) = value {
        *value = serde_json::from_str(text).unwrap_or_else(|_| json!({}));
    }
}

/// The database registry's `_dict_to_service`, not the file loader. Input is
/// the Python-equivalent dictionary (possibly still containing JSON strings).
/// Unknown extra row fields, including database timestamps, are ignored.
pub fn service_from_storage_value(value: &Value) -> Result<ServiceDefinition, RegistryError> {
    let mut fields = value
        .as_object()
        .cloned()
        .ok_or(RegistryError::InvalidConfig(
            "storage_service_must_be_object",
        ))?;
    fallback_enum::<ServiceType>(
        fields.entry("service_type").or_insert(Value::Null),
        "custom",
    );
    fallback_enum::<TransportType>(
        fields.entry("connector_type").or_insert(Value::Null),
        "http",
    );
    for (key, fallback) in [
        ("supported_modes", "sync"),
        ("accepted_content_types", "text"),
        ("output_content_types", "text"),
    ] {
        let items = fields.entry(key).or_insert_with(|| json!([fallback]));
        let items = items.as_array_mut().ok_or(RegistryError::InvalidConfig(
            "storage_service_enum_list_must_be_array",
        ))?;
        for item in items {
            if key == "supported_modes" {
                fallback_enum::<InvocationMode>(item, fallback);
            } else {
                fallback_enum::<ContentType>(item, fallback);
            }
        }
    }
    for key in [
        "connector_config",
        "metadata",
        "input_schema",
        "output_schema",
    ] {
        let value = fields.entry(key).or_insert_with(|| json!({}));
        decode_json_text(value);
        if matches!(key, "input_schema" | "output_schema") {
            if !value.is_object() {
                *value = json!({});
            }
        } else if !value.is_object() {
            // Python dataclasses admit values that violate their dict annotation.
            // Do not silently coerce a bad connector or metadata object to {}.
            return Err(RegistryError::InvalidConfig(
                "invalid_storage_service_object_field",
            ));
        }
    }
    let config = fields.entry("service_config").or_insert(Value::Null);
    if python_truthy(config) {
        decode_json_text(config);
    }
    if !python_truthy(config) {
        *config = Value::Null;
    }
    service_from_value(&Value::Object(fields))
}

/// The database registry's `_service_to_dict`. Absent service_config is omitted
/// so the database layer writes its {} default; nullable legacy JSON stays null
/// here, before the database's truthiness-to-SQL-NULL rule is applied.
pub fn service_to_storage_value(service: &ServiceDefinition) -> Result<Value, RegistryError> {
    if !service.retry_delay.is_finite() {
        return Err(RegistryError::InvalidConfig("nonfinite_float_config"));
    }
    let mut value = serde_json::to_value(service)
        .map_err(|_| RegistryError::InvalidConfig("invalid_storage_service_write"))?;
    let fields = value.as_object_mut().ok_or(RegistryError::InvalidConfig(
        "invalid_storage_service_write",
    ))?;
    for key in [
        "connector_config",
        "metadata",
        "input_schema",
        "output_schema",
    ] {
        if !python_truthy(&fields[key]) {
            fields.insert(key.into(), json!({}));
        }
    }
    if service.service_config.is_none() {
        fields.remove("service_config");
    }
    Ok(value)
}

fn database_service(mut row: Value) -> Result<ServiceHandle, RegistryError> {
    let fields = row
        .as_object_mut()
        .ok_or(RegistryError::InvalidConfig("invalid_storage_service_row"))?;
    // These fields are not decoded by Python DatabaseStorage._row_to_dict.
    // Reconstruct their asyncpg text representation before the registry's one
    // decoding pass. Metadata, unlike these fields, has already been decoded.
    for key in [
        "connector_config",
        "input_schema",
        "output_schema",
        "service_config",
    ] {
        if let Some(value) = fields.get_mut(key) {
            *value = json!(value.to_string());
        }
    }
    // Deliberate typed boundary: rate_limit/async_config retain decoded JSON,
    // rather than the undecoded strings leaked by the Python persistence seam.
    service_from_storage_value(&row).map(ServiceHandle::new)
}

fn unavailable(_: sqlx::Error) -> RegistryError {
    RegistryError::StorageUnavailable
}
fn integer(value: i64) -> Result<i32, RegistryError> {
    i32::try_from(value).map_err(|_| RegistryError::InvalidConfig("storage_integer_overflow"))
}
fn nullable_json(value: &Option<Value>) -> Option<Value> {
    value.as_ref().filter(|value| python_truthy(value)).cloned()
}

/// asyncpg's numeric binary encoder uses Decimal(float), preserving the exact
/// binary float before PostgreSQL's NUMERIC(5,2) rounding. Shortest float text
/// changes half-cent cases (1.005, 2.675). A finite IEEE-754 value has at most
/// 1074 fractional decimal places; this exact fixed representation needs no new
/// decimal codec dependency. Only parameter 17's text->numeric cast differs.
fn numeric_text(value: f64) -> Result<String, RegistryError> {
    if !value.is_finite() {
        return Err(RegistryError::InvalidConfig("nonfinite_float_config"));
    }
    let exponent = ((value.to_bits() >> 52) & 0x7ff) as i32;
    let places = if exponent == 0 {
        1074
    } else {
        (1075 - exponent).max(0)
    } as usize;
    let rendered = format!("{value:.places$}");
    Ok(if places == 0 {
        rendered
    } else {
        rendered
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_owned()
    })
}

impl PgRegistryStorage {
    async fn write(&self, service: ServiceHandle, create_only: bool) -> Result<bool, RegistryError> {
        let insert;
        let statement = if create_only {
            insert = create_statement();
            insert.as_str()
        } else { SAVE };
            // Canonical DatabaseRegistryStorage builds typed/scalar fields before
            // low-level pool acquisition; a nonempty connector remains aliased.
            let connector = service.connector().dictionary_or_empty();
            let service = service.snapshot();
            let mut fields = service_to_storage_value(&service)?;
            // Python copies scalar values above without SQL-width coercion.
            // Driver encoding (including integer bounds) follows acquisition;
            // an unavailable pool must therefore win over a bind-width error.
            let mut connection = self.pool.acquire().await.map_err(unavailable)?;
            fields["connector_config"] = connector.snapshot();
            let modes = service
                .supported_modes
                .iter()
                .map(|mode| mode.as_str())
                .collect::<Vec<_>>();
            let accepted = service
                .accepted_content_types
                .iter()
                .map(|kind| kind.as_str())
                .collect::<Vec<_>>();
            let outputs = service
                .output_content_types
                .iter()
                .map(|kind| kind.as_str())
                .collect::<Vec<_>>();
            let config = fields
                .get("service_config")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let result = sqlx::query(&self.sql(statement))
                .bind(&service.service_id)
                .bind(&service.name)
                .bind(&service.description)
                .bind(&service.version)
                .bind(service.service_type.as_str())
                .bind(service.connector_type.as_str())
                .bind(fields["connector_config"].clone())
                .bind(modes)
                .bind(accepted)
                .bind(outputs)
                .bind(fields["input_schema"].clone())
                .bind(fields["output_schema"].clone())
                .bind(service.session_enabled)
                .bind(&service.session_adapter)
                .bind(integer(service.timeout)?)
                .bind(integer(service.max_retries)?)
                .bind(numeric_text(service.retry_delay)?)
                .bind(service.circuit_breaker_enabled)
                .bind(integer(service.failure_threshold)?)
                .bind(integer(service.recovery_timeout)?)
                .bind(nullable_json(&service.rate_limit))
                .bind(service.concurrency_limit.map(integer).transpose()?)
                .bind(config)
                .bind(nullable_json(&service.async_config))
                .bind(&service.status)
                .bind(&service.tags)
                .bind(fields["metadata"].clone())
                .execute(&mut *connection)
                .await
                .map_err(unavailable)?;
            Ok(result.rows_affected() == 1)
    }
}
/// Fixed SQL, never caller input; retains the exact production 27-field bind order.
fn create_statement() -> String {
    let (insert, _) = SAVE.split_once("ON CONFLICT (service_id)").expect("save conflict clause");
    format!("{insert}ON CONFLICT (service_id) DO NOTHING")
}
impl RegistryStorage for PgRegistryStorage {
    fn save(&self, service: ServiceHandle) -> RegistryFuture<'_, ()> {
        Box::pin(async move { self.write(service, false).await.map(|_| ()) })
    }
    fn insert_if_absent(&self, service: ServiceHandle) -> RegistryFuture<'_, bool> {
        Box::pin(async move { self.write(service, true).await })
    }

    fn get<'a>(&'a self, service_id: &'a str) -> RegistryFuture<'a, Option<ServiceHandle>> {
        Box::pin(async move {
            sqlx::query_scalar::<_, Value>(&self.sql(GET))
                .bind(service_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(unavailable)?
                .map(database_service)
                .transpose()
        })
    }

    fn list(&self) -> RegistryFuture<'_, Vec<ServiceHandle>> {
        Box::pin(self.list_filtered(RegistryFilter::default()))
    }

    fn delete<'a>(&'a self, service_id: &'a str) -> RegistryFuture<'a, bool> {
        Box::pin(async move {
            let result = sqlx::query(&self.sql(DELETE))
                .bind(service_id)
                .execute(&self.pool)
                .await
                .map_err(unavailable)?;
            Ok(result.rows_affected() == 1)
        })
    }
}


#[cfg(test)]
mod atomic_registration_sql_tests {
    use super::*;
    #[test]
    fn atomic_registration_sql_is_single_insert_without_update() {
        let sql=create_statement();
        assert_eq!(sql.matches("INSERT INTO").count(),1);
        assert!(sql.ends_with("ON CONFLICT (service_id) DO NOTHING"));
        assert!(!sql.contains("DO UPDATE")); assert!(!sql.contains("SELECT"));
        assert_eq!(sql.split("ON CONFLICT").next(),SAVE.split("ON CONFLICT").next());
        for i in 1..=27 { assert!(sql.contains(&format!("${i}"))); }
    }
}
