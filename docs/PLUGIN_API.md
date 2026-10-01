# Kanon 插件协议参考 (Plugin API Reference)

本文是插件与核心之间 gRPC 契约的参考手册，适合编写 SDK、调试协议或不使用官方 SDK 直接接入的读者。
日常开发请先读 [PLUGIN_GUIDE.md](./PLUGIN_GUIDE.md)。

**唯一事实来源是 [`proto/kanon/v1/plugin.proto`](../proto/kanon/v1/plugin.proto)**，本文解释其语义、时限与错误约定；两者冲突时以 proto 为准。

---

## 1. 传输与连接

| 项目 | 约定 |
| --- | --- |
| 协议 | gRPC over HTTP/2，包名 `kanon.plugin.v1` |
| 核心端点 | 核心监听 `core.sock`（位于运行时目录，如 `./run/` 或 `$XDG_RUNTIME_DIR/kanon/run/`） |
| 宿主端点 | 每个宿主监听自己专属的 `host_<id>.sock`，路径由 Supervisor 指定 |
| Windows | 改用 Loopback TCP；首个 HTTP/2 HEADERS 必须携带 32 字节随机令牌 `x-kanon-auth-token`，核心以恒定时间比较 |
| HTTP/2 authority | 走 UDS 时 authority 必须是合法主机名（官方 SDK 固定为 `localhost`）；Python 请用 `kanon_sdk.ipc.connect_core_channel()` 建链 |

Supervisor 启动宿主时注入的**启动契约**（插件不得把其他环境变量当作配置来源）：

| 变量 | 含义 |
| --- | --- |
| `KANON_HOST_ID` | 宿主 ID，`RegisterHost` 时回报 |
| `KANON_HOST_SOCK` | 宿主应监听的端点 |
| `KANON_CORE_SOCK` | 核心端点 |
| `KANON_IPC_TOKEN` | Windows Loopback 鉴权令牌 |

启动顺序：宿主**先**绑定并开始服务自己的端点，**再**调用 `RegisterHost`——核心收到注册后可能立刻回连。

---

## 2. 服务总览

| 服务 | 运行在 | 调用方 | RPC |
| --- | --- | --- | --- |
| `PluginHostService` | 宿主 | 核心 | `Ping`、`ReloadPluginConfig`、`GetPluginMeta`、`InvokeAction` |
| `MessagePipelineService` | 宿主 | 核心 | `OnPreFilter`、`OnExecuteCommand`、`OnCallTool`、`OnEvent`、`OnDeliverMessage`、`OnDecorateReply` |
| `BotApiService` | 核心 | 宿主 | `RegisterHost`、`Ping`、`IngestEvent`、`SendMessage`、`ReplyMessage`、`RequestLLM`、`CallPlatformApi`、`SetStorage`、`GetStorage` |

---

## 3. PluginHostService（核心 → 宿主）

### `Ping(PingRequest) → PingResponse`
存活探测，原样回显 `timestamp`。Supervisor 据此判断宿主是否卡死。

### `GetPluginMeta(GetPluginMetaRequest) → GetPluginMetaResponse`
握手时调用，返回宿主内每个插件的 `PluginMeta`。核心的命令路由、触发器、工具目录、事件订阅与装饰器列表**全部**以此为准（`plugin.toml` 中的 `[[commands]]`/`[[tools]]` 仅用于控制台离线展示）。

### `ReloadPluginConfig(ReloadPluginConfigRequest) → ReloadPluginConfigResponse`
推送新配置。`version` 为单调递增的 CAS 版本：`version <= 当前版本` 必须拒绝（`success = false`，`applied_version` 为当前版本）。插件拒绝配置时返回 `success = false` 与原因，核心不会持久化该配置。

### `InvokeAction(PluginActionRequest) → PluginActionResponse`
控制台调用的管理动作（`POST /api/v1/plugins/{id}/actions/{action}`），从不暴露给模型。失败以 `success = false` + `error_message` 表达，`result` 为 JSON 对象。

---

## 4. MessagePipelineService（核心 → 宿主）

核心对每条入站消息的处理顺序：

```
实例闸门 → 通知事件（OnEvent: notice）→ OnPreFilter 链 → 内置命令
  → 会话接管（continuation）→ 斜杠命令 → 正则触发器 → 回复策略 → 模型
                                     ↘ 回复经 OnDecorateReply 后投递，成功后 OnEvent: message_sent
```

### `OnPreFilter(PipelineEventRequest) → PreFilterResult`

