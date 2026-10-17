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
#   make hot-update      从原生源码构建镜像并更新选定服务
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
#   make dev-compose     从源码构建开发服务，原生代码修改后需要重新编译
#
#   make help            显示此帮助
# =============================================================================

SHELL := /bin/bash
SCRIPTS := scripts/new
ENV_FILE ?= .env
COMPOSE := $(shell docker compose version >/dev/null 2>&1 && echo "docker compose" || echo "docker-compose")
BUILD_COMPOSE := $(COMPOSE) -f docker-compose.yml -f deploy/compose/docker-compose.build.yml
DEV_COMPOSE := $(BUILD_COMPOSE) -f deploy/compose/docker-compose.dev.yml
COMPOSE_PARALLEL_LIMIT ?= 1
export COMPOSE_PARALLEL_LIMIT

.DEFAULT_GOAL := help

# -- Quick Start --------------------------------------------------------------

.PHONY: doctor harness-check runtime-dependency-gate gateway-kb-boundary-gate architecture-boundary-gate core-boundary-gate single-instance-guard verify-openapi-contract live-openapi-contract-gate agent-runtime-source-build-local agent-runtime-source-contract agent-runtime-build-local agent-runtime-contract agent-runtime-smoke agent-runtime-text-gate agent-runtime-single-kernel-gate agent-runtime-readonly-gate agent-runtime-write-gate agent-thread-store-contract agent-capability-worker-build-local agent-capability-worker-smoke agent-runtime-release-gate agent-runtime-rollback-rehearsal sdk-sse-contract snapshot-gateway-openapi quickstart quickstart-build validate-config validate-example-config validate seed-demo seed-demo-apply

doctor:                     ## 环境体检: 工具/Docker/内存/端口/compose 归属 (只读)
	@ENV_FILE="$(ENV_FILE)" bash $(SCRIPTS)/doctor.sh --env "$(ENV_FILE)"

harness-check: ## Native equivalent gate
	@platform-tools harness-check --repo-root .

runtime-dependency-gate: ## Native equivalent gate
	@platform-tools python-inventory --repo-root . --require-zero

gateway-kb-boundary-gate:   ## 阻止 gateway 重新持有 KB 表 SQL 或 KB 请求 schema（PRD T8.4）
	@ai-platform-openapi gateway-kb-boundary

architecture-boundary-gate: ## Native equivalent gate
	@platform-tools architecture-boundary --repo-root .

core-boundary-gate: ## Native equivalent gate
	@platform-tools architecture-boundary --repo-root .

single-instance-guard: ## Native equivalent gate
	@platform-tools single-instance-guard --repo-root .

agent-runtime-source-contract: ## Native equivalent gate
	@platform-tools source-lock validate --repo-root . --lock deploy/agent-runtime-source/lock.json --require-artifact agent_runtime

agent-runtime-contract:     ## 校验 Rust Agent Runtime 已锁到独立、可运行的 OCI 制品
	@platform-tools source-lock validate \
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
	@agent-runtime-text-gate \
		--env-file "$(ENV_FILE)" \
		--runtime-image "$(AI_PLATFORM_AGENT_RUNTIME_IMAGE)"

agent-runtime-single-kernel-gate: ## 验证唯一 Rust Agent 内核、V1/V2 投影和迁移合同
	@platform-tools agent-runtime-single-kernel-gate
	@ENV_FILE="$(ENV_FILE)" cargo test --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-gateway --lib migration_agent
	@cargo test --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-gateway --test runtime_v2_turn_contract --test runtime_control_plane
	@cargo test --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-migration-authority --test runtime_thread_store_migration
	@echo "Disposable PostgreSQL execution is the separate agent-thread-store-contract L2 gate; this offline target does not certify it."
	@$(MAKE) verify-native-openapi-contract

agent-runtime-readonly-gate: ## Native read-only contributor contract
	@platform-tools agent-runtime-readonly-gate

agent-runtime-write-gate:   ## 校验工具审批、dispatch fence、幂等和中断恢复闭环
	@platform-tools agent-runtime-write-gate \
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
	@$(MAKE) agent-runtime-history-gate
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
	@cargo test --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-client --lib --test sdk_transport
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

snapshot-gateway-openapi: snapshot-native-openapi ## Export independent native candidate

