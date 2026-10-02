# Kanon 跨语言 Bot 架构设计与技术规范文档

> **项目名称**：Kanon (カノン)  
> **定位**：高性能、低内存占用、高并发的现代聊天机器人微内核引擎，支持 **Rust / Python / TypeScript** 多语言插件与适配器无缝接入，全系统严格模块化，前后端完全解耦。  
> **文档版本**：v1.1.0-draft  
> **状态**：方案制定与规范设计阶段  

---

## 1. 架构选型与背景对比

### 1.1 设计背景与核心工程挑战

在多平台聊天机器人与大模型（LLM）融合的应用场景中，随着业务复杂度增加，系统在生产环境及资源敏感环境中面临以下核心工程挑战：
- **资源占用与环境纯净度**：单体脚本运行时在冷启动与基础内存上存在一定门槛，跨机部署时常受限于目标环境的解释器版本与三方依赖库。
- **并发吞吐与网络心跳稳定性**：高频事件与大模型流式推理耗时较长，如果缺乏严格的异步解耦机制，容易导致 IM 网关底层的长连接心跳出现超时断连。
- **第三方扩展的物理故障隔离**：社区第三方插件生态多样，若单插件发生未捕获异常、耗时同步阻塞或底层动态库段错误 (SegFault)，在单进程模型下极易导致全系统瘫痪。

为此，Kanon 从设计之初即确立了微内核架构，兼顾极致的自包含性、高并发稳定性与多语言插件接入体验。

### 1.2 核心选型方案对比矩阵

针对“Rust 核心 + 多语言（Rust/Python/TS）插件接入”的目标，技术委员会对业界三种主流架构进行了评估：

| 评估维度 | 方案 A：进程外宿主 (Out-of-Process gRPC via UDS) ★选定 | 方案 B：单进程内嵌运行时 (PyO3 + V8/QuickJS) | 方案 C：WebAssembly 沙箱 (Wasmtime / Extism) |
| :--- | :--- | :--- | :--- |
| **故障隔离性** | **物理隔离**：插件崩溃/OOM 不影响主核心与其他会话 | **较弱**：插件 C 扩展崩溃可能导致主进程异常退出 | **极优**：内存/指令级硬沙箱隔离 |
| **生态兼容性** | **良好兼容**：pip / npm / Cargo 生态大部分功能开箱可用 | **良好但受限**：受制于 PyO3 与 Tokio 异步运行时的死锁风险 | **受限**：带底层 C/C++ 绑定的三方库难以编译为 WASI |
| **开发体验** | **原生调试**：各自语言的原生调试器、热重载与工具链 | **中等**：跨语言 FFI 较复杂，宏报错较难排查 | **繁琐**：需要专用工具链将源码编译为 .wasm |
| **进程间通信延迟**| **低延迟**：UDS 内部管道传输，基准耗时通常在微秒级（远低于网络 I/O）| **零开销**：直接内存共享访问 | **微秒级**：Wasm 内存拷贝 |
| **交付难度** | **标准解耦**：Rust 主二进制独立，插件依赖由 `uv`/`npm`/`bun` 在插件目录内自管 | **困难**：分发时通常需动态链接特定版本的运行时动态库 | **单文件**：单二进制内嵌运行时 |

**结论**：选定 **方案 A（Out-of-Process Sidecar + gRPC over UDS）**，在保持 Rust 核心轻量精炼和高可靠的同时，为 Rust、Python 与 TypeScript 开发者提供统一的开发接入体验。

---

## 2. 总体拓扑与全模块化工程架构

### 2.1 独立端点通信拓扑 (Dedicated Endpoint per Host Model)

- **运行时目录与 POSIX 权限防御**：系统遵循 XDG 规范，使用跨平台隔离目录（如 Linux/macOS 优先使用 `$XDG_RUNTIME_DIR/kanon/run/`；Windows 下使用安全随机端口 TCP Loopback 并注入 32-Byte CSPRNG Token 校验）。在无桌面环境的纯净 Linux 服务器、Docker 容器或最小化 Alpine 系统中，环境变量 `$XDG_RUNTIME_DIR` 经常未被设置。若代码静默降级为标准 `/tmp/kanon-run/`，由于公共 `/tmp` 的黏滞位特性和共享可见性，会导致本地其他用户窥探甚至通过符号链接劫持 socket 文件。因此规范补充如下硬性分支规定：
  - **UID 隔离回退**：当 `$XDG_RUNTIME_DIR` 为空时，回退目录为 `/tmp/kanon-run-$UID/`（通过 POSIX `libc::getuid()` 获取调用方真实 Effective UID）。
  - **强制 0700 权限**：核心与 Supervisor 在创建或检测该目录时，必须通过系统调用强制显式设为 `0700`（仅当前 UID 拥有 `rwx------` 权限），若存在权限冲突或符号链接劫持则立即拒绝启动。
- **核心端点 (`core.sock`)**：Rust 核心启动 gRPC 服务端，监听 `core.sock`，向所有 Host 提供 `BotApiService`（主动发消息、事件灌入、LLM 代理等）。
- **宿主独立端点 (`host_<id>.sock`)**：Supervisor 为每个拉起的 Host 分配专属通信端点，Host 在该端点启动 gRPC 服务端，提供 `MessagePipelineService`。Rust 核心作为 Client 连接各 Host 端点发起命令调度与 Tool Calling。
- **架构收益**：保持标准 gRPC 纯粹语义，双向互相调用完全解耦，绝无应用层帧路由负担，每个端点均可使用 `grpcurl` 独立排障。

```mermaid
flowchart TB
    subgraph RuntimeDir["运行时端点目录 (Runtime Directory)"]
        CoreSock["core.sock (Core 提供 BotApiService)"]
        PySock["host_py_101.sock (PyHost 提供 PipelineService)"]
        TsSock["host_ts_102.sock (TsHost 提供 PipelineService)"]
        RustSock["host_rust_103.sock (RustHost 提供 PipelineService)"]
    end

    subgraph CoreDomain["Rust 核心 (Kanon Core)"]
        CoreSrv["BotApiService Server (监听 core.sock)"]
        Router["流水线调度与路由器"]
        Supervisor["Supervisor 进程管理器"]
        Transport["kanon-transport 抽象层"]
    end

    subgraph HostDomain["多语言插件宿主 (独立进程)"]
        PyHost["Python 插件 Host 进程 (独立 .venv)"]
        TsHost["TypeScript 插件 Host 进程"]
        RustHost["Rust 原生插件独立二进制"]
    end

    CoreSrv <==> CoreSock
    PySock <==> PyHost
    TsSock <==> TsHost
    RustSock <==> RustHost

    PyHost -->|IngestEvent / SendMessage| CoreSock
    TsHost -->|IngestEvent / SendMessage| CoreSock
    RustHost -->|IngestEvent / SendMessage| CoreSock

    Router -->|OnPreFilter / OnExecuteCommand / OnCallTool| PySock
    Router -->|OnPreFilter / OnExecuteCommand / OnCallTool| TsSock
    Router -->|OnPreFilter / OnExecuteCommand / OnCallTool| RustSock

    Supervisor -->|纳管进程生命周期 / 注入端点与鉴权 Token| HostDomain
```

### 2.2 生产环境进程隔离与崩溃防护 (Process Isolation Strategy)

1. **生产环境默认独立子进程隔离 (Per-Plugin Process)**：
   - 为有效隔离单插件崩溃传播、同步 I/O 阻塞（如 `time.sleep`、无超时网络请求）拖垮同语言其他插件，**生产环境下每个插件默认独占独立子进程与专有依赖环境 (`.venv` / `node_modules`)**。
   - 某个插件遭遇 SegFault、OOM 或死锁，仅自身子进程退出，核心与其他插件不受影响。
2. **轻量开发环境共享模式 (Shared Host for Dev)**：
   - 仅在 `kanon-dev dev` 本地调试或用户显式配置 `group = "shared"` 时，才允许多个受信任的轻量插件合部在同一个共享 Host 进程中以节约开发机内存。

### 2.3 插件宿主进程完整生命周期 (Host Process Lifecycle)

进程隔离只有在"宿主进程生命周期完整"时才成立：**没有看门狗的宿主在 Core 被杀后会变成幽灵进程，继续持有平台长连接**；一旦新 Core 启动，幽灵与新生宿主会同时服务同一平台，导致每条消息被处理两次（两次回复、两次模型调用）。因此所有语言的宿主必须实现同一套生命周期：

| 阶段 | 行为 | 责任方 |
| :--- | :--- | :--- |
| 启动 | `Supervisor` 以 `KANON_HOST_ID` / `KANON_HOST_SOCK` / `KANON_CORE_SOCK` 注入环境并拉起子进程 | Core |
| 注册 | 宿主调用 `RegisterHost`（注册即连通性探针；失败则显式进入 standalone）。Supervisor 正在拉起的宿主只获确认，`GetPluginMeta` 握手由拉起流程在其 `host_<id>.sock` 就绪后完成；只有外部宿主会被 Core 回拨，须先开始服务再注册 | 宿主 + Core |
| 服务 | `MessagePipelineService`（事件/指令/工具/投递）+ `PluginHostService`（生命周期、配置热重载、管理动作） | 宿主 |
| 存活 | 定时 `BotApiService.Ping` 探活 Core；**连续 3 次失败（默认 15s 间隔）即自行退出**，避免抢占平台连接 | 宿主（Python `CoreWatchdog` / Rust `watch_core` / TS `startCoreWatchdog`） |
| 停止 | `SIGTERM`/`SIGINT` → 执行 `on_unload` → 关闭平台连接 → 卸载 socket | 宿主 |
| 回收 | Core 关停时先 `SIGTERM`，超过 3s 宽限期仍未退出则 `SIGKILL`；未就绪的宿主直接强杀 | Core (`terminate_child`) |
| 外部注册宿主 | 无子进程句柄不可杀；Core 仅将其移出注册表，宿主依靠自身看门狗退出 | Core + 宿主 |

> 由此可保证：任何一次 Core 退出（含 `SIGKILL`、终端关闭）都不会留下仍在服务平台的宿主；重启后平台连接数恒为 1。

### 2.4 全模块化工程结构划分 (Modular Workspace Architecture)

全系统严格模块化，前后端完全物理分离，各 crate / package 职责单一：

```
kanon/
├── Cargo.toml                      # 根 Workspace 配置
├── proto/
│   └── kanon/v1/plugin.proto       # 跨语言通用 gRPC 协议契约 (强类型 oneof & Struct 双模载荷)
├── crates/
│   ├── kanon/                      # 程序入口：唯一节点二进制 kanon（组合根，仅负责装配）
│   ├── kanon-proto/                # gRPC 契约与 Tonic 桩代码生成 (含 prost-types)
│   ├── kanon-transport/            # 跨平台 IPC 传输层抽象 (UDS / 认证 Loopback TCP)
│   ├── kanon-core/                 # 核心事件循环、消息流水线、Supervisor 进程监管 (库)
│   ├── kanon-storage/              # 嵌入式持久化支持与插件安全目录隔离管理器 (库)
│   ├── kanon-llm/                  # LLM 多端点路由、静态优先提示词分层、仅追加会话记忆与缓存友好的上下文压缩、Tool Calling 状态机 (库)
│   ├── kanon-api/                  # Axum RESTful API 与实时 WebSocket 驱动 (库，供独立前端连接)
│   └── kanon-dev/                  # 官方专用 CLI：项目管理、模板脚手架、开发热重载与沙盒测试
├── sdks/
│   ├── rust/                       # Rust 插件开发 SDK (kanon-sdk)
│   ├── python/                     # Python 插件开发 SDK (kanon-sdk-python)
│   └── typescript/                 # TypeScript 插件开发 SDK (kanon-sdk-ts)
└── webui/                          # 前后端完全独立的现代 Web 控制台 (Svelte 5 SPA)
```

