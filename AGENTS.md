# AGENTS.md — Kanon 智能体工程规范

> **核心原则**：
> **简单优先，显式优先，失败优先，验证优先；不要为不存在的问题增加复杂度。**

---

## 1. 通用工程原则

1. **先定义问题，再写代码。**
2. **保持最小设计，不为未来需求提前设计。**
3. **优先复用和删除，而不是增加抽象。**
4. **每个状态、数据源和责任都应该唯一且明确。**
5. **错误必须显式处理，禁止静默忽略。**
6. **输入尽早验证，内部逻辑只处理可信数据。**
7. **失败必须可预测；不要猜，不要用隐式 fallback 掩盖错误。**
8. **减少副作用，明确不可逆操作和提交点。**
9. **测试正常路径，也测试失败、边界和中断。**
10. **发现问题先修根因，不要用补丁堆复杂度。**
11. **代码应当容易理解、验证和删除，而不是容易扩展。**
12. **当设计已经足够简单且正确时，停止重构。**
13. **测试与业务逻辑物理分离**：禁止在业务模块（`src/`）中内联混杂 `#[cfg(test)]` 代码；所有测试统一独立置于所属 Crate 根目录下的 `tests/` 目录，保持核心逻辑精简纯粹。

---

## 2. 项目定位与核心设计哲学

Kanon 是基于 Rust 2024 构建的高性能多平台聊天机器人微内核，支持 **Rust / Python / TypeScript** 插件接入。详细架构请参阅项目内相对路径：[./docs/ARCHITECTURE.md](./docs/ARCHITECTURE.md)。

- **零外部硬依赖**：节点二进制 `kanon` 是 100% 自包含的单一程序，无 Python/Node 即可在纯净系统上独立运行（常驻内存 < 20MB）。Python/TS 仅为按需惰性探测的扩展运行时。
- **物理故障隔离**：生产环境各插件运行于独立子进程，杜绝单插件阻塞或崩溃（SegFault/OOM）拖垮全盘。
- **强契约通信**：跨进程统一走 gRPC（HTTP/2 + Protobuf）。富媒体用强类型 `oneof`，Tool Calling 用 `google.protobuf.Struct` 双模载荷，禁止平铺字符串 Map 与冗余序列化。
- **前后端解耦**：无头微内核专注调度与 REST/WebSocket API 网关，WebUI 保持独立。
- **严禁硬编码绝对路径**：全工程禁止出现任何特定用户的本地绝对路径，统一采用相对于项目根目录的相对路径，运行时目录采用标准跨平台抽象（如 `$XDG_RUNTIME_DIR/kanon/run/`）。

---

## 3. 模块目录结构 (相对路径)

```
./
├── Cargo.toml                      # 根 Workspace 配置 (Rust 2024)
├── AGENTS.md                       # 本规范
├── docs/ARCHITECTURE.md            # 全景架构设计规范文档
├── proto/kanon/v1/plugin.proto     # gRPC 契约 IDL
├── crates/                         # 核心 Rust 模块
│   ├── kanon/                      # 程序入口：唯一节点二进制 kanon（组合根，仅负责装配）
│   ├── kanon-proto/                # gRPC 桩代码 (tonic-build + prost-types)（库）
│   ├── kanon-transport/            # 跨平台 IPC (UDS / Windows 认证 Loopback TCP)（库）
│   ├── kanon-storage/              # 嵌入式 KV 存储与数据目录隔离（库）
│   ├── kanon-llm/                  # 模型网关、提示词分层、仅追加会话记忆与压缩、Tool 状态机（库）
│   ├── kanon-core/                 # 事件循环、流水线调度、Supervisor 进程监管（库）
│   ├── kanon-api/                  # RESTful API 与 WebSocket 实时网关（库）
│   └── kanon-dev/                  # 官方 CLI（项目管理、热重载、沙盒测试）→ kanon-dev 二进制
├── sdks/                           # 多语言 SDK 与宿主
│   ├── rust/                       # Rust 插件 SDK (kanon-sdk)
│   ├── python/                     # Python 插件 SDK (kanon-sdk-python)
│   └── typescript/                 # TypeScript 插件 SDK (kanon-sdk-ts)
└── webui/                          # 独立 Web 控制台前端
```

---

## 4. 关键硬性约束速查 (Checklist)