verify-openapi-contract: verify-native-openapi-contract ## Native source-owned public contract

.PHONY: snapshot-native-openapi verify-native-openapi-contract compare-openapi-candidate
snapshot-native-openapi: ## 原生声明候选；完整性由原生门禁校验
	@test -n "$(NATIVE_OPENAPI_PRODUCER)" -a -n "$(OPENAPI_OUTPUT)" -a -n "$(OPENAPI_COVERAGE_OUTPUT)" || (echo "Set NATIVE_OPENAPI_PRODUCER, OPENAPI_OUTPUT and OPENAPI_COVERAGE_OUTPUT (external paths)"; exit 2)
	@ai-platform-openapi snapshot --root . --producer "$(NATIVE_OPENAPI_PRODUCER)" --output "$(OPENAPI_OUTPUT)" --coverage-output "$(OPENAPI_COVERAGE_OUTPUT)" $(if $(NATIVE_OPENAPI_PREVIOUS),--previous "$(NATIVE_OPENAPI_PREVIOUS)")

verify-native-openapi-contract: ## 原生完整公开合同；缺独立候选时失败，不跳过
	@ai-platform-openapi gate --root . --profile native $(if $(NATIVE_OPENAPI_CANDIDATE),--candidate "$(NATIVE_OPENAPI_CANDIDATE)") --evidence-out tmp/gate-evidence/verify-native-openapi-contract.json

compare-openapi-candidate: ## 仅比较制品，不构成原生来源或发布证明
	@test -n "$(OPENAPI_CANDIDATE)" || (echo "Set OPENAPI_CANDIDATE" >&2; exit 2)
	@ai-platform-openapi gate --root . --profile published --candidate "$(OPENAPI_CANDIDATE)" --evidence-out tmp/gate-evidence/compare-openapi-candidate.json

live-openapi-contract-gate: ## Live OpenAPI 合同门禁: 需要活栈; 栈未起时报告 SKIPPED, 绝不冒充 PASS
	@platform-tools live-openapi-contract --repo-root . --evidence-out tmp/gate-evidence/live-openapi-contract-gate.json

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
	@platform-tools compose-image-matrix --repo-root .

.PHONY: database-identity-gate
database-identity-gate:     ## 以公开 fixture 验证全部 Compose 模式的独立数据库身份
	@platform-tools database-identity-gate --repo-root . --matrix

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

hot-update:                 ## 从原生源码构建镜像并更新选定服务
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

dev-reset:                  ## 重建开发容器并保留已有数据卷
	@ENV_FILE="$(ENV_FILE)" bash $(SCRIPTS)/setup-dev.sh --reset

dev-status:                 ## 查看开发环境状态
	@ENV_FILE="$(ENV_FILE)" bash $(SCRIPTS)/setup-dev.sh --status

dev-compose:                ## 从源码构建并挂载后端服务，启用热重载
	@bash $(SCRIPTS)/validate-env.sh --env "$(ENV_FILE)" --config-only
	@$(DEV_COMPOSE) --env-file "$(ENV_FILE)" up -d --build --remove-orphans gateway knowledge-service knowledge-worker agent-runtime agent-capability-worker frontend
	@echo "Development compose is running native services built from the selected source."

dev-compose-logs:           ## 查看源码挂载开发服务日志
	@$(DEV_COMPOSE) --env-file "$(ENV_FILE)" logs -f gateway knowledge-service knowledge-worker agent-runtime agent-capability-worker frontend

# -- Agent Trace / Eval Development Gates ------------------------------------

.PHONY: verify-agent-studio verify-eval-dev agent-eval-core-gate agent-runtime-eval-contract-gate eval-e1-gate eval-e1-unit-gate eval-regression-gate rag-eval-fixture-contract rag-live-quality-gate kb-unit-gate kb-golden-gate kb-release-evidence-gate kb-migration-gate kb-image-lock-refresh kb-image-lock-gate kb-integration-smoke kb-baseline-record verify-assistant-runtime-dev test-isolation

EVAL_REGRESSION_REPORT_DIR ?= tmp/eval-regression
EVAL_E1_ARTIFACT_DIR ?= tmp/eval-e1
# Native Cargo gates retain their own structured output; live gates emit explicit receipts.

