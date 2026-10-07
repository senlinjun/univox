# Univox

> **uni** + **vox** —— 多平台语音协议集成库。

Univox 把多个语音平台的协议实现收敛到**一套 Rust API** 之下：同一个机器人/客户端程序可以用统一的模型（服务器、频道、成员、消息、语音流、事件）同时驱动多个平台会话，而不用为每个平台学一套 SDK。

当前已交付 **TeamSpeak 3** 驱动（原生客户端协议 + ServerQuery，经真实
服务器 3.13.8 验收）；KOOK / OOPZ / Discord 在路线图中。项目现状、平台
路线图与开发流程见 **[CONTRIBUTING.md](CONTRIBUTING.md)**；完整功能列表见
**[docs/FEATURES.md](docs/FEATURES.md)**。

## 快速上手

```bash
cargo run -p univox --example echo_bot -- <address> [nickname]
```

```toml
[dependencies]
univox = "0.1.0"        # 门面 crate：统一抽象 + TeamSpeak 3 驱动，一个依赖即可
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
```

```rust,no_run
use univox::{
    ConnectOptions, Credential, Event, Platform, SessionManager, SessionRequest, Ts3Driver,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manager = SessionManager::new();
    manager.register_driver(std::sync::Arc::new(Ts3Driver));
    let opts = ConnectOptions::new("ts3server://ts.example.com?nickname=MyBot")
        .credential(Credential::Anonymous);
    let session = manager.connect(SessionRequest::new(Platform::Ts3, opts)).await?;

    let mut events = session.events();
    while let Some(ev) = events.next().await {
        match &*ev {
            Event::Connected => { /* 会话就绪 */ }
            // 统一事件：消息、成员进出、说话起止……
            _ => {}
        }
    }
    Ok(())
}
```

## 设计要点

- **统一 API + 插件式平台驱动**：平台差异用能力声明（capabilities）与平台扩展层（`Ts3Ext` / `KookExt` / `OopzExt` / `DiscordExt`）隔离。
- **Rust 编译为动态库**：核心为 Rust 实现，以 C ABI（`cdylib` → `.so` / `.dll`）交付，Python / C# / Java 等语言经 FFI 绑定复用同一份实现。
- **状态簿记**：内存镜像服务器状态（频道树/成员/角色），属性级变更事件驱动。
- **统一事件总线**：平台事件统一映射，未映射事件经 `RawEvent` 透传。
- **多会话**：同进程跨平台、跨账号任意数量会话。
- **音频为 PCM 流抽象**：库负责 f32 PCM / Opus 编解码与传输，设备采集留给应用层。

## 参考实现与资料

- TeamSpeak 3：[ReSpeak/tsclientlib](https://github.com/ReSpeak/tsclientlib)（含 `ts-bookkeeping`、`tsproto-packets`）、TS3 ServerQuery 文档（`serverquerydocs`）、ClientQuery 插件接口、[TS3 Client Plugin SDK](https://github.com/teamspeak/ts3client-pluginsdk)
- TeamSpeak 6：[teamspeak/teamspeak6-server](https://github.com/teamspeak/teamspeak6-server)（releases/docs）、[TS6 ServerQuery 文档](https://mintlify.wiki/teamspeak/teamspeak6-server/server-query/overview.md)
- KOOK：[developer.kookapp.cn](https://developer.kookapp.cn) 与 [kaiheila/api-docs](https://github.com/kaiheila/api-docs)、[TWT233/khl.py](https://github.com/TWT233/khl.py)、[gehongyan/Kook.Net](https://github.com/gehongyan/Kook.Net)、[shuyangzhang/kookvoice](https://github.com/shuyangzhang/kookvoice)
- OOPZ：[DeeChael/oopz-api-docs](https://github.com/DeeChael/oopz-api-docs)、[tangqingfeng7/Oopzbot-SDK](https://github.com/tangqingfeng7/Oopzbot-SDK)（逆向协议，无官方文档）
- Discord：[官方开发者文档](https://discord.com/developers/docs)（Gateway / Intents / Voice / Interactions / Permissions）、[discord/dave-protocol](https://github.com/discord/dave-protocol)（语音 E2EE 规范）、Rust 参考 [serenity](https://github.com/serenity-rs/serenity) / [twilight](https://github.com/twilight-rs/twilight)、[discord.py](https://github.com/Rapptz/discord.py)（语音/DAVE 实现）

## 文档

- [CONTRIBUTING.md](CONTRIBUTING.md) —— 项目现状、平台路线图、开发与测试流程。
- [docs/FEATURES.md](docs/FEATURES.md) —— 完整功能列表（数据模型、会话生命周期、事件系统、语音/消息/管理模块、平台能力矩阵、风险与非目标）。
- [docs/TS3_PROTOCOL_NOTES.md](docs/TS3_PROTOCOL_NOTES.md) —— TS3 协议实现笔记（握手/加密/命令响应命名/文件传输/实测怪癖）。
- [docs/EVENT_MAPPING.md](docs/EVENT_MAPPING.md) —— TS3 通知 → 统一事件映射表。
- [CHANGELOG.md](CHANGELOG.md) —— 发布历史。

## 许可

MIT OR Apache-2.0（见 [LICENSE-MIT](LICENSE-MIT) / [LICENSE-APACHE](LICENSE-APACHE)）。
