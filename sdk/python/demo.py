"""
AI Gateway SDK Demo — 直接运行: python demo.py
"""
import asyncio
import os
import sys

sys.path.insert(0, os.path.dirname(__file__))
from ai_assistant import AssistantClient

API_KEY = os.environ.get("GATEWAY_API_KEY", "")
BASE_URL = os.environ.get("GATEWAY_BASE_URL", "http://localhost:8080")


async def main():
    if not API_KEY:
        raise RuntimeError("GATEWAY_API_KEY must be set")
    async with AssistantClient(api_key=API_KEY, base_url=BASE_URL) as client:

        # 1. 普通对话
        print("=" * 50)
        print("1. 普通对话")
        print("=" * 50)
        resp = await client.chat.send("用一句话介绍这个平台适合解决什么问题")
        print(f"回答: {resp.content}\n")

        # 2. 流式对话
        print("=" * 50)
        print("2. 流式对话（逐字输出）")
        print("=" * 50)
        async for event in client.chat.stream("用三句话解释知识库问答如何工作"):
            if event.event_type == "text_delta":
                text = event.data if isinstance(event.data, str) else event.data.get("data", "")
                print(text, end="", flush=True)
            elif event.event_type == "done":
                break
        print("\n")

        # 3. 知识库问答
        print("=" * 50)
        print("3. 知识库问答")
        print("=" * 50)
        datasets = await client.knowledge.list_datasets()
        print(f"可用知识库: {[d.name for d in datasets]}")
        if datasets:
            resp = await client.knowledge.ask(
                "What is the ruling on Zakat?",
                dataset_ids=[datasets[0].dataset_id],
            )
            print(f"回答: {resp.content[:200]}...\n")

        # 4. 查看可用工具
        print("=" * 50)
        print("4. 可用工具")
        print("=" * 50)
        tools = await client.tools.list_tools()
        for t in tools:
            print(f"  - {t.name} ({t.category})")

    print("\n✅ Demo 完成!")


if __name__ == "__main__":
    asyncio.run(main())