- **IPC 拓扑**：目录隔离模式。Core 监听 `./run/core.sock`；各 Host 监听专属 `./run/host_<id>.sock`。
- **异步入站防锁步**：`BotApiService.IngestEvent` 必须非阻塞推入带高水位线的 Tokio MPSC 通道并立即 Fast-ACK（< 50µs），绝不同步等待 LLM，彻底切断反压链，保障 IM 心跳永不掉线。
- **Windows 本地安全**：TCP Loopback 握手必须在首包 HTTP/2 HEADERS 中携带 32-Byte CSPRNG 随机 Token（`x-kanon-auth-token`），核心恒定时间校验。
- **数据访问防放大**：只读配置 Host 内存缓存；复杂业务持久化直接在专属目录 `./data/plugins/<id>/` 本地读写 SQLite/DuckDB。
- **程序入口唯一**：全工程仅 `crates/kanon` 提供节点可执行文件 `kanon`；其余 crate 一律为库，禁止新增 `src/main.rs` 或 `[[bin]]`（`crates/kanon-dev` 的 `kanon-dev` 开发者 CLI 为唯一例外）。
- **构建产物收敛**：根 `default-members` 仅含 `crates/kanon` 与 `crates/kanon-dev`，一次默认构建（`cargo build` / `cargo test`）只产出 `kanon` 与 `kanon-dev` 两个可执行文件；全仓校验必须显式 `--workspace`（示例插件宿主属测试夹具，需先 `cargo build -p demo-weather -p demo-rust-plugin` 才能运行插件宿主用例）。
- **Rust 标准**：统一 **Rust 2024 Edition**，异步基于 Tokio/Tonic/Axum。错误用 `thiserror`/`anyhow` 显式追踪。`cargo check --workspace` 必须保持 **0 错误、0 警告**。
- **测试隔离规范**：所有测试代码必须从业务代码中独立剥离至 `tests/` 目录，禁止在 `src/` 中内联测试，确保逻辑代码零冗余。
- **测试只加有用的**：只为真实行为、失败路径与回归补充测试；禁止堆砌重复、琐碎或只为凑数量的测试。
- **提示词静态优先**：模型请求固定为 `工具 → 单条系统块（人设 + 技能目录 + 摘要）→ 仅追加历史 → 当前轮`。时间、发送者等运行时才知道的信息只能进入当前轮用户消息；工具列表按名称排序、Schema 键名排序；钩子不得在对话开始后再插入 system 消息。任何会改变请求前缀的改动都必须有测试证明前缀仍稳定（`crates/kanon-llm/tests/prompt_layout_test.rs`）。
- **记忆仅追加**：会话历史除 `Memory::compact_history` 与 `clear` 外禁止删除或重排消息，禁止再引入滑动窗口；压缩必须复用会话自己的请求前缀，且模型未给出摘要时绝不丢弃历史。
- **会话必须可续接**：历史、摘要与会话记录统一落盘 `./data/sessions.db`；新增会话级状态时必须写穿到 `SessionStore`。组合根打开失败即启动失败，禁止静默降级为内存会话。
- **单一默认模型**：节点只有一个全局默认模型（`<provider>/<model-id>`），提供商只是端点；模型引用必须带已配置的提供商前缀，禁止重新引入“默认提供商”或隐式回退。

---

## 5. 代码注释规范 (Code Comment Convention)

- **语言要求 (Language)**：**English only**。全工程（Rust、Python、TypeScript、Proto、脚本）所有代码注释、文档注释（doc comments）必须全部使用英文。
- **禁止翻译 (No Translation)**：任何场景下都不要把已有的非英文内容（注释、生成代码、文档、提示词等）翻译成英文——这是不必要的工作。英文要求只约束你新写的注释。
- **详尽注释原则 (Comment Abundantly)**：多留注释，避免“代码即注释”的盲目自信。
  - 每个公开模块、结构体（struct）、特征（trait）、函数与枚举必须提供清晰的文档级注释（Rust 使用 `///` 或 `//!`，Python/TS 使用标准 docstrings/JSDoc）；
  - 针对非显然的并发边界、内存生命周期、不可逆提交点、锁顺序、背压阈值与安全握手校验等关键逻辑，必须留下详尽的行内注释解释其背后的设计考量（*Why* 而非仅 *What*）。

---

## 6. Git 提交规范 (Commit Message Convention)

所有 Git 提交必须严格遵守 Conventional Commits 规范，杜绝冗长与中文提交信息：

- **Language**: **English only**.
- **Length**: As concise and brief as possible.
- **Format**:
  ```text
  <type>(<scope>): <short description>
  ```
- **Allowed Types**: `feat`, `fix`, `docs`, `refactor`, `perf`, `test`, `chore`
- **Common Scopes**: `kanon`, `proto`, `core`, `transport`, `storage`, `llm`, `api`, `dev`, `sdk-rust`, `sdk-py`, `sdk-ts`
- **Examples**:
  - `feat(transport): add loopback tcp auth handshake for windows`
  - `fix(core): ensure fast-ack mpsc queue non-blocking`
  - `docs(spec): update architecture spec to v1.2`
  - `chore(deps): upgrade workspace to rust 2024`
