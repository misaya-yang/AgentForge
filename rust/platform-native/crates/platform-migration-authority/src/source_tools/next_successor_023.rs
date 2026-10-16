//! The reviewed 022 -> 023 Local Node, Knowledge, and Assistant-runtime extension.
//! It reuses the strict active-022 verifier, safe source helpers, and shared one-step runner.
use crate::{
    AuthorityError, Result, commands, distribution, manifest,
    source_tools::{measure_successors, next_successor, next_successor_022},
    sql::reject_embedded_transactions,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
};

pub const BASELINE: &str = "2026_08_post_kb_v1";
pub const PREDECESSOR_EPOCH: i32 = 22;
pub const CANDIDATE_EPOCH: i32 = 23;
pub const APPROVED_022_LOCK_SHA256: &str =
    "df91c12df09ecd9f6361eceaf476ac3230cff43e1548217cfcd214286ee37855";
const REVIEWED_023_PLAN_SHA256: &str =
    "9ebce9c791d52db0266f4105a9930dd7649a6e3a1576908daf51f71077a86f11";
const PLAN_RELATIVE: &str = "docs/design/canonical-schema-successor-023.json";
const LOCK_ARCHIVE_RELATIVE: &str =
    "docs/design/canonical-schema-successor-023-approved22-package-lock.json";
const MANIFEST_ARCHIVE_RELATIVE: &str =
    "docs/design/canonical-schema-successor-023-approved22-manifest.yml";
