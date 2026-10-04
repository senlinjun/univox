# Univox

> **uni** + **vox** —— 多平台语音协议集成库。

Univox 把多个语音平台的协议实现收敛到**一套 Rust API** 之下：同一个机器人/客户端程序可以用统一的模型（服务器、频道、成员、消息、语音流、事件）同时驱动多个平台会话，而不用为每个平台学一套 SDK。

**当前状态：TeamSpeak 3 驱动完成并经真实服务器（3.13.8）验收（96 个测试
全绿：单元 + 协议向量 + 集成）；KOOK / OOPZ 仍为规划。** 完整功能列表见
**[docs/FEATURES.md](docs/FEATURES.md)**。

## TeamSpeak 3 已交付能力

- **原生客户端协议**（UDP 9987）：Init1+RSA 谜题、initivexpand2
  Curve25519/ECDSA 证书链、clientek、EAX 加密、分片/QuickLZ、
  return_code 命令关联、断线监督与自动重连（状态恢复）。
- **ServerQuery 管理驱动**（raw/SSH）：登录、命令、通知、防洪水退避。
- **统一抽象**：Session/Driver trait、状态镜像（Book）、统一事件总线、
  能力声明、多会话管理器。
- **语音管线**：Opus 编解码（48k 单声道 20ms 帧）、逐成员抖动缓冲、
  混音、SpeakingStarted/Stopped 事件；whisper 频道定向。
- **管理面**：频道 CRUD、成员移动/踢/封、权限组、privilege key、
  离线消息、clientdb、投诉、插件命令转发（Ts3Ext）。
- **文件传输/头像**：ftinit + TCP 通道，频道文件与头像上传下载。
- **地址解析**：`host:port`、`ts3server://` 邀请链接、TSDNS。

## 快速上手

```bash
# 依赖：Rust 1.75+；集成测试需要本机 ts3server（test/teamspeak3-server）
cargo test --workspace          # 单元 + 协议向量 + 真实服务器集成
cargo run -p univox --example echo_bot -- <address> [nickname]
```

```rust,no_run
use univox_core::{ConnectOptions, Credential, SessionRequest, session::SessionManager};
use univox_ts3::Ts3Driver;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manager = SessionManager::new();
    manager.register_driver(std::sync::Arc::new(Ts3Driver));
    let opts = ConnectOptions::new("ts3server://ts.example.com?nickname=MyBot")
        .credential(Credential::Anonymous);
    let session = manager.connect(SessionRequest::new(
        univox_core::platform::Platform::Ts3, opts)).await?;
    let mut events = session.events();
    while let Some(ev) = events.next().await {
        // 统一事件：消息、成员进出、说话起止……
    }
    Ok(())
}
```

## 目标平台

| 平台 | 接入方式 | 状态 |
|------|----------|------|
| TeamSpeak 3 | 原生客户端协议（UDP）+ ServerQuery 管理接口 | **已完成** |
| KOOK | 官方 Bot API（REST + WebSocket + RTP 音频推流） | 规划中 |
| OOPZ | 社区逆向协议（REST + WebSocket + Agora RTC 桥） | 规划中 |
| TeamSpeak 6 | 无公开 SDK；语音线路与 TS3 兼容 | 未来项 |

## 设计要点

- **统一 API + 插件式平台驱动**：平台差异用能力声明（capabilities）与平台扩展层（`Ts3Ext` / `KookExt` / `OopzExt`）隔离。
- **Rust 编译为动态库**：核心为 Rust 实现，以 C ABI（`cdylib` → `.so` / `.dll`）交付，Python / C# / Java 等语言经 FFI 绑定复用同一份实现。
- **状态簿记**：内存镜像服务器状态（频道树/成员/角色），属性级变更事件驱动。
- **统一事件总线**：平台事件统一映射，未映射事件经 `RawEvent` 透传。
- **多会话**：同进程跨平台、跨账号任意数量会话。
- **音频为 PCM 流抽象**：库负责 f32 PCM / Opus 编解码与传输，设备采集留给应用层。

## 文档

- [docs/FEATURES.md](docs/FEATURES.md) —— 完整功能列表（数据模型、会话生命周期、事件系统、语音/消息/管理模块、平台能力矩阵、风险与非目标）。
- [docs/TS3_PROTOCOL_NOTES.md](docs/TS3_PROTOCOL_NOTES.md) —— TS3 协议实现笔记（握手/加密/命令响应命名/文件传输/实测怪癖）。
- [docs/EVENT_MAPPING.md](docs/EVENT_MAPPING.md) —— TS3 通知 → 统一事件映射表。

## 许可

未定（实现启动时确定）。
