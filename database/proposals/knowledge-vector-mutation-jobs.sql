-- Unnumbered proposal: primary authority must assign/grant/verify canonically.
-- API role inserts/reads; worker role additionally updates. No Qdrant key or
-- bearer token is persisted. Dataset is retained after deletion (tombstone).
DO $owner_pre$
DECLARE prefix text:=left(current_user,length(current_user)-length('owner'));
BEGIN
    IF current_user !~ '^[a-z][a-z0-9_]{0,20}_owner$'
       OR current_user<>(SELECT pg_catalog.pg_get_userbyid(nspowner)
                         FROM pg_catalog.pg_namespace WHERE nspname='knowledge')
       OR EXISTS(SELECT 1 FROM pg_catalog.pg_roles WHERE rolname=current_user AND
                 (rolcanlogin OR rolsuper OR rolcreatedb OR rolcreaterole OR
                  rolreplication OR rolbypassrls))
       OR prefix !~ '^[a-z][a-z0-9_]{0,20}_$'
       OR NOT EXISTS(SELECT 1 FROM pg_catalog.pg_roles WHERE rolname=prefix||'knowledge_api')
       OR NOT EXISTS(SELECT 1 FROM pg_catalog.pg_roles WHERE rolname=prefix||'knowledge_worker') THEN
        RAISE EXCEPTION 'canonical NOLOGIN Knowledge owner and roles required';
    END IF;
END $owner_pre$;

CREATE TABLE knowledge.vector_mutation_jobs (
    job_id TEXT PRIMARY KEY CHECK (job_id ~ '^[0-9a-f]{64}$'),
    tenant_id VARCHAR(255) NOT NULL,
    dataset_id VARCHAR(255) NOT NULL,
    requested_by VARCHAR(255) NOT NULL,
    expected_generation TEXT NOT NULL,
    expected_revision BIGINT,
    payload JSONB NOT NULL CHECK (jsonb_typeof(payload) = 'object'),
    state TEXT NOT NULL DEFAULT 'pending' CHECK (state IN ('pending','running','succeeded','failed')),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    result JSONB,
    error_class TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CHECK (length(tenant_id)>0 AND length(dataset_id)>0 AND length(requested_by)>0)
);
CREATE INDEX vector_mutation_jobs_dispatch ON knowledge.vector_mutation_jobs(created_at,job_id) WHERE state IN ('pending','running');
CREATE INDEX vector_mutation_jobs_tenant ON knowledge.vector_mutation_jobs(tenant_id,dataset_id,requested_by);

REVOKE ALL ON knowledge.vector_mutation_jobs FROM PUBLIC;
DO $grants$
DECLARE prefix text:=left(current_user,length(current_user)-length('owner'));
BEGIN
    IF prefix !~ '^[a-z][a-z0-9_]{0,20}_$' THEN
        RAISE EXCEPTION 'invalid canonical Knowledge role prefix';
    END IF;
    EXECUTE format('GRANT SELECT ON knowledge.vector_mutation_jobs TO %I',prefix||'knowledge_api');
    EXECUTE format(
        'GRANT INSERT(job_id,tenant_id,dataset_id,requested_by,expected_generation,payload,expected_revision) ON knowledge.vector_mutation_jobs TO %I',
        prefix||'knowledge_api'
    );
    EXECUTE format(
        'GRANT UPDATE(state,error_class,updated_at) ON knowledge.vector_mutation_jobs TO %I',
        prefix||'knowledge_api'
    );
    EXECUTE format('GRANT SELECT ON knowledge.vector_mutation_jobs TO %I',prefix||'knowledge_worker');
    EXECUTE format(
        'GRANT UPDATE(state,attempts,result,error_class,updated_at) ON knowledge.vector_mutation_jobs TO %I',
        prefix||'knowledge_worker'
    );
END $grants$;

DO $owner_post$
BEGIN
    IF current_user !~ '^[a-z][a-z0-9_]{0,20}_owner$'
       OR current_user<>(SELECT pg_catalog.pg_get_userbyid(nspowner)
                         FROM pg_catalog.pg_namespace WHERE nspname='knowledge')
       OR EXISTS(SELECT 1 FROM pg_catalog.pg_roles WHERE rolname=current_user AND
                 (rolcanlogin OR rolsuper OR rolcreatedb OR rolcreaterole OR
                  rolreplication OR rolbypassrls))
       OR NOT EXISTS(SELECT 1 FROM pg_catalog.pg_class c
                     JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace
                     WHERE n.nspname='knowledge' AND c.relname='vector_mutation_jobs'
                       AND pg_catalog.pg_get_userbyid(c.relowner)=current_user)
       OR EXISTS(SELECT 1 FROM pg_catalog.pg_class c
                 JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace
                 CROSS JOIN LATERAL pg_catalog.aclexplode(
                     COALESCE(c.relacl,pg_catalog.acldefault('r',c.relowner))) a
                 WHERE n.nspname='knowledge' AND c.relname='vector_mutation_jobs'
                   AND a.grantee=0) THEN
        RAISE EXCEPTION 'vector mutation job owner or public ACL changed';
    END IF;
END $owner_post$;