const PROPOSAL_RELATIVE: &str = "docs/design/native-workflow-authority-023.sql.proposal";
const ACTIVE_MANIFEST_RELATIVE: &str = "database/migrations/2026_08_post_kb_v1/manifest.yml";
const ACTIVE_LOCK_RELATIVE: &str = "database/authority/package-authority-lock.json";
const EPOCH_RELATIVE: &str = "migrations/2026_08_post_kb_v1";
pub const NEXT_SQL_FILE: &str = "023_native_workflow_authority.sql";
const NEXT_REFERENCE_FILE: &str = "023_verification.json";
const NEXT_MEASUREMENT_FILE: &str = "SUCCESSOR_023_MEASUREMENT.json";
const NEXT_ACTIVATION_FILE: &str = "SUCCESSOR_023_ACTIVATION.json";
const PREVIEW_RECEIPT_FILE: &str = "PREVIEW_023_NOT_ACTIVATED.json";
const ACL_DELTA: &str = "schema_acl:public:migrator:USAGE:grantopt=0";
const PRECONDITIONS: [&str; 6] = [
    "SELECT to_regclass('gateway.local_node_devices') IS NOT NULL AND to_regclass('gateway.local_node_channels') IS NOT NULL AND to_regclass('gateway.local_node_executions') IS NOT NULL AND to_regclass('gateway.local_node_pairing_challenges') IS NOT NULL AND to_regclass('gateway.local_node_grants') IS NOT NULL AND to_regclass('gateway.local_node_device_credentials') IS NULL AND NOT EXISTS (SELECT 1 FROM information_schema.columns WHERE table_schema='gateway' AND table_name='local_node_executions' AND column_name='cancel_requested_at')",
    "SELECT EXISTS (SELECT 1 FROM information_schema.columns WHERE table_schema='gateway' AND table_name='local_node_channels' AND column_name='fingerprint') AND has_table_privilege(left(current_user,length(current_user)-5)||'gateway','gateway.local_node_executions','UPDATE')",
    "SELECT to_regclass('knowledge.dataset_collection_bindings') IS NOT NULL AND to_regclass('knowledge.embedding_migrations') IS NOT NULL AND to_regclass('knowledge.embedding_migration_action_jobs') IS NOT NULL AND to_regclass('knowledge.embedding_migration_progress') IS NOT NULL AND NOT has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_api','knowledge.dataset_collection_bindings','SELECT') AND NOT has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_api','knowledge.dataset_collection_bindings','INSERT') AND NOT has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_api','knowledge.dataset_collection_bindings','UPDATE') AND NOT has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_api','knowledge.embedding_migrations','SELECT') AND NOT has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_api','knowledge.embedding_migrations','INSERT') AND NOT has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_api','knowledge.embedding_migrations','UPDATE') AND NOT has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_api','knowledge.embedding_migration_action_jobs','SELECT') AND NOT has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_api','knowledge.embedding_migration_action_jobs','INSERT') AND NOT has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_api','knowledge.embedding_migration_action_jobs','UPDATE') AND NOT has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_api','knowledge.embedding_migration_progress','SELECT') AND NOT has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_api','knowledge.embedding_migration_progress','DELETE') AND NOT has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_worker','knowledge.embedding_migration_progress','SELECT')",
    "SELECT to_regclass('knowledge.kb_document_batch_operations') IS NOT NULL AND to_regclass('knowledge.kb_document_batch_items') IS NOT NULL AND has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_api','knowledge.kb_document_batch_operations','SELECT') AND has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_api','knowledge.kb_document_batch_operations','INSERT') AND has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_api','knowledge.kb_document_batch_operations','UPDATE') AND NOT has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_api','knowledge.kb_document_batch_operations','DELETE') AND has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_api','knowledge.kb_document_batch_items','SELECT') AND has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_api','knowledge.kb_document_batch_items','INSERT') AND has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_api','knowledge.kb_document_batch_items','UPDATE') AND NOT has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_api','knowledge.kb_document_batch_items','DELETE') AND NOT has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_worker','knowledge.kb_document_batch_operations','SELECT') AND NOT has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_worker','knowledge.kb_document_batch_operations','INSERT') AND NOT has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_worker','knowledge.kb_document_batch_operations','UPDATE') AND NOT has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_worker','knowledge.kb_document_batch_operations','DELETE') AND NOT has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_worker','knowledge.kb_document_batch_items','SELECT') AND NOT has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_worker','knowledge.kb_document_batch_items','INSERT') AND NOT has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_worker','knowledge.kb_document_batch_items','UPDATE') AND NOT has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_worker','knowledge.kb_document_batch_items','DELETE')",
    "SELECT EXISTS (SELECT 1 FROM pg_proc p WHERE p.oid=to_regprocedure('assistant.issue_assistant_runtime_turn(uuid,uuid,uuid,uuid,character varying,character varying,character varying,character varying,character varying,jsonb,character,bigint,character varying,character varying,character varying,character varying,character varying,character,integer,bigint,bigint,bigint,timestamp with time zone,text)') AND pg_get_userbyid(p.proowner)=left(current_user,length(current_user)-5)||'owner' AND p.prosecdef=false AND p.proconfig IS NULL AND md5(p.prosrc)='159e51eac39d08f2dc99a046212422c9' AND (SELECT count(*)=2 AND bool_and(pg_get_userbyid(a.grantor)=left(current_user,length(current_user)-5)||'owner' AND NOT a.is_grantable AND a.privilege_type='EXECUTE' AND pg_get_userbyid(a.grantee)=ANY(ARRAY[left(current_user,length(current_user)-5)||'gateway',left(current_user,length(current_user)-5)||'owner'])) FROM aclexplode(p.proacl) a)) AND EXISTS (SELECT 1 FROM pg_proc p WHERE p.oid=to_regprocedure('assistant.reserve_assistant_runtime_model_call(uuid,uuid,character,bigint,bigint,bigint)') AND pg_get_userbyid(p.proowner)=left(current_user,length(current_user)-5)||'owner' AND p.prosecdef=false AND p.proconfig IS NULL AND md5(p.prosrc)='e9011ffbb32bb9fe4a5e62c18d39154f' AND (SELECT count(*)=2 AND bool_and(pg_get_userbyid(a.grantor)=left(current_user,length(current_user)-5)||'owner' AND NOT a.is_grantable AND a.privilege_type='EXECUTE' AND pg_get_userbyid(a.grantee)=ANY(ARRAY[left(current_user,length(current_user)-5)||'gateway',left(current_user,length(current_user)-5)||'owner'])) FROM aclexplode(p.proacl) a)) AND NOT has_table_privilege(left(current_user,length(current_user)-5)||'gateway','assistant.assistant_runtime_snapshots','INSERT') AND NOT has_table_privilege(left(current_user,length(current_user)-5)||'gateway','assistant.assistant_runs','INSERT') AND NOT has_table_privilege(left(current_user,length(current_user)-5)||'gateway','assistant.assistant_runtime_model_leases','INSERT') AND NOT has_table_privilege(left(current_user,length(current_user)-5)||'gateway','assistant.assistant_runtime_model_calls','INSERT')",
    "SELECT to_regclass('knowledge.document_versions') IS NOT NULL AND NOT has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_worker','knowledge.document_versions','UPDATE') AND NOT has_column_privilege(left(current_user,length(current_user)-5)||'knowledge_worker','knowledge.document_versions','change_type','UPDATE') AND NOT has_column_privilege(left(current_user,length(current_user)-5)||'knowledge_worker','knowledge.document_versions','metadata','UPDATE') AND NOT has_column_privilege(left(current_user,length(current_user)-5)||'knowledge_worker','knowledge.document_versions','content','UPDATE') AND NOT has_column_privilege(left(current_user,length(current_user)-5)||'knowledge_worker','knowledge.document_versions','content_hash','UPDATE') AND NOT has_column_privilege(left(current_user,length(current_user)-5)||'knowledge_worker','knowledge.document_versions','document_id','UPDATE') AND NOT has_column_privilege(left(current_user,length(current_user)-5)||'knowledge_worker','knowledge.document_versions','version_number','UPDATE') AND NOT has_column_privilege(left(current_user,length(current_user)-5)||'knowledge_worker','knowledge.document_versions','title','UPDATE')",
];
const POSTCONDITIONS: [&str; 8] = [
    "SELECT to_regclass('gateway.local_node_device_credentials') IS NOT NULL AND (SELECT count(*)=11 AND bool_and(column_name=ANY(ARRAY['credential_id','credential_sha256','tenant_id','user_id','device_id','channel_id','expires_at','accepted_through_sequence','last_seen_at','revoked_at','created_at'])) FROM information_schema.columns WHERE table_schema='gateway' AND table_name='local_node_device_credentials') AND NOT EXISTS (SELECT 1 FROM information_schema.columns WHERE table_schema='gateway' AND table_name='local_node_device_credentials' AND column_name IN ('credential','token','secret','plaintext'))",
    "SELECT to_regclass('gateway.local_node_device_credentials_active_channel') IS NOT NULL AND to_regclass('gateway.local_node_device_credentials_owner_device') IS NOT NULL AND (SELECT count(*)=6 FROM pg_constraint WHERE conrelid='gateway.local_node_device_credentials'::regclass AND conname=ANY(ARRAY['local_node_device_credentials_pkey','local_node_device_credentials_credential_sha256_key','local_node_device_credentials_credential_sha256_check','local_node_device_credentials_sequence_check','local_node_device_credentials_device_scope_fkey','local_node_device_credentials_channel_id_fkey'])) AND EXISTS (SELECT 1 FROM information_schema.columns WHERE table_schema='gateway' AND table_name='local_node_executions' AND column_name='cancel_requested_at' AND data_type='timestamp with time zone' AND is_nullable='YES')",
    "SELECT has_table_privilege(left(current_user,length(current_user)-5)||'gateway','gateway.local_node_device_credentials','SELECT') AND has_table_privilege(left(current_user,length(current_user)-5)||'gateway','gateway.local_node_device_credentials','INSERT') AND has_table_privilege(left(current_user,length(current_user)-5)||'gateway','gateway.local_node_device_credentials','UPDATE') AND NOT has_table_privilege(left(current_user,length(current_user)-5)||'gateway','gateway.local_node_device_credentials','DELETE') AND NOT has_table_privilege(left(current_user,length(current_user)-5)||'gateway','gateway.local_node_device_credentials','TRUNCATE') AND NOT has_table_privilege(left(current_user,length(current_user)-5)||'gateway','gateway.local_node_device_credentials','REFERENCES') AND NOT has_table_privilege(left(current_user,length(current_user)-5)||'gateway','gateway.local_node_device_credentials','TRIGGER') AND (SELECT count(*)=3 AND bool_and(pg_get_userbyid(a.grantor)=current_user AND NOT a.is_grantable AND a.privilege_type=ANY(ARRAY['SELECT','INSERT','UPDATE'])) FROM pg_class c CROSS JOIN LATERAL aclexplode(c.relacl) a WHERE c.oid='gateway.local_node_device_credentials'::regclass AND pg_get_userbyid(a.grantee)=left(current_user,length(current_user)-5)||'gateway')",
    "SELECT has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_api','knowledge.dataset_collection_bindings','SELECT') AND has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_api','knowledge.dataset_collection_bindings','INSERT') AND has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_api','knowledge.dataset_collection_bindings','UPDATE') AND NOT has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_api','knowledge.dataset_collection_bindings','DELETE') AND has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_api','knowledge.embedding_migrations','SELECT') AND has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_api','knowledge.embedding_migrations','INSERT') AND has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_api','knowledge.embedding_migrations','UPDATE') AND NOT has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_api','knowledge.embedding_migrations','DELETE') AND has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_api','knowledge.embedding_migration_action_jobs','SELECT') AND has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_api','knowledge.embedding_migration_action_jobs','INSERT') AND has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_api','knowledge.embedding_migration_action_jobs','UPDATE') AND NOT has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_api','knowledge.embedding_migration_action_jobs','DELETE') AND has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_api','knowledge.embedding_migration_progress','SELECT') AND has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_api','knowledge.embedding_migration_progress','DELETE') AND NOT has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_api','knowledge.embedding_migration_progress','INSERT') AND NOT has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_api','knowledge.embedding_migration_progress','UPDATE')",
    "SELECT has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_worker','knowledge.embedding_migration_progress','SELECT') AND has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_worker','knowledge.embedding_migration_progress','DELETE') AND has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_worker','knowledge.embedding_migration_progress','INSERT') AND has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_worker','knowledge.embedding_migration_progress','UPDATE')",
    "SELECT has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_api','knowledge.kb_document_batch_operations','SELECT') AND has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_api','knowledge.kb_document_batch_operations','INSERT') AND has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_api','knowledge.kb_document_batch_operations','UPDATE') AND NOT has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_api','knowledge.kb_document_batch_operations','DELETE') AND has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_api','knowledge.kb_document_batch_items','SELECT') AND has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_api','knowledge.kb_document_batch_items','INSERT') AND has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_api','knowledge.kb_document_batch_items','UPDATE') AND NOT has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_api','knowledge.kb_document_batch_items','DELETE') AND has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_worker','knowledge.kb_document_batch_operations','SELECT') AND has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_worker','knowledge.kb_document_batch_operations','UPDATE') AND NOT has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_worker','knowledge.kb_document_batch_operations','INSERT') AND NOT has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_worker','knowledge.kb_document_batch_operations','DELETE') AND has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_worker','knowledge.kb_document_batch_items','SELECT') AND has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_worker','knowledge.kb_document_batch_items','UPDATE') AND NOT has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_worker','knowledge.kb_document_batch_items','INSERT') AND NOT has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_worker','knowledge.kb_document_batch_items','DELETE')",
    "SELECT EXISTS (SELECT 1 FROM pg_proc p WHERE p.oid=to_regprocedure('assistant.issue_assistant_runtime_turn(uuid,uuid,uuid,uuid,character varying,character varying,character varying,character varying,character varying,jsonb,character,bigint,character varying,character varying,character varying,character varying,character varying,character,integer,bigint,bigint,bigint,timestamp with time zone,text)') AND pg_get_userbyid(p.proowner)=left(current_user,length(current_user)-5)||'owner' AND p.prosecdef=true AND p.proconfig=ARRAY['search_path=pg_catalog, assistant, pg_temp']::text[] AND md5(p.prosrc)='159e51eac39d08f2dc99a046212422c9' AND (SELECT count(*)=2 AND bool_and(pg_get_userbyid(a.grantor)=left(current_user,length(current_user)-5)||'owner' AND NOT a.is_grantable AND a.privilege_type='EXECUTE' AND pg_get_userbyid(a.grantee)=ANY(ARRAY[left(current_user,length(current_user)-5)||'gateway',left(current_user,length(current_user)-5)||'owner'])) FROM aclexplode(p.proacl) a)) AND EXISTS (SELECT 1 FROM pg_proc p WHERE p.oid=to_regprocedure('assistant.reserve_assistant_runtime_model_call(uuid,uuid,character,bigint,bigint,bigint)') AND pg_get_userbyid(p.proowner)=left(current_user,length(current_user)-5)||'owner' AND p.prosecdef=true AND p.proconfig=ARRAY['search_path=pg_catalog, assistant, pg_temp']::text[] AND md5(p.prosrc)='e9011ffbb32bb9fe4a5e62c18d39154f' AND (SELECT count(*)=2 AND bool_and(pg_get_userbyid(a.grantor)=left(current_user,length(current_user)-5)||'owner' AND NOT a.is_grantable AND a.privilege_type='EXECUTE' AND pg_get_userbyid(a.grantee)=ANY(ARRAY[left(current_user,length(current_user)-5)||'gateway',left(current_user,length(current_user)-5)||'owner'])) FROM aclexplode(p.proacl) a)) AND NOT has_table_privilege(left(current_user,length(current_user)-5)||'gateway','assistant.assistant_runtime_snapshots','INSERT') AND NOT has_table_privilege(left(current_user,length(current_user)-5)||'gateway','assistant.assistant_runs','INSERT') AND NOT has_table_privilege(left(current_user,length(current_user)-5)||'gateway','assistant.assistant_runtime_model_leases','INSERT') AND NOT has_table_privilege(left(current_user,length(current_user)-5)||'gateway','assistant.assistant_runtime_model_calls','INSERT')",
    "SELECT NOT has_table_privilege(left(current_user,length(current_user)-5)||'knowledge_worker','knowledge.document_versions','UPDATE') AND has_column_privilege(left(current_user,length(current_user)-5)||'knowledge_worker','knowledge.document_versions','change_type','UPDATE') AND has_column_privilege(left(current_user,length(current_user)-5)||'knowledge_worker','knowledge.document_versions','metadata','UPDATE') AND NOT has_column_privilege(left(current_user,length(current_user)-5)||'knowledge_worker','knowledge.document_versions','content','UPDATE') AND NOT has_column_privilege(left(current_user,length(current_user)-5)||'knowledge_worker','knowledge.document_versions','content_hash','UPDATE') AND NOT has_column_privilege(left(current_user,length(current_user)-5)||'knowledge_worker','knowledge.document_versions','document_id','UPDATE') AND NOT has_column_privilege(left(current_user,length(current_user)-5)||'knowledge_worker','knowledge.document_versions','version_number','UPDATE') AND NOT has_column_privilege(left(current_user,length(current_user)-5)||'knowledge_worker','knowledge.document_versions','title','UPDATE')",
];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    schema_version: String,
    status: String,
    baseline_id: String,
    predecessor_epoch: i32,
    predecessor_manifest_sha256: String,
    approved_predecessor_manifest_archive: String,
    approved_predecessor_manifest_archive_sha256: String,
    approved_predecessor_package_lock: String,
    approved_predecessor_package_lock_sha256: String,
    approved_predecessor_baseline_manifest_sha256: String,
    approved_predecessor_measurement_receipt_sha256: String,
    approved_predecessor_activation_receipt_sha256: String,
    approved_predecessor_sql_sha256: String,
    approved_predecessor_verification_sha256: String,
    candidate_epoch: i32,
    sequence_allocation: String,
    database_actions_authorized: bool,
    fingerprint_status: String,
    successor: PlanSuccessor,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlanSuccessor {
    proposal: String,
    change: manifest::ChangeSpec,
}

