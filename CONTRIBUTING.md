# 贡献指南

README 只保留门面信息；项目现状、路线图、开发与测试流程集中在这里。

## 现状与路线图

**当前状态：TeamSpeak 3 驱动完成并经真实服务器（3.13.8）验收（148 个测试
全绿：单元 + 协议向量 + 本地真实服务器集成）；KOOK / OOPZ / Discord 仍为
规划。** 完整功能列表与平台能力矩阵见
**[docs/FEATURES.md](docs/FEATURES.md)**。

| 平台 | 接入方式 | 状态 |
|------|----------|------|
| TeamSpeak 3 | 原生客户端协议（UDP）+ ServerQuery 管理接口 | **已完成** |
| KOOK | 官方 Bot API（REST + WebSocket + RTP 音频推流） | 规划中 |
| OOPZ | 社区逆向协议（REST + WebSocket + Agora RTC 桥） | 规划中 |
| Discord | 官方 Bot API（Gateway WebSocket + REST + 语音 UDP，DAVE 端到端加密） | 规划中 |
| TeamSpeak 6 | 无公开 SDK；语音线路与 TS3 兼容 | 未来项 |

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

## 开发环境与测试

- 工具链：Rust 1.75+（实际验收在 1.97 上进行，旧工具链未验证）。
- 集成测试需要本机 ts3server：`test/teamspeak3-server`（test-support
  启动器会从 stderr 解析 query 密码/token）。

```bash
cargo test --workspace                        # 单元 + 协议向量 + 真实服务器集成
cargo check --workspace --all-targets         # 基线：零警告
cargo test -p univox --doc                    # 门面 doctest
cargo check -p univox --no-default-features   # 关 voice：audiopus 必须退出依赖树
cargo run -p univox --example echo_bot -- <address> [nickname]
```

已知注意点：

- **feature 门控写在 workspace 表**：成员侧对 workspace 继承依赖写
  `default-features = false` 会被静默忽略（见根 Cargo.toml 的
  `univox-ts3` 条目注释）。
- **满载偶发**：`reconnect_restores_self_state` 在测试并行满载时，服务器
  重启可能慢于重连预算而超时失败；单独重跑即可。

## Crate 结构

| crate | 职责 |
|-------|------|
| `univox` | 门面：统一 re-export（`univox::{…}` 与 `core / ts3 / proto / voice` 别名），单依赖入口 |
| `univox-core` | 统一抽象：数据模型、会话生命周期、事件总线、Book、音频 trait |
| `univox-ts3` | TS3 驱动：原生客户端协议 + ServerQuery + `Ts3Ext` 平台扩展 |
| `univox-ts3-proto` | TS3 线协议：编解码、加密、握手、identity |
| `univox-voice` | 语音管线：Opus、抖动缓冲、混音器、3D 定位 |
| `test-support` | 集成测试脚手架（本地 ts3server 启动器） |

## 文档地图

- [docs/FEATURES.md](docs/FEATURES.md) —— 功能列表与平台能力矩阵（规划面）。
- [docs/TS3_PROTOCOL_NOTES.md](docs/TS3_PROTOCOL_NOTES.md) —— 协议实现笔记（3.13.8 实测怪癖）。
- [docs/EVENT_MAPPING.md](docs/EVENT_MAPPING.md) —— TS3 通知 → 统一事件映射。
- [CHANGELOG.md](CHANGELOG.md) —— 发布历史。
