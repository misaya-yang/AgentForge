# =============================================================================
# AI Gateway - Operations Makefile
# =============================================================================
# 运维只需要记住 make 命令:
#
#   make doctor          环境体检 (工具/内存/端口/compose 归属，只读不改动)
#   make quickstart      首次一键启动 (生成 .env + 拉镜像 + 启动 + 迁移 + 校验)
#   make harness-check   校验 harness.yml 契约与文档预算
#   make agent-runtime-source-contract 校验 Agent Runtime 源码/Schema/SBOM/OCI 锁
#
#   make deploy          部署全部服务 (启动+迁移+健康检查，不默认重建镜像)
#   make deploy-build    部署并强制重新构建镜像 (需要 AI_PLATFORM_AGENT_RUNTIME_SOURCE)
#   make deploy-cn       使用国内镜像构建部署
#   make validate-config 校验 .env 和 Compose 配置
#   make validate-example-config 校验开源示例配置和 Compose 渲染
#   make validate        校验 .env、Compose 配置和运行时依赖
#   make seed-demo       预览本地 demo 数据
#   make seed-demo-apply 写入本地 demo 数据
#   make status          查看所有服务状态
#   make logs            查看实时日志
#   make stop            停止所有服务
#   make restart         重启所有服务
#   make hot-update      热更新本地部署容器源码 (不 pip、不重建镜像)
#
#   make migrate         运行数据库迁移 (自动跳过已执行的)
#   make migrate-init    首次初始化 schema 并运行全部待执行迁移
#   make migrate-status  查看迁移状态
#
#   make backup          创建数据库备份
#   make restore         从最新备份恢复
#   make backup-list     列出所有备份
#
#   make dev-setup       一键搭建本地开发环境
#   make dev-start       启动开发容器
#   make dev-stop        停止开发容器
#   make dev-reset       重置开发环境
#   make dev-status      查看开发环境状态
#   make dev-compose     使用源码挂载启动应用服务 (无需反复构建)
#
#   make help            显示此帮助
# =============================================================================

SHELL := /bin/bash
SCRIPTS := scripts/new
ENV_FILE ?= .env
COMPOSE := $(shell docker compose version >/dev/null 2>&1 && echo "docker compose" || echo "docker-compose")
BUILD_COMPOSE := $(COMPOSE) -f docker-compose.yml -f docker-compose.build.yml
DEV_COMPOSE := $(BUILD_COMPOSE) -f docker-compose.dev.yml
COMPOSE_PARALLEL_LIMIT ?= 1
export COMPOSE_PARALLEL_LIMIT

.DEFAULT_GOAL := help

.PHONY: local-bootstrap local-baseline local-runtime-source
local-bootstrap:            ## 安装隔离工具链和锁定依赖，不启动服务
	@bash scripts/dev/bootstrap-local.sh

local-baseline:             ## 用隔离工具链执行现有离线 harness 并保留逐项结果
	@bash scripts/dev/with-local-toolchain.sh bash scripts/dev/check-local-baseline.sh

local-runtime-source:       ## 恢复并校验外部固定 Runtime upstream Git 源码
	@bash scripts/dev/with-local-toolchain.sh bash scripts/dev/prepare-runtime-source.sh

.PHONY: local-services-up local-services-check local-services-init local-services-status local-services-stop
local-services-up local-services-check local-services-init local-services-status local-services-stop:
	@bash scripts/dev/with-local-toolchain.sh uv run --all-packages --extra test python scripts/dev/local-services.py $(subst local-services-,,$@)

# -- Quick Start --------------------------------------------------------------

.PHONY: doctor harness-check runtime-dependency-gate gateway-kb-boundary-gate architecture-boundary-gate core-boundary-gate single-instance-guard verify-openapi-contract live-openapi-contract-gate agent-runtime-source-build-local agent-runtime-source-contract agent-runtime-build-local agent-runtime-contract agent-runtime-smoke agent-runtime-text-gate agent-runtime-single-kernel-gate agent-runtime-readonly-gate agent-runtime-write-gate agent-thread-store-contract agent-capability-worker-build-local agent-capability-worker-smoke agent-runtime-release-gate agent-runtime-rollback-rehearsal sdk-sse-contract snapshot-gateway-openapi quickstart quickstart-build validate-config validate-example-config validate seed-demo seed-demo-apply

doctor:                     ## 环境体检: 工具/Docker/内存/端口/compose 归属 (只读)
	@ENV_FILE="$(ENV_FILE)" bash $(SCRIPTS)/doctor.sh --env "$(ENV_FILE)"

harness-check:              ## 校验 harness.yml 契约: 命令存在、必备文档、指令文件行数预算
	@python3 scripts/harness/check_harness.py

runtime-dependency-gate:    ## 检查已退役 Python 执行面/docgen/OpenAPI 依赖未回流
	@python3 scripts/harness/runtime_dependency_gate.py

gateway-kb-boundary-gate:   ## 阻止 gateway 重新持有 KB 表 SQL 或 KB 请求 schema（PRD T8.4）
	@python3 scripts/harness/gateway_kb_boundary_gate.py

architecture-boundary-gate: ## 静态 import 边界门禁: gateway/apps/core/contracts 区间合同 (含负向自测)
	@python3 scripts/harness/import_boundary_gate.py --selftest
	@python3 scripts/harness/import_boundary_gate.py

core-boundary-gate:         ## ai-gateway-core/contracts 清单与兼容边界（Git provenance + 负向自测）
	@python3 scripts/core_boundary/check_core_boundary.py --self-test
	@python3 scripts/core_boundary/check_core_boundary.py

