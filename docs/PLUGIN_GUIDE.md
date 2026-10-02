# Kanon 插件开发指南

本文面向插件作者：如何用 Rust / Python / TypeScript 编写、调试和发布 Kanon 插件。
协议层的字段、错误码与时限见 [PLUGIN_API.md](./PLUGIN_API.md)；整体架构见 [ARCHITECTURE.md](./ARCHITECTURE.md)。

---

## 1. 插件是什么

插件是一个独立的**宿主进程**，由节点 `kanon` 的 Supervisor 按需拉起，通过 gRPC（Unix 域套接字，Windows 上为带令牌的 Loopback TCP）与核心通信。插件崩溃、卡死或内存泄漏都不会拖垮节点，核心会熔断并按退避策略重启它。

三种语言的 SDK 提供同一套能力，只是写法按语言习惯不同：

| 能力 | 适用场景 | Python | TypeScript | Rust |
| --- | --- | --- | --- | --- |
| 斜杠命令 | `/weather 北京` | `@command` | `@Command` | `.command(CommandSpec, ..)` |
| 命令组 | `/todo add 买牛奶`、`/todo list` | `@command("todo add")` | `@Command("todo add")` | `.command_group(..)` |
| 正则触发器 | 不带斜杠的关键词、`早安` | `@trigger` | `@Trigger` | `.trigger(TriggerSpec, ..)` |
| 多轮对话 | 问答、确认、小游戏 | `event.wait_next()` | `event.waitNext()` | `event.wait_next(..)` |
| LLM 工具 | 让模型调用你的函数 | `@tool`（由签名推断 Schema） | `@Tool(name, { args })`（`s` 构造 Schema） | `.tool(ToolSpec::typed::<Args>(..), ..)`（由结构体推断 Schema） |
| 运行时增删工具 | 按配置或登录状态开放工具 | `add_tool` / `remove_tool` | `addTool` / `removeTool` | `context.add_tool` / `remove_tool` |
| 改写系统提示 | 按群定制的规则、长期设定 | `@on_llm_request` | `@OnLlmRequest()` | `.rewrite_system_prompt(..)` |
| 管理动作 | 控制台按钮：扫码登录、诊断 | `@action` | `@Action` | `.action(name, ..)` |
| 事件订阅 | 入群欢迎、发送审计、回答统计 | `@on_event` | `@OnEvent` | `.subscribe(EventKind, ..)` |
| 回复装饰 | 统一签名、敏感词替换 | `@decorate_reply` | `@DecorateReply()` | `.decorate_reply(..)` |
| 前置过滤 | 黑名单、刷屏拦截 | 覆盖 `on_pre_filter` | 覆盖 `onPreFilter` | `.pre_filter(..)` |
| 主动发消息 | 定时提醒、订阅推送 | `core.send_message` | `core.sendMessage` | `core.send_message` |
| 独立调用模型 | 摘要、翻译、分类 | `core.request_llm` | `core.requestLlm` | `core.request_llm` |
| 平台原生 API | 禁言、取群成员列表 | `core.call_platform_api` | `core.callPlatformApi` | `core.call_platform_api` |
| 中心 KV | 计数器、开关、按用户的设置 | `self.kv` | `this.kv` | `core.kv_get` / `kv_set` |
| 调用智能体 | 让模型带着工具完成一件事 | `core.run_agent` | `core.runAgent` | `event.agent(..)` / `core.agent(..)` |
| 对话与人设 | 列出 / 切换 / 导入对话，维护人设 | `core.list_conversations` 等 | `core.listConversations` 等 | `core.list_conversations` 等 |
| 渲染图片 | 排行榜、卡片、图表 | `core.render_text` / `render_svg` | `core.renderText` / `renderSvg` | `core.render_text` / `render_svg` |
| HTTP 路由 | Webhook、给控制台页面的接口 | `@http_route` | `@HttpRoute` | `.http_route(..)` |
| 平台适配器 | 接入新的聊天平台 | `[adapter]` + `on_deliver_message` | 同左 | 同左 |

---

## 2. 快速开始

### 2.1 创建与校验

```bash
kanon-dev create my_plugin --lang python      # 或 rust / typescript
kanon-dev lint ./my_plugin                     # 静态校验 plugin.toml
kanon-dev test ./my_plugin                     # 离线沙盒：在终端里和插件对话
kanon-dev dev ./my_plugin                      # 热重载：改动后在运行中的节点上重启插件
kanon-dev pack ./my_plugin                     # 打包为 .kpk（附 SHA-256 校验）
```

`kanon-dev pack` 只打包节点运行插件所需的文件：`plugin.toml`、代码（Rust 插件先 `cargo build --release`，二进制放在 `entrypoint` 指向的路径，不含源码；Python/TS 插件为脚本与 `pyproject.toml`/`uv.lock`、`package.json`/锁文件）以及 `pages/`、`i18n/`。测试、文档和其他文件不会进包；打包后会逐个列出包内文件。

**沙盒 `kanon-dev test`** 在进程内启动一个最小节点：真实的流水线、内置命令（`/ls`、`/new`、`/switch`、`/del` 等）、钩子、中心 KV 与会话存储（均在临时目录），模型换成本地 mock，不需要聊天平台和 API Key。Rust 插件会先 `cargo build`，Python/TS 插件会先安装依赖。

- 交互模式：直接输入消息或 `/命令`；`!tool <工具名> [JSON 参数]` 让 mock 模型调用工具（走完整的工具轮次）；`:call <工具名> [JSON]` 直接调用工具；`:prompt` 打印上一次发给模型的请求（可检查系统块改写与工具列表）；`:quit` 退出。
- 脚本模式：`-m/--message` 可重复，按顺序发送后退出，适合写进 CI；`-c /hello`、`-t <工具> -a '<JSON>'` 只执行一个命令或工具，失败时退出码非零。

**热重载 `kanon-dev dev`** 连接已在运行的节点（默认 `http://127.0.0.1:8080`，用 `--node` 指定），插件目录有改动时先构建（Rust），再通过 `POST /api/v1/plugins/{id}/restart` 重启插件宿主；宿主会重新读取 `plugin.toml`、按需重装依赖并重新注册命令、工具与钩子。构建失败时保留正在运行的版本；插件崩溃后，修好代码保存即可重新启动。节点必须已经加载该插件：把插件目录（或指向它的符号链接）放进节点的 `./plugins/`。Rust 插件若位于其他 Cargo 工作区内，构建产物不在 `entrypoint` 指向的位置，`dev` 会报错而不是用旧二进制重启——给插件加独立的 `[workspace]` 即可。

把插件目录放进节点的 `./plugins/` 下（最多嵌套两层子目录），节点启动时扫描 `plugin.toml` 并按 `priority` 排序加载。节点运行期间不会自动扫描该目录：新放进去的插件要在控制台「扩展 → 插件」点「刷新」（即 `POST /api/v1/plugins/rescan`）后才会出现，再从那里启用；通过控制台安装的插件则会立即出现。

### 2.2 依赖安装

