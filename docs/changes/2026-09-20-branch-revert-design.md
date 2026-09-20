# Branch / Revert 实现设计（OpenPaw 引擎宿主）

- 来源任务 ID：待定（关联 T7 attached、T9 eval；团队大纲 §3.2 降级路由）
- 变更 ID：`2026-09-20-branch-revert-design`
- 状态：**设计待确认，未开工**（本文档不构成实现授权）
- 开发依据：用户 2026-09-20「现在 branch 和 revert 功能是不是不具备，可以思考一下怎么实现」；`openwork/AGENTS.md`（证据面为 `evals/specs/**`）；`openpaw/AGENTS.md`
- 基线：openwork `3e58d95`（工作区已回退 opencode-connection 补丁）、openpaw `9e5be37`（工作区含 SSE 路由符合性等未提交改动）

## 1. 结论：不具备，而且是"静默失败"而不是"明确不支持"

2026-09-20 对运行中的引擎宿主实测（session `ae861dd082674734804cfba18a4662f0`）：

| 请求 | 实测 | 应该 |
|---|---|---|
| `POST /session/:id/fork` | **204，空 body** | 200 + 新 Session |
| `POST /session/:id/revert` | **204，空 body** | 200 + 带 `revert` 的 Session |
| `POST /session/:id/unrevert` | **404（路由不存在）** | 200 + 清空 `revert` 的 Session |
| `POST /session/:id/{summarize,shell,command}` | 204，空 body | 本次不处理 |

前端拿到空 body 会直接抛错：`unwrap()` 只接受 `result.data !== undefined`（`apps/app/src/app/lib/opencode.ts:307-319`），因此用户看到的是
`session.branch_failed`（`i18n/locales/en.ts:1245`）/ `session.revert_failed`（`en.ts:1244`）的 toast，而不是"该功能不可用"。

## 2. 契约（OpenWork 侧的真实调用面）

来源均为 openwork 仓库（只读核对）：

- `apps/app/src/app/lib/opencode-session.ts:95-122`
  - `revertSession(client, sessionID, messageID)` → `client.session.revert({sessionID, messageID})` → 期望 `Session`
  - `forkSession(client, sessionID, messageID?)` → `client.session.fork({sessionID, messageID})` → 期望 `Session`
  - `unrevertSession(client, sessionID)` → `client.session.unrevert({sessionID})` → 期望 `Session`
- `apps/app/src/react-app/domains/session/sync/transcript-reconcile.ts:36-74`
  - **revert 游标语义**：`revert.messageID` 是**第一条被撤销的消息**，`id >= 游标` 的消息全部隐藏（游标本身也隐藏）。
  - **fork 边界语义**：OpenCode 复制**严格早于**给定 id 的消息；前端因此传"被点击消息的下一条真实消息 id"，若点击的是最后一条则传 `null`（= 整会话分支）；合成消息（`session-error:*`）不参与边界。
- `apps/app/src/react-app/domains/session/sync/session-sync.ts:912-921`：从 `session.updated` 事件的 `info.revert` 同步游标到快照缓存 → **事件必须带上新 Session**。
- `apps/app/src/react-app/shell/session-route.tsx:1512-1537`、`actions-store.ts:699-760`、`safe-edit-resend.ts`：revert 之前先 abort；redo 走 `unrevert`；`applySessionRevert` 用返回值打游标。
- `apps/app/src/app/types.ts:24`：Session 有 `parentID?: string | null` → fork 产物应带父会话 id。
- UI 入口：`components/chat/message-list.tsx:770,1394`（"Branch in new chat"）。

## 3. OpenPaw 侧现状（改哪里）

- **路由**：`crates/openpaw-engine-host/src/lib.rs:158-160` 把 `fork`/`revert`/`summarize` 指向 `http::degraded`；`http.rs:108-112` 的 `degraded()` 返回 `204 NO_CONTENT` 且无 body。**`unrevert` 完全没有路由**。
- **协议**：`protocol.rs:19-25` 的 `Session {id,title,slug,directory,time}` —— 无 `revert`、无 `parentID`。
- **状态模型**：`runtime.rs:202-215`
  - `sessions: Vec<Session>`（内存）
  - `messages: HashMap<sid, Vec<Message>>`（**wire 层**，`GET /session/:id/message` 的数据源）
  - `engine_sessions: HashMap<sid, Arc<AsyncMutex<openpaw_core::Session>>>`（**模型上下文层**）
  - `writers: HashMap<sid, SessionWriter>`（`<home>/sessions/<id>.jsonl`）
