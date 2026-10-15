from __future__ import annotations

import io
import json
import os
import tempfile
import uuid
from copy import deepcopy
from types import SimpleNamespace
from typing import Any

import pytest
from fastapi import HTTPException, UploadFile
from knowledge_service.api.routes import knowledge as routes
from knowledge_service.api.schemas.knowledge import (
    BatchRetrieveRequestSchema,
    ChunkPreviewRequestSchema,
    DatasetCreateSchema,
    DocumentBatchCreateSchema,
    DocumentCreateTextSchema,
    LLMConfigSchema,
    QABatchTestSchema,
    QAQuerySchema,
    RetrievalConfigSchema,
    RetrievalEvalRequestSchema,
    RetrieveRequestSchema,
)
from knowledge_service.auth.user_context import ANONYMOUS_CONTEXT
from knowledge_service.config import Settings
from knowledge_service.core.auth.user_resolver import UserContext
from knowledge_service.core.exceptions import ValidationFailedError
from knowledge_service.services.knowledge.qa_service import (
    LLMConfig,
    LLMProvider,
    QAResult,
    QAService,
    QATestCase,
    QATestResult,
)
from pydantic import ValidationError

USER = UserContext(user_id="user-a", tenant_id="tenant-a")


def test_dataset_default_matches_dashscope_only_quickstart() -> None:
    request = DatasetCreateSchema(name="default-provider")
    settings = Settings()

    assert request.embedding_provider == "dashscope"
    assert request.embedding_model == "text-embedding-v4"
    assert request.embedding_dimension == 1024
    assert settings.embeddings.provider == "dashscope"
    assert settings.embeddings.model == "text-embedding-v4"


def test_anonymous_context_is_not_authenticated() -> None:
    assert ANONYMOUS_CONTEXT.is_authenticated is False
    assert ANONYMOUS_CONTEXT.roles == ["guest"]


@pytest.mark.parametrize(
    "forbidden",
    [
        {"api_key": "caller-secret"},
        {"base_url": "http://127.0.0.1:9/private"},
        {"endpoint": "http://169.254.169.254/latest/meta-data"},
    ],
)
def test_qa_selector_rejects_caller_credentials_and_endpoints(
    forbidden: dict[str, str],
) -> None:
    with pytest.raises(ValidationError):
        LLMConfigSchema(**forbidden)


def test_qa_schema_enforces_paid_request_budgets() -> None:
    with pytest.raises(ValidationError):
        QAQuerySchema(query="x", top_k=101)
    with pytest.raises(ValidationError):
        QABatchTestSchema(test_cases=[{"query": f"q-{index}"} for index in range(11)])


@pytest.mark.parametrize(
    "payload",
    [
        {"query": "q", "top_k": 101},
        {"query": "q", "vector_top_k": 1001},
        {"query": "q", "keyword_top_k": 1001},
        {"query": "q", "candidate_top_k": 2001},
        {"query": "q", "keyword_candidate_k": 501},
        {"query": "q", "rerank_top_n": 1001},
        {"query": "q", "rrf_k": 10_001},
        {"query": "q" * 4097},
    ],
)
def test_public_retrieval_rejects_resource_amplification(payload: dict[str, Any]) -> None:
    with pytest.raises(ValidationError):
        RetrieveRequestSchema.model_validate(payload)


@pytest.mark.parametrize(
    "payload",
    [
        {"queries": [f"q-{index}" for index in range(21)]},
        {"queries": ["q" * 4097]},
        {"queries": ["q"], "max_parallel": 11},
        {"queries": ["q"], "vector_top_k": 1001},
        {"query": ",".join(f"q-{index}" for index in range(21))},
    ],
)
def test_batch_retrieval_rejects_resource_amplification(payload: dict[str, Any]) -> None:
    with pytest.raises(ValidationError):
        BatchRetrieveRequestSchema.model_validate(payload)


