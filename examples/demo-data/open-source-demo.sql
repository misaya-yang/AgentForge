-- Open-source demo data for local AI Gateway installations.
-- Safe to rerun: all records use deterministic IDs and ON CONFLICT upserts.

BEGIN;

INSERT INTO tenants (tenant_id, name, description, tier, status, metadata)
VALUES
    (
        'default',
        'Default',
        'Default local tenant for open-source demo data',
        'normal',
        'active',
        '{"seed":"open-source-demo"}'::jsonb
    )
ON CONFLICT (tenant_id) DO UPDATE
SET
    name = EXCLUDED.name,
    description = EXCLUDED.description,
    status = EXCLUDED.status,
    metadata = tenants.metadata || EXCLUDED.metadata,
    updated_at = NOW();

INSERT INTO users (
    user_id,
    username,
    email,
    display_name,
    tenant_id,
    tier,
    roles,
    permissions,
    status,
    metadata,
    force_password_change,
    email_verified
)
VALUES (
    'demo-user',
    'demo-user',
    'demo@example.com',
    'Demo User',
    'default',
    'admin',
    ARRAY['admin']::VARCHAR(50)[],
    ARRAY[
        'console:dashboard:view',
        'console:services:view',
        'knowledge:dataset:view',
        'conversation:playground:access',
        'console:settings:view',
        'user:list',
        'user:edit'
    ]::VARCHAR(100)[],
    'active',
    '{"seed":"open-source-demo"}'::jsonb,
    FALSE,
    TRUE
)
ON CONFLICT (user_id) DO UPDATE
SET
    email = EXCLUDED.email,
    display_name = EXCLUDED.display_name,
    tenant_id = EXCLUDED.tenant_id,
    tier = EXCLUDED.tier,
    roles = EXCLUDED.roles,
    permissions = EXCLUDED.permissions,
    status = EXCLUDED.status,
    metadata = users.metadata || EXCLUDED.metadata,
    force_password_change = EXCLUDED.force_password_change,
    email_verified = EXCLUDED.email_verified,
    updated_at = NOW();

INSERT INTO datasets (
    dataset_id,
    name,
    description,
    tenant_id,
    visibility,
    embedding_provider,
    embedding_model,
    embedding_dimension,
    embedding_config,
    collection_name,
    kb_type,
    created_by
)
VALUES (
    'demo-kb-ai-gateway',
    'AI Gateway Demo Knowledge Base',
    'Small local knowledge base used by the open-source quickstart.',
    'default',
    'tenant',
    'openai',
    'text-embedding-3-small',
    1536,
    '{"demo":true}'::jsonb,
    'demo_kb_ai_gateway',
    'document',
    'demo-user'
)
ON CONFLICT (dataset_id) DO UPDATE
SET
    name = EXCLUDED.name,
    description = EXCLUDED.description,
    tenant_id = EXCLUDED.tenant_id,
    visibility = EXCLUDED.visibility,
    embedding_provider = EXCLUDED.embedding_provider,
    embedding_model = EXCLUDED.embedding_model,
    embedding_dimension = EXCLUDED.embedding_dimension,
    embedding_config = EXCLUDED.embedding_config,
    collection_name = EXCLUDED.collection_name,
    kb_type = EXCLUDED.kb_type,
    is_deleted = FALSE,
    updated_at = NOW();

INSERT INTO documents (
    document_id,
    dataset_id,
    title,
    source_type,
    source_uri,
    mime_type,
    status,
    progress,
    content,
    metadata,
    enabled,
    word_count,
    segment_count,
    tokens,
    created_by,
    completed_at
)
VALUES (
    'demo-doc-quickstart-runbook',
    'demo-kb-ai-gateway',
    'Local Quickstart Runbook',
    'text',
    'examples/demo-data/open-source-demo.sql',
    'text/plain',
    'completed',
    100,
    'The local quickstart starts the gateway, frontend, Agent Runtime, Capability Worker, knowledge service, PostgreSQL, Redis, and Qdrant. Use make validate before sharing a deployment.',
    '{"seed":"open-source-demo","route":"/knowledge/demo-kb-ai-gateway"}'::jsonb,
    TRUE,
    27,
    1,
    42,
    'demo-user',
    NOW()
)
ON CONFLICT (document_id) DO UPDATE
SET
    title = EXCLUDED.title,
    status = EXCLUDED.status,
    progress = EXCLUDED.progress,
    content = EXCLUDED.content,
    metadata = EXCLUDED.metadata,
    enabled = EXCLUDED.enabled,
    word_count = EXCLUDED.word_count,
    segment_count = EXCLUDED.segment_count,
    tokens = EXCLUDED.tokens,
    completed_at = EXCLUDED.completed_at,
    updated_at = NOW();

