-- Isolated draft only. Do not apply directly or copy into canonical migrations.
-- Candidate canonical successor 022: let the Assistant runtime create its
-- thread projection as part of the existing root-thread creation transaction.
--
-- Source evidence:
--   PostgresThreadStore::create_thread inserts assistant_runtime_thread_projections.
--   The runtime already has SELECT and UPDATE on the projection table, and its
--   normal create-thread/first-turn paths have the required rights on the
--   thread, member, owner, invocation, item, and execution-function surfaces.
--   Only INSERT on this projection is missing.
--
-- The forward-only delta grants INSERT to the prefix-derived runtime role;
-- it does not alter the frozen baseline or grant other projection privileges.
DO $permissions$
DECLARE prefix text := left(current_user, length(current_user) - 5);
BEGIN
    IF current_user !~ '^[a-z][a-z0-9_]{0,20}_owner$' THEN
        RAISE EXCEPTION 'Assistant runtime projection grant requires the authority owner role';
    END IF;
    EXECUTE format(
        'GRANT INSERT ON TABLE assistant.assistant_runtime_thread_projections TO %I',
        prefix || 'runtime'
    );
END
$permissions$;