def test_chunk_preview_rejects_oversize_and_custom_regex() -> None:
    with pytest.raises(ValidationError):
        ChunkPreviewRequestSchema(text="x" * 200_001)
    with pytest.raises(ValidationError, match="custom regex"):
        ChunkPreviewRequestSchema(
            text="safe-sized",
            config={"mode": "regex", "regex_pattern": "(a+)+$"},
        )


@pytest.mark.parametrize(
    "config",
    [
        {"mode": "regex"},
        {"regex_pattern": "(a+)+$"},
        {"regex": "(a+)+$"},
        {"heading_patterns": ["(a+)+$"]},
        {"page_marker": "(a+)+$"},
    ],
)
def test_chunk_preview_schema_rejects_every_custom_regex_surface(
    config: dict[str, Any],
) -> None:
    with pytest.raises(ValidationError):
        ChunkPreviewRequestSchema(text="safe-sized", config=config)


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "config",
    [
        {"mode": "regex"},
        {"regex_pattern": "(a+)+$"},
        {"regex": "(a+)+$"},
        {"heading_patterns": ["(a+)+$"]},
        {"page_marker": "(a+)+$"},
    ],
)
async def test_generic_preview_rejects_custom_regex_surfaces_before_service(
    config: dict[str, Any],
) -> None:
    class Config:
        def model_dump(self, **_kwargs: Any) -> dict[str, Any]:
            return config

    class Service:
        async def preview_chunking(self, *_args: Any, **_kwargs: Any) -> None:
            raise AssertionError("preview service started for unsafe config")

    payload = ChunkPreviewRequestSchema.model_construct(
        text="safe-sized",
        config=Config(),
        document_id=None,
    )
    with pytest.raises(HTTPException) as exc_info:
        await routes.preview_chunking_generic(
            payload=payload,
            svc=Service(),  # type: ignore[arg-type]
            user=USER,
        )

    assert exc_info.value.status_code == 400


@pytest.mark.parametrize(
    "payload",
    [
        {"top_k": 101},
        {"vector_top_k": 1001},
        {"keyword_top_k": 1001},
        {"rrf_k": 10_001},
        {"rerank_top_n": 1001},
        {"alpha": float("nan")},
        {"mmr_lambda": float("inf")},
    ],
)
def test_persisted_retrieval_schema_rejects_resource_amplification(
    payload: dict[str, Any],
) -> None:
    with pytest.raises(ValidationError):
        RetrievalConfigSchema.model_validate(payload)


def test_text_document_schemas_bound_single_and_aggregate_content() -> None:
    with pytest.raises(ValidationError):
        DocumentCreateTextSchema(title="t", content="x" * 200_001)
    with pytest.raises(ValidationError, match="batch document content"):
        DocumentBatchCreateSchema(
            documents=[
                {"title": f"doc-{index}", "content": "x" * 200_000}
                for index in range(11)
            ]
        )


