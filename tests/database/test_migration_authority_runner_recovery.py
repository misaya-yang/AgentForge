from __future__ import annotations

import hashlib
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any

import pytest

from database.authority import adoption, bootstrap, ledger, runner
from database.authority.manifest import (
    BaselineManifest,
    ChangeSpec,
    EpochManifest,
    RollbackClass,
    TransactionMode,
)
from database.authority.runner import AuthorityBlockedError, AuthorityError, AuthorityPaths


def _baseline() -> BaselineManifest:
    return BaselineManifest(
        baseline_id="2026_08_post_kb_v1",
        schema_revision="112",
        source_git_sha="a" * 40,
        last_legacy_change="112_last.sql",
        structural_sha256="1" * 64,
        acl_sha256="2" * 64,
        extensions_sha256="3" * 64,
        reference_data_sha256="4" * 64,
        generator="test",
        generated_at="2026-08-30T00:00:00Z",
        postgres_version="16",
    )


def _marker(baseline: BaselineManifest, manifest_sha: str = "5" * 64) -> dict[str, str]:
    return {
        "baseline_id": baseline.baseline_id,
        "manifest_sha256": manifest_sha,
        "structural_sha256": baseline.structural_sha256,
        "acl_sha256": baseline.acl_sha256,
        "extensions_sha256": baseline.extensions_sha256,
        "reference_data_sha256": baseline.reference_data_sha256,
        "source_git_sha": baseline.source_git_sha,
    }


class AdoptionConnection:
    def __init__(self, rows: list[dict[str, str]]) -> None:
        self.rows = rows
        self.executed: list[tuple[str, tuple[Any, ...]]] = []

    async def fetch(self, _query: str) -> list[dict[str, str]]:
        return self.rows

    async def execute(self, query: str, *args: Any) -> None:
        self.executed.append((query, args))
        if query == ledger.INSERT_BASELINE_MARKER and not self.rows:
            self.rows = [
                {
                    "baseline_id": str(args[0]),
                    "manifest_sha256": str(args[1]),
                    "structural_sha256": str(args[2]),
                    "acl_sha256": str(args[3]),
                    "extensions_sha256": str(args[4]),
                    "reference_data_sha256": str(args[5]),
                    "source_git_sha": str(args[6]),
                }
            ]


@pytest.mark.parametrize(
    "field",
    [
        "baseline_id",
        "manifest_sha256",
        "structural_sha256",
        "acl_sha256",
        "extensions_sha256",
        "reference_data_sha256",
        "source_git_sha",
    ],
)
async def test_direct_adoption_rejects_every_existing_marker_drift(field: str) -> None:
    baseline = _baseline()
    marker = _marker(baseline)
    marker[field] = "drift"
    conn = AdoptionConnection([marker])

    with pytest.raises(AuthorityError, match=field):
        await adoption.adopt_baseline(conn, baseline, manifest_sha256="5" * 64)

    assert conn.executed == []


async def test_direct_adoption_accepts_only_one_exact_existing_marker() -> None:
    baseline = _baseline()
    conn = AdoptionConnection([_marker(baseline)])

    result = await adoption.adopt_baseline(conn, baseline, manifest_sha256="5" * 64)

    assert result == {"already_adopted": baseline.baseline_id}
    assert conn.executed == []


