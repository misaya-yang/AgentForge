WITH visible AS (
  SELECT a.* FROM agents a WHERE a.tenant_id=$1 AND a.deleted_at IS NULL
    AND EXISTS (SELECT 1 FROM agent_members m WHERE m.tenant_id=a.tenant_id
      AND m.agent_id=a.agent_id AND m.principal_type='user' AND m.principal_id=$3)
), refs AS (
  SELECT a.agent_id::text AS agent_id,a.name,'draft'::text AS kind,
    d.draft_id::text AS reference_id,d.revision::text AS revision,
    d.spec #>> '{model,model_id}' AS model_id,
    d.spec #>> '{model,provider_id}' AS provider_id
  FROM visible a JOIN agent_drafts d ON d.tenant_id=a.tenant_id
    AND d.agent_id=a.agent_id AND d.draft_id=a.current_draft_id
  UNION ALL
  SELECT a.agent_id::text,a.name,'publication:'||p.channel,p.publication_id::text,
    v.agent_version_id::text,v.resolved_spec #>> '{model,model_id}',
    v.resolved_spec #>> '{model,provider_id}'
  FROM visible a JOIN agent_publications p ON p.tenant_id=a.tenant_id
    AND p.agent_id=a.agent_id AND p.status='active'
  JOIN agent_versions v ON v.tenant_id=p.tenant_id AND v.agent_id=p.agent_id
    AND v.agent_version_id=p.version_id
)
SELECT to_jsonb(r)||jsonb_build_object('match',CASE WHEN provider_id=$2 THEN 'exact_provider' ELSE 'legacy_unqualified_potential' END)
FROM refs r WHERE model_id IS NOT NULL AND (provider_id=$2 OR COALESCE(provider_id,'')='')
ORDER BY agent_id,kind,reference_id LIMIT 2001
