# agentpulse-rs

Shared Rust foundation for AgentPulse.

AgentPulse 的共享 Rust 基础仓库。

This repository hosts the cross-platform domain core, local agent bridge, optional self-hosted Relay, transport abstractions, Rust protocol implementation, AI agent Providers, and user-facing Channels. It provides consistent behavior across desktop-side integrations, server components, native clients, bots, and Webhooks.

本仓库承载跨平台领域核心、本地 Agent Bridge、可选的自托管 Relay、传输抽象、Rust 协议实现、AI Agent Provider 与用户交互 Channel，为电脑端集成、服务端组件、原生客户端、Bot 和 Webhook 提供一致的基础能力。

## Architecture / 架构

AgentPulse has two independent extension dimensions:

AgentPulse 包含两个相互独立的扩展维度：

- A **Provider** integrates an AI coding agent. It translates agent-specific events into the shared domain model and writes supported responses or commands back to the agent.
- A **Channel** presents shared events and interaction requests to a user, then translates supported user responses into the shared domain model.

- **Provider** 集成具体 AI Coding Agent，将 Agent 特有事件转换为统一领域模型，并在能力允许时将响应或指令回写给 Agent。
- **Channel** 向用户展示统一事件和交互请求，再将受支持的用户响应转换为统一领域模型。

```text
AI Coding Agent → Provider → Bridge/Core → Channel
                                            ├─ Native (LAN or optional Relay) → App
                                            ├─ Feishu / QQ → Bot platform
                                            └─ Webhook → Consumer
```

Providers and Channels do not depend on one another. The Core routes channel-neutral events and interactions and never decides how a request should be rendered.

Provider 与 Channel 不直接依赖。Core 只路由与展示方式无关的事件和交互，不决定请求应如何呈现。

## Components / 组件

| Component | Responsibility / 职责 |
| --- | --- |
| `agentpulse-core` | Shared domain models and task/session state / 共享领域模型及任务、会话状态 |
| `agentpulse-bridge` | Runtime-neutral endpoint orchestration, subscriptions, fan-out, and Adapter lifecycle hosting / 运行时中立的端点编排、订阅、扇出与 Adapter 生命周期托管 |
| [`agentpulse-relay`](agentpulse-relay) | Optional authenticated opaque public tunnel for Native v1 / 可选、带认证且不解密 Native v1 的公网隧道 |
| `agentpulse-protocol` | Rust types and codecs implementing the canonical protocol / 协议规范的 Rust 类型与编解码实现 |
| [`agentpulse-transport`](agentpulse-transport) | Bounded concrete transport primitives / 有界的具体传输原语 |
| [`agentpulse-pairing`](agentpulse-pairing) | Host identity, one-shot pairing, and device credentials / Host 身份、一次性配对与设备凭证 |
| [`agentpulse-host`](agentpulse-host) | User-facing local Host CLI / 面向用户的本地 Host CLI |
| `agentpulse-providers` | Integrations for supported AI coding agents / 各类 AI Coding Agent 的接入实现 |
| `agentpulse-channels` | Native, bot, and Webhook user-interaction outputs / Native、Bot 与 Webhook 用户交互出口 |

Initial Provider targets are Codex, Claude Code, OpenCode, and DeepSeek Harness. Provider integrations should prefer official RPC or SDK support, followed by Plugin APIs, writable Hooks, read-only Hooks, and finally PTY/TUI techniques.

首批 Provider 目标为 Codex、Claude Code、OpenCode 与 DeepSeek Harness。Provider 集成应优先使用官方 RPC 或 SDK，其次为 Plugin API、可回写 Hook、只读 Hook，最后才考虑 PTY/TUI 技术。

The first production Provider is [`agentpulse-provider-codex`](agentpulse-providers/codex). It manages a shared Unix-socket Codex App Server, strictly validates the byte-identical generated `0.152.0`/`0.152.1` schema for explicitly verified CLI versions, and starts valid newer SemVer releases best-effort with a visible warning while preserving strict protocol failure. It can resume explicit threads or transiently follow threads opened through the same App Server, publishes live session events, and correlates command/file approval options back to exact Codex decisions.