**构建产物与程序入口 (Build Outputs)**：
- 全工程只有两个可执行文件：节点二进制 `kanon`（由 `crates/kanon` 产出）与开发者 CLI `kanon-dev`（由 `crates/kanon-dev` 产出）；
- 根 `Cargo.toml` 的 `default-members` 仅包含上述两个 crate，因此一次默认构建（`cargo build` / `cargo test`）恰好只产出这两个可执行文件；
- 其余 crate 一律为纯库、不提供任何入口：`crates/kanon-api` 只导出网关库，微内核 `crates/kanon-core` 只导出引擎与 Supervisor 库；
- 示例插件宿主（`plugins/demo_weather`、`sdks/rust/plugins/demo_rust_plugin`）是插件 IPC 环路的可运行夹具，不属节点产物，需显式 `cargo build -p demo-weather -p demo-rust-plugin`（或用 `--workspace`）构建。

### 2.5 运行时设计原则：零外部硬依赖与按需惰性激活 (Zero Hard Dependency Principle)

> **核心哲学**：**Python 与 Node.js / Bun 并非运行 Kanon 微内核的必需品。**  
> Kanon 核心与 Rust 原生插件是独立自包含的原生二进制程序，可以在没有任何外部脚本解释器的纯净操作系统上独立运转。

1. **零硬依赖（独立纯净运行）**：
   - 若用户仅使用 Rust 原生插件、内置组件或基础 IM 网关，Kanon 完全不依赖、不探测、不接触任何 Python 或 Node 环境。
   - 核心具备轻量空载基准（在基础调度下内存占用通常低于 20MB），分发时仅需单个独立二进制文件。
2. **按需惰性激活 (On-Demand & Lazy Activation)**：
   - 核心启动时**不会盲目启动任何外部子进程**。
   - 仅当扫描 `plugins/` 目录且**确实存在**声明为 `runtime = "python"` 的插件时，Supervisor 才会使用该插件目录下的 `.venv` 拉起 Python 宿主。
   - 同理，仅当**确实存在**声明为 `runtime = "typescript"` 的插件时，才会尝试探测 Node/Bun 并拉起 `kanon-tshost`。
3. **环境缺失优雅降级 (Graceful Degradation)**：
   - 若用户放入了 Python 或 TS 插件，但当前操作系统未安装对应运行环境：
     - Kanon 核心**不会直接崩溃或拒绝启动**；
     - 仅对缺少运行时的插件输出清晰友好的告警日志，将其状态标记为 `RuntimeUnavailable`；
     - 机器人核心、IM 适配网络连接及所有 Rust 插件依然照常运行。
4. **可选运行时的极简治理（仅在用户需要时生效）**：
   - **Python**：每个插件使用自己目录下的 `.venv`，绝不回退到共享或系统解释器。
   - **TypeScript**：仅在激活 TS 插件时，优先检测 `bun` 或 `node/tsx`；若插件的 `package.json` 声明了 `dependencies`，则使用插件自己的 `node_modules`。
   - **自动安装依赖（调用原生工具）**：启动插件前，若其环境不存在，或比描述它的文件（`pyproject.toml`、`uv.lock`、`package.json`、锁文件）旧，Supervisor 在插件目录内调用该生态自己的工具：Python 为 `uv sync`（有 `uv.lock` 时加 `--locked`，并显式指定 `UV_PROJECT_ENVIRONMENT=<plugin>/.venv`）；TypeScript 按锁文件选工具——`bun.lock` 用 `bun install --frozen-lockfile`，`package-lock.json` 用 `npm ci`，都没有时用 `bun install`（无 bun 时 `npm install`）。成功后在环境内写入标记文件 `.kanon-installed`，以它的修改时间判断“是否过期”，所以改了依赖的插件在下次启动时自动重装，最新的插件只多几次 `stat`。
   - 安装有超时（10 分钟，超时即终止子进程），同一插件目录同一时间只有一次安装；失败时插件标记为 `RuntimeUnavailable`，原因附工具输出的最后 20 行，下次启动再试。工具未安装而环境已存在时沿用现有环境并记录警告；环境也不存在则报告 `RuntimeUnavailable`。
   - **Kanon 不是包管理器**：`kanon` 与 `kanon-dev` 自己不解析、不下载、不锁定任何包，只调用原生工具；运维可在 `data/system.json` 的 `startup.install_dependencies` 设为 `false` 关闭自动安装，此时由运维自行安装，环境缺失即 `RuntimeUnavailable`。

### 2.6 事件入站异步队列与防锁步机制 (Async Ingest Queue & Lockstep Prevention)

为防范 **“LLM 慢推理/插件慢 I/O 反向阻塞 IM 适配器心跳”** 的时序锁步风险，系统确立严格的异步解耦规则：

1. **Fast-ACK 快速确认机制**：
   - 适配器插件调用 `BotApiService.IngestEvent` 时，Rust 核心仅执行两件事：事件有效性基本校验、放入带高水位（默认 10,000 缓冲）的内部 Tokio MPSC 异步通道。
   - 核心在非阻塞推入有界内存通道后立即 Fast-ACK 返回（基准投递延迟通常在微秒级，受 OS 调度与系统负载影响）。
2. **流水线异步消费与独立出站**：
   - 核心工作协程池独立从 MPSC 通道消费事件，调度 PreFilter 拦截链、LLM 编排与 Tool Calling 状态机。
   - 无论 LLM 生成耗时多久，反压均被内部有界队列隔离，绝不沿着 gRPC 链路逆向传导至适配器。
   - 最终出站响应通过独立的 `OnDeliverMessage` 单向调用适配器，适配器底层（WebSocket 心跳、长轮询）保持保活稳定性，大幅降低因业务阻塞导致的断连风险。

### 2.7 Windows 本地 Loopback TCP 密码学鉴权规范 (Loopback Token Authentication)

在 Windows 环境下采用本地端口（`127.0.0.1:EphemeralPort`）通信时，为有效防范同机器上非特权进程伪造请求或注入数据，制定硬性安全约束：

1. **32-Byte 随机 Token 注入**：
   - Supervisor 在拉起任何 Host 子进程前，通过密码学安全随机数生成器 (CSPRNG) 生成 32 字节高熵随机 Token（64 字符十六进制编码）。
   - Token 仅通过子进程私有环境变量 `KANON_IPC_TOKEN`（或私有安全 stdin 握手管道）单向注入该子进程，对外不可见。
2. **首包 HTTP/2 HEADERS 恒定时间校验**：
   - Host 连接 Core 或 Core 连接 Host 时，首帧必须携带 `x-kanon-auth-token`。
   - 接收端通过 `subtle::constant_time_eq` 恒定时间比对，校验失败立即切断连接。

### 2.8 Linux/macOS Unix Domain Socket 目录隔离与权限加固规范 (POSIX Socket Security)

在 Unix/Linux 环境下采用 UDS（Unix Domain Socket）通信时，为有效防范本地非特权多租户环境下的符号链接劫持、套接字投毒与非法窃听，制定强制安全约束：

1. **运行目录发现与 UID 安全隔离**：
   - 优先遵循 XDG Base Directory 规范使用 `$XDG_RUNTIME_DIR/kanon/run`；
   - 若环境变量 `$XDG_RUNTIME_DIR` 未设置（例如无桌面环境、部分 Docker 容器或基础 SSH 会话），强制安全回退至 `/tmp/kanon-run-$UID/`（通过 POSIX `libc::getuid()` 获取调用方真实 Effective UID），有效消除多用户同机共享未隔离路径的安全风险。
2. **符号链接攻击防御 (Symlink Rejection)**：
   - 在创建或绑定套接字前，核心与 Host 必须使用 `std::fs::symlink_metadata` 深度检查目录元数据；
   - 若目标运行目录属于符号链接（Symlink），直接拒绝并抛出 `std::io::ErrorKind::PermissionDenied`，防止本地非特权攻击者预先埋设软链接诱骗核心向敏感系统路径写入套接字。
3. **强制 0700 权限收敛 (Mandatory 0700 Permissions Enforcement)**：
   - 运行目录的所有者 UID 必须与当前进程所有者完全一致；
   - 无论是全新建立目录还是已存在的既有目录，统一调用 `Permissions::from_mode(0o700)`（`rwx------`）强制将目录权限收敛为仅当前用户可读写执行，剥夺同组及全局用户的读取与遍历权限。

### 2.9 kanon-transport 架构落地形态与 Tower 鉴权中间件

为消除 Tonic/Hyper 与操作系统底层的适配胶水代码，`kanon-transport` 提供统一抽象：

1. **统一流封装 (`IpcStream`)**：
   - 跨平台抽象底层 `UnixStream` 与 `TcpStream`，实现 `AsyncRead`、`AsyncWrite` 与 `tonic::transport::server::Connected`；
   - 核心与 Host 统一使用 `Server::builder().serve_with_incoming(...)`，零感知底层协议差异。
2. **Tower / Tonic 统一鉴权拦截器 (`AuthInterceptor`)**：
   - 将 Windows 安全鉴权封装为标准的 `tonic::service::Interceptor`；
   - 在 gRPC 解包前统一从 HTTP/2 HEADERS 中抽取 `x-kanon-auth-token` 并进行 `constant_time_eq` 恒定时间校验；
   - 鉴权逻辑严格隔离在 Transport 层，禁止将安全握手代码散落侵入至上层业务 RPC Handler。
3. **跨语言客户端的 HTTP/2 `:authority` 硬性约束**：
   - Core 服务端基于 Tonic/Hyper（`h2`），会对 `:authority` 做严格语法校验；百分号转义仅允许出现在 userinfo 或 IPv6 zone id 中，出现在 host 段即判定为畸形头，服务端在业务逻辑执行前直接回 `RST_STREAM(PROTOCOL_ERROR)`。
   - **禁止将 socket 路径写入 `:authority`**：gRPC C-core（Python）在 `unix:/abs/path/core.sock` 目标下会把百分号转义后的路径（如 `run%2Fuser%2F1000%2Fkanon%2Frun%2Fcore.sock`）当作 authority 发送，导致 `BotApiService` 全部 RPC 失败，且客户端只能看到与崩溃相似的误导性错误：`StatusCode.INTERNAL "Stream removed (RST_STREAM (Received RST_STREAM with error code 1))"`。改写为 `unix:///abs/path` 亦无效，必须显式固定合法 authority。
   - Python SDK 统一通过 `kanon_sdk.ipc.connect_core_channel()` 建链（固定 `grpc.default_authority`），禁止各 Host / 插件自行调用 `grpc.aio.insecure_channel("unix:...")`；Rust SDK（Tonic `http://localhost`）与 TypeScript SDK（gRPC-js unix resolver 固定 `localhost`）天然合规。

---

## 3. 插件清单规范 (Plugin Manifest Spec)

每个插件放置于独立目录下，以静态 `plugin.toml` 声明身份、运行时、入口、优先级、配置 Schema 与可选的平台适配器声明。完整字段说明与示例见 [PLUGIN_GUIDE.md §3](./PLUGIN_GUIDE.md)。

架构层面的约束：

- **清单是静态的**：核心无需启动子进程即可据此渲染配置表单、确定平台归属与调度优先级；命令、触发器、工具等运行期能力以握手时的 `GetPluginMeta` 为准，清单中的 `[[commands]]` / `[[tools]]` 仅供控制台离线展示。
- **严格解析**：未知小节或字段（包括旧的 `[dependencies]`）直接解析失败。
- **清单不声明依赖**：Python 依赖写在 `pyproject.toml`（附 `uv.lock`），TypeScript 写在 `package.json`（附锁文件），Supervisor 启动插件前调用原生工具安装到插件目录内的 `.venv` / `node_modules`（见 2.5）；安装失败即 `RuntimeUnavailable`。

---

## 4. 通信协议规范 (Protocol Buffers IDL)