single-instance-guard:      ## Gateway/Runtime 单实例门禁: topology + Helm values/templates (含负向自测)
	@python3 scripts/harness/single_instance_guard.py --selftest
	@python3 scripts/harness/single_instance_guard.py

agent-runtime-source-contract: ## 校验 Agent Runtime 不可变源码、Schema、SBOM、许可证和 OCI 身份
	@python3 scripts/harness/agent_runtime_supply_chain.py validate \
		--repo-root . \
		--lock deploy/agent-runtime-source/lock.json \
		--require-artifact agent_runtime
	@uv run --all-packages --extra test pytest -q --no-cov \
		tests/harness/test_agent_runtime_supply_chain.py

agent-runtime-contract:     ## 校验 Rust Agent Runtime 已锁到独立、可运行的 OCI 制品
	@python3 scripts/harness/agent_runtime_supply_chain.py validate \
		--repo-root . \
		--lock deploy/agent-runtime-source/lock.json \
		--require-artifact agent_runtime

agent-runtime-build-local:  ## 从干净受控 fork 构建 Rust Agent Runtime 镜像
	@bash scripts/harness/build_agent_runtime_image.sh

agent-runtime-recovery-tests: ## Docker 内运行 Runtime/Worker 库测试和 Core 恢复回归
	@bash scripts/harness/test_agent_runtime_recovery_docker.sh

agent-runtime-smoke:        ## 在隔离 PostgreSQL/Docker 网络验证 Runtime 健康、Thread 和重启恢复
	@bash scripts/harness/smoke_agent_runtime_image.sh

agent-runtime-text-gate:    ## 真实 Qwen Responses：简单、长输出与重启后多轮恢复
	@uv run --all-packages --extra test python \
		scripts/harness/agent_runtime_text_gate.py \
		--env-file "$(ENV_FILE)" \
		--runtime-image "$(AI_PLATFORM_AGENT_RUNTIME_IMAGE)"

agent-runtime-single-kernel-gate: ## 验证唯一 Rust Agent 内核、V1/V2 投影和迁移合同
	@python3 scripts/harness/agent_runtime_single_kernel_gate.py
	@ENV_FILE="$(ENV_FILE)" uv run --all-packages --extra test pytest -q --no-cov \
		tests/services/assistant/test_runtime_assignment.py \
		tests/services/agent_runtime/test_thread_store.py \
		tests/api/test_agent_v2.py \
		tests/database/test_agent_runtime_thread_store_migration.py \
		tests/contract/test_openapi_schema_compat.py

agent-runtime-readonly-gate: ## Context/Knowledge/Tool/MCP/Artifact 只读桥合同
	@uv run --all-packages --extra test python scripts/harness/agent_runtime_readonly_gate.py
	@uv run --all-packages --extra test pytest -q --no-cov \
		tests/services/agent_runtime/test_readonly_capabilities.py \
		tests/harness/test_agent_runtime_readonly_gate.py

agent-runtime-write-gate:   ## 校验工具审批、dispatch fence、幂等和中断恢复闭环
	@uv run --all-packages --extra test python \
		scripts/harness/agent_runtime_write_gate.py \
		--fork "$(AI_PLATFORM_AGENT_RUNTIME_SOURCE)"

agent-thread-store-contract: ## 在真实 PostgreSQL 验证 Agent ThreadStore 与预授权根线程闭环
	@ENV_FILE="$(ENV_FILE)" AI_PLATFORM_AGENT_RUNTIME_SOURCE="$(AI_PLATFORM_AGENT_RUNTIME_SOURCE)" \
		bash scripts/harness/agent_thread_store_contract.sh

agent-capability-worker-build-local: ## 单 job 构建固定源码的 Rust capability worker 镜像
	@AI_PLATFORM_AGENT_RUNTIME_SOURCE="$(AI_PLATFORM_AGENT_RUNTIME_SOURCE)" \
		bash scripts/harness/build_agent_capability_worker_image.sh

agent-capability-worker-smoke: ## 隔离 PostgreSQL 验证 capability worker 的租约、幂等、事件、取消和恢复
	@bash scripts/harness/smoke_agent_capability_worker_image.sh

agent-runtime-release-gate:   ## 串行运行单内核发布合同（不构建镜像、不调用 provider）
	@$(MAKE) agent-runtime-source-contract
	@$(MAKE) runtime-dependency-gate
	@$(MAKE) agent-runtime-single-kernel-gate
	@$(MAKE) agent-runtime-readonly-gate
	@$(MAKE) verify-assistant-runtime-dev
	@$(MAKE) agent-eval-core-gate
	@$(MAKE) test-isolation
	@$(MAKE) rag-eval-fixture-contract
	@$(MAKE) sdk-sse-contract
	@$(MAKE) harness-check

agent-runtime-rollback-rehearsal: ## 用冻结 release 执行 new→old→new 回滚并验证会话/执行账本
	@ENV_FILE="$(ENV_FILE)" bash scripts/harness/agent_runtime_rollback_rehearsal.sh

agent-runtime-source-build-local: ## 低内存构建本地 Agent Runtime 源码镜像并验证源码/Schema 标签与 initialize
	@bash scripts/harness/build_agent_runtime_source_image.sh