INSERT INTO segments (
    segment_id,
    dataset_id,
    document_id,
    position,
    text,
    token_count,
    metadata,
    source_type,
    source_reference,
    citation_text,
    language,
    content_hash,
    enabled,
    status,
    word_count,
    keywords,
    created_by
)
VALUES (
    'demo-seg-quickstart-001',
    'demo-kb-ai-gateway',
    'demo-doc-quickstart-runbook',
    1,
    'AI Gateway is a local-first open-source platform for routing AI providers, managing assistant sessions, and testing knowledge-base retrieval. The demo seed gives contributors stable records for dynamic pages.',
    42,
    '{"seed":"open-source-demo"}'::jsonb,
    'demo',
    '{"file":"examples/demo-data/open-source-demo.sql"}'::jsonb,
    'Local Quickstart Runbook',
    'en',
    'demo-seg-quickstart-001',
    TRUE,
    'completed',
    27,
    '["ai-gateway","quickstart","knowledge-base"]'::jsonb,
    'demo-user'
)
ON CONFLICT (document_id, position) DO UPDATE
SET
    text = EXCLUDED.text,
    token_count = EXCLUDED.token_count,
    metadata = EXCLUDED.metadata,
    source_reference = EXCLUDED.source_reference,
    citation_text = EXCLUDED.citation_text,
    language = EXCLUDED.language,
    content_hash = EXCLUDED.content_hash,
    enabled = EXCLUDED.enabled,
    status = EXCLUDED.status,
    word_count = EXCLUDED.word_count,
    keywords = EXCLUDED.keywords,
    updated_at = NOW();

INSERT INTO sessions (
    session_id,
    service_id,
    user_id,
    tenant_id,
    history,
    metadata,
    config,
    status
)
VALUES (
    'demo-assistant-session',
    '__builtin_assistant__',
    'demo-user',
    'default',
    '[
        {"role":"user","content":"What can this platform do?"},
        {"role":"assistant","content":"It routes AI providers, serves a general assistant, and exposes knowledge-base workflows through a local gateway."}
    ]'::jsonb,
    '{"title":"Open-source demo conversation","seed":"open-source-demo"}'::jsonb,
    '{"model_id":"gpt-4o"}'::jsonb,
    'active'
)
ON CONFLICT (session_id) DO UPDATE
SET
    service_id = EXCLUDED.service_id,
    user_id = EXCLUDED.user_id,
    tenant_id = EXCLUDED.tenant_id,
    history = EXCLUDED.history,
    metadata = EXCLUDED.metadata,
    config = EXCLUDED.config,
    status = EXCLUDED.status,
    updated_at = NOW();

INSERT INTO conversation_shares (
    id,
    share_code,
    session_id,
    user_id,
    tenant_id,
    title,
    snapshot,
    message_count,
    artifact_count,
    is_active,
    view_count
)
VALUES (
    '00000000-0000-4000-8000-000000000045'::uuid,
    'demo-share',
    'demo-assistant-session',
    'demo-user',
    'default',
    'Open-source demo conversation',
    '{
        "model_id":"gpt-4o",
        "shared_at":"2026-06-18T00:00:00Z",
        "artifacts":[],
        "messages":[
            {"role":"user","content":"What can this platform do?","timestamp":"2026-06-18T00:00:00Z"},
            {"role":"assistant","content":"It routes AI providers, serves a general assistant, and exposes knowledge-base workflows through a local gateway.","timestamp":"2026-06-18T00:00:01Z"}
        ]
    }'::jsonb,
    2,
    0,
    TRUE,
    1
)
ON CONFLICT (share_code) DO UPDATE
SET
    session_id = EXCLUDED.session_id,
    user_id = EXCLUDED.user_id,
    tenant_id = EXCLUDED.tenant_id,
    title = EXCLUDED.title,
    snapshot = EXCLUDED.snapshot,
    message_count = EXCLUDED.message_count,
    artifact_count = EXCLUDED.artifact_count,
    is_active = EXCLUDED.is_active,
    view_count = EXCLUDED.view_count;