pub type Candidate = next_successor_022::Candidate;

fn refused(message: impl Into<String>) -> AuthorityError {
    AuthorityError::Refused(message.into())
}

fn lock_files(lock: &Value) -> Result<&Map<String, Value>> {
    if lock["schema"] != "migration-authority/source-distribution/v1"
        || lock["baseline_id"] != BASELINE
    {
        return Err(refused("approved 022 lock identity is invalid"));
    }
    lock["files_sha256"]
        .as_object()
        .ok_or_else(|| refused("approved 022 lock has no file inventory"))
}

fn lock_digest<'a>(files: &'a Map<String, Value>, relative: &str) -> Option<&'a str> {
    files.get(relative).and_then(Value::as_str)
}

fn load_plan(root: &Path) -> Result<(Value, Plan, String)> {
    let path = next_successor::source_file(root, PLAN_RELATIVE)?;
    let plan_sha = manifest::file_sha256(&path)?;
    if plan_sha != REVIEWED_023_PLAN_SHA256 {
        return Err(refused("023 source plan differs from its reviewed digest"));
    }
    let bytes = fs::read(&path)?;
    let plan: Plan = serde_json::from_slice(&bytes)
        .map_err(|_| refused("invalid or unknown 023 successor plan shape"))?;
    if plan.schema_version != "ai-platform/canonical-next-successor-source-plan/v1"
        || plan.status != "inactive_source_candidate"
        || plan.baseline_id != BASELINE
        || plan.predecessor_epoch != PREDECESSOR_EPOCH
        || plan.candidate_epoch != CANDIDATE_EPOCH
        || plan.sequence_allocation != "single_forward_extension_requires_root_review"
        || plan.database_actions_authorized
        || plan.fingerprint_status != "not_measured"
    {
        return Err(refused(
            "023 plan identity or stage is not the reviewed candidate",
        ));
    }
    if plan.predecessor_manifest_sha256
        != "d723f09097401133748454355cad52e886df50c628d964b2fc55ca256685fc89"
        || plan.approved_predecessor_manifest_archive != MANIFEST_ARCHIVE_RELATIVE
        || plan.approved_predecessor_manifest_archive_sha256 != plan.predecessor_manifest_sha256
        || plan.approved_predecessor_package_lock != LOCK_ARCHIVE_RELATIVE
        || plan.approved_predecessor_package_lock_sha256 != APPROVED_022_LOCK_SHA256
    {
        return Err(refused(
            "023 plan differs from its exact archived-022 preimage pins",
        ));
    }
    if plan.approved_predecessor_baseline_manifest_sha256
        != "74ffe24788479e72840681d9522838a617d874cd3f170f70c647f9c0072eb2b2"
        || plan.approved_predecessor_measurement_receipt_sha256
            != "b202d269b13a43a2789cfb052ee1a9d9af71dba449b5d5841e6d33e000cd4c54"
        || plan.approved_predecessor_activation_receipt_sha256
            != "5ac38c281890ebbfea6539f37077d888ae04b54cb1da3fd89aef784250afd6fa"
        || plan.approved_predecessor_sql_sha256
            != "e74e55bc51d10c434e1334de7fa8f522a966dc2c8fac11807d942b6ef84d0a7c"
        || plan.approved_predecessor_verification_sha256
            != "c363e5b3f6cff0b5a8394fa1106a1729048d1254c14fda122eea262221ee2120"
        || plan.successor.proposal != PROPOSAL_RELATIVE
    {
        return Err(refused(
            "023 plan differs from its exact epoch-22 evidence pins",
        ));
    }
    let change = &plan.successor.change;
    let preconditions = change
        .preconditions
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let postconditions = change
        .postconditions
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    if change.sequence != CANDIDATE_EPOCH
        || change.name != "native_workflow_authority"
        || change.file != NEXT_SQL_FILE
        || change.owner != "owner"
        || change.transaction_mode != manifest::TransactionMode::Transactional
        || change.rollback_class != manifest::RollbackClass::ForwardFixOnly
        || preconditions.as_slice() != PRECONDITIONS.as_slice()
        || postconditions.as_slice() != POSTCONDITIONS.as_slice()
        || change.timeout_seconds != 30
        || change.lock_budget_seconds != 5
        || change.resume_handler.is_some()
        || change.repair_handler.is_some()
    {
        return Err(refused(
            "023 change contract is not the one reviewed extension",
        ));
    }
    let proposal = next_successor::source_file(root, PROPOSAL_RELATIVE)?;
    if manifest::file_sha256(&proposal)? != change.sha256 {
        return Err(refused("023 proposal differs from its reviewed SQL digest"));
    }
    reject_embedded_transactions(&fs::read_to_string(proposal)?, &change.name)?;
    let value =
        serde_json::from_slice(&bytes).map_err(|_| refused("invalid 023 successor plan JSON"))?;
    Ok((value, plan, plan_sha))
}

fn approved_lock(root: &Path, plan: &Plan) -> Result<(Value, String)> {
    let archived = next_successor::source_file(root, &plan.approved_predecessor_package_lock)?;
    let current = next_successor::source_file(root, ACTIVE_LOCK_RELATIVE)?;
    let archive_sha = manifest::file_sha256(&archived)?;
    let current_sha = manifest::file_sha256(&current)?;
    if archive_sha != APPROVED_022_LOCK_SHA256
        || current_sha != APPROVED_022_LOCK_SHA256
        || fs::read(&archived)? != fs::read(&current)?
    {
        return Err(refused(
            "active source differs from the exact approved epoch-22 lock",
        ));
    }
    let value: Value = serde_json::from_slice(&fs::read(&archived)?)
        .map_err(|_| refused("invalid approved epoch-22 package lock"))?;
    let files = lock_files(&value)?;
    if files.len() != 186 {
        return Err(refused(
            "approved epoch-22 inventory has an unexpected size",
        ));
    }
    Ok((value, archive_sha))
}