| `action` | 含义 |
| --- | --- |
| `PASS` | 放行 |
| `BLOCK` | 终止处理；`reply_messages` 非空时作为回复发出 |
| `MODIFY` | 以 `modified_text` 替换文本后继续 |

按插件 `priority` 升序串行调用。整条链总预算 **30ms**，单插件超过 5ms 告警，预算耗尽后其余过滤器被跳过、消息放行。

### `OnExecuteCommand(CommandExecuteRequest) → CommandExecuteResponse`

斜杠命令、正则触发器与会话续接共用此 RPC。

| 请求字段 | 说明 |
| --- | --- |
| `command` | 正式命令名（绝不是别名），或触发器名 |
| `args` | 命令：按空白切分，引号内保持整体；触发器：捕获组 1..n，未参与匹配为 `""`；续接：整条消息文本 |
| `raw_args` | 命令名之后的原文；续接时为整条消息文本（去掉开头的 @） |
| `context` | 入站事件 |
| `continuation` | 本消息是对上一次 `capture_seconds` 的续接 |

| 响应字段 | 说明 |
| --- | --- |
| `success` / `error_message` | 失败只记入核心日志，不发给用户；`replies` 仍会发出 |
| `replies` | 作为**一条**平台消息发出（先经过回复装饰） |
| `capture_seconds` | 非 0 时，同一平台 + `channel_id` + `sender_id` 的**下一条**消息在该秒数内（最多 600）跳过命令、触发器与模型，以 `continuation = true` 回到本插件的同一 `command`。每次接管只覆盖一条消息；前置过滤仍先执行；同一会话的新接管覆盖旧接管。`sender_id` 为空的事件无法接管 |

命令与触发器都受命令权限策略约束：插件在 `CommandMeta.access` / `TriggerMeta.access` 中给出默认值，节点的 `command-policy` 可按名称覆盖。

### `OnCallTool(ToolCallRequest) → ToolCallResponse`

| 字段 | 说明 |
| --- | --- |
| `structured_args` / `raw_bytes` | 双模载荷：JSON 参数走 `Struct`，二进制走 `bytes` |
| `context` | 触发本次调用的入站消息；控制台聊天等非平台会话中缺省 |
| `structured_result` / `raw_bytes` | 返回值 |
| `attachments` | 工具产出的富媒体（图片、文件），核心会附到出站消息上 |

`success = false` 时模型会看到一次失败的工具调用。

### `OnEvent(EventNotification) → EventAck`

只发送给在 `PluginMeta.events` 中订阅了该类型的插件。即发即忘：核心为每个订阅者单独派发，最多等待 **5 秒**，不关心结果。

| `detail` | `EventKind` | 内容 |
| --- | --- | --- |
| `message_sent` | `EVENT_KIND_MESSAGE_SENT` | 投递成功的 `DeliverMessageRequest` 与平台消息 ID |
| `notice` | `EVENT_KIND_NOTICE` | 平台通知原样的 `PipelineEventRequest`（类型见 `kanon.notice`） |
| `llm_response` | `EVENT_KIND_LLM_RESPONSE` | 被回答的消息与模型最终文本（装饰前） |

字段号 1、2 已保留（旧版扁平字段），不得复用。

### `OnDecorateReply(DecorateReplyRequest) → DecorateReplyResult`

只调用 `PluginMeta.decorates_replies = true` 的插件，按宿主优先级**串行**执行，后者看到前者的结果。

| 字段 | 说明 |
| --- | --- |
| `context` | 被回答的入站消息 |
| `segments` | 待发送的回复 |
| `source` | `REPLY_SOURCE_LLM` 或 `REPLY_SOURCE_COMMAND` |
| `command` | `source` 为命令时的命令/触发器名 |
| 结果 `modified` | `false`：保留原回复；`true`：以 `segments` 替换，空列表表示不发送 |

每个装饰器限时 **3 秒**；RPC 出错或超时一律保留原回复。装饰不影响会话记忆。

### `OnDeliverMessage(DeliverMessageRequest) → DeliverMessageResponse`

仅发给在清单中声明了 `[adapter] platform` 的插件，`platform` 与之匹配。实现必须如实报告：未发送就返回 `success = false`，绝不“假成功”。核心对每个平台维护熔断器与死信队列。

---

## 5. BotApiService（宿主 → 核心）

