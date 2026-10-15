#!/usr/bin/env python3
"""ARC-04 domain implementation ownership inventory (goal 4).

Concrete business domains living inside ``ai_gateway_core`` today — quiz,
skills, image, memory, sharing, knowledge client, eval — must eventually be
owned by their domain, not by the shared core package.  PRD §ARC-04:
"quiz、skills、image、memory、sharing、knowledge、eval 等具体实现迁回 owner".

This script inventories each domain **without moving anything**:

- the core modules that belong to the domain (LOC, direct I/O deps);
- every consumer file, grouped by owner (gateway service code, knowledge
  service code, intra-core, tests, scripts);
- the SQL tables the domain writes (from the persistence inventory);
- the target owner and the conditions a migration batch must satisfy.

It is a synthesis of the two committed inventories, so regenerate those
first when the tree changes::

    uv run python scripts/core_boundary/inventory_core_consumption.py
    uv run python scripts/core_boundary/inventory_persistence_sql.py
    uv run python scripts/core_boundary/inventory_domain_ownership.py

Output: ``reports/inventory/core-domain-ownership-inventory.json``.
"""

from __future__ import annotations

import argparse
import json
import sys
from datetime import UTC, datetime
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from inventory_core_consumption import repo_root  # noqa: E402

SCHEMA_VERSION = "arc04-domain-ownership/v1"
IMPORT_INVENTORY = Path("reports/inventory/core-import-inventory.json")
SQL_INVENTORY = Path("reports/inventory/core-persistence-sql-inventory.json")

# PRD §ARC-04 goal 4 + 第一批优先级.  ``target_owner`` names where the
# implementation belongs once moved; ``move_conditions`` are the per-batch
# requirements from the PRD (one domain per commit, consumer list, thin
# compat shim with removal conditions, no behaviour change).
DOMAIN_DECISIONS: dict[str, dict[str, object]] = {
    "quiz": {
        "target_owner": "Gateway 领域模块 (quiz 能力, 建议 src/services/quiz)",
        "rationale": "纯业务实现(评分/访问控制), 无跨服务消费者, 不符合 contracts 准入",
        "move_conditions": [
            "单次提交只迁 quiz 一个领域",
            "消费者: src/api/v1/quiz.py, src/api/v1/conversation_shares.py, sharing.artifact_share_manager(core 内)",
            "注意 quiz_grader 被 sharing/artifact_share_manager 跨域消费 — 先处理该边或同批迁走",
            "保留薄 shim + 删除条件, 测试通过后再排期删除",
        ],
    },
    "skills": {
        "target_owner": "Gateway 领域模块 (skills 能力, 建议 src/services/skills)",
        "rationale": "技能解析/执行/注册是业务实现; parser 依赖 yaml(非协议), 不应进 contracts",
        "move_conditions": [
            "单次提交只迁 skills 一个领域",
            "消费者: src/main.py, src/api/v1/skills.py, tests",
            "artifact_repository 被 persistence.repositories.agent_repository 消费 — 该边属于 god class 分离范围(见分离方案)",
            "builder 写 assistant_audit_events(agent-runtime 表) — 迁移时一并评估该跨域写",
        ],
    },
    "image": {
        "target_owner": "Gateway 领域模块 (image 能力, 建议并入 src/services/images)",
        "rationale": "图像状态机/缩略图/水印是业务实现, 带 PIL/numpy/httpx 重依赖, 与 src/services/images 同源",
        "move_conditions": [
            "单次提交只迁 image 一个领域",
            "消费者: src/services/images/{service,repository}.py, src/api/v1/agent_images.py, tests",
            "image_state 写 assistant.image_* 五表 — 与 src/services/images/repository.py 的 SQL 一并核对",
            "PIL/numpy 依赖随模块离开 core 后, core 不再需要这些依赖",
        ],
    },
    "memory": {
        "target_owner": "Gateway 领域模块 (memory 能力, 建议 src/services/memory)",
        "rationale": "会话/用户记忆业务逻辑, 消费者仅 Gateway(container + control_plane)",
        "move_conditions": [
            "单次提交只迁 memory 一个领域",
            "消费者: src/container.py, src/services/agent_runtime/control_plane.py",
            "写 session_memory/user_memory(memory 域表) — 无跨域 SQL",
            "persistence.database/agent_repository 也写 memory 表 — god class 分离先行(见分离方案)",
        ],
    },
    "sharing": {
        "target_owner": "Gateway 领域模块 (sharing 能力, 建议 src/services/sharing)",
        "rationale": "artifact 分享/答题令牌是业务实现, 消费者仅 Gateway API 层",
        "move_conditions": [
            "单次提交只迁 sharing 一个领域",
            "消费者: src/api/v1/artifact_shares.py, src/api/v1/quiz.py, tests",
            "artifact_share_manager 反向依赖 quiz_grader — 与 quiz 批次协调顺序",
            "写 assistant.artifact_share* 三表(sharing 域) — 无跨域 SQL",
        ],
    },
    "knowledge": {
        "target_owner": "Gateway client owner(knowledge HTTP 客户端归 Gateway 的 client 层, PRD 第一批具名)",
        "rationale": "proxy_client 是 HTTP 客户端(httpx), PRD §ARC-04 第一批: 'knowledge HTTP proxy → Gateway client owner'; errors/utils 随迁",
        "move_conditions": [
            "knowledge-service 是 core 消费方(2 文件) — 迁移不得增加 knowledge→core 依赖数(门禁冻结值 13)",
            "消费者(以重新生成的 JSON consumers 为准; ARC-01 拆分后为): src/main.py, _assistant_routes/catalog.py, knowledge_authz.py, kb_ragas_client.py, knowledge-service 2 文件, tests",
            "HTTP 客户端不属于 contracts 内容白名单 — 目标位置是 Gateway 的 client 层而非 contracts",
        ],
    },
    "eval": {
        "target_owner": "Gateway 领域模块 (eval 能力, 建议 src/services/eval)",
        "rationale": "评测执行/候选/采样是业务实现(evaluator_executor 1992 LOC), 消费者为 Gateway eval API 与 harness 脚本",
        "move_conditions": [
            "单次提交只迁 eval 一个领域",
            "消费者: src/api/v1/{eval,agents}.py, src/services/eval/*, scripts/harness/agent_runtime_eval_contract.py, tests",
            "agent_version_candidate 被 persistence.repositories.agent_repository 消费 — 属 god class 分离范围",
            "迁移后 scripts/harness 的 import 路径需同批更新(脚本消费者无门禁冻结, 但不得破坏)",
        ],
    },
}


