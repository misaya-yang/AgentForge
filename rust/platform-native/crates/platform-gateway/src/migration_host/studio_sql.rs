//! Source SQL for immutable Agent resource seals.
pub const SKILLS_0: &str = r####"
            SELECT skill.skill_id, skill.tenant_id, skill.user_id, skill.name,
                   skill.title, skill.description, skill.tags, skill.permissions,
                   skill.enabled, skill.status AS skill_status,
                   skill.disabled_at, skill.deleted_at,
                   version.version_id, version.version, version.revision,
                   version.manifest, version.entrypoint, version.content,
                   version.content_hash, version.status,
                   version.artifact_type, version.created_at,
                   EXISTS (
                       SELECT 1 FROM assistant_skill_version_revocations revoked
                       WHERE revoked.tenant_id = version.tenant_id
                         AND revoked.version_id = version.version_id
                   ) AS revoked
            FROM assistant_skill_versions AS version
            JOIN assistant_skills AS skill
              ON skill.tenant_id = version.tenant_id
             AND skill.skill_id = version.skill_id
            WHERE version.tenant_id = $1
              AND version.version_id = ANY($2::text[]::uuid[])
              AND skill.user_id = $3
              AND skill.enabled = TRUE
              AND skill.status = 'active'
              AND skill.deleted_at IS NULL
              AND version.status = 'active'
              AND version.artifact_type = 'tenant_instruction'
              AND NOT EXISTS (
                  SELECT 1 FROM assistant_skill_version_revocations revoked
                  WHERE revoked.tenant_id = version.tenant_id
                    AND revoked.version_id = version.version_id
              )
            "####;
pub const BINDING_0: &str = r####"
                SELECT s.schema_hash, s.risk_level, srv.auth_method,
                       srv.oauth_audience, c.audience, c.scopes
                FROM mcp_tools t
                JOIN mcp_tool_snapshots s
                  ON s.tenant_id = t.tenant_id
                 AND s.snapshot_id = t.current_snapshot_id
                JOIN mcp_servers srv
                  ON srv.tenant_id = t.tenant_id AND srv.server_id = t.server_id
                JOIN mcp_connections c
                  ON c.tenant_id = t.tenant_id AND c.server_id = t.server_id
                WHERE t.tenant_id = $1 AND t.runtime_name = $2
                  AND c.connection_id = $3::uuid
                  AND c.principal_type = $4
                  AND t.enabled = TRUE AND srv.enabled = TRUE
                  AND srv.deleted_at IS NULL
                  AND c.enabled = TRUE AND c.revoked_at IS NULL
                  AND (c.expires_at IS NULL OR c.expires_at > NOW())
                "####;
pub const BINDING_1: &str = r####"
                    SELECT grant_id, scopes FROM connector_credential_principals
                    WHERE tenant_id = $1 AND grant_id = $2::uuid
                      AND provider = $3 AND principal_type = $4
                      AND enabled = TRUE AND revoked_at IS NULL
                      AND (expires_at IS NULL OR expires_at > NOW())
                    "####;
pub const BINDING_2: &str = r####"
                SELECT provider, mode, enabled FROM connector_configs
                WHERE provider = $1 AND (tenant_id = $2 OR tenant_id = '')
                ORDER BY tenant_id DESC LIMIT 1
                "####;