- **一轮对话的上下文来源**：`http.rs:271-291` 把 user 消息 push 进 `engine_session.messages`，`run_turn` 之后把新增部分 upsert 成 wire 消息。→ **模型上下文就是 `engine_session.messages`**，这是 revert 能真正生效的着力点。
- **可复用语义**：`openpaw-core/src/session.rs:162-175` 已有 `parent_session_id` 与 `agent_type`（注释明确 `"fork"` = forked child session）。
- **没有工作区快照机制**：`git stash/write-tree/checkout --` 只出现在 `openpaw-tools/src/bashast/readonly.rs` 的只读判定里。

## 4. 设计

### 4.1 协议层（`protocol.rs`）

```rust
pub struct Session {
    pub id: String,
    pub title: String,
    pub slug: String,
    pub directory: String,
    pub time: Time,
    #[serde(rename = "parentID", default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revert: Option<Revert>,
}

pub struct Revert {
    #[serde(rename = "messageID")] pub message_id: String,
    #[serde(rename = "partID", default, skip_serializing_if = "Option::is_none")]
    pub part_id: Option<String>,
}
```

请求体沿用既有手写解析风格（`http.rs` 里 `parse_create_body` / `parse_prompt_body` 的做法：手解析以便畸形体返回 400 而不是 axum 的 422）：

- `ForkRequest { #[serde(rename="messageID")] message_id: Option<String> }`
- `RevertRequest { #[serde(rename="messageID")] message_id: String, #[serde(rename="partID")] part_id: Option<String> }`

### 4.2 状态层（`runtime.rs`）

新增四个方法，全部返回 `Result<Session, RevertError>`（错误枚举映射到 400/404/409）：

| 方法 | 职责 |
|---|---|
| `fork_session(src: &str, boundary: Option<&str>) -> Result<Session, _>` | 新建分支会话并复制 wire 消息与上下文 |
| `revert_session(id: &str, message_id: &str, part_id: Option<String>) -> Result<Session, _>` | 打游标 + 截断模型上下文 |
| `unrevert_session(id: &str) -> Result<Session, _>` | 清游标 + 从 wire 层重建上下文 |
| `session_is_busy(id) -> bool` | 复用 `loops` 判断 busy |

**关键设计：wire 层与上下文层分工**

- wire 层（`messages`）**保留全部消息**——这是 `GET /session/:id/message` 的返回源，也是 `unrevert` 能恢复的唯一依据。
- 上下文层（`engine_session.messages`）**按游标截断**——这才是"撤销"的实质：revert 之后模型不能再看到被撤销的对话。
- 每次 `prompt` 入口先幂等地应用一次游标（防御并发/交错），避免已撤销内容重新进入上下文。

### 4.3 fork 的步骤

1. 解析边界：`boundary = messageID`；在 wire `messages[src]` 中找到第一条 `info.id == boundary` 的下标 `k`，复制 `[..k]`（**严格早于**，与 `transcript-reconcile.ts:57-74` 一致）。`boundary` 为 `None` → 复制全部。`boundary` 给了但找不到 → 400 `unknown_message`。
2. `create_session(directory, title)` 复用现有创建路径（同时落好 JSONL 与事件通道）。标题建议 `"{源标题} (branch)"`，并截断到既有 80 字符规则。
3. 复制 wire 消息到新会话 `messages[new]`。
4. 用同一批消息重建新会话的 `engine_session.messages`（`openpaw_core` 消息类型）。
5. 设置 `parent_id = Some(src)`；同时在核心 `Session` 上置 `agent_type = Some("fork")`（复用既有语义）。
6. 持久化：把复制进来的消息按 `SessionEvent` 追加到新 JSONL。
7. emit `session.created`（新会话）；**不**逐条 emit `message.updated`——前端切换会话时会全量 `GET /session/:new/message`。
8. 返回 200 + 新 `Session`。

### 4.4 revert / unrevert 的步骤

- 前置校验：会话存在（404）；`messageID` 命中 wire 消息（400 `unknown_message`）；会话 busy → 409（前端本来就会先 abort，见 `session-route.tsx:1512-1516`）。
- `revert`：写 `session.revert` → `engine_session.messages.truncate(k)` → emit `session.updated` → 200 + Session。
- `unrevert`：清 `session.revert` → 用 wire 消息按当前可见前缀重建 `engine_session.messages` → emit `session.updated` → 200 + Session。
- `partID`：V1 **只接受并回存，不参与渲染**（前端游标只看 `messageID`），保证契约完整、实现最小。
- 持久化：新增 `SessionEvent::Revert { message_id, part_id }` 与 `SessionEvent::RevertCleared`，写入 JSONL。