| RPC | 语义 | 主要错误 |
| --- | --- | --- |
| `RegisterHost` | 宿主就绪后登记 `host_id`、运行时、端点与插件 ID | `UNAVAILABLE`：核心未配置 Supervisor |
| `Ping` | 宿主侧的核心存活探测；连续失败后宿主应自行退出 | — |
| `IngestEvent` | 适配器推入入站事件，Fast-ACK（不等待处理） | 队列满时**不报错**，返回 `accepted = false`；核心关闭时 `UNAVAILABLE` |
| `SendMessage` | 主动发送；成功仅表示进入出站队列 | `UNAVAILABLE`：出站调度器未就绪或已关闭 |
| `ReplyMessage` | 回复某条入站事件，**等待平台投递结果**（最长 30 秒） | `INVALID_ARGUMENT`：缺 `event_id`/`platform`/`channel_id`；`RESOURCE_EXHAUSTED`：出站队列满；`DEADLINE_EXCEEDED`：投递结果未知，**禁止自动重试** |
| `RequestLLM` | 独立模型调用，流式返回 `LLMChunk` | `UNAVAILABLE`：未配置模型；`INVALID_ARGUMENT`：消息不合法；`INTERNAL`：上游错误 |
| `CallPlatformApi` | 调用内置适配器的平台原生 API | 见下 |
| `SetStorage` / `GetStorage` | 不提供中心化 KV | 恒为 `UNIMPLEMENTED`，请写本地 `data/plugins/<id>/` |

### `RequestLLM(LLMRequest) → stream LLMChunk`

| 字段 | 说明 |
| --- | --- |
| `model` | 空为节点默认模型；否则必须为 `<provider>/<model-id>` |
| `system_prompt` | 可选系统提示 |
| `messages` | 至少一条，时间正序。`role` 不能为 `LLM_ROLE_UNSPECIFIED`；`images` 只允许出现在用户消息中，且只接受 `url` / `file_path`（`raw_bytes` 返回 `INVALID_ARGUMENT`） |
| `temperature` / `max_tokens` | `optional`：不设置即沿用提供商默认值 |

调用与任何会话无关：不读写会话记忆，也不经过人设与工具。字段号 1、3（旧 `prompt`、`parameters`）已保留。

### `CallPlatformApi(PlatformApiRequest) → PlatformApiResponse`

| 条件 | 状态码 |
| --- | --- |
| `action` 含 `[A-Za-z0-9_.]` 以外的字符 | `INVALID_ARGUMENT` |
| 目标平台由插件适配器提供 | `UNIMPLEMENTED` |
| 未知平台 | `NOT_FOUND` |
| 内置适配器未实现平台 API | `UNIMPLEMENTED` |
| 平台拒绝或调用失败 | `UNAVAILABLE` |

`params` 为 JSON 对象，`result` 为任意 JSON 值（`google.protobuf.Value`）。数字一律为 double。

---

## 6. 数据类型

### 6.1 `PipelineEventRequest`

| 字段 | 说明 |
| --- | --- |
| `event_id` | 平台限定的消息 ID，用作追踪键、去重键与引用回复目标 |
| `platform` | 平台标识，与适配器 `[adapter] platform` 一致 |
| `channel_id` | 会话标识（如 `group:123`），对插件不透明 |
| `sender_id` | 发送者平台 ID |
| `raw_text` | 纯文本 |
| `segments` | 富媒体段；模型看到的内容由它构造 |
| `metadata` | `Struct`，平台无关键见下表 |

### 6.2 元数据键

| 键 | 类型 | 含义 |
| --- | --- | --- |
| `kanon.conversation_kind` | string | `private` / `group` / `channel` |
| `kanon.bot_mentioned` | bool | 是否 @ 了机器人 |
| `kanon.sender_name` | string | 发送者昵称或群名片 |
| `kanon.sender_role` | string | `owner` / `admin` / `member` |
| `kanon.timestamp` | number | Unix 秒 |
| `kanon.timestamp_text` | string | 适配器格式化的时间，优先于上者 |
| `kanon.notice` | string | 通知类型：`member_join`、`bot_join`、`friend_add`、`poke`、`recall`、`friend_request`、`group_invite` |
| `kanon.notice_actor` | string | 引发通知者的名称或 ID |
| `kanon.notice_target` | string | 被撤回消息的 `event_id` |
| `kanon.request_token` | string | 适配器用于应答好友申请/群邀请的私有句柄 |

### 6.3 `MessageSegment`

