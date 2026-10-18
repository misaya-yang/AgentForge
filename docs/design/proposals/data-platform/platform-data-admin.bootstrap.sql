-- ADMIN-ONLY AUTHORITY BOOTSTRAP PROPOSAL. Not an application role or normal successor.
-- A future audited operations identity may SET ROLE only after explicit admin provisioning.
-- No LOGIN, password, application membership or default service credential is created.
CREATE ROLE ai_gateway_platform_data_admin NOLOGIN NOINHERIT NOSUPERUSER
  NOCREATEDB NOCREATEROLE NOREPLICATION BYPASSRLS;
GRANT USAGE ON SCHEMA knowledge,assistant TO ai_gateway_platform_data_admin;
GRANT SELECT ON knowledge.datasets,knowledge.documents,knowledge.segments,
  knowledge.child_chunks,knowledge.confluence_connections,
  assistant.artifacts,assistant.assistant_runtime_items,assistant.assistant_runs,
  assistant.sessions TO ai_gateway_platform_data_admin;
-- Mutation privileges require a separate reviewed operation, never this read-only identity.
-- The request/job owner must write an immutable audit event before and after use.
