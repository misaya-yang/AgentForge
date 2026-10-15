# 知识库测试工具

本目录包含知识库模块的测试和工具脚本。

## 可选手工验证脚本

### test_kb_qa.py - 问答功能验证
```bash
# 单次查询测试
uv run python tests/knowledge/test_kb_qa.py --dataset agent --query "release rollback"

# 多模态检索测试（含图片）
uv run python tests/knowledge/test_kb_qa.py --dataset agent --query "diagram" --multimodal

# 交互模式
uv run python tests/knowledge/test_kb_qa.py --dataset agent --interactive

# 列出所有文档
uv run python tests/knowledge/test_kb_qa.py --dataset agent --list-docs
```

## 批量入库工具

批量入库脚本位于 `tests/knowledge/tools/batch_ingest.py`：

```bash
# 单个文件入库
uv run python tests/knowledge/tools/batch_ingest.py --dataset agent --files "/path/to/file.pdf"

# 批量入库
uv run python tests/knowledge/tools/batch_ingest.py --dataset agent --files "/path/to/*.pdf" --concurrency 3

# 目录递归扫描
uv run python tests/knowledge/tools/batch_ingest.py --dataset agent --dir ./documents --recursive
```

## 环境变量

```bash
export GATEWAY_BASE_URL="http://localhost:8080"
export GATEWAY_API_KEY="your-api-key"
```
