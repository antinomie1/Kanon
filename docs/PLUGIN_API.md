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
| `MessagePipelineService` | 宿主 | 核心 | `OnPreFilter`、`OnExecuteCommand`、`OnCallTool`、`OnEvent`、`OnDeliverMessage`、`OnDecorateReply`、`OnPrepareTurn`、`OnLlmRequest` |
| `BotApiService` | 核心 | 宿主 | `RegisterHost`、`Ping`、`IngestEvent`、`SendMessage`、`ReplyMessage`、`RequestLLM`、`CallPlatformApi`、`GetConversationHistory`、`ListConversations`、`NewConversation`、`SwitchConversation`、`DeleteConversation`、`AppendConversation`、`ListPersonas`、`UpsertPersona`、`DeletePersona`、`SetStorage`、`GetStorage`、`DeleteStorage`、`ListStorage`、`RenderImage`、`RunAgent`、`RefreshPluginMeta` |

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
  → 会话接管（continuation）→ 斜杠命令 → 正则触发器 → 回复策略 → OnPrepareTurn → 模型（首个请求前 OnLlmRequest）
                                     ↘ 回复经 OnDecorateReply 后投递，成功后 OnEvent: message_sent

命令或触发器返回 pass_to_model 时，其回复照常投递，消息继续进入“回复策略 → 模型”。
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
| `pass_to_model` | `true` 时，`replies` 照常（先于模型回复）投递，随后消息继续走回复策略与模型，如同未命中任何命令或触发器。与 `capture_seconds` 同时设置时接管优先，本字段被忽略并记录告警 |
| `model_text` | 仅在 `pass_to_model` 时生效：以此文本替换消息中的文本段（图片等保留），模型读到的是改写后的文本；未设置则原样交给模型 |

命令与触发器都受命令权限策略约束：插件在 `CommandMeta.access` / `TriggerMeta.access` 中给出默认值，节点的 `command-policy` 可按名称覆盖。

命令与触发器还可用 `platforms` / `conversation_kinds` 限定作用域（空列表不限）。范围之外，该命令视同未声明——同名命令的其他声明者仍可胜出——触发器则不参与匹配。会话类型取自元数据 `kanon.conversation_kind`，缺省按私聊处理。

### `OnCallTool(ToolCallRequest) → ToolCallResponse`

| 字段 | 说明 |
| --- | --- |
| `structured_args` / `raw_bytes` | 双模载荷：JSON 参数走 `Struct`，二进制走 `bytes` |
| `context` | 触发本次调用的入站消息；控制台聊天等非平台会话中缺省 |
| `structured_result` / `raw_bytes` | 返回值 |
| `attachments` | 工具产出的富媒体，核心会附到出站消息上：按 `mime_type` 发送为图片、语音（`audio/*`）、视频或文件（其余类型），文件名即 `file_path` 的文件名；目标适配器不支持的类型会被替换为一行说明 |

`success = false` 时模型会看到一次失败的工具调用。

### `OnEvent(EventNotification) → EventAck`

只发送给在 `PluginMeta.events` 中订阅了该类型的插件。即发即忘：核心为每个订阅者单独派发，最多等待 **5 秒**，不关心结果。

| `detail` | `EventKind` | 内容 |
| --- | --- | --- |
| `message_sent` | `EVENT_KIND_MESSAGE_SENT` | 投递成功的 `DeliverMessageRequest` 与平台消息 ID |
| `notice` | `EVENT_KIND_NOTICE` | 平台通知原样的 `PipelineEventRequest`（类型见 `kanon.notice`） |
| `llm_response` | `EVENT_KIND_LLM_RESPONSE` | 被回答的消息与模型最终文本（装饰前） |
| `agent_begin` | `EVENT_KIND_AGENT_BEGIN` | 智能体开始回答某条消息：`context`、`session_id` |
| `agent_done` | `EVENT_KIND_AGENT_DONE` | 该轮结束：`success`、`content`（装饰前的最终回答）、`error`、`tools`（按调用顺序） |
| `tool_call` | `EVENT_KIND_TOOL_CALL` | 模型请求调用工具（工具执行前发出，仅供观察，不能否决）：`tool_name`、`arguments` |
| `tool_result` | `EVENT_KIND_TOOL_RESULT` | 工具调用结束：`tool_name`、`success`、`result`（模型读到的文本） |