协议采用强类型 `oneof` 联合体与 `google.protobuf.Struct` 双模载荷，避免 JSON 字符串二次序列化与二进制载荷的内存膨胀。三个服务的分工：

| 服务 | 运行在 | 职责 |
| --- | --- | --- |
| `PluginHostService` | 宿主 | 存活探测、配置热更新、能力握手、管理动作 |
| `MessagePipelineService` | 宿主 | 前置过滤、命令/触发器、工具调用、事件通知、出站投递（适配器）、回复装饰 |
| `BotApiService` | 核心 | 宿主注册、入站事件、回复与主动发送、独立模型调用、平台原生 API |

IDL 唯一事实来源为 [`proto/kanon/v1/plugin.proto`](../proto/kanon/v1/plugin.proto)；逐个 RPC 的语义、时限、错误码与数据类型见 [PLUGIN_API.md](./PLUGIN_API.md)。

### 4.1 OnPreFilter 拦截链执行顺序与性能预算 (Pipeline Deadline & Priority)

为防止多个外部 Python/TS 宿主在文本前置过滤阶段阻塞消息主流程，系统确立严格的执行流水线规范：

1. **优先级调度链 (Priority Chain)**：
   - 插件清单 `plugin.toml` 中必须显式声明 `priority`（范围 `1 ~ 1000`，数值越小越先执行，未声明默认 `500`）；
   - 核心 Pipeline 严格按 priority 升序依次调用各插件宿主的 `OnPreFilter`。
2. **总耗时预算与动态熔断 (Strict 30ms Deadline)**：
   - **全局预算**：整条 PreFilter 拦截链分配严格的总体 Deadline（**最大 30ms**）。
   - **单插件告警阈值**：单插件执行耗时超过 **5ms** 即在日志输出性能劣化警告。
   - **超时短路保护**：一旦链条累计耗时逼近 30ms，核心立即短路跳过后续尚未执行的 PreFilter 插件，直接放行消息进入命令匹配与大模型路由，杜绝慢脚本拉低系统吞吐。

---

## 5. 多语言 SDK 开发者体验规范 (Rust / Python / TypeScript)

SDK 只是协议的封装：三语言提供同一套能力（命令、正则触发器、多轮对话、LLM 工具、管理动作、事件订阅、回复装饰，以及回复、主动发送、独立模型调用与平台原生 API），名称按语言习惯调整。能力对照表与三语言示例见 [PLUGIN_GUIDE.md](./PLUGIN_GUIDE.md)。

**`wait_next` 的实现约束**：核心流水线逐条串行处理消息，绝不阻塞等待插件"听到"下一条消息。因此 SDK 把命令处理器放在独立任务里运行，当前 RPC 只等待处理器的下一个"让出点"：处理器结束，或调用 `wait_next`。`wait_next(t)` 立即结束当前 RPC（携带已收集的回复与 `capture_seconds = t`），核心把同一发送者在同一会话的下一条消息作为 `continuation` 路由回来，SDK 再唤醒挂起的处理器。同一会话的新等待会取代旧等待（旧等待收到超时）；宿主重启后到达的 continuation 会以 `continuation = true` 重新调用命令处理器。没有 RPC 在等待时的回复（例如等待超时后）经 `ReplyMessage` 单独投递。

---

## 6. 异常、自适应背压与熔断保障机制

1. **子进程崩溃自愈与故障物理隔离**：
   - 生产环境采用独立子进程，任何插件崩溃（OOM、SegFault、未捕获异常）均被完全限制在其子进程内部。
   - Supervisor 捕获退出状态码并触发指数退避重启（1s -> 2s -> 4s，最多重试 5 次），核心与其他插件保持正常运行。
2. **IPC 超时与分层熔断机制 (Tiered Timeouts & Circuit Breaking)**：
   - **插件调用超时控制**：指令执行默认超时 5 秒；LLM Tool Calling 默认超时 15 秒；PreFilter 链执行受整体超时预算控制（默认 30ms），超出预算时自动短路跳过后续插件，防止慢脚本拖垮整个流水线。
   - **平台出站熔断保护 (Platform Circuit Breaker)**：由 `PipelineEngine` 为各出站平台独立维护熔断状态机。当目标平台出站连续失败达 5 次时，自动进入 `Open` 状态进行快速短路与死信归档，并以 30 秒周期进入 `Half-Open` 试探探测，避免无效重试持续挤占系统计算资源与并发通道。
3. **优雅停机与资源回收**：
   - 主核心捕获 SIGINT/SIGTERM 后，向所有激活的 Host 广播停机通知，宿主触发插件 `on_unload` 钩子并在规定时限内平稳退出，核心自动清理运行时目录下的所有 socket 文件与临时状态。

---

## 7. 插件状态与数据持久化规范 (Persistence & Anti-Amplification Spec)

针对高频 PreFilter 场景下的 I/O 放大风险与进程隔离要求，采用 **读缓存本地化 + 独立数据目录物理隔离** 的工程原则：

1. **配置与元数据本地化缓存 (Read-Cache in Host Memory)**：
   - 只读配置项、白名单、动态参数在插件加载及核心推送 `ReloadPluginConfig` 时，直接常驻于 Host 进程内存字典中。
   - 过滤链与指令处理一律内存命中，严禁每条消息往返一次 gRPC 远程读取配置。
2. **中心 KV 存储（小状态）**：
   - 计数器、开关、缓存的令牌、按用户的设置这类小状态，插件直接用核心的 KV 存储（`SetStorage` / `GetStorage` / `DeleteStorage` / `ListStorage`，SDK 封装为 `kv`），无需自己建库。
   - 实现为 `kanon_storage::KvStore`：单个 SQLite 文件 `data/kv.db`（WAL），主键 `(plugin_id, key)`，每个插件一个命名空间；值是不透明字节（SDK 在其上做 JSON）。键 1–256 字节，值至多 1 MiB，超限显式报错。
   - 可设 TTL：读取、列举时按当前时间过滤，过期即不可见；过期行在被读取、被覆盖时及启动时清理，不需要后台任务。
   - 每次操作在阻塞线程池执行，慢磁盘不会阻塞 gRPC 运行时。文件无法打开时节点启动失败，而不是让插件静默丢失状态。
   - 命名空间用于避免插件之间的键冲突，不是访问控制：插件以节点权限运行，本就能读取 `data/`。
3. **专属物理数据目录（大数据与关系数据）**：
   - 核心在加载插件时，确保 `./data/plugins/<plugin_id>/` 物理隔离目录就绪，并将路径注入 `ctx.data_dir`。
   - 大量数据、需要查询或事务的数据（业务表、离线数据、文件）在插件内部直接使用嵌入式数据库（SQLite / DuckDB）或文件存储在专属目录下，不经核心代理，没有跨进程序列化开销。

---

## 8. LLM 编排与 Tool Calling 状态机规范 (LLM Orchestration Spec)

### 8.1 核心职责与架构定位
Rust 核心全权主导 LLM 的生命周期与推理编排，确保高并发下的 Token 预算控制与流式吞吐：
- **Agent 抽象**：流水线、控制台聊天与插件网关从不直接驱动模型，而是把一轮对话交给 `kanon_llm::Agent`（trait：`run_message_with` / `run_stream` / `compact_session`，以及 `config`、`provider`、`memory`；`run_message` 是不带 `TurnOptions` 的便捷形式，`TurnOptions` 可限制工具轮次或不提供工具），并只依据其 `AgentOutput` 行事。节点目前只有一个实现——内置的 `BuiltinAgent`（`kanon-llm/src/builtin.rs`，基于 `LlmProvider` 的工具循环）；将来换用外部 Agent SDK 时只需新增实现并在 `AgentFactory` 中构建，流水线不变。实现必须遵守同一会话契约：历史仅追加、失败轮次留下可续接的历史、停止信号触发时及时结束、在每次模型请求前与每次工具调用前后调用 `AgentHook`。
- **插件进入智能体的轮次**：流水线回答一条消息的每一轮都由 `PipelineEngine::run_conversation_turn` 驱动——发出 `AGENT_BEGIN` / `AGENT_DONE` 事件、登记 `/stop`，并在 `with_turn` 任务作用域内运行，作用域记录入站消息与该实例启用的插件宿主。节点为每个 Agent 注册 `PluginAgentHook`（`kanon-core/src/pipeline/agent_hook.rs`），它从作用域中找到插件：模型请求前调用 `OnLlmRequest` 改写系统提示（见 8.6），工具调用前后发出 `TOOL_CALL` / `TOOL_RESULT`。作用域是任务本地的而不是 Agent 的字段，因为同一个 Agent 同时服务多个聊天的轮次；作用域之外（控制台聊天、插件的私有运行、后台压缩）从不调用插件，插件在钩子里调用模型因此不会递归。
- **插件调用智能体 (`RunAgent`)**：插件可以让节点的智能体回答一个提示（`kanon-core/src/pipeline/agent_run.rs`）。在某个聊天的对话中运行时，它就是该对话的一轮，同样经 `run_conversation_turn`，且像其他外部写入者一样只在能立即拿到会话写锁时进行；默认则用 `AgentFactory::private_agent` 构建一次性 Agent（独立的内存记忆、不压缩、无插件钩子作用域），会话名为 `plugin:<plugin_id>:<n>`，结束即丢弃。两种运行都不执行 Bash。
- **统一模型网关**：内置支持 OpenAI-compatible、DeepSeek、Claude、Ollama 等多端点协议，支持动态权重与故障自动重试。
- **全局会话上下文管理 (Session Memory)**：
  - 基于 `channel_id:sender_id` 分配会话上下文，实例会话键为 `instance:<id>:<会话>#<代数>`：一个会话（私聊、群成员或共享的群）可以有多段对话，每段是一个代数，实例记录当前代数；`/new` 开新对话，`/ls`、`/switch`、`/del` 列出、切换、删除（见 9.5 的“多段对话”）。
  - 会话记忆**仅追加**、不做滑动窗口：上下文增长到模型窗口的一定比例时，才做一次缓存友好的摘要压缩（见 8.7）。
  - **会话持久化**：历史、摘要与会话记录（人设绑定、计数器、状态）统一落盘至 `./data/sessions.db`，重启节点或编辑实例后对话原地续接（见 8.8）。
- **人设 (Persona)**：人设是放在每次请求最前面的**固定文本**，由运营者在控制台「人设」页增删（`data/personas.json`）；节点只自带一个极简的基础助手（`assistant`，只读、不可删除，未选择其他人设时使用）。人设不再支持模板变量，也不再自带预设库（见 8.6）。
- **模型选择**：提供商只是端点（协议、地址、密钥）；节点只有**一个全局默认模型**（`<provider>/<model-id>`），实例可单独覆盖。不存在“默认提供商/当前生效提供商”这类第二个决定（见 9.2）。

### 8.2 模型推理通道的可见性边界 (Reasoning Visibility)

- 模型返回的原生 `reasoning_content` 与正文 `content` 分开传递、存储和回放。有独立推理字段时（包括空字符串），正文中的 `<think>` 是正文，不再按标签重新分类。流式响应采用相同规则。
- 无独立推理字段时，仅在模型边界兼容开头的一个完整标准 `<think>…</think>`；不为嵌套、连续或畸形输出增加解析兜底。正文中的标签、代码示例、单独结束标签以及 `<think >` / `</think >` 等空白变体均不作为推理块过滤。无独立字段且以标准标签开头的纯文本存在固有歧义，字面示例需引用或放进代码块。未解析成实际工具调用的标签正文原样下发。
- 该解码器与 provider 的编码器成对维护，禁止在适配器/插件内各自实现标签解析。

### 8.3 工具与"管理动作"的边界 (Tools vs. Management Actions)

