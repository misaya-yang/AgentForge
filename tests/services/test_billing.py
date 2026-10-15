"""
Billing Services Tests

Tests for:
- QuotaService - User quota management
- ModelPricingService - Model pricing and cost calculation
- UsageRecorder - Usage recording and querying
"""

from datetime import datetime, timezone
from decimal import Decimal
from unittest.mock import AsyncMock, MagicMock

import pytest

from src.api.v1.proxy import _estimate_tokens_from_payload
from src.core.utils import estimate_tokens
from src.services.billing.model_pricing import DEFAULT_PRICING, ModelPrice, ModelPricingService
from src.services.billing.quota_service import QuotaService, QuotaStatus


class TestModelPricingService:
    """Model pricing service tests"""

    @pytest.fixture
    def pricing_service(self):
        """Pricing service instance without DB (uses default pricing)"""
        # No database provided - will use DEFAULT_PRICING
        return ModelPricingService(database=None)

    @pytest.mark.asyncio
    async def test_calculate_cost_gpt4(self, pricing_service):
        """Test cost calculation for GPT-4"""
        # GPT-4: $0.03/1k input, $0.06/1k output
        result = await pricing_service.calculate_cost(
            model="gpt-4",
            input_tokens=1000,
            output_tokens=500,
        )

        # 1000 input tokens = $0.03 = 3 cents
        # 500 output tokens = $0.03 = 3 cents
        assert result["input_cost_cents"] == 3
        assert result["output_cost_cents"] == 3
        assert result["total_cost_cents"] == 6

    @pytest.mark.asyncio
    async def test_calculate_cost_claude_sonnet(self, pricing_service):
        """Test cost calculation for Claude Sonnet"""
        # Claude 3 Sonnet: $0.003/1k input, $0.015/1k output
        result = await pricing_service.calculate_cost(
            model="claude-3-sonnet",
            input_tokens=10000,
            output_tokens=2000,
        )

        # 10000 input tokens = $0.03 = 3 cents
        # 2000 output tokens = $0.03 = 3 cents
        assert result["input_cost_cents"] == 3
        assert result["output_cost_cents"] == 3
        assert result["total_cost_cents"] == 6

    @pytest.mark.asyncio
    async def test_calculate_cost_unknown_model_uses_default(self, pricing_service):
        """Test that unknown models use default pricing as fallback"""
        # Default: $0.001/1k input, $0.002/1k output
        result = await pricing_service.calculate_cost(
            model="unknown-model-xyz",
            input_tokens=10000,
            output_tokens=10000,
        )

        # 10000 input tokens = $0.01 = 1 cent
        # 10000 output tokens = $0.02 = 2 cents
        assert result["input_cost_cents"] == 1
        assert result["output_cost_cents"] == 2
        assert result["total_cost_cents"] == 3

    @pytest.mark.asyncio
    async def test_calculate_cost_zero_tokens(self, pricing_service):
        """Test cost calculation with zero tokens"""
        result = await pricing_service.calculate_cost(
            model="gpt-4",
            input_tokens=0,
            output_tokens=0,
        )

        assert result["input_cost_cents"] == 0
        assert result["output_cost_cents"] == 0
        assert result["total_cost_cents"] == 0

    def test_default_pricing_contains_major_models(self):
        """Test that default pricing includes major model families"""
        assert "gpt-4" in DEFAULT_PRICING
        assert "gpt-4-turbo" in DEFAULT_PRICING
        assert "gpt-3.5-turbo" in DEFAULT_PRICING
        assert "claude-3-opus" in DEFAULT_PRICING
        assert "claude-3-sonnet" in DEFAULT_PRICING
        assert "claude-3-haiku" in DEFAULT_PRICING


class TestModelPrice:
    """Test ModelPrice dataclass"""

    def test_model_price_cost_calculation(self):
        """Test ModelPrice cost calculation method"""
        price = ModelPrice(
            model="test-model",
            provider="test",
            input_price_per_1k=Decimal("0.01"),
            output_price_per_1k=Decimal("0.02"),
        )

        result = price.calculate_cost(input_tokens=5000, output_tokens=2000)

        # 5000 input tokens @ $0.01/1k = $0.05 = 5 cents
        # 2000 output tokens @ $0.02/1k = $0.04 = 4 cents
        assert result["input_cost_cents"] == 5
        assert result["output_cost_cents"] == 4
        assert result["total_cost_cents"] == 9

    def test_model_price_to_dict(self):
        """Test ModelPrice to_dict method"""
        price = ModelPrice(
            model="test-model",
            provider="test-provider",
            input_price_per_1k=Decimal("0.01"),
            output_price_per_1k=Decimal("0.02"),
            display_name="Test Model",
            context_window=128000,
            supports_vision=True,
        )

        result = price.to_dict()

        assert result["model"] == "test-model"
        assert result["provider"] == "test-provider"
        assert result["display_name"] == "Test Model"
        assert result["pricing"]["input_per_1k_usd"] == 0.01
        assert result["pricing"]["output_per_1k_usd"] == 0.02
        assert result["capabilities"]["context_window"] == 128000
        assert result["capabilities"]["supports_vision"] is True