def test_qa_config_uses_only_server_endpoint_and_key(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(
        "ai_gateway_core.config.resolve_dashscope",
        lambda _domain: (
            "server-owned-key",
            "https://server-owned.example.test/compatible-mode",
        ),
    )
    settings = Settings(
        ragas_eval={
            "provider": "dashscope",
            "model": "qwen3.7-plus",
            "base_url": "https://qa-server.example.test/compatible-mode/v1",
            "allowed_providers": ["dashscope"],
            "allowed_models": ["qwen3.7-plus"],
        }
    )

    config = routes._build_server_qa_llm_config(
        LLMConfigSchema(
            provider="dashscope",
            model="qwen3.7-plus",
            temperature=0.2,
            max_tokens=512,
        ),
        settings,
    )

    assert config.provider.value == "dashscope"
    assert config.model == "qwen3.7-plus"
    assert config.api_key == "server-owned-key"
    assert config.base_url == "https://qa-server.example.test/compatible-mode/v1"


@pytest.mark.parametrize(
    "payload",
    [
        {"query": "x", "include_images": True},
        {"query": "x", "include_associated_images": True},
        {"query": "x", "content_type_filter": "image"},
        {"query": "x", "multimodal_rerank": True},
        {"query": "x", "image_search_enabled": True},
    ],
)
def test_public_retrieval_rejects_unreleased_multimodal_flags(
    payload: dict[str, Any],
) -> None:
    with pytest.raises(ValidationError, match="multimodal retrieval is not enabled"):
        RetrieveRequestSchema(**payload)


@pytest.mark.asyncio
async def test_dataset_config_route_redacts_legacy_rerank_secret() -> None:
    raw_dataset = {
        "dataset_id": "dataset-a",
        "tenant_id": "tenant-a",
        "embedding_provider": "dashscope",
        "embedding_model": "text-embedding-v4",
        "embedding_dimension": 1024,
        "collection_name": "kb_a",
        "index_config": {
            "retrieval": {
                "mode": "hybrid",
                "rerank": {"enabled": True, "api_key": "sentinel-rerank-secret"},
            }
        },
    }

    class Service:
        async def require_dataset_access(self, *_args: Any, **_kwargs: Any) -> dict[str, Any]:
            return deepcopy(raw_dataset)

        @staticmethod
        def sanitize_dataset_for_response(dataset: dict[str, Any]) -> dict[str, Any]:
            sanitized = deepcopy(dataset)
            sanitized["index_config"]["retrieval"]["rerank"]["api_key"] = "*****"
            return sanitized

        async def get_dataset_statistics(self, *_args: Any, **_kwargs: Any) -> dict[str, int]:
            return {"segment_count": 0}

    response = await routes.get_dataset_config(
        "dataset-a",
        svc=Service(),  # type: ignore[arg-type]
        user=USER,
    )

    serialized = json.dumps(response)
    assert "sentinel-rerank-secret" not in serialized
    assert response["retrieval"]["rerank"]["api_key"] == "*****"


@pytest.mark.asyncio
async def test_image_upload_route_is_explicitly_unavailable_after_access_check() -> None:
    calls: list[tuple[str, str]] = []

    class Service:
        async def require_dataset_access(
            self,
            _user: UserContext,
            dataset_id: str,
            *,
            required: str,
        ) -> dict[str, str]:
            calls.append((dataset_id, required))
            return {"dataset_id": dataset_id}

    with pytest.raises(HTTPException) as exc_info:
        await routes.upload_images(
            "dataset-a",
            svc=Service(),  # type: ignore[arg-type]
            user=USER,
        )

    assert exc_info.value.status_code == 503
    assert calls == [("dataset-a", "editor")]


@pytest.mark.asyncio
async def test_generic_upload_rejects_explicit_multimodal_mode() -> None:
    upload = UploadFile(filename="document.pdf", file=io.BytesIO(b"%PDF-test"))

    class Service:
        async def require_dataset_access(
            self,
            _user: UserContext,
            dataset_id: str,
            *,
            required: str,
        ) -> dict[str, str]:
            assert required == "editor"
            return {"dataset_id": dataset_id}

    with pytest.raises(HTTPException) as exc_info:
        await routes.upload_document(
            "dataset-a",
            file=upload,
            processing_mode="multimodal",
            svc=Service(),  # type: ignore[arg-type]
            worker=SimpleNamespace(),  # type: ignore[arg-type]
            user=USER,
            settings=Settings(),
        )

    assert exc_info.value.status_code == 400
    assert "disabled" in str(exc_info.value.detail)


@pytest.mark.asyncio
async def test_single_upload_rejects_guest_before_tempfile_or_read(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    class PoisonUpload(UploadFile):
        async def read(self, _size: int = -1) -> bytes:
            raise AssertionError("upload body read before authentication")

    def poison_tempfile(*_args: Any, **_kwargs: Any) -> None:
        raise AssertionError("temporary file created before authentication")

    monkeypatch.setattr(tempfile, "NamedTemporaryFile", poison_tempfile)
    guest = UserContext(
        user_id="anonymous",
        tenant_id="default",
        user_type="anonymous",
        roles=["guest"],
        is_authenticated=False,
    )

    with pytest.raises(HTTPException) as exc_info:
        await routes.upload_document(
            "dataset-a",
            file=PoisonUpload(filename="document.txt", file=io.BytesIO(b"ignored")),
            processing_mode="text_only",
            svc=SimpleNamespace(),  # type: ignore[arg-type]
            worker=SimpleNamespace(),  # type: ignore[arg-type]
            user=guest,
            settings=Settings(),
        )

    assert exc_info.value.status_code == 403


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("route", "payload"),
    [
        (routes.qa_query, QAQuerySchema(query="q")),
        (routes.qa_query_stream, QAQuerySchema(query="q")),
        (routes.qa_batch_test, QABatchTestSchema(test_cases=[{"query": "q"}])),
    ],
)
async def test_all_qa_routes_apply_server_owned_config_gate(
    monkeypatch: pytest.MonkeyPatch,
    route: Any,
    payload: Any,
) -> None:
    def reject_config(*_args: Any, **_kwargs: Any) -> None:
        raise ValidationFailedError("server-owned QA gate")

    monkeypatch.setattr(routes, "_build_server_qa_llm_config", reject_config)

    class Service:
        async def require_dataset_access(
            self,
            _user: UserContext,
            _dataset_id: str,
            *,
            required: str,
        ) -> dict[str, str]:
            assert required == "editor"
            return {"dataset_id": "dataset-a"}

    admin = UserContext(
        user_id="admin-a",
        tenant_id="tenant-a",
        roles=["admin"],
        is_authenticated=True,
    )

    with pytest.raises(HTTPException) as exc_info:
        await route(
            request=SimpleNamespace(),
            dataset_id="dataset-a",
            payload=payload,
            svc=Service(),  # type: ignore[arg-type]
            user=admin,
            settings=Settings(),
        )

    assert exc_info.value.status_code == 400
    assert "server-owned QA gate" in str(exc_info.value.detail)


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("route", "payload"),
    [
        (routes.qa_query, QAQuerySchema(query="q")),
        (routes.qa_query_stream, QAQuerySchema(query="q")),
        (routes.qa_batch_test, QABatchTestSchema(test_cases=[{"query": "q"}])),
    ],
)
async def test_all_qa_routes_reject_guest_before_paid_config(
    monkeypatch: pytest.MonkeyPatch,
    route: Any,
    payload: Any,
) -> None:
    config_calls = 0

    def config_must_not_resolve(*_args: Any, **_kwargs: Any) -> None:
        nonlocal config_calls
        config_calls += 1
        raise AssertionError("paid config resolved for guest")

    class Service:
        async def require_dataset_access(self, *_args: Any, **_kwargs: Any) -> None:
            raise AssertionError("dataset lookup started for guest")

    monkeypatch.setattr(routes, "_build_server_qa_llm_config", config_must_not_resolve)
    guest = UserContext(
        user_id="guest-forwarded",
        tenant_id="tenant-a",
        user_type="guest",
        roles=["guest"],
        is_authenticated=True,
    )

    with pytest.raises(HTTPException) as exc_info:
        await route(
            request=SimpleNamespace(),
            dataset_id="dataset-a",
            payload=payload,
            svc=Service(),  # type: ignore[arg-type]
            user=guest,
            settings=Settings(),
        )

    assert exc_info.value.status_code == 403
    assert config_calls == 0