- **`tools`（LLM 可见）**：注册进 `ToolMeta` 的能力会被聚合后交给模型做 function calling。**适配器插件禁止声明任何 tool**——适配器的职责是平台收发，若其把"扫码绑定/凭证轮换"等运维能力注册为 tool，模型就会在闲聊中尝试调用它们。
- **`actions`（仅控制台可见）**：运维操作通过 `PluginHostService.InvokeAction` 暴露（Python SDK 用 `@action(...)` 声明，不会出现在 `Plugin.meta()` 中）。控制台走 `POST /api/v1/plugins/{id}/actions/{action}`，因此这类能力永远不会进入模型的函数列表。
- 判定规则：**模型可以主动调用的 → tool；只能由人/控制台触发的 → action**。

### 8.4 扩展能力：MCP 服务器与技能 (MCP & Skills)

核心自身不内置任何业务工具，但允许两类**外部能力来源**与插件工具共用同一套 Tool Calling 状态机：

- **MCP 服务器**：核心内置 MCP 客户端（`stdio` 子进程与 `http` 端点两种传输），在 `tools/list` 后把每个工具注册为 `mcp__<server>__<tool>`。工具返回的图片等富媒体由核心落盘到 `data/attachments/`（单文件上限 8 MiB、单次上限 4 个），作为**附件**沿 `ToolCallResponse.attachments` 传到流水线，最终成为出站消息里的 `ImageSegment`——因此「出图」类工具（如 maimai B50）能把图片真正发到群里，而不是只把文件路径当文本交给模型；图片无法解码或超限时会把原因写回工具结果文本，绝不静默丢弃。MCP 服务器与插件宿主一样实现 `ToolHost`，因此路由、审计追踪与熔断逻辑完全复用；连接按需建立，独立看门狗每 30 秒以 `tools/list` 作为存活探针，连续失败则标记为 `failed` 并在下一轮重连。
- **技能 (Skills)**：`data/skills/<id>/SKILL.md` 中的指令包。**只有 `name` 与 `description` 进入系统提示词**（`SkillCatalogHook`），完整正文由模型通过内置 `read_skill` 工具按需读取——这样上下文开销与实际需要成正比，而不是把全部技能塞进每次请求。单个技能正文上限 64 KiB。

两类能力与插件共享**同一套开关模型**，且全局开关优先：

| 层级 | 载体 | 语义 |
| :--- | :--- | :--- |
| 全局 | `data/toggles.json` 的 `plugins` / `skills` / `mcp` 分区 | 运维总闸；停用即彻底不可用 |
| 实例 | `BotInstance` 的 `plugins` / `skills` / `mcp` 覆盖表（`inherit` / `enable` / `disable`） | 仅能进一步限制，**不能**复活被全局停用的项 |

控制台的"工具列表"页签通过 `GET /api/v1/tools` 展示三类来源的合并结果：内置工具（`read_skill` 等，直接来自 Agent 的原生工具表）、运行中插件宿主声明的工具，以及已启用 MCP 服务器通告的工具；插件与 MCP 工具经由同一个 `resolve_tools` 解析，因此控制台显示的名称与模型收到的名称由构造保证一致（含重名时的 `plugin__tool` 命名空间）。被全局关闭的 MCP 服务器不会出现在其中——该接口列出的是"此刻真的可调用"的工具。

插件可以在运行时增删自己的工具（以及命令、触发器）：变更后调用 `RefreshPluginMeta`，核心重新读取该宿主的 `GetPluginMeta`，从下一轮起生效；工具列表因此变化一次，前缀随之重新稳定。刷新后的元数据若缺少宿主此前声明过的插件，核心拒绝并保留旧元数据。

流水线在实例门禁之后立即按该策略过滤插件宿主（`PreFilter`、内置/插件命令、工具聚合因此同时生效），MCP 与技能策略则在聚合工具与构建技能目录时求值。策略通过会话键中的实例标识解析，因此共享同一 Agent 的不同实例不会串用彼此的工具与技能。

### 8.5 Tool Calling 跨语言执行状态机闭环

```mermaid
sequenceDiagram
    autonumber
    participant IM as 平台/适配器插件
    participant Core as Rust 核心 (LLM Orchestrator)
    participant LLM as 大模型 (DeepSeek/OpenAI)
    participant PyHost as Python/TS 宿主插件

    IM->>Core: 用户消息: "查询杭州今天天气"
    Core->>Core: 组装 Prompt + 聚合所有插件已注册的 Tool Definitions
    Core->>LLM: 发送 Chat Completion (带 Tools 契约)
    LLM-->>Core: 返回 ToolCall: { name: "fetch_weather", args: { "city": "杭州" } }
    Core->>Core: 状态机进入 ToolCalling 阶段，解析 tool 所属插件
    Core->>PyHost: gRPC: OnCallTool("fetch_weather", args)
    PyHost-->>Core: 返回执行结果: { "temperature": 25, "condition": "晴" }
    Core->>Core: 将 Tool 结果追加至会话历史 (role="tool")
    Core->>LLM: 再次调用 LLM，推进第二轮推理
    LLM-->>Core: 流式输出最终回答: "杭州今天天气晴朗，气温25℃..."
    Core->>IM: 出站 DeliverMessage 分发至适配器并回复用户
```

### 8.6 提示词静态→动态分层与前缀缓存 (Prompt Layout & Prefix Caching)

模型服务商按**前缀**缓存提示词：与历史请求逐 token 相同的最长前缀直接命中缓存，从第一个不同的 token 起全部重算。因此每个请求都严格按“变化频率由低到高”分层，`kanon-llm` 的 `layout` 模块是这条规则的唯一实现：

| 层 | 内容 | 何时变化 |
| :--- | :--- | :--- |
| 1. 工具 | 全部可调用工具，按名称排序，Schema 键名排序 | 插件 / MCP / 技能开关变化时 |
| 2. 系统块 | 人设提示词 + 技能目录（插件可经 `OnLlmRequest` 改写，见下；+ 会话摘要，见 8.7），合并为**单条** system 消息 | 运营者编辑配置或插件改写结果变化时 |
| 3. 历史 | 此前各轮对话，**仅追加** | 每轮追加，不修改已有内容 |
| 4. 当前轮 | 用户输入（含运营者启用的 `[时间]`/`[发送者]` 前缀） | 每次请求 |

**避免前缀抖动 (Prefix Jitter)**：语义相同但字节不同同样会让缓存失效，因此：
- 工具列表顺序固定（按名称排序），**禁止**随注册顺序、宿主启动顺序或调用频率重排；工具 JSON Schema 递归按键名排序（即使依赖树启用了 `serde_json` 的 `preserve_order`，或 Schema 来自 protobuf `Struct` 的哈希顺序，输出字节也不变）；
- 字段结构不随轮次增减：某个配置下，字段要么始终存在、要么始终缺省，不会一轮传 `null`、一轮省略键；
- 人设提示词是**纯静态文本**（保存时统一换行并去除首尾空白），不再支持 `{{变量}}` 模板；时间、发送者等运行时才知道的信息只出现在当前轮用户消息里；
- system 文本逐段 trim 后以固定分隔符 `\n\n` 合并，钩子数量变化不会改变提示词形状；若有钩子在对话开始后再插入 system 消息（会切断缓存前缀），系统记录告警。

**插件改写系统提示**：声明 `rewrites_system_prompt` 的插件可通过 `OnLlmRequest` 改写第 2 层。为保持前缀稳定：
- 每轮只在首个模型请求前按宿主优先级串行询问一次，结果按会话记住（`PluginAgentHook`，最多记住最近 256 个会话）；同一轮的工具轮次与随后的后台压缩复用这次结果，因此一轮的全部请求与其摘要请求共享同一前缀。被遗忘的结果只会让一次压缩少命中缓存，绝不会用错提示。
- 记住的结果绑定插件看到的原提示（哈希）：运维修改人设或技能后，旧改写作废并重新询问，不会掩盖修改；不再有插件改写时立即遗忘，压缩不会重新带出旧改写。
- 改写结果替换开头的全部 system 消息成为一条，摘要始终插在其后（`prompt_layout_test.rs` 验证）；插件出错、超时（3 秒）或返回空串时保留原提示。插件必须对同一会话给出确定的结果，按消息变化的上下文应走 `OnPrepareTurn` 进入当前轮。

**服务商适配**：OpenAI Chat / Responses 与 DeepSeek 等按前缀自动缓存，稳定前缀即可命中；Anthropic 需要显式断点，`AnthropicMessagesProvider` 在工具列表末尾、system 块、对话最后一个内容块各打一个 `cache_control: ephemeral` 断点（流式与非流式共用同一请求构造器，保证两者布局一致）。

**可观测性**：各服务商返回的缓存命中 token 统一进入 `TokenUsage.cached_tokens`（OpenAI `prompt_tokens_details.cached_tokens`、DeepSeek `prompt_cache_hit_tokens`、Responses `input_tokens_details.cached_tokens`、Anthropic `cache_read_input_tokens`；Anthropic 的 `prompt_tokens` 为 `input + cache_read + cache_creation` 之和）。`/api/v1/metrics` 导出 `kanon_llm_prompt_tokens_total` 与 `kanon_llm_cached_prompt_tokens_total`，二者之比即缓存命中率；`llm_response` 追踪事件同时携带 `prompt_tokens` / `cached_tokens`。

### 8.7 会话记忆：仅追加与缓存友好的压缩 (Append-only Memory & Cache-safe Compaction)

**为什么不用滑动窗口**：每轮丢弃最旧一条消息会让其后所有内容的绝对偏移和前缀全部改变，服务商缓存整段历史全部失效、每轮按首次读取计费。因此记忆层（`Memory` trait；节点用 `SqliteMemory`，嵌入式与测试可用 `InMemory`）**只追加**：除 `compact_history` 与 `clear` 外，任何操作都不得删除或重排消息（`clear` 清空整段会话，用于重置与 `/del` 删除对话，见 8.8）。会话历史里只有用户 / 助手 / 工具消息；人设与技能目录每次请求时组装进系统块，不按会话存储（避免系统提示词出现第二个数据源）。摘要与历史并列保存（`MemorySnapshot { summary, messages }` 一次性读取，读者不会看到某次压缩的摘要配上另一次的历史）。

**何时压缩**：一轮回复完成后，若上下文 token（优先取服务商返回的 `prompt_tokens + completion_tokens`，缺失时用估算）达到模型上下文窗口的 70%（窗口取自模型目录，未知时按 32,768 估计；`CompactionPolicy` 可调），则压缩一次；历史少于 4 条、或未停在完整的助手回复上（等待回答的用户消息、工具循环中途）时不压缩。压缩在后台执行，用户不会因此等待。

**怎么压缩（两步都吃缓存）**：
1. **同前缀摘要**：摘要请求就是该会话自己的请求——工具、系统块、历史逐字节相同——只在**最末尾**追加一条“请压缩上述对话”的用户消息。服务商从刚写入的缓存里读取前面全部内容，摘要只需为自己的输出付费；选择在回复刚结束时执行，正是因为此时缓存最热。摘要必须是纯文本：模型返回空内容或请求调用工具都视为失败，历史保持原样，绝不在没有摘要的情况下丢弃对话。
2. **在新前缀里挂载摘要**：被覆盖的前 N 条消息替换为摘要，摘要进入静态系统块（人设 → 技能目录 → 摘要）。前缀只在这一刻变化一次（一次缓存未命中），此后重新稳定追加，直到下一次压缩。

**并发语义**：压缩读取快照后要花数秒等待模型，期间会话照常进行，所以 `compact_history(covered, summary)` **只移除快照覆盖的前 `covered` 条**，之后追加的消息原样保留；同一会话同一时间只有一个压缩任务（同会话再次触发直接跳过）。SQLite 后端在一个事务内删除前缀并写入摘要，失败不丢历史；旧库（含 `system_prompt` 列）自动补 `summary` 列后继续使用。