class TestQuotaService:
    """Quota service tests"""

    @pytest.fixture
    def mock_db(self):
        """Mock database manager"""
        db = MagicMock()
        db.pool = MagicMock()
        return db

    @pytest.fixture
    def quota_service(self, mock_db):
        """Quota service instance with mock DB"""
        return QuotaService(mock_db)

    @pytest.mark.asyncio
    async def test_check_quota_blocked_user(self, quota_service, mock_db):
        """Test quota check for blocked user"""
        # Mock blocked user
        mock_conn = AsyncMock()
        mock_conn.fetchrow = AsyncMock(
            return_value={
                "daily_token_limit": 100000,
                "monthly_cost_limit_cents": 10000,
                "current_daily_tokens": 50000,
                "current_monthly_cost_cents": 5000,
                "requests_per_minute": 60,
                "is_blocked": True,
                "blocked_reason": "Suspicious activity",
                "warning_threshold": 0.8,
            }
        )
        mock_db.pool.acquire = AsyncMock()
        mock_db.pool.acquire.return_value.__aenter__ = AsyncMock(return_value=mock_conn)
        mock_db.pool.acquire.return_value.__aexit__ = AsyncMock(return_value=None)

        result = await quota_service.check_quota(
            tenant_id="test_tenant",
            user_id="blocked_user",
        )

        assert result.status == QuotaStatus.BLOCKED
        assert result.can_proceed is False

    @pytest.mark.asyncio
    async def test_get_or_create_quota_casts_reset_boundaries(self, quota_service, mock_db):
        """Reset boundary parameters must bind as timestamptz for legacy schemas."""
        now = datetime.now(timezone.utc)
        quota_row = {
            "tenant_id": "test_tenant",
            "user_id": "new_user",
            "daily_token_limit": None,
            "monthly_token_limit": None,
            "monthly_cost_limit_cents": None,
            "requests_per_minute": None,
            "requests_per_day": None,
            "current_daily_tokens": 0,
            "current_monthly_tokens": 0,
            "current_monthly_cost_cents": 0,
            "current_daily_requests": 0,
            "daily_reset_at": now,
            "monthly_reset_at": now,
            "is_blocked": False,
            "blocked_reason": None,
            "warning_threshold": 80,
            "overage_strategy": "allow_but_alert",
            "downgraded_model": None,
            "temporary_extra_tokens": 0,
            "temporary_extra_cost_cents": 0,
            "temporary_expires_at": None,
        }
        mock_conn = AsyncMock()
        mock_conn.fetchrow = AsyncMock(side_effect=[None, quota_row])
        mock_conn.execute = AsyncMock(return_value="INSERT 0 1")
        mock_db.pool.acquire = AsyncMock()
        mock_db.pool.acquire.return_value.__aenter__ = AsyncMock(return_value=mock_conn)
        mock_db.pool.acquire.return_value.__aexit__ = AsyncMock(return_value=None)

        quota = await quota_service._get_or_create_quota("test_tenant", "new_user")

        assert quota is not None
        insert_sql = mock_conn.execute.await_args.args[0]
        assert "$3::timestamptz" in insert_sql
        assert "$4::timestamptz" in insert_sql

    @pytest.mark.asyncio
    async def test_quota_resets_cast_boundary_parameters(self, quota_service, mock_db):
        mock_conn = AsyncMock()
        mock_conn.execute = AsyncMock(return_value="UPDATE 1")
        mock_db.pool.acquire = AsyncMock()
        mock_db.pool.acquire.return_value.__aenter__ = AsyncMock(return_value=mock_conn)
        mock_db.pool.acquire.return_value.__aexit__ = AsyncMock(return_value=None)

        await quota_service.reset_daily_quotas()
        daily_sql = mock_conn.execute.await_args.args[0]

        await quota_service.reset_monthly_quotas()
        monthly_sql = mock_conn.execute.await_args.args[0]

        assert "$1::timestamptz" in daily_sql
        assert "$2::timestamptz" in daily_sql
        assert "$1::timestamptz" in monthly_sql
        assert "$2::timestamptz" in monthly_sql

    @pytest.mark.asyncio
    async def test_get_quota_forecast(self, quota_service, mock_db):
        """Forecast should project month-end usage from recent trend."""
        now = datetime.now(timezone.utc)
        quota_row = {
            "tenant_id": "test_tenant",
            "user_id": "test_user",
            "daily_token_limit": None,
            "monthly_token_limit": 10000,
            "monthly_cost_limit_cents": 200,
            "requests_per_minute": 60,
            "requests_per_day": 1000,
            "current_daily_tokens": 0,
            "current_monthly_tokens": 3000,
            "current_monthly_cost_cents": 25,
            "current_daily_requests": 0,
            "daily_reset_at": now,
            "monthly_reset_at": now,
            "is_blocked": False,
            "blocked_reason": None,
            "warning_threshold": 80,
            "overage_strategy": "allow_but_alert",
            "downgraded_model": None,
            "temporary_extra_tokens": 0,
            "temporary_extra_cost_cents": 0,
            "temporary_expires_at": None,
        }
        month_row = {"month_tokens": 3000, "month_cost_microcents": 250000}
        recent_row = {"recent_tokens": 2100, "recent_cost_microcents": 140000}

        mock_conn = AsyncMock()
        mock_conn.fetchrow = AsyncMock(side_effect=[quota_row, month_row, recent_row])
        mock_db.pool.acquire = AsyncMock()
        mock_db.pool.acquire.return_value.__aenter__ = AsyncMock(return_value=mock_conn)
        mock_db.pool.acquire.return_value.__aexit__ = AsyncMock(return_value=None)

        result = await quota_service.get_quota_forecast(
            tenant_id="test_tenant",
            user_id="test_user",
            lookback_days=7,
        )

        assert "error" not in result
        assert result["tokens"]["current"] == 3000
        assert result["tokens"]["avg_daily"] == 300.0
        assert result["tokens"]["projected_month_end"] >= result["tokens"]["current"]
        assert result["cost"]["current_cents"] == 25.0
        assert result["cost"]["avg_daily_cents"] == 2.0
        assert result["tokens"]["limit"] == 10000
        assert result["cost"]["limit_cents"] == 200


