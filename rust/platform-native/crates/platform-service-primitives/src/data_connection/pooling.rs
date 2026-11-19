//! Aggregate budgets, not independent per-pool limits. No sockets or env reads.
use super::config::ensure;
use super::{ConnectionConfig, ConnectionMode, ConnectionPurpose, DataError, DatabaseRole};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Budget {
    pub schema: String,
    pub postgres_max_connections: u32,
    pub administrative_reserve: u32,
    pub unmanaged_backend_reserve: u32,
    pub role_limits: BTreeMap<String, u32>,
    pub poolers: Vec<Pooler>,
    pub clients: Vec<Client>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pooler {
    pub id: String,
    pub replicas: u32,
    pub max_client_conn: u32,
    pub max_user_connections: u32,
    pub max_prepared_statements: u32,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Client {
    pub id: String,
    pub role: String,
    pub replicas: u32,
    pub pool_max: u32,
    pub mode: String,
    pub purpose: String,
    pub database_alias: String,
    pub pooler: Option<String>,
    pub statement_cache_capacity: u32,
}
impl Client {
    pub fn from_config(
        id: &str,
        c: &ConnectionConfig,
        pooler: Option<String>,
    ) -> Result<Self, DataError> {
        c.validate()?;
        Ok(Self {
            id: id.into(),
            role: c.role.name().into(),
            replicas: c.replicas,
            pool_max: c.max_connections,
            mode: match c.mode {
                ConnectionMode::Direct => "direct",
                ConnectionMode::Session => "session",
                ConnectionMode::Transaction => "transaction",
            }
            .into(),
            purpose: match c.purpose {
                ConnectionPurpose::BackendPinned => "backend_pinned",
                ConnectionPurpose::TenantTransactions => "tenant_transactions",
                ConnectionPurpose::CancellationControl => "cancellation_control",
            }
            .into(),
            database_alias: c.expected_database.clone(),
            pooler,
            statement_cache_capacity: if c.mode == ConnectionMode::Transaction {
                128
            } else {
                100
            },
        })
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct RoleBudget {
    pub direct_backends: u64,
    pub pooled_backends: u64,
    pub total_backend_bound: u64,
    pub role_connection_limit: u32,
}
#[derive(Clone, Debug, Serialize)]
pub struct Report {
    pub roles: BTreeMap<String, RoleBudget>,
    pub frontend_connections: BTreeMap<String, u64>,
    pub total_backend_bound: u64,
    pub reserved_connections: u64,
    pub postgres_max_connections: u32,
    pub effects: bool,
    pub replica_capacity_acceptance: bool,
}
fn id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}
impl Budget {
    pub fn validate(&self) -> Result<Report, DataError> {
        ensure(
            self.schema == "agentforge-role-pool-budget/v1",
            "budget_schema",
        )?;
        ensure(
            (16..=100000).contains(&self.postgres_max_connections)
                && self.administrative_reserve >= 3
                && self.clients.len() <= 128
                && !self.clients.is_empty()
                && self.poolers.len() <= 16,
            "budget_bounds",
        )?;
        let expected = DatabaseRole::ALL
            .map(|r| r.name().to_string())
            .into_iter()
            .collect::<BTreeSet<_>>();
        ensure(
            self.role_limits.keys().cloned().collect::<BTreeSet<_>>() == expected
                && self.role_limits.values().all(|n| (1..=128).contains(n)),
            "budget_role_inventory",
        )?;
        let mut roles = self
            .role_limits
            .iter()
            .map(|(name, limit)| {
                (
                    name.clone(),
                    RoleBudget {
                        direct_backends: 0,
                        pooled_backends: 0,
                        total_backend_bound: 0,
                        role_connection_limit: *limit,
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        let mut poolers = BTreeMap::new();
        let mut frontend = BTreeMap::<String, u64>::new();
        for p in &self.poolers {
            ensure(
                id(&p.id)
                    && (1..=16).contains(&p.replicas)
                    && (1..=10000).contains(&p.max_client_conn)
                    && (1..=64).contains(&p.max_user_connections)
                    && (1..=10000).contains(&p.max_prepared_statements)
                    && poolers.insert(p.id.clone(), p).is_none(),
                "budget_pooler",
            )?;
            frontend.insert(p.id.clone(), 0);
        }
        let mut ids = BTreeSet::new();
        let mut used = BTreeSet::new();
        for c in &self.clients {
            ensure(
                id(&c.id)
                    && ids.insert(c.id.clone())
                    && (1..=64).contains(&c.replicas)
                    && (1..=64).contains(&c.pool_max),
                "budget_client",
            )?;
            let role = DatabaseRole::ALL
                .into_iter()
                .find(|r| r.name() == c.role)
                .ok_or(DataError("budget_client_role"))?;
            ensure(
                matches!(
                    c.purpose.as_str(),
                    "backend_pinned" | "tenant_transactions" | "cancellation_control"
                ),
                "budget_client_purpose",
            )?;
            ensure(
                c.purpose != "cancellation_control" || (c.mode == "direct" && c.pool_max <= 2),
                "budget_cancellation_identity",
            )?;
            let n = u64::from(c.replicas) * u64::from(c.pool_max);
            match c.mode.as_str() {
                "direct" => {
                    ensure(
                        c.pooler.is_none() && id(&c.database_alias),
                        "budget_direct_alias",
                    )?;
                    roles.get_mut(&c.role).unwrap().direct_backends += n;
                }
                "session" | "transaction" => {
                    ensure(
                        c.mode != "transaction" || c.purpose == "tenant_transactions",
                        "budget_transaction_identity",
                    )?;
                    let p = poolers
                        .get(
                            c.pooler
                                .as_deref()
                                .ok_or(DataError("budget_pooler_required"))?,
                        )
                        .ok_or(DataError("budget_unknown_pooler"))?;
                    let alias = format!(
                        "{}_{}",
                        role.alias(),
                        if c.mode == "transaction" {
                            "tx"
                        } else {
                            "session"
                        }
                    );
                    ensure(
                        c.database_alias == alias
                            && c.statement_cache_capacity > 0
                            && c.statement_cache_capacity <= p.max_prepared_statements,
                        "budget_alias_or_prepared_statements",
                    )?;
                    *frontend.get_mut(&p.id).unwrap() += n;
                    // max_user_connections is shared across this role's session
                    // and tx aliases, but NOT shared across pooler replicas.
                    if used.insert((p.id.clone(), c.role.clone())) {
                        roles.get_mut(&c.role).unwrap().pooled_backends +=
                            u64::from(p.replicas) * u64::from(p.max_user_connections);
                    }
                }
                _ => return Err(DataError("budget_client_mode")),
            }
        }
        for (name, n) in &frontend {
            let p = poolers[name];
            // Conservative failover: all frontends must fit one surviving
            // pooler, not merely a hoped-for even load-balancer distribution.
            ensure(
                *n > 0 && *n <= u64::from(p.max_client_conn),
                "budget_frontend_failover_capacity",
            )?;
        }
        let mut total = 0;
        for r in roles.values_mut() {
            r.total_backend_bound = r.direct_backends + r.pooled_backends;
            ensure(r.total_backend_bound > 0, "budget_role_clients_missing")?;
            ensure(
                r.total_backend_bound <= u64::from(r.role_connection_limit),
                "aggregate_role_connection_budget",
            )?;
            total += r.total_backend_bound;
        }
        let reserve =
            u64::from(self.administrative_reserve) + u64::from(self.unmanaged_backend_reserve);
        ensure(
            total + reserve <= u64::from(self.postgres_max_connections),
            "aggregate_postgres_connection_budget",
        )?;
        Ok(Report {
            roles,
            frontend_connections: frontend,
            total_backend_bound: total,
            reserved_connections: reserve,
            postgres_max_connections: self.postgres_max_connections,
            effects: false,
            replica_capacity_acceptance: false,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Budget {
        Budget {
            schema: "agentforge-role-pool-budget/v1".into(),
            postgres_max_connections: 100,
            administrative_reserve: 10,
            unmanaged_backend_reserve: 10,
            role_limits: DatabaseRole::ALL.map(|r| (r.name().into(), 64)).into(),
            poolers: vec![Pooler {
                id: "reviewed".into(),
                replicas: 2,
                max_client_conn: 256,
                max_user_connections: 8,
                max_prepared_statements: 200,
            }],
            clients: DatabaseRole::ALL
                .into_iter()
                .flat_map(|r| {
                    [
                        Client {
                            id: format!("{}-data", r.alias()),
                            role: r.name().into(),
                            replicas: 2,
                            pool_max: 4,
                            mode: "session".into(),
                            purpose: "backend_pinned".into(),
                            database_alias: format!("{}_session", r.alias()),
                            pooler: Some("reviewed".into()),
                            statement_cache_capacity: 100,
                        },
                        Client {
                            id: format!("{}-control", r.alias()),
                            role: r.name().into(),
                            replicas: 2,
                            pool_max: 2,
                            mode: "direct".into(),
                            purpose: "cancellation_control".into(),
                            database_alias: "gateway".into(),
                            pooler: None,
                            statement_cache_capacity: 100,
                        },
                    ]
                })
                .collect(),
        }
    }
    #[test]
    fn aggregate_accounts_for_pooler_replicas_and_direct_controls() {
        let mut b = fixture();
        assert!(b.validate().is_err());
        b.postgres_max_connections = 120;
        let r = b.validate().unwrap();
        assert_eq!(r.total_backend_bound, 100);
        assert_eq!(r.roles["ai_gateway_runtime"].pooled_backends, 16);
        assert_eq!(r.roles["ai_gateway_runtime"].direct_backends, 4);
        assert!(!r.replica_capacity_acceptance);
    }
    #[test]
    fn two_aliases_share_only_one_per_user_cap() {
        let mut b = fixture();
        b.postgres_max_connections = 120;
        let mut c = b.clients[0].clone();
        c.id = "extra-scoped-gateway".into();
        c.mode = "transaction".into();
        c.purpose = "tenant_transactions".into();
        c.database_alias = "gateway_tx".into();
        c.statement_cache_capacity = 128;
        b.clients.push(c);
        assert_eq!(
            b.validate().unwrap().roles["ai_gateway_gateway"].pooled_backends,
            16
        );
    }
    #[test]
    fn individually_safe_direct_pools_can_exhaust_one_role() {
        let mut b = fixture();
        b.postgres_max_connections = 1000;
        for i in 0..20 {
            let mut c = b.clients[1].clone();
            c.id = format!("extra-{i}");
            b.clients.push(c);
        }
        assert_eq!(
            b.validate().unwrap_err(),
            DataError("aggregate_role_connection_budget")
        );
    }
    #[test]
    fn prepared_alias_cancellation_and_failover_negatives() {
        let b = fixture();
        for mutation in 0..5 {
            let mut c = b.clone();
            c.postgres_max_connections = 120;
            match mutation {
                0 => c.clients[0].database_alias = "runtime_session".into(),
                1 => c.clients[1].mode = "transaction".into(),
                2 => c.clients[0].statement_cache_capacity = 201,
                3 => c.poolers[0].max_client_conn = 1,
                _ => c.poolers[0].replicas = 0,
            }
            assert!(c.validate().is_err());
        }
    }
}

#[cfg(test)]
mod inventory_negatives {
    use super::*;
    #[test]
    fn checked_in_budget_covers_all_five_roles_and_rejects_narrow_or_unknown_inventory() {
        let bytes =
            include_str!("../../../../../../deploy/data/pgbouncer/connection-budget.example.json");
        let original: Budget = serde_json::from_str(bytes).unwrap();
        assert_eq!(original.validate().unwrap().total_backend_bound, 60);
        let mut b = original.clone();
        b.clients.retain(|c| c.role != "ai_gateway_runtime");
        assert!(b.validate().is_err());
        let mut b = original.clone();
        b.clients[0].role = "postgres".into();
        assert!(b.validate().is_err());
        let mut b = original.clone();
        b.clients.push(b.clients[0].clone());
        assert!(b.validate().is_err());
    }
}