@pytest.mark.asyncio
async def test_retrieval_eval_rejects_guest_before_dataset_or_retrieval() -> None:
    class Service:
        async def require_dataset_access(self, *_args: Any, **_kwargs: Any) -> None:
            raise AssertionError("dataset lookup started for guest")

        async def retrieve(self, **_kwargs: Any) -> None:
            raise AssertionError("retrieval started for guest")

    guest = UserContext(
        user_id="anonymous",
        tenant_id="default",
        user_type="anonymous",
        roles=["guest"],
        is_authenticated=False,
    )
    payload = RetrievalEvalRequestSchema.model_validate(
        {"cases": [{"query": "q", "relevant_segment_ids": ["s1"]}]}
    )

    with pytest.raises(HTTPException) as exc_info:
        await routes.retrieve_evaluate(
            "dataset-a",
            payload,
            svc=Service(),  # type: ignore[arg-type]
            user=guest,
        )

    assert exc_info.value.status_code == 403


@pytest.mark.asyncio
async def test_generic_preview_rejects_guest_before_service_call() -> None:
    class Service:
        async def preview_chunking(self, *_args: Any, **_kwargs: Any) -> None:
            raise AssertionError("preview service started for guest")

    guest = UserContext(
        user_id="anonymous",
        tenant_id="default",
        user_type="anonymous",
        roles=["guest"],
        is_authenticated=False,
    )
    with pytest.raises(HTTPException) as exc_info:
        await routes.preview_chunking_generic(
            payload=ChunkPreviewRequestSchema(text="safe"),
            svc=Service(),  # type: ignore[arg-type]
            user=guest,
        )

    assert exc_info.value.status_code == 403


