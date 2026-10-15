# R1-RP03 收尾与本地 main 集成（2026-09-27）

结论：独立可执行的 RP03 安全增量已验收，按用户最新“尽快收尾并合入 main”授权进行本地集成。保留 DR01/RP01/RP02；不推送、不删除分支，不宣称完整 R1 / RELEASE_100。

基线 main `2a5b7d74`；实施分支 `codex/r1-remaining-flow-acceptance`。Git 集成事实由同目录 receipt 与最终 Git 状态记录。

## 已修复的真实问题

| 需求/旅程 | 缺陷与最小修正 | 验收证据 |
| --- | --- | --- |
| AS07/AS10，J03 | Send 双击第二击落在新 Stop，误取消任务；Stop 保留单击/键盘，忽略第二击 | 受控1000事件：1280/390px、100工具唯一、两个滚动位置保持、仅一个 turn/interrupt |
| AS05/AS09，J05 | 原视觉任务刷新后上传附件标记丢失；从 immutable snapshot 的 attachments 列表批量恢复，联合 owner/thread/session/run 边界；原文件名在新绑定时校验保存，历史不读取上传存储 | 真实原 Vision：附件1、原像素 SHA一致、重复 history相同，IAB刷新/历史重开保持原答案；零执行请求 |
| AR02/AR03，J08 | 预览有 PDF，访客没有卡片；将冻结白名单中未绑定文件显示一次，跨消息去重，不扩大授权 | 真实 PDF include false/true、匿名下载404/200及原 SHA；撤销后页面/文件404 |
| AS10/AR03，J08 | 自制分享弹窗没有 ESC、焦点管理；复用现有 Radix Dialog，默认关闭按钮兼容其他调用 | IAB Tab在弹窗内、ESC后焦点返回；390px专项 E2E |
| AR03/AR04，J08 | Quiz 需先作答才有分享入口，刷新后找不到旧链接的撤销入口；新增本人分享管理只读 GET，复用现有创建/撤销和 expires_hours；答前题目/选项预览、7/30天/不设期限；公共 scope 不提供管理入口 | 真实既有 C4 Quiz，不重新生成：答前预览、7天回读、匿名答前无答案、刷新列表、UI撤销后 read/start404 |
| AS10，J08 | 390px 分享页长代码地址横向溢出；长词换行、pre 自身滚动 | IAB实际 scrollWidth379 ≤ viewport390；真实分享 E2E复测通过 |
| AR03/AS10，J08 | 公开失效提示仅英文；新增中英文状态与联系所有者获取新链接指引 | 专用链接单行截止时间受控注入：view/download从200变410；IAB中文及中英文E2E |

新增 `GET /api/v1/artifact-shares?quiz_id=<uuid>&limit=1..200` 仅返回当前租户/创建人的管理元数据。无 payload/answer_keys；来源失效仍可管理撤销，创建和公开来源检查未放宽。更新 `sdk/openapi.json`；语义变化仅新 GET 与 ArtifactShareSummary，既有 operation 无语义变化。

旧上传 artifact 无 original_filename 时使用已保存的存储文件名，不猜原名、不改写旧事实；新绑定保存经校验的原名。普通上传存储 display metadata 读取失败不会使已验证字节不可用。

## 分层实际通过

### 自动化

- Python **83 passed**，0 failed/0 skipped：thread_store、artifact_shares、attachment_refs、source_access、conversation_share_quiz。
- Node **8 passed**，0 failed/0 skipped：共享白名单放置、历史状态和活动终态。
- 真实既有服务/文件专项 E2E **2 passed**：PDF 范围/字节/撤销；Quiz 预览/期限/刷新列表/撤销。
- 受控前端活动 E2E **2 passed**：1000事件、1280/390px；不是真实 Worker 吞吐测试。
- 专用链接受控到期的中英文 E2E **1 passed**：真实HTTP410，截止时间为注入，不冒称自然等待一周。
- Ruff、直接 app/node TS、全 Web lint、i18n、build及 bundle budget、OpenAPI2、architecture（609文件/0违规）、harness、diff-check 通过。
- Node实际环境24.14.0/pnpm10.33.0；repo声明Node22，命令有engine警告。验证通过的1个warning及既有OpenAPI重复operationId警告未扩展修正。

### 真实服务 / 内置浏览器