INSERT INTO quizzes (
    id,
    tenant_id,
    created_by,
    title,
    description,
    dataset_ids,
    topic,
    question_count,
    difficulty,
    config,
    status
)
VALUES (
    '00000000-0000-4000-8000-000000000041'::uuid,
    'default',
    'demo-user',
    'AI Gateway Demo Quiz',
    'A one-question quiz that proves public quiz routes can render after seeding.',
    '["demo-kb-ai-gateway"]'::jsonb,
    'Open-source quickstart',
    1,
    'easy',
    '{"seed":"open-source-demo"}'::jsonb,
    'ready'
)
ON CONFLICT (id) DO UPDATE
SET
    title = EXCLUDED.title,
    description = EXCLUDED.description,
    dataset_ids = EXCLUDED.dataset_ids,
    topic = EXCLUDED.topic,
    question_count = EXCLUDED.question_count,
    difficulty = EXCLUDED.difficulty,
    config = EXCLUDED.config,
    status = EXCLUDED.status,
    updated_at = NOW();

INSERT INTO quiz_questions (
    id,
    quiz_id,
    question_num,
    question_type,
    question_text,
    options,
    correct_answer,
    explanation,
    source_chunks
)
VALUES (
    '00000000-0000-4000-8000-000000000042'::uuid,
    '00000000-0000-4000-8000-000000000041'::uuid,
    1,
    'mc_single',
    'Which route should render after loading the open-source demo data?',
    '[
        {"label":"A","text":"/quiz/demo-quiz"},
        {"label":"B","text":"/settings/billing"}
    ]'::jsonb,
    '{"answer":"A"}'::jsonb,
    'The seed creates a public quiz share with code demo-quiz.',
    '[{"dataset_id":"demo-kb-ai-gateway","document_id":"demo-doc-quickstart-runbook","segment_id":"demo-seg-quickstart-001"}]'::jsonb
)
ON CONFLICT (quiz_id, question_num) DO UPDATE
SET
    question_text = EXCLUDED.question_text,
    options = EXCLUDED.options,
    correct_answer = EXCLUDED.correct_answer,
    explanation = EXCLUDED.explanation,
    source_chunks = EXCLUDED.source_chunks;

INSERT INTO artifact_shares (
    id,
    share_code,
    kind,
    title,
    payload,
    answer_keys,
    tenant_id,
    created_by,
    is_active,
    max_attempts,
    require_name,
    created_at
)
VALUES (
    '00000000-0000-4000-8000-000000000043'::uuid,
    'demo-quiz',
    'quiz',
    'AI Gateway Demo Quiz',
    '{
        "quiz_id":"00000000-0000-4000-8000-000000000041",
        "description":"A one-question quiz that proves public quiz routes can render after seeding.",
        "question_count":1,
        "difficulty":"easy",
        "questions":[
            {
                "id":"00000000-0000-4000-8000-000000000042",
                "question_num":1,
                "question_type":"mc_single",
                "question_text":"Which route should render after loading the open-source demo data?",
                "options":[
                    {"label":"A","text":"/quiz/demo-quiz"},
                    {"label":"B","text":"/settings/billing"}
                ]
            }
        ]
    }'::jsonb,
    '[
        {
            "id":"00000000-0000-4000-8000-000000000042",
            "question_num":1,
            "correct_answer":{"answer":"A"},
            "explanation":"The seed creates a public quiz share with code demo-quiz."
        }
    ]'::jsonb,
    'default',
    'demo-user',
    TRUE,
    NULL,
    FALSE,
    NOW()
)
ON CONFLICT (share_code) DO UPDATE
SET
    title = EXCLUDED.title,
    payload = EXCLUDED.payload,
    answer_keys = EXCLUDED.answer_keys,
    is_active = EXCLUDED.is_active,
    max_attempts = EXCLUDED.max_attempts,
    require_name = EXCLUDED.require_name;

COMMIT;
