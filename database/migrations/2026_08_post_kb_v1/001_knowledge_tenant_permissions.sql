-- Forward-only security boundary: old readers cannot resolve tenant_role,
-- and old writers cannot recreate unqualified role grants.
ALTER TABLE knowledge.dataset_permissions ADD COLUMN subject_tenant_id varchar(255);

UPDATE knowledge.dataset_permissions AS p
SET subject_type = 'tenant_role', subject_tenant_id = d.tenant_id
FROM knowledge.datasets AS d
WHERE p.dataset_id = d.dataset_id AND p.subject_type = 'role';

ALTER TABLE knowledge.dataset_permissions
    ADD CONSTRAINT dataset_permission_subject_scope CHECK (
        (subject_type = 'user' AND subject_tenant_id IS NULL)
        OR (subject_type = 'tenant_role' AND subject_tenant_id IS NOT NULL
            AND length(btrim(subject_tenant_id)) > 0
            AND subject_tenant_id = btrim(subject_tenant_id))
    ),
    ADD CONSTRAINT dataset_permission_tenant_fk
        FOREIGN KEY (dataset_id, subject_tenant_id)
        REFERENCES knowledge.datasets(dataset_id, tenant_id);

-- The authority runs as <configured prefix>owner. Never embed a deployment
-- role name: default and namespaced installations receive identical grants.
DO $permissions$
DECLARE
    prefix text := left(current_user, length(current_user) - 5);
    principal text;
    relation_name text;
    sequence_record record;
BEGIN
    IF current_user !~ '^[a-z][a-z0-9_]{0,20}_owner$' THEN
        RAISE EXCEPTION 'Knowledge permissions migration requires the authority owner role';
    END IF;
    FOREACH principal IN ARRAY ARRAY[prefix || 'knowledge_api', prefix || 'knowledge_worker'] LOOP
        FOREACH relation_name IN ARRAY ARRAY['users', 'user_roles', 'user_permissions', 'rbac_roles', 'role_permissions'] LOOP
            EXECUTE format('REVOKE ALL ON TABLE gateway.%I FROM %I', relation_name, principal);
            FOR sequence_record IN
                SELECT n.nspname, c.relname
                FROM pg_class c
                JOIN pg_namespace n ON n.oid = c.relnamespace
                JOIN pg_depend d ON d.classid = 'pg_class'::regclass AND d.objid = c.oid
                WHERE c.relkind = 'S' AND d.deptype IN ('a', 'i')
                  AND d.refobjid = to_regclass(format('gateway.%I', relation_name))
            LOOP
                EXECUTE format('REVOKE ALL ON SEQUENCE %I.%I FROM %I', sequence_record.nspname, sequence_record.relname, principal);
            END LOOP;
        END LOOP;
    END LOOP;
END
$permissions$;
