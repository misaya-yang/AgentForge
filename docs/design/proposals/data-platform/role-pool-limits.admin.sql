-- ADMIN-ONLY AUTHORITY BOOTSTRAP PROPOSAL, not an ordinary migrator successor.
-- Existing migrator intentionally lacks CREATEROLE; never retry this as an app.
-- Measure role settings/limits through the native bootstrap authority before admission.
ALTER ROLE ai_gateway_gateway CONNECTION LIMIT 64;
ALTER ROLE ai_gateway_runtime CONNECTION LIMIT 64;
ALTER ROLE ai_gateway_capability_worker CONNECTION LIMIT 64;
ALTER ROLE ai_gateway_knowledge_api CONNECTION LIMIT 64;
ALTER ROLE ai_gateway_knowledge_worker CONNECTION LIMIT 64;
ALTER ROLE ai_gateway_gateway SET statement_timeout='30s';
ALTER ROLE ai_gateway_runtime SET statement_timeout='30s';
ALTER ROLE ai_gateway_capability_worker SET statement_timeout='30s';
ALTER ROLE ai_gateway_knowledge_api SET statement_timeout='30s';
ALTER ROLE ai_gateway_knowledge_worker SET statement_timeout='30s';
ALTER ROLE ai_gateway_gateway SET idle_in_transaction_session_timeout='30s';
ALTER ROLE ai_gateway_runtime SET idle_in_transaction_session_timeout='30s';
ALTER ROLE ai_gateway_capability_worker SET idle_in_transaction_session_timeout='30s';
ALTER ROLE ai_gateway_knowledge_api SET idle_in_transaction_session_timeout='30s';
ALTER ROLE ai_gateway_knowledge_worker SET idle_in_transaction_session_timeout='30s';
-- No new role, membership, BYPASSRLS, schema privileges or wildcard table grant.