@pytest.mark.asyncio
async def test_batch_upload_rejects_more_than_fifty_files() -> None:
    class Service:
        async def require_dataset_access(self, *_args: Any, **_kwargs: Any) -> dict[str, str]:
            return {"dataset_id": "dataset-a"}

    files = [
        UploadFile(filename=f"document-{index}.txt", file=io.BytesIO(b"ok"))
        for index in range(51)
    ]

    with pytest.raises(HTTPException) as exc_info:
        await routes.batch_upload_documents(
            "dataset-a",
            files=files,
            svc=Service(),  # type: ignore[arg-type]
            worker=object(),  # type: ignore[arg-type]
            user=USER,
            settings=Settings(),
        )

    assert exc_info.value.status_code == 400
    assert exc_info.value.detail == "Maximum 50 files allowed per batch"


@pytest.mark.asyncio
async def test_batch_upload_rejects_oversize_before_create_or_enqueue(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path,
) -> None:
    monkeypatch.setenv("KB_MAX_FILE_SIZE_MB", "1")
    monkeypatch.setenv("KB_MAX_BATCH_SIZE_MB", "1")
    created_temp_paths: list[str] = []
    real_named_temporary_file = tempfile.NamedTemporaryFile

    def recording_tempfile(*args: Any, **kwargs: Any):
        kwargs["dir"] = tmp_path
        handle = real_named_temporary_file(*args, **kwargs)
        created_temp_paths.append(handle.name)
        return handle

    monkeypatch.setattr(tempfile, "NamedTemporaryFile", recording_tempfile)

    class Service:
        async def require_dataset_access(self, *_args: Any, **_kwargs: Any) -> dict[str, str]:
            return {"dataset_id": "dataset-a"}

        async def create_document_from_upload(self, *_args: Any, **_kwargs: Any) -> None:
            raise AssertionError("oversize upload reached document creation")

    class Worker:
        async def enqueue(self, *_args: Any, **_kwargs: Any) -> None:
            raise AssertionError("oversize upload reached enqueue")

    upload = UploadFile(
        filename="oversize.txt",
        file=io.BytesIO(b"x" * (1024 * 1024 + 1)),
    )
    response = await routes.batch_upload_documents(
        "dataset-a",
        files=[upload],
        svc=Service(),  # type: ignore[arg-type]
        worker=Worker(),  # type: ignore[arg-type]
        user=USER,
        settings=Settings(),
    )

    assert response["accepted"] == 0
    assert response["rejected"] == 1
    assert "too large" in response["errors"][0]["error"].lower()
    assert created_temp_paths
    assert all(not os.path.exists(path) for path in created_temp_paths)