**边界**：节点使用 `SqliteMemory`（`data/sessions.db`，见 8.8）；嵌入式场景与测试可换用进程内 `InMemory`。流式聊天（沙盒 `text/event-stream`）不触发自动压缩，`Agent::compact_session` 可手动触发。

### 8.8 会话持久化与续接 (Durable Sessions)

一段对话由两部分组成，重启后要原地续接，两部分都必须落盘：

| 部分 | 内容 | 所有者 | 落盘 |
| :--- | :--- | :--- | :--- |
| 历史 | 用户 / 助手 / 工具消息与压缩摘要 | `Memory`（`SqliteMemory`） | `data/sessions.db` 的 `messages` / `sessions` 表，追加与压缩均在事务内 |
| 会话记录 | 人设绑定、变量、轮次与 Token 计数、状态、作用域 | `SessionManager` | 同一数据库的 `session_meta` 表（每会话一份 JSON），**写穿 (write-through)**：每次变更立即落盘 |

会话键由“实例 + 会话 + 代数”确定，而实例目录（`data/instances.json`）与各会话的当前代数（`/new`、`/switch`、`/del` 改变它）同样持久化——因此**重启节点后**，或**编辑实例后**（改名、改策略、改适配器；`update` 保留实例标识与代数），下一条消息落在同一个会话里，模型看到的仍是原有历史、摘要与人设。控制台的会话列表在重启后立即可见。

- **写入开销**：写穿只在记录真正变化时发生（流水线每条消息都会重复绑定实例人设，绑定不变时不写库），且在 `DashMap` 分片锁释放后进行，不阻塞其他会话。
- **失败语义**：会话记录写入失败会以 `error` 级别记录日志，但不会把已生成的回复变成错误；历史写入失败则照常向上报错（缓存绝不领先于数据库）。数据库无法打开或读取（文件损坏、目录不可写）时，节点**启动失败**而不是静默退化为空的内存会话表——那会让机器人看起来忘了所有对话。
- **重置与删除人设**：`/sessions/{id}/reset` 同时清空历史与摘要并持久化计数归零，保留人设与变量；删除人设会解绑并持久化所有使用它的会话。
- **删除对话**：`/del` 与 `DeleteConversation` 经 `SessionManager::delete_session` 先删历史与摘要、再删会话记录；记录删除失败时返回错误并保留记录（内存与磁盘一致），重试即可完成。这是压缩之外唯一删除消息的途径，删除的是整段对话，留下的对话不会出现缺口。
- **图片只属于当前轮**：入站图片由节点在模型调用前自行下载（单张上限 10 MiB、15 秒超时），以内联数据交给模型服务商，平台签名 URL（如 QQ 的 `rkey`）过期或服务商访问不到 CDN 都不再让整轮请求被拒；下载失败的图片不发送，并在当前轮文本中写明原因，模型照常回答文字部分。图片只随它所在那一轮的请求发送，历史只保存文本投影（`[图片]`），`messages.parts` 列不再读写（旧版本写入的图片 URL 一并忽略）——否则一个过期 URL 会让该会话之后的每一轮都失败。代价是下一轮请求在这条消息处少命中一次缓存，以及后续轮次看不到早先的图片。

---

## 9. 管理控制面与前后端分离 API 规范 (Management Gateway Spec)

### 9.1 解耦部署架构
为保持 Rust 核心代码库的专精与纯粹，系统采用 **前后端解耦部署 (Decoupled Services)** 架构：
- **Rust Core (Backend)**：纯粹的节点进程（Headless Engine，二进制名 `kanon`），专注于协议接入、事件调度、高频 IPC 与安全审计。通过 Axum 暴露轻量高性能的 OpenAPI/RESTful 接口与 WebSocket 实时推送信道。
- **WebUI (Frontend)**：作为独立的前端工程单独维护、构建与分发，用户可通过 Docker Compose、Vercel 或静态托管一键部署。

### 9.2 核心 RESTful 端点定义

> 全部端点由 `crates/kanon-api` 实现（Axum Router，路径参数采用 Axum 0.7 `:id` 语法，文档统一写作 `{id}`）。
> 统一错误信封：`{"error": {"code": "...", "message": "..."}}`，状态码语义为
> `400` 契约违规 / `404` 资源不存在 / `409` 当前状态下不可执行 / `413` 载荷过大 / `503` 依赖未配置或宿主未运行 / `502` 插件宿主机或模型网关失败 / `504` 插件未在时限内应答。

| Method | Endpoint | Description |
| :--- | :--- | :--- |
| `GET` | `/api/v1/health` | 核心健康状态与基础运行指标 (Memory, Uptime, 插件与会话计数) |
| `GET` | `/api/v1/plugins` | 查询插件清单、运行状态与静态元数据；目录部分来自最近一次扫描（启动时或手动重新扫描），读取清单本身不扫描磁盘 |
| `POST` | `/api/v1/plugins/rescan` | 重新扫描 `./plugins` 并返回刷新后的清单；手动放进目录的插件只有经过这一步才会被节点识别 |
| `GET` | `/api/v1/plugins/{id}` | 查询单个插件（字段同列表项：运行状态、清单元数据、`has_pages`、`serves_http`、翻译 `i18n` 及翻译文件问题 `i18n_errors`） |
| `POST` | `/api/v1/plugins/install` | 安装插件，来源四选一：节点上的目录 `path`、上传的 `.kpk`/`.zip`（multipart）、包链接 `url`（仅 `https`，`http` 只允许回环地址）、Git 仓库 `git`（可选 `ref`）。统一走同一安装器：校验清单与 `kanon_version` → 在 `./plugins` 内暂存 → 停止旧宿主 → 原子替换 → 拉起。已安装同一 `id` 时返回 `409`，需显式 `"replace": true` 才替换；已安装插件保留原目录（手动复制、目录名与 `id` 不同者亦然） |
| `GET` | `/api/v1/plugins/market` | 读取 `data/system.json` 中 `plugin_market.indexes` 列出的市场索引，合并后标注已安装版本与 `kanon_version` 兼容性；未配置即返回 `configured: false`，节点不会主动访问任何第三方 |
| `PUT` | `/api/v1/plugins/{id}/enabled` | 启用（拉起宿主）或停用（停止宿主并退出路由）插件 |
| `GET` | `/api/v1/plugins/{id}/pages/{path}` | 只读提供插件 `pages/` 目录下的静态页面（目录只返回 `index.html`，拒绝隐藏文件、路径穿越与指出目录的符号链接） |
| `ANY` | `/api/v1/plugins/{id}/http/{path}` | 转发给声明了 `serves_http` 的插件的 `OnHttpRequest`：请求体 ≤ 3 MiB，30 秒超时，剔除逐跳头。未知插件或未声明 → `404`，宿主未运行 → `503`，宿主出错 → `502`，超时 → `504`，过大 → `413` |
| `GET` | `/api/v1/plugins/{id}/config` | 获取指定插件的配置项当前值、JSON Schema 及当前单调递增版本号 `version` |
| `PUT` | `/api/v1/plugins/{id}/config` | 校验配置 → 检查 CAS 乐观锁版本向量 → 触发跨进程热重载 → 原子持久化（版本冲突返回 409，宿主拒绝则不落盘） |
| `POST` | `/api/v1/plugins/{id}/restart` | 重启指定插件所在的宿主进程；插件已启用但无宿主运行（崩溃或启动失败）时按磁盘上的清单重新启动 |
| `POST` | `/api/v1/plugins/{id}/actions/{action}` | 触发插件的**管理动作**（运维操作，永不进入模型的函数列表；区别于 `tools`） |
| `POST` | `/api/v1/plugins/{id}/tools/{tool}` | 以 JSON 参数直接调用插件声明的工具（调试用） |
| `GET` | `/api/v1/tools` | 列出模型当前可调用的**全部工具**及其提供方（内置 / 插件 / MCP），名称与分发规则和模型实际收到的完全一致 |
| `GET` | `/api/v1/sessions` | 分页查询会话记录（Turn 计数、Token 消耗、活跃时间、Persona、作用域）；记录持久化，节点重启后仍可见 |
| `POST` | `/api/v1/sessions/{id}/reset` | 安全重置会话历史，保留配置变量与人设 |
| `POST` | `/api/v1/sessions/{id}/persona` | 动态热切换指定会话的生效人设；`persona_id` 为空/`null` 即解除绑定（使用基础助手） |
| `GET` | `/api/v1/personas` | 查询人设库：内置基础助手（`builtin`，只读）、运营者自建人设（`custom`）与实例自带提示词生成的人设（`instance`），并标注被哪些实例引用 |
| `POST` | `/api/v1/personas` | 新建人设（`name` + `prompt` + 可选 `description`；`id` 缺省时由名称派生并保证唯一）：校验 → 持久化至 `data/personas.json` → 热应用 |
| `PUT` | `/api/v1/personas/{id}` | 编辑自建人设（`id` 不变，引用不受影响）；内置与实例人设返回 `409` |
| `DELETE` | `/api/v1/personas/{id}` | 删除自建人设；仍被实例选用时返回 `409` 并列出实例，绑定该人设的会话自动解绑（回到基础助手） |
| `GET` | `/api/v1/providers` | 查询已配置的提供商端点（不含密钥）、可用协议与预设；提供商只是端点，没有“默认/当前生效”之分 |
| `POST` | `/api/v1/providers` | 新增或替换一个命名提供商端点（校验 → 持久化至 `data/system.json` → 热应用）；省略密钥即保留已存密钥 |
| `POST` | `/api/v1/providers/delete` | 删除提供商及其模型目录；若它正是全局默认模型的提供商，则同时清空默认模型 |
| `POST` | `/api/v1/providers/test` | 以提供商名称探测连通性：服务端使用已存密钥，浏览器永远拿不到密钥 |
| `GET` | `/api/v1/models` | 查询按 `provider/model-id` 索引的模型目录，并返回**全局默认模型** |
| `PUT` | `/api/v1/models/default` | 设置（或置空）唯一的全局默认模型：校验其提供商已配置 → 持久化 → 热应用（流水线、`RequestLLM`、聊天接口下一请求即生效，无需重启） |
| `GET` | `/api/v1/metrics` | 导出 Prometheus 格式的系统与消息吞吐指标 |
| `POST` | `/api/v1/chat/completions` | 在线沙盒对话调试，支持标准 JSON 与 `text/event-stream` 流式输出 |
| `GET` | `/api/v1/adapters` | 查询已注册的平台适配器（内置 + 插件声明）及其连接存活状态与断路器状态 (`circuit_state`) |
| `POST` | `/api/v1/adapters/{platform}/ingest` | 平台入站数据面：Fast-ACK 接收外部消息并推入核心流水线 |
| `GET` | `/api/v1/skills` | 查询已安装技能及其全局开关状态 |
| `POST` | `/api/v1/skills` | 安装技能（上传 zip 压缩包或指定本地目录，目录内必须含 `SKILL.md`） |
| `DELETE` | `/api/v1/skills/{id}` | 卸载技能 |
| `PUT` | `/api/v1/skills/{id}/enabled` | 全局启用/停用技能（实例级覆盖见 `/api/v1/instances`） |
| `GET` | `/api/v1/mcp/servers` | 查询已配置的 MCP 服务器及其连接健康度与工具数量 |
| `PUT` | `/api/v1/mcp/servers/{id}` | 新增或替换 MCP 服务器定义（`stdio` 子进程或 `http` 端点），保存后立即同步连接池 |
| `DELETE` | `/api/v1/mcp/servers/{id}` | 删除 MCP 服务器定义并断开连接 |
| `PUT` | `/api/v1/mcp/servers/{id}/enabled` | 全局启用/停用 MCP 服务器（停用即刻断开，释放子进程或连接） |

