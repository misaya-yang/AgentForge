WITH draft_refs AS (
                SELECT a.agent_id
                FROM agent_draft_knowledge_bindings b
                JOIN agents a
                  ON a.tenant_id = b.tenant_id
                 AND a.current_draft_id = b.draft_id
                WHERE b.tenant_id = $1 AND b.dataset_id = $2
            ),
            version_refs AS (
                SELECT v.agent_id,
                       BOOL_OR(p.publication_id IS NOT NULL) AS active_publication,
                       COUNT(DISTINCT v.agent_version_id) FILTER (
                           WHERE p.publication_id IS NULL
                       ) AS historical_version_count
                FROM agent_version_knowledge_bindings b
                JOIN agent_versions v
                  ON v.tenant_id = b.tenant_id
                 AND v.agent_version_id = b.agent_version_id
                LEFT JOIN agent_publications p
                  ON p.tenant_id = v.tenant_id
                 AND p.agent_id = v.agent_id
                 AND p.version_id = v.agent_version_id
                 AND p.status = 'active'
                WHERE b.tenant_id = $1 AND b.dataset_id = $2
                GROUP BY v.agent_id
            ),
            referenced_agents AS (
                SELECT agent_id FROM draft_refs
                UNION
                SELECT agent_id FROM version_refs
            )
            SELECT a.agent_id, a.name,
                   a.deleted_at IS NULL
                     AND current_draft.draft_id IS NOT NULL
                     AND ($4::boolean OR self_member.principal_id IS NOT NULL) AS visible,
                   d.agent_id IS NOT NULL AS current_draft,
                   COALESCE(v.active_publication, FALSE) AS active_publication,
                   COALESCE(v.historical_version_count, 0) AS historical_version_count
            FROM referenced_agents r
            JOIN agents a ON a.tenant_id = $1 AND a.agent_id = r.agent_id
            LEFT JOIN agent_drafts current_draft
              ON current_draft.tenant_id = a.tenant_id
             AND current_draft.draft_id = a.current_draft_id
            LEFT JOIN draft_refs d ON d.agent_id = a.agent_id
            LEFT JOIN version_refs v ON v.agent_id = a.agent_id
            LEFT JOIN agent_members self_member
              ON self_member.tenant_id = a.tenant_id
             AND self_member.agent_id = a.agent_id
             AND self_member.principal_type = 'user'
             AND self_member.principal_id = $3
            ORDER BY a.name, a.agent_id
