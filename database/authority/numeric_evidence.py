"""Read-only evidence and identity primitives for ambiguous numeric ledgers.

Versions 016, 030 and 031 were reused by old runners.  A numeric row is not
enough to infer which file ran, so continuation requires both an immutable
file identity and a complete read-only check of the resulting database state.
This module never mutates the legacy ledger or administrator-owned pricing.
"""

from __future__ import annotations

import json
import re
from dataclasses import dataclass, field
from typing import Any

from .discovery import MIGRATION_PATTERN
from .runner import AuthorityBlockedError


@dataclass(frozen=True)
class NumericLedgerRecord:
    version: str
    name: str | None = None
    checksum: str | None = None
    dirty: bool = False


@dataclass
class NumericReconciliationReceipt:
    """Machine-readable proof for the three ambiguous historic revisions."""

    ledger_kind: str = "numeric"
    verdict: str = "proven"
    versions: dict[str, dict[str, Any]] = field(default_factory=dict)
    notes: list[str] = field(default_factory=list)

    def block(self, version: str, reason: str) -> None:
        self.verdict = "blocked"
        self.versions.setdefault(version, {})["verdict"] = "blocked"
        self.versions[version]["reason"] = reason

    def as_dict(self) -> dict[str, Any]:
        return {
            "ledger_kind": self.ledger_kind,
            "verdict": self.verdict,
            "versions": self.versions,
            "notes": self.notes,
        }

    def to_json(self) -> str:
        return json.dumps(self.as_dict(), indent=2, sort_keys=True)


class NumericReconciliationBlocked(AuthorityBlockedError):
    """A blocked receipt preserved for logging or explicit evidence output."""

    def __init__(self, message: str, receipt: NumericReconciliationReceipt) -> None:
        super().__init__(message + "\n" + receipt.to_json())
        self.receipt = receipt


