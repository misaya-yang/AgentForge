-- UNNUMBERED PROPOSAL ONLY. Apply only through database authority after review.
-- No bootstrap/runtime DDL and no frozen schema/fingerprint modifications.
CREATE TABLE knowledge.pipeline_work (
 document_id varchar PRIMARY KEY REFERENCES knowledge.documents(document_id),
 dataset_id varchar NOT NULL REFERENCES knowledge.datasets(dataset_id), tenant_id varchar NOT NULL,
 job_id uuid NOT NULL UNIQUE, execution_id varchar,
 phase text NOT NULL CHECK (phase IN ('worker','parse','embed','index')),
 status text NOT NULL CHECK (status IN ('ready','running','completed','error')),
 owner_token text, fence bigint NOT NULL DEFAULT 0 CHECK (fence>=0),
 lease_expires_at timestamptz, checkpoints jsonb NOT NULL DEFAULT '{}' CHECK(jsonb_typeof(checkpoints)='object'),
 updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 CHECK ((status='running')=(owner_token IS NOT NULL AND lease_expires_at IS NOT NULL))
);
CREATE INDEX pipeline_work_dispatch ON knowledge.pipeline_work(phase,status,updated_at);
-- Grants/RLS must use the native authority's actual Knowledge API/worker roles.
-- API role: no writes/claims. Worker role: tenant-authorized work + CAS only.