智能体事件覆盖流水线回答的每一轮，以及 `RunAgent` 在会话内的运行；控制台聊天与插件的私有运行不发送。`agent_done.error` 是失败类别：`stopped`（被 `/stop` 中止）、`model_error`（模型服务出错）、`tool_error`（工具调用失败）。工具事件的 `tool_name` 是模型看到的名字（重名时为 `<plugin>__<tool>`）。同一轮的事件按发生顺序派发，但各订阅者独立接收，插件不应依赖跨事件的到达顺序。

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

### `OnPrepareTurn(PrepareTurnRequest) → PrepareTurnResult`

只调用 `PluginMeta.prepares_turns = true` 的插件，且仅当模型即将回答某条消息时（通知、未通过回复策略的消息不会触发）。用于检索知识库、长期记忆等按轮注入的上下文。

| 字段 | 说明 |
| --- | --- |
| `context` | 即将被回答的入站消息 |
| `session_id` | 模型将续写的会话（与 `GetConversationHistory` 返回的一致） |
| 结果 `text` | 置于本轮用户消息开头的文本；空字符串表示不注入 |

所有准备器**并发**执行，每个限时 **3 秒**；出错、超时或返回空串的不贡献内容。结果按宿主优先级、`host_id`、插件顺序拼接，插在召回提示之后、群聊记录与发送者标签之前。文本只进入当前轮用户消息，从不进入系统提示，因此不破坏请求前缀缓存；它随本轮消息一起写入会话历史。

### `OnLlmRequest(LlmRequestHookRequest) → LlmRequestHookResult`

只调用 `PluginMeta.rewrites_system_prompt = true` 的插件，用于改写会话的系统提示（人设之外的长期规则、按群定制的语气等）。

| 字段 | 说明 |
| --- | --- |
| `context` | 即将被回答的入站消息 |
| `session_id` | 模型将续写的会话 |
| `system_prompt` | 目前的系统提示：实例提示词或人设、技能目录，以及排在前面的插件的改写 |
| 结果 `system_prompt` | 设置时替换系统提示；不设置表示不改；空字符串被拒绝（保留原提示并记录日志） |

- **每轮一次。** 核心在一轮的首个模型请求前按宿主优先级、`host_id`、插件顺序**串行**询问，后者看到前者的结果；同一轮后续的工具轮次以及随后的历史压缩复用这次的结果，不再询问。
- **必须确定。** 系统提示位于每个请求的最前面，决定提供商的前缀缓存：对同一会话应返回相同文本，不要放入时间、计数器或按消息变化的内容（这些属于 `OnPrepareTurn`）。当运维修改人设或技能使原提示变化时，旧的改写自动作废并重新询问。
- **只在会话轮次中调用。** 控制台聊天、插件的 `RequestLLM` 与私有 `RunAgent` 不会触发，因此插件在钩子里调用模型不会递归回到自己。
- 每个插件限时 **3 秒**；出错或超时的插件不生效，下一个插件从未被改写的提示继续。会话历史中的摘要始终排在改写后的系统提示之后。

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
| `GetConversationHistory` | 只读获取某条入站消息所属的模型会话 | 见下 |
| `ListConversations` / `NewConversation` / `SwitchConversation` / `DeleteConversation` | 某条入站消息所属会话的多段对话：列出、新建、切换、删除，即内置 `/ls`、`/new`、`/switch`、`/del` | 见下 |
| `AppendConversation` | 把完整的用户/助手轮次追加到当前对话 | 见下 |
| `ListPersonas` / `UpsertPersona` / `DeletePersona` | 节点的人设目录 | 见下 |
| `SetStorage` / `GetStorage` / `DeleteStorage` / `ListStorage` | 核心的中心 KV 存储，每个插件一个命名空间 | 见下 |
| `RenderImage` | 把文本或 SVG 渲染成 PNG，供图片段发送 | 见下 |
| `RunAgent` | 让节点的智能体（模型 + 工具循环）回答一个提示，可在私有会话或某个聊天的当前对话中运行 | 见下 |
| `RefreshPluginMeta` | 插件在运行时增删工具、命令或触发器后，请核心重新读取本宿主的 `GetPluginMeta` | 见下 |