# Every predicate below is read-only and binds named constraints/indexes to the
# one accepted table OID.  Merely finding a same-named object in another schema
# cannot prove a migration.  Unqualified data probes are safe here because the
# caller first installs the historical knowledge/gateway/assistant/public path;
# missing or ambiguous relations fail closed.
NUMERIC_EVIDENCE_SQL = {
    "016_effective": r"""
        /* arc03-legacy-evidence:016-effective */
        WITH platform_relations AS (
            SELECT c.oid, c.relname, c.relowner
            FROM pg_class AS c
            JOIN pg_namespace AS n ON n.oid = c.relnamespace
            WHERE n.nspname IN ('public', 'gateway', 'assistant', 'knowledge')
              AND c.relkind IN ('r', 'p')
              AND c.relname IN ('confluence_space_bindings', 'usage_hourly_aggregates')
        ), relation_counts AS (
            SELECT relname, count(*) AS copies FROM platform_relations GROUP BY relname
        ), expected_columns(table_name, column_name, udt_name_re, not_null, default_re) AS (
            VALUES
                ('confluence_space_bindings', 'root_page_ids', '_text', FALSE,
                    '^''\{\}''::text\[\]$'),
                ('confluence_space_bindings', 'root_page_titles', '_text', FALSE,
                    '^''\{\}''::text\[\]$'),
                ('confluence_space_bindings', 'sync_images', 'bool', FALSE,
                    '^(true|false)$'),
                ('confluence_space_bindings', 'image_max_size_bytes', 'int4', FALSE,
                    '^3145728$'),
                ('usage_hourly_aggregates', 'id', 'uuid', TRUE,
                    '^gen_random_uuid\(\)$'),
                ('usage_hourly_aggregates', 'tenant_id', 'varchar', TRUE, NULL),
                ('usage_hourly_aggregates', 'bucket_start', '^(timestamp|timestamptz)$', TRUE,
                    NULL),
                ('usage_hourly_aggregates', 'date', 'date', TRUE, NULL),
                ('usage_hourly_aggregates', 'request_count', 'int4', FALSE, '^0$'),
                ('usage_hourly_aggregates', 'success_count', 'int4', FALSE, '^0$'),
                ('usage_hourly_aggregates', 'error_count', 'int4', FALSE, '^0$'),
                ('usage_hourly_aggregates', 'total_input_tokens', 'int8', FALSE, '^0$'),
                ('usage_hourly_aggregates', 'total_output_tokens', 'int8', FALSE, '^0$'),
                ('usage_hourly_aggregates', 'total_cost_cents', 'int8', FALSE, '^0$'),
                ('usage_hourly_aggregates', 'avg_latency_ms', 'int4', FALSE, '^0$'),
                ('usage_hourly_aggregates', 'avg_first_token_ms', 'int4', FALSE, '^0$'),
                ('usage_hourly_aggregates', 'created_at', '^(timestamp|timestamptz)$', FALSE,
                    '^(CURRENT_TIMESTAMP|now\(\))$'),
                ('usage_hourly_aggregates', 'updated_at', '^(timestamp|timestamptz)$', FALSE,
                    '^(CURRENT_TIMESTAMP|now\(\))$')
        ), actual_columns AS (
            SELECT r.relname AS table_name, a.attname AS column_name,
                   t.typname AS udt_name, a.attnotnull AS not_null,
                   pg_get_expr(d.adbin, d.adrelid) AS column_default
            FROM platform_relations AS r
            JOIN pg_attribute AS a ON a.attrelid = r.oid
            JOIN pg_type AS t ON t.oid = a.atttypid
            LEFT JOIN pg_attrdef AS d ON d.adrelid = r.oid AND d.adnum = a.attnum
            WHERE a.attnum > 0 AND NOT a.attisdropped
        ), column_mismatches AS (
            SELECT e.*
            FROM expected_columns AS e
            LEFT JOIN actual_columns AS a USING (table_name, column_name)
            WHERE a.column_name IS NULL OR a.udt_name !~ e.udt_name_re
               OR a.not_null <> e.not_null
               OR (e.default_re IS NULL AND a.column_default IS NOT NULL)
               OR (e.default_re IS NOT NULL AND COALESCE(a.column_default, '') !~ e.default_re)
        ), usage_table AS (
            SELECT oid FROM platform_relations WHERE relname = 'usage_hourly_aggregates'
        ), usage_dimensions AS (
            SELECT a.attname, a.attnotnull,
                   pg_get_expr(d.adbin, d.adrelid) AS column_default
            FROM pg_attribute AS a
            JOIN pg_type AS t ON t.oid = a.atttypid
            LEFT JOIN pg_attrdef AS d ON d.adrelid = a.attrelid AND d.adnum = a.attnum
            WHERE a.attrelid = (SELECT oid FROM usage_table)
              AND a.attname IN ('user_id', 'model', 'assistant_id', 'service_id')
              AND a.attnum > 0 AND NOT a.attisdropped AND t.typname = 'varchar'
        ), usage_timestamps AS (
            SELECT t.typname
            FROM pg_attribute AS a
            JOIN pg_type AS t ON t.oid = a.atttypid
            WHERE a.attrelid = (SELECT oid FROM usage_table)
              AND a.attname IN ('bucket_start', 'created_at', 'updated_at')
              AND a.attnum > 0 AND NOT a.attisdropped
        ), usage_constraints AS (
            SELECT c.*, ARRAY(
                SELECT a.attname
                FROM unnest(c.conkey) WITH ORDINALITY AS key(attnum, position)
                JOIN pg_attribute AS a ON a.attrelid = c.conrelid AND a.attnum = key.attnum
                ORDER BY key.position
            ) AS columns
            FROM pg_constraint AS c
            WHERE c.conrelid = (SELECT oid FROM usage_table)
        ), required_indexes(name, columns) AS (
            VALUES
                ('idx_usage_hourly_tenant_date', ARRAY['tenant_id', 'date']::text[]),
                ('idx_usage_hourly_bucket', ARRAY['bucket_start']::text[])
        ), actual_indexes AS (
            SELECT ic.relname AS name, i.indisvalid, i.indisready, i.indisunique,
                   pg_get_expr(i.indpred, i.indrelid) AS predicate,
                   pg_get_indexdef(i.indexrelid) AS definition,
                   ARRAY(
                       SELECT a.attname
                       FROM unnest(i.indkey::smallint[]) WITH ORDINALITY AS key(attnum, position)
                       JOIN pg_attribute AS a
                         ON a.attrelid = i.indrelid AND a.attnum = key.attnum
                       WHERE key.position <= i.indnkeyatts
                       ORDER BY key.position
                   ) AS columns
            FROM pg_index AS i
            JOIN pg_class AS ic ON ic.oid = i.indexrelid
            WHERE i.indrelid = (SELECT oid FROM usage_table)
        )
        SELECT
            COALESCE((SELECT copies = 1 FROM relation_counts
                      WHERE relname = 'confluence_space_bindings'), FALSE)
            AND COALESCE((SELECT copies = 1 FROM relation_counts
                          WHERE relname = 'usage_hourly_aggregates'), FALSE)
            AND NOT EXISTS (SELECT 1 FROM column_mismatches)
            AND (SELECT count(*) = 4 FROM usage_dimensions)
            AND (
                NOT EXISTS (
                    SELECT 1 FROM usage_dimensions
                    WHERE attnotnull OR column_default IS NOT NULL
                )
                OR NOT EXISTS (
                    SELECT 1 FROM usage_dimensions
                    WHERE NOT attnotnull
                       OR COALESCE(column_default, '') !~ '^''''::character varying$'
                )
            )
            AND (SELECT count(*) = 3 AND count(DISTINCT typname) = 1
                 FROM usage_timestamps)
            AND EXISTS (
                SELECT 1 FROM usage_constraints
                WHERE contype = 'p' AND convalidated AND columns = ARRAY['id']::text[]
            )
            AND (
                (
                    EXISTS (
                        SELECT 1 FROM usage_constraints
                        WHERE conname = 'uq_usage_hourly_aggregates_dimensions'
                          AND contype = 'u' AND convalidated
                          AND columns = ARRAY[
                              'tenant_id', 'user_id', 'model', 'assistant_id', 'service_id',
                              'bucket_start'
                          ]::text[]
                    )
                    AND NOT EXISTS (
                        SELECT 1 FROM actual_indexes WHERE name = 'idx_usage_hourly_unique'
                    )
                )
                OR (
                    NOT EXISTS (
                        SELECT 1 FROM usage_constraints
                        WHERE conname = 'uq_usage_hourly_aggregates_dimensions'
                    )
                    AND EXISTS (
                        SELECT 1 FROM actual_indexes
                        WHERE name = 'idx_usage_hourly_unique'
                          AND indisunique AND indisvalid AND indisready
                          AND definition ~ 'COALESCE\(user_id, ''''::character varying\)'
                          AND definition ~ 'COALESCE\(model, ''''::character varying\)'
                          AND definition ~ 'COALESCE\(assistant_id, ''''::character varying\)'
                          AND definition ~ 'COALESCE\(service_id, ''''::character varying\)'
                    )
                )
            )
            AND NOT EXISTS (
                SELECT 1 FROM required_indexes AS required
                LEFT JOIN actual_indexes AS actual USING (name)
                WHERE actual.name IS NULL OR NOT actual.indisvalid OR NOT actual.indisready
                   OR actual.indisunique OR actual.columns <> required.columns
                   OR actual.predicate IS NOT NULL
            )
            AND NOT EXISTS (
                SELECT 1 FROM confluence_space_bindings
                WHERE root_page_id IS NOT NULL
                  AND NOT COALESCE(root_page_ids, '{}'::text[]) @> ARRAY[root_page_id]
            )
            AND NOT EXISTS (
                SELECT 1 FROM confluence_space_bindings
                WHERE root_page_title IS NOT NULL
                  AND NOT COALESCE(root_page_titles, '{}'::text[]) @> ARRAY[root_page_title]
            )
    """,
    "030_forward": r"""
        /* arc03-legacy-evidence:030-forward */
        WITH expected_tables(name) AS (
            VALUES ('usage_records'), ('usage_daily_aggregates'),
                   ('usage_hourly_aggregates'), ('user_quotas'), ('model_pricing'),
                   ('quota_alerts'), ('security_event_daily_aggregates')
        ), platform_relations AS (
            SELECT c.oid, c.relname
            FROM pg_class AS c
            JOIN pg_namespace AS n ON n.oid = c.relnamespace
            JOIN expected_tables AS e ON e.name = c.relname
            WHERE n.nspname IN ('public', 'gateway', 'assistant', 'knowledge')
              AND c.relkind IN ('r', 'p')
        ), unique_tables AS (
            SELECT candidate.relname, candidate.oid
            FROM platform_relations AS candidate
            WHERE (
                SELECT count(*) FROM platform_relations AS sibling
                WHERE sibling.relname = candidate.relname
            ) = 1
        ), timestamp_columns(table_name, column_name) AS (
            VALUES
                ('usage_records', 'created_at'),
                ('usage_daily_aggregates', 'created_at'),
                ('usage_daily_aggregates', 'updated_at'),
                ('usage_hourly_aggregates', 'bucket_start'),
                ('usage_hourly_aggregates', 'created_at'),
                ('usage_hourly_aggregates', 'updated_at'),
                ('user_quotas', 'daily_reset_at'),
                ('user_quotas', 'monthly_reset_at'),
                ('user_quotas', 'blocked_at'),
                ('user_quotas', 'created_at'),
                ('user_quotas', 'updated_at'),
                ('model_pricing', 'effective_from'),
                ('model_pricing', 'effective_to'),
                ('model_pricing', 'created_at'),
                ('model_pricing', 'updated_at'),
                ('quota_alerts', 'acknowledged_at'),
                ('quota_alerts', 'created_at'),
                ('security_event_daily_aggregates', 'created_at'),
                ('security_event_daily_aggregates', 'updated_at')
        ), security_table AS (
            SELECT oid FROM unique_tables WHERE relname = 'security_event_daily_aggregates'
        ), security_dimensions AS (
            SELECT a.attname, a.attnotnull, t.typname,
                   pg_get_expr(d.adbin, d.adrelid) AS column_default
            FROM pg_attribute AS a
            JOIN pg_type AS t ON t.oid = a.atttypid
            LEFT JOIN pg_attrdef AS d ON d.adrelid = a.attrelid AND d.adnum = a.attnum
            WHERE a.attrelid = (SELECT oid FROM security_table)
              AND a.attname IN ('user_id', 'service_id')
              AND a.attnum > 0 AND NOT a.attisdropped
        ), forward_constraint AS (
            SELECT c.*, ARRAY(
                SELECT a.attname
                FROM unnest(c.conkey) WITH ORDINALITY AS key(attnum, position)
                JOIN pg_attribute AS a ON a.attrelid = c.conrelid AND a.attnum = key.attnum
                ORDER BY key.position
            ) AS columns
            FROM pg_constraint AS c
            WHERE c.conrelid = (SELECT oid FROM security_table)
              AND c.conname = 'uq_security_event_daily_dimensions'
        )
        SELECT
            (SELECT count(*) = 7 FROM unique_tables)
            AND NOT EXISTS (
                SELECT 1 FROM timestamp_columns AS expected
                LEFT JOIN unique_tables AS rel ON rel.relname = expected.table_name
                LEFT JOIN pg_attribute AS a
                  ON a.attrelid = rel.oid AND a.attname = expected.column_name
                 AND a.attnum > 0 AND NOT a.attisdropped
                LEFT JOIN pg_type AS t ON t.oid = a.atttypid
                WHERE a.attname IS NULL OR t.typname <> 'timestamptz'
            )
            AND (SELECT count(*) = 2 FROM security_dimensions)
            AND NOT EXISTS (
                SELECT 1 FROM security_dimensions
                WHERE typname <> 'varchar' OR NOT attnotnull
                   OR COALESCE(column_default, '') !~ '^''''::character varying$'
            )
            AND (SELECT count(*) = 1 FROM forward_constraint)
            AND EXISTS (
                SELECT 1 FROM forward_constraint
                WHERE contype = 'u' AND convalidated
                  AND columns = ARRAY[
                      'tenant_id', 'user_id', 'service_id', 'event_type', 'date'
                  ]::text[]
            )
            AND NOT EXISTS (
                SELECT 1 FROM pg_index AS i
                JOIN pg_class AS ic ON ic.oid = i.indexrelid
                WHERE i.indrelid = (SELECT oid FROM security_table)
                  AND ic.relname = 'idx_security_event_daily_unique'
            )
            AND NOT EXISTS (
                SELECT 1 FROM security_event_daily_aggregates
                WHERE user_id IS NULL OR service_id IS NULL
            )
    """,
    "030_rollback": r"""
        /* arc03-legacy-evidence:030-rollback */
        WITH expected_tables(name) AS (
            VALUES ('usage_records'), ('usage_daily_aggregates'),
                   ('usage_hourly_aggregates'), ('user_quotas'), ('model_pricing'),
                   ('quota_alerts'), ('security_event_daily_aggregates')
        ), platform_relations AS (
            SELECT c.oid, c.relname
            FROM pg_class AS c
            JOIN pg_namespace AS n ON n.oid = c.relnamespace
            JOIN expected_tables AS e ON e.name = c.relname
            WHERE n.nspname IN ('public', 'gateway', 'assistant', 'knowledge')
              AND c.relkind IN ('r', 'p')
        ), unique_tables AS (
            SELECT candidate.relname, candidate.oid
            FROM platform_relations AS candidate
            WHERE (
                SELECT count(*) FROM platform_relations AS sibling
                WHERE sibling.relname = candidate.relname
            ) = 1
        ), timestamp_columns(table_name, column_name) AS (
            VALUES
                ('usage_records', 'created_at'),
                ('usage_daily_aggregates', 'created_at'),
                ('usage_daily_aggregates', 'updated_at'),
                ('usage_hourly_aggregates', 'bucket_start'),
                ('usage_hourly_aggregates', 'created_at'),
                ('usage_hourly_aggregates', 'updated_at'),
                ('user_quotas', 'daily_reset_at'),
                ('user_quotas', 'monthly_reset_at'),
                ('user_quotas', 'blocked_at'),
                ('user_quotas', 'created_at'),
                ('user_quotas', 'updated_at'),
                ('model_pricing', 'effective_from'),
                ('model_pricing', 'effective_to'),
                ('model_pricing', 'created_at'),
                ('model_pricing', 'updated_at'),
                ('quota_alerts', 'acknowledged_at'),
                ('quota_alerts', 'created_at'),
                ('security_event_daily_aggregates', 'created_at'),
                ('security_event_daily_aggregates', 'updated_at')
        ), security_table AS (
            SELECT oid FROM unique_tables WHERE relname = 'security_event_daily_aggregates'
        ), security_dimensions AS (
            SELECT a.attname, a.attnotnull, t.typname,
                   pg_get_expr(d.adbin, d.adrelid) AS column_default
            FROM pg_attribute AS a
            JOIN pg_type AS t ON t.oid = a.atttypid
            LEFT JOIN pg_attrdef AS d ON d.adrelid = a.attrelid AND d.adnum = a.attnum
            WHERE a.attrelid = (SELECT oid FROM security_table)
              AND a.attname IN ('user_id', 'service_id')
              AND a.attnum > 0 AND NOT a.attisdropped
        ), rollback_index AS (
            SELECT i.*, pg_get_indexdef(i.indexrelid) AS definition
            FROM pg_index AS i
            JOIN pg_class AS ic ON ic.oid = i.indexrelid
            WHERE i.indrelid = (SELECT oid FROM security_table)
              AND ic.relname = 'idx_security_event_daily_unique'
        )
        SELECT
            (SELECT count(*) = 7 FROM unique_tables)
            AND NOT EXISTS (
                SELECT 1 FROM timestamp_columns AS expected
                LEFT JOIN unique_tables AS rel ON rel.relname = expected.table_name
                LEFT JOIN pg_attribute AS a
                  ON a.attrelid = rel.oid AND a.attname = expected.column_name
                 AND a.attnum > 0 AND NOT a.attisdropped
                LEFT JOIN pg_type AS t ON t.oid = a.atttypid
                WHERE a.attname IS NULL OR t.typname <> 'timestamp'
            )
            AND (SELECT count(*) = 2 FROM security_dimensions)
            AND NOT EXISTS (
                SELECT 1 FROM security_dimensions
                WHERE typname <> 'varchar' OR attnotnull OR column_default IS NOT NULL
            )
            AND NOT EXISTS (
                SELECT 1 FROM pg_constraint
                WHERE conrelid = (SELECT oid FROM security_table)
                  AND conname = 'uq_security_event_daily_dimensions'
            )
            AND (SELECT count(*) = 1 FROM rollback_index)
            AND EXISTS (
                SELECT 1 FROM rollback_index
                WHERE indisunique AND indisvalid AND indisready
                  AND definition ~ 'COALESCE\(user_id, ''''::character varying\)'
                  AND definition ~ 'COALESCE\(service_id, ''''::character varying\)'
            )
            AND NOT EXISTS (
                SELECT 1 FROM security_event_daily_aggregates
                WHERE user_id = '' OR service_id = ''
            )
    """,
    "031_hierarchy_effective": r"""
        /* arc03-legacy-evidence:031-hierarchy-effective */
        WITH platform_relations AS (
            SELECT c.oid, c.relname, c.relowner
            FROM pg_class AS c
            JOIN pg_namespace AS n ON n.oid = c.relnamespace
            WHERE n.nspname IN ('public', 'knowledge')
              AND c.relkind IN ('r', 'p')
              AND c.relname IN ('segments', 'documents', 'document_summaries')
        ), unique_tables AS (
            SELECT candidate.relname, candidate.oid, candidate.relowner
            FROM platform_relations AS candidate
            WHERE (
                SELECT count(*) FROM platform_relations AS sibling
                WHERE sibling.relname = candidate.relname
            ) = 1
        ), expected_columns(table_name, column_name, formatted_type, not_null, default_re) AS (
            VALUES
                ('segments', 'level', 'integer', FALSE, '^3$'),
                ('segments', 'parent_segment_id', 'character varying(255)', FALSE, NULL),
                ('segments', 'summary', 'text', FALSE, NULL),
                ('segments', 'page_start', 'integer', FALSE, NULL),
                ('segments', 'page_end', 'integer', FALSE, NULL),
                ('documents', 'detection_result', 'jsonb', FALSE, NULL),
                ('document_summaries', 'document_id', 'character varying(255)', TRUE, NULL),
                ('document_summaries', 'summary', 'text', TRUE, NULL),
                ('document_summaries', 'keywords', 'jsonb', FALSE, '^''\[\]''::jsonb$'),
                ('document_summaries', 'topics', 'jsonb', FALSE, '^''\[\]''::jsonb$'),
                ('document_summaries', 'vector_id', 'character varying(64)', FALSE, NULL),
                ('document_summaries', 'created_at', 'timestamp with time zone', FALSE,
                    '^now\(\)$'),
                ('document_summaries', 'updated_at', 'timestamp with time zone', FALSE,
                    '^now\(\)$')
        ), actual_columns AS (
            SELECT r.relname AS table_name, a.attname AS column_name,
                   format_type(a.atttypid, a.atttypmod) AS formatted_type,
                   a.attnotnull AS not_null,
                   pg_get_expr(d.adbin, d.adrelid) AS column_default
            FROM unique_tables AS r
            JOIN pg_attribute AS a ON a.attrelid = r.oid
            LEFT JOIN pg_attrdef AS d ON d.adrelid = r.oid AND d.adnum = a.attnum
            WHERE a.attnum > 0 AND NOT a.attisdropped
        ), column_mismatches AS (
            SELECT e.*
            FROM expected_columns AS e
            LEFT JOIN actual_columns AS a USING (table_name, column_name)
            WHERE a.column_name IS NULL OR a.formatted_type <> e.formatted_type
               OR a.not_null <> e.not_null
               OR (e.default_re IS NULL AND a.column_default IS NOT NULL)
               OR (e.default_re IS NOT NULL AND COALESCE(a.column_default, '') !~ e.default_re)
        ), segments_table AS (
            SELECT oid FROM unique_tables WHERE relname = 'segments'
        ), documents_table AS (
            SELECT oid FROM unique_tables WHERE relname = 'documents'
        ), summaries_table AS (
            SELECT oid, relowner FROM unique_tables WHERE relname = 'document_summaries'
        ), summary_functions AS (
            SELECT p.oid, p.proowner, p.prosecdef, p.proconfig
            FROM pg_proc AS p
            JOIN pg_namespace AS n ON n.oid = p.pronamespace
            WHERE n.nspname IN ('public', 'knowledge')
              AND p.proname = 'update_document_summaries_timestamp'
              AND p.pronargs = 0
        ), named_indexes AS (
            SELECT ic.relname AS name, i.indrelid, i.indisvalid, i.indisready,
                   i.indisunique,
                   am.amname, pg_get_indexdef(i.indexrelid) AS definition,
                   pg_get_expr(i.indpred, i.indrelid) AS predicate
            FROM pg_index AS i
            JOIN pg_class AS ic ON ic.oid = i.indexrelid
            JOIN pg_am AS am ON am.oid = ic.relam
            WHERE ic.relname IN (
                'idx_segments_level', 'idx_segments_parent', 'idx_segments_document_level',
                'idx_documents_detection_result', 'idx_document_summaries_keywords',
                'idx_document_summaries_vector_id'
            )
        )
        SELECT
            (SELECT count(*) = 3 FROM unique_tables)
            AND NOT EXISTS (SELECT 1 FROM column_mismatches)
            AND (SELECT count(*) = 7 FROM actual_columns
                 WHERE table_name = 'document_summaries')
            AND EXISTS (
                SELECT 1 FROM pg_constraint
                WHERE conrelid = (SELECT oid FROM segments_table)
                  AND conname = 'chk_segments_level' AND contype = 'c' AND convalidated
                  AND pg_get_constraintdef(oid) ~ 'level.*(1.*2.*3|ARRAY\[1, 2, 3\])'
            )
            AND EXISTS (
                SELECT 1 FROM pg_constraint
                WHERE conrelid = (SELECT oid FROM segments_table)
                  AND conname = 'chk_segments_page_range' AND contype = 'c' AND convalidated
                  AND pg_get_constraintdef(oid) ~ 'page_end.*page_start'
            )
            AND EXISTS (
                SELECT 1 FROM pg_constraint
                WHERE conrelid = (SELECT oid FROM segments_table)
                  AND confrelid = (SELECT oid FROM segments_table)
                  AND conname = 'fk_segments_parent' AND contype = 'f' AND convalidated
                  AND condeferrable AND condeferred AND confdeltype = 'c'
                  AND ARRAY(
                      SELECT a.attname
                      FROM unnest(conkey) WITH ORDINALITY AS key(attnum, position)
                      JOIN pg_attribute AS a
                        ON a.attrelid = conrelid AND a.attnum = key.attnum
                      ORDER BY key.position
                  ) = ARRAY['parent_segment_id']::text[]
                  AND ARRAY(
                      SELECT a.attname
                      FROM unnest(confkey) WITH ORDINALITY AS key(attnum, position)
                      JOIN pg_attribute AS a
                        ON a.attrelid = confrelid AND a.attnum = key.attnum
                      ORDER BY key.position
                  ) = ARRAY['segment_id']::text[]
            )
            AND EXISTS (
                SELECT 1 FROM pg_constraint
                WHERE conrelid = (SELECT oid FROM summaries_table)
                  AND contype = 'p' AND convalidated
                  AND pg_get_constraintdef(oid) = 'PRIMARY KEY (document_id)'
            )
            AND EXISTS (
                SELECT 1 FROM pg_constraint
                WHERE conrelid = (SELECT oid FROM summaries_table)
                  AND confrelid = (SELECT oid FROM documents_table)
                  AND contype = 'f' AND convalidated AND confdeltype = 'c'
                  AND ARRAY(
                      SELECT a.attname
                      FROM unnest(conkey) WITH ORDINALITY AS key(attnum, position)
                      JOIN pg_attribute AS a
                        ON a.attrelid = conrelid AND a.attnum = key.attnum
                      ORDER BY key.position
                  ) = ARRAY['document_id']::text[]
                  AND ARRAY(
                      SELECT a.attname
                      FROM unnest(confkey) WITH ORDINALITY AS key(attnum, position)
                      JOIN pg_attribute AS a
                        ON a.attrelid = confrelid AND a.attnum = key.attnum
                      ORDER BY key.position
                  ) = ARRAY['document_id']::text[]
            )
            AND (SELECT count(*) = 1 FROM summary_functions)
            AND EXISTS (
                SELECT 1 FROM summary_functions
                WHERE NOT prosecdef
                  AND proowner = (SELECT relowner FROM summaries_table)
                  AND proconfig @> ARRAY['search_path=public']::text[]
            )
            AND EXISTS (
                SELECT 1 FROM pg_trigger
                WHERE tgrelid = (SELECT oid FROM summaries_table)
                  AND tgname = 'trigger_document_summaries_updated_at'
                  AND NOT tgisinternal AND tgenabled <> 'D' AND tgtype = 19
                  AND tgfoid = (SELECT oid FROM summary_functions)
            )
            AND (SELECT count(*) = 6 FROM named_indexes)
            AND NOT EXISTS (
                SELECT 1 FROM named_indexes
                WHERE NOT indisvalid OR NOT indisready OR indisunique
                   OR (name IN ('idx_documents_detection_result',
                                'idx_document_summaries_keywords') AND amname <> 'gin')
                   OR (name NOT IN ('idx_documents_detection_result',
                                    'idx_document_summaries_keywords') AND amname <> 'btree')
                   OR (name LIKE 'idx_segments_%'
                       AND indrelid <> (SELECT oid FROM segments_table))
                   OR (name = 'idx_documents_detection_result'
                       AND indrelid <> (SELECT oid FROM documents_table))
                   OR (name LIKE 'idx_document_summaries_%'
                       AND indrelid <> (SELECT oid FROM summaries_table))
            )
            AND EXISTS (
                SELECT 1 FROM named_indexes
                WHERE name = 'idx_segments_level'
                  AND definition ~ '\(dataset_id, level\)$' AND predicate IS NULL
            )
            AND EXISTS (
                SELECT 1 FROM named_indexes
                WHERE name = 'idx_segments_parent'
                  AND definition ~ '\(parent_segment_id\) WHERE \(parent_segment_id IS NOT NULL\)$'
            )
            AND EXISTS (
                SELECT 1 FROM named_indexes
                WHERE name = 'idx_segments_document_level'
                  AND definition ~ '\(document_id, level\)$' AND predicate IS NULL
            )
            AND EXISTS (
                SELECT 1 FROM named_indexes
                WHERE name = 'idx_documents_detection_result'
                  AND definition ~ 'USING gin \(detection_result\)'
                  AND predicate = '(detection_result IS NOT NULL)'
            )
            AND EXISTS (
                SELECT 1 FROM named_indexes
                WHERE name = 'idx_document_summaries_keywords'
                  AND definition ~ 'USING gin \(keywords\)$' AND predicate IS NULL
            )
            AND EXISTS (
                SELECT 1 FROM named_indexes
                WHERE name = 'idx_document_summaries_vector_id'
                  AND definition ~ '\(vector_id\) WHERE \(vector_id IS NOT NULL\)$'
            )
            AND NOT EXISTS (
                SELECT 1 FROM segments AS child
                LEFT JOIN segments AS parent ON parent.segment_id = child.parent_segment_id
                WHERE child.parent_segment_id IS NOT NULL AND parent.segment_id IS NULL
            )
    """,
}


