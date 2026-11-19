
                CREATE TABLE eval_datasets (
                    dataset_id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
                    tenant_id VARCHAR(255) NOT NULL,
                    name VARCHAR(160) NOT NULL DEFAULT '',
                    description TEXT NOT NULL DEFAULT '',
                    version VARCHAR(64) NOT NULL DEFAULT 'v1',
                    schema JSONB NOT NULL DEFAULT '{}'::jsonb,
                    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
                    created_by VARCHAR(255) NOT NULL DEFAULT '',
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
                );
                CREATE TABLE eval_examples (
                    example_id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
                    dataset_id UUID NOT NULL REFERENCES eval_datasets(dataset_id),
                    tenant_id VARCHAR(255) NOT NULL,
                    split VARCHAR(32) NOT NULL DEFAULT 'regression',
                    input JSONB NOT NULL DEFAULT '{}'::jsonb,
                    expected_output JSONB NOT NULL DEFAULT '{}'::jsonb,
                    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
                    source_trace_id UUID,
                    source_span_id UUID,
                    created_by VARCHAR(255) NOT NULL DEFAULT '',
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
                );
                CREATE TABLE eval_experiment_runs (
                    run_id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
                    tenant_id VARCHAR(255) NOT NULL,
                    dataset_id UUID,
                    run_mode VARCHAR(32) NOT NULL DEFAULT 'rescore_trace',
                    status VARCHAR(32) NOT NULL DEFAULT 'queued',
                    target_snapshot JSONB NOT NULL DEFAULT '{}'::jsonb,
                    score_summary JSONB NOT NULL DEFAULT '{}'::jsonb,
                    metrics JSONB NOT NULL DEFAULT '{}'::jsonb,
                    dataset_manifest_hash VARCHAR(64),
                    evaluator_suite_hash VARCHAR(64)
                );
                CREATE TABLE llm_providers (
                    tenant_id VARCHAR(255) NOT NULL,
                    provider_id VARCHAR(50) NOT NULL,
                    is_enabled BOOLEAN NOT NULL DEFAULT TRUE,
                    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    PRIMARY KEY (tenant_id, provider_id)
                );
                CREATE TABLE llm_models (
                    tenant_id VARCHAR(255) NOT NULL,
                    provider_id VARCHAR(50) NOT NULL,
                    model_id VARCHAR(100) NOT NULL,
                    access_level VARCHAR(20) NOT NULL DEFAULT 'public',
                    is_enabled BOOLEAN NOT NULL DEFAULT TRUE,
                    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    PRIMARY KEY (tenant_id, provider_id, model_id),
                    FOREIGN KEY (tenant_id, provider_id)
                        REFERENCES llm_providers(tenant_id, provider_id)
                );
                CREATE TABLE agent_version_revocations (
                    tenant_id VARCHAR(255) NOT NULL,
                    agent_version_id UUID NOT NULL,
                    PRIMARY KEY (tenant_id, agent_version_id)
                );
                