**插件内容隔离**：`pages/` 与 `http/` 都由管理网关自身的源提供，因此两者的每个响应都带 `Content-Security-Policy: sandbox allow-scripts allow-forms allow-popups allow-modals allow-downloads` 与 `X-Content-Type-Options: nosniff`。即使用户在新标签页直接打开，插件内容也运行在不透明源中，无法读取控制台的存储或调用控制台的接口；控制台内嵌页面的 `<iframe>` 同样不授予 `allow-same-origin`。插件页面以相对路径（`../http/...`）访问自己的 HTTP 路由。

### 9.3 实时数据流 (WebSocket)

- **实时日志流**：`ws://host:port/ws/v1/logs` —— 采用结构化 JSON 实时回传主核心及各子进程的标准输出日志（支持按 log level、plugin_id 过滤）。
- **事件追踪总线**：`ws://host:port/ws/v1/events` —— 用于控制台实时可视化展示消息到达、PreFilter 状态、LLM Tool Calling 过程及最终出站全链路追踪。
- **过滤协商**：两条信道均支持查询参数（`level` / `plugin_id` / `session_id` / `kind`）与运行时 `{"type":"filter",...}` 控制帧；
  连接建立后先回送 `ready` 帧回显生效过滤器，订阅端滞后于广播缓冲时显式推送 `{"type":"lagged","skipped":N}`，绝不静默丢弃。
- **事件分层**：流水线事件以 `kind="pipeline"` 承载，并附带细粒度 `stage` 字段
  （`ingested` → `pre_filter_started` → `pre_filter_passed` / `pre_filter_blocked` → `command_matched` → `llm_replied` → `outbound_queued`），
  控制台既可订阅 `kind=pipeline` 观察全链路，也可按 `kind=ingested` 等单阶段精确过滤；LLM 与 Tool Calling 阶段由 `EventBus` 本身作为
  `AgentHook` 注入 Agent，与流水线阶段共用同一条有序事件流。

### 9.4 节点运行形态 (`kanon` 二进制)

`crates/kanon` 是全工程唯一的节点入口，产出 `kanon` 可执行文件：作为组合根，它在同一进程内启动
`core.sock` IPC 服务、流水线工作循环、Supervisor 与 Axum 管理网关，并将可观测性中心同时接入 `tracing` 与控制台广播信道。
微内核（`crates/kanon-core`）与网关（`crates/kanon-api`）在此仅以库形式被装配 —— 二者自身不再提供任何可执行文件，
整个工程的可执行产物只有 `kanon` 与开发者 CLI `kanon-dev`。

节点不读取任何环境变量，全部配置都在 `data/system.json`：提供商、模型、策略与适配器由控制台编辑；`startup` 小节由运维手工编辑、下次启动生效，控制台从不写入。所有键都可省略，出现未知键时启动直接失败而不是被忽略。

| `startup` 键 | 默认值 | 说明 |
| :--- | :--- | :--- |
| `api_addr` | `127.0.0.1:8080` | 管理网关监听地址（默认仅回环，避免误暴露） |
| `log` | `info` | 标准 `tracing` 过滤指令 |
| `run_dir` | 平台运行时目录 | IPC 套接字（`core.sock`、`host_<id>.sock`）所在目录 |
| `typescript_runtime` | 依次在 `PATH` 中查找 `bun`、`node` | TypeScript 插件的解释器 |
| `install_dependencies` | `true` | 启动 Python / TypeScript 插件前用原生工具安装其依赖（见 2.5）；`false` 时由运维自行安装 |

需要预置配置的部署（容器镜像、CI）直接随附一份准备好的 `data/system.json`。Supervisor 向宿主子进程注入的 `KANON_HOST_ID` / `KANON_HOST_SOCK` / `KANON_CORE_SOCK`（以及 Windows 上的 `KANON_IPC_TOKEN`）属于进程启动契约，不是运维配置。

节点的运行时状态都在工作目录下的 `./data/`：

| 文件 | 内容 |
| :--- | :--- |
| `system.json` | 提供商端点（含密钥，权限 `0600`）、模型目录、**全局默认模型**、回复/上下文/事件策略、适配器配置 |
| `instances.json` | Bot 实例目录（适配器归属、人设/模型/策略覆盖、各会话的当前对话代数） |
| `personas.json` | 运营者自建人设（基础助手内置，不落盘） |
| `sessions.db` | 对话历史、压缩摘要与会话记录（SQLite WAL，见 8.8）；启动时无法打开即启动失败 |
| `kv.db` | 插件的中心 KV 存储（SQLite WAL，见第 7 节）；启动时无法打开即启动失败 |
| `toggles.json`、`mcp.json`、`skills/`、`attachments/`、`dead_letter/` | 插件/技能/MCP 开关、MCP 服务器定义、已安装技能、工具附件、出站死信 |
| `plugins/<id>/` | 各插件的专属数据目录 |

以上 JSON 文档均采用「临时文件 + `rename`」原子提交。插件配置持久化位置遵循数据隔离规范：`./data/plugins/<plugin_id>/config.json`，
写入顺序为 **校验 → 宿主热重载确认 → 落盘**，宿主拒绝时不会留下半更新配置。

**CAS 乐观并发控制与单调版本向量 (CAS Optimistic Concurrency Control)**：
- 为彻底消灭并发修改或多控制台重叠提交引发的「配置时序倒退」与「静默脏写」隐患，管理控制面引入严格的 CAS 版本向量机制；
- `GET /api/v1/plugins/{id}/config` 返回当前配置值、JSON Schema 及单调递增版本号 `version`；
- `PUT /api/v1/plugins/{id}/config` 载荷支持携带可选的 `expected_version`。Supervisor 内存中维护各插件当前的最新单调递增版本向量：
  - 若调用方传入的 `expected_version` 与服务端当前维护的版本号不一致（或并发重载产生竞态），核心立即终止操作并返回 HTTP `409 Conflict` (`stale_config_version`)，杜绝旧配置覆盖新配置；
  - 校验通过后，Supervisor 递增版本号并将新版本携带在 `ReloadPluginConfigRequest.version` 中下发至宿主；
  - 跨语言插件宿主（Rust、Python、TypeScript）校验版本单调递增性，应用成功后在 `ReloadPluginConfigResponse.applied_version` 回传已应用的配置版本，实现跨进程配置时序强一致性。

### 9.5 平台适配器契约 (Platform Adapter Contract)

微内核自身不实现任何 IM 协议：**平台适配器**独占一个平台标识（与 `PipelineEventRequest.platform` 同值），负责两个方向：

1. **入站 (Inbound)**：把平台消息经 `EventIngress` 非阻塞推入核心 → 保持 Fast-ACK 语义（队列满时返回 `accepted=false` 或 HTTP `503`，绝不阻塞平台回调）。
2. **出站 (Outbound)**：把 `DeliverMessageRequest` 真正投递到平台 API，失败必须显式上报。

两条实现路径共用同一契约与同一条出站路由：

| 路径 | 注册方式 | 出站投递 | 入站 |
| :--- | :--- | :--- | :--- |
| **内置 (Built-in)** | 进程内实现 `PlatformAdapter` 并注册到 `AdapterRegistry`（内置优先于插件，是运维的显式覆盖） | 直接调用适配器 `deliver()`，零 IPC 开销 | 由适配器自行推送（HTTP 网关路由或自建长轮询任务） |
| **插件 (Plugin)** | `plugin.toml` 声明 `[adapter] platform = "..."`，核心从静态清单发现，**无需注册调用** | `MessagePipelineService.OnDeliverMessage` RPC 投递到宿主进程 | 插件经 `BotApiService.IngestEvent` 推回；Rust/Python/TS SDK 的 `ctx.core.ingest_event(...)` 已封装 |

**出站调度与背压**：流水线工作循环绝不等待平台 I/O。回复统一进入全局有界出站队列后，由 Dispatcher 按 `platform` 分区路由至独立的单平台 Worker 队列（默认容量 64）：
- **平台并发隔离**：不同平台间并发执行，单一卡顿或故障平台绝不阻塞其他平台的出站吞吐量（避免跨平台队头阻塞）；
- **平台内时序保障**：同一平台内部由独立 Worker 串行消费，严格保证 FIFO 消息递送顺序；
- **背压与死信追踪**：单平台队列饱和时，溢出消息立即丢弃并自动持久化归档至死信日志，同时上报 `outbound_failed` 阶段，不静默膨胀内存。

**出站单平台故障熔断器 (Circuit Breaker)**：
若目标平台 API 出现长时间物理级宕机（例如网络切断或服务崩溃持续数小时），每条出站消息重试 2 次会造成单平台队列持续占满，队列持续溢出并对同一故障地址发起无意义的重试请求。为此系统补齐单平台独立熔断机制：
- **短路阈值**：当特定平台的出站调用连续失败达到 **5 次**时，该平台的 Dispatcher Worker 自动进入 `CircuitOpen` 状态。
- **短路行为**：熔断期间后续消息直接丢弃或落盘到死信存储并上报 `outbound_failed`，不再触发实际网络 I/O，并以 30 秒为周期进入 `Half-Open` 状态发送单条试探探测，探测成功后恢复，防止阻塞 worker 协程与连接句柄。
- **控制面状态透出**：`GET /api/v1/adapters` 实时输出各个适配器的熔断器健康状态（`circuit_state: "closed" | "open" | "half_open"`），使运维能够实时监控各平台连接质量与故障熔断状态。

**出站死信队列与持久化归档 (Outbound Dead-Letter Queue & Cold Persistence)**：
- 因平台出站队列饱和背压丢弃、网络重试耗尽失败，或因断路器 `Open` 极速短路拦截的所有出站消息，均受核心死信引擎（`DeadLetterWriter`）全程护航，绝不静默丢失；
- 死信日志按平台标识与 UTC 自然日期进行目录隔离与文件分片持久化，归档路径为：
  `./data/dead_letter/<platform>_<YYYY-MM-DD>.jsonl`；
- 每条死信记录包含 `event_id`（全链路追踪关联 ID）、`platform`、`target_id`、`channel_id`、`reason`（失败原因/熔断说明）、`timestamp_millis`（毫秒时间戳）以及结构化的 `segments` 消息段载荷，为不可逆投递失败提供完整的审计追溯与运维离线补发对账能力。

**通用入站鉴权**：`POST /api/v1/adapters/{platform}/ingest` 对所有已注册适配器开放（内置或插件），并在入站前调用该适配器的
`verify_inbound`（默认放行）；实现签名校验的适配器可据此在进入核心管道之前拒绝伪造载荷，失败以 HTTP 401 显式返回。

**插件侧能力**：声明 `[adapter]` 但未实现出站钩子的插件会收到明确的失败响应（`success=false` + 原因），
核心据此记录投递失败 —— 契约不允许「假成功」。三语言 SDK 的默认 `on_deliver_message` / `onDeliverMessage` 均已改为显式拒绝。