def _legacy_description(filename: str) -> str:
    match = MIGRATION_PATTERN.fullmatch(filename)
    if match is None:
        return ""
    return match.group(2).replace("_", " ").title()


async def _numeric_ledger_records(conn: Any) -> list[NumericLedgerRecord]:
    columns = await conn.fetch(
        """
        SELECT column_name
        FROM information_schema.columns
        WHERE table_schema = 'public' AND table_name = 'schema_migrations'
        """
    )
    names = {str(row["column_name"]) for row in columns}
    if "version" not in names:
        return []
    name_expr = "name::text" if "name" in names else "NULL::text"
    checksum_expr = "checksum::text" if "checksum" in names else "NULL::text"
    dirty_expr = "dirty" if "dirty" in names else "FALSE"
    rows = await conn.fetch(
        f"""
        SELECT version::text AS version, {name_expr} AS name,
               {checksum_expr} AS checksum, {dirty_expr} AS dirty
        FROM public.schema_migrations
        ORDER BY version::integer
        """
    )
    records: list[NumericLedgerRecord] = []
    for row in rows:
        try:
            version = f"{int(row['version']):03d}"
        except (TypeError, ValueError) as exc:
            raise AuthorityBlockedError(
                f"numeric legacy ledger contains invalid version {row['version']!r}"
            ) from exc
        records.append(
            NumericLedgerRecord(
                version=version,
                name=str(row["name"]).strip() if row["name"] is not None else None,
                checksum=(
                    str(row["checksum"]).strip().lower() if row["checksum"] is not None else None
                ),
                dirty=bool(row["dirty"]),
            )
        )
    return records