### 中心 KV：`SetStorage`、`GetStorage`、`DeleteStorage`、`ListStorage`

存放小状态（计数器、开关、令牌、按用户的设置），落盘于 `data/kv.db`，节点重启后仍在。大数据与需要查询的数据请写插件目录 `data/plugins/<id>/`。

| 字段 | 说明 |
| --- | --- |
| `plugin_id` | 命名空间，即插件 ID（SDK 自动填写）；只用于区分插件，不是访问控制 |
| `key` | 1–256 字节 |
| `value` | 不透明字节，至多 1 MiB（SDK 存 JSON） |
| `ttl_seconds` | `SetStorage`：`0` 为永不过期；正数为秒数，过期后读取、列举、删除都视为不存在；负数为 `INVALID_ARGUMENT`。再次 `SetStorage` 会同时替换值与过期时间 |
| `prefix` | `ListStorage`：按字面前缀筛选（`_`、`%` 不是通配符），空为全部；结果按键排序，只含未过期的键 |
| 结果 `found` / `deleted` | 键不存在或已过期时为 `false`，不是错误 |

错误：插件 ID 或键不合法为 `INVALID_ARGUMENT`；值超过 1 MiB 为 `RESOURCE_EXHAUSTED`；核心未提供 KV 为 `UNAVAILABLE`；数据库故障为 `INTERNAL`。

### `RenderImage(RenderImageRequest) → RenderImageResponse`

把文本排成卡片，或把 SVG 原样渲染为 PNG（resvg，纯 Rust，节点不需要浏览器），写入插件自己的目录 `data/plugins/<plugin_id>/render/`，返回绝对路径，可直接作为 `ImageSegment.file_path` 发送。

| 字段 | 说明 |
| --- | --- |
| `text` | 纯文本卡片：按 `width` 自动换行（英文按词、中文按字），空行分段，以 `# ` 开头的行是标题；最多 20,000 字 |
| `svg` | 完整的 SVG 文档，按其自身尺寸渲染，未绘制处透明；`<image>` 只接受内嵌的 `data:` URI，指向节点文件的引用被忽略 |
| `width` | 仅文本：卡片宽度，默认 720，范围 200–2000 |
| 结果 | `file_path`、`width`、`height` |

字体取自节点系统：`sans-serif` 优先选用已安装的中文无衬线字体（如 Noto Sans CJK），缺字的字符（中文、emoji）自动从其他已安装字体中补齐。相同内容渲染为同一个文件；超过 24 小时的渲染结果在下次渲染时清理，所以应在渲染后尽快发送。错误：参数不合法、SVG 无法解析、图片超过 1600 万像素为 `INVALID_ARGUMENT`；节点未安装任何字体时渲染文本为 `FAILED_PRECONDITION`。

### `RunAgent(RunAgentRequest) → RunAgentResponse`

与 `RequestLLM` 不同，`RunAgent` 运行的是节点的智能体：它可以调用工具（插件、MCP 与内置工具），也可以在某个聊天的对话里作为一轮回答。

