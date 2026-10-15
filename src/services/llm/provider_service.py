"""
LLM Provider Service.

Manages LLM provider configurations including API keys and endpoints.
"""

import asyncio
import json
import os
from collections.abc import Mapping
from datetime import datetime, timezone
from typing import Any

import httpx
from ai_gateway_core.logging import get_logger

from ...core.crypto import decrypt_value, encrypt_value
from ...persistence.database import DatabaseStorage

logger = get_logger(__name__)


class ProviderService:
    """Service for managing LLM providers."""

    def __init__(self, database: DatabaseStorage, encryption_key: str = ""):
        self.db = database
        self.encryption_key = encryption_key

    async def list_providers(
        self,
        tenant_id: str,
        include_disabled: bool = False,
    ) -> list[dict[str, Any]]:
        """List all providers for a tenant."""
        if include_disabled:
            query = """
                SELECT provider_id, tenant_id, display_name, api_type, base_url,
                       api_key_encrypted, metadata, is_enabled, created_at, updated_at
                FROM llm_providers
                WHERE tenant_id = $1
                ORDER BY display_name
            """
            rows = await self.db.fetch(query, tenant_id)
        else:
            query = """
                SELECT provider_id, tenant_id, display_name, api_type, base_url,
                       api_key_encrypted, metadata, is_enabled, created_at, updated_at
                FROM llm_providers
                WHERE tenant_id = $1 AND is_enabled = true
                ORDER BY display_name
            """
            rows = await self.db.fetch(query, tenant_id)

        return [self._row_to_dict(row) for row in rows]

    async def get_provider(
        self,
        tenant_id: str,
        provider_id: str,
    ) -> dict[str, Any] | None:
        """Get a specific provider."""
        query = """
            SELECT provider_id, tenant_id, display_name, api_type, base_url,
                   api_key_encrypted, metadata, is_enabled, created_at, updated_at
            FROM llm_providers
            WHERE tenant_id = $1 AND provider_id = $2
        """
        row = await self.db.fetchrow(query, tenant_id, provider_id)
        return self._row_to_dict(row) if row else None

    async def create_provider(
        self,
        tenant_id: str,
        provider_id: str,
        display_name: str,
        api_type: str = "openai",
        base_url: str | None = None,
        api_key: str | None = None,
        metadata: dict[str, Any] | None = None,
        is_enabled: bool = True,
    ) -> dict[str, Any]:
        """Create a new provider."""
        # Encrypt API key if provided
        api_key_encrypted = None
        if api_key:
            api_key_encrypted = encrypt_value(api_key, self.encryption_key)

        query = """
            INSERT INTO llm_providers (
                provider_id, tenant_id, display_name, api_type, base_url,
                api_key_encrypted, metadata, is_enabled
            ) VALUES ($1, $2, $3, $4, $5, $6, $7::jsonb, $8)
            RETURNING provider_id, tenant_id, display_name, api_type, base_url,
                      api_key_encrypted, metadata, is_enabled, created_at, updated_at
        """
        metadata_json = json.dumps(metadata or {})
        row = await self.db.fetchrow(
            query,
            provider_id,
            tenant_id,
            display_name,
            api_type,
            base_url,
            api_key_encrypted,
            metadata_json,
            is_enabled,
        )
        return self._row_to_dict(row)

    async def update_provider(
        self,
        tenant_id: str,
        provider_id: str,
        display_name: str | None = None,
        api_type: str | None = None,
        base_url: str | None = None,
        api_key: str | None = None,
        metadata: dict[str, Any] | None = None,
        is_enabled: bool | None = None,
    ) -> dict[str, Any] | None:
        """Update a provider."""
        # Build dynamic update query
        updates = []
        params = []
        param_idx = 1

        if display_name is not None:
            updates.append(f"display_name = ${param_idx}")
            params.append(display_name)
            param_idx += 1

        if api_type is not None:
            updates.append(f"api_type = ${param_idx}")
            params.append(api_type)
            param_idx += 1

        if base_url is not None:
            updates.append(f"base_url = ${param_idx}")
            params.append(base_url)
            param_idx += 1

        if api_key is not None:
            encrypted = encrypt_value(api_key, self.encryption_key)
            updates.append(f"api_key_encrypted = ${param_idx}")
            params.append(encrypted)
            param_idx += 1

        if metadata is not None:
            updates.append(f"metadata = ${param_idx}::jsonb")
            params.append(json.dumps(metadata))
            param_idx += 1

        if is_enabled is not None:
            updates.append(f"is_enabled = ${param_idx}")
            params.append(is_enabled)
            param_idx += 1

        if not updates:
            return await self.get_provider(tenant_id, provider_id)

        updates.append(f"updated_at = ${param_idx}")
        params.append(datetime.now(timezone.utc))
        param_idx += 1

        params.extend([tenant_id, provider_id])

        query = f"""
            UPDATE llm_providers
            SET {", ".join(updates)}
            WHERE tenant_id = ${param_idx} AND provider_id = ${param_idx + 1}
            RETURNING provider_id, tenant_id, display_name, api_type, base_url,
                      api_key_encrypted, metadata, is_enabled, created_at, updated_at
        """
        row = await self.db.fetchrow(query, *params)
        return self._row_to_dict(row) if row else None

    async def delete_provider(
        self,
        tenant_id: str,
        provider_id: str,
    ) -> bool:
        """Delete a provider and all its models."""
        # First delete all models for this provider
        await self.db.execute(
            "DELETE FROM llm_models WHERE tenant_id = $1 AND provider_id = $2",
            tenant_id,
            provider_id,
        )
        # Then delete the provider
        result = await self.db.execute(
            "DELETE FROM llm_providers WHERE tenant_id = $1 AND provider_id = $2",
            tenant_id,
            provider_id,
        )
        return "DELETE 1" in str(result)

    async def test_connection(
        self,
        tenant_id: str,
        provider_id: str,
    ) -> dict[str, Any]:
        """Test API connection for a provider."""
        provider = await self.get_provider(tenant_id, provider_id)
        if not provider:
            return {"success": False, "message": "Provider not found"}

        # Get decrypted API key
        api_key = await self._get_api_key(tenant_id, provider_id)
        api_type = provider.get("api_type", "openai")
        if not api_key and api_type != "google-vertex":
            return {"success": False, "message": "No API key configured"}
        base_url = provider.get("base_url", "")

        try:
            async with httpx.AsyncClient(timeout=10.0) as client:
                if api_type == "openai":
                    # Test OpenAI-compatible API
                    url = f"{base_url}/v1/models"
                    headers = {"Authorization": f"Bearer {api_key}"}
                    response = await client.get(url, headers=headers)
                elif api_type == "anthropic":
                    # Test Anthropic API (use a simple request)
                    url = f"{base_url}/v1/messages"
                    headers = {
                        "x-api-key": api_key,
                        "anthropic-version": "2023-06-01",
                        "content-type": "application/json",
                    }
                    # Just check if we can reach the endpoint
                    response = await client.post(
                        url,
                        headers=headers,
                        json={
                            "model": "claude-3-5-haiku-20241022",
                            "max_tokens": 1,
                            "messages": [{"role": "user", "content": "Hi"}],
                        },
                    )
                elif api_type == "google":
                    # Test Google API
                    url = f"{base_url}/v1beta/models?key={api_key}"
                    response = await client.get(url)
                elif api_type == "google-vertex":
                    metadata = provider.get("metadata") if isinstance(provider.get("metadata"), dict) else {}
                    if not api_key or api_key.lstrip().startswith("{"):
                        project = (
                            metadata.get("project")
                            or metadata.get("project_id")
                            or os.getenv("GOOGLE_CLOUD_PROJECT")
                            or os.getenv("GOOGLE_PROJECT_ID")
                        )
                        location = (
                            metadata.get("location")
                            or metadata.get("region")
                            or os.getenv("GOOGLE_CLOUD_LOCATION")
                            or os.getenv("GOOGLE_CLOUD_REGION")
                            or "us-central1"
                        )
                        import google.auth
                        from google.auth.transport.requests import Request as GoogleAuthRequest
                        from google.oauth2 import service_account

                        scopes = ["https://www.googleapis.com/auth/cloud-platform"]
                        if api_key:
                            service_account_info = json.loads(api_key)
                            if isinstance(service_account_info, dict):
                                project = project or service_account_info.get("project_id")
                            credentials = service_account.Credentials.from_service_account_info(
                                service_account_info,
                                scopes=scopes,
                            )
                        else:
                            credentials, default_project = google.auth.default(scopes=scopes)
                            project = project or default_project
                        if not project:
                            return {"success": False, "message": "Google Cloud project is required"}
                        await asyncio.to_thread(credentials.refresh, GoogleAuthRequest())
                        url = (
                            f"https://{location}-aiplatform.googleapis.com/v1/projects/"
                            f"{project}/locations/{location}/publishers/google/models"
                        )
                        response = await client.get(
                            url,
                            headers={"Authorization": f"Bearer {credentials.token}"},
                        )
                    else:
                        # API-key smoke test. Production Vertex credentials are
                        # verified by the Agent runtime with official Google auth.
                        url = f"{base_url}/v1/publishers/google/models?key={api_key}"
                        response = await client.get(url)
                else:
                    return {"success": False, "message": f"Unknown API type: {api_type}"}

                if response.status_code in (200, 201):
                    return {
                        "success": True,
                        "message": "Connection successful",
                        "latency_ms": int(response.elapsed.total_seconds() * 1000),
                    }
                else:
                    return {
                        "success": False,
                        "message": f"API returned {response.status_code}: {response.text[:200]}",
                    }

        except httpx.TimeoutException:
            return {"success": False, "message": "Connection timed out"}
        except httpx.ConnectError as e:
            return {"success": False, "message": f"Connection failed: {str(e)}"}
        except Exception as e:
            return {"success": False, "message": f"Error: {str(e)}"}

    async def get_runtime_provider_config(
        self,
        tenant_id: str,
        provider_id: str,
    ) -> dict[str, Any]:
        """Return provider config with decrypted key for internal Agent calls."""
        provider = await self.get_provider(tenant_id, provider_id)
        if not provider:
            raise ValueError(f"Provider not found: {provider_id}")

        api_key = await self._get_api_key(tenant_id, provider_id)
        result = dict(provider)
        result["api_key"] = api_key
        result["runtime_provider"] = self.to_runtime_provider(result)
        result["runtime_base_url"] = self.normalize_runtime_base_url(result)
        result["allow_environment_credentials"] = self.allows_environment_credentials(result)
        return result

    @staticmethod
    def to_runtime_provider(provider: Mapping[str, Any]) -> str:
        """Map DB provider metadata to the small runtime provider vocabulary."""
        api_type = str(provider.get("api_type") or "").strip().lower()
        provider_id = str(provider.get("provider_id") or "").strip().lower()
        base_url = str(provider.get("base_url") or "").strip().lower()

        if api_type in {"google", "google-ai-studio"}:
            return "gemini"
        if api_type in {"google-vertex", "vertex"}:
            return "vertex"
        if api_type in {"dashscope", "aliyun"}:
            return "dashscope"
        if "dashscope.aliyuncs.com" in base_url or provider_id.startswith("dashscope"):
            return "dashscope"
        if api_type in {"openai", "openai-compatible"}:
            return "openai"
        return api_type

    @classmethod
    def normalize_runtime_base_url(cls, provider: Mapping[str, Any]) -> str | None:
        """Normalize provider base URL for the runtime SDK adapter."""
        base_url = str(provider.get("base_url") or "").strip().rstrip("/")
        runtime_provider = cls.to_runtime_provider(provider)
        if runtime_provider == "dashscope":
            if not base_url:
                return "https://dashscope.aliyuncs.com/compatible-mode/v1"
            if base_url.endswith("/compatible-mode"):
                return f"{base_url}/v1"
            return base_url
        return base_url or None

    @staticmethod
    def allows_environment_credentials(provider: Mapping[str, Any]) -> bool:
        """Whether this provider may rely on Agent-side environment credentials."""
        api_type = str(provider.get("api_type") or "").strip().lower()
        if api_type in {"google-vertex", "vertex"}:
            return True
        return bool(
            provider.get("allow_environment_credentials")
            or provider.get("uses_environment_credentials")
        )

    async def _get_api_key(
        self,
        tenant_id: str,
        provider_id: str,
    ) -> str | None:
        """Get decrypted API key for a provider."""
        query = """
            SELECT api_key_encrypted
            FROM llm_providers
            WHERE tenant_id = $1 AND provider_id = $2
        """
        row = await self.db.fetchrow(query, tenant_id, provider_id)
        if not row or not row["api_key_encrypted"]:
            return None
        return decrypt_value(row["api_key_encrypted"], self.encryption_key)

    def _row_to_dict(self, row) -> dict[str, Any]:
        """Convert database row to dictionary."""
        if not row:
            return {}
        result = dict(row)
        # Add has_api_key flag without exposing the actual key
        result["has_api_key"] = bool(result.pop("api_key_encrypted", None))
        result["allow_environment_credentials"] = self.allows_environment_credentials(result)
        metadata = result.get("metadata")
        if isinstance(metadata, str):
            try:
                metadata = json.loads(metadata)
            except json.JSONDecodeError:
                metadata = {}
        result["metadata"] = dict(metadata) if isinstance(metadata, dict) else {}
        return result