fn verified_candidate_source(root: &Path) -> Result<Candidate> {
    let root =
        next_successor::canonical_directory(root, "source root must be a nonsymlink directory")?;
    let (plan_value, plan, plan_sha256) = load_plan(&root)?;
    let (package_lock, package_lock_sha256) = approved_lock(&root, &plan)?;
    distribution::verify(&root.join("database"), BASELINE)?;
    let paths = crate::runner::AuthorityPaths {
        database_dir: root.join("database"),
    };
    let (baseline, baseline_sha256) = commands::load_baseline_with_mode(
        &paths,
        BASELINE,
        distribution::SourceMode::Distribution,
    )?;
    let (_, source_epoch) = next_successor_022::verify_activated_source(&root)?;
    let manifest_path = next_successor::source_file(&root, ACTIVE_MANIFEST_RELATIVE)?;
    let current_manifest_sha = manifest::file_sha256(&manifest_path)?;
    let files = lock_files(&package_lock)?;
    if source_epoch.epoch != PREDECESSOR_EPOCH
        || source_epoch.changes.len() != PREDECESSOR_EPOCH as usize
        || current_manifest_sha != plan.predecessor_manifest_sha256
        || baseline_sha256 != plan.approved_predecessor_baseline_manifest_sha256
        || lock_digest(files, &format!("migrations/{BASELINE}/manifest.yml"))
            != Some(current_manifest_sha.as_str())
        || lock_digest(files, &format!("baselines/{BASELINE}/manifest.json"))
            != Some(baseline_sha256.as_str())
        || lock_digest(
            files,
            &format!("migrations/{BASELINE}/SUCCESSOR_022_MEASUREMENT.json"),
        ) != Some(
            plan.approved_predecessor_measurement_receipt_sha256
                .as_str(),
        )
        || lock_digest(
            files,
            &format!("migrations/{BASELINE}/SUCCESSOR_022_ACTIVATION.json"),
        ) != Some(plan.approved_predecessor_activation_receipt_sha256.as_str())
        || lock_digest(
            files,
            &format!("migrations/{BASELINE}/022_assistant_runtime_thread_projection_insert.sql"),
        ) != Some(plan.approved_predecessor_sql_sha256.as_str())
        || lock_digest(
            files,
            &format!("migrations/{BASELINE}/022_verification.json"),
        ) != Some(plan.approved_predecessor_verification_sha256.as_str())
    {
        return Err(refused(
            "approved epoch-22 source preimage or receipts changed",
        ));
    }
    let archived_manifest =
        next_successor::source_file(&root, &plan.approved_predecessor_manifest_archive)?;
    if manifest::file_sha256(&archived_manifest)? != plan.predecessor_manifest_sha256
        || fs::read(&archived_manifest)? != fs::read(&manifest_path)?
    {
        return Err(refused("approved epoch-22 manifest archive changed"));
    }
    let epoch_dir = root.join("database").join(EPOCH_RELATIVE);
    for path in [
        epoch_dir.join(NEXT_SQL_FILE),
        epoch_dir.join(NEXT_REFERENCE_FILE),
        epoch_dir.join(NEXT_MEASUREMENT_FILE),
        epoch_dir.join(NEXT_ACTIVATION_FILE),
    ] {
        if fs::symlink_metadata(path).is_ok() {
            return Err(refused("023 already entered the active migration source"));
        }
    }
    let proposal = next_successor::source_file(&root, PROPOSAL_RELATIVE)?;
    let proposal_sha256 = manifest::file_sha256(&proposal)?;
    let mut manifest_value: Value = serde_json::from_slice(&fs::read(&manifest_path)?)
        .map_err(|_| refused("active epoch-22 manifest must be JSON-compatible"))?;
    manifest_value["epoch"] = json!(CANDIDATE_EPOCH);
    manifest_value["changes"]
        .as_array_mut()
        .ok_or_else(|| refused("active epoch-22 manifest has no change list"))?
        .push(
            serde_json::to_value(&plan.successor.change)
                .map_err(|_| refused("023 change serialization failed"))?,
        );
    let mut epoch = source_epoch;
    epoch.epoch = CANDIDATE_EPOCH;
    epoch.changes.push(plan.successor.change.clone());
    Ok(Candidate {
        plan: plan_value,
        manifest_value,
        epoch,
        baseline,
        baseline_sha256,
        package_lock,
        package_lock_sha256,
        plan_sha256,
        proposal_sha256,
        predecessor_measurement_sha256: plan.approved_predecessor_measurement_receipt_sha256,
        predecessor_activation_sha256: plan.approved_predecessor_activation_receipt_sha256,
    })
}

pub fn validate_source(root: &Path) -> Result<Candidate> {
    verified_candidate_source(root)
}

fn expected_input_paths(lock: &Value, measured: bool, activated: bool) -> Result<BTreeSet<String>> {
    let mut expected: BTreeSet<String> = lock_files(lock)?.keys().cloned().collect();
    let epoch = format!("migrations/{BASELINE}/");
    expected.insert(format!("{epoch}{NEXT_SQL_FILE}"));
    if measured || activated {
        expected.insert(format!("{epoch}{NEXT_REFERENCE_FILE}"));
        expected.insert(format!("{epoch}{NEXT_MEASUREMENT_FILE}"));
    }
    if activated {
        expected.insert(format!("{epoch}{NEXT_ACTIVATION_FILE}"));
    }
    Ok(expected)
}

fn verify_preview_root(preview_root: &Path) -> Result<()> {
    let mut entries = BTreeSet::new();
    for entry in fs::read_dir(preview_root)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_symlink() || !(kind.is_file() || kind.is_dir()) {
            return Err(refused("023 preview contains an unsupported entry"));
        }
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| refused("023 preview contains an invalid name"))?;
        if !entries.insert(name.clone())
            || (name == "database") != kind.is_dir()
            || (name != "database" && name != PREVIEW_RECEIPT_FILE)
        {
            return Err(refused(
                "023 preview contains missing or unexpected entries",
            ));
        }
    }
    if entries != BTreeSet::from(["database".to_owned(), PREVIEW_RECEIPT_FILE.to_owned()]) {
        return Err(refused(
            "023 preview contains missing or unexpected entries",
        ));
    }
    Ok(())
}

fn require_preview_inventory(
    source_root: &Path,
    preview_root: &Path,
    candidate: &Candidate,
    measured: bool,
) -> Result<()> {
    if next_successor::collect_distribution_inputs(&preview_root.join("database"))?
        != expected_input_paths(&candidate.package_lock, measured, false)?
    {
        return Err(refused(
            "023 preview contains missing or unexpected authority inputs",
        ));
    }
    let files = lock_files(&candidate.package_lock)?;
    let manifest_key = format!("migrations/{BASELINE}/manifest.yml");
    for (relative, expected) in files {
        if relative == &manifest_key {
            continue;
        }
        let path = next_successor::source_file(preview_root, &format!("database/{relative}"))?;
        if manifest::file_sha256(&path)? != expected.as_str().unwrap_or_default() {
            return Err(refused("023 preview changed an approved 022 preimage"));
        }
    }
    let copied_lock = next_successor::source_file(preview_root, ACTIVE_LOCK_RELATIVE)?;
    if manifest::file_sha256(&copied_lock)? != candidate.package_lock_sha256 {
        return Err(refused(
            "023 preview package lock differs from approved 022",
        ));
    }
    let source_proposal = next_successor::source_file(source_root, PROPOSAL_RELATIVE)?;
    let preview_sql = next_successor::source_file(
        preview_root,
        &format!("database/{EPOCH_RELATIVE}/{NEXT_SQL_FILE}"),
    )?;
    if manifest::file_sha256(&source_proposal)? != candidate.proposal_sha256
        || manifest::file_sha256(&preview_sql)? != candidate.proposal_sha256
        || fs::read(&source_proposal)? != fs::read(&preview_sql)?
    {
        return Err(refused(
            "023 preview SQL differs from the reviewed proposal",
        ));
    }
    Ok(())
}

fn verify_marker(preview_root: &Path, candidate: &Candidate) -> Result<Value> {
    let marker: Value = serde_json::from_slice(&fs::read(preview_root.join(PREVIEW_RECEIPT_FILE))?)
        .map_err(|_| refused("invalid 023 preview receipt"))?;
    if !next_successor::exact_keys(
        &marker,
        &[
            "schema",
            "status",
            "database_actions",
            "source_activation",
            "predecessor_epoch",
            "candidate_epoch",
            "source_plan_sha256",
            "approved_predecessor_package_lock_sha256",
            "predecessor_manifest_sha256",
            "baseline_manifest_sha256",
            "proposal_sha256",
            "predecessor_measurement_receipt_sha256",
            "predecessor_activation_receipt_sha256",
            "candidate_manifest_sha256",
            "candidate_sql_sha256",
        ],
    ) || marker["schema"] != "ai-platform/canonical-next-successor-preview/v1"
        || marker["status"] != "inactive_external_preview_only"
        || marker["database_actions"] != false
        || marker["source_activation"] != false
        || marker["predecessor_epoch"] != PREDECESSOR_EPOCH
        || marker["candidate_epoch"] != CANDIDATE_EPOCH
        || marker["source_plan_sha256"] != candidate.plan_sha256
        || marker["approved_predecessor_package_lock_sha256"] != candidate.package_lock_sha256
        || marker["predecessor_manifest_sha256"] != candidate.plan["predecessor_manifest_sha256"]
        || marker["baseline_manifest_sha256"] != candidate.baseline_sha256
        || marker["proposal_sha256"] != candidate.proposal_sha256
        || marker["predecessor_measurement_receipt_sha256"]
            != candidate.predecessor_measurement_sha256
        || marker["predecessor_activation_receipt_sha256"]
            != candidate.predecessor_activation_sha256
        || marker["candidate_manifest_sha256"]
            != manifest::file_sha256(&preview_root.join(ACTIVE_MANIFEST_RELATIVE))?
        || marker["candidate_sql_sha256"]
            != manifest::file_sha256(
                &preview_root
                    .join("database")
                    .join(EPOCH_RELATIVE)
                    .join(NEXT_SQL_FILE),
            )?
    {
        return Err(refused(
            "023 preview receipt does not bind the exact source candidate",
        ));
    }
    Ok(marker)
}