**通知、合并转发与回复体验**（适配器只报告事实，措辞与策略归核心）：
- **通知事件**：进群、机器人被拉群/加好友、戳一戳、撤回以普通 `PipelineEventRequest` 入站，元数据带 `kanon.notice`（`member_join` / `bot_join` / `friend_add` / `poke` / `recall`）、可选 `kanon.notice_actor`（显示名）与 `kanon.notice_target`（被撤回消息入站时的 `event_id`）。是否回应由节点级事件策略（`system.json` 的 `event_policy`，`/api/v1/system/event-policy`）决定；被启用的通知跳过指令与回复策略，以一行 `[事件] …` 交给模型。
- **撤回提示**：只有模型看过的消息被撤回时，才在该会话下一轮的当前用户消息前加 `[通知] …`；模型没看过的内容绝不因撤回而被透露。提示只进入当前轮，不改变请求前缀。
- **合并转发**：适配器取回内容写入转发段载荷 `messages: [{sender, text, images}]`；上下文策略 `expand_forward`（默认开）决定是否逐条展开并为识图模型附带图片（有条数与图片上限）。
- **引用回复**：回复策略 `quote_message` 开启时，群聊/频道中的模型回复首段为指向原事件的 `Reply` 段，由各适配器转换为原生引用。
- **思考内容**：回复策略 `send_reasoning`（默认关闭）只控制独立推理通道是否作为文本段置于回答之前；正文中的字面标签保持不变。提供商设置 `replay_reasoning` 单独控制 API 回传，关闭不删除历史，也不阻止新推理入库。
- **工具产出的媒体**：工具结果中的附件按 MIME 类型发送为对应的段——`image/*` 为图片、`audio/*` 为语音、`video/*` 为视频，其余一律为文件，文件名即附件文件自身的名字。核心只发送目标适配器声明了 `send_image` / `send_voice` / `send_video` / `send_file` 的类型；不支持的附件不进入消息，而是在回复末尾附一行说明，其余内容照常投递。一个媒体能力都没声明的适配器插件（多为媒体能力出现前写的清单）在加载时记录警告，`kanon-dev lint` 也会提示。MCP 工具的 `image` / `audio` 内容与带 `blob` 的嵌入资源都会成为附件，资源以其 URI 末段命名；原生工具通过 `ToolOutput.attachments` 交出附件，规则相同（仅成功的调用、按执行顺序去重）。
- **QQ 官方的媒体**：每个附件单次上传，上限 20 MiB（更大的需分片上传，未实现，超限即明确报错）。语音接口只播放 SILK、WAV、MP3：这三种原样上传；FLAC、Ogg Vorbis、AAC/M4A、ALAC、AIFF、CAF 由适配器用纯 Rust 解码（symphonia）并转为 24 kHz 单声道 16 位 WAV，节点运行时不需要 ffmpeg 或 SILK 库；Opus、AMR 无法转换，投递失败并说明可用格式。语音 URL 由适配器下载后再判断格式，而不是交给 QQ 拉取。
- **处理中反馈**：回复策略 `acknowledge` 开启时，流水线决定用模型回答后非阻塞调用内置适配器的 `acknowledge()`（默认无操作）；QQ 官方私聊显示「正在输入」，Milky 群聊对原消息点赞。
- **好友申请与入群邀请**：适配器以 `friend_request` / `group_invite` 通知入站，并在 `kanon.request_token` 中放入仅自己能解读的凭据；事件策略的 `accept_friend_requests` / `accept_group_invites` 开启时，核心调用该适配器的 `accept_request()`。

**适配器能力声明 (Capabilities)**：凡依赖平台差异的功能都走上面的通用契约（元数据键、回复段、`acknowledge()`、`accept_request()`），每个适配器用 `Capability` 声明自己实现了哪些：`sender_name`、`sender_role`、`group_messages`、`quote_reply`、`forward_content`、`acknowledge`、`member_join`、`bot_join`、`friend_add`、`poke`、`recall`、`friend_requests`、`group_invites`、`platform_api`、`send_image`、`send_voice`、`send_video`、`send_file`。内置适配器实现 `PlatformAdapter::capabilities()`，插件在 `plugin.toml` 的 `[adapter] capabilities` 中声明（需要回调的 `acknowledge` / `friend_requests` / `group_invites` 仅内置适配器可用，插件声明即清单错误）。`GET /api/v1/adapters` 返回每个适配器的能力，控制台在每个相关设置旁列出支持它的适配器——新适配器只需如实声明，无需改动控制台。

**命令权限**：节点级 `command_policy`（`/api/v1/system/command-policy`）列出管理员（`<平台>:<用户 ID>`），可选把群主/群管理员（`kanon.sender_role`）视为管理员，并按命令名设定 `everyone` / `admins_in_groups` / `admins`；默认 `/new` 为「群聊仅管理员」、`/model` 为「仅管理员」；`/stop` 未列出时也是「仅管理员」，`/switch`、`/del` 未列出时为「群聊仅管理员」（由核心声明，与插件命令的默认值同理，旧配置里没有这一项时同样生效；共享会话的群里切换或删除对话会影响所有成员），`/ls` 只读，所有人可用，未列出的命令（含插件命令）所有人可用。被拒绝时回复发送者 ID，方便运维加入管理员列表。实例可设置自己的 `command_policy` 覆盖（为 `null` 时继承节点策略），覆盖时整体替换节点策略，包括管理员列表。

**`/stop`**：停止当前实例所有正在运行的模型轮次，用于模型反复调用工具、提供商迟迟不返回或命令长时间不结束时。流水线逐条串行处理消息，所以工作循环在等待当前轮次时继续预读入站队列（至多一个队列容量）：`/stop` 立即处理，其他消息按原顺序排队。轮次只在等待模型或工具时停止：正在等待的工具调用被放弃（Bash 进程随之终止，插件工具的 RPC 被取消），本轮已记录但还没有结果的工具调用补上“已停止”的结果——提供商拒绝缺少结果的工具调用，这样会话之后的请求仍然有效。被停止的轮次不发送回复，发送者收到“已停止 N 个正在运行的任务。”。控制台聊天接口的轮次不受 `/stop` 影响。

**多段对话（`/ls`、`/switch`、`/del`）**：同一会话的各段对话即各个代数的会话（`instance:<id>:<会话>#<代数>`），列表由 `data/sessions.db` 中带该前缀的历史与会话记录合成，按代数从旧到新编号，当前对话即使还没有消息也会列出。`/ls` 显示序号、标题（第一条用户消息，去掉核心加在前面的 `[发送者: …]` 等标签，截到 24 字）、消息数与最后一轮时间；`/switch <序号>` 改变实例记录的当前代数，下一条消息续写那段对话的历史与摘要；`/del [序号]` 删除一段对话（缺省为当前对话），历史、摘要与会话记录一起删除，删除当前对话后自动开新对话。`/new` 与删除当前对话都取“已有最大代数 + 1”，所以切回旧对话后再 `/new` 不会落进另一段已有的对话。插件通过 `ListConversations`、`NewConversation`、`SwitchConversation`、`DeleteConversation`、`AppendConversation` 操作同一组对话，会话定位规则与模型阶段完全一致（见 `docs/PLUGIN_API.md`）。

**同一会话的写入互斥**：模型轮次边运行边写入历史（用户消息、工具调用、工具结果、回答逐条追加），其他写入者插在中间会打乱下一次请求所依赖的“用户 → 助手”顺序。因此每个会话有一把写锁：流水线的轮次运行期间持有它（流水线串行处理事件，轮次之间不会互相等待）；删除对话、插件追加轮次等外部写入只在能立即拿到锁时进行，否则返回“会话正在运行任务”（RPC 为 `FAILED_PRECONDITION`）——外部写入者从不排队等待，插件工具在轮次内部调用这些 RPC 也不会死锁。

**模型轮次失败**：提供商返回错误、超时、无法连接或插件工具调用失败时，失败的轮次**从不重跑**——每轮的工具回合有上限（`max_iterations`，默认 5），每次模型请求最长 5 分钟（连接 15 秒；请求不是流式的，模型在一次工具调用里写长程序需要几分钟，更短的上限会让同一请求每次都超时），核心没有任何自动重试。失败或被 `/stop` 停止的轮次在历史中**被关闭**：没有结果的工具调用补上“未完成”的结果，再追加一条说明“本轮失败/被停止、未重试”的助手消息——否则历史以未回答的请求结尾，下一条消息的模型会重新去做那件失败过的工作，并以同样的方式再次失败，形成循环。若有人在等回复（私聊，或群里 @ 了机器人），发送者收到一条提示：只说明原因类别（如“模型服务返回错误（HTTP 400）”，不附提供商的原始错误），并说明不会自动重试。机器人自己发起的轮次（“总是回复”或按概率抽样的群消息、通知事件）失败时只记日志，避免提供商故障期间刷屏。

**Bash 工具**：`bash_policy.enabled` 打开后，仅所属实例生效命令权限（实例覆盖或节点策略）中 `admins` 按 ID 显式列出的管理员可用，群主/群管理员不算。调用者（`BashCaller`：发送者、实例、是否共享上下文）取自适配器的原始事件；通知与控制台聊天没有调用者。实例级 `bash` 决定可用范围：`disabled`、`own_context`（默认，仅私聊和未旁听的按人会话）、`shared_context`（全群共享会话与旁听群也可用——其他成员的消息会进入上下文，需运维显式开启）。可用性提示、首次校验以及排队/审查后的复核都经过同一个 `Gate::check`，拒绝时会告诉模型具体是哪项设置。执行后端（持久化容器，或本机加可选 AI 审查）只由运维选择。

**`send_file` 工具**：与 Bash 同时注册、经过同一个 `Gate::check`，把 Bash 工作目录（容器内为 `/workspace`）中的一个文件附到本轮回复上，媒体类型按扩展名判断。路径只接受工作目录内的相对路径（或容器视角的 `/workspace/...`），拒绝 `..`；Unix 上逐级 `openat(O_NOFOLLOW)` 打开，任何一级是符号链接都拒绝，最后一级加 `O_NONBLOCK` 并要求是普通文件——沙箱里的代码可以在挂载的工作目录中放置指向宿主文件的链接，"先检查路径再打开"会留下被替换的窗口。打开后的文件复制到 `data/attachments/`（上限 20 MiB）再交给投递，送出的就是检查过的那份字节，之后由启动时的清扫回收。

**群聊上下文**（实例级）：`session_scope` 为 `user`（默认，群内每人一个会话）或 `group`（全群共享一个会话，每条消息以 `kanon.sender_name` 标注说话人）；`observe_group` 开启时，未被回复的群消息与机器人自己的回复进入有界缓冲（30 条 / 30 分钟），在该会话下次被回答时作为 `[群聊记录]` 放在当前轮开头，并按会话记录已读位置——每行只进入一次历史，请求前缀保持仅追加（`group_context_test` 验证）。旁听依赖 `group_messages` 能力。

---

## 10. 开发者工具链与 CLI 规范 (`kanon-dev` Toolchain Spec)

系统提供统一轻量级的命令行工程与插件管理工具 **`kanon-dev`**（由 `crates/kanon-dev` 编译），赋能 Rust、Python、TypeScript 插件全生命周期的极速开发与调试：

### 10.1 核心命令体系

| 命令 | 功能说明 | 跨语言行为 |
| :--- | :--- | :--- |
| **`kanon-dev plugin create <name> --lang <rust\|python\|ts>`** | 自动生成标准插件骨架项目 | - `rust`: 生成 `Cargo.toml`、`src/lib.rs` 或 `main.rs` 与 `plugin.toml`<br>- `python`: 生成基于 `uv` 的 `pyproject.toml`、`main.py`<br>- `ts`: 生成 `package.json`、`tsconfig.json`、`src/index.ts` |
| **`kanon-dev dev <path> [--node <url>]`** | 对运行中的节点热重载插件 | 轮询插件目录变动，经管理 API `POST /api/v1/plugins/{id}/restart` 重启插件宿主。Rust 插件先执行 `cargo build`，失败则保留运行中版本；Python/TS 插件直接重启，依赖由 Supervisor 按需重装 |
| **`kanon-dev test <path>`** | 脱机交互与测试驱动器 | 进程内最小节点（真实流水线、内置命令、钩子、内存 KV、临时会话库）+ mock 模型；支持交互 REPL、`-m` 脚本化多轮对话、`-c`/`-t` 单次执行，`!tool` 触发完整 Tool Calling 轮次，`:prompt` 查看请求 |
| **`kanon-dev lint <path>`** | 静态清单与类型规范校验 | 静态校验 `plugin.toml` 的 JSON Schema、命令命名冲突与权限合法性 |
| **`kanon-dev pack <path>`** | 打包可分发插件制品 | 自动校验依赖并打包为 `.kpk` (Kanon Plugin Package) 标准分发包，供发布到插件中心 |

### 10.2 插件分发包物理格式规范 (`.kpk` Package Specification)