async def test_direct_adoption_rechecks_marker_after_conflicting_insert(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    baseline = _baseline()

    class ConflictingInsertConnection(AdoptionConnection):
        async def execute(self, query: str, *args: Any) -> None:
            self.executed.append((query, args))
            marker = _marker(baseline)
            marker["source_git_sha"] = "concurrent-writer"
            self.rows = [marker]

    async def matching_fingerprints(*_args: Any, **_kwargs: Any) -> dict[str, str]:
        return baseline.fingerprints

    monkeypatch.setattr(adoption, "compute_fingerprints", matching_fingerprints)
    conn = ConflictingInsertConnection([])

    with pytest.raises(AuthorityError, match="source_git_sha"):
        await adoption.adopt_baseline(conn, baseline, manifest_sha256="5" * 64)


class FakeTransaction:
    def __init__(self, conn: BootstrapConnection | RunnerConnection) -> None:
        self.conn = conn
        self.snapshot: tuple[list[Any], set[str], dict[str, str] | None] | None = None

    async def __aenter__(self) -> FakeTransaction:
        marker = getattr(self.conn, "baseline_marker", None)
        self.snapshot = (
            list(self.conn.executed),
            set(self.conn.objects),
            dict(marker) if marker is not None else None,
        )
        self.conn.transaction_depth += 1
        return self

    async def __aexit__(self, _exc_type: Any, _exc: Any, _tb: Any) -> bool:
        self.conn.transaction_depth -= 1
        if _exc_type is not None and self.snapshot is not None:
            self.conn.executed[:] = self.snapshot[0]
            self.conn.objects.clear()
            self.conn.objects.update(self.snapshot[1])
            if hasattr(self.conn, "baseline_marker"):
                self.conn.baseline_marker = self.snapshot[2]
        return False


class BootstrapConnection:
    def __init__(self) -> None:
        self.executed: list[tuple[str, tuple[Any, ...], int]] = []
        self.objects: set[str] = set()
        self.transaction_depth = 0
        self.fail_reference_once = False
        self.verify_ok = True
        self.baseline_marker: dict[str, str] | None = None
        # Cluster roles are provisioned by a separate admin before schema init.
        self.roles_ready = True

    def transaction(self) -> FakeTransaction:
        return FakeTransaction(self)

    async def fetchval(self, query: str, *args: Any) -> int:
        if "arc03-role-bootstrap-admin" in query:
            return 1
        if "to_regclass($1)" in query:
            return int(str(args[0]).removeprefix("public.") in self.objects)
        if "SELECT sum(object_count)" in query:
            return len(self.objects)
        raise AssertionError(f"unexpected bootstrap fetchval: {query}")

    async def fetch(self, query: str, *args: Any) -> list[dict[str, Any]]:
        if "VERIFY_BASELINE" in query:
            return [{"check_name": "baseline_shape", "ok": self.verify_ok}]
        if "arc03-role-bootstrap-state" in query:
            if not self.roles_ready:
                return []
            names = list(args[0])
            owner = next(name for name in names if name.endswith("owner"))
            return [
                {
                    "rolname": name,
                    "rolcanlogin": not name.endswith("owner"),
                    "rolinherit": False,
                    "rolsuper": False,
                    "rolcreatedb": False,
                    "rolcreaterole": False,
                    "rolreplication": False,
                    "rolbypassrls": False,
                    "rolconfig": [
                        "search_path=pg_catalog, knowledge, gateway, assistant, public"
                        if name.endswith(("knowledge_api", "knowledge_worker"))
                        else "search_path=pg_catalog, gateway, assistant, knowledge, public"
                    ],
                    "memberships": [owner] if name.endswith("migrator") else [],
                }
                for name in names
            ]
        if "arc03-schema-bootstrap-state" in query:
            if not self.roles_ready:
                return []
            names = list(args[1])
            owner = next(name for name in names if name.endswith("owner"))
            return [
                {
                    "nspname": schema,
                    "owner": owner,
                    "public_create": False,
                    "create_roles": [owner],
                }
                for schema in args[0]
            ]
        if "arc03-default-acl-bootstrap-state" in query:
            if not self.roles_ready:
                return []
            return [
                {
                    "nspname": schema,
                    "objtype": object_type,
                    "public_has_privilege": False,
                }
                for schema in args[1]
                for object_type in ("r", "S", "f", "T")
            ]
        if query == ledger.SELECT_APPLIED_CHANGES:
            return []
        if query == ledger.SELECT_BASELINE:
            return [self.baseline_marker] if self.baseline_marker is not None else []
        raise AssertionError(f"unexpected bootstrap fetch: {query}")

    async def fetchrow(self, query: str, *args: Any) -> dict[str, Any] | None:
        if "arc03-database-bootstrap-state" in query:
            names = list(args[0])
            owner = next(name for name in names if name.endswith("owner"))
            return {"public_create": False, "create_roles": [owner]}
        raise AssertionError(f"unexpected bootstrap fetchrow: {query}")

    async def execute(self, query: str, *args: Any) -> None:
        if self.fail_reference_once and "REFERENCE_DATA" in query:
            self.fail_reference_once = False
            raise RuntimeError("reference load interrupted")
        self.executed.append((query, args, self.transaction_depth))
        if query == ledger.INSERT_BASELINE_MARKER:
            self.baseline_marker = {
                "baseline_id": str(args[0]),
                "manifest_sha256": str(args[1]),
                "structural_sha256": str(args[2]),
                "acl_sha256": str(args[3]),
                "extensions_sha256": str(args[4]),
                "reference_data_sha256": str(args[5]),
                "source_git_sha": str(args[6]),
            }
        for marker in (
            "ROLES",
            "EXTENSIONS",
            "INIT_SCHEMA",
            "REFERENCE_DATA",
            "GRANTS",
            "platform_schema_baselines",
            "platform_schema_changes",
            "platform_schema_change_attempts",
        ):
            if marker in query:
                self.objects.add(marker)


def _write_bootstrap_files(paths: AuthorityPaths, baseline: BaselineManifest) -> None:
    paths.bootstrap_dir.mkdir(parents=True)
    paths.baseline_dir(baseline.baseline_id).mkdir(parents=True)
    (paths.bootstrap_dir / "roles.sql").write_text("-- ROLES\n", encoding="utf-8")
    (paths.bootstrap_dir / "extensions.sql").write_text("-- EXTENSIONS\n", encoding="utf-8")
    baseline_dir = paths.baseline_dir(baseline.baseline_id)
    (baseline_dir / "init.sql").write_text("-- INIT_SCHEMA\n", encoding="utf-8")
    (baseline_dir / "reference_data.sql").write_text("-- REFERENCE_DATA\n", encoding="utf-8")
    (baseline_dir / "grants.sql").write_text("-- GRANTS ai_gateway_\n", encoding="utf-8")
    (baseline_dir / "verify.sql").write_text(
        "-- VERIFY_BASELINE\nSELECT 'baseline_shape' AS check_name, TRUE AS ok;\n",
        encoding="utf-8",
    )


async def test_fresh_install_rolls_back_partial_init_and_is_reentrant(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    baseline = _baseline()
    paths = AuthorityPaths(tmp_path / "database")
    _write_bootstrap_files(paths, baseline)
    conn = BootstrapConnection()
    conn.fail_reference_once = True

    async def matching_fingerprints(*_args: Any, **_kwargs: Any) -> dict[str, str]:
        return baseline.fingerprints

    monkeypatch.setattr(bootstrap, "compute_fingerprints", matching_fingerprints)

    with pytest.raises(RuntimeError, match="reference load interrupted"):
        await bootstrap.fresh_install(conn, paths, baseline, "5" * 64)

    assert conn.objects == set()
    assert conn.executed == []

    result = await bootstrap.fresh_install(conn, paths, baseline, "5" * 64)

    assert result == baseline.fingerprints
    assert any(query == ledger.LEDGER_DDL for query, _args, _depth in conn.executed)
    assert any(query == ledger.INSERT_BASELINE_MARKER for query, _args, _depth in conn.executed)
    assert not any(query == ledger.INSERT_ATTEMPT for query, _args, _depth in conn.executed)
    assert all(depth > 0 for _query, _args, depth in conn.executed)
    statements = [query for query, _args, _depth in conn.executed]
    extension_index = next(index for index, query in enumerate(statements) if "EXTENSIONS" in query)
    assert statements[extension_index - 1] == 'SET LOCAL ROLE "ai_gateway_owner"'
    assert statements[extension_index + 1] == "RESET ROLE"

    executed_schema_statements = [
        statement for statement in conn.executed if not statement[0].startswith("SET LOCAL ROLE")
    ]
    rerun = await bootstrap.fresh_install(conn, paths, baseline, "5" * 64)
    assert rerun == baseline.fingerprints
    assert [
        statement for statement in conn.executed if not statement[0].startswith("SET LOCAL ROLE")
    ] == executed_schema_statements


async def test_fresh_install_wrong_nonempty_database_fails_before_sql(
    tmp_path: Path,
) -> None:
    baseline = _baseline()
    paths = AuthorityPaths(tmp_path / "database")
    _write_bootstrap_files(paths, baseline)
    conn = BootstrapConnection()
    conn.objects.add("FOREIGN_OBJECT")

    with pytest.raises(AuthorityError, match="database is not empty"):
        await bootstrap.fresh_install(conn, paths, baseline, "5" * 64)

    assert conn.executed == []
    assert conn.objects == {"FOREIGN_OBJECT"}


async def test_fresh_install_failed_verify_rolls_back_before_marker(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    baseline = _baseline()
    paths = AuthorityPaths(tmp_path / "database")
    _write_bootstrap_files(paths, baseline)
    conn = BootstrapConnection()
    conn.verify_ok = False

    async def matching_fingerprints(*_args: Any, **_kwargs: Any) -> dict[str, str]:
        return baseline.fingerprints

    monkeypatch.setattr(bootstrap, "compute_fingerprints", matching_fingerprints)

    with pytest.raises(AuthorityError, match="failed or returned malformed"):
        await bootstrap.fresh_install(conn, paths, baseline, "5" * 64)

    assert conn.baseline_marker is None
    assert conn.objects == set()


async def test_fresh_install_existing_marker_requires_complete_ledger(
    tmp_path: Path,
) -> None:
    baseline = _baseline()
    paths = AuthorityPaths(tmp_path / "database")
    _write_bootstrap_files(paths, baseline)
    conn = BootstrapConnection()
    conn.objects.add(ledger.BASELINES_TABLE)
    conn.baseline_marker = _marker(baseline)

    with pytest.raises(AuthorityError, match="ledger is incomplete"):
        await bootstrap.fresh_install(conn, paths, baseline, "5" * 64)

    assert conn.executed == []


class RunnerConnection:
    def __init__(
        self,
        *,
        latest_attempt: dict[str, Any] | None = None,
        conditions: dict[str, Any] | None = None,
    ) -> None:
        self.latest_attempt = latest_attempt
        self.conditions = conditions or {}
        self.executed: list[tuple[str, tuple[Any, ...], int]] = []
        self.fetches: list[tuple[str, tuple[Any, ...]]] = []
        self.transaction_depth = 0
        self.objects: set[str] = set()
        self.lose_fence_for: str | None = None

    def transaction(self) -> FakeTransaction:
        return FakeTransaction(self)

    async def fetch(self, query: str, *args: Any) -> list[dict[str, Any]]:
        self.fetches.append((query, args))
        if query == runner._SELECT_LATEST_ATTEMPT:
            return [self.latest_attempt] if self.latest_attempt is not None else []
        raise AssertionError(f"unexpected runner fetch: {query}")

    async def fetchval(self, query: str, *args: Any) -> Any:
        self.fetches.append((query, args))
        if query in self.conditions:
            return self.conditions[query]
        action = {
            runner._TRANSITION_EXPIRED_ATTEMPT: "transition",
            runner._CLAIM_ATTEMPT: "claim",
            runner._UPDATE_ATTEMPT_CHECKPOINT_FENCED: "checkpoint",
            runner._UPDATE_ATTEMPT_TERMINAL_FENCED: "terminal",
        }.get(query)
        if action is None:
            raise AssertionError(f"unexpected runner fetchval: {query}")
        if self.lose_fence_for == action:
            return None
        expected_fence = int(args[2])
        return expected_fence if action == "transition" else expected_fence + 1

    async def execute(self, query: str, *args: Any) -> None:
        self.executed.append((query, args, self.transaction_depth))


def _write_change(
    tmp_path: Path,
    sql: str,
    *,
    mode: TransactionMode,
    resume_handler: str | None = None,
    repair_handler: str | None = None,
    postconditions: tuple[str, ...] = (),
) -> tuple[ChangeSpec, Path]:
    epoch_dir = tmp_path / "epoch"
    epoch_dir.mkdir()
    path = epoch_dir / "001_change.sql"
    path.write_text(sql, encoding="utf-8")
    spec = ChangeSpec(
        sequence=1,
        name="change",
        file=path.name,
        sha256=hashlib.sha256(sql.encode()).hexdigest(),
        owner="migrator",
        transaction_mode=mode,
        rollback_class=RollbackClass.OLD_BINARY_COMPATIBLE,
        postconditions=postconditions,
        resume_handler=resume_handler,
        repair_handler=repair_handler,
    )
    return spec, epoch_dir


def _authority(
    tmp_path: Path,
    *,
    handlers: dict[str, runner.RecoveryHandler] | None = None,
) -> runner.MigrationAuthority:
    return runner.MigrationAuthority(
        "postgresql://unused",
        AuthorityPaths(tmp_path / "database"),
        asyncpg_module=object(),
        recovery_handlers=handlers,
    )


async def _noop_recovery_handler(_conn: Any, _context: runner.RecoveryContext) -> None:
    return None


def _attempt(
    spec: ChangeSpec,
    *,
    state: str,
    checkpoint: str,
    expired: bool = True,
) -> dict[str, Any]:
    age = runner.ATTEMPT_LEASE_TIMEOUT + timedelta(seconds=1) if expired else timedelta()
    return {
        "attempt_id": "001-attempt",
        "baseline_id": "baseline",
        "sequence": spec.sequence,
        "checksum_sha256": spec.sha256,
        "runner_digest": runner.RUNNER_DIGEST,
        "phase": "apply",
        "checkpoint": checkpoint,
        "lease_owner": "old-lease",
        "fence_generation": 2,
        "state": state,
        "started_at": datetime.now(timezone.utc) - age,
    }


def test_runner_digest_binds_the_executable_state_machine_source() -> None:
    assert hashlib.sha256(Path(runner.__file__).read_bytes()).hexdigest() == runner.RUNNER_DIGEST


async def test_transactional_ddl_and_success_ledger_share_one_transaction(
    tmp_path: Path,
) -> None:
    sql = "CREATE TABLE transactional_test(id integer);"
    spec, epoch_dir = _write_change(tmp_path, sql, mode=TransactionMode.TRANSACTIONAL)
    conn = RunnerConnection()

    await _authority(tmp_path).apply_change_transactional(conn, "baseline", spec, epoch_dir)

    ddl = next(item for item in conn.executed if item[0] == sql)
    success = next(item for item in conn.executed if item[0] == ledger.INSERT_CHANGE_SUCCESS)
    role = next(item for item in conn.executed if item[0] == 'SET LOCAL ROLE "ai_gateway_migrator"')
    assert ddl[2] == success[2] == 1
    assert conn.executed.index(role) < conn.executed.index(ddl) < conn.executed.index(success)


def test_checkpoint_parser_rejects_duplicate_reserved_and_empty_segments() -> None:
    with pytest.raises(AuthorityError, match="duplicate/reserved"):
        runner.MigrationAuthority._split_checkpoints(
            "SELECT 1;\n-- @checkpoint same\nSELECT 2;\n-- @checkpoint same\nSELECT 3;"
        )
    with pytest.raises(AuthorityError, match="duplicate/reserved"):
        runner.MigrationAuthority._split_checkpoints("-- @checkpoint __preamble__\nSELECT 1;")
    with pytest.raises(AuthorityError, match="has no SQL segment"):
        runner.MigrationAuthority._split_checkpoints("-- @checkpoint final\n")


async def test_nontransactional_change_requires_executable_recovery_contract(
    tmp_path: Path,
) -> None:
    sql = "SELECT 1;"
    spec, epoch_dir = _write_change(
        tmp_path,
        sql,
        mode=TransactionMode.NON_TRANSACTIONAL,
        resume_handler="resume",
    )

    with pytest.raises(AuthorityBlockedError, match="no registered recovery"):
        await _authority(tmp_path).apply_change_non_transactional(
            RunnerConnection(), "baseline", spec, epoch_dir
        )


async def test_nontransactional_change_rejects_embedded_transaction(
    tmp_path: Path,
) -> None:
    sql = "BEGIN;\nSELECT 1;\nCOMMIT;"
    spec, epoch_dir = _write_change(
        tmp_path,
        sql,
        mode=TransactionMode.NON_TRANSACTIONAL,
        repair_handler="repair",
    )

    with pytest.raises(AuthorityError, match="transaction control"):
        await _authority(
            tmp_path, handlers={"repair": _noop_recovery_handler}
        ).apply_change_non_transactional(RunnerConnection(), "baseline", spec, epoch_dir)


@pytest.mark.parametrize(
    "sql",
    [
        "DO $$\nBEGIN\n  PERFORM 1;\nEND\n$$;",
        "DO $body$\nBEGIN\n  PERFORM 1;\nEND\n$body$;",
        "SELECT CASE WHEN TRUE THEN 1 ELSE 0 END;",
        "/*\nBEGIN;\nCOMMIT;\n*/\nSELECT 1;",
    ],
)
def test_transaction_control_parser_ignores_sql_bodies_and_expressions(sql: str) -> None:
    runner.MigrationAuthority._reject_embedded_transactions(sql, "probe")


@pytest.mark.parametrize(
    "sql",
    [
        "SELECT '$$'; BEGIN; SELECT '$$';",
        "-- $$ false opener\nBEGIN;\n-- $$ false closer\nSELECT 1;",
        "SELECT '$tag$'; COMMIT; SELECT '$tag$';",
    ],
)
def test_quote_like_tokens_in_comments_or_strings_cannot_hide_transaction_control(
    sql: str,
) -> None:
    with pytest.raises(AuthorityError, match="transaction control"):
        runner.MigrationAuthority._reject_embedded_transactions(sql, "probe")


@pytest.mark.parametrize(
    "sql",
    [
        "DO $$ BEGIN PERFORM 1; END;",
        "SELECT 'unterminated",
        "/* unterminated",
    ],
)
def test_transaction_parser_rejects_unterminated_lexical_bodies(sql: str) -> None:
    with pytest.raises(AuthorityError, match="unterminated"):
        runner.MigrationAuthority._reject_embedded_transactions(sql, "probe")


@pytest.mark.parametrize(
    "sql",
    [
        "BEGIN; SELECT 1; COMMIT;",
        "START TRANSACTION ISOLATION LEVEL SERIALIZABLE; SELECT 1;",
        "SAVEPOINT unsafe; SELECT 1;",
        "RELEASE SAVEPOINT unsafe; SELECT 1;",
        "ROLLBACK TO SAVEPOINT unsafe;",
        "END WORK AND NO CHAIN;",
        "PREPARE TRANSACTION 'unsafe';",
        "SET TRANSACTION ISOLATION LEVEL SERIALIZABLE;",
        "SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY;",
    ],
)
def test_transaction_control_parser_rejects_top_level_controls(sql: str) -> None:
    with pytest.raises(AuthorityError, match="transaction control"):
        runner.MigrationAuthority._reject_embedded_transactions(sql, "probe")


def test_authority_constructor_rejects_unsafe_role_prefix(tmp_path: Path) -> None:
    with pytest.raises(AuthorityError, match="unsafe role prefix"):
        runner.MigrationAuthority(
            "postgresql://unused",
            AuthorityPaths(tmp_path / "database"),
            role_prefix='owner"; DROP SCHEMA public; --',
            asyncpg_module=object(),
        )


async def test_active_nontransactional_attempt_cannot_be_stolen(tmp_path: Path) -> None:
    sql = "SELECT 1;"
    spec, epoch_dir = _write_change(
        tmp_path,
        sql,
        mode=TransactionMode.NON_TRANSACTIONAL,
        resume_handler="resume",
    )
    conn = RunnerConnection(
        latest_attempt=_attempt(
            spec, state=ledger.ATTEMPT_STATE_RUNNING, checkpoint="", expired=False
        )
    )

    with pytest.raises(AuthorityBlockedError, match="active lease"):
        await _authority(
            tmp_path, handlers={"resume": _noop_recovery_handler}
        ).apply_change_non_transactional(conn, "baseline", spec, epoch_dir)

    select_query = conn.fetches[0][0]
    assert "ORDER BY started_at DESC, attempt_id DESC" in select_query
    assert "LIMIT 1" in select_query


async def test_succeeded_attempt_without_success_ledger_fails_closed(tmp_path: Path) -> None:
    sql = "SELECT 1;"
    spec, epoch_dir = _write_change(
        tmp_path,
        sql,
        mode=TransactionMode.NON_TRANSACTIONAL,
        repair_handler="repair",
    )
    conn = RunnerConnection(
        latest_attempt=_attempt(
            spec,
            state=ledger.ATTEMPT_STATE_SUCCEEDED,
            checkpoint=runner._PREAMBLE_CHECKPOINT,
        )
    )

    with pytest.raises(AuthorityBlockedError, match="unsupported state 'succeeded'"):
        await _authority(
            tmp_path, handlers={"repair": _noop_recovery_handler}
        ).apply_change_non_transactional(conn, "baseline", spec, epoch_dir)

    assert "state <>" not in conn.fetches[0][0]
    assert conn.executed == []


async def test_expired_attempt_calls_resume_handler_and_starts_after_checkpoint(
    tmp_path: Path,
) -> None:
    sql = "SEGMENT_ONE;\n-- @checkpoint second\nSEGMENT_TWO;"
    spec, epoch_dir = _write_change(
        tmp_path,
        sql,
        mode=TransactionMode.NON_TRANSACTIONAL,
        resume_handler="resume",
    )
    calls: list[runner.RecoveryContext] = []

    async def resume_handler(conn: Any, context: runner.RecoveryContext) -> None:
        assert isinstance(conn, RunnerConnection)
        calls.append(context)

    conn = RunnerConnection(
        latest_attempt=_attempt(
            spec,
            state=ledger.ATTEMPT_STATE_RUNNING,
            checkpoint=runner._PREAMBLE_CHECKPOINT,
        )
    )
    authority = _authority(tmp_path, handlers={"resume": resume_handler})

    await authority.apply_change_non_transactional(conn, "baseline", spec, epoch_dir)

    assert [context.checkpoint for context in calls] == [runner._PREAMBLE_CHECKPOINT]
    executed_sql = [query for query, _args, _depth in conn.executed]
    assert "SET statement_timeout = 300000" in executed_sql
    assert "SET lock_timeout = 30000" in executed_sql
    assert "RESET statement_timeout" in executed_sql
    assert "RESET lock_timeout" in executed_sql
    assert not any("SEGMENT_ONE" in query for query in executed_sql)
    assert any("SEGMENT_TWO" in query for query in executed_sql)
    assert 'SET ROLE "ai_gateway_migrator"' in executed_sql
    transition = next(
        args for query, args in conn.fetches if query == runner._TRANSITION_EXPIRED_ATTEMPT
    )
    assert transition[3] == ledger.ATTEMPT_STATE_RESUMABLE
    assert any(query == ledger.INSERT_CHANGE_SUCCESS for query in executed_sql)


async def test_expired_attempt_without_resume_handler_transitions_failed_and_repairs(
    tmp_path: Path,
) -> None:
    sql = "SEGMENT_ONE;\n-- @checkpoint second\nSEGMENT_TWO;"
    spec, epoch_dir = _write_change(
        tmp_path,
        sql,
        mode=TransactionMode.NON_TRANSACTIONAL,
        repair_handler="repair",
    )
    calls: list[str] = []

    async def repair_handler(conn: Any, context: runner.RecoveryContext) -> None:
        assert isinstance(conn, RunnerConnection)
        calls.append(context.prior_state)

    conn = RunnerConnection(
        latest_attempt=_attempt(
            spec,
            state=ledger.ATTEMPT_STATE_RUNNING,
            checkpoint=runner._PREAMBLE_CHECKPOINT,
        )
    )

    await _authority(tmp_path, handlers={"repair": repair_handler}).apply_change_non_transactional(
        conn, "baseline", spec, epoch_dir
    )

    assert calls == [ledger.ATTEMPT_STATE_FAILED]
    transition = next(
        args for query, args in conn.fetches if query == runner._TRANSITION_EXPIRED_ATTEMPT
    )
    assert transition[3] == ledger.ATTEMPT_STATE_FAILED


async def test_unknown_checkpoint_fails_before_claim_or_sql(tmp_path: Path) -> None:
    sql = "SELECT 1;\n-- @checkpoint known\nSELECT 2;"
    spec, epoch_dir = _write_change(
        tmp_path,
        sql,
        mode=TransactionMode.NON_TRANSACTIONAL,
        resume_handler="resume",
    )
    conn = RunnerConnection(
        latest_attempt=_attempt(
            spec,
            state=ledger.ATTEMPT_STATE_RESUMABLE,
            checkpoint="unknown",
        )
    )

    with pytest.raises(AuthorityBlockedError, match="unknown checkpoint"):
        await _authority(
            tmp_path, handlers={"resume": _noop_recovery_handler}
        ).apply_change_non_transactional(conn, "baseline", spec, epoch_dir)

    assert conn.executed == []
    assert not any(query == runner._CLAIM_ATTEMPT for query, _args in conn.fetches)


async def test_nontransactional_success_waits_for_all_postconditions(
    tmp_path: Path,
) -> None:
    sql = "SELECT 1;"
    spec, epoch_dir = _write_change(
        tmp_path,
        sql,
        mode=TransactionMode.NON_TRANSACTIONAL,
        postconditions=("postcondition",),
        repair_handler="repair",
    )
    conn = RunnerConnection(conditions={"postcondition": False})

    with pytest.raises(AuthorityError, match="postcondition"):
        await _authority(
            tmp_path, handlers={"repair": _noop_recovery_handler}
        ).apply_change_non_transactional(conn, "baseline", spec, epoch_dir)

    assert not any(query == ledger.INSERT_CHANGE_SUCCESS for query, _args, _depth in conn.executed)
    terminal_args = [
        args for query, args in conn.fetches if query == runner._UPDATE_ATTEMPT_TERMINAL_FENCED
    ]
    assert terminal_args[-1][3] == ledger.ATTEMPT_STATE_FAILED
    executed_sql = [query for query, _args, _depth in conn.executed]
    assert "RESET statement_timeout" in executed_sql
    assert "RESET lock_timeout" in executed_sql


async def test_lost_fence_blocks_resume(tmp_path: Path) -> None:
    sql = "SELECT 1;"
    spec, epoch_dir = _write_change(
        tmp_path,
        sql,
        mode=TransactionMode.NON_TRANSACTIONAL,
        repair_handler="repair",
    )

    async def repair_handler(conn: Any, context: runner.RecoveryContext) -> None:
        assert isinstance(conn, RunnerConnection)
        assert context.attempt_id == "001-attempt"

    conn = RunnerConnection(
        latest_attempt=_attempt(spec, state=ledger.ATTEMPT_STATE_FAILED, checkpoint="")
    )
    conn.lose_fence_for = "claim"

    with pytest.raises(AuthorityBlockedError, match="lost its lease/fence"):
        await _authority(
            tmp_path, handlers={"repair": repair_handler}
        ).apply_change_non_transactional(conn, "baseline", spec, epoch_dir)


@pytest.mark.parametrize(
    ("applied", "match"),
    [
        ({3: "3" * 64}, "outside its immutable manifest"),
        ({2: "2" * 64}, "not a contiguous prefix"),
    ],
)
async def test_epoch_rejects_unknown_or_gapped_success_ledger(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    applied: dict[int, str],
    match: str,
) -> None:
    first = ChangeSpec(
        sequence=1,
        name="first",
        file="001_first.sql",
        sha256="1" * 64,
        owner="owner",
        transaction_mode=TransactionMode.TRANSACTIONAL,
        rollback_class=RollbackClass.OLD_BINARY_COMPATIBLE,
    )
    second = ChangeSpec(
        sequence=2,
        name="second",
        file="002_second.sql",
        sha256="2" * 64,
        owner="owner",
        transaction_mode=TransactionMode.TRANSACTIONAL,
        rollback_class=RollbackClass.OLD_BINARY_COMPATIBLE,
    )
    authority = _authority(tmp_path)

    async def recorded(_conn: Any, _baseline_id: str) -> dict[int, str]:
        return applied

    monkeypatch.setattr(authority, "applied_changes", recorded)

    with pytest.raises(AuthorityError, match=match):
        await authority.apply_epoch(
            RunnerConnection(),
            EpochManifest(baseline_id="baseline", epoch=1, changes=(first, second)),
            tmp_path,
        )