verify-agent-studio:        ## 运行 AS-00~AS-08 版本化 Agent Studio 整体回归门禁
	@agent_studio_regression $(ARGS)

verify-assistant-runtime-dev: ## 运行 Assistant Runtime 离线回归门禁 (AHR-01~AHR-04)
	@assistant_runtime_regression gate --no-write

agent-runtime-eval-contract-gate: ## 验证 V2 Agent Runtime 的离线 Thread/Turn/Item 质量合同
	@cargo test --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-eval --test runtime_contract_source
	@cargo test --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-gateway --test runtime_eval_contract
	@cargo test --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-gateway --lib migration_compat::eval

agent-eval-core-gate:       ## 运行 Agent Runtime 候选执行、统计、评估器与多代理核心门禁
	@$(MAKE) agent-runtime-eval-contract-gate
	@cargo test --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-eval --lib
	@cargo test --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-gateway --test eval_online_outbox_contract
	@echo 'Offline candidate/evaluator gate; real-provider acceptance requires the explicit live gate and credentials.'

verify-eval-dev:            ## 运行 Agent Trace/Eval dev 分支验证门禁
	@cargo fmt --manifest-path rust/platform-native/Cargo.toml -p ai-platform-eval -- --check
	@cargo test --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-gateway --lib migration_compat::eval
	@cargo test --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-gateway --test runtime_eval_contract --test eval_online_outbox_contract --test trace_feedback_source_matrix --test agent_studio_eval
	@cargo test --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-eval
	@$(MAKE) eval-e1-gate
	@pnpm -C web lint
	@pnpm -C web type-check

eval-regression-gate:       ## 运行离线 Assistant Eval golden regression gate
	@eval_golden validate tests/fixtures/eval/golden/assistant_regression_v1.jsonl
	@eval_golden gate tests/fixtures/eval/golden/assistant_regression_v1.jsonl \
		--observations tests/fixtures/eval/observations/assistant_regression_v1.jsonl \
		--output $(EVAL_REGRESSION_REPORT_DIR)/latest.json \
		--markdown $(EVAL_REGRESSION_REPORT_DIR)/latest.md

rag-eval-fixture-contract:  ## L1 离线双轨 RAG Eval fixture contract 回放（不调用 provider/活栈）
	@eval_rag validate tests/fixtures/eval/rag/golden/rag_regression_v1.jsonl \
		--observations tests/fixtures/eval/rag/observations/rag_regression_v1.jsonl
	@eval_rag gate tests/fixtures/eval/rag/golden/rag_regression_v1.jsonl \
		--observations tests/fixtures/eval/rag/observations/rag_regression_v1.jsonl \
		--output $(EVAL_E1_ARTIFACT_DIR)/rag-latest.json

# -- Knowledge Base gates ------------------------------------------------------
# Native Knowledge gates exercise production modules; live and migration gates remain separate.

kb-image-lock-refresh:      ## 从原生 Rust 源码与依赖锁重建镜像输入清单（带 SHA-256）
	@native-image-inputs --repo-root . --write

kb-image-lock-gate:         ## 校验 Knowledge Service 镜像锁同步、哈希与静态分发合同
	@native-image-inputs --repo-root .

kb-unit-gate:               ## 运行 Knowledge Service 与原生运维工具的离线 Rust 单元门禁
	@cargo test --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-knowledge-service --lib
	@cargo test --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-knowledge-service --test process_contract --test local_embedding_contract --test rerank_contract
	@cargo test --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-knowledge-ops --lib
	@cargo test --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-gateway-integrations --lib migration_knowledge
	@cargo test --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-eval --test rag
	@cargo test --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-openapi --test kb_release_source_matrix

kb-migration-gate:          ## 运行 KB 100–112 + 097/101 restore + 完整链/账本迁移门禁（需临时 Postgres + pg_dump/restore）
	@echo "Native migration contract: full chain, idempotence, roles, lock serialization, crash rollback, and 101 dump/restore. Specialized historical embedding/artifact assertions are not certified by this target."
	@cargo test --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-migration-authority --test kb_migration_contract -- --ignored --test-threads=1