sdk-sse-contract:           ## 验证四端 SSE 合同；发布 CI 要求 Maven/Dart 必须存在
	@uv run --all-packages --extra test pytest -q --no-cov \
		sdk/python/tests/test_sse_inner_envelopes.py \
		tests/contract/test_sdk_sse_fixture_contract.py
	@npm --prefix sdk/cli run typecheck
	@npm --prefix sdk/cli test
	@if command -v mvn >/dev/null 2>&1; then \
		mvn -q -f sdk/java/pom.xml test; \
	elif [ "$${SDK_SSE_CONTRACT_REQUIRE_ALL:-0}" = "1" ]; then \
		echo "ERROR Java SDK SSE contract requires mvn" >&2; exit 1; \
	else \
		echo "SKIP Java SDK SSE contract: mvn not installed"; \
	fi
	@if command -v dart >/dev/null 2>&1; then \
		cd sdk/dart/ai_gateway_sdk && dart pub get && dart test; \
	elif [ "$${SDK_SSE_CONTRACT_REQUIRE_ALL:-0}" = "1" ]; then \
		echo "ERROR Dart SDK SSE contract requires dart" >&2; exit 1; \
	else \
		echo "SKIP Dart SDK SSE contract: dart not installed"; \
	fi

independent-cli-gate:       ## 验证独立 CLI 配置、provider adapter、launcher 和发布 bundle
	@npm --prefix sdk/cli run typecheck
	@npm --prefix sdk/cli test
	@npm --prefix sdk/cli run build
	@CLI_DOCTOR_ENTRY="$(CURDIR)/sdk/cli/dist/cli.js" node --test sdk/cli/scripts/check-doctor.mjs
	@node --check sdk/cli/scripts/verify-native-package.mjs
	@bash -n scripts/harness/build_agent_runtime_cli_package.sh

agent-runtime-cli-build-local: ## 在 Docker 内从固定 composed source 构建独立 native CLI
	@AI_PLATFORM_AGENT_RUNTIME_SOURCE="$(AI_PLATFORM_AGENT_RUNTIME_SOURCE)" \
		bash scripts/harness/build_agent_runtime_cli_package.sh

snapshot-gateway-openapi:  ## 导出 Python/hybrid 候选，保留混合 SDK 合同
	@test -n "$(OPENAPI_OUTPUT)" || (echo "Set OPENAPI_OUTPUT to a separate candidate path" >&2; exit 2)
	@uv run --all-packages --extra test python scripts/snapshot_gateway_openapi.py --profile python-hybrid --output "$(OPENAPI_OUTPUT)"

verify-openapi-contract:   ## 进程内离线 OpenAPI 合同门禁: 直接导出 FastAPI app, 无活栈, 绝不 skip
	@uv run --all-packages --extra test python scripts/harness/openapi_contract_gate.py --selftest
	@uv run --all-packages --extra test python scripts/harness/openapi_contract_gate.py --profile python-hybrid
	@uv run --all-packages --extra test pytest -q --no-cov tests/contract/test_openapi_schema_compat.py tests/contract/test_openapi_profiles.py

.PHONY: snapshot-native-openapi verify-native-openapi-contract compare-openapi-candidate
snapshot-native-openapi: ## 原生及保留Python Knowledge声明候选；不满足完整原生门禁
	@test -n "$(NATIVE_OPENAPI_PRODUCER)" -a -n "$(OPENAPI_OUTPUT)" -a -n "$(OPENAPI_COVERAGE_OUTPUT)" || (echo "Set NATIVE_OPENAPI_PRODUCER, OPENAPI_OUTPUT and OPENAPI_COVERAGE_OUTPUT (external paths)"; exit 2)
	@uv run --all-packages --extra test python scripts/snapshot_native_openapi.py --producer "$(NATIVE_OPENAPI_PRODUCER)" --output "$(OPENAPI_OUTPUT)" --coverage-output "$(OPENAPI_COVERAGE_OUTPUT)" $(if $(NATIVE_OPENAPI_PREVIOUS),--previous "$(NATIVE_OPENAPI_PREVIOUS)") $(if $(KNOWLEDGE_OPENAPI_TOKENIZER_CACHE),--knowledge-tokenizer-cache "$(KNOWLEDGE_OPENAPI_TOKENIZER_CACHE)")

verify-native-openapi-contract: ## 原生完整公开合同；缺独立候选时失败，不跳过
	@uv run --all-packages --extra test python scripts/harness/openapi_contract_gate.py --profile native $(if $(NATIVE_OPENAPI_CANDIDATE),--candidate "$(NATIVE_OPENAPI_CANDIDATE)") --evidence-out tmp/gate-evidence/verify-native-openapi-contract.json

compare-openapi-candidate: ## 仅比较制品，不构成原生来源或发布证明
	@test -n "$(OPENAPI_CANDIDATE)" || (echo "Set OPENAPI_CANDIDATE" >&2; exit 2)
	@uv run --all-packages --extra test python scripts/harness/openapi_contract_gate.py --profile published --candidate "$(OPENAPI_CANDIDATE)" --evidence-out tmp/gate-evidence/compare-openapi-candidate.json

live-openapi-contract-gate: ## Live OpenAPI 合同门禁: 需要活栈; 栈未起时报告 SKIPPED, 绝不冒充 PASS
	@echo "TIER=L3-live: this gate needs the running stack; a skip here is SKIPPED, never PASS."
	@uv run --all-packages --extra test pytest -q --no-cov -m integration \
		tests/integration/test_gateway_openapi_contract.py

quickstart:                 ## 拉取版本化多架构镜像并一键部署 (仅需模型配置)
	@bash $(SCRIPTS)/init-env.sh --env "$(ENV_FILE)" --if-missing
	@bash $(SCRIPTS)/deploy.sh --env "$(ENV_FILE)" --pull

quickstart-build:           ## 维护者从当前源码串行构建并部署
	@bash $(SCRIPTS)/init-env.sh --env "$(ENV_FILE)" --if-missing
	@bash $(SCRIPTS)/deploy.sh --env "$(ENV_FILE)" --build