| 字段 | 说明 |
| --- | --- |
| `prompt` | 给智能体的用户消息；仅当提供了 `images` 时可以为空 |
| `images` | 本轮的图片：`url`、`file_path` 或 `raw_bytes`（须带 `image/*` 的 `mime_type`，至多 10 MiB）；所用模型必须声明视觉能力 |
| `context` | 本次运行服务的聊天（一条入站消息）：决定实例，从而决定默认模型、可用插件与工具策略，并作为工具调用的 `context` |
| `in_conversation` | `true`：在该聊天的当前对话中运行，带着对话历史与人设回答，并追加到历史中，与模型回答该消息完全一样（订阅者收到智能体与工具事件，`OnLlmRequest` 参与，`/stop` 可中止）。`false`（默认）：使用一次性的私有会话，结束即丢弃，不调用任何插件钩子 |
| `system_prompt` | 私有运行的指令，空则沿用节点的基础人设；在对话中运行时忽略 |
| `model` | `<provider>/<model-id>`；空为实例的模型，否则为节点默认模型 |
| `use_tools` | 是否向模型提供工具；为 `false` 时只做纯文本回答 |
| `max_steps` | 允许的工具轮次，`0` 为智能体默认值 |
| 结果 | `content`（去掉推理内容的最终回答）、`attachments`（工具产出的富媒体，由插件自行发送）、`tools`（按顺序调用过的工具）、`session_id`（对话的会话，或已丢弃的私有会话 `plugin:<plugin_id>:<n>`） |

运行**不会**把回答发送到聊天，发送由插件决定。运行从不执行 Bash：聊天由插件持有的消息指定，插件可以伪造的发送者身份不能用于解锁 shell。

| 条件 | 状态码 |
| --- | --- |
| 提示与图片都为空；`in_conversation` 缺 `context`；图片不合法或模型不支持图片；模型引用不合法 | `INVALID_ARGUMENT` |
| `context` 所在的聊天没有启用的实例认领 | `NOT_FOUND` |
| 未配置模型 | `UNAVAILABLE` |
| 对话正有一轮在运行（`in_conversation`，不排队） | `FAILED_PRECONDITION` |
| 运行被 `/stop` 中止 | `ABORTED` |
| 模型或工具失败 | `UNAVAILABLE` |

### `RefreshPluginMeta(RefreshPluginMetaRequest) → RefreshPluginMetaResponse`

插件在运行时增删了工具（`add_tool` / `remove_tool`）、命令或触发器后调用，核心立即重新调用该宿主的 `GetPluginMeta`，下一轮起生效（正在进行的轮次不受影响）。结果 `plugin_ids` 为核心现在持有其元数据的插件。

| 条件 | 状态码 |
| --- | --- |
| 未知的 `host_id` | `NOT_FOUND` |
| 新的元数据缺少宿主此前声明过的插件（核心保留旧元数据） | `FAILED_PRECONDITION` |
| `GetPluginMeta` 本身失败 | 原样返回其状态码 |

### `RequestLLM(LLMRequest) → stream LLMChunk`

| 字段 | 说明 |
| --- | --- |
| `model` | 空为节点默认模型；否则必须为 `<provider>/<model-id>` |
| `system_prompt` | 可选系统提示 |
| `messages` | 至少一条，时间正序。`role` 不能为 `LLM_ROLE_UNSPECIFIED`；`images` 只允许出现在用户消息中，可以是 `url`、`file_path` 或 `raw_bytes`（须带 `image/*` 的 `mime_type`，至多 10 MiB） |
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

### `GetConversationHistory(ConversationHistoryRequest) → ConversationHistoryResponse`

按与模型阶段完全相同的规则（实例解析、会话共享策略、会话键）定位 `context` 所属会话，返回模型回答它时将续写的历史。