- 已配置 Qwen VL Max 读取自制 PNG 的7319/红色正方形/蓝色圆形全部正确：run `c82c304d-6df6-4052-be21-2a6af9f0bcdd`，原session `aa2b5164-6a12-489c-9b08-e751e4258cb9`，工具0。Qwen3.8Flash 非视觉模型给原因、禁发、保草稿/附件，run0。这是视觉输入链路，不使用生成CDN。
- IAB390：原视觉历史/刷新保持正文和附件标记；真实 PDF 分享卡及下载链接；原 Quiz 答前分享预览/期限/历史链接；两弹窗ESC与焦点返回，Share Tab保持弹窗内；到期页给中文状态/下一步。
- 匿名PDF/Quiz断言由独立无auth标准Playwright context/API验证；IAB公共页面同主账号浏览器环境，不冒称IAB匿名。
- 不重新生成原 PDF/Quiz，不重复原工具。原 Quiz 真实答题/幂等提交/撤销事实沿用 RP01，原长任务停止/资源回收/未知回执和后续轮沿用 RP02，原run重启六崩溃窗口沿用 DR01；本轮未重跑 Rust矩阵或Hosted CI。

### Docker

Compose owner均为本仓库。按规范分别 Python `make hot-update ARGS="--python"` 与前端 `--frontend`；最终Gateway三处源码SHA与工作树一致，前端184个index/JS加5个CSS全部一致。Runtime/Worker沿用 RP02 `a55739b4a6c5` release unit，无Rust源/DB/dependency修改。`make validate` 和 `make status`通过；详见 final-runtime-identity.json。

初次失败均保留事实：活动mobile坐标受打开动画影响，等待面板进viewport后旧CSS通过；真实Send双击缺陷已修复。分享第一次发现窄屏长地址溢出已修复；Quiz第一次测试使用错误GET路径，修为已有 `/quiz/shared/{code}` 后通过。到期脚本第一次使用不存在的容器名，未修改DB；核对 owner 后复用原专用链接，只缩短该行截止时间。没有把这些初次失败算通过。

## Review

独立 closure_review 最终 **PASS**，无确认 blocker/high。核对所有权、快照白名单、去重、无重放、Quiz管理隔离/答案、旧来源撤销及键盘追加修正；独立Python50/Node7均为主结果子集，不相加。实际分享2与到期1、Vision SHA及源码差异已复核。reviewer未执行live；主代理负责Docker/IAB。见 independent-review.md。

## 完整 R1 状态及剩余验收

| 范围 | 当前状态 |
| --- | --- |
| AS01/02/06/08，TL01/02/04 | 保留既有功能与 DR01/RP01/早期验收；未重复重写。会话/草稿、六状态/来源撤销、KB查询引用、记忆CRUD/off和当前safe权限合同已有记录 |
| AS03/TL03 Rust 持久执行 | DR01原run/审批续接、回执去重、取消/撤权/未知不重放已验收且在main；RP02终态后续轮/资源事实保留 |
| AS04/05/07/09/10 | 已有模型/default、附件与活动增量保留；本轮视觉、冷恢复附件、1000活动、窄屏键盘补验通过；真实图片生成正例仍阻塞 |
| AR01～04 | 真实Office/PDF/Quiz、分享范围与受控到期/撤销证据补齐；图片成功及部分产物异常UI组合未完成新实测 |

真实阻塞：最终Docker只读DNS仍将 `dashscope-463f.oss-accelerate.aliyuncs.com` 解析为198.18/15 Fake-IP。安全下载拒绝；未削弱SSRF/TLS，未改变共享网络，未再次调用付费图像生成。单域名最小修正/回滚方案见 cdn-single-domain-proposal.md；配置文件与当前Core内存一致未冒称已核实。

未验证的必需旅程记录保留：客户端断网/后台重连、409保附件草稿新会话的页面操作、有效参数页面与snapshot完整对照、422失效附件页面指引、普通用户引用点击、跨账号/重登录记忆隔离、审批真正到期的页面/执行负例、两产物一失败/零字节等异常UI组合。相关服务端自动化已有部分证据，不能据此把这些页面旅程写成已通过。

因此本次是已验证安全增量的本地main交付，不是完整R1完成声明。没有新账号、凭据输出/复制、权限扩展、共享网络修改、远程推送或分支删除。

证据：`reports/r1-flows/`；临时截图：`tmp/r1-flows/pdf-shared-final.png`、`vision-history-final.png`、`quiz-share-final.png`、`expired-share-final.png`。IAB已恢复普通视口并保留原视觉会话。

## 实际本地集成

实现 `0033b8c6`；记录规范化 `2521d0a4` 已快进合入本地main。最终Docker三处源码SHA与实现commit的Git blobs一致；Runtime/Worker与前端无需为记录commit重建。文档跟随记录随后合入；没有push、history rewrite或branch deletion。归档日志末尾空行在跟随记录中规范化后，最终diff-check通过；早期缓存检查曾报告两处日志空行，不算最终通过。