class TestQuotaStatusEnum:
    """Test QuotaStatus enum values"""

    def test_quota_status_values(self):
        """Test that QuotaStatus has expected values"""
        assert QuotaStatus.OK == "ok"
        assert QuotaStatus.WARNING == "warning"
        assert QuotaStatus.EXCEEDED == "exceeded"
        assert QuotaStatus.BLOCKED == "blocked"


class TestCostCalculationEdgeCases:
    """Edge case tests for cost calculation"""

    @pytest.fixture
    def pricing_service(self):
        """Pricing service instance without DB (uses default pricing)"""
        return ModelPricingService(database=None)

    @pytest.mark.asyncio
    async def test_very_large_token_count(self, pricing_service):
        """Test cost calculation with very large token counts"""
        input_tokens = 1_000_000  # 1M tokens
        output_tokens = 500_000  # 500K tokens

        result = await pricing_service.calculate_cost(
            model="gpt-4",
            input_tokens=input_tokens,
            output_tokens=output_tokens,
        )

        # GPT-4: $0.03/1k input, $0.06/1k output
        # 1M input = $30 = 3000 cents
        # 500K output = $30 = 3000 cents
        assert result["input_cost_cents"] == 3000
        assert result["output_cost_cents"] == 3000
        assert result["total_cost_cents"] == 6000


class TestUsageRecorderBasics:
    """Basic tests for UsageRecorder (without full DB integration)"""

    def test_usage_record_dataclass(self):
        """Test UsageRecord dataclass creation"""
        from src.services.metrics.usage_recorder import UsageRecord

        record = UsageRecord(
            tenant_id="test_tenant",
            user_id="test_user",
            model="gpt-4",
            input_tokens=100,
            output_tokens=50,
            input_cost_cents=1,
            output_cost_cents=2,
        )

        assert record.tenant_id == "test_tenant"
        assert record.user_id == "test_user"
        assert record.model == "gpt-4"
        assert record.input_tokens == 100
        assert record.output_tokens == 50

    def test_usage_record_with_optional_fields(self):
        """Test UsageRecord with optional fields"""
        from src.services.metrics.usage_recorder import UsageRecord

        record = UsageRecord(
            tenant_id="test_tenant",
            user_id="test_user",
            model="gpt-4",
            input_tokens=100,
            output_tokens=50,
            input_cost_cents=1,
            output_cost_cents=2,
            service_id="chat-service",
            assistant_id="asst_123",
            request_id="req_456",
            latency_ms=1500,
            first_token_ms=200,
            status="success",
            request_type="chat",
        )

        assert record.service_id == "chat-service"
        assert record.assistant_id == "asst_123"
        assert record.request_id == "req_456"
        assert record.latency_ms == 1500
        assert record.first_token_ms == 200
        assert record.status == "success"
        assert record.request_type == "chat"


class TestProxyTokenEstimate:
    """Proxy token estimate helper tests."""

    def test_estimate_tokens_no_double_count_nested_messages(self):
        payload = {
            "input": {
                "messages": [
                    {"role": "user", "content": "hello world"},
                ]
            }
        }
        expected = estimate_tokens("hello world")
        assert _estimate_tokens_from_payload(payload) == expected

    def test_estimate_tokens_aggregates_distinct_text_fields(self):
        payload = {
            "query": "how are you",
            "input": {
                "messages": [
                    {"role": "user", "content": "explain zakat rules"},
                ]
            },
        }
        expected = estimate_tokens("how are you") + estimate_tokens("explain zakat rules")
        assert _estimate_tokens_from_payload(payload) == expected