def synthesize(root: Path) -> dict:
    import_inv = json.loads((root / IMPORT_INVENTORY).read_text(encoding="utf-8"))
    sql_inv = json.loads((root / SQL_INVENTORY).read_text(encoding="utf-8"))
    modules = import_inv["modules"]

    domains: dict[str, dict] = {}
    for domain, decision in DOMAIN_DECISIONS.items():
        prefix = f"ai_gateway_core.{domain}"
        own = {
            m: info for m, info in modules.items() if m == prefix or m.startswith(prefix + ".")
        }
        consumers: dict[str, set[str]] = {}
        intra_core_consumers: dict[str, list[str]] = {}
        for dotted, info in own.items():
            for owner, files in info.get("consumer_owners", {}).items():
                if owner == "core":
                    intra_core_consumers[dotted] = list(files)
                    continue
                consumers.setdefault(owner, set()).update(files)
        tables = {
            m: {
                "write": sorted(e["table"] for e in info["write"] if e["known_table"]),
                "read": sorted(e["table"] for e in info["read"] if e["known_table"]),
            }
            for m, info in sql_inv["modules"].items()
            if (m == prefix or m.startswith(prefix + "."))
        }
        total_loc = sum(info["loc"] for info in own.values())
        io_deps = sorted({dep for info in own.values() for dep in info["io_deps"]})
        domains[domain] = {
            "target_owner": decision["target_owner"],
            "rationale": decision["rationale"],
            "move_conditions": decision["move_conditions"],
            "module_count": len(own),
            "total_loc": total_loc,
            "direct_io_deps": io_deps,
            "modules": {
                m: {
                    "file": info["file"],
                    "loc": info["loc"],
                    "io_deps": info["io_deps"],
                }
                for m, info in sorted(own.items())
            },
            "consumers": {owner: sorted(files) for owner, files in sorted(consumers.items())},
            "intra_core_consumers": intra_core_consumers,
            "sql_tables": tables,
        }

    return {
        "schema_version": SCHEMA_VERSION,
        "generated_at": datetime.now(UTC).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "sources": [IMPORT_INVENTORY.as_posix(), SQL_INVENTORY.as_posix()],
        "note": (
            "盘点只确定目标 owner 与消费者清单, 不移动代码; 每个领域的迁移单独成批, "
            "遵守 PRD §ARC-04 '每个提交只迁一个领域, 并提供消费者清单'. "
            "target_owner 是文档性结论, 迁移批次启动前需按当时的消费者清单复核."
        ),
        "domains": domains,
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--output",
        default="reports/inventory/core-domain-ownership-inventory.json",
        help="inventory JSON destination (repo-relative)",
    )
    args = parser.parse_args(argv)
    root = repo_root()
    inventory = synthesize(root)
    out_path = root / args.output
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(json.dumps(inventory, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(
        f"wrote {out_path.relative_to(root)}: "
        + ", ".join(
            f"{name}={info['module_count']}mod/{info['total_loc']}LOC"
            for name, info in inventory["domains"].items()
        )
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
