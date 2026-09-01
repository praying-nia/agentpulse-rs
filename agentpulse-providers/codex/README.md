# AgentPulse Codex Provider

`agentpulse-provider-codex` is a complete read-only Provider for explicit Codex threads. It owns a version-pinned Codex App Server, exposes that server over a private Unix socket, validates the complete raw protocol, and maps live thread activity into AgentPulse sessions.

`agentpulse-provider-codex` 是面向显式 Codex Thread 的完整只读 Provider。它托管固定版本的 Codex App Server，通过私有 Unix Socket 暴露共享端点，校验完整原始协议，并将实时 Thread 活动映射为 AgentPulse Session。

## Compatibility / 兼容范围

- Codex CLI: explicitly verified `0.150.1` and `0.152.0`; other versions fail closed / 已明确验证 `0.150.1` 与 `0.152.0`，其他版本默认拒绝。
- Platforms: Linux and macOS managed Unix sockets / Linux 与 macOS 受管 Unix Socket。
- Input: explicitly configured UUIDv7 thread IDs / 显式配置的 UUIDv7 Thread ID。
- Events: live state, agent messages, connection changes, and turn outcomes / 实时状态、Agent 消息、连接变化与 Turn 结果。
- Write-back: unsupported; server requests receive an explicit JSON-RPC read-only error / 不支持回写；服务端请求会收到明确的 JSON-RPC 只读错误。

The bundled schema was generated with:

```bash
codex app-server generate-json-schema --out schemas
```

The bundled `0.152.0` aggregate schema SHA-256 is `d8faa38d5f00aa7ddfe635a2d374ee5f871ffd217d4d175c72fbe7f009f4f669`. The existing `0.150.1` captured fixtures also validate against this bundle. A Codex upgrade requires regenerating the schema and verifying fixtures plus a real managed App Server before adding the version to the explicit compatibility set.

## Runtime usage / 运行方式

```rust,no_run
use std::error::Error;

use agentpulse_bridge::RuntimeHost;
use agentpulse_core::ProviderId;
use agentpulse_provider_codex::{CodexProvider, CodexProviderConfig};

fn main() -> Result<(), Box<dyn Error>> {
    let config = CodexProviderConfig::new(
        ProviderId::new(),
        "/tmp/agentpulse-runtime",
        ["019976a4-00f0-7312-b36c-d01f9c5c06f6"],
    )?;
    let parts = CodexProvider::build(config)?;
    let remote_uri = parts.handle().remote_uri().to_owned();
    let (port, source, handle) = parts.into_parts();

    let mut host = RuntimeHost::new();
    host.register_provider(port, source)?;
    let _ = host.start()?;

    println!("connect Codex with: codex --remote {remote_uri}");
    println!("Provider health: {:?}", handle.snapshot().health());

    let _ = host.stop()?;
    Ok(())
}
```

Start RuntimeHost before connecting another client with `codex --remote <uri>`. The Provider resumes every configured thread before reporting startup success. It does not replay turns included in `thread/resume`; only notifications observed after the subscription boundary become AgentPulse events.

RuntimeHost 停止会关闭 WebSocket、等待读取 Worker、终止受管 App Server，并仅清理 Provider 自己创建的私有目录。实时协议或进程故障会将已跟踪 Session 标记为 `Disconnected`，并通过 `CodexProviderHandle` 暴露终态错误；恢复方式是显式停止并重新启动 RuntimeHost。

The canonical mapping and lifecycle contract is defined in the AgentPulse protocol repository's `codex-provider.md`.