| 字段号 | 段 | 方向 | 说明 |
| --- | --- | --- | --- |
| 1 | `text` | 收/发 | |
| 2 | `image` | 收/发 | `url` / `file_path` / `raw_bytes` 三选一，可选 `mime_type`、`filename` |
| 3 | `audio` | 收/发 | 同上，可选 `duration_seconds` |
| 4 | `mention` | 收/发 | `target_user_id`、`display_name`、`is_all` |
| 5 | `reply` | 收/发 | `target_message_id` 为被引用消息的 `event_id` |
| 6 | `custom` | 收/发 | `type_name` + `Struct` 载荷，适配器私有（如 `milky.video`） |
| 7 | `video` | 发 | 来源同图片 |
| 8 | `file` | 发 | 来源同图片，`name` 必填 |
| 9 | `face` | 发 | 平台表情 ID |

入站的视频、文件与表情由内置适配器表示为 `custom` 段（如 `milky.video`）。`file_path` 是节点本机路径，由适配器读取。平台不支持某种段时，投递明确失败，而不是静默丢弃。

### 6.4 `PluginMeta`

| 字段 | 说明 |
| --- | --- |
| `id` / `name` / `version` / `author` / `description` | 身份信息，`id` 必须与清单一致 |
| `commands` | `CommandMeta`：`name`、`description`、`usage`、`priority`、`aliases`、`access` |
| `triggers` | `TriggerMeta`：`name`、`description`（空则不在 `/help` 列出）、`pattern`（Rust `regex` 语法）、`priority`、`access` |
| `tools` | `ToolMeta`：`name`、`description`、`parameters`（JSON Schema） |
| `events` | 订阅的 `EventKind` |
| `decorates_replies` | 是否参与回复装饰 |

命令与触发器共享名称空间；同名命令按 `CommandMeta.priority`、再按宿主优先级决出唯一胜者。内置命令 `help`、`info`、`new`、`model` 不可被覆盖。

### 6.5 枚举

| 枚举 | 取值 |
| --- | --- |
| `CommandAccess` | `COMMAND_ACCESS_EVERYONE`、`COMMAND_ACCESS_ADMINS_IN_GROUPS`、`COMMAND_ACCESS_ADMINS` |
| `EventKind` | `EVENT_KIND_UNSPECIFIED`、`EVENT_KIND_MESSAGE_SENT`、`EVENT_KIND_NOTICE`、`EVENT_KIND_LLM_RESPONSE` |
| `ReplySource` | `REPLY_SOURCE_UNSPECIFIED`、`REPLY_SOURCE_LLM`、`REPLY_SOURCE_COMMAND` |
| `LLMRole` | `LLM_ROLE_UNSPECIFIED`、`LLM_ROLE_USER`、`LLM_ROLE_ASSISTANT` |

---

## 7. 时限与限额

| 项目 | 值 |
| --- | --- |
| 前置过滤链总预算 | 30ms（单插件 5ms 告警） |
| 会话接管 `capture_seconds` 上限 | 600 秒 |
| `OnEvent` 单订阅者等待 | 5 秒 |
| `OnDecorateReply` 单装饰器 | 3 秒 |
| `ReplyMessage` 等待投递结果 | 30 秒（SDK 客户端截止时间 35 秒） |
| 宿主启动就绪等待 | 5 秒 |
| 宿主停止宽限 | 3 秒 |
| 宿主看门狗间隔 | 5 秒；崩溃重启退避最长 30 秒 |

---

## 8. 错误约定

- **业务失败用值表达，传输失败用状态码表达。** 命令、工具、动作、投递失败都通过 `success = false` + `error_message` 返回；`IngestEvent` 的背压通过 `accepted = false` 表达。
- **结果不确定时不重试。** `ReplyMessage` 的 `DEADLINE_EXCEEDED` 与传输错误都意味着消息可能已发出。`ReplyMessage` 与普通命令回复共用同一个有界出站 FIFO 与平台熔断器；调用方取消后仍在排队的请求会被跳过，但已进入平台 I/O 的发送无法撤回。
- **不做隐式回退。** 未配置模型、平台不支持、运行时缺失都会明确报错，核心不会伪造成功结果。

---

## 9. 兼容性

- 删除的字段一律 `reserved` 字段号与名称，不得复用。
- 新增字段必须对旧客户端无害：未设置时的默认值须等于“旧行为”（如 `capture_seconds = 0`、`continuation = false`、空的 `events`）。
- 修改 proto 后需同步：Rust 由 `kanon-proto` 构建时生成；Python 需重新生成 `kanon_sdk/proto/plugin_pb2*.py`；TypeScript 运行时动态加载 IDL，无需生成。