validate:                   ## 校验 .env、Compose 配置和运行时依赖
	@bash $(SCRIPTS)/validate-env.sh --env "$(ENV_FILE)" --runtime

validate-config:            ## 仅校验 .env 和 Compose 配置
	@bash $(SCRIPTS)/validate-env.sh --env "$(ENV_FILE)" --config-only

validate-example-config:    ## 校验提交的 .env.example 和 Compose 示例配置
	@bash $(SCRIPTS)/validate-env.sh --env ".env.example" --example

seed-demo:                  ## 预览本地 demo 数据 SQL 和路由 (不写数据库)
	@bash $(SCRIPTS)/seed-demo-data.sh --env "$(ENV_FILE)" --dry-run

seed-demo-apply:            ## 写入本地 demo 数据 (仅用于开发/本地演示)
	@bash $(SCRIPTS)/seed-demo-data.sh --env "$(ENV_FILE)" --apply

# -- Deployment ---------------------------------------------------------------

.PHONY: deploy deploy-build deploy-cn deploy-infra deploy-app stop logs restart status hot-update

deploy:                     ## 部署全部服务 (启动+迁移+健康检查，不默认重建镜像)
	@bash $(SCRIPTS)/deploy.sh --env "$(ENV_FILE)" $(ARGS)

deploy-build:               ## 部署并重新构建镜像
	@bash scripts/rust/build-update.sh --artifact all
	@bash scripts/rust/locks.sh run \
		--resource integration-runtime \
		--timeout-seconds "$${AI_PLATFORM_INTEGRATION_LOCK_TIMEOUT_SECONDS:-7200}" \
		--heartbeat-seconds 10 \
		--expected-end-condition "serial candidate image deploy and health checks finish" \
		-- env AI_PLATFORM_RUST_IMAGES_PREBUILT=1 \
		bash $(SCRIPTS)/deploy.sh --env "$(ENV_FILE)" --build $(ARGS)

deploy-cn:                  ## 使用国内镜像构建部署
	@bash $(SCRIPTS)/deploy.sh --env "$(ENV_FILE)" --cn $(ARGS)

deploy-infra:               ## 仅部署基础设施 (postgres/redis/qdrant)
	@bash $(SCRIPTS)/deploy.sh --env "$(ENV_FILE)" --infra $(ARGS)

deploy-app:                 ## 仅部署应用服务 (gateway/frontend/knowledge/Agent Runtime/Worker)
	@bash $(SCRIPTS)/deploy.sh --env "$(ENV_FILE)" --app $(ARGS)

stop:                       ## 停止所有服务
	@cd "$(shell pwd)" && $(COMPOSE) --env-file "$(ENV_FILE)" stop

restart:                    ## 重启所有服务
	@cd "$(shell pwd)" && $(COMPOSE) --env-file "$(ENV_FILE)" restart

logs:                       ## 查看实时日志
	@cd "$(shell pwd)" && $(COMPOSE) --env-file "$(ENV_FILE)" logs -f

status:                     ## 查看所有服务状态和健康检查
	@ENV_FILE="$(ENV_FILE)" bash $(SCRIPTS)/status.sh

hot-update:                 ## 热更新本地部署容器源码 (不 pip、不重建镜像)
	@bash $(SCRIPTS)/hot-update.sh --env "$(ENV_FILE)" $(ARGS)

# -- Database Migrations ------------------------------------------------------

.PHONY: migrate migrate-init migrate-status

migrate:                    ## 运行数据库迁移 (自动跳过已执行的)
	@bash $(SCRIPTS)/migrate.sh --env "$(ENV_FILE)"

migrate-init:               ## 首次初始化 schema 并运行全部待执行迁移
	@bash $(SCRIPTS)/migrate.sh --env "$(ENV_FILE)" --init

migrate-status:             ## 查看迁移状态 (已执行/待执行)
	@bash $(SCRIPTS)/migrate.sh --env "$(ENV_FILE)" --status

# -- Backup & Restore --------------------------------------------------------

.PHONY: backup restore backup-list

backup:                     ## 创建数据库备份
	@ENV_FILE="$(ENV_FILE)" bash $(SCRIPTS)/backup.sh

restore:                    ## 从最新备份恢复
	@ENV_FILE="$(ENV_FILE)" bash $(SCRIPTS)/backup.sh --restore

backup-list:                ## 列出所有备份
	@ENV_FILE="$(ENV_FILE)" bash $(SCRIPTS)/backup.sh --list

# -- Development Environment --------------------------------------------------

.PHONY: dev-setup dev-start dev-stop dev-reset dev-status dev-compose dev-compose-logs

dev-setup:                  ## 一键搭建本地开发环境 (容器+数据库+迁移)
	@ENV_FILE="$(ENV_FILE)" bash $(SCRIPTS)/setup-dev.sh

dev-start:                  ## 启动开发容器
	@ENV_FILE="$(ENV_FILE)" bash $(SCRIPTS)/setup-dev.sh --start

dev-stop:                   ## 停止开发容器
	@ENV_FILE="$(ENV_FILE)" bash $(SCRIPTS)/setup-dev.sh --stop

dev-reset:                  ## 重置开发环境 (销毁并重建)
	@ENV_FILE="$(ENV_FILE)" bash $(SCRIPTS)/setup-dev.sh --reset

dev-status:                 ## 查看开发环境状态
	@ENV_FILE="$(ENV_FILE)" bash $(SCRIPTS)/setup-dev.sh --status