# -- KB golden evaluation foundation (PRD T0-#2/#3/#4/#7) ---------------------
# kb-golden-gate is a fully offline, CI-safe development-structure check. It
# verifies manifest hashes, seed drift, and case shape; it does not establish
# human review, a real-corpus baseline, or release readiness.

KB_GOLDEN_DIR := tests/fixtures/eval/rag/golden

kb-golden-gate:             ## 校验 KB 开发夹具结构: manifest 哈希 + 种子漂移 + case 合同
	@regen_rag_observations verify --manifest $(KB_GOLDEN_DIR)/manifest.json
	@seed_kb_golden_set --check
	@eval_rag validate $(KB_GOLDEN_DIR)/kb_golden_qa_v1.jsonl

kb-release-evidence-gate:   ## T0 发布证据: 人审黄金集 + manifest + 真实语料基线绑定
	@ai-platform-openapi kb-release-evidence

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
	@regen_rag_observations record --retrieval-only \
		--expectations $(KB_GOLDEN_DIR)/kb_golden_qa_v1.jsonl \
		--dataset-id $(RAG_LIVE_DATASET_ID) --url $(RAG_LIVE_URL) \
		--output tmp/rag-live/observations.jsonl --force
	@eval_rag gate $(KB_GOLDEN_DIR)/kb_golden_qa_v1.jsonl \
		--observations tmp/rag-live/observations.jsonl \
		--track retrieval_only \
		--min-total-samples 1 --min-track-samples 1 \
		--min-recall $(RAG_LIVE_MIN_RECALL) --min-mrr $(RAG_LIVE_MIN_MRR) --min-ndcg $(RAG_LIVE_MIN_NDCG) \
		--output reports/rag-live-quality/latest.json

# Explicit native live smoke requires Qdrant; ordinary cargo tests keep it ignored.
# The named live gate fails if its required stack is unavailable.
kb-integration-smoke:       ## Qdrant 集成冒烟（需 kbms 栈；显式调用时缺栈失败）
	@cargo test --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-knowledge-service --test qdrant_integration_smoke -- --ignored --test-threads=1

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
	@regen_rag_observations record --retrieval-only \
		--expectations $(KB_GOLDEN_DIR)/kb_golden_qa_v1.jsonl \
		--dataset-id $(KB_BASELINE_DATASET_ID) --url $(KB_BASELINE_URL) \
		--output tmp/eval-e1/kb-baseline-observations.jsonl --force
	@eval_rag gate --track retrieval_only \
		--expectations $(KB_GOLDEN_DIR)/kb_golden_qa_v1.jsonl \
		--observations tmp/eval-e1/kb-baseline-observations.jsonl \
		--min-recall 0 --min-mrr 0 --min-ndcg 0 \
		--min-total-samples 1 --min-track-samples 1 \
		--output reports/kb-eval-baseline/$(KB_BASELINE_NAME).json
	@echo "baseline candidate recorded; it is not release evidence until reviewed, bound, and kb-release-evidence-gate passes"

eval-e1-unit-gate:          ## 运行 Agent stateful 与 RAG Eval E1 单元门禁
	@cargo test --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-eval --lib --test golden --test rag --test runtime_contract_source
	@cargo test --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-gateway --test runtime_eval_contract

eval-e1-gate:               ## 运行 Eval E1 离线 fixture-contract 门禁（Agent + RAG）
	@$(MAKE) eval-e1-unit-gate
	@$(MAKE) eval-regression-gate
	@$(MAKE) rag-eval-fixture-contract

# -- Agent Runtime isolation gate --------------------------------------------

test-isolation:             ## 运行 Agent Runtime 与 Gateway 隔离契约测试
	@platform-tools live-openapi-contract --repo-root . --evidence-out tmp/gate-evidence/isolation-openapi.json
	@cargo test --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-gateway --test agent_studio_boot --test agent_studio_authorization --test runtime_control_plane
	@cargo test --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-gateway --test agent_studio_boot gateway_boot_constructs_all_production_routes_in_native_subprocess -- --ignored --exact

# -- Repository quality gates (ARC-00B) ---------------------------------------