`.kpk` (Kanon Plugin Package) 是 Kanon 生态的标准化分发归档格式，物理上为**标准 ZIP 容器 + SHA-256 完整性校验文件**：

1. **根目录强制契约**：
   - 根目录下必须包含合法的 `plugin.toml` 清单文件；
   - 必须包含 `README.md` 与可选的 `LICENSE`。
2. **多语言制品打包规范**：
   - **Rust 插件**：打包对应编译目标的预编译原生可执行文件（如 `bin/x86_64-unknown-linux-gnu/<plugin>` 或 `bin/x86_64-pc-windows-msvc/<plugin>.exe`），做到用户端零编译闪电加载；
   - **Python 插件**：携带插件源码、`pyproject.toml` 与 `uv.lock`；首次启动时 Supervisor 在插件目录执行 `uv sync --locked` 复现 `.venv`；
   - **TypeScript 插件**：携带转译后的 `dist/` 或源码及附带锁定文件的 `package.json`，首次启动时按锁文件执行 `bun install --frozen-lockfile` 或 `npm ci`（支持由 `bun` 或 `node/tsx` 直接加载）。

---

## 11. 设计缺陷修正与工程优化对照表 (Defect Fixes & Optimization Matrix)

| 架构维度 | 初稿设计隐患 (V1.0) | 工程收敛方案 (Convergence Baseline) | 核心收益与防护目标 |
| :--- | :--- | :--- | :--- |
| **IPC 路由与端点绑定** | 单一 Socket 混杂监听，多客户端反向 RPC 寻址悖论 | **独立端点目录隔离模型**（Core 监听 `core.sock`，各 Host 监听专属 `host_<id>.sock`） | 保持标准 gRPC 纯粹语义，双向调用完全解耦，支持使用 `grpcurl` 独立排障。 |
| **故障隔离边界** | 默认单一共享 Host 进程，单插件阻塞/崩溃累及全盘 | **生产默认独立子进程隔离 (`Per-Plugin Process`)**，仅在开发调试模式支持合批共享 | 有效隔离同步代码阻塞与 C 扩展段错误 (SegFault) 导致的跨插件连环瘫痪。 |
| **跨平台传输层** | 硬编码 `/tmp/` 路径，Windows 缺失或存在事件循环兼容坑 | **抽象独立 Crate `kanon-transport`**，Linux/macOS 走 UDS，Windows 走安全认证本地 Loopback TCP | 统一 `IpcListener`/`IpcStream` 抽象，规范跨平台传输层实现。 |
| **富媒体消息载荷** | 弱类型 `map<string, string>` 反模式，二进制被迫 base64 膨胀 | **`oneof` 强类型联合体**，明确划分文本、图片、音频、艾特，支持本地零拷贝路径与裸二进制 | 恢复 Protobuf 类型安全优势，降低内存膨胀与额外解析开销。 |
| **Tool Calling 序列化** | 采用 string JSON 传参，跨进程带来四次序列化/反序列化消耗 | **双模载荷 (`oneof { google.protobuf.Struct; bytes }`)** | 大模型字典参数免除字符串解析损耗，同时保留超大二进制张量的直传通道。 |
| **持久化访问开销** | 高频 PreFilter 频繁发起 gRPC 远程读取 KV，I/O 放大严重 | **读缓存常驻 Host 内存 + 业务持久化直接下沉至插件专属目录**（本地 SQLite/DuckDB） | 消除集中式数据库代理延迟与数据放大，保障插件存储物理隔离。 |
| **容灾与超时保护** | 仅依赖静态超时判定，高负载与 GC 停顿引发级联超时雪崩 | **流水线 PreFilter 全局预算 + 单平台出站独立熔断器 (Circuit Breaker)** | 限制插件链慢调用，快速短路已宕机平台，降低系统级联雪崩风险。 |
| **运行时依赖定位** | 容易被误解为 Python/Node 为强依赖 | **明确核心自包含与按需探测原则**，纯 Rust 运行时具备低开销基线，Python/Node 仅按需惰性探测 | 保持 Rust 极简纯净单二进制分发的部署优势。 |
| **入站与心跳锁步** | 适配器同步阻塞等待核心处理，大模型慢推理导致 IM 网关反向断连 | **Fast-ACK 异步队列机制**（入站 Tokio MPSC 快速返回，出站独立 OnDeliverMessage） | 解耦入站流水线与出站投递，保障适配器心跳不被下游慢推理阻塞。 |
| **Windows 本地安全性** | 开放 127.0.0.1 端口暴露于同机非特权进程，存在指令嗅探注入风险 | **CSPRNG 32-Byte 随机 Token 握手鉴权**（私有环境变量注入 + gRPC 首帧恒定时间比对） | 有效防范本机未授权非特权进程伪造请求或探测。 |
| **Linux IPC 路径与权限安全** | 默认目录权限宽松或依赖不可靠的共享 `/tmp/`，面临符号链接劫持与多租户权限越界 | **基于 UID 隔离的 `/tmp/kanon-run-$UID/` 降级路径 + 强制 `0700` 权限收敛与符号链接深度拦截** | 消除本地多用户非特权攻击者对 UDS 套接字的窃听、替换与权限越界风险。 |
| **插件配置并发更新竞争** | 多并发热更新时缺乏时序锁，后发请求可能被先发慢请求覆盖产生时序倒退 | **CAS (Compare-And-Swap) 乐观并发控制与单调版本向量**（冲突返回 HTTP 409 Conflict，三语言 SDK 验证版本号） | 保证跨进程与控制面配置更新的严格线性一致性与时序安全性。 |
| **出站平台级联雪崩与抖动** | 外部平台接口严重劣化或停机时，出站重试导致单平台队列严重堵塞甚至反压 | **单平台独立短路熔断器 (Circuit Breaker)**（连续 5 次失败转为 Open，极速短路，30s 半开自愈探测） | 避免无效重试持续消耗系统资源，保护底层连接池。 |
| **提示词缓存命中率** | 提示词按“身份 → 上下文 → 指令”随意拼接，动态信息（时间、发送者）混进系统提示词，工具列表顺序随注册/宿主启动顺序变化 | **静态→动态分层**：工具 → 单条系统块 → 仅追加历史 → 当前轮；工具与 Schema 稳定排序；人设为纯静态文本（见 8.6） | 每轮只有尾部变化，前缀稳定命中服务商缓存；`kanon_llm_cached_prompt_tokens_total` 可直接度量命中率。 |
| **会话记忆与缓存** | 滑动窗口每轮丢弃最旧消息，其后所有内容的偏移改变，整段历史缓存全部失效 | **仅追加记忆 + 缓存友好的压缩**：阈值触发一次同前缀摘要，摘要挂载到新前缀（见 8.7） | 前缀只在压缩那一刻变化一次，其余时间稳定增长；摘要请求本身也吃缓存。 |
| **模型选择语义** | “默认提供商”与“默认模型”两个可能互相矛盾的设置，控制台还要展示“当前生效提供商” | **只有一个全局默认模型**（`PUT /api/v1/models/default`），提供商仅为端点；模型引用必须带已配置的提供商前缀，不再隐式回退 | 消除第二个数据源与隐式回退；配置错误在写入前即被拒绝。 |
| **会话续接** | 会话历史与元数据只在内存，重启或编辑实例后对话断裂 | **历史 + 会话记录统一落盘 `data/sessions.db`**，会话键由持久化的实例与代数确定，打开失败即启动失败（见 8.8） | 重启节点、编辑实例后对话原地续接，且不会静默退化为失忆。 |
| **出站饱和丢包与死信排查** | 队列背压打满或永久失败后丢弃消息仅有日志，无法离线对账与重放补发 | **按平台与日期分片的死信冷存储归档 (DLQ JSONL)**（落地 `./data/dead_letter/<platform>_<date>.jsonl`） | 实现不可逆出站失败的审计追踪与离线排查能力。 |

---

## 12. 系统边界与已知局限 (System Boundaries & Known Limitations)

为贯彻“简单优先、显式优先、失败优先、验证优先”的工程准则，本节坦诚列出当前架构版本的明确系统边界与已知局限性：

### 12.1 存储模型边界 (Storage Model Boundaries)
- **中心 KV 只存小状态**：`data/kv.db` 按插件分命名空间，单值至多 1 MiB，没有跨键事务、没有跨插件共享，也不是分布式存储（见第 7 节）。
- **大数据留在插件目录**：业务表、需要查询或事务的数据在插件所属的 `./data/plugins/<plugin_id>/` 中本地持久化（推荐 SQLite、DuckDB 或文件系统），核心不代理这类读写。
- **会话历史与会话记录的持久化边界 (Session History vs Record)**：
  - **对话历史与摘要**：`SqliteMemory`（`crates/kanon-llm/src/sqlite_memory.rs`）以 SQLite WAL 模式持久化仅追加的对话历史与压缩摘要，压缩在单个事务内完成，单会话内具备串行写入一致性与跨进程重启持久性。
  - **会话记录 (`SessionMetadata`)**：人设绑定、变量、轮次与 Token 计数、状态由 `SessionManager` 在内存（DashMap）中维护，并经 `SqliteSessionStore` **写穿**到同一个 `data/sessions.db`；节点启动时一次性加载。写入失败记录 `error` 日志但不使回复失败，库无法打开/读取则启动失败（见 8.8）。
  - **未持久化的仅有**：进行中的后台压缩任务与同会话去重集合（重启后自然重新评估）。

### 12.2 宿主环境与语言运行时依赖 (Language Host Runtime Dependencies)
- **Rust 核心自包含**：节点二进制 `kanon`（以及开发者 CLI `kanon-dev`）为零动态外部依赖的单一原生二进制；`kanon-core` / `kanon-api` 等 crate 仅提供库，不产出可执行文件。
- **外部多语言宿主依赖宿主环境**：Python 插件需要主机安装 Python 3.10+ 及包管理器（如 `uv` / `pip`）；TypeScript 插件需要系统安装 `bun` 或 `node`/`tsx`。若系统未安装相应运行时，核心在尝试拉起外部宿主时会显式记录错误并拒绝激活该插件，但不会影响 Rust 原生插件与核心流水线的持续运行。

### 12.3 单机有界队列与背压行为 (Single-Node Bounded Queue & Backpressure)
- **非分布式消息队列**：流水线调度基于 Tokio 进程内内存 MPSC 通道（入站默认容量 1024，单平台出站默认容量 64），不具备类似 Kafka / RabbitMQ 的跨节点分布式容灾与持久化确认机制。
- **背压饱和丢弃**：入站队列饱和时，核心立即通过 Fast-ACK 返回 `accepted: false`（HTTP 503）；出站平台队列饱和或断路器处于 `Open` 状态时，消息直接转入死信日志（DLQ JSONL）进行冷归档，不提供消息重放消费者，需由运维人员或外部审计工具介入重放。

### 12.4 性能基准声明与调优空间 (Performance Benchmark & Tuning Disclaimer)
- **指标基线定位**：文档中所提及的时延与吞吐目标（如 PreFilter 30ms 预算、出站熔断连续 5 次阈值等）均为架构级工程约束与默认配置基线，而非在所有硬件环境下的绝对物理性能保证。
- **实际性能变量**：端到端延迟主要受大语言模型提供商的网络 RTT、推理生成速度、外部 IM 平台 API 速率限制以及 Python/TS 宿主 GC 与脚本效率影响。生产环境应结合实际负载压测调整通道缓冲区大小与超时时间。

### 12.5 单机拓扑与容灾边界 (Single-Node Topology & HA Scope)
- **单节点进程模型**：微内核当前设计为单机单节点运行形态，Supervisor 仅负责管理本机子进程，不包含跨主机心跳、主备选举或分布式协同功能。
- **高可用建议**：生产部署时建议通过外部进程管理工具（如 `systemd`、Docker Compose 或 Kubernetes）提供进程级与容器级的高可用拉起守护。