dev-compose:                ## 从源码构建并挂载后端服务，启用热重载
	@bash $(SCRIPTS)/validate-env.sh --env "$(ENV_FILE)" --config-only
	@$(DEV_COMPOSE) --env-file "$(ENV_FILE)" up -d --build --remove-orphans gateway knowledge-service knowledge-worker agent-runtime agent-capability-worker frontend
	@echo "Development compose is running with backend source mounts and uvicorn reload."

dev-compose-logs:           ## 查看源码挂载开发服务日志
	@$(DEV_COMPOSE) --env-file "$(ENV_FILE)" logs -f gateway knowledge-service knowledge-worker agent-runtime agent-capability-worker frontend

# -- Agent Trace / Eval Development Gates ------------------------------------

.PHONY: verify-agent-studio verify-eval-dev agent-eval-core-gate agent-runtime-eval-contract-gate eval-e1-gate eval-e1-unit-gate eval-regression-gate rag-eval-fixture-contract rag-live-quality-gate kb-unit-gate kb-golden-gate kb-release-evidence-gate kb-migration-gate kb-image-lock-refresh kb-image-lock-gate kb-integration-smoke kb-baseline-record verify-assistant-runtime-dev test-isolation

EVAL_REGRESSION_REPORT_DIR ?= tmp/eval-regression
EVAL_E1_ARTIFACT_DIR ?= tmp/eval-e1
EVAL_UV_RUN ?= uv run --all-packages --extra test
EVAL_RUFF_RUN ?= uv run --all-packages --extra dev
# Extra pytest flags for gates (CI uses this to emit JUnit evidence into
# tmp/gate-evidence/ for the final gate-enforcement audit).
PYTEST_EXTRA ?=

verify-agent-studio:        ## 运行 AS-00~AS-08 版本化 Agent Studio 整体回归门禁
	@uv run python scripts/agent_studio_regression.py $(ARGS)

verify-assistant-runtime-dev: ## 运行 Assistant Runtime 离线回归门禁 (AHR-01~AHR-04)
	@$(EVAL_UV_RUN) python scripts/assistant_runtime_regression.py gate --no-write

agent-runtime-eval-contract-gate: ## 验证 V2 Agent Runtime 的离线 Thread/Turn/Item 质量合同
	@$(EVAL_UV_RUN) pytest -q --no-cov \
		tests/services/eval/test_agent_runtime_eval_contract.py \
		tests/services/eval/test_eval_candidate_client.py

agent-eval-core-gate:       ## 运行 Agent Runtime 候选执行、统计、评估器与多代理核心门禁
	@$(MAKE) agent-runtime-eval-contract-gate
	@$(EVAL_UV_RUN) pytest -q --no-cov \
		tests/services/eval/test_candidate_runner.py \
		tests/services/eval/test_experiment_statistics.py \
		tests/services/eval/test_eval_candidate_client.py \
		tests/services/eval/test_evaluator_executor.py
	@echo "Agent Runtime live-provider candidate is not run by this offline gate; use the explicit V2 live gate with AGENT_EVAL_AUTH_TOKEN or AGENT_EVAL_API_KEY."

verify-eval-dev:            ## 运行 Agent Trace/Eval dev 分支验证门禁
	@$(EVAL_RUFF_RUN) ruff check \
		src/api/v1/eval.py \
		src/api/schemas/eval.py \
		src/api/eval_export.py \
		packages/ai-gateway-core/src/ai_gateway_core/eval \
		packages/ai-gateway-core/src/ai_gateway_core/persistence/repositories/agent_trace_repository.py \
		tests/api/test_eval_traces.py \
		tests/api/test_eval_api_trace_tree.py \
		tests/services/eval \
		tests/services/eval/test_agent_runtime_eval_contract.py
	@$(EVAL_UV_RUN) pytest -q --no-cov \
		tests/api/test_eval_traces.py \
		tests/api/test_eval_api_trace_tree.py \
		tests/services/eval/test_eval_permissions.py
	@$(EVAL_UV_RUN) pytest -q --no-cov \
		tests/services/eval/test_evaluator_executor.py \
		tests/services/eval/test_outbox_worker.py \
		tests/services/eval/test_online_sampling.py \
		tests/services/eval/test_eval_llm_client.py \
		tests/services/eval/test_golden_regression_gate.py \
		tests/services/eval/test_trace_retention_scheduler.py \
		tests/services/eval/test_search_indexes.py \
		tests/services/eval/test_drive_shipped_entrypoints.py
	@$(MAKE) eval-e1-gate
	@$(EVAL_UV_RUN) pytest -q --no-cov \
		tests/services/eval/test_agent_runtime_eval_contract.py
	@$(EVAL_UV_RUN) pytest -q --no-cov \
		tests/services/eval/test_ingest_roundtrip.py \
		tests/services/eval/test_trace_capture_helpers.py
	@pnpm -C web lint
	@pnpm -C web type-check

eval-regression-gate:       ## 运行离线 Assistant Eval golden regression gate
	@$(EVAL_UV_RUN) python scripts/eval_golden.py validate tests/fixtures/eval/golden/assistant_regression_v1.jsonl
	@$(EVAL_UV_RUN) python scripts/eval_golden.py gate tests/fixtures/eval/golden/assistant_regression_v1.jsonl \
		--observations tests/fixtures/eval/observations/assistant_regression_v1.jsonl \
		--output $(EVAL_REGRESSION_REPORT_DIR)/latest.json \
		--markdown $(EVAL_REGRESSION_REPORT_DIR)/latest.md