| 字段 | 说明 |
| --- | --- |
| `context` | 入站事件，必填 |
| `limit` | 只保留最近的若干条消息；`0` 为全部 |
| 结果 `session_id` | 会话 ID（`/new`、`/switch`、`/del` 之前保持不变） |
| 结果 `summary` | 已压缩的早期轮次摘要；从未压缩为空 |
| 结果 `messages` | `HistoryMessage { role, text }`，时间正序；只含用户与助手轮次，工具调用、工具结果与推理内容均被剔除 |

已有消息只读，插件无法改写其中某条，只能追加完整轮次（`AppendConversation`）或删除整段对话（`DeleteConversation`）。错误：缺 `context` 为 `INVALID_ARGUMENT`；没有实例接管该平台为 `NOT_FOUND`；未配置模型或核心未就绪为 `UNAVAILABLE`；会话存储读取失败为 `INTERNAL`。

### 对话管理：`ListConversations`、`NewConversation`、`SwitchConversation`、`DeleteConversation`

一个会话（私聊、群成员，或共享会话的整个群）可以有多段对话，与内置 `/ls`、`/new`、`/switch`、`/del` 是同一份实现。会话由 `context`（该会话的一条入站消息）按与 `GetConversationHistory` 相同的规则定位，插件与模型永远指向同一组对话。四个 RPC 都返回操作**之后**的 `ConversationList`。

| 字段 | 说明 |
| --- | --- |
| `context` | 入站事件，必填 |
| `session_id` | `SwitchConversation` / `DeleteConversation`：目标对话的 `ConversationInfo.session_id` |
| 结果 `conversations` | 按创建先后排列（`/ls` 的序号即下标 + 1）；当前对话即使没有消息也在列 |
| `ConversationInfo.current` | 该会话下一条消息是否续写这段对话 |
| `ConversationInfo.title` | 第一条用户消息（去掉核心添加的 `[发送者: …]` 等前缀，截到 24 字）；没有消息时为空 |
| `ConversationInfo.message_count` | 保存的用户与助手消息数（已被压缩进摘要的不计） |
| `ConversationInfo.last_active_at` | 最后一轮的 Unix 秒；从未有过轮次为 0 |

- `NewConversation` 开一段空对话并设为当前，代数取已有最大代数 + 1，不会落进已有的对话。
- `DeleteConversation` 删除历史、摘要与会话记录；删除的是当前对话时，会话随即换到一段新的空对话。
- 插件代替用户调用这些 RPC 时，应自行检查权限：内置 `/switch`、`/del` 在群里默认仅管理员可用。

错误：缺 `context` 为 `INVALID_ARGUMENT`；没有实例接管该平台、`session_id` 不属于该会话为 `NOT_FOUND`；未配置模型为 `UNAVAILABLE`；对话正被运行中的轮次写入为 `FAILED_PRECONDITION`（稍后重试，或先 `/stop`）；存储或实例目录写入失败为 `INTERNAL`。

### `AppendConversation(AppendConversationRequest) → AppendConversationResponse`

把在别处发生的轮次（插件自己的问答、导入的记录）写入 `context` 所属会话的**当前**对话，模型下一轮把它们当作这段对话自己的历史。

| 字段 | 说明 |
| --- | --- |
| `context` | 入站事件，必填 |
| `messages` | `HistoryMessage { role, text }`，至少一对；必须严格按“用户、助手”交替，以用户开始、以助手结束，保证历史始终由完整轮次组成 |
| 结果 `session_id` | 写入的会话 |

只追加、不改写：已有消息保持不变，前缀缓存不受影响。每一对计为一轮。错误：消息为空、角色不是用户/助手或不交替为 `INVALID_ARGUMENT`；该对话正在运行轮次为 `FAILED_PRECONDITION`——外部写入从不排队等待运行中的轮次，所以插件工具在轮次内调用它会立即得到这个错误而不会死锁；其余同上。

### 人设：`ListPersonas`、`UpsertPersona`、`DeletePersona`