只需在插件目录里声明依赖，**节点启动插件前会自动安装**：环境不存在，或比依赖声明旧（改了 `pyproject.toml` / `package.json` / 锁文件）时，在插件目录调用该语言自己的工具。每个插件有自己的环境，绝不回退到共享或系统解释器：

| 语言 | 依赖声明 | 安装位置 | 自动执行 | 失败时 |
| --- | --- | --- | --- | --- |
| Python | `pyproject.toml`（建议附 `uv.lock`） | `<plugin>/.venv` | `uv sync`（有 `uv.lock` 时 `uv sync --locked`） | `RuntimeUnavailable`，附工具输出末尾 |
| TypeScript | `package.json` + 锁文件 | `<plugin>/node_modules` | `bun.lock` → `bun install --frozen-lockfile`；`package-lock.json` → `npm ci`；无锁文件 → `bun install` 或 `npm install` | 同上 |
| Rust | `Cargo.toml` | 编译产物 | 不自动构建，需 `cargo build --release` | 入口文件不存在即启动失败 |

节点所在机器需要装有 `uv`（Python 插件）或 `bun` / `npm`（TypeScript 插件）。安装最长 10 分钟；失败的原因显示在控制台的插件状态里，下次启动会重试。运维可在 `data/system.json` 设置 `"startup": { "install_dependencies": false }` 关闭自动安装，改为自己在插件目录执行上述命令。

Python 插件以 `<plugin>/.venv` 中的解释器运行 `python -m kanon_host.main`，因此 `kanon-sdk` 必须是插件自己的依赖。TypeScript 插件优先使用 `node_modules/@kanon/sdk-and-host` 里的宿主脚本，运行时优先 `bun`，其次 `node`。

### 2.3 最小插件

**Python**

```python
from kanon_sdk import CommandEvent, Plugin, command


class HelloPlugin(Plugin):
    id = "org.example.hello"
    name = "Hello"
    version = "0.1.0"

    @command("hello", description="Says hello", usage="/hello [name]")
    async def hello(self, event: CommandEvent) -> str:
        return f"Hello, {event.raw_args or event.sender_name or 'world'}!"
```

**TypeScript**

```typescript
import { Command, CommandEvent, Plugin } from "@kanon/sdk-and-host";

export default class HelloPlugin extends Plugin {
  id = "org.example.hello";
  name = "Hello";
  version = "0.1.0";

  @Command("hello", { description: "Says hello", usage: "/hello [name]" })
  async hello(event: CommandEvent) {
    return `Hello, ${event.rawArgs || event.senderName || "world"}!`;
  }
}
```

**Rust**

```rust
use kanon_sdk::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let plugin = Router::new("org.example.hello", "Hello", "0.1.0").command(
        CommandSpec::new("hello").description("Says hello").usage("/hello [name]"),
        |event| async move {
            let name = match event.raw_args() {
                "" => event.sender_name().to_string(),
                given => given.to_string(),
            };
            Ok(format!("Hello, {name}!"))
        },
    );
    KanonHost::new(plugin).run().await?;
    Ok(())
}
```

Rust 插件也可以不用 `Router`，直接为自己的类型实现 `Plugin` trait（`meta`、`on_execute_command` 等），以获得完全的控制。

---

## 3. 插件清单 `plugin.toml`

```toml
[plugin]
id = "org.example.weather"        # 全局唯一，建议反向域名
name = "天气"
version = "1.0.0"
author = "Example"
description = "查询天气并向模型提供天气工具"
runtime = "python"                # rust | python | typescript
entrypoint = "main.py"            # Rust 为可执行文件路径，如 target/release/weather
isolated = false
priority = 100                    # 1..1000，越小越先执行，默认 500
kanon_version = ">=0.1, <0.3"     # 可选：适用的节点版本（semver 要求）
platforms = ["qq", "telegram"]    # 可选：面向的平台，仅用于展示；省略表示全部
homepage = "https://example.com/weather"
repository = "https://github.com/example/kanon-weather"  # 控制台可据此从 Git 安装

[config_schema]                   # 控制台据此渲染配置表单（JSON Schema）
type = "object"
required = ["api_key"]
properties = { api_key = { type = "string", title = "API Key" } }

[[commands]]                      # 仅供控制台在插件未启动时展示
name = "weather"
description = "查询天气"
usage = "/weather <城市>"

[[tools]]                         # 同上
name = "fetch_weather"
description = "获取城市实时天气"
parameters = { type = "object", properties = { city = { type = "string" } }, required = ["city"] }

[adapter]                         # 可选：声明后本插件成为该平台的适配器
platform = "my_im"
display_name = "My IM"
capabilities = ["sender_name", "quote_reply", "send_image", "send_file"]
```

要点：

- **运行时以代码为准**。命令、触发器、工具、事件订阅都由插件在握手时通过 `GetPluginMeta` 上报（即装饰器/`Router` 中的声明）。`[[commands]]` / `[[tools]]` 只用于插件未运行时的控制台展示，两处应保持一致。
- **未知小节直接报错**，包括旧的 `[dependencies]`：依赖只写在 `pyproject.toml` / `package.json` / `Cargo.toml` 里。
- `priority` 同时决定前置过滤链顺序、同名命令的胜出者与回复装饰的执行顺序。
- `kanon_version` 在安装和启动时校验：节点版本不满足要求时插件不会启动，控制台显示“需要 Kanon …，当前节点为 …”；写错的要求（不是合法 semver）同样报错而不是被忽略。省略即任何节点都可运行。
- `[adapter]` 必须写在静态清单而不是代码里：核心要在收到第一条消息前就知道平台归哪个宿主。

---

## 4. 生命周期与上下文

```
Supervisor 拉起进程 → 宿主绑定专属 socket → 连接 core.sock 并 RegisterHost
  → 读取 data/plugins/<id>/config.json → 设置 context → on_load(ctx)
  → 正常服务（命令 / 工具 / 事件 …） → on_unload → 退出
```

`PluginContext`（Python/TS 中为 `plugin.context`，Rust `Router` 中为 `router.context()`）包含：

| 字段 | 说明 |
| --- | --- |
| `data_dir` / `dataDir` | 插件专属可写目录 `./data/plugins/<id>/`。持久化数据（SQLite、DuckDB、文件）都放这里。 |
| `config` | 操作员在控制台保存的配置（与 `[config_schema]` 对应）。 |
| `core` | 回调核心的句柄；独立运行（无核心）时为 `None`/`undefined`。 |