fn verify_candidate_epoch(preview_root: &Path, candidate: &Candidate) -> Result<()> {
    let manifest_path = preview_root.join(ACTIVE_MANIFEST_RELATIVE);
    let epoch = manifest::load_epoch_manifest(&manifest_path)?;
    let actual: Value = serde_json::from_slice(&fs::read(&manifest_path)?)
        .map_err(|_| refused("invalid 023 preview manifest"))?;
    if actual != candidate.manifest_value
        || epoch.epoch != CANDIDATE_EPOCH
        || epoch.baseline_id != BASELINE
        || epoch.changes.len() != CANDIDATE_EPOCH as usize
        || serde_json::to_value(&epoch.changes[..PREDECESSOR_EPOCH as usize])
            .map_err(|_| refused("cannot bind epoch-22 prefix"))?
            != serde_json::to_value(&candidate.epoch.changes[..PREDECESSOR_EPOCH as usize])
                .map_err(|_| refused("cannot bind 023 predecessor"))?
        || manifest::file_sha256(
            &preview_root
                .join("database")
                .join(EPOCH_RELATIVE)
                .join(NEXT_SQL_FILE),
        )? != candidate.proposal_sha256
    {
        return Err(refused(
            "023 candidate is not the exact one-change extension of epoch 22",
        ));
    }
    Ok(())
}

fn preview_is_external(source_root: &Path, preview_root: &Path) -> Result<()> {
    if source_root.starts_with(preview_root) || preview_root.starts_with(source_root) {
        return Err(refused(
            "023 preview must be external to the active source tree",
        ));
    }
    Ok(())
}

pub fn materialize(root: &Path, output: &Path) -> Result<Value> {
    let root =
        next_successor::canonical_directory(root, "source root must be a nonsymlink directory")?;
    let candidate = verified_candidate_source(&root)?;
    let absolute = std::path::absolute(output)?;
    if absolute.exists() {
        return Err(refused("023 preview output must not already exist"));
    }
    let parent = absolute
        .parent()
        .ok_or_else(|| refused("invalid 023 preview destination"))?
        .canonicalize()?;
    let resolved = parent.join(
        absolute
            .file_name()
            .ok_or_else(|| refused("invalid preview destination"))?,
    );
    if resolved != absolute || resolved.starts_with(&root) || root.starts_with(&resolved) {
        return Err(refused(
            "023 preview must be external to the active source tree",
        ));
    }
    fs::create_dir(&absolute)?;
    next_successor::copy_tree(&root.join("database"), &absolute.join("database"))?;
    let epoch_dir = absolute.join("database").join(EPOCH_RELATIVE);
    let mut sql = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(epoch_dir.join(NEXT_SQL_FILE))?;
    sql.write_all(&fs::read(next_successor::source_file(
        &root,
        PROPOSAL_RELATIVE,
    )?)?)?;
    sql.sync_all()?;
    let manifest_path = epoch_dir.join("manifest.yml");
    fs::write(
        &manifest_path,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&candidate.manifest_value)
                .map_err(|_| refused("023 candidate manifest serialization failed"))?
        ),
    )?;
    let marker = json!({
        "schema":"ai-platform/canonical-next-successor-preview/v1",
        "status":"inactive_external_preview_only",
        "database_actions":false,
        "source_activation":false,
        "predecessor_epoch":PREDECESSOR_EPOCH,
        "candidate_epoch":CANDIDATE_EPOCH,
        "source_plan_sha256":candidate.plan_sha256,
        "approved_predecessor_package_lock_sha256":candidate.package_lock_sha256,
        "predecessor_manifest_sha256":candidate.plan["predecessor_manifest_sha256"],
        "baseline_manifest_sha256":candidate.baseline_sha256,
        "proposal_sha256":candidate.proposal_sha256,
        "predecessor_measurement_receipt_sha256":candidate.predecessor_measurement_sha256,
        "predecessor_activation_receipt_sha256":candidate.predecessor_activation_sha256,
        "candidate_manifest_sha256":manifest::file_sha256(&manifest_path)?,
        "candidate_sql_sha256":candidate.proposal_sha256
    });
    next_successor::write_new_json(&absolute.join(PREVIEW_RECEIPT_FILE), &marker)?;
    verify_preview(&root, &absolute)?;
    Ok(marker)
}

