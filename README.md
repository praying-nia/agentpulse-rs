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
| `agentpulse-relay` | Optional self-hosted synchronization and message-routing service / 可选的自托管同步与消息路由服务 |
| `agentpulse-protocol` | Rust types and codecs implementing the canonical protocol / 协议规范的 Rust 类型与编解码实现 |
| [`agentpulse-transport`](agentpulse-transport) | Bounded concrete transport primitives / 有界的具体传输原语 |
| `agentpulse-providers` | Integrations for supported AI coding agents / 各类 AI Coding Agent 的接入实现 |
| `agentpulse-channels` | Native, bot, and Webhook user-interaction outputs / Native、Bot 与 Webhook 用户交互出口 |

Initial Provider targets are Codex, Claude Code, OpenCode, and DeepSeek Harness. Provider integrations should prefer official RPC or SDK support, followed by Plugin APIs, writable Hooks, read-only Hooks, and finally PTY/TUI techniques.

首批 Provider 目标为 Codex、Claude Code、OpenCode 与 DeepSeek Harness。Provider 集成应优先使用官方 RPC 或 SDK，其次为 Plugin API、可回写 Hook、只读 Hook，最后才考虑 PTY/TUI 技术。

The first production Provider is [`agentpulse-provider-codex`](agentpulse-providers/codex). It manages a shared Unix-socket Codex App Server, strictly validates the pinned `0.150.1` schema, resumes explicit threads, and publishes live read-only session events.

首个正式 Provider 是 [`agentpulse-provider-codex`](agentpulse-providers/codex)。它托管共享 Unix Socket Codex App Server，严格校验固定的 `0.150.1` Schema，恢复显式 Thread，并发布实时只读 Session Event。

Initial Channel targets are Native, Feishu, QQ, and Webhook. The Native Channel implements the protocol and transport between the Rust Bridge and the separately maintained Android, iOS, and HarmonyOS apps; it does not reimplement those clients.

首批 Channel 目标为 Native、飞书、QQ 与 Webhook。Native Channel 实现 Rust Bridge 与独立维护的 Android、iOS、HarmonyOS App 之间的协议和传输，不会重新实现这些客户端。

The first production Channel is [`agentpulse-channel-native`](agentpulse-channels/native). It serves one local client over a bounded loopback WebSocket, performs strict Hello/Discovery/Subscription control, establishes exact Session baseline cursors, and streams unchanged JSON v1 Session/Event envelopes. It is deliberately read-only and declares exactly notification, session-view, and real-time synchronization capabilities.

首个正式 Channel 是 [`agentpulse-channel-native`](agentpulse-channels/native)。它通过有界 Loopback WebSocket 服务一个本地客户端，执行严格的 Hello/Discovery/Subscription 控制，建立精确 Session Baseline Cursor，并持续传输未经改写的 JSON v1 Session/Event Envelope。该版本明确保持只读，只声明通知、Session View 与实时同步能力。

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

The Rust workspace now contains a complete local read-only product path: strict domain and Native protocols, multi-endpoint Bridge and RuntimeHost lifecycle, the complete read-only Codex Provider, bounded loopback WebSocket transport, and the Native Channel with discovery, cursor-safe subscription, live delivery, disconnect cleanup, and explicit reconnect. A real native client UI remains the next product gap; Relay, persistence, and remote connectivity remain separate future milestones.

Rust workspace 现已形成完整的本地只读产品链路：严格领域与 Native 协议、Bridge 多端点编排与 RuntimeHost 生命周期、完整只读 Codex Provider、有界 Loopback WebSocket Transport，以及具备 Discovery、Cursor-safe Subscription、Live Delivery、断线清理和显式重连的 Native Channel。下一个产品缺口是真实原生客户端 UI；Relay、持久化与远程连接仍是独立的未来里程碑。
