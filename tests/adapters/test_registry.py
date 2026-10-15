"""
适配器注册表测试

测试内容：
- 适配器注册
- 适配器获取
- 适配器元数据
"""

from src.adapters.base import ProtocolAdapter
from src.adapters.registry import (
    get_adapter,
    get_adapter_metadata,
    list_adapters,
    register_adapter_class,
)


class TestAdapterRegistry:
    """适配器注册表测试"""

    def test_register_adapter(self):
        """测试注册适配器"""

        class TestAdapter(ProtocolAdapter):
            """Test adapter"""

            pass

        register_adapter_class("test_adapter", TestAdapter)

        adapter_cls = get_adapter("test_adapter")
        assert adapter_cls is TestAdapter

    def test_get_adapter_metadata(self):
        """测试获取适配器元数据"""

        class MetadataTestAdapter(ProtocolAdapter):
            """Metadata test adapter"""

            pass

        register_adapter_class("metadata_test_adapter", MetadataTestAdapter)

        metadata = get_adapter_metadata("metadata_test_adapter")
        assert metadata is not None
        assert metadata.name == "metadata_test_adapter"

    def test_list_adapters(self):
        """测试列出所有适配器"""

        class ListTestAdapter(ProtocolAdapter):
            """List test adapter"""

            pass

        register_adapter_class("list_test_adapter", ListTestAdapter)

        all_adapters = list_adapters()
        names = [a.name for a in all_adapters]
        assert "list_test_adapter" in names

    def test_get_nonexistent_adapter(self):
        """测试获取不存在的适配器"""
        adapter_cls = get_adapter("nonexistent_adapter_xyz")
        assert adapter_cls is None
