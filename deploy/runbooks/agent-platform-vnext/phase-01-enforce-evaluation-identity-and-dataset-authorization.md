# P1-01 — 修正Eval租户身份及Dataset主体边界；依赖P1-00

- PHASE_ID: P1-01
- FEATURE_ID: P1-F002
- DEPENDS_ON: P1-00

## Outcome

修正Eval租户身份及Dataset主体边界；依赖P1-00

## Scope

In:

- 需求：ID-01,ID-02,ID-03,KB-01。
- Owner范围：`src/services/eval`, `src/api/v1/knowledge.py`, KS `auth`, `dataset_service.py`, `persistence`, `database/authority`, `database/migrations`, contracts。Act前以work-packages.yml的文件allowlist为准。

Out:

- 后一期功能、上游kernel算法重写、无关重构、未授权提交/推送/生产变更。

## Done when

- [ ] S01/S02/S03；DB privilege matrix、旧ACL迁移/public-sharing矩阵；先离线反例，再isolated HTTP/DB
- [ ] 直接/隔离/指定live证据及review真实完成；未跑live不标通过。

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| Direct baseline | `uv run --all-packages --extra test pytest -q --no-cov tests/services/eval/test_eval_candidate_client.py tests/api/test_agent_v2.py` | 当前门禁覆盖的直接行为；不替代下列真实场景 |
| Outcome | S01/S02/S03；DB privilege matrix、旧ACL迁移/public-sharing矩阵；先离线反例，再isolated HTTP/DB | 本包需求的真实成功/失败结果 |

S场景与证据层级见[门禁分册](../../../docs/plans/agent-platform-vnext-2026-09/06-evaluation-and-release-gates.md)；精确命令和结果写本包receipt。

## Stop or confirm

- 超出owned paths或公共/数据合同发生冲突，先更新范围/裁决。
- 回滚类别：R1；旧reader须安全栅栏；无安全回滚时不得release。
- 本地环境与既有凭证使用已授权；禁止输出秘密、未授权Git/生产写操作。
