"""
LLM Model Service.

Manages LLM model configurations.
"""

import json
from datetime import datetime, timezone
from decimal import Decimal
from typing import Any

from ai_gateway_core.billing import build_pricing_snapshot
from ai_gateway_core.logging import get_logger
from ai_gateway_core.models import (
    ModelCapabilityError,
    get_builtin_model_capabilities,
    merge_model_capability_profiles,
)

from ...persistence.database import DatabaseStorage
from ...services.billing.model_pricing import get_pricing_service

logger = get_logger(__name__)


class ModelProviderAmbiguous(ValueError):
    """A model name needs an explicit provider before launch or mutation."""


class ModelCapabilityRevisionConflict(RuntimeError):
    """An administrator edited an older capability profile revision."""


def _jsonb_object(value: dict[str, Any]) -> str:
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"))


class ModelService:
    """Service for managing LLM models."""

    def __init__(self, database: DatabaseStorage):
        self.db = database

    async def list_models(
        self,
        tenant_id: str,
        provider_id: str | None = None,
        include_disabled: bool = False,
        access_level: str | None = None,
    ) -> list[dict[str, Any]]:
        """
        List models for a tenant.

        Args:
            tenant_id: Tenant ID
            provider_id: Optional filter by provider
            include_disabled: Include disabled models
            access_level: Filter by max access level (public, premium, admin)
        """
        conditions = ["tenant_id = $1"]
        params: list[Any] = [tenant_id]
        param_idx = 2

        if provider_id:
            conditions.append(f"provider_id = ${param_idx}")
            params.append(provider_id)
            param_idx += 1

        if not include_disabled:
            conditions.append("is_enabled = true")

        # Access level filtering: show models up to the user's access level
        if access_level:
            if access_level == "public":
                conditions.append("access_level = 'public'")
            elif access_level == "premium":
                conditions.append("access_level IN ('public', 'premium')")
            # admin can see all

        where_clause = " AND ".join(conditions)

        query = f"""
            SELECT model_id, tenant_id, provider_id, display_name,
                   context_window, max_output_tokens, supports_vision, supports_tools,
                   catalog_capabilities, capability_overrides, capability_revision,
                   input_price_per_1k, output_price_per_1k, access_level,
                   is_enabled, sort_order, created_at, updated_at
            FROM llm_models
            WHERE {where_clause}
            ORDER BY provider_id, sort_order, display_name
        """
        rows = await self.db.fetch(query, *params)
        return [self._row_to_dict(row) for row in rows]

    async def get_model(
        self,
        tenant_id: str,
        model_id: str,
        provider_id: str | None = None,
    ) -> dict[str, Any] | None:
        """Get a specific model.

        ``provider_id`` disambiguates when the same model_id exists under
        multiple providers (e.g. ``gemini-3.1-pro-preview`` on both
        ``google`` AI Studio and ``google-vertex`` Vertex Express Mode).
        When omitted, only a unique tenant model is accepted.
        """
        if provider_id is not None:
            query = """
                SELECT model_id, tenant_id, provider_id, display_name,
                       context_window, max_output_tokens, supports_vision, supports_tools,
                       catalog_capabilities, capability_overrides, capability_revision,
                       input_price_per_1k, output_price_per_1k, access_level,
                       is_enabled, sort_order, created_at, updated_at
                FROM llm_models
                WHERE tenant_id = $1 AND provider_id = $2 AND model_id = $3
            """
            row = await self.db.fetchrow(query, tenant_id, provider_id, model_id)
        else:
            query = """
                SELECT COUNT(*) OVER () AS provider_matches,
                       model_id, tenant_id, provider_id, display_name,
                       context_window, max_output_tokens, supports_vision, supports_tools,
                       catalog_capabilities, capability_overrides, capability_revision,
                       input_price_per_1k, output_price_per_1k, access_level,
                       is_enabled, sort_order, created_at, updated_at
                FROM llm_models
                WHERE tenant_id = $1 AND model_id = $2
                ORDER BY sort_order, provider_id
                LIMIT 1
            """
            row = await self.db.fetchrow(query, tenant_id, model_id)
            if row and int(row.get("provider_matches") or 1) > 1:
                raise ModelProviderAmbiguous("model_provider_required")
        return self._row_to_dict(row) if row else None

    async def get_provider_model(
        self,
        tenant_id: str,
        provider_id: str,
        model_id: str,
    ) -> dict[str, Any] | None:
        """Get a model scoped to one provider to avoid model_id ambiguity."""
        return await self.get_model(
            tenant_id=tenant_id,
            model_id=model_id,
            provider_id=provider_id,
        )

    async def create_model(
        self,
        tenant_id: str,
        model_id: str,
        provider_id: str,
        display_name: str,
        context_window: int = 128000,
        max_output_tokens: int = 4096,
        supports_vision: bool = False,
        supports_tools: bool = True,
        input_price_per_1k: Decimal = Decimal("0"),
        output_price_per_1k: Decimal = Decimal("0"),
        access_level: str = "public",
        is_enabled: bool = True,
        sort_order: int = 0,
        catalog_capabilities: dict[str, Any] | None = None,
        capability_overrides: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        """Create a new model."""
        build_pricing_snapshot(
            tenant_id=tenant_id, provider_id=provider_id, model_id=model_id,
            input_price_per_1k=input_price_per_1k, output_price_per_1k=output_price_per_1k,
        )
        catalog_capabilities = dict(catalog_capabilities or {})
        capability_overrides = dict(capability_overrides or {})
        # The read path (_row_to_dict) synthesizes the builtin catalog into
        # ``catalog_capabilities`` whenever the persisted catalog is empty, so
        # create-time validation must use the same source.  Otherwise an
        # override that is valid for the model's builtin adapter is rejected
        # here, and an override that conflicts with the builtin profile is
        # accepted but silently discarded on every read.
        validation_catalog = catalog_capabilities or (
            get_builtin_model_capabilities(provider_id, model_id) or {}
        )
        merge_model_capability_profiles(
            validation_catalog,
            capability_overrides,
            supports_tools=supports_tools,
            supports_vision=supports_vision,
        )
        query = """
            INSERT INTO llm_models (
                model_id, tenant_id, provider_id, display_name,
                context_window, max_output_tokens, supports_vision, supports_tools,
                catalog_capabilities, capability_overrides, capability_revision,
                input_price_per_1k, output_price_per_1k, access_level,
                is_enabled, sort_order
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 1, $11, $12, $13, $14, $15)
            RETURNING model_id, tenant_id, provider_id, display_name,
                      context_window, max_output_tokens, supports_vision, supports_tools,
                      catalog_capabilities, capability_overrides, capability_revision,
                      input_price_per_1k, output_price_per_1k, access_level,
                      is_enabled, sort_order, created_at, updated_at
        """
        row = await self.db.fetchrow(
            query,
            model_id,
            tenant_id,
            provider_id,
            display_name,
            context_window,
            max_output_tokens,
            supports_vision,
            supports_tools,
            _jsonb_object(catalog_capabilities),
            _jsonb_object(capability_overrides),
            input_price_per_1k,
            output_price_per_1k,
            access_level,
            is_enabled,
            sort_order,
        )
        row_dict = self._row_to_dict(row)

        # Refresh price visibility without writing the global catalog
        await self._sync_single_model_pricing(
            tenant_id=tenant_id,
            model_id=model_id,
            input_price_per_1k=float(input_price_per_1k),
            output_price_per_1k=float(output_price_per_1k),
            provider=provider_id,
            display_name=display_name,
            context_window=context_window,
            max_output_tokens=max_output_tokens,
            supports_vision=supports_vision,
            supports_tools=supports_tools,
        )

        return row_dict

    async def upsert_model_from_catalog(
        self,
        *,
        tenant_id: str,
        provider_id: str,
        model_id: str,
        display_name: str,
        context_window: int = 128000,
        max_output_tokens: int = 4096,
        supports_vision: bool = False,
        supports_tools: bool = True,
        input_price_per_1k: Decimal = Decimal("0"),
        output_price_per_1k: Decimal = Decimal("0"),
        access_level: str = "public",
        sort_order: int = 0,
        enable_new: bool = True,
        catalog_capabilities: dict[str, Any] | None = None,
    ) -> tuple[str, dict[str, Any], bool]:
        """Create or refresh catalog metadata for a provider-scoped model.

        Existing rows keep administrator-owned access, input/output prices and
        ``is_enabled``. Catalog defaults initialize new rows only; a later sync
        cannot broaden visibility, replace tenant prices, or re-enable a model.

        The third return value reports whether the effective capability
        profile changed (a new row, or a ``capability_revision`` bump), so
        callers can publish exact invalidations on the model-config channel.
        """
        existing = await self.get_provider_model(
            tenant_id=tenant_id,
            provider_id=provider_id,
            model_id=model_id,
        )
        if existing:
            catalog_update = (
                catalog_capabilities
                if dict(existing.get("catalog_capabilities") or {})
                != dict(catalog_capabilities or {})
                else None
            )
            updated = await self.update_model(
                tenant_id=tenant_id,
                provider_id=provider_id,
                model_id=model_id,
                display_name=display_name,
                context_window=context_window,
                max_output_tokens=max_output_tokens,
                supports_vision=supports_vision,
                supports_tools=supports_tools,
                # Tenant access and prices are administrator-owned after creation.
                # Omit them, rather than rewriting a stale pre-sync snapshot.
                sort_order=sort_order,
                catalog_capabilities=catalog_update,
            )
            result = updated or existing
            capability_changed = int(result.get("capability_revision") or 1) != int(
                existing.get("capability_revision") or 1
            )
            return "updated", result, capability_changed

        created = await self.create_model(
            tenant_id=tenant_id,
            provider_id=provider_id,
            model_id=model_id,
            display_name=display_name,
            context_window=context_window,
            max_output_tokens=max_output_tokens,
            supports_vision=supports_vision,
            supports_tools=supports_tools,
            input_price_per_1k=input_price_per_1k,
            output_price_per_1k=output_price_per_1k,
            access_level=access_level,
            is_enabled=enable_new,
            sort_order=sort_order,
            catalog_capabilities=catalog_capabilities,
        )
        return "created", created, True

    async def update_model(
        self,
        tenant_id: str,
        model_id: str,
        new_model_id: str | None = None,
        display_name: str | None = None,
        context_window: int | None = None,
        max_output_tokens: int | None = None,
        supports_vision: bool | None = None,
        supports_tools: bool | None = None,
        input_price_per_1k: Decimal | None = None,
        output_price_per_1k: Decimal | None = None,
        access_level: str | None = None,
        is_enabled: bool | None = None,
        sort_order: int | None = None,
        provider_id: str | None = None,
        catalog_capabilities: dict[str, Any] | None = None,
        capability_overrides: dict[str, Any] | None = None,
        expected_capability_revision: int | None = None,
    ) -> dict[str, Any] | None:
        """Update a model. ``new_model_id`` renames the primary key.

        ``provider_id`` disambiguates when the same model_id exists under
        multiple providers. Post-migration-055 callers should pass it to
        avoid updating the wrong row. Omission is accepted only for a unique
        model; the mutation then uses that exact provider identity.
        """
        for price in (input_price_per_1k, output_price_per_1k):
            if price is not None:
                number = Decimal(str(price))
                if not number.is_finite() or number < 0:
                    raise ValueError("invalid_model_price")
        current_model = None
        if provider_id is None:
            current_model = await self.get_model(tenant_id, model_id)
            if current_model is None:
                return None
            provider_id = current_model["provider_id"]
        updates = []
        params = []
        param_idx = 1

        if new_model_id is not None and new_model_id != model_id:
            updates.append(f"model_id = ${param_idx}")
            params.append(new_model_id)
            param_idx += 1

        if display_name is not None:
            updates.append(f"display_name = ${param_idx}")
            params.append(display_name)
            param_idx += 1

        if context_window is not None:
            updates.append(f"context_window = ${param_idx}")
            params.append(context_window)
            param_idx += 1

        if max_output_tokens is not None:
            updates.append(f"max_output_tokens = ${param_idx}")
            params.append(max_output_tokens)
            param_idx += 1

        if supports_vision is not None:
            updates.append(f"supports_vision = ${param_idx}")
            params.append(supports_vision)
            param_idx += 1

        if supports_tools is not None:
            updates.append(f"supports_tools = ${param_idx}")
            params.append(supports_tools)
            param_idx += 1

        if input_price_per_1k is not None:
            updates.append(f"input_price_per_1k = ${param_idx}")
            params.append(input_price_per_1k)
            param_idx += 1

        if output_price_per_1k is not None:
            updates.append(f"output_price_per_1k = ${param_idx}")
            params.append(output_price_per_1k)
            param_idx += 1

        if access_level is not None:
            updates.append(f"access_level = ${param_idx}")
            params.append(access_level)
            param_idx += 1

        if is_enabled is not None:
            updates.append(f"is_enabled = ${param_idx}")
            params.append(is_enabled)
            param_idx += 1

        if sort_order is not None:
            updates.append(f"sort_order = ${param_idx}")
            params.append(sort_order)
            param_idx += 1

        # The effective capability profile depends on the catalog/override
        # layers AND on the supports_tools/supports_vision columns (the safe
        # base profile derives tools.function_calling, streaming.tool_call_deltas
        # and modalities.input from them).  A change to any of these must bump
        # capability_revision so one revision always means one immutable
        # effective profile (ADR-005 invariant 6).
        capability_touched = (
            catalog_capabilities is not None
            or capability_overrides is not None
            or supports_tools is not None
            or supports_vision is not None
        )
        revision_guarded = capability_touched and expected_capability_revision is not None
        if capability_touched:
            current = current_model or await self.get_model(
                tenant_id,
                model_id,
                provider_id=provider_id,
            )
            if current is None:
                return None
            next_catalog = (
                dict(catalog_capabilities)
                if catalog_capabilities is not None
                else dict(current.get("catalog_capabilities") or {})
            )
            next_overrides = (
                dict(capability_overrides)
                if capability_overrides is not None
                else dict(current.get("capability_overrides") or {})
            )
            next_supports_tools = (
                supports_tools
                if supports_tools is not None
                else bool(current.get("supports_tools", True))
            )
            next_supports_vision = (
                supports_vision
                if supports_vision is not None
                else bool(current.get("supports_vision", False))
            )
            merge_model_capability_profiles(
                next_catalog,
                next_overrides,
                supports_tools=next_supports_tools,
                supports_vision=next_supports_vision,
            )
            if expected_capability_revision is not None and int(
                current.get("capability_revision") or 1
            ) != expected_capability_revision:
                raise ModelCapabilityRevisionConflict("model capability revision conflict")
            if catalog_capabilities is not None:
                updates.append(f"catalog_capabilities = ${param_idx}")
                params.append(_jsonb_object(next_catalog))
                param_idx += 1
            if capability_overrides is not None:
                updates.append(f"capability_overrides = ${param_idx}")
                params.append(_jsonb_object(next_overrides))
                param_idx += 1
            capability_value_changed = (
                catalog_capabilities is not None
                and next_catalog != dict(current.get("catalog_capabilities") or {})
            ) or (
                capability_overrides is not None
                and next_overrides != dict(current.get("capability_overrides") or {})
            ) or (
                bool(current.get("supports_tools", True)) != next_supports_tools
                or bool(current.get("supports_vision", False)) != next_supports_vision
            )
            if capability_value_changed:
                updates.append("capability_revision = capability_revision + 1")

        if not updates:
            return await self.get_model(tenant_id, model_id, provider_id=provider_id)

        updates.append(f"updated_at = ${param_idx}")
        params.append(datetime.now(timezone.utc))
        param_idx += 1

        # WHERE clause composition. With the post-migration-055 PK of
        # (tenant_id, provider_id, model_id), callers that know their
        # provider MUST pass it here — otherwise the UPDATE could match
        # multiple rows when the same model_id exists under ``google``
        # and ``google-vertex``. Callers that don't pass provider_id get
        # the old 2-column semantics; at worst that's a no-op when no
        # row matches.
        if provider_id is not None:
            params.extend([tenant_id, provider_id, model_id])
            where_clause = (
                f"WHERE tenant_id = ${param_idx} "
                f"AND provider_id = ${param_idx + 1} "
                f"AND model_id = ${param_idx + 2}"
            )
            param_idx += 3
        else:
            params.extend([tenant_id, model_id])
            where_clause = (
                f"WHERE tenant_id = ${param_idx} AND model_id = ${param_idx + 1}"
            )
            param_idx += 2

        if revision_guarded:
            # The pre-check read above is not atomic with this UPDATE; a
            # concurrent writer can bump the revision between the read and
            # here.  The WHERE-clause guard closes that race: the UPDATE then
            # matches no row, and the existence re-check below reports the
            # conflict instead of silently overwriting the newer profile.
            params.append(expected_capability_revision)
            where_clause += f" AND capability_revision = ${param_idx}"
            param_idx += 1

        query = f"""
            UPDATE llm_models
            SET {", ".join(updates)}
            {where_clause}
            RETURNING model_id, tenant_id, provider_id, display_name,
                      context_window, max_output_tokens, supports_vision, supports_tools,
                      catalog_capabilities, capability_overrides, capability_revision,
                      input_price_per_1k, output_price_per_1k, access_level,
                      is_enabled, sort_order, created_at, updated_at
        """
        row = await self.db.fetchrow(query, *params)

        if row:
            row_dict = self._row_to_dict(row)
            await self._sync_single_model_pricing(
                tenant_id=tenant_id, model_id=row["model_id"],
            )
            return row_dict

        if revision_guarded:
            # The UPDATE matched no row.  The row existed when the pre-check
            # read it, so either it was deleted concurrently (None -> 404) or
            # the revision moved between the read and the UPDATE (conflict).
            existing = await self.get_model(
                tenant_id, model_id, provider_id=provider_id
            )
            if existing is not None:
                raise ModelCapabilityRevisionConflict(
                    "model capability revision conflict"
                )
        return None

    async def reset_model_capabilities(
        self,
        *,
        tenant_id: str,
        model_id: str,
        provider_id: str | None = None,
        expected_capability_revision: int | None = None,
    ) -> dict[str, Any] | None:
        return await self.update_model(
            tenant_id=tenant_id,
            model_id=model_id,
            provider_id=provider_id,
            capability_overrides={},
            expected_capability_revision=expected_capability_revision,
        )

    async def delete_model(
        self,
        tenant_id: str,
        model_id: str,
        provider_id: str | None = None,
    ) -> bool:
        """Delete a model.

        ``provider_id`` scopes the delete to one row when the same
        model_id exists under multiple providers. Without it, callers
        could unintentionally delete more than one row post-migration-055
        — flag this in any new caller and pass provider_id wherever the
        lookup context has it.
        """
        if provider_id is None:
            current_model = await self.get_model(tenant_id, model_id)
            if current_model is None:
                return False
            provider_id = current_model["provider_id"]
        if provider_id is not None:
            result = await self.db.execute(
                "DELETE FROM llm_models WHERE tenant_id = $1 AND provider_id = $2 AND model_id = $3",
                tenant_id,
                provider_id,
                model_id,
            )
        else:
            result = await self.db.execute(
                "DELETE FROM llm_models WHERE tenant_id = $1 AND model_id = $2",
                tenant_id,
                model_id,
            )
        return "DELETE" in str(result) and "DELETE 0" not in str(result)

    async def toggle_model(
        self,
        tenant_id: str,
        model_id: str,
        is_enabled: bool,
        provider_id: str | None = None,
    ) -> dict[str, Any] | None:
        """Toggle model enabled state."""
        return await self.update_model(
            tenant_id,
            model_id,
            is_enabled=is_enabled,
            provider_id=provider_id,
        )

    async def get_models_for_assistant(
        self,
        tenant_id: str,
        user_access_level: str = "public",
    ) -> list[dict[str, Any]]:
        """
        Get models available for the AI assistant.

        Only returns enabled models from enabled providers that the user has access to.
        """
        # Map user access level to SQL filter
        if user_access_level == "admin":
            access_filter = ""  # Admin sees all
        elif user_access_level == "premium":
            access_filter = "AND m.access_level IN ('public', 'premium')"
        else:
            access_filter = "AND m.access_level = 'public'"

        query = f"""
            SELECT m.model_id, m.tenant_id, m.provider_id, m.display_name,
                   m.context_window, m.max_output_tokens, m.supports_vision, m.supports_tools,
                   m.catalog_capabilities, m.capability_overrides, m.capability_revision,
                   m.input_price_per_1k, m.output_price_per_1k, m.access_level,
                   m.is_enabled, m.sort_order, m.created_at, m.updated_at
            FROM llm_models m
            INNER JOIN llm_providers p ON m.tenant_id = p.tenant_id AND m.provider_id = p.provider_id
            WHERE m.tenant_id = $1
              AND m.is_enabled = true
              AND p.is_enabled = true
              {access_filter}
            ORDER BY p.display_name, m.sort_order, m.display_name
        """
        rows = await self.db.fetch(query, tenant_id)
        return [self._row_to_dict(row) for row in rows]

    async def sync_pricing_from_llm_models(
        self,
        tenant_id: str,
        include_disabled: bool = True,
    ) -> int:
        """
        Refresh price visibility for the legacy startup hook.

        Tenant prices stay in llm_models; model_pricing is a global catalog.

        Returns:
            Number of scoped model rows whose visibility was refreshed.
        """
        models = await self.list_models(
            tenant_id=tenant_id,
            include_disabled=include_disabled,
        )
        synced = 0
        for row in models:
            ok = await self._sync_single_model_pricing(
                tenant_id=tenant_id,
                model_id=row["model_id"],
                input_price_per_1k=float(row.get("input_price_per_1k") or 0),
                output_price_per_1k=float(row.get("output_price_per_1k") or 0),
                provider=row.get("provider_id"),
                display_name=row.get("display_name"),
                context_window=row.get("context_window"),
                max_output_tokens=row.get("max_output_tokens"),
                supports_vision=row.get("supports_vision"),
                supports_tools=row.get("supports_tools"),
            )
            if ok:
                synced += 1
        return synced

    async def _sync_single_model_pricing(
        self, *, tenant_id: str, model_id: str, **_pricing: Any,
    ) -> bool:
        del tenant_id, model_id
        # No cross-tenant mirror. Reads resolve llm_models by compound identity.
        get_pricing_service().invalidate_cache()
        return True

    def _row_to_dict(self, row) -> dict[str, Any]:
        """Convert database row to dictionary."""
        if not row:
            return {}
        result = dict(row)
        result.pop("provider_matches", None)
        result["pricing_snapshot"] = build_pricing_snapshot(
            tenant_id=str(result.get("tenant_id") or ""),
            provider_id=str(result.get("provider_id") or ""),
            model_id=str(result.get("model_id") or ""),
            input_price_per_1k=result.get("input_price_per_1k") or 0,
            output_price_per_1k=result.get("output_price_per_1k") or 0,
        )
        for field in ("catalog_capabilities", "capability_overrides"):
            raw = result.get(field)
            if isinstance(raw, str):
                try:
                    parsed = json.loads(raw)
                except json.JSONDecodeError:
                    parsed = {}
                result[field] = parsed if isinstance(parsed, dict) else {}
            elif not isinstance(raw, dict):
                result[field] = {}
        # Convert Decimal to float for JSON serialization
        if "input_price_per_1k" in result and result["input_price_per_1k"] is not None:
            result["input_price_per_1k"] = float(result["input_price_per_1k"])
        if "output_price_per_1k" in result and result["output_price_per_1k"] is not None:
            result["output_price_per_1k"] = float(result["output_price_per_1k"])
        if not result.get("catalog_capabilities"):
            result["catalog_capabilities"] = get_builtin_model_capabilities(
                str(result.get("provider_id") or ""),
                str(result.get("model_id") or ""),
            ) or {}
        try:
            result["effective_capabilities"] = merge_model_capability_profiles(
                result.get("catalog_capabilities") or {},
                result.get("capability_overrides") or {},
                supports_tools=bool(result.get("supports_tools", True)),
                supports_vision=bool(result.get("supports_vision", False)),
            )
        except ModelCapabilityError:
            logger.warning(
                "Rejected invalid persisted capability profile model_id=%s provider_id=%s",
                result.get("model_id"),
                result.get("provider_id"),
            )
            result["effective_capabilities"] = merge_model_capability_profiles(
                {},
                {},
                supports_tools=bool(result.get("supports_tools", True)),
                supports_vision=bool(result.get("supports_vision", False)),
            )
        return result
