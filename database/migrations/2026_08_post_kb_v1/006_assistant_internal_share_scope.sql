-- AR-06: audience and private source authority for existing share tables.
ALTER TABLE assistant.conversation_shares
    ADD COLUMN audience TEXT NOT NULL DEFAULT 'public',
    ADD COLUMN source_scope JSONB,
    ADD CONSTRAINT conversation_shares_audience_check
        CHECK (audience IN ('public', 'internal')),
    ADD CONSTRAINT conversation_shares_scope_check
        CHECK (
            (audience = 'public' AND source_scope IS NULL)
            OR (audience = 'internal' AND source_scope IS NOT NULL
                AND jsonb_typeof(source_scope) = 'object')
        );

ALTER TABLE assistant.artifact_shares
    ADD COLUMN audience TEXT NOT NULL DEFAULT 'public',
    ADD COLUMN source_scope JSONB,
    ADD CONSTRAINT artifact_shares_audience_check
        CHECK (audience IN ('public', 'internal')),
    ADD CONSTRAINT artifact_shares_scope_check
        CHECK (
            (audience = 'public' AND source_scope IS NULL)
            OR (audience = 'internal' AND source_scope IS NOT NULL
                AND jsonb_typeof(source_scope) = 'object')
        );
