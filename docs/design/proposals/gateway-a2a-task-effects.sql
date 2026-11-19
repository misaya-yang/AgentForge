-- UNNUMBERED proposal: install through the native authority as a new successor.
-- Search path must resolve these exclusively to the gateway schema.
CREATE TABLE gateway.gateway_a2a_contexts (
 context_id uuid PRIMARY KEY, tenant_id text NOT NULL, user_id text NOT NULL,
 publication_id uuid NOT NULL, version_id uuid NOT NULL, created_at timestamptz NOT NULL DEFAULT NOW(),
 UNIQUE(context_id,tenant_id,user_id,publication_id)
);
CREATE TABLE gateway.gateway_a2a_tasks (
 task_id uuid PRIMARY KEY, context_id uuid NOT NULL, tenant_id text NOT NULL, user_id text NOT NULL,
 publication_id uuid NOT NULL, message_id uuid NOT NULL, request_hash text NOT NULL CHECK(request_hash~'^[0-9a-f]{64}$'),
 effect_state text NOT NULL CHECK(effect_state IN ('reserved','dispatched','unknown','confirmed','rejected')),
 run_id uuid, thread_id uuid, created_at timestamptz NOT NULL DEFAULT NOW(),updated_at timestamptz NOT NULL DEFAULT NOW(),
 FOREIGN KEY(context_id,tenant_id,user_id,publication_id) REFERENCES gateway.gateway_a2a_contexts(context_id,tenant_id,user_id,publication_id),
 UNIQUE(tenant_id,user_id,publication_id,message_id)
);
CREATE INDEX gateway_a2a_context_effects ON gateway.gateway_a2a_tasks(context_id,effect_state);
CREATE TABLE gateway.gateway_a2a_outbound_effects (
 tenant_id text NOT NULL,user_id text NOT NULL,agent_name text NOT NULL,message_id uuid NOT NULL,
 request_hash text NOT NULL CHECK(request_hash~'^[0-9a-f]{64}$'),effect_state text NOT NULL CHECK(effect_state IN ('unknown','confirmed')),
 response jsonb,created_at timestamptz NOT NULL DEFAULT NOW(),updated_at timestamptz NOT NULL DEFAULT NOW(),
 PRIMARY KEY(tenant_id,user_id,agent_name,message_id)
);
CREATE INDEX gateway_a2a_outbound_daily ON gateway.gateway_a2a_outbound_effects(tenant_id,agent_name,created_at);
REVOKE ALL ON gateway.gateway_a2a_contexts,gateway.gateway_a2a_tasks,gateway.gateway_a2a_outbound_effects FROM PUBLIC;
-- Authority integrates least-privilege SELECT,INSERT,UPDATE for ai_gateway_app.
-- No DELETE privilege. Apply existing tenant isolation/role policy conventions.