rag-eval-fixture-contract:  ## L1 离线双轨 RAG Eval fixture contract 回放（不调用 provider/活栈）
	@$(EVAL_UV_RUN) python scripts/eval_rag.py validate tests/fixtures/eval/rag/golden/rag_regression_v1.jsonl \
		--observations tests/fixtures/eval/rag/observations/rag_regression_v1.jsonl
	@$(EVAL_UV_RUN) python scripts/eval_rag.py gate tests/fixtures/eval/rag/golden/rag_regression_v1.jsonl \
		--observations tests/fixtures/eval/rag/observations/rag_regression_v1.jsonl \
		--output $(EVAL_E1_ARTIFACT_DIR)/rag-latest.json

# -- Knowledge Base gates ------------------------------------------------------
# kb-unit-gate is the real KB suite: it imports knowledge_service, unlike
# rag-eval-fixture-contract which only replays fixtures offline.
#
# The two tests/scripts files MUST stay ahead of the directory args: they sit
# inside the tests/scripts package, so collecting them inserts tests/ onto
# sys.path, which then shadows the repo-root `scripts` namespace package for
# every later module (collection breaks with "cannot import name
# 'migrate_sparse_vectors' from 'scripts'"). Keep the order.

KB_IMAGE_BUILD_LOCK := apps/knowledge-service/build-requirements.lock.txt
KB_IMAGE_RUNTIME_LOCK := apps/knowledge-service/requirements.lock.txt

kb-image-lock-refresh:      ## 从 uv.lock 重建 Knowledge Service 镜像依赖锁（带 SHA-256）
	@uv export --locked --package knowledge-service --only-group image-build \
		--no-annotate --no-header --no-emit-project --no-emit-workspace \
		--output-file $(KB_IMAGE_BUILD_LOCK)
	@uv export --locked --package knowledge-service --no-dev --no-group image-build \
		--no-annotate --no-header \
		--no-emit-package knowledge-service --no-emit-package ai-gateway-core \
		--no-emit-package ai-gateway-contracts \
		--output-file $(KB_IMAGE_RUNTIME_LOCK)

kb-image-lock-gate:         ## 校验 Knowledge Service 镜像锁同步、哈希与静态分发合同
	@$(EVAL_RUFF_RUN) ruff check tests/scripts/test_knowledge_image_supply_chain.py
	@$(EVAL_UV_RUN) pytest -q --no-cov tests/scripts/test_knowledge_image_supply_chain.py

kb-unit-gate:               ## 运行 Knowledge Service 真实单元门禁（导入 knowledge_service,离线可跑）
	@$(EVAL_UV_RUN) pytest -q --no-cov $(PYTEST_EXTRA) \
		tests/scripts/test_backfill_bm25_v2.py \
		tests/scripts/test_migrate_sparse_vectors.py \
		tests/scripts/test_kb_golden_set.py \
		tests/scripts/test_regen_rag_observations.py \
		tests/scripts/test_import_kb_eval_golden.py \
		tests/harness/test_kb_release_evidence_gate.py \
		tests/services/eval/test_kb_ragas_client.py \
		tests/services/eval/test_kb_ragas_service.py \
		tests/services/eval/test_rag_regression_gate.py \
		tests/services/eval/test_retrieval_metrics.py \
		tests/services/knowledge \
		tests/knowledge

kb-migration-gate:          ## 运行 KB 100–112 + 097/101 restore + 完整链/账本迁移门禁（需临时 Postgres + pg_dump/restore）
	@$(EVAL_UV_RUN) pytest -q --no-cov $(PYTEST_EXTRA) \
		tests/database/test_migration_authority_entrypoints.py \
		tests/database/test_run_migration.py \
		tests/database/test_migration_runner_contract.py \
		tests/database/test_migration_runner_concurrency.py \
		tests/database/test_image_task_runtime_scope_migration.py \
		tests/database/test_kb_migration_full_chain.py \
		tests/database/test_kb_migration_101_restore.py \
		tests/database/test_kb_migrations.py \
		tests/database/test_kb_query_telemetry_migration.py \
		tests/database/test_kb_query_feedback_migration.py \
		tests/database/test_kb_ingestion_lifecycle_migration.py \
		tests/database/test_kb_embedding_versioning_migration.py \
		tests/database/test_kb_embedding_conflict_paths.py \
		tests/database/test_kb_embedding_action_jobs.py \
		tests/database/test_kb_process_rule_snapshot.py \
		tests/database/test_kb_eval_golden_migration.py \
		tests/database/test_bm25_v2_lifecycle_tierb.py \
		tests/database/test_kb_artifact_migrations_tierb.py \
		tests/database/test_kb_document_batch_migration.py

# -- KB golden evaluation foundation (PRD T0-#2/#3/#4/#7) ---------------------
# kb-golden-gate is a fully offline, CI-safe development-structure check. It
# verifies manifest hashes, seed drift, and case shape; it does not establish
# human review, a real-corpus baseline, or release readiness.

KB_GOLDEN_DIR := tests/fixtures/eval/rag/golden

kb-golden-gate:             ## 校验 KB 开发夹具结构: manifest 哈希 + 种子漂移 + case 合同
	@$(EVAL_UV_RUN) python scripts/regen_rag_observations.py verify --manifest $(KB_GOLDEN_DIR)/manifest.json
	@$(EVAL_UV_RUN) python scripts/seed_kb_golden_set.py --check
	@$(EVAL_UV_RUN) python scripts/eval_rag.py validate $(KB_GOLDEN_DIR)/kb_golden_qa_v1.jsonl

kb-release-evidence-gate:   ## T0 发布证据: 人审黄金集 + manifest + 真实语料基线绑定
	@$(EVAL_UV_RUN) python scripts/harness/kb_release_evidence_gate.py

