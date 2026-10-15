# Nuo (傩)

[English](README.md) | [简体中文](README-zh.md)

> 项目名 **Nuo** 取自拼音「**傩**」。傩是中国传统驱灾的祭祀，后被认为沟通神灵的仪式。按照传统傩的流程，祭司需要戴上傩面，根据傩面的形象而传达意图。当前项目的灵感来自于此：通过 harness 工程使用不同的身份与智能沟通。

Nuo 是一个基于 Rust 构建的高性能、模块化 AI 会话守护进程与语义终端系统。它提供常驻后台服务、丰富的交互式终端界面，并原生基于 **Agent Coordination Protocol (ACP)** 提供一流的多智能体协同能力。

---

## 核心特性

- **语义终端界面**：基于 `nuo-tui` 与自研 `nuotc` 渲染引擎的终端界面，提供亚毫秒级差异化增量渲染、纯净防闪烁体验以及完美的 CJK 宽字符排版。
- **会话守护进程 (`nuo`)**：跨项目托管多会话的轻量级后台进程，提供本地 IPC（Unix Domain Sockets）与 WebSocket 双控制面。
- **执行 Harness (`nuo-harness`)**：为智能体动态装配身份与上下文，实施严格的操作风险管控（`RiskProfile`）、人类交互确认断点与实时死循环熔断。
- **多智能体协作协议 (ACP)**：原生实现 Agent Coordination Protocol，支持点对点任务委派（`agent://`）与多订阅者频道广播（`acp://`）。
- **零开销工具体系**：纯粹的零 I/O 契约设计，支持编译期 JSON Schema 派生（`nuo-tool-derive`），并原生集成 MCP（Model Context Protocol）客户端与服务端。

---

## 快速上手

### 1. 编译构建

```bash
cargo build --release -p nuo
```

### 2. 启动服务（可选 / 容器与无头服务环境）

```bash
# 后台启动会话服务
./target/release/nuo server start

# 或前台运行服务容器（适用于 systemd 或 Docker 容器环境）
./target/release/nuo server start --fg

# 查看系统运行状态与活跃会话
./target/release/nuo status

# 获取客户端连接所需的鉴权 Token
./target/release/nuo server token
```

### 3. 启动终端交互

```bash
# 启动交互式 TUI（无感知自动连接或自启服务）
./target/release/nuo

# 恢复最近的会话线程
./target/release/nuo -r

# 在非交互 Headless 模式下直接执行任务并流式输出
./target/release/nuo run "介绍一下 Nuo 的设计理念"
```

---

## CLI 常用命令

| 命令 | 说明 |
| :--- | :--- |
| `nuo` | 打开交互式全屏终端界面（默认） |
| `nuo -r` / `nuo <thread_id>` | 在交互式 TUI 中恢复最近或指定的会话线程 |
| `nuo run "<prompt>"` / `nuo -p` | 一次性 Headless 执行 Prompt 并流式输出结果 |
| `nuo status [--watch]` | 查看系统健康状态、会话线程列表与监听端点 |
| `nuo server start [--fg]` | 启动会话服务（默认后台运行，可选 `--fg` 前台调试/容器） |
| `nuo server stop` | 优雅终止会话服务及所有后台连接 |
| `nuo server reload` | 平滑热重载配置与 MCP/技能 |
| `nuo server restart` | 重启或替换会话服务实例 |
| `nuo server token` | 打印当前可用的 Bearer Token |
| `nuo thread ls` | 列出所有托管的会话线程及其状态 |
| `nuo thread rm <id>` | 销毁指定的会话线程 |
| `nuo config <ls\|get\|set>` | 查看或修改系统配置 |
| `nuo mcp ls` | 列出已配置的 MCP 服务器及其工具探活状态 |
| `nuo skill ls` | 列出已发现的技能清单 |

---

## 相关文档

- **[子系统架构蓝图 (Subsystem Architecture)](docs/architecture/subsystems.md)**：工作区拓扑、Crate 边界与架构不变量约束。
- **[架构决策记录 (ADR)](docs/adr/index.md)**：核心技术决策演进、权衡考量与否定知识记录。
