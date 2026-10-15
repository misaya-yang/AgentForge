# R1 剩余产物与长任务验收（R1-RP02）

基线1107bd06，分支codex/r1-artifacts-long-tasks。DR01/RP01已接受，不重复；主代理唯一写入，最多三名只读子代理。最新用户授权尽快收尾并合入本地main，覆盖此前本轮不提交/合并限制；允许必要本地commit，不推送/删分支。

| 需求/旅程 | 复用有效证据 | 本轮必需验收 | 当前状态 |
| --- | --- | --- | --- |
| AS09/AR01 J05/J08 | 图片失败/unknown及安全回执自动化、真实历史unknown；不自动重试 | 新独立图片成功、视觉输入、刷新回看、实际生效模型 | 按下文最终状态 |
| AR01/AR02 J08 | 真实DOCX与字节下载E2E通过，IAB实际卡片 | 真实PDF内容、预览/下载/刷新/失效 | 按下文最终状态 |
| AS03/AS07/TL03 J03 | DR01同run恢复和副作用去重已通过 | 长Python主动停止的Worker结果/进程事实及下一轮 | 按下文最终状态 |
| AS07/AS10 J03/J01 | 去重/历史不滚底单测，390px附件真实读取 | 1000事件受控交互、390px审批/停止/产物/引用键盘 | 按下文最终状态 |
| AR03/AR04 J08 | 普通Quiz无答案泄漏/幂等/撤销真实API，RP01私有来源限制 | 分享页面范围、访客失效与窄屏受影响路径 | 按下文最终状态 |

Compose owner核对为本仓库，9个服务均healthy，保留C4 Runtime/Worker。旧报告J03阻塞已被DR01关闭，不沿用旧状态。

## 已复现图片缺陷

独立UI任务927B为unknown（7.56秒），零artifact。配置DashScope/wan2.6-t2i，key存在，GET只读探针网络正常。新独立provider诊断927C实际submit200→PENDING→RUNNING→SUCCEEDED，但适配器no_image/unknown；不是重放旧未知任务。当前只解析results/images，漏Wan2.6官方choices.message.content[].image。

