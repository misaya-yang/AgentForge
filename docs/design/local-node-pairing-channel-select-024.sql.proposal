-- Inactive candidate only. Do not apply directly or copy into canonical migrations.
-- 024 grants Local Node's Gateway role column-scoped SELECT needed by pairing,
-- heartbeat, control and receipt flows. Existing table INSERT/UPDATE rights
-- remain unchanged; no table-level SELECT is granted.
DO $permissions$
DECLARE prefix text := left(current_user, length(current_user) - 5);
BEGIN
    IF current_user !~ '^[a-z][a-z0-9_]{0,20}_owner$' THEN
        RAISE EXCEPTION 'successor 024 grants require the authority owner role';
    END IF;
    EXECUTE format(
        'GRANT SELECT (challenge_id, user_code_sha256, tenant_id, user_id, consumed_at, expires_at) ON TABLE gateway.local_node_pairing_challenges TO %I',
        prefix || 'gateway'
    );
    EXECUTE format(
        'GRANT SELECT (channel_id, device_id, tenant_id, user_id, status, receipt_cursor, fingerprint, created_at) ON TABLE gateway.local_node_channels TO %I',
        prefix || 'gateway'
    );
END
$permissions$;