首个正式 Provider 是 [`agentpulse-provider-codex`](agentpulse-providers/codex)。它托管共享 Unix Socket Codex App Server，针对明确验证过的 CLI 版本严格校验 `0.152.0`/`0.152.1` 生成且逐字节相同的 Schema；更高的合法 SemVer 会在明确警告后尽力启动，但协议不兼容仍严格失败。Provider 可恢复显式 Thread，也可临时跟踪同一 App Server 中打开的 Thread，发布实时 Session Event，并把命令/文件审批 Option 精确关联回 Codex Decision。

Initial Channel targets are Native, Feishu, QQ, and Webhook. The Native Channel implements the protocol and transport between the Rust Bridge and the separately maintained Android, iOS, and HarmonyOS apps; it does not reimplement those clients.

首批 Channel 目标为 Native、飞书、QQ 与 Webhook。Native Channel 实现 Rust Bridge 与独立维护的 Android、iOS、HarmonyOS App 之间的协议和传输，不会重新实现这些客户端。

The first production Channel is [`agentpulse-channel-native`](agentpulse-channels/native). It serves one client over bounded loopback WebSocket or authenticated private-LAN WSS, performs strict Hello/Discovery/Subscription control, establishes exact Session/pending-interaction baselines, streams unchanged JSON v1 envelopes, and submits opaque approval options. It declares exactly notification, session-view, approval, and real-time synchronization capabilities.

首个正式 Channel 是 [`agentpulse-channel-native`](agentpulse-channels/native)。它通过有界 Loopback WebSocket 或带认证私有 LAN WSS 服务一个客户端，执行严格的 Hello/Discovery/Subscription 控制，建立精确 Session/Pending Interaction Baseline，持续传输未经改写的 JSON v1 Envelope，并提交不透明审批 Option。它精确声明通知、Session View、Approval 与实时同步能力。

## Channel experience / Channel 体验

Native apps are the full experience and are intended to support session lists, state, plans, progress, real-time events, approvals, structured input, LAN connections, and Relay connections. Bot Channels are lightweight compatibility paths and may expose supported interactions as commands such as:

原生 App 是完整体验，计划支持 Session 列表、状态、Plan、进度、实时事件、审批、结构化输入、LAN 直连和 Relay 连接。Bot Channel 是轻量兼容路径，可以将受支持的交互表现为以下命令：

```text
/approve <interaction-id>
/reject <interaction-id>
/answer <interaction-id> <value>
/status
/sessions
```

The Relay is never a Core requirement. Native clients may connect directly over LAN or through the Relay, while Bot and Webhook Channels may communicate directly with their third-party platforms.

Relay 不作为 Core 的强依赖。原生客户端可以通过 LAN 直连或经过 Relay；Bot 和 Webhook Channel 则可以直接连接第三方平台。

Every Provider and Channel declares its capabilities. A remote operation is exposed only when the complete route supports it; otherwise the experience degrades to the available notification or read-only behavior.

每个 Provider 与 Channel 都会声明自身能力。只有完整链路均支持时才开放远程操作，否则退化为当前可用的通知或只读体验。

## Status / 状态

The Rust workspace now powers a usable observation and approval product path: the `agentpulse` Host CLI owns a stable private identity and CA, runtime-discovered Codex threads, authenticated private-LAN Native WSS, mDNS discovery, QR-only public first pairing, per-device revocation, credential rotation, and an outbound Relay connector. `agentpulse-relay` authenticates routes and pumps end-to-end Host-CA TLS ciphertext without access to bootstrap/device Tokens or Session/Event/approval plaintext. Session/Event, pending approval, and Relay route state remain in memory. Offline history, databases, and broader input remain separate milestones. Production deployment assets are documented in [`deploy`](deploy).

Rust workspace 现已支撑可用的观察与审批产品链路：`agentpulse` Host CLI 管理稳定私有身份与 CA、运行态发现的 Codex Thread、带认证私有 LAN Native WSS、mDNS 发现、纯二维码公网首次配对、逐设备撤销、凭证轮换及出站 Relay Connector。`agentpulse-relay` 认证路由并泵送端到端 Host-CA TLS 密文，无法获取 Bootstrap/设备 Token 或 Session/Event/审批明文。Session/Event、Pending Approval 与 Relay Route 仅保存在内存中；离线历史、数据库与更广泛输入属于后续里程碑。