- **配置热更新**：操作员保存配置后，核心调用 `ReloadPluginConfig`。宿主先替换 `context.config`，再调用 `on_config_reload(config)`；在其中抛出异常即拒绝本次更新，核心不会持久化被拒绝的配置。版本号单调递增，过期的更新会被宿主拒绝。
- **核心存活看门狗**：宿主定期探测核心；核心消失后宿主自行退出，避免出现继续占用平台连接的“幽灵机器人”。关闭的每一步都有超时上限。
- **中心 KV**：小状态（计数器、开关、令牌、按用户的设置）存核心的 KV（`self.kv`，见 [8.5](#85-中心-kv)），大数据与需要查询的数据写 `data_dir`。

---

## 5. 处理消息

### 5.1 事件对象

处理函数收到的是事件对象，不是原始 protobuf 请求：

| 含义 | Python | TypeScript | Rust |
| --- | --- | --- | --- |
| 消息 ID（可用于引用回复） | `event.event_id` | `event.eventId` | `event.event_id()` |
| 平台 / 会话 / 发送者 | `platform` / `channel_id` / `sender_id` | `platform` / `channelId` / `senderId` | 同名方法 |
| 纯文本 | `event.text` | `event.text` | `event.text()` |
| 消息段 | `event.segments` | `event.segments` | `event.segments()` |
| 图片 | `event.images` | `event.images` | `event.images()` |
| 发送者昵称 / 群身份 | `sender_name` / `sender_role` | `senderName` / `senderRole` | 同名方法 |
| 是否群聊 / 是否 @ 了机器人 | `is_group` / `bot_mentioned` | `isGroup` / `botMentioned` | 同名方法 |
| 通知类型（普通消息为空） | `event.notice` | `event.notice` | `event.notice()` |
| 全部元数据 | `event.metadata`（dict） | `event.metadata` | `event.metadata(key)` |
| 原始请求 | `event.raw` | `event.raw` | `event.raw()` |

命令与触发器的处理函数收到的是 `CommandEvent`，它在 `MessageEvent` 的基础上增加了 `command`、`args`、`raw_args`、`continuation`；Rust 中 `CommandEvent` 解引用为 `MessageEvent`。

`channel_id` 形如 `group:123`、`private:456`，由适配器决定，插件应把它当作不透明字符串原样传回。

### 5.2 斜杠命令

```python
@command("roll", aliases=("r",), usage="/roll [faces]", priority=100, access="everyone")
async def roll(self, event: CommandEvent, args: list) -> str: ...
```

- **解析**：消息以 `/` 开头才算命令；参数按空白切分，引号（`"a b"`、`'a b'`、`“a b”`）内的文本保持为一个参数，`raw_args` 是命令名之后未切分的原文。
- **别名**：路由时与正式名同等对待；处理函数收到的 `command` 永远是正式名。
- **同名冲突**：多个插件声明同名命令时，命令 `priority` 小者胜出，其次比较插件 `priority`。
- **内置命令优先**：`/help`、`/info` 始终由核心处理；`/new`、`/ls`、`/switch`、`/del`、`/model`、`/stop` 在有实例的会话中由核心处理。插件无法覆盖。
- **作用域**：`platforms=("onebot",)`、`conversation_kinds=("group",)` 把命令限定在指定平台或会话类型（`private`、`group`、`channel`），留空即不限。范围之外该命令视同未声明——其他插件的同名命令仍可胜出。TS 为 `{ platforms, conversationKinds }`，Rust 为 `CommandSpec::new(..).platform("onebot").conversation_kind(ConversationKind::Group)`。
- **权限**：`access` 只是插件给出的默认值——`everyone`、`admins_in_groups`（私聊任何人可用，群内仅管理员）、`admins`。操作员可以在控制台或 `PUT /api/v1/system/command-policy` 中按命令名覆盖；管理员名单格式为 `<platform>:<user id>`，默认群主与群管理员也视为管理员。

**命令组**：名称中带空格即声明子命令，一组命令共用一个命令名：

```python
@command("todo add", description="Add a todo", usage="/todo add <text>")
async def add(self, event: CommandEvent) -> str:
    # "/todo add buy milk": event.args == ["buy", "milk"], event.raw_args == "buy milk"
    ...

@command("todo list", description="List todos")
async def list_todos(self) -> str: ...
```

- 核心只认识组名 `todo`：路由、同名冲突、权限与作用域都按组名计算；`/help` 在组名下逐行列出子命令。
- 子命令处理函数收到的 `args` / `raw_args` 已去掉子命令本身。
- 只发 `/todo`，或子命令不存在时，SDK 回复子命令列表；如果同时声明了 `@command("todo")`，则由它处理这两种情况，组的描述、别名、`access`、作用域与优先级也写在它上面。子命令只能设置 `description` 与 `usage`，写其他参数会在加载时报错。
- TS 写法相同：`@Command("todo add", { description, usage })`，处理函数收到的 `event.args` / `event.rawArgs` 同样已去掉子命令。
- Rust 用 `command_group` 一次声明整个组，组的 `CommandSpec` 即上面的“根命令”，`event.subcommand()` 给出子命令名：

```rust
router.command_group(CommandSpec::new("todo").description("待办"), |group| {
    group
        .command(CommandSpec::new("add").usage("/todo add <内容>"), |event| async move {
            Ok(format!("已添加：{}", event.raw_args()))
        })
        .command("list", |event| async move { Ok("…") })
})
```

### 5.3 正则触发器

```python
@trigger(r"^(\d+)d(\d+)$", description="掷骰子，如 2d6")
async def dice(self, event: CommandEvent) -> str:
    count, faces = map(int, event.args)   # args are the capture groups
    ...
```

- 核心用 Rust `regex` 语法匹配消息纯文本，捕获组 1..n 作为 `args`（未参与匹配的组为空字符串）。
- 触发器在斜杠命令之后、模型之前执行；多个触发器同时匹配时 `priority` 小者胜出。
- 未锚定的模式会匹配消息中的任意位置，通常应写成 `^...$`。
- 无效的正则不会匹配任何消息，并在核心日志中告警；Python/TS SDK 在声明时会先用本语言的正则引擎做一次校验。
- 命令与触发器共享名称空间（核心都通过 `OnExecuteCommand` 下发），同名会在加载时报错。
- 触发器同样支持 `platforms` / `conversation_kinds` 作用域；范围之外不参与匹配。

### 5.4 回复

处理函数有三种回复方式：

1. **返回值**：文本、单个消息段，或二者混合的列表。返回 `None`/`undefined`/`Ok(())` 表示不回复。
2. **`await event.reply(...)`**：可多次调用。核心正在等待本处理函数时，回复会被收集，并在处理函数结束或调用 `wait_next` 时作为**一条**平台消息发出。
3. **`await event.send(...)`**：立即单独发送并等待平台的投递结果，适合长任务中的进度提示（“正在生成…”）。

还可以返回完整的 `CommandExecuteResponse`（Python/TS），这时其中的 `success`、`error_message`、`capture_seconds`、`pass_to_model`、`model_text` 会被沿用。

处理函数抛出异常时，命令以失败告终：已收集的回复照常发出，错误信息只记入核心日志，不会发给用户。

### 5.5 多轮对话：`wait_next`

```python
@command("guess")
async def guess(self, event: CommandEvent) -> None:
    secret = random.randint(1, 10)
    await event.reply("我想了一个 1~10 的数，猜猜看？")
    while True:
        try:
            answer = await event.wait_next(timeout=60)
        except asyncio.TimeoutError:
            await event.reply(f"超时了，答案是 {secret}")   # delivered on its own
            return
        if answer.text.strip() == str(secret):
            return "猜对了！"
        await answer.reply("不对，再试试")
```

语义：

- `wait_next` 先把目前收集的回复发出，然后让核心**接管**该会话：同一平台、同一 `channel_id`、同一发送者的下一条消息跳过命令、触发器和模型，直接交回给这个处理函数（前置过滤仍然先执行）。
- 返回的新事件 `continuation` 为真，其 `args`/`raw_args` 是整条消息文本（已去掉开头的 @）。
- 超时上限 600 秒；超时后抛出 `asyncio.TimeoutError`（Python）、`WaitTimeoutError`（TS），或返回 `Err(WaitTimeout)`（Rust）。超时后的 `reply` 会作为单独的消息发出。
- 同一会话中再次进入的 `wait_next`（例如用户重新发了 `/guess`）会取代旧的等待，旧的那个立即收到超时。
- 每次接管只覆盖**一条**消息；想继续对话就再调用一次 `wait_next`。
- 宿主重启时挂起的处理函数会丢失；之后到达的续接消息会以 `continuation = true` 重新调用该命令的处理函数，处理函数可据此给出提示。

原理：核心逐条串行处理消息，绝不阻塞等待插件。SDK 把处理函数放在独立任务里运行，当前 RPC 只等到处理函数的下一个“让出点”（结束或 `wait_next`）就返回，并在响应中携带 `capture_seconds`；续接消息到达时 SDK 再唤醒挂起的处理函数。不使用 SDK 时，也可以直接在 `CommandExecuteResponse` 中设置 `capture_seconds`，自己处理 `continuation`。

### 5.6 交给模型：`pass_to_model`

命令或触发器处理完后，可以让这条消息继续交给模型回答，就像它没有命中任何命令一样：

```python
@command("remember")
async def remember(self, event: CommandEvent, args: list) -> None:
    save_note(event.sender_id, event.raw_args)
    await event.reply("已记下")
    event.pass_to_model(f"我刚让你记住：{event.raw_args}，请简短确认。")
```

| 语言 | 原样交给模型 | 改写文本后交给模型 |
| --- | --- | --- |
| Python | `event.pass_to_model()` | `event.pass_to_model(text)` |
| TypeScript | `event.passToModel()` | `event.passToModel(text)` |
| Rust | `event.pass_to_model()?` | `event.pass_to_model_as(text)?` |

- 本轮收集的回复照常先发出，随后消息走回复策略与模型；回复策略可能决定不回答（例如群聊未被 @）。
- 改写只替换消息里的文本，图片等其他段保留；模型和会话历史看到的都是改写后的文本。
- 与 `wait_next` 互斥：同一轮中之后调用了 `wait_next` 时，接管优先，交给模型被取消。
- 必须在核心仍在等待处理函数时调用；`wait_next` 超时之后再调用会报错。

### 5.7 前置过滤

前置过滤在所有命令与模型之前按插件 `priority` 依次执行，可以放行（`PASS`）、拦截（`BLOCK`，可附带回复）或改写文本（`MODIFY`）。整条链的总预算只有 **30ms**，单个插件超过 5ms 会被告警，超出预算的后续过滤器会被跳过——这里不要做网络请求。

---

## 6. LLM 工具与管理动作

最简单的写法是让 SDK 从函数签名与文档字符串推断工具：

```python
@tool
async def fetch_weather(self, city: str, event: MessageEvent, days: int = 1) -> dict:
    """获取城市的实时天气与预报。

    Args:
        city: 城市名，如“北京”。
        days: 预报天数，1–7。
    """
    # event is the message the model was answering, or None (e.g. the console chat)
    return {"city": city, "temp": 25}
```

- **工具名**为函数名（或 `@tool("name")`），**描述**为文档字符串第一段，**参数说明**取自 `Args:` 小节或 `Annotated[str, "说明"]`。
- 支持的类型：`str`、`int`、`float`、`bool`、`Literal[...]`（枚举）、`list[T]`、`dict[str, T]`、`Optional[T]`、`Union[...]`、`Any`。有默认值或为 `Optional` 的参数不是必填；默认值会写进 Schema。没有类型标注、`*args`、`Enum` 等无法描述的参数在**加载时**报错，而不是让模型去猜。
- 模型的参数以关键字参数传入，多余的参数会作为失败的工具调用告诉模型；声明为 `int` 的参数收到的就是 `int`（protobuf 只有双精度数，SDK 按 Schema 还原）。
- 名为 `event` 的参数不属于模型：它是模型正在回答的那条消息（控制台聊天中为 `None`）。工具据此知道“是谁在问”，不必让模型把用户 ID 当参数传进来——模型可以伪造参数，但伪造不了这个上下文。

需要完全控制 Schema 时显式给出 `parameters`，处理函数改为接收参数字典：

```python
@tool("fetch_weather", "获取城市实时天气",
      {"type": "object", "properties": {"city": {"type": "string"}}, "required": ["city"]})
async def fetch_weather(self, params: dict, event) -> dict:
    return {"city": params["city"], "temp": 25}
```

- 返回字典即为结构化结果；返回其他 JSON 值（数字、字符串、列表）会被包装为 `{"result": ...}`；返回 `bytes` 走二进制通道。
- 抛出异常时，模型会得到一次失败的工具调用（含异常信息），可以自行解释或重试。
- 工具名与参数 Schema 会进入模型请求前缀。核心对工具按名称排序、对 Schema 键排序以保证前缀缓存稳定；不要随消息变化改变工具声明。

**TypeScript**：类型在运行时不存在，参数用 `s` 描述，处理函数收到参数对象与消息：

```typescript
@Tool("fetch_weather", {
  description: "获取城市的实时天气与预报。",
  args: {
    city: s.string("城市名，如“北京”"),
    days: s.integer("预报天数，1–7").default(1),
    unit: s.enum(["c", "f"]).optional(),
  },
})
async fetchWeather({ city, days }: { city: string; days: number }, event?: MessageEvent) {
  return { city, temp: 25 };
}
```

- 构造器：`s.string`、`s.number`、`s.integer`、`s.boolean`、`s.enum([...])`、`s.array(item)`、`s.object({...})`、`s.any()`；每个都可接说明文字，并可链式调用 `.optional()`、`.default(value)`、`.describe(text)`。
- 参数默认必填。SDK 在调用前补上默认值，并把未知参数、缺少的必填参数作为失败的工具调用告诉模型；不重复校验类型。
- 仍可用 `parameters` 传入手写的 JSON Schema（与 `args` 二选一），此时参数原样传入。

**Rust**：参数是一个 `#[derive(Deserialize, JsonSchema)]` 结构体，Schema 由它生成，字段的文档注释即参数说明；插件需依赖 `serde`（`derive` 特性）与 `schemars = "1"`：

```rust
/// fetch_weather 的参数。
#[derive(Deserialize, JsonSchema)]
struct WeatherArgs {
    /// 城市名，如“北京”。
    city: String,
    /// 预报天数，1–7（默认 1）。
    days: Option<u32>,
}

router.tool(
    ToolSpec::typed::<WeatherArgs>("fetch_weather").description("获取城市的实时天气与预报。"),
    |args, event| async move { Ok(json!({ "city": args.city, "temp": 25 })) },
)
```

- `Option<T>` 与 `#[serde(default)]` 字段不是必填；枚举、嵌套结构体、`Vec`、`HashMap` 都会展开成提供商普遍接受的简单 Schema。
- 参数无法解析为该结构体（缺字段、类型不符）时，处理函数不会运行，模型收到指明问题的失败调用；整数字段可以正常接收（SDK 会把 protobuf 的双精度整数还原）。
- `ToolSpec::new(name).parameters(json!({..}))` 手写 Schema，处理函数收到 `serde_json::Value`。返回值为任意可序列化的值，非对象包装为 `{"result": ...}`。

**运行时增删工具**：工具集合需要随配置或状态变化时（例如登录后才开放的工具），用 `add_tool` / `remove_tool`：

```python
async def on_config_reload(self, config: dict) -> None:
    if config.get("enable_search"):
        await self.add_tool(self.search)          # schema inferred like @tool
    else:
        await self.remove_tool("search")
```

- TS：`await this.addTool("search", { description, args }, async ({ query }) => ...)`、`await this.removeTool("search")`；用 `@Tool` 声明的工具不能移除。
- Rust：`context.add_tool(ToolSpec::typed::<SearchArgs>("search"), handler).await?`、`context.remove_tool("search").await?`，其中 `context` 为 `router.context()`；参数与 `.tool(..)` 相同。
- 两者都会请核心重新读取插件元数据（`RefreshPluginMeta`），**下一轮**起生效，正在进行的轮次不受影响。核心拒绝时抛出异常，SDK 回滚本地改动，两边始终一致。
- 每次增删都会改变请求前缀，使提供商的前缀缓存失效一次，只在状态真正变化时调用。

**管理动作**（`@action`）是工具在控制台一侧的对应物：只能由 `POST /api/v1/plugins/{id}/actions/{action}` 调用，永远不会出现在模型的函数列表里。凭证绑定、扫码登录、诊断等操作必须用动作而不是工具，否则模型可能在对话中途尝试调用它们。动作返回 JSON 对象。

---

## 7. 事件订阅与回复装饰

### 7.1 事件订阅

```python
@on_event("notice")
async def welcome(self, event: MessageEvent) -> None:
    if event.notice == "member_join":
        await event.send([MessageSegment.mention(event.sender_id), " 欢迎入群！"])
```

| 事件 | 处理函数收到 | 时机 |
| --- | --- | --- |
| `message_sent` | `MessageSentEvent`（投递请求 + 平台消息 ID） | 机器人的每条消息投递成功后 |
| `notice` | `MessageEvent`（`notice` 为类型） | 平台通知到达时，无论机器人是否对其作出反应 |
| `llm_response` | `LlmResponseEvent`（被回答的消息 + 模型最终文本） | 模型回答之后、回复装饰之前 |
| `agent_begin` / `agent_done` | `AgentBeginEvent` / `AgentDoneEvent`（成功与否、最终回答、错误类别、调用过的工具） | 智能体开始 / 结束回答一轮 |
| `tool_call` / `tool_result` | `ToolCallEvent`（工具名、参数）/ `ToolResultEvent`（成功与否、模型读到的结果） | 每次工具调用之前 / 之后；只能观察，不能否决 |

通知类型包括 `member_join`、`bot_join`、`friend_add`、`poke`、`recall`、`friend_request`、`group_invite`，具体取决于适配器支持哪些。

事件是**通知**：核心不等待结果，每个订阅者最多 5 秒，失败只记日志。只有声明了订阅的插件才会收到对应事件。智能体与工具事件覆盖聊天中的每一轮（包括 `run_agent(in_conversation=True)`），适合做统计、审计或“正在查询…”之类的提示；Python 与 TypeScript 中它们是原始 protobuf 消息（TS 为 snake_case 字段的普通对象），`context` 字段即对应的聊天消息；Rust 中为 `Event::AgentDone(AgentDone)` 等强类型结构，`event` 字段即对应的 `MessageEvent`（控制台聊天中为 `None`）。

### 7.2 回复装饰

```python
@decorate_reply
async def sign(self, reply: Reply):
    if reply.source != "llm":
        return None                              # leave command replies alone
    return list(reply.segments) + [MessageSegment.text("\n— Kanon")]
```

- 适用于模型回复和命令/触发器回复（`reply.source` 为 `llm` 或 `command`；后者带 `reply.command`）。
- 返回 `None` 表示不修改；返回新内容即替换；返回空列表则**不发送**该回复。
- 多个装饰器按插件 `priority` 依次执行，后者看到前者的结果。
- 每个装饰器限时 3 秒，失败或超时则保留原回复。装饰器位于回复路径上，务必保持轻量。
- 装饰只影响发出的消息，**不会**改变会话记忆中模型“说过的话”。
- 每个插件只能有一个装饰器。

---

### 7.3 轮次准备：注入上下文

模型即将回答一条消息时，准备器可以返回一段文本，置于本轮用户消息的开头——适合检索知识库、读取长期记忆：

```python
@prepare_turn
async def recall(self, event: MessageEvent, session_id: str):
    facts = self.memory.search(event.sender_id, event.text)
    return "已知信息：\n" + "\n".join(facts) if facts else None
```

TS 用 `@PrepareTurn()` 装饰方法 `(event, sessionId) => string | undefined`；Rust 用 `router.prepare_turn(|event, session_id| async move { Ok(text) })`。

- 只在模型真正要回答时调用；通知、命令、未通过回复策略的消息都不会触发。
- 所有插件的准备器**并发**执行，每个限时 3 秒；出错或超时就当没有返回。返回 `None`/`undefined`/空串表示不注入。
- 文本只进入当前轮用户消息，绝不进入系统提示，因此不会破坏请求前缀缓存；它会随本轮消息写入会话历史，后续轮次模型仍能看到。所以只注入与本轮相关、篇幅可控的内容。
- 每个插件只能有一个准备器。

### 7.4 改写系统提示

需要改变模型的**长期设定**（按群定制的规则、额外的人设约束）时，改写系统提示：

```python
@on_llm_request
async def chat_rules(self, event: MessageEvent, system_prompt: str, session_id: str):
    rules = await self.kv.get(f"rules:{event.channel_id}")
    return f"{system_prompt}\n\n本群规则：\n{rules}" if rules else None
```

- TS 用 `@OnLlmRequest()` 装饰方法 `(event, systemPrompt, sessionId) => string | undefined`；Rust 用 `router.rewrite_system_prompt(|prompt| async move { .. })`，`prompt` 含 `event`、`session_id` 与 `prompt`，返回 `Ok(Some(新提示))` 或 `Ok(None)`。
- 每轮一次，在该轮首个模型请求前调用；返回新的系统提示即替换，返回 `None` 表示不改。多个插件按优先级串行，后者看到前者的结果。参数可以只取前几个（`event`、`system_prompt`、`session_id`）。
- **结果必须确定**：系统提示位于每个请求的最前面，同一会话应返回相同文本。时间、计数器、检索结果等按消息变化的内容放进 `@prepare_turn`，否则每一轮都会使前缀缓存失效。
- 只在会话轮次中调用；控制台聊天、`request_llm` 与私有 `run_agent` 不会触发。限时 3 秒，出错或超时则跳过该插件。
- 每个插件只能有一个改写器。

---

## 8. 调用核心

以下方法都在 `core` 句柄上（Python/TS：`self.core` 或 `event.core`；Rust：`event.core()?` 或 `router.context().core()?`），独立运行时不可用。Rust 的这些方法返回 `Result<_, CoreError>`，错误按 gRPC 状态码分为 `InvalidArgument`、`NotFound`、`Unavailable`、`DeadlineExceeded` 等变体，可直接 `match`；独立运行时为 `CoreError::Standalone`。

### 8.1 发送消息

| 方法 | 语义 |
| --- | --- |
| `event.send(content)` / `core.reply_to(event, content)` | 回复某条入站消息，**等待平台投递结果**。超时或 RPC 失败时消息可能已发出，切勿自动重试。 |
| `core.send_message(platform, channel_id, content)` | 主动发送（提醒、推送）。成功只表示进入核心的出站队列，不代表平台已送达。 |

`content` 可以是文本、消息段或二者的列表。

### 8.2 调用模型

```python
summary = await self.core.request_llm(
    messages=[llm_message(history_text), llm_message("请总结以上内容", role="user")],
    system_prompt="你是一个简洁的摘要助手。",
    temperature=0.2,
    max_tokens=300,
)

async for delta in self.core.stream_llm("讲个笑话"):
    ...
```

- 与任何会话完全独立：不读取也不写入会话记忆，不经过人设与技能目录。
- `model` 留空即使用节点的唯一默认模型；指定时必须写成 `<provider>/<model-id>`。
- `prompt` 与 `messages` 二选一。多轮消息按时间从早到晚排列；图片只能放在用户消息中，可用 URL、文件路径或原始字节（`MessageSegment.image_bytes(data, "image/png")`，必须注明 `image/*` 的 MIME 类型，单张至多 10 MiB）。
- 节点未配置模型时返回 `UNAVAILABLE`，消息不合法时返回 `INVALID_ARGUMENT`。

### 8.3 平台原生 API

```python
members = await self.core.call_platform_api("onebot", "get_group_member_list", group_id=123)
```

- 只对声明了 `platform_api` 能力的**内置**适配器（OneBot、Milky）可用；其他平台返回 `UNIMPLEMENTED`，未知平台返回 `NOT_FOUND`，平台拒绝时返回 `UNAVAILABLE`。
- 动作名只允许字母、数字、`_` 和 `.`。参数与结果都是普通 JSON。protobuf 只有双精度数：参数中的整数会以整数交给平台，而 Python 收到的结果里数字是 `float`（如 `42.0`），需要时自行 `int()`。
- 这是“逃生舱”：用了它，插件就绑定在特定平台上了。能用通用能力完成的事不要走这里。

### 8.4 读取会话历史

```python
history = await self.core.conversation_history(event, limit=20)
for role, text in history.messages:      # role is "user" or "assistant"
    ...
```

- 返回模型回答 `event` 时将续写的那个会话：`session_id`、`summary`（早期轮次被压缩后的摘要）与按时间正序的 `messages`。`limit` 只保留最近若干条，`0` 为全部。
- 只含用户与助手的文本；工具调用、工具结果和模型的推理过程都被剔除。
- **只读**：插件无法修改或删除会话历史。
- TS：`await this.core.conversationHistory(event, 20)`，`messages` 为 `{ role, text }`；Rust：`core.conversation_history(&event, 20).await?` 返回原始 `ConversationHistoryResponse`。
- 没有实例接管该平台时返回 `NOT_FOUND`，节点未配置模型时返回 `UNAVAILABLE`。

### 8.5 中心 KV

```python
count = await self.kv.get(f"visits:{event.sender_id}", 0)
await self.kv.set(f"visits:{event.sender_id}", count + 1)
await self.kv.set("captcha:123", {"answer": 7}, ttl=300)   # expires after 5 minutes
keys = await self.kv.keys("visits:")
await self.kv.delete("captcha:123")
```

- TS：`await this.kv.get(key, fallback)`、`await this.kv.set(key, value, { ttl: 300 })`、`delete`、`keys`。
- Rust：`core.kv_get::<T>(key).await?` 返回 `Option<T>`（存储的值无法解析为 `T` 时是 `CoreError::Json`，而不是 `None`）、`kv_set(key, &value)`、`kv_set_with_ttl(key, &value, Duration::from_secs(300))`、`kv_delete(key)`、`kv_keys(prefix)`。
- 值是任意 JSON 可序列化的对象，落盘于节点的 `data/kv.db`，重启后仍在；每个插件一个命名空间，互不可见。各语言 SDK 都以 UTF-8 JSON 存储，彼此可读。
- `get` 在键不存在或已过期时返回默认值；`ttl` 为秒数，省略即永不过期。单个值至多 1 MiB，超出时在本地就报错。
- 适合小状态。大数据或需要查询的数据请在 `data_dir` 中使用 SQLite 等本地存储。

### 8.6 调用智能体

`request_llm` 只是一次模型调用；`run_agent` 运行的是节点的**智能体**——模型加工具循环，可以调用插件、MCP 与内置工具：

```python
result = await self.core.run_agent("查一下明天北京的天气，适合爬山吗？", event=event)
await event.reply(result.content)

# Answer as a turn of this chat's conversation: history, persona and hooks apply,
# and the turn is appended to the history.
result = await self.core.run_agent(event.text, event=event, in_conversation=True)
```

- TS：`await this.core.runAgent(prompt, { event, inConversation, images, systemPrompt, model, useTools, maxSteps })`。
- Rust 为构建器：`event.agent(prompt).use_tools().await?`（即 `core.agent(prompt).event(&event)`），另有 `.in_conversation()`、`.images(event.images())`、`.system_prompt(..)`、`.model(..)`、`.max_steps(..)`。结果 `AgentReply` 可直接作为命令处理函数的返回值：文本之后附上工具产出的附件。
- 结果为 `content`（最终回答）、`attachments`（工具产出的图片等，需要自己发送）、`tools`（按顺序调用过的工具）与 `session_id`。**不会**自动发送到聊天。
- `event` 决定由哪个实例回答（模型、可用插件与工具策略），并作为工具调用的上下文；`in_conversation=True` 时必填。
- 私有运行（默认）使用一次性会话，可通过 `system_prompt`、`model`、`use_tools`、`max_steps` 调整；在对话中运行时这些设定以会话为准。
- 对话正有一轮在运行时，`in_conversation=True` 立即失败（`FAILED_PRECONDITION`），不会排队。
- 内置智能体是节点的默认实现；将来可以替换为其他智能体框架，插件的调用方式不变。

### 8.7 对话与人设

```python
conversations = await self.core.list_conversations(event)       # same as /ls
await self.core.switch_conversation(event, conversations[0].session_id)
await self.core.append_conversation(event, [("user", "问题"), ("assistant", "回答")])
await self.core.upsert_persona("translator", "Translator", "你是一名专业译者。")
```

- `list_conversations` / `new_conversation` / `switch_conversation` / `delete_conversation` 与内置 `/ls`、`/new`、`/switch`、`/del` 是同一份实现，都返回操作之后的 `ConversationInfo` 列表（`session_id`、`current`、`title`、`message_count`、`last_active_at`）。代替用户调用时请自行检查权限。
- `append_conversation` 只能追加完整的“用户、助手”轮次到当前对话；已有消息不能修改。
- `list_personas` / `upsert_persona` / `delete_persona` 维护与控制台「人设」页相同的目录。
- TS 为驼峰命名（`listConversations`、`switchConversation(event, sessionId)`、`appendConversation(event, [{ role: "user", text }, ...])`、`upsertPersona` 等），返回字段同样为驼峰（`sessionId`、`messageCount`）。
- Rust：`core.list_conversations(&event)`、`switch_conversation(&event, id)` 等；`append_conversation(&event, [("用户", "助手")])` 以“用户、助手”二元组追加，不可能写出不成对的轮次；`upsert_persona(Persona { .. })`。

### 8.8 渲染图片

```python
card = await self.core.render_text("# 今日排行\n1. Alice 120\n2. Bob 95", width=480)
await event.reply(card)

chart = await self.core.render_svg('<svg xmlns="http://www.w3.org/2000/svg" ...>...</svg>')
```

- 返回可直接发送的图片消息段（TS：`await this.core.renderText(text, 480)`、`renderSvg(svg)`；Rust：`core.render_text(text)`、`render_text_width(text, 480)`、`render_svg(svg)`）。文本按宽度自动换行，`# ` 开头的行是标题；SVG 按自身尺寸渲染。
- 由节点用纯 Rust 渲染，不需要浏览器；字体取自节点系统。渲染结果保留 24 小时，请渲染后尽快发送。

---

## 9. 消息段

| 段 | 构造（Python / TS / Rust `segment::`） | 说明 |
| --- | --- | --- |
| 文本 | `text` | |
| 图片 | `image_url` / `image_file` / `image_bytes` | 三种来源三选一 |
| 语音 | `audio_url` / `audio_file` / `audio_bytes` | |
| 视频 | `video_url` / `video_file` | 仅发送 |
| 文件 | `file(name, url=/file_path=/raw_bytes=)`；Rust `file_url` / `file_path` / `file_bytes` | 必须带文件名；仅发送 |
| 表情 | `face(id)` | 平台表情 ID；仅发送 |
| @ | `mention(user_id)` / `mention_all()` | |
| 引用 | `quote(event_id)` | 以平台原生“回复”形式发送 |
| 自定义 | 直接构造 `custom` | 适配器私有段，如 `milky.video` |

TypeScript 中方法名为驼峰（`imageUrl`、`mentionAll`）。`file_path` 指向的是节点本机路径，由适配器读取。

平台支持差异：Milky 不支持发送文件，QQ 官方不支持表情，遇到时投递会明确失败而不是静默丢弃。收到的视频、文件、表情仍以适配器自定义段（如 `milky.video`）的形式出现，因为其中携带的平台 ID 无法用 URL 代替。

---

## 10. 平台适配器插件

在清单中声明 `[adapter] platform = "..."` 后，插件就成了该平台的适配器：

- **出站**：核心把 `platform` 匹配的每条消息通过 `OnDeliverMessage` 投给本插件。必须覆盖 `on_deliver_message`；默认实现会明确返回失败，绝不“假成功”。
- **入站**：用 `core.ingest_event(...)` 把平台消息推回流水线。必须检查返回的 `accepted`：为 `false` 表示核心队列已满、事件被丢弃，适配器应自行降速。
- **元数据**：用 `kanon.conversation_kind`、`kanon.bot_mentioned`、`kanon.sender_name` 等平台无关键名描述事件（见 [PLUGIN_API.md §6.2](./PLUGIN_API.md#62-元数据键)），并通过 `capabilities` 声明实现了哪些通用能力。需要回调适配器的能力（如 `acknowledge`、`platform_api`）只有内置适配器能声明。能发送哪些媒体也在这里声明：`send_image`、`send_voice`、`send_video`、`send_file`——核心只把已声明类型的工具附件交给本插件，未声明的类型以一行说明代替。一个媒体能力都没声明的适配器插件（多为媒体能力出现前写的清单）在加载时会记录警告，`kanon-dev lint` 也会提示。

完整契约见 [ARCHITECTURE.md §9.5](./ARCHITECTURE.md)。

---

## 11. 配置表单、控制台页面与 HTTP 路由

### 11.1 配置表单

控制台按 `[config_schema]` 自动生成配置表单，操作员也可随时切换到 JSON 视图。常用写法与对应控件：

| Schema | 控件 |
| --- | --- |
| `type = "boolean"` | 开关 |
| `enum = [...]` | 下拉框；非必填字段多一个“默认”选项 |
| `type = "string"` | 单行输入；`writeOnly = true` 或 `format = "password"` 为密码框，`format = "textarea"` 为多行 |
| `type = "number"` / `"integer"` | 数字输入，保存前校验 |
| `type = "array"`，`items` 为字符串或数字 | 每行一项的列表 |
| `type = "object"` 且有 `properties` | 嵌套分组 |
| 其他 | 该字段单独的 JSON 输入框 |

`title` 作标签、`description` 作说明、`default` 作占位提示；清空字段即删除该键，让插件使用自己的默认值。

### 11.2 控制台页面 `pages/`

插件目录下的 `pages/`（`index.html` 加静态资源）会出现在控制台插件卡片的「打开页面」里，地址为 `/api/v1/plugins/<id>/pages/`。页面只读提供：不列目录、不提供隐藏文件（`.env`、`.git`），符号链接不能指向目录之外。

页面运行在沙盒中（`Content-Security-Policy: sandbox`，内嵌框架也不授予 `allow-same-origin`）：可以运行脚本、提交表单、弹窗和下载，但拿不到控制台的源，读不到它的存储，也不能以控制台身份调用管理接口。页面需要的数据由插件自己的 HTTP 路由提供，用相对路径访问即可：

```js
const stats = await fetch('../http/stats').then((r) => r.json());
```

### 11.3 HTTP 路由

插件可以在 `/api/v1/plugins/<id>/http/<path>` 下提供接口，用于 Webhook 回调、给页面用的数据等：

```python
@http_route("/stats")
async def stats(self, request: HttpRequest) -> dict:
    return {"page": request.arg("page"), "visits": await self.kv.get("visits", 0)}

@http_route("/webhook", methods=("POST",))
async def webhook(self, request: HttpRequest) -> HttpResponse:
    if request.headers.get("x-signature") != expected_signature(request.body):
        return HttpResponse(status=401)
    payload = request.json()
    ...
    return HttpResponse(status=204)
```

- TS：`@HttpRoute("/stats")`、`@HttpRoute("/webhook", { methods: ["POST"] })`，处理函数收到同样的 `HttpRequest`，返回值规则相同（`new HttpResponse(401)`、`HttpResponse.json(value, 201)`）。
- Rust：`router.http_route("GET", "/stats", |request| async move { Ok(json!({ .. })) })`，每个方法单独声明。`http::Request` 有 `query_param(name)`、`header(name)`、`text()`、`json::<T>()`；返回 `serde_json::Value`（JSON）、`String`（纯文本）或 `http::Response::new(204)` 等。处理函数里需要核心时用 `router.context().core()?`。
- `HttpRequest` 有 `method`、`path`、`query`（`request.arg(name)` 取第一个值）、`headers`（小写名）、`body`，以及 `text()`、`json()`。
- 返回字典、列表等即为 JSON；返回 `str` 为纯文本，`bytes` 为二进制，`None` 为 `204`；需要状态码或响应头时返回 `HttpResponse`（`HttpResponse.json(...)`、`.text(...)`、`.html(...)`）。
- 路径精确匹配。未知路径返回 `404`，方法不符返回 `405`（附 `Allow` 头），处理函数抛出异常返回 `500`——异常只打印到宿主的标准错误，不会发给调用方。
- 请求体至多 3 MiB、30 秒内必须应答。协议层细节（`serves_http`、`OnHttpRequest`）见 [PLUGIN_API.md](./PLUGIN_API.md)。

网关只转发、不鉴权，这些路由与管理网关一同对外暴露：接收 Webhook 时请校验平台签名，修改数据的接口要先校验调用方。

---

## 12. 多语言

在 `i18n/<语言标签>.json`（如 `zh-CN.json`、`en.json`）中翻译控制台展示的文字。每个文件是一个只含字符串的扁平 JSON 对象，可用的键：

| 键 | 含义 |
| --- | --- |
| `name`、`description` | 插件名称与简介 |
| `config.<字段路径>.title`、`config.<字段路径>.description` | 配置字段的标签与说明；路径为从根开始的属性名，以 `.` 连接，如 `config.api.key.title` |
| `commands.<命令名>.description` | 命令说明 |

```json
{
  "name": "天气",
  "description": "查询天气并向模型提供天气工具",
  "config.api_key.title": "API 密钥",
  "commands.weather.description": "查询城市实时天气"
}
```

控制台选用与界面语言相同（或为其地区变体，如界面为 `zh` 时的 `zh-CN`）的文件，缺失的键回退到清单与代码中的原文。无法使用的文件或键（不是 JSON、值不是字符串、未知键、超过 256 KiB）不会被静默忽略，而是显示在插件卡片的“翻译文件问题”里。翻译只影响控制台；聊天中的 `/help` 与模型看到的工具说明仍使用代码中的原文。

---

## 13. 发布与安装

### 13.1 安装来源

控制台「扩展 → 插件 → 安装插件」（即 `POST /api/v1/plugins/install`）支持四种来源，全部经过同一个安装器：

| 来源 | 请求 | 说明 |
| --- | --- | --- |
| 节点上的目录 | `{"path": "./my_plugin"}` | 相对路径从节点工作目录算起 |
| 上传插件包 | multipart 上传 `.kpk` / `.zip` | 包至多 64 MiB（链接下载同此限制），解压后至多 256 MiB、1 万个条目 |
| 包链接 | `{"url": "https://…/weather-1.2.0.kpk"}` | 只允许 `https`（`http` 仅限回环地址），不跟随降级到 `http` 的跳转 |
| Git 仓库 | `{"git": "https://…/weather.git", "ref": "v1.2.0"}` | 使用系统的 `git`；`ref` 可选（分支或标签） |

安装器先校验清单与 `kanon_version`，在 `./plugins` 内暂存一份完整副本，再停止旧宿主、原子替换、拉起新宿主；暂存失败时旧版本原样保留。依赖不在安装时处理，而是由节点在启动插件前用该语言自己的工具安装（见 §2.2）。

同一 `id` 已安装时返回 `409`，需在请求中加 `"replace": true` 才会替换（控制台会把按钮变成「替换」）。已安装的插件保留原来的目录：即使是手动复制进来、目录名与 `id` 不同的插件，从原目录重新安装即原地登记，替换也落在原目录，同一个 `id` 不会出现两份。

### 13.2 插件市场

市场就是一个 JSON 索引文件。运维在 `data/system.json` 中列出要读取的索引，控制台「扩展 → 市场」合并展示、标注已安装版本与兼容性，并一键安装或更新；未配置时节点不会访问任何第三方：

```json
"plugin_market": {
  "indexes": ["https://example.org/kanon/index.json"]
}
```

索引格式：

```json
{
  "name": "Example market",
  "plugins": [
    {
      "id": "org.example.weather",
      "name": "Weather",
      "description": "Weather commands and a weather tool",
      "author": "Example",
      "version": "1.2.0",
      "download_url": "https://example.org/weather-1.2.0.kpk",
      "repository": "https://github.com/example/kanon-weather.git",
      "kanon_version": ">=0.1, <0.3",
      "platforms": ["qq"],
      "homepage": "https://example.org/weather"
    }
  ]
}
```

`id`、`name`、`version` 必填，`download_url`（优先）与 `repository` 至少有一个；未知字段被忽略。格式错误的条目作为该索引的警告显示，不影响其他条目；`kanon_version` 不满足的条目会标注原因且不可安装。安装时仍以包内的 `plugin.toml` 为准，并再次校验 `kanon_version`。

---

## 14. 约束与最佳实践

- **不要读环境变量做配置**。宿主只会注入 `KANON_HOST_ID`、`KANON_HOST_SOCK`、`KANON_CORE_SOCK`、`KANON_IPC_TOKEN`（以及操作系统变量）；插件配置一律走 `[config_schema]` + 控制台。
- **不要硬编码绝对路径**，使用 `data_dir`。
- **不要阻塞事件循环**。Python 中的 CPU 密集或阻塞 I/O 请放进 `asyncio.to_thread`；Rust 请用 `spawn_blocking`。
- **前置过滤要快**（总预算 30ms），**回复装饰要快**（3 秒），**事件处理不要依赖顺序**（并发、即发即忘）。
- **共享状态**：Rust `Router` 的处理函数是 `'static` 闭包，请用 `Arc` 捕获共享状态；Python/TS 直接用实例属性即可，但要注意多个会话的处理函数会并发执行。
- **调试**：宿主进程继承节点的标准输出与标准错误，`print` / `console.log` 会直接出现在节点终端；`kanon-dev test` 可以在没有聊天平台和模型的情况下跑通命令、工具与钩子，`kanon-dev dev` 在改动后自动重启插件。