def _match_numeric_identity(
    record: NumericLedgerRecord,
    identities: dict[str, tuple[str, str]],
    ordered_identities: tuple[str, ...],
) -> tuple[str | None, str | None, str | None]:
    """Resolve a row by exact filename/description and/or known digest."""
    name_matches: set[str] | None = None
    checksum_matches: set[str] | None = None
    if record.name:
        name_matches = {
            filename
            for filename in identities
            if record.name in {filename, _legacy_description(filename)}
        }
        if not name_matches:
            return None, f"unknown legacy migration name {record.name!r}", None
    if record.checksum:
        if len(record.checksum) not in (16, 64) or not re.fullmatch(r"[0-9a-f]+", record.checksum):
            return None, f"invalid legacy checksum shape {record.checksum!r}", None
        checksum_matches = {
            filename
            for filename, (full_sha, short_sha) in identities.items()
            if record.checksum in {full_sha, short_sha}
        }
        if not checksum_matches:
            return (
                None,
                f"checksum {record.checksum!r} matches no immutable legacy file",
                None,
            )
    if name_matches is None and checksum_matches is None:
        return (
            None,
            "bare numeric row has neither a migration name nor a known checksum",
            None,
        )

    matches = name_matches if checksum_matches is None else checksum_matches
    if name_matches is not None and checksum_matches is not None:
        matches = name_matches & checksum_matches
        if not matches:
            # The old runner retained the first sibling name on conflict but
            # updated the checksum.  Only first-name + last-checksum proves the
            # exact historical ordered batch; every other mixture is blocked.
            if name_matches == {ordered_identities[0]} and checksum_matches == {
                ordered_identities[-1]
            }:
                return ordered_identities[-1], None, "historical_name_checksum_overwrite"
            return (
                None,
                "legacy migration name and checksum identify an unknown mixed state",
                None,
            )
    assert matches is not None
    if len(matches) != 1:
        return None, f"legacy identity is ambiguous across {sorted(matches)}", None
    return next(iter(matches)), None, "exact_name_or_checksum"


async def _numeric_evidence(conn: Any, key: str) -> dict[str, Any]:
    try:
        passed = bool(await conn.fetchval(NUMERIC_EVIDENCE_SQL[key]))
    except Exception as exc:  # noqa: BLE001 - receipt preserves a fail-closed error code
        return {"check": key, "passed": False, "error_code": type(exc).__name__}
    return {"check": key, "passed": passed}