@pytest.mark.asyncio
async def test_batch_upload_aggregate_cap_has_bounded_partial_success(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path,
) -> None:
    monkeypatch.setenv("KB_MAX_FILE_SIZE_MB", "1")
    monkeypatch.setenv("KB_MAX_BATCH_SIZE_MB", "1")
    real_named_temporary_file = tempfile.NamedTemporaryFile
    created_temp_paths: list[str] = []

    def recording_tempfile(*args: Any, **kwargs: Any):
        kwargs["dir"] = tmp_path
        handle = real_named_temporary_file(*args, **kwargs)
        created_temp_paths.append(handle.name)
        return handle

    monkeypatch.setattr(tempfile, "NamedTemporaryFile", recording_tempfile)

    class Service:
        def __init__(self) -> None:
            self.created: list[str] = []

        async def require_dataset_access(self, *_args: Any, **_kwargs: Any) -> dict[str, str]:
            return {"dataset_id": "dataset-a"}

        async def create_document_from_upload(
            self,
            *_args: Any,
            filename: str,
            **_kwargs: Any,
        ) -> dict[str, str]:
            self.created.append(filename)
            return {"document_id": f"doc-{len(self.created)}", "title": filename}

    class Worker:
        def __init__(self) -> None:
            self.enqueued: list[str] = []

        async def enqueue(
            self, _dataset_id: str, document_id: str, **_kwargs: Any
        ) -> bool:
            self.enqueued.append(document_id)
            return True

    service = Service()
    worker = Worker()
    payload = b"x" * (700 * 1024)
    response = await routes.batch_upload_documents(
        "dataset-a",
        files=[
            UploadFile(filename="first.txt", file=io.BytesIO(payload)),
            UploadFile(filename="second.txt", file=io.BytesIO(payload)),
        ],
        svc=service,  # type: ignore[arg-type]
        worker=worker,  # type: ignore[arg-type]
        user=USER,
        settings=Settings(),
    )

    assert response["accepted"] == 1
    assert response["rejected"] == 1
    assert service.created == ["first.txt"]
    assert worker.enqueued == ["doc-1"]
    assert "aggregate limit" in response["errors"][0]["error"]
    assert all(not os.path.exists(path) for path in created_temp_paths)


@pytest.mark.asyncio
async def test_batch_upload_same_name_failure_identifies_file_position() -> None:
    class Service:
        async def require_dataset_access(self, *_args: Any, **_kwargs: Any) -> dict[str, str]:
            return {"dataset_id": "dataset-a"}

        async def create_document_from_upload(
            self, *_args: Any, filename: str, **_kwargs: Any
        ) -> dict[str, str]:
            return {"document_id": str(uuid.uuid4()), "title": filename}

    class Worker:
        async def enqueue(self, *_args: Any, **_kwargs: Any) -> bool:
            return True

    response = await routes.batch_upload_documents(
        "dataset-a",
        files=[
            UploadFile(filename="same.txt", file=io.BytesIO(b"")),
            UploadFile(filename="same.txt", file=io.BytesIO(b"first")),
            UploadFile(filename="other.txt", file=io.BytesIO(b"second")),
        ],
        svc=Service(),  # type: ignore[arg-type]
        worker=Worker(),  # type: ignore[arg-type]
        user=USER,
        settings=Settings(),
    )

    assert response["accepted"] == 2
    assert response["errors"] == [
        {
            "file_index": 0,
            "filename": "same.txt",
            "error": "Empty files are not accepted",
            "retry_safe": True,
        }
    ]


@pytest.mark.asyncio
async def test_batch_upload_enqueue_rejection_identifies_created_record() -> None:
    class Service:
        async def require_dataset_access(self, *_args: Any, **_kwargs: Any) -> dict[str, str]:
            return {"dataset_id": "dataset-a"}

        async def create_document_from_upload(
            self, *_args: Any, filename: str, **_kwargs: Any
        ) -> dict[str, str]:
            return {"document_id": "created-doc", "title": filename}

    class Worker:
        async def enqueue(self, *_args: Any, **_kwargs: Any) -> bool:
            return False

    response = await routes.batch_upload_documents(
        "dataset-a",
        files=[UploadFile(filename="source.txt", file=io.BytesIO(b"source"))],
        svc=Service(),  # type: ignore[arg-type]
        worker=Worker(),  # type: ignore[arg-type]
        user=USER,
        settings=Settings(),
    )

    assert response["accepted"] == 0
    assert response["errors"][0]["file_index"] == 0
    assert response["errors"][0]["document_id"] == "created-doc"
    assert response["errors"][0]["retry_safe"] is False