### 4.5 必须同步修改的验收资产（跨仓库）

- `openwork/evals/specs/openpaw-engine-attach.test.ts`：把降级断言从 `["command","shell","fork","revert","summarize"]` 收窄为 `["command","shell","summarize"]`，并为三条新路由补**可观察断言**：
  - fork：返回新 session、`parentID` 指向源会话、消息条数符合边界语义；
  - revert：返回的 Session 带 `revert.messageID`，且后续 `prompt_async` 的上下文不含被撤销消息（可用 mock 断言）；
  - unrevert：`revert` 消失。
  按 openwork `AGENTS.md`：**默认不新增测试文件**，扩展现有 spec。
- `openpaw/crates/openpaw-engine-host/tests/http_contract.rs`：补三条路由的状态码与 body 形状（200/400/404/409）。
- 团队大纲 §3.2 的"降级路由"清单需要同步（fork/revert 移出降级列表）。

### 4.6 Phase 2：文件级撤销（需要决策）

真实 OpenCode 的 `revert` 还会把工作区文件恢复到该消息之前的快照；OpenWork 的 "编辑并重发" 路径（`safe-edit-resend.ts`）语义上也依赖这一点。OpenPaw 目前**没有任何快照机制**。两个可选方案：

- **(a) 影子 git 仓库**：每轮 turn 前在 `<home>/snapshots/<sid>/` 用 `git write-tree` / `commit-tree` 记录树哈希，把哈希存进该轮消息的元数据；revert 时 `git checkout <tree> -- .` 恢复工作区（不触碰用户仓库历史）。需要新增 git plumbing 封装（`openpaw-sandbox` 里没有），工作量中等。
- **(b) 只做会话级撤销**（Phase 1），文件变化不回收，在文档与 UI 文案上明确边界。

建议：先交付 Phase 1 让两个按钮可用，再单独评估 (a)——它是独立能力，且涉及"是否允许引擎改写用户工作区文件"的产品决策。

## 5. 风险与未决问题

1. **host 重启不重放会话**：`RuntimeState::with_provider_config` 开始时 `sessions` 为空，JSONL 只写不读（`runtime.rs:229`）。因此本次的游标持久化**当前没有消费方**；若要求跨重启保留 revert 状态，需要先做"启动时重放 JSONL"（建议另立任务）。
2. **fork 的上下文重建策略**：父会话若带 compaction / `injection_records`，复制策略需定；建议 V1 只复制 messages，其余清零。
3. **busy 语义**：OpenCode 拒绝 busy 上的 revert，我们照做并在 spec 中固化。
4. **改动会变更已交付的 T3 降级契约**：需要 openpaw/openwork 两侧变更记录 + 团队大纲同步，不能只改代码。
5. **规模估计**：Phase 1 约 250–400 行 Rust（protocol + runtime + http + 事件 + 测试）+ 约 40 行 spec 改动。

## 6. 待确认事项

- 是否立项（需要团队大纲/个人计划里的任务 ID 与范围确认）。
- Phase 1 是否接受"仅会话级撤销"，Phase 2（文件快照）另行评估。
- fork 的标题与 `parentID` 展示形式是否符合产品预期。
- 顺序：先补"启动时重放 JSONL"再做 revert 持久化，还是先做 Phase 1 的会话内撤销。

## 7. 更正：与 OpenCode 真实语义的差异（2026-09-20 追加）

本节**更正**第 4 节的若干设计，原文保留以便追溯。来源为 OpenCode 公开文档镜像与上游 PR（外部资料，仅作口径参考，落地前需与团队大纲/上游实现再核对）：

- 会话管理文档（含 `Session.Info`、Fork 流程、Snapshot 系统）：<https://raw.githubusercontent.com/bgauryy/open-docs/refs/heads/main/docs/opencode/03-session-management.md>
- `fix(core): commit staged revert before admitting new prompt`（PR #34338）：<https://github.com/anomalyco/opencode/pull/34338>
- 相关事故记录（消息 id 回绕导致 revert 删除历史）：<https://github.com/anomalyco/opencode/issues/43303>

### 7.1 【严重】revert 是"暂存"，下一条 prompt 会"提交"并真删消息

第 4.2 节写的"wire 层全部保留、只加游标隐藏"**是错的**。真实语义是：

1. `session.revert` 只是**暂存（staged）**：写入 `revert.messageID`，消息仍在。
2. 此时 UI 靠游标隐藏尾部，`unrevert` 可以撤销这次暂存（丢弃游标，什么都没变）。
3. **一旦接受新 prompt，暂存被提交（commit）**：`revert` 被清空，且边界之后的消息被**真正删除**。
   PR #34338 的测试断言了这两点：`session.get().revert` 变为 `undefined`，且那条 stale assistant 消息在数据库里查不到。

