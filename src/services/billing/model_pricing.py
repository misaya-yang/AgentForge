"""
Model Pricing Service - Manage model pricing configurations.

This service handles:
- Fetching model pricing from database
- Caching pricing for performance
- Calculating costs for token usage
- Managing model pricing updates
"""

from __future__ import annotations

import logging
import time
from dataclasses import dataclass
from decimal import Decimal
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from ...persistence.database import DatabaseStorage

from .pricing_catalog import resolve_pricing_with_status, to_model_pricing_defaults

logger = logging.getLogger(__name__)

# Global singleton instance
_pricing_service: ModelPricingService | None = None


# Default pricing when database is not available
DEFAULT_PRICING: dict[str, dict[str, Any]] = to_model_pricing_defaults()


@dataclass
class ModelPrice:
    """Model pricing information."""

    model: str
    provider: str
    input_price_per_1k: Decimal
    output_price_per_1k: Decimal
    display_name: str = ""
    context_window: int = 0
    max_output_tokens: int = 0
    supports_vision: bool = False
    supports_tools: bool = True
    is_active: bool = True
    pricing_status: str = "catalog"

    def calculate_cost(self, input_tokens: int, output_tokens: int) -> dict[str, Any]:
        """
        Calculate cost for given token usage.

        Returns:
            Dictionary with cost breakdown in USD and cents
        """
        input_cost = (Decimal(input_tokens) / 1000) * self.input_price_per_1k
        output_cost = (Decimal(output_tokens) / 1000) * self.output_price_per_1k
        total_cost = input_cost + output_cost

        return {
            "input_cost_usd": float(input_cost),
            "output_cost_usd": float(output_cost),
            "total_cost_usd": float(total_cost),
            "input_cost_cents": int(input_cost * 100),
            "output_cost_cents": int(output_cost * 100),
            "total_cost_cents": int(total_cost * 100),
            "pricing_status": self.pricing_status,
        }

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary."""
        return {
            "model": self.model,
            "provider": self.provider,
            "display_name": self.display_name or self.model,
            "pricing": {
                "input_per_1k_usd": float(self.input_price_per_1k),
                "output_per_1k_usd": float(self.output_price_per_1k),
            },
            "capabilities": {
                "context_window": self.context_window,
                "max_output_tokens": self.max_output_tokens,
                "supports_vision": self.supports_vision,
                "supports_tools": self.supports_tools,
            },
            "is_active": self.is_active,
        }


class ModelPricingService:
    """
    Service for managing model pricing.

    Features:
    - Database-backed pricing with caching
    - Cost calculation utilities
    - Model capability queries
    """

    def __init__(
        self,
        database: DatabaseStorage | None = None,
        cache_ttl_seconds: float = 300,  # 5 minutes
    ):
        self.database = database
        self.cache_ttl = cache_ttl_seconds
        self._cache: dict[str, ModelPrice] = {}
        self._cache_time: float = 0
        self._all_models_cache: list[ModelPrice] = []

    def set_database(self, database: DatabaseStorage) -> None:
        """Set or update the database storage instance."""
        self.database = database

    @staticmethod
    def _with_pricing_status(price: ModelPrice, pricing_status: str) -> ModelPrice:
        return ModelPrice(
            model=price.model,
            provider=price.provider,
            input_price_per_1k=price.input_price_per_1k,
            output_price_per_1k=price.output_price_per_1k,
            display_name=price.display_name,
            context_window=price.context_window,
            max_output_tokens=price.max_output_tokens,
            supports_vision=price.supports_vision,
            supports_tools=price.supports_tools,
            is_active=price.is_active,
            pricing_status=pricing_status,
        )

    def invalidate_cache(self) -> None:
        self._cache_time = 0

    async def get_model_pricing(
        self, model: str, *, tenant_id: str | None = None, provider_id: str | None = None,
    ) -> ModelPrice:
        """
        Get pricing for a specific model.

        Args:
            model: Model name/ID

        Returns:
            ModelPrice instance
        """
        if tenant_id is not None and self.database and self.database._pool:
            async with self.database._pool.acquire() as conn:
                rows = await conn.fetch(
                    """SELECT model_id, provider_id, input_price_per_1k, output_price_per_1k
                       FROM llm_models WHERE tenant_id = $1 AND model_id = $2
                       AND ($3::text IS NULL OR provider_id = $3) LIMIT 2""",
                    tenant_id, model, provider_id,
                )
            if len(rows) > 1:
                raise ValueError("model_provider_required")
            if rows:
                row = rows[0]
                return ModelPrice(
                    model=model, provider=row["provider_id"],
                    input_price_per_1k=Decimal(str(row["input_price_per_1k"])),
                    output_price_per_1k=Decimal(str(row["output_price_per_1k"])),
                    pricing_status="tenant_model",
                )
            # A scoped miss must never read a different tenant's legacy mirror.
            return self._get_default_price(model)

        if tenant_id is not None:
            return self._get_default_price(model)

        await self._ensure_cache()

        # Direct match
        if model in self._cache:
            return self._cache[model]

        # Partial match (for model variants)
        for cached_model, price in self._cache.items():
            if model.startswith(cached_model):
                return self._with_pricing_status(price, "provider_model")

        # Return default
        return self._cache.get("default", self._get_default_price(model))

    async def get_all_pricing(self, active_only: bool = True) -> list[ModelPrice]:
        """Get all model pricing."""
        await self._ensure_cache()

        if active_only:
            return [p for p in self._all_models_cache if p.is_active]
        return self._all_models_cache.copy()

    async def calculate_cost(
        self,
        model: str,
        input_tokens: int,
        output_tokens: int,
        *, tenant_id: str | None = None, provider_id: str | None = None,
    ) -> dict[str, Any]:
        """
        Calculate cost for token usage.

        Returns:
            Cost breakdown dictionary
        """
        price = await self.get_model_pricing(model, tenant_id=tenant_id, provider_id=provider_id)
        cost = price.calculate_cost(input_tokens, output_tokens)
        cost["model"] = price.model
        cost["provider"] = price.provider
        cost["pricing_status"] = price.pricing_status
        return cost

    async def update_pricing(
        self,
        model: str,
        input_price_per_1k: float,
        output_price_per_1k: float,
        provider: str | None = None,
        display_name: str | None = None,
        context_window: int | None = None,
        max_output_tokens: int | None = None,
        supports_vision: bool | None = None,
        supports_tools: bool | None = None,
    ) -> dict[str, Any]:
        """Update or create model pricing."""
        if not self.database or not self.database._pool:
            return {"error": "Database not available"}

        try:
            async with self.database._pool.acquire() as conn:
                await conn.execute(
                    """
                    INSERT INTO model_pricing (
                        model, provider, display_name,
                        input_price_per_1k, output_price_per_1k,
                        context_window, max_output_tokens,
                        supports_vision, supports_tools
                    ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                    ON CONFLICT (model) DO UPDATE SET
                        input_price_per_1k = EXCLUDED.input_price_per_1k,
                        output_price_per_1k = EXCLUDED.output_price_per_1k,
                        provider = COALESCE(EXCLUDED.provider, model_pricing.provider),
                        display_name = COALESCE(EXCLUDED.display_name, model_pricing.display_name),
                        context_window = COALESCE(EXCLUDED.context_window, model_pricing.context_window),
                        max_output_tokens = COALESCE(EXCLUDED.max_output_tokens, model_pricing.max_output_tokens),
                        supports_vision = COALESCE(EXCLUDED.supports_vision, model_pricing.supports_vision),
                        supports_tools = COALESCE(EXCLUDED.supports_tools, model_pricing.supports_tools),
                        updated_at = CURRENT_TIMESTAMP
                    """,
                    model,
                    provider,
                    display_name,
                    input_price_per_1k,
                    output_price_per_1k,
                    context_window,
                    max_output_tokens,
                    supports_vision,
                    supports_tools,
                )

            # Invalidate cache
            self._cache_time = 0

            return {"success": True, "model": model}

        except Exception as e:
            logger.error(f"Failed to update model pricing: {e}")
            return {"error": str(e)}

    async def set_model_active(self, model: str, is_active: bool) -> bool:
        """Set model active status."""
        if not self.database or not self.database._pool:
            return False

        try:
            async with self.database._pool.acquire() as conn:
                await conn.execute(
                    """
                    UPDATE model_pricing
                    SET is_active = $2, updated_at = CURRENT_TIMESTAMP
                    WHERE model = $1
                    """,
                    model,
                    is_active,
                )

            # Invalidate cache
            self._cache_time = 0
            return True

        except Exception as e:
            logger.error(f"Failed to set model active status: {e}")
            return False

    async def _ensure_cache(self) -> None:
        """Ensure cache is populated and fresh."""
        now = time.time()
        if now - self._cache_time < self.cache_ttl and self._cache:
            return

        await self._refresh_cache()

    async def _refresh_cache(self) -> None:
        """Refresh pricing cache from database."""
        if not self.database or not self.database._pool:
            # Use default pricing
            self._load_default_pricing()
            return

        try:
            async with self.database._pool.acquire() as conn:
                rows = await conn.fetch(
                    """
                    SELECT
                        model, provider, display_name,
                        input_price_per_1k, output_price_per_1k,
                        context_window, max_output_tokens,
                        supports_vision, supports_tools, is_active
                    FROM model_pricing
                    """
                )

                self._cache.clear()
                self._all_models_cache.clear()

                for row in rows:
                    price = ModelPrice(
                        model=row["model"],
                        provider=row["provider"] or "unknown",
                        display_name=row["display_name"] or row["model"],
                        input_price_per_1k=Decimal(str(row["input_price_per_1k"])),
                        output_price_per_1k=Decimal(str(row["output_price_per_1k"])),
                        context_window=row["context_window"] or 0,
                        max_output_tokens=row["max_output_tokens"] or 0,
                        supports_vision=row["supports_vision"] or False,
                        supports_tools=row["supports_tools"]
                        if row["supports_tools"] is not None
                        else True,
                        is_active=row["is_active"] if row["is_active"] is not None else True,
                        pricing_status="catalog",
                    )
                    self._cache[price.model] = price
                    self._all_models_cache.append(price)

                self._cache_time = time.time()
                logger.debug(f"Refreshed pricing cache with {len(rows)} models")

        except Exception as e:
            logger.warning(f"Failed to refresh pricing cache from database: {e}")
            self._load_default_pricing()

    def _load_default_pricing(self) -> None:
        """Load default pricing into cache."""
        self._cache.clear()
        self._all_models_cache.clear()

        for model, pricing in DEFAULT_PRICING.items():
            price = ModelPrice(
                model=model,
                provider=pricing.get("provider", "unknown"),
                input_price_per_1k=Decimal(str(pricing["input"])),
                output_price_per_1k=Decimal(str(pricing["output"])),
                pricing_status="catalog" if model != "default" else "unknown",
            )
            self._cache[model] = price
            self._all_models_cache.append(price)

        self._cache_time = time.time()

    def _get_default_price(self, model: str) -> ModelPrice:
        """Get default price for unknown model."""
        pricing, pricing_status = resolve_pricing_with_status(model)
        return ModelPrice(
            model=model,
            provider=pricing.get("provider", "unknown"),
            input_price_per_1k=Decimal(str(pricing["input"])),
            output_price_per_1k=Decimal(str(pricing["output"])),
            pricing_status=pricing_status,
        )


def get_pricing_service() -> ModelPricingService:
    """Get the global ModelPricingService singleton."""
    global _pricing_service
    if _pricing_service is None:
        _pricing_service = ModelPricingService()
    return _pricing_service


def init_pricing_service(database: DatabaseStorage) -> ModelPricingService:
    """Initialize the global ModelPricingService with database storage."""
    global _pricing_service
    if _pricing_service is None:
        _pricing_service = ModelPricingService(database)
    else:
        _pricing_service.set_database(database)
    return _pricing_service