@pytest.mark.asyncio
async def test_batch_upload_unexpected_error_does_not_expose_internal_detail() -> None:
    class Service:
        async def require_dataset_access(self, *_args: Any, **_kwargs: Any) -> dict[str, str]:
            return {"dataset_id": "dataset-a"}

        async def create_document_from_upload(self, *_args: Any, **_kwargs: Any) -> None:
            raise RuntimeError("private-connection-detail")

    response = await routes.batch_upload_documents(
        "dataset-a",
        files=[UploadFile(filename="source.txt", file=io.BytesIO(b"source"))],
        svc=Service(),  # type: ignore[arg-type]
        worker=object(),  # type: ignore[arg-type]
        user=USER,
        settings=Settings(),
    )

    error = response["errors"][0]
    assert "private-connection-detail" not in error["error"]
    assert error["retry_safe"] is False


@pytest.mark.asyncio
async def test_batch_upload_post_creation_validation_is_not_directly_retryable() -> None:
    class Service:
        async def require_dataset_access(self, *_args: Any, **_kwargs: Any) -> dict[str, str]:
            return {"dataset_id": "dataset-a"}

        async def create_document_from_upload(self, *_args: Any, **_kwargs: Any) -> None:
            # The upload service may already have inserted its uploading row.
            raise ValidationFailedError("private-post-insert-detail")

    response = await routes.batch_upload_documents(
        "dataset-a",
        files=[UploadFile(filename="source.txt", file=io.BytesIO(b"source"))],
        svc=Service(),  # type: ignore[arg-type]
        worker=object(),  # type: ignore[arg-type]
        user=USER,
        settings=Settings(),
    )

    error = response["errors"][0]
    assert error["file_index"] == 0
    assert error["retry_safe"] is False
    assert "private-post-insert-detail" not in error["error"]


@pytest.mark.asyncio
async def test_single_upload_post_creation_validation_is_unknown_not_retryable() -> None:
    class Service:
        async def require_dataset_access(self, *_args: Any, **_kwargs: Any) -> dict[str, str]:
            return {"dataset_id": "dataset-a"}

        async def create_document_from_upload(self, *_args: Any, **_kwargs: Any) -> None:
            raise ValidationFailedError("private-post-insert-detail")

    with pytest.raises(HTTPException) as exc_info:
        await routes.upload_document(
            "dataset-a",
            file=UploadFile(filename="source.txt", file=io.BytesIO(b"source")),
            processing_mode="text_only",
            svc=Service(),  # type: ignore[arg-type]
            worker=object(),  # type: ignore[arg-type]
            user=USER,
            settings=Settings(),
        )

    assert exc_info.value.status_code == 409
    assert "private-post-insert-detail" not in str(exc_info.value.detail)


@pytest.mark.asyncio
async def test_qa_batch_fails_closed_when_any_case_fails(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    service = QAService(
        SimpleNamespace(),  # type: ignore[arg-type]
        LLMConfig(provider=LLMProvider.DASHSCOPE, model="qwen3.7-plus"),
    )

    async def run_case(*_args: Any, test_case: QATestCase, **_kwargs: Any) -> QATestResult:
        if test_case.query == "fails":
            raise RuntimeError("provider unavailable")
        return QATestResult(
            test_case=test_case,
            qa_result=QAResult(
                query=test_case.query,
                answer="ok",
                context_segments=[],
                retrieval_metadata={},
                retrieval_time_ms=1,
                llm_time_ms=1,
                total_time_ms=2,
                model="qwen3.7-plus",
            ),
        )

    monkeypatch.setattr(service, "run_test_case", run_case)

    with pytest.raises(RuntimeError, match="1 of 2 cases failed"):
        await service.run_test_batch(
            user_context=USER,
            dataset_id="dataset-a",
            test_cases=[QATestCase(query="ok"), QATestCase(query="fails")],
        )