# rag-live-quality-gate is the L3 LIVE tier of the RAG gate family: it replays
# the KB golden set against a running Knowledge Service and gates on measured
# retrieval quality. Distinct from:
#   - rag-eval-fixture-contract (L1, offline fixture replay, no stack)
#   - kb-baseline-record (records a baseline candidate with zero thresholds)
# Thresholds default to 0 with a mandatory sample floor so the gate always
# produces real evidence; release runs must set the RAG_LIVE_MIN_* thresholds
# once a reviewed baseline exists (reports/kb-eval-baseline/).
RAG_LIVE_URL ?= http://localhost:8092
RAG_LIVE_DATASET_ID ?=
RAG_LIVE_MIN_RECALL ?= 0
RAG_LIVE_MIN_MRR ?= 0
RAG_LIVE_MIN_NDCG ?= 0
rag-live-quality-gate:      ## L3 活栈 RAG 质量门禁: 黄金集对真实 KS 回放并门控（需绑定数据集）
	@test -n "$(RAG_LIVE_DATASET_ID)" || { echo "usage: make rag-live-quality-gate RAG_LIVE_DATASET_ID=<bound eval dataset id> [RAG_LIVE_URL=...] [RAG_LIVE_MIN_RECALL=...]"; exit 2; }
	@mkdir -p tmp/rag-live reports/rag-live-quality
	@$(EVAL_UV_RUN) python scripts/regen_rag_observations.py record --retrieval-only \
		--expectations $(KB_GOLDEN_DIR)/kb_golden_qa_v1.jsonl \
		--dataset-id $(RAG_LIVE_DATASET_ID) --url $(RAG_LIVE_URL) \
		--output tmp/rag-live/observations.jsonl --force
	@$(EVAL_UV_RUN) python scripts/eval_rag.py gate $(KB_GOLDEN_DIR)/kb_golden_qa_v1.jsonl \
		--observations tmp/rag-live/observations.jsonl \
		--track retrieval_only \
		--min-total-samples 1 --min-track-samples 1 \
		--min-recall $(RAG_LIVE_MIN_RECALL) --min-mrr $(RAG_LIVE_MIN_MRR) --min-ndcg $(RAG_LIVE_MIN_NDCG) \
		--output reports/rag-live-quality/latest.json

# kb-integration-smoke is NOT wired into CI: it needs the docker-compose.kbms.yml
# Qdrant up. The test skips itself when the stack is unreachable, so the target
# is safe to run at any time.
kb-integration-smoke:       ## Qdrant 集成冒烟（需 kbms 栈；栈未起时自动 skip）
	@$(EVAL_UV_RUN) pytest -q --no-cov -m integration \
		tests/knowledge/test_qdrant_integration_smoke.py

# kb-baseline-record drives the T0-#3 discipline end to end against a LIVE
# knowledge service: regenerate retrieval observations for the KB golden set
# from /retrieve (read-only), then record the hit-rate/MRR/nDCG/recall@k
# distribution into a versioned report under reports/kb-eval-baseline/.
# First round records the distribution only — thresholds stay at zero until
# the baseline is reviewed (agent-kb-eval discipline). Requires
# KB_BASELINE_DATASET_ID: a dataset whose corpus the golden segment ids are
# bound to. This is NOT a CI gate (needs the live stack).
KB_BASELINE_URL ?= http://localhost:8092
KB_BASELINE_DATASET_ID ?=
KB_BASELINE_NAME ?= kb-golden-v1-$(shell date +%Y-%m-%d-run1)
kb-baseline-record:         ## 记录真实语料检索基线候选（需活的 KS + 已绑定的评测数据集）
	@test -n "$(KB_BASELINE_DATASET_ID)" || { echo "usage: make kb-baseline-record KB_BASELINE_DATASET_ID=<bound eval dataset id> [KB_BASELINE_URL=...]"; exit 2; }
	@mkdir -p reports/kb-eval-baseline tmp/eval-e1
	@$(EVAL_UV_RUN) python scripts/regen_rag_observations.py record --retrieval-only \
		--expectations $(KB_GOLDEN_DIR)/kb_golden_qa_v1.jsonl \
		--dataset-id $(KB_BASELINE_DATASET_ID) --url $(KB_BASELINE_URL) \
		--output tmp/eval-e1/kb-baseline-observations.jsonl --force
	@$(EVAL_UV_RUN) python scripts/eval_rag.py gate --track retrieval_only \
		--expectations $(KB_GOLDEN_DIR)/kb_golden_qa_v1.jsonl \
		--observations tmp/eval-e1/kb-baseline-observations.jsonl \
		--min-recall 0 --min-mrr 0 --min-ndcg 0 \
		--min-total-samples 1 --min-track-samples 1 \
		--output reports/kb-eval-baseline/$(KB_BASELINE_NAME).json
	@echo "baseline candidate recorded; it is not release evidence until reviewed, bound, and kb-release-evidence-gate passes"

eval-e1-unit-gate:          ## 运行 Agent stateful 与 RAG Eval E1 单元门禁
	@$(EVAL_UV_RUN) pytest -q --no-cov \
		tests/services/eval/test_agent_observation_adapter.py \
		tests/services/eval/test_golden_regression_gate.py \
		tests/services/eval/test_stateful_agent_eval.py \
		tests/services/eval/test_rag_regression_gate.py \
		tests/services/eval/test_retrieval_metrics.py

eval-e1-gate:               ## 运行 Eval E1 离线 fixture-contract 门禁（Agent + RAG）
	@$(MAKE) eval-e1-unit-gate
	@$(MAKE) eval-regression-gate
	@$(MAKE) rag-eval-fixture-contract

