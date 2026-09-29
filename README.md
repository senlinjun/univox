# Univox

> **uni** + **vox** —— 多平台语音协议集成库。

Univox 把多个语音平台的协议实现收敛到**一套 Rust API** 之下：同一个机器人/客户端程序可以用统一的模型（服务器、频道、成员、消息、语音流、事件）同时驱动多个平台会话，而不用为每个平台学一套 SDK。

**当前状态：规划阶段。** 完整功能列表见 **[docs/FEATURES.md](docs/FEATURES.md)**。

## 目标平台

| 平台 | 接入方式 | 状态 |
|------|----------|------|
| TeamSpeak 3 | 原生客户端协议（UDP）+ ServerQuery 管理接口 | 规划中 |
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

## 许可

未定（实现启动时确定）。
