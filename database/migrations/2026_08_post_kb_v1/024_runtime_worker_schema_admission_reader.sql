-- UNACTIVATED PROPOSAL: no successor number assigned. Never apply directly.
-- Promote only through canonical authority with complete fingerprint verification.
-- Read-only metadata API for application startup. The canonical owner installs
-- this function transactionally with epoch18. Ledger ACL v1 is unchanged.
-- No public-schema USAGE, ledger SELECT, role membership or business-data rights.
DO $owner_check$
BEGIN
    IF current_user !~ '^[a-z][a-z0-9_]{0,20}_owner$'
       OR current_user <> (SELECT pg_get_userbyid(nspowner) FROM pg_namespace WHERE nspname='assistant') THEN
        RAISE EXCEPTION 'canonical owner scope required';
    END IF;
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname=current_user
               AND (rolcanlogin OR rolsuper OR rolcreaterole OR rolcreatedb
                    OR rolreplication OR rolbypassrls)) THEN
        RAISE EXCEPTION 'canonical unprivileged NOLOGIN owner required';
    END IF;
END
$owner_check$;

CREATE FUNCTION assistant.platform_schema_admission_snapshot_v1()
RETURNS jsonb
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $function$
    SELECT pg_catalog.jsonb_build_object(
        'schema_version', 'ai-platform/schema-admission-snapshot/v1',
        'baselines', (SELECT COALESCE(pg_catalog.jsonb_agg(pg_catalog.to_jsonb(b)), '[]'::jsonb)
            FROM (SELECT baseline_id, manifest_sha256, structural_sha256, acl_sha256,
                         extensions_sha256, reference_data_sha256, source_git_sha
                  FROM public.platform_schema_baselines ORDER BY baseline_id LIMIT 2) b),
        'epoch', (SELECT COALESCE(max(sequence), 0) FROM public.platform_schema_changes),
        'changes', (SELECT COALESCE(pg_catalog.jsonb_agg(pg_catalog.to_jsonb(c)
                            ORDER BY c.baseline_id, c.sequence), '[]'::jsonb)
            FROM (SELECT baseline_id, sequence, name, checksum_sha256
                  FROM public.platform_schema_changes ORDER BY baseline_id, sequence LIMIT 64) c),
        'has_attempts', EXISTS(SELECT 1 FROM public.platform_schema_change_attempts),
        'has_incomplete_attempts', EXISTS(SELECT 1 FROM public.platform_schema_change_attempts
                                         WHERE state IS DISTINCT FROM 'succeeded'))
$function$;
REVOKE ALL ON FUNCTION assistant.platform_schema_admission_snapshot_v1() FROM PUBLIC;

-- Installation DDL only: fixed roles, validated prefix, identifier quoting.
-- The callable reader above contains no dynamic SQL and changes no state.
DO $grant$
DECLARE
    prefix text := left(current_user, length(current_user) - length('owner'));
    principal text;
BEGIN
    IF prefix !~ '^[a-z][a-z0-9_]{0,20}_$' THEN
        RAISE EXCEPTION 'invalid canonical role prefix';
    END IF;
    FOREACH principal IN ARRAY ARRAY['runtime','capability_worker'] LOOP
        EXECUTE format('GRANT EXECUTE ON FUNCTION assistant.platform_schema_admission_snapshot_v1() TO %I', prefix || principal);
    END LOOP;
END
$grant$;
