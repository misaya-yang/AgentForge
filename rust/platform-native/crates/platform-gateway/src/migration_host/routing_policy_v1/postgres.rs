use super::*;
use serde_json::Value;
use sqlx::Row;
impl RoutingPolicyService {
    pub async fn publish(&self, a: &RoutingAuthority, p: &Policy) -> Result<String> {
        if !a.manage || p.tenant != a.tenant {
            return Err(RoutingError::Forbidden);
        }
        let pin = p.pin()?;
        let payload = serde_json::to_value(p).map_err(|_| RoutingError::InvalidPolicy)?;
        let mut tx = self.pool.begin().await.map_err(storage)?;
        sqlx::query("INSERT INTO gateway.routing_policy_versions(tenant_id,policy_id,version,pin,payload,created_by) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(tenant_id,policy_id,version) DO NOTHING").bind(&a.tenant).bind(&p.policy_id).bind(&p.version).bind(&pin).bind(payload).bind(&a.actor).execute(&mut *tx).await.map_err(storage)?;
        let stored:String=sqlx::query_scalar("SELECT pin FROM gateway.routing_policy_versions WHERE tenant_id=$1 AND policy_id=$2 AND version=$3").bind(&a.tenant).bind(&p.policy_id).bind(&p.version).fetch_one(&mut *tx).await.map_err(storage)?;
        if stored != pin {
            return Err(RoutingError::Conflict);
        }
        tx.commit()
            .await
            .map_err(|_| RoutingError::UncertainCommit)?;
        Ok(pin)
    }
    /// Operator-controlled binding. Request bodies cannot activate a policy.
    pub async fn activate(
        &self,
        a: &RoutingAuthority,
        policy_id: &str,
        version: &str,
        expected: Option<&str>,
    ) -> Result<String> {
        if !a.manage {
            return Err(RoutingError::Forbidden);
        }
        let p = self.load_version(a, policy_id, version).await?;
        let pin = p.pin()?;
        let rows=match expected {
            None=>sqlx::query("INSERT INTO gateway.routing_policy_active(tenant_id,policy_id,pin) VALUES($1,$2,$3) ON CONFLICT DO NOTHING").bind(&a.tenant).bind(policy_id).bind(&pin).execute(&self.pool).await,
            Some(old)=>sqlx::query("UPDATE gateway.routing_policy_active SET pin=$3 WHERE tenant_id=$1 AND policy_id=$2 AND pin=$4").bind(&a.tenant).bind(policy_id).bind(&pin).bind(old).execute(&self.pool).await,
        }.map_err(storage)?.rows_affected();
        if rows != 1 {
            return Err(RoutingError::Conflict);
        }
        Ok(pin)
    }
    pub async fn load_version(
        &self,
        a: &RoutingAuthority,
        id: &str,
        version: &str,
    ) -> Result<Policy> {
        let row=sqlx::query("SELECT pin,payload FROM gateway.routing_policy_versions WHERE tenant_id=$1 AND policy_id=$2 AND version=$3").bind(&a.tenant).bind(id).bind(version).fetch_optional(&self.pool).await.map_err(storage)?.ok_or(RoutingError::NotFound)?;
        decode_policy(
            row.try_get("payload").map_err(storage)?,
            row.try_get("pin").map_err(storage)?,
            &a.tenant,
            id,
        )
    }
    pub async fn active(
        &self,
        a: &RoutingAuthority,
        id: &str,
        pin: Option<&str>,
    ) -> Result<Policy> {
        // A supplied evaluation pin selects an existing immutable version, never
        // a client-authored policy or a foreign tenant policy.
        let row=if let Some(pin)=pin {
            sqlx::query("SELECT pin,payload FROM gateway.routing_policy_versions WHERE tenant_id=$1 AND policy_id=$2 AND pin=$3").bind(&a.tenant).bind(id).bind(pin).fetch_optional(&self.pool).await
        }else{
            sqlx::query("SELECT p.pin,p.payload FROM gateway.routing_policy_active a JOIN gateway.routing_policy_versions p USING(tenant_id,policy_id,pin) WHERE a.tenant_id=$1 AND a.policy_id=$2").bind(&a.tenant).bind(id).fetch_optional(&self.pool).await
        }.map_err(storage)?.ok_or(RoutingError::NotFound)?;
        decode_policy(
            row.try_get("payload").map_err(storage)?,
            row.try_get("pin").map_err(storage)?,
            &a.tenant,
            id,
        )
    }
    pub async fn decide(
        &self,
        a: &RoutingAuthority,
        p: &Policy,
        seed: &str,
        invocation_id: &str,
        at_ms: i64,
        requirements: std::collections::BTreeSet<String>,
        set: CandidateSet,
    ) -> Result<Decision> {
        if set.tenant != a.tenant || set.actor != a.actor {
            return Err(RoutingError::Forbidden);
        }
        // Ensure publication really exists; an in-process caller cannot persist
        // a forged/unpublished policy by only constructing its typed struct.
        let stored = self.load_version(a, &p.policy_id, &p.version).await?;
        if stored.pin()? != p.pin()? {
            return Err(RoutingError::PinMismatch);
        }
        let d = evaluate(
            p,
            Evaluation {
                tenant: a.tenant.clone(),
                actor: a.actor.clone(),
                invocation_id: invocation_id.into(),
                policy_pin: p.pin()?,
                seed: seed.into(),
                at_ms,
                required_capabilities: requirements,
            },
            set.candidates,
        )?;
        let payload = serde_json::to_value(&d).map_err(|_| RoutingError::InvalidSnapshot)?;
        // Bind a single decision to an invocation. A re-evaluation after catalog
        // or policy drift must conflict, not silently change an admitted call.
        sqlx::query("INSERT INTO gateway.routing_decisions(tenant_id,actor_id,invocation_id,decision_id,policy_id,policy_pin,payload) VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT(tenant_id,actor_id,invocation_id) DO NOTHING").bind(&a.tenant).bind(&a.actor).bind(invocation_id).bind(&d.id).bind(&p.policy_id).bind(&d.context.policy_pin).bind(payload).execute(&self.pool).await.map_err(storage)?;
        let saved = self.read_invocation(a, invocation_id).await?;
        if saved != d {
            return Err(RoutingError::Conflict);
        }
        Ok(saved)
    }
    pub async fn read_invocation(&self, a: &RoutingAuthority, id: &str) -> Result<Decision> {
        let payload:Value=sqlx::query_scalar("SELECT payload FROM gateway.routing_decisions WHERE tenant_id=$1 AND actor_id=$2 AND invocation_id=$3").bind(&a.tenant).bind(&a.actor).bind(id).fetch_optional(&self.pool).await.map_err(storage)?.ok_or(RoutingError::NotFound)?;
        decode_decision(payload, a)
    }
    pub async fn read(&self, a: &RoutingAuthority, id: &str) -> Result<Decision> {
        let payload:Value=sqlx::query_scalar("SELECT payload FROM gateway.routing_decisions WHERE tenant_id=$1 AND decision_id=$2 AND (actor_id=$3 OR $4)").bind(&a.tenant).bind(id).bind(&a.actor).bind(a.manage).fetch_optional(&self.pool).await.map_err(storage)?.ok_or(RoutingError::NotFound)?;
        let decision = decode_decision(payload, a)?;
        if decision.id != id {
            return Err(RoutingError::PinMismatch);
        }
        Ok(decision)
    }
}
fn decode_policy(payload: Value, pin: String, tenant: &str, id: &str) -> Result<Policy> {
    let p: Policy = serde_json::from_value(payload).map_err(|_| RoutingError::InvalidPolicy)?;
    if p.tenant != tenant || p.policy_id != id || p.pin()? != pin {
        return Err(RoutingError::PinMismatch);
    }
    Ok(p)
}
fn decode_decision(payload: Value, a: &RoutingAuthority) -> Result<Decision> {
    let d: Decision = serde_json::from_value(payload).map_err(|_| RoutingError::InvalidSnapshot)?;
    d.verify()?;
    if d.context.tenant != a.tenant || (!a.manage && d.context.actor != a.actor) {
        return Err(RoutingError::Forbidden);
    }
    Ok(d)
}