与控制台「人设」页共用同一份目录与 `data/personas.json`，所有修改都按“校验 → 落盘 → 生效”在一把锁下进行，控制台与插件的修改不会互相覆盖。

| RPC | 说明 |
| --- | --- |
| `ListPersonas` | 全部人设：基础助手（`builtin = true`）、运营者的人设，以及由实例自带提示词生成的人设 |
| `UpsertPersona(Persona)` | 新建或替换运营者人设；`name` 为空时取 `id`；替换时保留控制台填写的描述。结果 `replaced` 表示是否替换了已有人设 |
| `DeletePersona` | 删除运营者人设；不存在时返回 `deleted = false` 而不是错误。使用它的会话退回基础助手 |

错误：`id` 不合法、`name` 或 `prompt` 为空为 `INVALID_ARGUMENT`；修改基础助手或实例生成的人设、删除仍被某个实例选用的人设为 `FAILED_PRECONDITION`；`personas.json` 写入失败为 `INTERNAL`（目录保持不变）；核心未提供人设目录为 `UNAVAILABLE`。

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
| `commands` | `CommandMeta`：`name`、`description`、`usage`、`priority`、`aliases`、`access`、`platforms`、`conversation_kinds` |
| `triggers` | `TriggerMeta`：`name`、`description`（空则不在 `/help` 列出）、`pattern`（Rust `regex` 语法）、`priority`、`access`、`platforms`、`conversation_kinds` |
| `tools` | `ToolMeta`：`name`、`description`、`parameters`（JSON Schema） |
| `events` | 订阅的 `EventKind` |
| `decorates_replies` | 是否参与回复装饰 |
| `prepares_turns` | 是否参与轮次准备（`OnPrepareTurn`） |
| `rewrites_system_prompt` | 是否参与系统提示改写（`OnLlmRequest`） |

命令与触发器共享名称空间；同名命令按 `CommandMeta.priority`、再按宿主优先级决出唯一胜者。内置命令 `help`、`info`、`new`、`model` 不可被覆盖。

### 6.5 枚举

| 枚举 | 取值 |
| --- | --- |
| `CommandAccess` | `COMMAND_ACCESS_EVERYONE`、`COMMAND_ACCESS_ADMINS_IN_GROUPS`、`COMMAND_ACCESS_ADMINS` |
| `EventKind` | `EVENT_KIND_UNSPECIFIED`、`EVENT_KIND_MESSAGE_SENT`、`EVENT_KIND_NOTICE`、`EVENT_KIND_LLM_RESPONSE`、`EVENT_KIND_AGENT_BEGIN`、`EVENT_KIND_AGENT_DONE`、`EVENT_KIND_TOOL_CALL`、`EVENT_KIND_TOOL_RESULT` |
| `ReplySource` | `REPLY_SOURCE_UNSPECIFIED`、`REPLY_SOURCE_LLM`、`REPLY_SOURCE_COMMAND` |
| `LLMRole` | `LLM_ROLE_UNSPECIFIED`、`LLM_ROLE_USER`、`LLM_ROLE_ASSISTANT` |
| `ConversationKind` | `CONVERSATION_KIND_UNSPECIFIED`、`CONVERSATION_KIND_PRIVATE`、`CONVERSATION_KIND_GROUP`、`CONVERSATION_KIND_CHANNEL` |

---

## 7. 时限与限额

| 项目 | 值 |
| --- | --- |
| 前置过滤链总预算 | 30ms（单插件 5ms 告警） |
| 会话接管 `capture_seconds` 上限 | 600 秒 |
| `OnEvent` 单订阅者等待 | 5 秒 |
| `OnDecorateReply` 单装饰器 | 3 秒 |
| `OnPrepareTurn` 单准备器 | 3 秒（并发执行） |
| `OnLlmRequest` 单插件 | 3 秒（串行，每轮一次） |
| `RequestLLM` / `RunAgent` 图片 `raw_bytes` | 每张至多 10 MiB |
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
