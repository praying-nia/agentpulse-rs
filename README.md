# agentpulse-rs

Shared Rust foundation for AgentPulse.

AgentPulse 的共享 Rust 基础仓库。

This repository hosts the cross-platform domain core, local agent bridge, self-hosted relay, transport abstractions, Rust protocol implementation, and CLI agent adapters. It is intended to provide consistent behavior across desktop-side integrations, server components, and native mobile clients.

本仓库承载跨平台领域核心、本地 Agent 桥接、自托管 Relay、传输抽象、Rust 协议实现与 CLI Agent Adapter，为电脑端集成、服务端组件和原生移动客户端提供一致的基础能力。

## Components / 组件

| Component | Responsibility / 职责 |
| --- | --- |
| `agentpulse-core` | Shared domain models and task/session state / 共享领域模型及任务、会话状态 |
| `agentpulse-bridge` | Local integration point for collecting and forwarding agent events / 采集并转发 Agent 事件的本地桥接层 |
| `agentpulse-relay` | Self-hosted synchronization and message-routing service / 自托管同步与消息路由服务 |
| `agentpulse-protocol` | Rust types and codecs implementing the canonical protocol / 协议规范的 Rust 类型与编解码实现 |
| `agentpulse-transport` | Connection, delivery, and reconnection abstractions / 连接、投递与重连抽象 |
| `agentpulse-adapters` | Integrations for supported CLI coding agents / 各类 CLI Coding Agent 的适配实现 |

Initial adapter targets are Codex, Claude Code, OpenCode, and DeepSeek Harness.

首批 Adapter 目标为 Codex、Claude Code、OpenCode 与 DeepSeek Harness。

## Status / 状态

The repository currently contains the planned component layout only. No Rust workspace or runtime implementation has been initialized yet.

当前仓库仅包含计划中的组件结构，尚未初始化 Rust workspace 或运行时代码。