.PHONY: hygiene-check evidence-policy-gate artifact-status artifact-cleanup compatibility-manifest-gate platform-db-convergence-gate agent-execution-integration-gate knowledge-integration-gate fresh-install-gate platform-rollback-rehearsal platform-release-gate loc-no-growth-gate web-quality-gate affected-gates ci-gate-enforcement ci-gate-enforcement-selftest rust-changed-crate-gate gateway-unit-gate

BASE_SHA ?=

hygiene-check:              ## 卫生检查: 空测试体/自证测试、.only/.fixme 扫描（含负向自测）
	@platform-tools hygiene-check --selftest
	@platform-tools hygiene-check --skip-baseline docs/architecture/baselines/2026-10-native/skip-baseline.json
	@$(MAKE) evidence-policy-gate

evidence-policy-gate:       ## ARC-07 evidence policy/manifest与具名候选机械合同
	@platform-tools evidence validate
	@platform-tools known-candidates

artifact-status:            ## 只读列出scratch artifact分类/年龄/大小/拒绝原因
	@platform-tools evidence status $(ARTIFACT_ARGS)

artifact-cleanup:           ## 默认dry-run；apply需精确authorization manifest和外部quarantine
	@platform-tools evidence cleanup $(ARTIFACT_ARGS)

COMPATIBILITY_LEVEL ?= draft
compatibility-manifest-gate: ## draft验证结构/离线事实；release必须COMPATIBILITY_LEVEL=candidate
	@platform-release compatibility --level "$(COMPATIBILITY_LEVEL)"

platform-db-convergence-gate: ## L2真实DB authority/fingerprint/migration matrix；缺DSN=BLOCKED
	@platform-release integration --gate platform-db

agent-execution-integration-gate: ## L2真实ThreadStore/Runtime/Worker/write-path组合门禁
	@platform-release integration --gate agent-execution

knowledge-integration-gate: ## L2真实Knowledge unit/Qdrant/live retrieval组合门禁
	@platform-release integration --gate knowledge

fresh-install-gate:        ## L3隔离quickstart/validate/status；必须显式live authorization
	@platform-release integration --gate fresh-install

platform-rollback-rehearsal: ## L3 DB+Runtime+Knowledge current→frozen→current组合门禁
	@platform-release integration --gate rollback

platform-release-gate:     ## ARC-08最终总门禁：完整manifest + L2/L3/fresh/rollback零skip
	@$(MAKE) compatibility-manifest-gate COMPATIBILITY_LEVEL=candidate
	@platform-release integration --gate all

loc-no-growth-gate:         ## LOC 不增长门禁: 超阈值文件零增长 + 新文件低于阈值（绑定基线清单）
	@platform-tools loc-no-growth --selftest
	@platform-tools loc-no-growth

web-quality-gate:           ## Web 质量门禁: type-check + lint + node 单测 + i18n 检查
	@pnpm -C web type-check
	@pnpm -C web lint
	@pnpm -C web test
	@pnpm -C web i18n:check

affected-gates:             ## 由 BASE_SHA 的 diff 选择必须运行的门禁（未匹配路径 = 失败）
	@test -n "$(BASE_SHA)" || { echo "usage: make affected-gates BASE_SHA=<base git sha>"; exit 2; }
	@platform-tools affected-gates --base "$(BASE_SHA)"

ci-gate-enforcement:        ## CI 最终门禁: diff 所需 job 必须存在且成功（缺失/skip/fail 均失败）
	@platform-tools ci-gate-enforcement

ci-gate-enforcement-selftest: ## CI 最终门禁的纯离线负向自测
	@platform-tools ci-gate-enforcement --selftest

rust-changed-crate-gate:    ## 组合 lock-pinned upstream + 当前 overlay，串行测试变更 crate
	@test -n "$(BASE_SHA)" || { echo "usage: make rust-changed-crate-gate BASE_SHA=<base git sha>"; exit 2; }
	@platform-tools rust-changed-crate-gate --base "$(BASE_SHA)"

gateway-unit-gate:          ## Gateway 离线单元门禁: 排除 integration 标记与 KB/DB 套件
	@cargo test --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-gateway -p ai-platform-gateway-foundation -p ai-platform-gateway-integrations -p ai-platform-auth -p ai-platform-eval -p ai-platform-openapi -p ai-platform-tools --lib --tests

# -- Help ---------------------------------------------------------------------

.PHONY: help