pub fn verify_preview(source_root: &Path, preview_root: &Path) -> Result<(Candidate, Value)> {
    let source_root = next_successor::canonical_directory(
        source_root,
        "source root must be a nonsymlink directory",
    )?;
    let preview_root = next_successor::canonical_directory(
        preview_root,
        "023 preview must be a nonsymlink directory",
    )?;
    preview_is_external(&source_root, &preview_root)?;
    verify_preview_root(&preview_root)?;
    let candidate = verified_candidate_source(&source_root)?;
    require_preview_inventory(&source_root, &preview_root, &candidate, false)?;
    verify_candidate_epoch(&preview_root, &candidate)?;
    let marker = verify_marker(&preview_root, &candidate)?;
    for path in [
        preview_root
            .join("database")
            .join(EPOCH_RELATIVE)
            .join(NEXT_REFERENCE_FILE),
        preview_root
            .join("database")
            .join(EPOCH_RELATIVE)
            .join(NEXT_MEASUREMENT_FILE),
    ] {
        match fs::symlink_metadata(path) {
            Ok(_) => return Err(refused("023 measurement inputs already exist")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok((candidate, marker))
}

pub(crate) fn verification_reference(
    candidate: &Candidate,
    evidence: &measure_successors::SingleStepEvidence,
) -> Value {
    let mut fingerprints = evidence.raw_fingerprints.clone();
    fingerprints.insert("acl".into(), evidence.base_acl_sha256.clone());
    let changes = evidence
        .applied
        .iter()
        .map(|(sequence, checksum)| (sequence.to_string(), json!(checksum)))
        .collect::<Map<_, _>>();
    json!({
        "baseline_id":candidate.baseline.baseline_id,
        "sequence":CANDIDATE_EPOCH,
        "baseline_manifest_sha256":candidate.baseline_sha256,
        "changes":changes,
        "fingerprints":fingerprints,
        "raw_fingerprints":evidence.raw_fingerprints,
        "acl_compatibility":{
            "revision":crate::ledger_acl::REVISION,
            "manifest_sha256":crate::ledger_acl::MANIFEST_SHA256,
            "delta_line":ACL_DELTA,
            "base_acl_sha256":evidence.base_acl_sha256,
            "effective_acl_sha256":evidence.raw_acl_sha256,
            "removed_delta_count":1,
            "raw_acl_lines":evidence.raw_acl_lines,
            "base_acl_lines":evidence.base_acl_lines
        },
        "provenance":"Explicit PostgreSQL 16 measurement of only the 022 -> 023 extension against the exact approved epoch-22 prefix.",
        "provenance_details":{
            "tool":"rust/platform-native/crates/platform-migration-authority/src/source_tools/measure_next_successor_023.rs",
            "predecessor_epoch":PREDECESSOR_EPOCH,
            "approved_predecessor_package_lock_sha256":candidate.package_lock_sha256,
            "predecessor_manifest_sha256":candidate.plan["predecessor_manifest_sha256"],
            "source_plan_sha256":candidate.plan_sha256,
            "candidate_sql_sha256":candidate.proposal_sha256,
            "predecessor_measurement_receipt_sha256":candidate.predecessor_measurement_sha256,
            "predecessor_activation_receipt_sha256":candidate.predecessor_activation_sha256,
            "git_baseline_provenance_verified":false
        }
    })
}

fn is_sha256(value: &Value) -> bool {
    value.as_str().is_some_and(|hash| {
        hash.len() == 64
            && hash
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn verify_reference(
    candidate: &Candidate,
    source_root: &Path,
    preview_root: &Path,
) -> Result<String> {
    let path = preview_root
        .join("database")
        .join(EPOCH_RELATIVE)
        .join(NEXT_REFERENCE_FILE);
    let reference: Value = serde_json::from_slice(&fs::read(&path)?)
        .map_err(|_| refused("invalid 023 verification reference"))?;
    let changes = candidate
        .epoch
        .changes
        .iter()
        .map(|change| (change.sequence.to_string(), json!(change.sha256)))
        .collect::<Map<_, _>>();
    let fingerprints = reference["fingerprints"]
        .as_object()
        .ok_or_else(|| refused("023 reference lacks canonical fingerprints"))?;
    let raw = reference["raw_fingerprints"]
        .as_object()
        .ok_or_else(|| refused("023 reference lacks raw fingerprints"))?;
    let expected_keys: BTreeSet<&str> = ["structural", "acl", "extensions", "reference_data"]
        .into_iter()
        .collect();
    if !next_successor::exact_keys(
        &reference,
        &[
            "baseline_id",
            "sequence",
            "baseline_manifest_sha256",
            "changes",
            "fingerprints",
            "raw_fingerprints",
            "acl_compatibility",
            "provenance",
            "provenance_details",
        ],
    ) || !next_successor::exact_keys(
        &reference["acl_compatibility"],
        &[
            "revision",
            "manifest_sha256",
            "delta_line",
            "base_acl_sha256",
            "effective_acl_sha256",
            "removed_delta_count",
            "raw_acl_lines",
            "base_acl_lines",
        ],
    ) || !next_successor::exact_keys(
        &reference["provenance_details"],
        &[
            "tool",
            "predecessor_epoch",
            "approved_predecessor_package_lock_sha256",
            "predecessor_manifest_sha256",
            "source_plan_sha256",
            "candidate_sql_sha256",
            "predecessor_measurement_receipt_sha256",
            "predecessor_activation_receipt_sha256",
            "git_baseline_provenance_verified",
        ],
    ) || reference["baseline_id"] != BASELINE
        || reference["sequence"] != CANDIDATE_EPOCH
        || reference["baseline_manifest_sha256"] != candidate.baseline_sha256
        || reference["changes"] != Value::Object(changes)
        || fingerprints
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>()
            != expected_keys
        || raw.keys().map(String::as_str).collect::<BTreeSet<_>>() != expected_keys
        || fingerprints
            .values()
            .chain(raw.values())
            .any(|value| !is_sha256(value))
        || fingerprints["structural"] != raw["structural"]
        || fingerprints["extensions"] != raw["extensions"]
        || fingerprints["reference_data"] != raw["reference_data"]
        || reference["provenance"]
            != "Explicit PostgreSQL 16 measurement of only the 022 -> 023 extension against the exact approved epoch-22 prefix."
        || reference["provenance_details"]["tool"]
            != "rust/platform-native/crates/platform-migration-authority/src/source_tools/measure_next_successor_023.rs"
        || reference["provenance_details"]["predecessor_epoch"] != PREDECESSOR_EPOCH
        || reference["provenance_details"]["approved_predecessor_package_lock_sha256"]
            != candidate.package_lock_sha256
        || reference["provenance_details"]["predecessor_manifest_sha256"]
            != candidate.plan["predecessor_manifest_sha256"]
        || reference["provenance_details"]["source_plan_sha256"] != candidate.plan_sha256
        || reference["provenance_details"]["candidate_sql_sha256"] != candidate.proposal_sha256
        || reference["provenance_details"]["predecessor_measurement_receipt_sha256"]
            != candidate.predecessor_measurement_sha256
        || reference["provenance_details"]["predecessor_activation_receipt_sha256"]
            != candidate.predecessor_activation_sha256
        || reference["provenance_details"]["git_baseline_provenance_verified"] != false
    {
        return Err(refused(
            "023 reference does not bind the complete 1-23 prefix",
        ));
    }
    let raw_values: BTreeMap<String, String> =
        serde_json::from_value(reference["raw_fingerprints"].clone())
            .map_err(|_| refused("invalid 023 raw fingerprints"))?;
    let acl_lines: Vec<String> =
        serde_json::from_value(reference["acl_compatibility"]["raw_acl_lines"].clone())
            .map_err(|_| refused("invalid 023 raw ACL lines"))?;
    let (base_acl, raw_acl, base_lines) =
        measure_successors::acl_reference(&raw_values, &acl_lines)?;
    if reference["acl_compatibility"]["revision"] != crate::ledger_acl::REVISION
        || reference["acl_compatibility"]["manifest_sha256"] != crate::ledger_acl::MANIFEST_SHA256
        || reference["acl_compatibility"]["delta_line"] != ACL_DELTA
        || reference["acl_compatibility"]["removed_delta_count"] != 1
        || reference["acl_compatibility"]["base_acl_sha256"] != base_acl
        || reference["acl_compatibility"]["effective_acl_sha256"] != raw_acl
        || fingerprints["acl"] != base_acl
        || reference["acl_compatibility"]["base_acl_lines"] != json!(base_lines)
    {
        return Err(refused(
            "023 ACL compatibility does not preserve ledger ACL v1",
        ));
    }
    let previous_path = next_successor::source_file(
        source_root,
        &format!("database/{EPOCH_RELATIVE}/022_verification.json"),
    )?;
    if manifest::file_sha256(&previous_path)?
        != candidate.plan["approved_predecessor_verification_sha256"]
    {
        return Err(refused("approved 022 verification reference changed"));
    }
    let previous: Value = serde_json::from_slice(&fs::read(previous_path)?)
        .map_err(|_| refused("invalid approved 022 verification reference"))?;
    for key in ["extensions", "reference_data"] {
        if raw[key] != previous["raw_fingerprints"][key]
            || fingerprints[key] != previous["fingerprints"][key]
        {
            return Err(refused("023 changed an unrelated canonical fingerprint"));
        }
    }
    if raw["structural"] == previous["raw_fingerprints"]["structural"]
        || fingerprints["acl"] == previous["fingerprints"]["acl"]
    {
        return Err(refused(
            "023 measurement does not contain the reviewed structural and ACL changes",
        ));
    }
    let previous_lines: Vec<String> =
        serde_json::from_value(previous["acl_compatibility"]["raw_acl_lines"].clone())
            .map_err(|_| refused("approved 022 raw ACL lines are malformed"))?;
    let old: BTreeSet<&str> = previous_lines.iter().map(String::as_str).collect();
    let new: BTreeSet<&str> = acl_lines.iter().map(String::as_str).collect();
    let added = new.difference(&old).copied().collect::<BTreeSet<_>>();
    let removed = old.difference(&new).copied().collect::<BTreeSet<_>>();
    if old.len() != previous_lines.len()
        || new.len() != acl_lines.len()
        || removed
            != BTreeSet::from([
                "acl:knowledge.embedding_migration_progress:knowledge_worker:DELETE,INSERT,UPDATE:grantopt=0",
            ])
        || added
            != BTreeSet::from([
                "owner:gateway.local_node_device_credentials:owner",
                "acl:gateway.local_node_device_credentials:owner:DELETE,INSERT,REFERENCES,SELECT,TRIGGER,TRUNCATE,UPDATE:grantopt=0",
                "acl:gateway.local_node_device_credentials:gateway:INSERT,SELECT,UPDATE:grantopt=0",
                "acl:knowledge.dataset_collection_bindings:knowledge_api:INSERT,SELECT,UPDATE:grantopt=0",
                "acl:knowledge.embedding_migrations:knowledge_api:INSERT,SELECT,UPDATE:grantopt=0",
                "acl:knowledge.embedding_migration_action_jobs:knowledge_api:INSERT,SELECT,UPDATE:grantopt=0",
                "acl:knowledge.embedding_migration_progress:knowledge_api:DELETE,SELECT:grantopt=0",
                "acl:knowledge.embedding_migration_progress:knowledge_worker:DELETE,INSERT,SELECT,UPDATE:grantopt=0",
                "acl:knowledge.kb_document_batch_operations:knowledge_worker:SELECT,UPDATE:grantopt=0",
                "acl:knowledge.kb_document_batch_items:knowledge_worker:SELECT,UPDATE:grantopt=0",
                "column_acl:knowledge.document_versions.change_type:knowledge_worker:UPDATE:grantopt=0",
                "column_acl:knowledge.document_versions.metadata:knowledge_worker:UPDATE:grantopt=0",
            ])
    {
        return Err(refused(
            "023 ACL measurement differs from the exact native-workflow grant set",
        ));
    }
    Ok(manifest::file_sha256(&path)?)
}

fn verify_measurement_receipt(
    candidate: &Candidate,
    marker: &Value,
    preview_root: &Path,
) -> Result<Value> {
    let path = preview_root
        .join("database")
        .join(EPOCH_RELATIVE)
        .join(NEXT_MEASUREMENT_FILE);
    let receipt: Value = serde_json::from_slice(&fs::read(path)?)
        .map_err(|_| refused("invalid 023 measurement receipt"))?;
    let reference_sha = manifest::file_sha256(
        &preview_root
            .join("database")
            .join(EPOCH_RELATIVE)
            .join(NEXT_REFERENCE_FILE),
    )?;
    let database = receipt["expected_database"].as_str().unwrap_or_default();
    if !next_successor::exact_keys(
        &receipt,
        &[
            "schema",
            "status",
            "database_actions",
            "database_actions_scope",
            "source_activation",
            "expected_database",
            "predecessor_epoch",
            "candidate_epoch",
            "source_plan_sha256",
            "approved_predecessor_package_lock_sha256",
            "predecessor_manifest_sha256",
            "candidate_manifest_sha256",
            "baseline_manifest_sha256",
            "candidate_sql_sha256",
            "successor_sequences",
            "predecessor_measurement_receipt_sha256",
            "predecessor_activation_receipt_sha256",
            "verification_ref_sha256",
            "post_migration_noop",
        ],
    ) || receipt["schema"] != "ai-platform/canonical-next-successor-measurement/v1"
        || receipt["status"] != "measured_external_preview"
        || receipt["database_actions"] != true
        || receipt["database_actions_scope"] != "named_isolated_database_only"
        || receipt["source_activation"] != false
        || receipt["predecessor_epoch"] != PREDECESSOR_EPOCH
        || receipt["candidate_epoch"] != CANDIDATE_EPOCH
        || receipt["successor_sequences"] != json!([CANDIDATE_EPOCH])
        || next_successor::validate_database_name(database).is_err()
        || receipt["source_plan_sha256"] != candidate.plan_sha256
        || receipt["approved_predecessor_package_lock_sha256"] != candidate.package_lock_sha256
        || receipt["predecessor_manifest_sha256"] != candidate.plan["predecessor_manifest_sha256"]
        || receipt["candidate_manifest_sha256"] != marker["candidate_manifest_sha256"]
        || receipt["baseline_manifest_sha256"] != candidate.baseline_sha256
        || receipt["candidate_sql_sha256"] != candidate.proposal_sha256
        || receipt["predecessor_measurement_receipt_sha256"]
            != candidate.predecessor_measurement_sha256
        || receipt["predecessor_activation_receipt_sha256"]
            != candidate.predecessor_activation_sha256
        || receipt["verification_ref_sha256"] != reference_sha
        || receipt["post_migration_noop"] != true
    {
        return Err(refused(
            "023 measurement receipt does not bind the measured candidate",
        ));
    }
    Ok(receipt)
}

fn verify_preview_state(
    source_root: &Path,
    preview_root: &Path,
    measured: bool,
) -> Result<(Candidate, Value)> {
    let source_root = next_successor::canonical_directory(
        source_root,
        "source root must be a nonsymlink directory",
    )?;
    let preview_root = next_successor::canonical_directory(
        preview_root,
        "023 preview must be a nonsymlink directory",
    )?;
    preview_is_external(&source_root, &preview_root)?;
    verify_preview_root(&preview_root)?;
    let candidate = verified_candidate_source(&source_root)?;
    require_preview_inventory(&source_root, &preview_root, &candidate, measured)?;
    verify_candidate_epoch(&preview_root, &candidate)?;
    let marker = verify_marker(&preview_root, &candidate)?;
    if measured {
        verify_reference(&candidate, &source_root, &preview_root)?;
        verify_measurement_receipt(&candidate, &marker, &preview_root)?;
    } else {
        for path in [
            preview_root
                .join("database")
                .join(EPOCH_RELATIVE)
                .join(NEXT_REFERENCE_FILE),
            preview_root
                .join("database")
                .join(EPOCH_RELATIVE)
                .join(NEXT_MEASUREMENT_FILE),
        ] {
            match fs::symlink_metadata(path) {
                Ok(_) => return Err(refused("023 measurement inputs already exist")),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok((candidate, marker))
}

pub fn verify_measured_preview(
    source_root: &Path,
    preview_root: &Path,
) -> Result<(Candidate, Value)> {
    verify_preview_state(source_root, preview_root, true)
}

pub fn activation_bundle(source_root: &Path, preview_root: &Path, output: &Path) -> Result<Value> {
    let source_root = next_successor::canonical_directory(
        source_root,
        "source root must be a nonsymlink directory",
    )?;
    let preview_root = next_successor::canonical_directory(
        preview_root,
        "023 preview must be a nonsymlink directory",
    )?;
    let (candidate, marker) = verify_measured_preview(&source_root, &preview_root)?;
    let measurement = verify_measurement_receipt(&candidate, &marker, &preview_root)?;
    let absolute = std::path::absolute(output)?;
    if absolute.exists() {
        return Err(refused(
            "023 activation bundle output must not already exist",
        ));
    }
    let parent = absolute
        .parent()
        .ok_or_else(|| refused("invalid 023 activation bundle destination"))?
        .canonicalize()?;
    let resolved = parent.join(
        absolute
            .file_name()
            .ok_or_else(|| refused("invalid activation bundle destination"))?,
    );
    if resolved != absolute
        || absolute.starts_with(&source_root)
        || source_root.starts_with(&absolute)
        || absolute.starts_with(&preview_root)
        || preview_root.starts_with(&absolute)
    {
        return Err(refused(
            "023 activation bundle must be external to source and preview",
        ));
    }
    let epoch_dir = preview_root.join("database").join(EPOCH_RELATIVE);
    let manifest_sha = manifest::file_sha256(&epoch_dir.join("manifest.yml"))?;
    let reference_sha = manifest::file_sha256(&epoch_dir.join(NEXT_REFERENCE_FILE))?;
    let measurement_sha = manifest::file_sha256(&epoch_dir.join(NEXT_MEASUREMENT_FILE))?;
    let activation = json!({
        "schema":"ai-platform/canonical-next-successor-activation/v1",
        "status":"activated_source_chain",
        "source_activation":true,
        "predecessor_epoch":PREDECESSOR_EPOCH,
        "epoch":CANDIDATE_EPOCH,
        "baseline_id":BASELINE,
        "source_plan_sha256":candidate.plan_sha256,
        "approved_predecessor_package_lock_sha256":candidate.package_lock_sha256,
        "baseline_manifest_sha256":candidate.baseline_sha256,
        "predecessor_manifest_sha256":candidate.plan["predecessor_manifest_sha256"],
        "epoch_manifest_sha256":manifest_sha,
        "measurement_receipt_sha256":measurement_sha,
        "verification_ref_sha256":reference_sha,
        "predecessor_measurement_receipt_sha256":candidate.predecessor_measurement_sha256,
        "predecessor_activation_receipt_sha256":candidate.predecessor_activation_sha256
    });
    let activation_bytes = format!(
        "{}\n",
        serde_json::to_string_pretty(&activation)
            .map_err(|_| refused("023 activation receipt serialization failed"))?
    )
    .into_bytes();
    let activation_sha = next_successor::sha256_bytes(&activation_bytes);
    let mut package_lock = candidate.package_lock.clone();
    let files = package_lock["files_sha256"]
        .as_object_mut()
        .ok_or_else(|| refused("approved 022 lock has no file inventory"))?;
    files.insert(
        format!("migrations/{BASELINE}/manifest.yml"),
        json!(manifest_sha),
    );
    files.insert(
        format!("migrations/{BASELINE}/{NEXT_SQL_FILE}"),
        json!(candidate.proposal_sha256),
    );
    files.insert(
        format!("migrations/{BASELINE}/{NEXT_REFERENCE_FILE}"),
        json!(reference_sha),
    );
    files.insert(
        format!("migrations/{BASELINE}/{NEXT_MEASUREMENT_FILE}"),
        json!(measurement_sha),
    );
    files.insert(
        format!("migrations/{BASELINE}/{NEXT_ACTIVATION_FILE}"),
        json!(activation_sha),
    );
    let package_lock_bytes = format!(
        "{}\n",
        serde_json::to_string_pretty(&package_lock)
            .map_err(|_| refused("023 package lock serialization failed"))?
    )
    .into_bytes();
    let package_lock_sha = next_successor::sha256_bytes(&package_lock_bytes);
    let summary = json!({
        "schema":"ai-platform/canonical-next-successor-activation-bundle/v1",
        "status":"ready_for_owner_review",
        "database_actions":false,
        "source_activation":false,
        "predecessor_epoch":PREDECESSOR_EPOCH,
        "candidate_epoch":CANDIDATE_EPOCH,
        "source_plan_sha256":candidate.plan_sha256,
        "approved_predecessor_package_lock_sha256":candidate.package_lock_sha256,
        "candidate_manifest_sha256":manifest_sha,
        "candidate_sql_sha256":candidate.proposal_sha256,
        "measurement_receipt_sha256":measurement_sha,
        "verification_ref_sha256":reference_sha,
        "activation_receipt_sha256":activation_sha,
        "package_authority_lock_sha256":package_lock_sha,
        "compiled_distribution_lock_sha256_to_review":package_lock_sha,
        "expected_database":measurement["expected_database"],
        "preview_root":preview_root.to_string_lossy()
    });
    fs::create_dir(&absolute)?;
    next_successor::write_new_json(&absolute.join(NEXT_ACTIVATION_FILE), &activation)?;
    next_successor::write_new_bytes(
        &absolute.join(ACTIVE_LOCK_RELATIVE.rsplit('/').next().unwrap()),
        &package_lock_bytes,
    )?;
    next_successor::write_new_json(&absolute.join("ACTIVATION_023_REVIEW.json"), &summary)?;
    Ok(summary)
}

/// Verify the active epoch-23 chain against the exact archived epoch-22 inventory.
/// Earlier migration SQL and receipts remain byte-pinned by that approved package lock.
pub fn verify_activated_source(root: &Path) -> Result<(Value, manifest::EpochManifest)> {
    let root =
        next_successor::canonical_directory(root, "source root must be a nonsymlink directory")?;
    let (plan_value, plan, plan_sha) = load_plan(&root)?;
    let archive_path = next_successor::source_file(&root, LOCK_ARCHIVE_RELATIVE)?;
    let archive_sha = manifest::file_sha256(&archive_path)?;
    if archive_sha != APPROVED_022_LOCK_SHA256 {
        return Err(refused(
            "023 activation lost its approved 022 preimage lock",
        ));
    }
    let approved_lock: Value = serde_json::from_slice(&fs::read(archive_path)?)
        .map_err(|_| refused("invalid approved epoch-22 package lock"))?;
    let old_files = lock_files(&approved_lock)?;
    if old_files.len() != 186 {
        return Err(refused(
            "approved epoch-22 inventory has an unexpected size",
        ));
    }
    distribution::verify(&root.join("database"), BASELINE)?;
    let paths = crate::runner::AuthorityPaths {
        database_dir: root.join("database"),
    };
    let (baseline, baseline_sha) = commands::load_baseline_with_mode(
        &paths,
        BASELINE,
        distribution::SourceMode::Distribution,
    )?;
    let manifest_path = next_successor::source_file(&root, ACTIVE_MANIFEST_RELATIVE)?;
    let active_manifest_bytes = fs::read(&manifest_path)?;
    let active_manifest: Value = serde_json::from_slice(&active_manifest_bytes)
        .map_err(|_| refused("activated epoch-23 manifest is malformed"))?;
    let active_epoch = manifest::load_epoch_manifest(&manifest_path)?;
    let active_manifest_sha = manifest::file_sha256(&manifest_path)?;
    let archived_manifest_path = next_successor::source_file(&root, MANIFEST_ARCHIVE_RELATIVE)?;
    if manifest::file_sha256(&archived_manifest_path)? != plan.predecessor_manifest_sha256
        || manifest::file_sha256(&archived_manifest_path)?
            != plan.approved_predecessor_manifest_archive_sha256
    {
        return Err(refused("approved epoch-22 manifest archive changed"));
    }
    let mut expected_manifest: Value = serde_json::from_slice(&fs::read(&archived_manifest_path)?)
        .map_err(|_| refused("approved epoch-22 manifest archive is malformed"))?;
    expected_manifest["epoch"] = json!(CANDIDATE_EPOCH);
    expected_manifest["changes"]
        .as_array_mut()
        .ok_or_else(|| refused("approved epoch-22 manifest has no changes"))?
        .push(
            serde_json::to_value(&plan.successor.change)
                .map_err(|_| refused("invalid reviewed 023 change"))?,
        );
    if active_manifest != expected_manifest
        || active_epoch.epoch != CANDIDATE_EPOCH
        || active_epoch.baseline_id != BASELINE
        || active_epoch.changes.len() != CANDIDATE_EPOCH as usize
        || baseline_sha != plan.approved_predecessor_baseline_manifest_sha256
    {
        return Err(refused(
            "active source is not the exact 023 extension of approved 022",
        ));
    }
    let active_lock_path = next_successor::source_file(&root, ACTIVE_LOCK_RELATIVE)?;
    let active_lock_sha = manifest::file_sha256(&active_lock_path)?;
    let active_lock: Value = serde_json::from_slice(&fs::read(&active_lock_path)?)
        .map_err(|_| refused("invalid activated epoch-23 package lock"))?;
    let active_files = lock_files(&active_lock)?;
    let manifest_key = format!("migrations/{BASELINE}/manifest.yml");
    if lock_digest(active_files, &manifest_key) != Some(active_manifest_sha.as_str())
        || lock_digest(
            active_files,
            &format!("migrations/{BASELINE}/{NEXT_SQL_FILE}"),
        ) != Some(plan.successor.change.sha256.as_str())
    {
        return Err(refused(
            "activated package lock does not bind the epoch-23 source",
        ));
    }
    for (key, value) in old_files {
        if key == &manifest_key {
            continue;
        }
        if active_files.get(key) != Some(value) {
            return Err(refused(
                "activated package lock changed an approved 022 preimage",
            ));
        }
    }
    if active_files.keys().cloned().collect::<BTreeSet<_>>()
        != expected_input_paths(&approved_lock, true, true)?
    {
        return Err(refused(
            "activated package lock has missing or unexpected inputs",
        ));
    }
    let sql_path =
        next_successor::source_file(&root, &format!("database/{EPOCH_RELATIVE}/{NEXT_SQL_FILE}"))?;
    let proposal_path = next_successor::source_file(&root, PROPOSAL_RELATIVE)?;
    if manifest::file_sha256(&sql_path)? != plan.successor.change.sha256
        || fs::read(&sql_path)? != fs::read(&proposal_path)?
    {
        return Err(refused(
            "activated 023 SQL differs from the reviewed proposal",
        ));
    }
    if active_lock_sha == archive_sha || active_manifest_sha == plan.predecessor_manifest_sha256 {
        return Err(refused(
            "activated epoch-23 distribution did not advance its preimage",
        ));
    }
    let candidate = Candidate {
        plan: plan_value.clone(),
        manifest_value: active_manifest,
        epoch: active_epoch.clone(),
        baseline,
        baseline_sha256: baseline_sha.clone(),
        package_lock: approved_lock,
        package_lock_sha256: archive_sha.clone(),
        plan_sha256: plan_sha.clone(),
        proposal_sha256: plan.successor.change.sha256.clone(),
        predecessor_measurement_sha256: plan
            .approved_predecessor_measurement_receipt_sha256
            .clone(),
        predecessor_activation_sha256: plan.approved_predecessor_activation_receipt_sha256.clone(),
    };
    let reference_sha = verify_reference(&candidate, &root, &root)?;
    let measurement_path = next_successor::source_file(
        &root,
        &format!("database/{EPOCH_RELATIVE}/{NEXT_MEASUREMENT_FILE}"),
    )?;
    let measurement_sha = manifest::file_sha256(&measurement_path)?;
    let receipt = verify_measurement_receipt(
        &candidate,
        &json!({ "candidate_manifest_sha256": active_manifest_sha }),
        &root,
    )?;
    let activation_path = next_successor::source_file(
        &root,
        &format!("database/{EPOCH_RELATIVE}/{NEXT_ACTIVATION_FILE}"),
    )?;
    let activation_sha = manifest::file_sha256(&activation_path)?;
    let activation: Value = serde_json::from_slice(&fs::read(activation_path)?)
        .map_err(|_| refused("invalid 023 activation receipt"))?;
    if !next_successor::exact_keys(
        &activation,
        &[
            "schema",
            "status",
            "source_activation",
            "predecessor_epoch",
            "epoch",
            "baseline_id",
            "source_plan_sha256",
            "approved_predecessor_package_lock_sha256",
            "baseline_manifest_sha256",
            "predecessor_manifest_sha256",
            "epoch_manifest_sha256",
            "measurement_receipt_sha256",
            "verification_ref_sha256",
            "predecessor_measurement_receipt_sha256",
            "predecessor_activation_receipt_sha256",
        ],
    ) || activation["schema"] != "ai-platform/canonical-next-successor-activation/v1"
        || activation["status"] != "activated_source_chain"
        || activation["source_activation"] != true
        || activation["predecessor_epoch"] != PREDECESSOR_EPOCH
        || activation["epoch"] != CANDIDATE_EPOCH
        || activation["baseline_id"] != BASELINE
        || activation["source_plan_sha256"] != plan_sha
        || activation["approved_predecessor_package_lock_sha256"] != archive_sha
        || activation["baseline_manifest_sha256"] != baseline_sha
        || activation["predecessor_manifest_sha256"] != plan.predecessor_manifest_sha256
        || activation["epoch_manifest_sha256"] != active_manifest_sha
        || activation["measurement_receipt_sha256"] != measurement_sha
        || activation["verification_ref_sha256"] != reference_sha
        || activation["predecessor_measurement_receipt_sha256"]
            != plan.approved_predecessor_measurement_receipt_sha256
        || activation["predecessor_activation_receipt_sha256"]
            != plan.approved_predecessor_activation_receipt_sha256
        || receipt["source_plan_sha256"] != plan_sha
        || lock_digest(
            active_files,
            &format!("migrations/{BASELINE}/{NEXT_REFERENCE_FILE}"),
        ) != Some(reference_sha.as_str())
        || lock_digest(
            active_files,
            &format!("migrations/{BASELINE}/{NEXT_MEASUREMENT_FILE}"),
        ) != Some(measurement_sha.as_str())
        || lock_digest(
            active_files,
            &format!("migrations/{BASELINE}/{NEXT_ACTIVATION_FILE}"),
        ) != Some(activation_sha.as_str())
    {
        return Err(refused(
            "023 activation receipt does not bind the measured source chain",
        ));
    }
    Ok((plan_value, active_epoch))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repository_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../..")
            .canonicalize()
            .unwrap()
    }

    #[test]
    fn active_022_is_the_exact_preimage_for_one_reviewed_023_extension() {
        let root = repository_root();
        let active = manifest::load_epoch_manifest(&root.join(ACTIVE_MANIFEST_RELATIVE)).unwrap();
        if active.epoch == PREDECESSOR_EPOCH {
            let candidate = validate_source(&root).unwrap();
            assert_eq!(candidate.epoch.epoch, CANDIDATE_EPOCH);
            assert_eq!(candidate.epoch.changes.len(), CANDIDATE_EPOCH as usize);
            assert_eq!(candidate.package_lock_sha256, APPROVED_022_LOCK_SHA256);
            assert_eq!(
                candidate.proposal_sha256,
                "80f3b91376dbc773c71359490801718924c93313a6bfbf10153718fe674dd6e2"
            );
            assert_eq!(candidate.plan["fingerprint_status"], "not_measured");
        } else if active.epoch == CANDIDATE_EPOCH {
            let (_, verified) = verify_activated_source(&root).unwrap();
            assert_eq!(verified.epoch, CANDIDATE_EPOCH);
            assert_eq!(verified.changes.len(), CANDIDATE_EPOCH as usize);
        } else if active.epoch == crate::source_tools::next_successor_024::CANDIDATE_EPOCH {
            let (_, verified) =
                crate::source_tools::next_successor_024::verify_activated_source(&root).unwrap();
            assert_eq!(
                verified.epoch,
                crate::source_tools::next_successor_024::CANDIDATE_EPOCH
            );
            assert_eq!(
                verified.changes.len(),
                crate::source_tools::next_successor_024::CANDIDATE_EPOCH as usize
            );
        } else {
            panic!("unexpected canonical schema epoch");
        }
    }
}