# -- Agent Runtime isolation gate --------------------------------------------

test-isolation:             ## 运行 Agent Runtime 与 Gateway 隔离契约测试
	@uv run pytest -q --no-cov \
		tests/integration/test_gateway_openapi_contract.py \
		tests/integration/test_gateway_boot.py \
		tests/api/test_assistant_control_plane_routes.py

# -- Repository quality gates (ARC-00B) ---------------------------------------

.PHONY: hygiene-check evidence-policy-gate artifact-status artifact-cleanup compatibility-manifest-gate platform-db-convergence-gate agent-execution-integration-gate knowledge-integration-gate fresh-install-gate platform-rollback-rehearsal platform-release-gate loc-no-growth-gate web-quality-gate affected-gates ci-gate-enforcement ci-gate-enforcement-selftest rust-changed-crate-gate gateway-unit-gate

BASE_SHA ?=

hygiene-check:              ## 卫生检查: 空测试体/自证测试、.only/.fixme 扫描（含负向自测）
	@python3 scripts/harness/hygiene_check.py --selftest
	@python3 scripts/harness/hygiene_check.py
	@$(MAKE) evidence-policy-gate

evidence-policy-gate:       ## ARC-07 evidence policy/manifest与具名候选机械合同
	@python3 scripts/evidence/artifacts.py validate
	@python3 scripts/evidence/known_candidates.py

artifact-status:            ## 只读列出scratch artifact分类/年龄/大小/拒绝原因
	@python3 scripts/evidence/artifacts.py status $(ARTIFACT_ARGS)

artifact-cleanup:           ## 默认dry-run；apply需精确authorization manifest和外部quarantine
	@python3 scripts/evidence/artifacts.py cleanup $(ARTIFACT_ARGS)

COMPATIBILITY_LEVEL ?= draft
compatibility-manifest-gate: ## draft验证结构/离线事实；release必须COMPATIBILITY_LEVEL=candidate
	@python3 scripts/release/compatibility_manifest.py --level "$(COMPATIBILITY_LEVEL)"

platform-db-convergence-gate: ## L2真实DB authority/fingerprint/migration matrix；缺DSN=BLOCKED
	@python3 scripts/release/integration_gates.py --gate platform-db

agent-execution-integration-gate: ## L2真实ThreadStore/Runtime/Worker/write-path组合门禁
	@python3 scripts/release/integration_gates.py --gate agent-execution

knowledge-integration-gate: ## L2真实Knowledge unit/Qdrant/live retrieval组合门禁
	@python3 scripts/release/integration_gates.py --gate knowledge

fresh-install-gate:        ## L3隔离quickstart/validate/status；必须显式live authorization
	@python3 scripts/release/integration_gates.py --gate fresh-install

platform-rollback-rehearsal: ## L3 DB+Runtime+Knowledge current→frozen→current组合门禁
	@python3 scripts/release/integration_gates.py --gate rollback

platform-release-gate:     ## ARC-08最终总门禁：完整manifest + L2/L3/fresh/rollback零skip
	@$(MAKE) compatibility-manifest-gate COMPATIBILITY_LEVEL=candidate
	@python3 scripts/release/integration_gates.py --gate all

loc-no-growth-gate:         ## LOC 不增长门禁: 超阈值文件零增长 + 新文件低于阈值（绑定基线清单）
	@python3 scripts/harness/loc_no_growth_gate.py --selftest
	@python3 scripts/harness/loc_no_growth_gate.py

web-quality-gate:           ## Web 质量门禁: type-check + lint + node 单测 + i18n 检查
	@pnpm -C web type-check
	@pnpm -C web lint
	@pnpm -C web test
	@pnpm -C web i18n:check

affected-gates:             ## 由 BASE_SHA 的 diff 选择必须运行的门禁（未匹配路径 = 失败）
	@test -n "$(BASE_SHA)" || { echo "usage: make affected-gates BASE_SHA=<base git sha>"; exit 2; }
	@python3 scripts/harness/affected_gates.py --base "$(BASE_SHA)"

ci-gate-enforcement:        ## CI 最终门禁: diff 所需 job 必须存在且成功（缺失/skip/fail 均失败）
	@python3 scripts/harness/ci_gate_enforcement.py

ci-gate-enforcement-selftest: ## CI 最终门禁的纯离线负向自测
	@python3 scripts/harness/ci_gate_enforcement.py --selftest

rust-changed-crate-gate:    ## 组合 lock-pinned upstream + 当前 overlay，串行测试变更 crate
	@test -n "$(BASE_SHA)" || { echo "usage: make rust-changed-crate-gate BASE_SHA=<base git sha>"; exit 2; }
	@python3 scripts/harness/rust_changed_crate_gate.py --base "$(BASE_SHA)"

gateway-unit-gate:          ## Gateway 离线单元门禁: 排除 integration 标记与 KB/DB 套件
	@$(EVAL_UV_RUN) pytest -q --no-cov -m "not integration" $(PYTEST_EXTRA) \
		tests/unit \
		tests/api \
		tests/core \
		tests/contract \
		tests/harness \
		tests/scripts/test_topology_modes.py \
		tests/services/eval \
		tests/services/agent_runtime \
		tests/services/assistant

# -- Help ---------------------------------------------------------------------

.PHONY: help

help:                       ## 显示此帮助信息
	@echo ""
	@echo "AI Gateway 运维命令:"
	@echo ""
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | \
		awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-20s\033[0m %s\n", $$1, $$2}'
	@echo ""
