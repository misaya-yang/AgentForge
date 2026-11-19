-- READ-ONLY authority/isolated-rehearsal catalog export, not a successor/DDL.
-- Execute only through the selected native authority's read-only observation path.
-- No identity, policy, data, role, frozen fingerprint or setting is persisted.
BEGIN READ ONLY;
SET LOCAL search_path=pg_catalog;
SET LOCAL statement_timeout='10s';
SELECT jsonb_build_object(
 'schema','agentforge-tenant-rls-catalog/v1','database',current_database(),
 'controls',(SELECT COALESCE(jsonb_agg(jsonb_build_object(
     'signature',p.oid::regprocedure::text,'owner',pg_get_userbyid(p.proowner),
     'security_definer',p.prosecdef,'volatility',p.provolatile::text,
     'configuration',COALESCE(to_jsonb(p.proconfig),'[]'::jsonb),
     'execute_roles',(SELECT jsonb_agg(CASE WHEN a.grantee=0 THEN 'PUBLIC' ELSE pg_get_userbyid(a.grantee) END ORDER BY a.grantee)
       FROM aclexplode(COALESCE(p.proacl,acldefault('f',p.proowner))) a WHERE a.privilege_type='EXECUTE'),
     'grantable',(SELECT COALESCE(bool_or(a.is_grantable),false)
       FROM aclexplode(COALESCE(p.proacl,acldefault('f',p.proowner))) a WHERE a.privilege_type='EXECUTE')
   ) ORDER BY p.oid::regprocedure::text),'[]'::jsonb) FROM pg_proc p WHERE p.oid=ANY(ARRAY[
     to_regprocedure('assistant.runtime_thread_scope_v1(uuid,boolean,boolean)'),
     to_regprocedure('assistant.runtime_run_scope_v1(uuid)'),
     to_regprocedure('assistant.gateway_run_scope_v1(uuid)'),
     to_regprocedure('assistant.gateway_model_lease_scope_v1(uuid)'),
     to_regprocedure('assistant.gateway_model_call_scope_v1(uuid)'),
     to_regprocedure('assistant.recoverable_capability_executions_v1()'),
     to_regprocedure('knowledge.discover_pipeline_work_v1()'),
     to_regprocedure('knowledge.claim_pipeline_work_v1(text,text,double precision)'),
     to_regprocedure('knowledge.document_ingest_scope_v1(text,text)'),
     to_regprocedure('knowledge.next_confluence_acl_refresh_v1()'),
     to_regprocedure('knowledge.stuck_documents_v1(integer)'),
     to_regprocedure('knowledge.unfinished_publications_v1()'),
     to_regprocedure('knowledge.queued_documents_v1(bigint,text,double precision,text[])'),
     to_regprocedure('knowledge.claim_document_batch_item_v1(text,double precision)')
   ])),
 'roles',(SELECT jsonb_agg(jsonb_build_object('name',r.rolname,'superuser',r.rolsuper,
     'bypassrls',r.rolbypassrls,'inherits_owner',pg_has_role(r.rolname,'ai_gateway_owner','MEMBER'),
     'inherits_privileged',EXISTS(SELECT 1 FROM pg_roles privileged WHERE (privileged.rolsuper OR privileged.rolbypassrls) AND pg_has_role(r.rolname,privileged.rolname,'MEMBER'))) ORDER BY r.rolname)
   FROM pg_roles r WHERE r.rolname=ANY(ARRAY['ai_gateway_gateway','ai_gateway_runtime','ai_gateway_capability_worker','ai_gateway_knowledge_api','ai_gateway_knowledge_worker'])),
 'tables',(SELECT jsonb_agg(jsonb_build_object('relation',n.nspname||'.'||c.relname,
     'owner',pg_get_userbyid(c.relowner),'enabled',c.relrowsecurity,'forced',c.relforcerowsecurity,
     'policies',COALESCE((SELECT jsonb_agg(jsonb_build_object('name',p.polname,
         'permissive',p.polpermissive,'command',p.polcmd::text,
         'roles',(SELECT jsonb_agg(CASE WHEN x=0 THEN 'PUBLIC' ELSE pg_get_userbyid(x) END ORDER BY x) FROM unnest(p.polroles) x),
         'using_expression',pg_get_expr(p.polqual,p.polrelid),
         'check_expression',pg_get_expr(p.polwithcheck,p.polrelid)) ORDER BY p.polname)
       FROM pg_policy p WHERE p.polrelid=c.oid),'[]'::jsonb)) ORDER BY n.nspname,c.relname)
   FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
   WHERE n.nspname||'.'||c.relname=ANY(ARRAY['assistant.agent_runtime_attachments','assistant.agent_runtime_feedback','assistant.agent_runtime_idempotency','assistant.artifact_shares','assistant.artifacts','assistant.assistant_audit_events','assistant.assistant_capability_events','assistant.assistant_capability_executions','assistant.assistant_command_queue','assistant.assistant_context_breakdown','assistant.assistant_memory_chunks','assistant.assistant_memory_reflections','assistant.assistant_memory_sources','assistant.assistant_run_checkpoints','assistant.assistant_runs','assistant.assistant_runtime_items','assistant.assistant_runtime_model_calls','assistant.assistant_runtime_model_leases','assistant.assistant_runtime_snapshot_revocations','assistant.assistant_runtime_snapshots','assistant.assistant_runtime_thread_members','assistant.assistant_runtime_thread_projections','assistant.assistant_runtime_threads','assistant.assistant_scheduler_jobs','assistant.assistant_session_runtime_assignments','assistant.assistant_skill_runs','assistant.assistant_skill_version_revocations','assistant.assistant_skill_versions','assistant.assistant_skills','assistant.assistant_tool_approvals','assistant.conversation_shares','assistant.exams','assistant.image_tasks','assistant.quizzes','assistant.session_memory','assistant.sessions','assistant.user_memory','knowledge.child_chunks','knowledge.confluence_connections','knowledge.confluence_space_bindings','knowledge.dataset_collection_bindings','knowledge.dataset_query_feedback','knowledge.datasets','knowledge.kb_bm25_v2_lifecycle','knowledge.kb_document_batch_operations','knowledge.kb_parsing_ir','knowledge.kb_parsing_page_cache','knowledge.kb_segment_attachment_bindings','knowledge.documents','knowledge.segments'])));
ROLLBACK;