最小扩展RP02 owned paths为共享image_generation.py及对应既有测试，复用原safe_fetch、图片签名/大小验证及存储通道。响应结构依据[官方Wan文生图API](https://www.alibabacloud.com/help/en/model-studio/text-to-image-v2-api-reference)，不改变执行/权限架构。

解析修复后provider诊断927E实际SUCCEEDED并找到choices图片，随后原safe_fetch拒绝CDN；只读CDN根域及DNS分类确认Docker解析到198.18.0.0/15非公网benchmark地址。不能放宽SSRF或证书校验。环境前提：该DashScope OSS CDN需通过真实公网DNS解析（如本机代理Fake-IP过滤/真实DNS规则）；本轮未更改用户网络设置。新927D生成中刷新也复现产品缺口：server unknown/一task/零artifact，但历史仅user，无助手终态；需要接通现有持久图片事实的只读恢复，独立于外部CDN前提。

PDF实际9451字节、一页，%PDF头、MIME正确，中英标题/数字/两条列表可读（PDF文字提取Latin/CJK间插空格，视觉内容完整）。critic_passed=false为渲染器明确NotRun，非质量失败，不伪报自检通过。390px产物面板Preview目前仅下载，没有PDF打开入口，实际复现；最小复用authenticatedDownload.openAssistantArtifact补PDF预览入口，并保证移动操作可见，新增owned path为ArtifactsPanel.tsx。不重写下载系统。

图片history修正：复用持久image_turns/task，只读对账并按时间恢复一条助手终态；旧水印display/raw归属及diagnostic ID去重。运行中恢复仅history+artifact GET轮询，与Rust observedRun分开，不生成新任务/不发送取消到错误run。明确来源仍使用RP01同创建时刻来源规则。定向Python89、TSC/ESLint/Ruff通过，真实927D未知历史最终UI已重新打开验证：单助手unknown、原task ID、零Rust run ID，刷新无provider重放。

长Python实际执行与ESC停止：原run e5dd2355-a4cc-4845-84d6-1b7f461bd378，单execution/attempt执行中且看到Python PID145/私有namespace；ESC后原run cancelled、execution side_effect_unknown，PID和私有mount namespace无残留，Worker健康。刷新后execution/attempt不变。ESC全局hook实际有效，未按静态疑点改写键盘系统。活动面板真实仍显示工具“执行中…”，而unknown图片标题显示“执行失败”；仅视图终态归类修正为未结束动作unknown，待审批未获准为not_executed，保留已完成/失败工具事实，前端同步后内置浏览器原停止历史已验证标题为结果未知，无假running提示；受控Node验证unfinished→unknown/pending→not_executed。

## 快速收尾边界

2026-09-27最新用户要求尽快完成硬问题后合入main。冻结新增验收扩展；本轮只关闭已复现缺陷、验证受影响路径。剩余图像真实成功受CDN Fake-IP DNS阻塞，未授权变更共享主机网络，也不放宽SSRF；未覆盖项如实保留，不把本地合入等同完整R1验收通过。

取消后同thread新turn实际503，Runtime只读诊断500/thread_store_failure；原cancelled run owner已过期。持久tool ceiling复用无写入却调用旧owner fence，改为健康/归属/删除/不可扩权检查后的只读返回；首次绑定和所有写入fence原样保留。独立review确认最小修正，无清claim/续租/重放。

## 本轮已通过的独立检查

- Python89（图片provider、Gateway图片、session/source ACL与新图片历史回归）；Node14（终态分类/取消回执/活动终态与顺序）；源码合同18；OpenAPI2。
- 受影响Ruff、app/node TypeScript、ESLint、i18n、architecture-boundary（609文件零违规）、harness通过。harness原有程序schema/init警告保留；未执行host Rust或hosted CI。
- 标准完整Chromium E2E2：真实既有PDF认证下载逐字节SHA一致、认证blob预览、390px刷新回看；明确受控的image pending→unknown恢复，仅GET、零execution POST，草稿存在时发送阻止正确。初次测试的未开产物面板/重复历史按钮locator及headless-shell无PDFviewer问题已纠正；不记作产品已修复缺陷。
- Docker当前Python/core4源文件与188前端JS/CSS逐一哈希匹配；新Rust Runtime candidate编译成功（a55739b4a6c5），Worker同源release unit已构建并部署健康（同a55739b4a6c5），不用旧C4健康冒充新修正实测。
- 内置浏览器已复核原长Python停止后历史为结果未知，活动标题一致且无工具仍在执行的提示；真实工具已dispatch的未知回执保持不变。

证据：reports/r1-artifacts/source-bundle-identity.json、independent-review.md及本目录facts。规范命令日志在tmp/r1-artifacts，最终检查输出归档后补全。

## 最终收尾结果

本轮修复的安全增量达到CANDIDATE_80，准予本地main合入；完整R1未达到RELEASE_100。

| 范围 | 已修复/实际通过 | 剩余边界 |
| --- | --- | --- |
| AS03/TL03 | 保留DR01原run持久恢复；本轮原thread待审批取消、旧cancelled/succeeded owner真实过期后新轮成功，刷新/重开后execution/attempt不增 | Core内部主动卸载且无订阅者的额外冷分支未制造；不扩大原DR01受支持任务边界 |
| AS02/AS07/TL02 | unknown标题不再误报失败，停止不再假running或假完成；待审批取消显示未执行，审批卡消失；原unknown事实不变 | 原长Python资源证明见更新前实际PID/namespace回执，不从本轮重启推断回收 |
| AS09/AR01 | Wan choices解析回归；真实unknown图片恢复单条原ID；受控pending→unknown GET-only、草稿阻止发送通过 | 真实图片成功下载仍被CDN Fake-IP DNS阻塞；视觉输入未执行，未重试旧unknown或再花图片额度 |
| AR01/AR02 | 真实PDF9451字节/1页内容完整；390px真实下载SHA一致、完整Chromiumblob预览和刷新通过；IAB真实预览入口 | IAB不提供可观测原生PDF页面，不声明IAB PDF渲染通过；失效/部分PDF失败未补新实测 |
| AS10/AR03/AR04 | 本轮390px键盘审批/停止/产物，复用RP01普通Quiz匿名提交/幂等/撤销和私有来源拒绝证据 | 1000事件、额外分享范围/窄屏组合未执行；仍列为完整R1未验证，不无限扩展本轮 |

### 新候选的实际命令

- `AI_PLATFORM_AGENT_RUNTIME_SOURCE=/Users/yang/projects/opensource-harness/codex-harness make agent-runtime-build-local`：Docker Runtime编译通过。首次继承宿主机127.0.0.1代理失败，重跑仅移除构建进程HTTP/HTTPS/ALL代理变量；无主机网络修改。
- 同固定源码 `make agent-capability-worker-build-local`：Worker编译通过；release unit `a55739b4a6c5`与锁一致。
- 同固定源码 `make agent-runtime-recovery-tests`：Docker内Runtime78、Worker80、Core恢复2，合计160 passed/2 ignored；两个PG fixture是可选忽略项，不冒充通过。新增过期claim读取回归位于该可选fixture，本轮生产过期语义证据来自真实同thread两轮续发。
- `make hot-update ARGS="--frontend"` 与 `make hot-update ARGS="--gateway"`：分别同步188资源、Python/core和新Rust release unit。仅本地ignored .env三项非秘密Runtime/Worker/revision pin更新，无复制凭据。
- `make agent-runtime-source-contract`18、`make verify-openapi-contract`2、`make architecture-boundary-gate`、`make harness-check`、`make validate`、`make status`通过。Starlette/OpenAPI已有警告、harness旧schema/init警告、本地bootstrap配置warning保留。
- 前述Python89、Node14、app/node TS、定向ESLint/Ruff/i18n以及完整Chromium E2E2通过。精确路径和输出在reports/r1-artifacts/final-*.txt；无host Rust、hosted CI或全产品测试声明。

### 原会话真实续发与重复执行断言

内置浏览器390px原thread `01a0e41a-9b1b-76b0-969d-d746ea69f9ed`：新待审批run `58b9a7b7-a735-4709-84b5-458de2d3baac`主动取消，approval cancelled/executions0。确认该owner_expired=true后发送新纯文本，run `bbf3e4b0-960e-4d1a-b710-07290a7c5376` succeeded；确认其owner也过期后，run `b2a7dd8c-287a-4992-b0b1-40a1272d7db5` succeeded。两新文本都无工具execution。

刷新并重新打开历史，四原run身份与终态稳定，两个成功正文各一条；原Python `e5dd2355-a4cc-4845-84d6-1b7f461bd378` execution `01a0e41c-0f2a-7eb1-87ce-421a27b3d65b`、attempt `call_199dbab1f1bf4e998b1fa968`及side_effect_unknown完全不变。`followup-invariants.json`全部断言通过。浏览器viewport已恢复默认，保留原会话结果；截图为tmp/r1-artifacts/final-same-thread-success.png。

### Review与收口

独立closure_review最初发现待审批覆盖取消/失败，主代理最小修正并实际复测；最终静态+facts review PASS，无剩余blocker/high，可合入安全增量。Reviewer独立Python23、Node12、diff-check通过，没有操作live；实际Docker/IAB为主代理。详细报告见independent-review.md。

不改代理/SSRF/TLS，不重放unknown，不创建账号，不扩展R2、不迁移数据库。旧C4镜像保留，回滚需恢复对应三项本地非秘密pin并按既有release unit流程同步；没有删除镜像/缓存/分支。远程推送未授权。

## 本地合入事实

已在codex/r1-artifacts-long-tasks提交实现 `9373a65005fbbf440937d79433606262c52bd5cc`，本地main已从1107bd06 fast-forward到该实现。最终收据文档另记一笔；未推送、未删除分支。Docker运行候选与该实现源文件一致，不需要再次重启来同步文档。完整R1仍未验收完成；本轮安全增量收口。
