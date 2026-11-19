pub(super) const POLICY_UPSERT: &str = r#"INSERT INTO agent_governance_policies (
                    tenant_id, agent_id, trace_retention_days,
                    runtime_retention_days, attachment_retention_days, legal_hold,
                    principal_requests_per_minute, principal_requests_per_day,
                    ip_requests_per_minute, ip_requests_per_day,
                    publication_requests_per_minute, publication_requests_per_day,
                    alert_threshold_percent, max_agents_per_tenant,
                    max_active_publications, max_concurrent_runs,
                    max_daily_tokens, max_daily_mcp_calls, max_storage_bytes,
                    updated_by
                ) VALUES (
                    $1, $2::uuid, $3, $4, $5, $6, $7, $8, $9, $10,
                    $11, $12, $13, $14, $15, $16, $17, $18, $19, $20
                )
                ON CONFLICT (tenant_id, agent_id) DO UPDATE SET
                    trace_retention_days = EXCLUDED.trace_retention_days,
                    runtime_retention_days = EXCLUDED.runtime_retention_days,
                    attachment_retention_days = EXCLUDED.attachment_retention_days,
                    legal_hold = EXCLUDED.legal_hold,
                    principal_requests_per_minute = EXCLUDED.principal_requests_per_minute,
                    principal_requests_per_day = EXCLUDED.principal_requests_per_day,
                    ip_requests_per_minute = EXCLUDED.ip_requests_per_minute,
                    ip_requests_per_day = EXCLUDED.ip_requests_per_day,
                    publication_requests_per_minute = EXCLUDED.publication_requests_per_minute,
                    publication_requests_per_day = EXCLUDED.publication_requests_per_day,
                    alert_threshold_percent = EXCLUDED.alert_threshold_percent,
                    max_agents_per_tenant = EXCLUDED.max_agents_per_tenant,
                    max_active_publications = EXCLUDED.max_active_publications,
                    max_concurrent_runs = EXCLUDED.max_concurrent_runs,
                    max_daily_tokens = EXCLUDED.max_daily_tokens,
                    max_daily_mcp_calls = EXCLUDED.max_daily_mcp_calls,
                    max_storage_bytes = EXCLUDED.max_storage_bytes,
                    updated_by = EXCLUDED.updated_by,
                    updated_at = NOW()
                RETURNING to_jsonb(agent_governance_policies)"#;
pub(super) const USAGE_SQL: &str = r#"SELECT
                    (
                        SELECT COUNT(*)::bigint
                        FROM assistant_runs
                        WHERE tenant_id = $1 AND agent_id = $2::uuid
                          AND status IN ('queued', 'running', 'awaiting_approval')
                    ) AS concurrent_runs,
                    (
                        SELECT COALESCE(SUM(total_tokens), 0)::bigint
                        FROM agent_traces
                        WHERE tenant_id = $1 AND agent_id = $2::uuid
                          AND created_at >= date_trunc('day', NOW())
                    ) AS daily_tokens,
                    (
                        SELECT COUNT(*)::bigint
                        FROM agent_trace_spans span
                        JOIN agent_traces trace ON trace.trace_id = span.trace_id
                        WHERE trace.tenant_id = $1 AND trace.agent_id = $2::uuid
                          AND trace.created_at >= date_trunc('day', NOW())
                          AND span.span_kind = 'tool_execution'
                          AND (
                              span.name LIKE 'tool:mcp_%'
                              OR span.attributes->>'capability_type' = 'mcp'
                          )
                    ) AS daily_mcp_calls,
                    (
                        SELECT COALESCE(SUM(attachment.size_bytes), 0)::bigint
                        FROM agent_runtime_attachments attachment
                        JOIN agent_publications publication
                          ON publication.tenant_id = attachment.tenant_id
                         AND publication.publication_id = attachment.publication_id
                        WHERE publication.tenant_id = $1
                          AND publication.agent_id = $2::uuid
                          AND attachment.deleted_at IS NULL
                    ) AS storage_bytes"#;
pub(super) const CACHE_UPSERT: &str = r#"INSERT INTO agent_governance_policies (
                    tenant_id, agent_id, updated_by, cache_epoch
                ) VALUES ($1, $2::uuid, $3, 1)
                ON CONFLICT (tenant_id, agent_id) DO UPDATE SET
                    cache_epoch = agent_governance_policies.cache_epoch + 1,
                    updated_by = EXCLUDED.updated_by,
                    updated_at = NOW()
                RETURNING to_jsonb(agent_governance_policies)"#;
pub(super) const REVOKE0: &str = r#"UPDATE agent_api_tokens t
                SET revoked_at = COALESCE(t.revoked_at, NOW())
                FROM agent_publications p
                WHERE p.tenant_id = $1 AND p.agent_id = $2::uuid
                  AND t.tenant_id = p.tenant_id AND t.publication_id = p.publication_id
                  AND t.revoked_at IS NULL"#;
pub(super) const REVOKE1: &str = r#"UPDATE mcp_channel_grants g
                SET enabled = FALSE, updated_at = NOW()
                WHERE g.tenant_id = $1 AND g.enabled = TRUE
                  AND g.tool_id::text IN (
                      SELECT c.resource_id
                      FROM agent_version_capabilities c
                      JOIN agent_versions v
                        ON v.tenant_id = c.tenant_id
                       AND v.agent_version_id = c.agent_version_id
                      WHERE v.tenant_id = $1 AND v.agent_id = $2::uuid
                        AND c.capability_type = 'mcp'
                  )"#;
pub(super) const REVOKE2: &str = r#"UPDATE connector_credential_principals g
                SET enabled = FALSE, revoked_at = COALESCE(revoked_at, NOW()),
                    updated_by = $3, updated_at = NOW()
                WHERE g.tenant_id = $1 AND g.enabled = TRUE
                  AND g.grant_id::text IN (
                      SELECT c.resource_id
                      FROM agent_version_capabilities c
                      JOIN agent_versions v
                        ON v.tenant_id = c.tenant_id
                       AND v.agent_version_id = c.agent_version_id
                      WHERE v.tenant_id = $1 AND v.agent_id = $2::uuid
                        AND c.capability_type = 'connector'
                  )"#;