落地要求：`runtime.rs` 需要一个三态状态机 `none → staged → (commit | discard)`；提交发生在 `prompt` 入口（与 4.2 节"每次 prompt 先幂等应用游标"同一位置，但语义是**删除**而不是**跳过**）。这也意味着 `openpaw-store` 的 `SessionWriter` 目前只支持追加，需要支持"截断/重写"，否则磁盘上的 JSONL 会残留已提交撤销掉的记录。

### 7.2 `revert` 字段缺字段

真实形状（v1 `Session.Info`）：

```ts
revert: { messageID: string; partID?: string; snapshot?: string; diff?: string }
```

v2 事件里是 `revert: { messageID, files: [] }`。第 4.1 节只建了 `messageID` + `partID`，缺 `snapshot` / `diff` / `files`。

### 7.3 文件级撤销必须与消息级同批交付

OpenCode 有独立 Snapshot 系统：在 `write` / `edit` / `multiedit` / `patch` **执行前** `Snapshot.track()`，存储于 `~/.opencode/data/snapshot/<project-id>/`，撤销走 `Snapshot.restore(hash)`，另有 `diff(hash)` 与 `revert(patches)`。第 4.6 节把它降级为 Phase 2 需要重新评估：`revert` 契约里自带 `snapshot` 与 `diff`，说明**文件状态是 revert 的一等公民**，只做会话级撤销属于已知功能缺失，必须在验收口径里明确写出，或纳入本次范围。

接入点分布在 `openpaw-tools`（四个写工具前打点）与新增的快照子系统，不是 `openpaw-engine-host` 一个 crate 能完成的。

### 7.4 fork 的 id 策略写反了

第 4.3 节倾向"复用消息 id"。真实流程是：新建带 `parentID` 的子会话 → 复制边界之前的消息与 parts → **为所有内容生成全新 id** → 返回独立会话。标题也是 `"Child session - <timestamp>"`，不是第 4.3 节建议的 `"(branch)"` 后缀。

### 7.5 隐含前提：消息 id 必须字典序单调递增

前端用**字符串比较**判断消息是否在游标之后（`apps/app/src/react-app/domains/session/sync/actions-store.ts:746`：`id > revertMessageID`），游标定位也依赖 id 排序。上游出现过"消息 id 回绕导致新消息排在旧消息之前，进而 revert 误删历史"的真实事故（issue #43303）。因此落地前必须先核对并固化 openpaw 的消息 id 生成规则，否则 fork/revert 的边界会静默错乱。

### 7.6 openwork 侧的两条硬约束（第 5 节未展开）

1. **代理是通配的，无需改产品代码**：`apps/server/src/server.ts:914-927` 走 `parseWorkspaceOpencodeMount` 前缀匹配；`assertOpencodeProxyAllowed`（`server.ts:648-669`）只拦截 viewer 作用域的写操作与 viewer 审批回复，**没有路径白名单**；task-recovery 只接管 `/prompt_async`、`/prompt`、`/command`（`task-recovery.ts:233`）。新路由自动透传。
2. **fork 出来的会话必须带同样的 `directory`**：`assertWorkspaceOwnsProxiedSessionRead`（`server.ts:614-646`）对 GET `/session/:id` 会比对 `session.directory` 与 `workspace.directory` 的 realpath，不一致直接 404 `session_not_found`。否则前端一切到新分支就报错。
3. **v2 通道另有限制**：`apps/app/src/app/lib/opencode-v2-adapter.ts:1660-1661` 把 `session.revert` / `session.unrevert` 标为 `unsupportedResult`。走 v2 开关的会话即使引擎实现了也不生效，要打通必须改 openwork。

### 7.7 其余差异（较小）

- `Session.children(parentID)`（列子会话）OpenCode 有、openpaw 没有；需确认 OpenWork 是否调用。
- busy 语义真实是 `SessionLock` 抛 `LockedError`；第 4.4 节的 409 方向正确，但状态码与错误体要与 SDK 的错误识别对齐（前端文案 `session.revert_failed` 提示"等当前运行结束后重试"，说明它期望一种可识别的失败）。
- `partID`（撤销到消息内的某个片段）第 4.4 节只回存不实现，属 API 层不完全匹配（UI 目前只看 `messageID`）。
- compaction/summarize 与 revert 有交互（`summary: true` 的消息是 compaction point），本次排除，但属依赖项。
