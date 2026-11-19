
                CREATE TABLE datasets (
                    dataset_id VARCHAR(255) PRIMARY KEY,
                    tenant_id VARCHAR(255) NOT NULL DEFAULT '',
                    name VARCHAR(255) NOT NULL DEFAULT '',
                    created_by VARCHAR(255) NOT NULL DEFAULT '',
                    visibility VARCHAR(32) NOT NULL DEFAULT 'private',
                    is_deleted BOOLEAN NOT NULL DEFAULT FALSE
                );
                CREATE TABLE users (
                    user_id VARCHAR(255) PRIMARY KEY,
                    tenant_id VARCHAR(255) NOT NULL,
                    roles VARCHAR(255)[] NOT NULL DEFAULT '{}',
                    status VARCHAR(50) NOT NULL DEFAULT 'active'
                );
                CREATE TABLE dataset_permissions (
                    dataset_id VARCHAR(255) NOT NULL,
                    subject_type VARCHAR(32) NOT NULL,
                    subject_id VARCHAR(255) NOT NULL,
                    permission VARCHAR(32) NOT NULL
                );
                CREATE TABLE audit_logs (
                    id BIGSERIAL PRIMARY KEY,
                    event_type VARCHAR(100) NOT NULL,
                    user_id VARCHAR(255),
                    tenant_id VARCHAR(255),
                    resource_type VARCHAR(100),
                    resource_id VARCHAR(255),
                    action VARCHAR(50) NOT NULL,
                    request_summary JSONB,
                    status VARCHAR(50) NOT NULL,
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
                );
                