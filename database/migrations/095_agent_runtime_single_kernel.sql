-- 095 - Make the Agent Runtime the sole session owner.
--
-- Additive cutover: historical assignment values are normalized in place so
-- existing sessions remain resumable.  No session, thread, item, or run data
-- is deleted and no legacy value is emitted after this migration.

BEGIN;

-- Older installations already have the lease guards from migration 090.
-- Rewrite their owner/engine predicates in place without replacing the
-- function contract, so upgraded databases use the same single owner as
-- fresh databases.
DO $$
DECLARE
    v_definition TEXT;
    v_oid OID;
BEGIN
    -- Update all visible legacy PL/pgSQL guards in one transaction.  The
    -- function signatures stay unchanged; only the owner/engine literals and
    -- persisted event labels are normalized.
    FOR v_oid, v_definition IN
        SELECT p.oid, pg_get_functiondef(p.oid)
          FROM pg_proc AS p
         WHERE p.pronamespace = to_regnamespace(current_schema())
           AND p.prokind = 'f'
           AND p.proname = ANY (ARRAY[
               'append_assistant_runtime_item',
               'ensure_assistant_runtime_thread',
               'import_assistant_legacy_session',
               'issue_assistant_runtime_turn',
               'reserve_assistant_runtime_model_call'
           ])
           AND position('codex' IN pg_get_functiondef(p.oid)) > 0
    LOOP
        EXECUTE replace(
            replace(
                replace(
                    replace(
                        replace(v_definition, 'codex_candidate', 'agent_runtime'),
                        'codex_harness', 'agent_runtime'
                    ),
                    'codex-runtime', 'agent-runtime'
                ),
                'codex/', 'agent-runtime/'
            ),
            '''codex''', '''agent_runtime'''
        );
    END LOOP;
END;
$$;

DROP TRIGGER IF EXISTS assistant_session_runtime_assignments_immutable
    ON assistant_session_runtime_assignments;

ALTER TABLE assistant_session_runtime_assignments
    DROP CONSTRAINT IF EXISTS assistant_session_runtime_assignments_owner_check,
    DROP CONSTRAINT IF EXISTS assistant_session_runtime_assignments_kernel_check;

UPDATE assistant_session_runtime_assignments
   SET runtime_owner = 'agent_runtime',
       kernel_revision = COALESCE(NULLIF(kernel_revision, ''), 'legacy-runtime')
 WHERE runtime_owner IN ('python_control', 'codex_candidate')
    OR kernel_revision IS NULL
    OR kernel_revision = '';

CREATE TRIGGER assistant_session_runtime_assignments_immutable
    BEFORE UPDATE ON assistant_session_runtime_assignments
    FOR EACH ROW EXECUTE FUNCTION assistant_runtime_reject_assignment_update();

ALTER TABLE assistant_session_runtime_assignments
    ADD CONSTRAINT assistant_session_runtime_assignments_owner_check
        CHECK (runtime_owner = 'agent_runtime'),
    ADD CONSTRAINT assistant_session_runtime_assignments_kernel_check
        CHECK (kernel_revision IS NOT NULL AND length(kernel_revision) > 0);

ALTER TABLE assistant_runtime_threads
    ALTER COLUMN kernel_owner SET DEFAULT 'agent_runtime',
    DROP CONSTRAINT IF EXISTS assistant_runtime_threads_kernel_check;

UPDATE assistant_runtime_threads
   SET kernel_owner = 'agent_runtime'
 WHERE kernel_owner IN ('codex', 'python');

ALTER TABLE assistant_runtime_threads
    ADD CONSTRAINT assistant_runtime_threads_kernel_check
        CHECK (kernel_owner = 'agent_runtime');

ALTER TABLE assistant_runs
    DROP CONSTRAINT IF EXISTS assistant_runs_harness_shape_check;

UPDATE assistant_runs
   SET engine = 'agent_runtime'
 WHERE engine = 'codex_harness';

ALTER TABLE assistant_runs
    ADD CONSTRAINT assistant_runs_harness_shape_check CHECK (
        (engine <> 'agent_runtime'
            AND harness_thread_id IS NULL
            AND harness_turn_id IS NULL
            AND runtime_snapshot_id IS NULL
            AND kernel_revision IS NULL
            AND capability_revision IS NULL)
        OR
        (engine = 'agent_runtime'
            AND harness_thread_id IS NOT NULL
            AND harness_turn_id IS NOT NULL
            AND runtime_snapshot_id IS NOT NULL
            AND kernel_revision IS NOT NULL
            AND capability_revision >= 1)
    );

COMMENT ON TABLE assistant_session_runtime_assignments IS
    'Immutable tenant/user/session ownership for the single Agent Runtime.';

COMMIT;