help:                       ## 显示此帮助信息
	@echo ""
	@echo "AI Gateway 运维命令:"
	@echo ""
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | \
		awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-20s\033[0m %s\n", $$1, $$2}'
	@echo ""

# Native migration entry points. All generated artifacts must remain external.
NATIVE_MANIFEST := rust/platform-native/Cargo.toml
NATIVE_BIN_DIR ?= $(HOME)/.local/bin
.PHONY: native-check native-test native-install native-engine-build native-build-path-guard
native-build-path-guard:
	@test -n "$(CARGO_TARGET_DIR)" || { echo "Set an external CARGO_TARGET_DIR"; exit 2; }
	@mkdir -p "$(CARGO_TARGET_DIR)"; target=$$(cd "$(CARGO_TARGET_DIR)" && pwd -P); root=$$(pwd -P); case "$$target/" in "$$root/"*) echo "CARGO_TARGET_DIR must be outside the source checkout"; exit 2;; esac
native-check: native-build-path-guard ## Check every native service and test target
	@test -n "$(CARGO_TARGET_DIR)" || (echo 'Set CARGO_TARGET_DIR to an external build directory'; exit 2)
	@CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 cargo check --locked --manifest-path $(NATIVE_MANIFEST) --workspace --all-targets
native-test: native-build-path-guard ## Run native unit and integration contracts
	@test -n "$(CARGO_TARGET_DIR)" || (echo 'Set CARGO_TARGET_DIR to an external build directory'; exit 2)
	@CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test --locked --manifest-path $(NATIVE_MANIFEST) --workspace
native-engine-build: native-build-path-guard ## Build the native SDK execution process
	@test -n "$(CARGO_TARGET_DIR)" || (echo 'Set CARGO_TARGET_DIR to an external build directory'; exit 2)
	@CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 cargo build --locked --manifest-path rust/agent-runtime-overlay/kernel-rs/ai-platform-sdk-agent-engine/Cargo.toml
NATIVE_INSTALL_PACKAGES := ai-platform-client-packaging ai-platform-client ai-platform-gateway ai-platform-knowledge-service ai-platform-migration-authority ai-platform-sdk-agent-worker ai-platform-local-node ai-platform-tools ai-platform-release ai-platform-eval ai-platform-config-loader ai-platform-agent-plugins ai-platform-knowledge-ops ai-platform-openapi ai-platform-auth
NATIVE_INSTALL_BINS := local_live_acceptance macos_computer_live_acceptance platform-client-packaging platformctl gateway gateway_capacity_inventory platform-knowledge-service database-authority sdk-agent-worker local-node platform-tools platform-release platform-knowledge-ops ai-platform-openapi prepare_native_gateway_config validate_agent_plugin admin-create repair-usage-metrics check-successor-proposals render-baseline-contract freeze-baseline probe-successor-roles database-policy generate-database-grants gateway-preflight recovery-pack verify-local-stack uat-gateway-capacity-probe uat-gateway-capacity-smoke agent_studio_regression assistant_runtime_regression assistant_ttft_benchmark collect_cra_compliance_research eval_general_agent_failure_recovery eval_golden eval_rag native_agent_parity_benchmark ppr00_resource_profile real_agent_scenario_runner regen_rag_observations seed_kb_golden_set validate_real_finance_eval ai-platform-sdk-agent-engine native-image-inputs agent-runtime-eval-contract agent-runtime-text-gate assistant_capability_live judge_real_agent_receipts verify_coding_parallel_fixture runtime_recovery_reference runtime_recovery_observe runtime_durable_recovery_gate
native-install: native-engine-build ## Build and install native services and operational tools
	@CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 cargo build --locked --manifest-path $(NATIVE_MANIFEST) $(foreach p,$(NATIVE_INSTALL_PACKAGES),-p $(p)) --bins
	@install -d "$(NATIVE_BIN_DIR)"
	@for binary in $(NATIVE_INSTALL_BINS); do install "$(CARGO_TARGET_DIR)/debug/$$binary" "$(NATIVE_BIN_DIR)/$$binary"; done

.PHONY: agent-runtime-history-gate
agent-runtime-history-gate: ## N-1 native rollout decoder corpus; missing evidence blocks release
	@bash scripts/harness/check_agent_runtime_history.sh
