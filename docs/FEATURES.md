# Univox 功能列表

> **Univox**（uni + vox，"统一语音"）—— 集中多个语音平台协议的 Rust 集成库。
> 一套统一 API 驱动 TeamSpeak 3、KOOK、OOPZ、Discord，让同一个机器人/客户端程序能同时挂载任意多个平台会话。
>
> **状态**：规划阶段（本文档为功能列表，不含实现）。
> **图例**：✓ 完整支持 · ◐ 部分支持 · ✗ 不支持 · `Ext` 仅平台扩展层暴露 · ★ 相对参考实现 tsclientlib 的新增规划项。

---

## 0. 设计目标与总原则

| # | 原则 | 说明 |
|---|------|------|
| G1 | **统一 API + 插件式平台驱动** | 核心只定义统一抽象（trait），TS3 / KOOK / OOPZ / Discord 各实现一个驱动（driver）。新增平台 = 新增驱动，核心不动。 |
| G2 | **状态簿记（Bookkeeping）** | 每个会话在内存维护服务器状态的镜像（频道树、成员、角色），由属性级变更事件驱动更新（参照 `tsclientlib` 的 `ts-bookkeeping` 设计）。 |
| G3 | **统一事件总线** | 平台原生事件统一映射为平台无关事件；无法映射的原样透传（`RawEvent`），信息不丢失。 |
| G4 | **能力声明（Capabilities）** | 每个驱动启动时声明能力集（能否收发音频、有无消息历史……），调用方可查询能力再决定行为，避免对不支持平台的无效调用。 |
| G5 | **多会话** | 同一进程内任意数量的会话，跨平台、跨账号（例：同时挂 2 个 TS3 服务器 + 3 个 KOOK 服务器 + 1 个 OOPZ 账号）。 |
| G6 | **Rust 核心 → 编译为动态库供其他语言复用** | 核心以 Rust 实现，编译为 C ABI 动态库（`cdylib` → `.so` / `.dll` / `.dylib`），其他语言经 FFI 绑定复用同一份实现（详见 0.1）。 |
| G7 | **音频为 PCM 流抽象** | 库负责 f32 PCM 采样收发、Opus 编解码与平台传输适配；麦克风/扬声器等设备 I/O 留给应用层。 |
| G8 | **效果优先** | 以能够成功接入为主形态；尽量模拟真实客户端，其次BOT API（用户账号自动化见 §14.3）。 |

**分层结构**：

```
┌───────────────────────────────────────────────────────────────────┐
│ 应用层（任意语言，经 C ABI FFI 绑定）                             │
├───────────────────────────────────────────────────────────────────┤
│ FFI 边界层（C ABI：句柄/错误码/事件回调）                         │
├───────────────────────────────────────────────────────────────────┤
│ 统一层：数据模型 / 会话 / 事件总线 / 状态簿记                     │
│         语音 / 消息 / 管理 / 横切基础设施                         │
├────────────────┬────────────────┬────────────────┬────────────────┤
│ TS3 驱动       │ KOOK 驱动      │ OOPZ 驱动      │ Discord 驱动   │
│ (客户端协议    │ (REST + WS     │ (逆向 REST+WS  │ (Gateway WS +  │
│  + ServerQuery │  + RTP 推流)   │  + Agora 桥)   │  REST + 语音   │
│  管理驱动)     │                │                │  UDP/DAVE)     │
├────────────────┴────────────────┴────────────────┴────────────────┤
│ 平台扩展层：Ts3Ext / KookExt / OopzExt / DiscordExt trait         │
└───────────────────────────────────────────────────────────────────┘
```

### 0.1 语言边界与多语言复用（C ABI 约束）

交付形态：核心 crate 以 `crate-type = ["cdylib"]` 编译为 `.so` / `.dll` / `.dylib`，对外暴露**稳定 C ABI**；各语言的包装层（Python ctypes/cffi、C# P/Invoke、JNI、Node N-API 等）按需另建，不在核心库范围。统一层与驱动的接口设计需满足以下跨语言约束：

- **对象模型**：一切对象以不透明句柄（handle）跨边界传递，不暴露 Rust 类型；生命周期由显式 `create / free` 函数管控。
- **数据布局**：字符串 = UTF-8 指针 + 长度；列表 = 指针 + 数量；PCM 音频缓冲 = 指针 + 长度（§6 的 `AudioSource`/`AudioSink` 按 C 函数指针回调供数/受数）。
- **错误**：统一错误分类（§12）映射为跨语言错误码 + 错误消息获取函数；不跨边界抛异常。
- **事件**：跨边界两种方式——注册 C 函数指针回调，或轮询式取事件（对带 GC 的语言更安全，优先支持）。
- **异步**：tokio 运行时封装在库内；跨边界只暴露阻塞调用与回调，不暴露 Future。
- **线程**：逐函数声明线程约束（哪些可在任意线程调用、哪些须单线程），文档化。
- **版本化**：导出 `univox_version()` 与 ABI 版本号，供绑定层在加载时校验兼容性。

---

## 1. 核心数据模型

统一数据模型，实体用平台无关 ID 关联；平台特有字段放各驱动的扩展结构。

| 实体 | 字段（统一部分） | 备注 |
|------|------|------|
| `Platform` | 枚举：`Ts3`、`Kook`、`Oopz`、`Discord`（预留 `Ts6`） | |
| `Id<T>` | 平台内主键的统一包装（内部字符串化，兼容 TS3 数字 ID 与 KOOK/OOPZ/Discord 字符串 ID，即 Discord 雪花 ID） | 具体化：`ServerId` / `ChannelId` / `MemberId` / `MessageId` / `RoleId` / `DbId` |
| `Server` | `id`、`name`、`icon`、`description/公告（host_message）`、`member_count`、`member_limit`、`created_at`、`version/platform`（TS3 `serverinfo`）、`region`（KOOK）、`host_banner`（TS3） | TS3=virtual server / KOOK=服务器(guild) / OOPZ=域(area) / Discord=服务器(guild) |
| `Channel` | `id`、`server_id`、`kind: Voice / Text`、`name`、`parent_id + order`（层级 + 同级排序）、`topic/description`、`password_protected`、`user_limit`、`is_default`、`persistence: Permanent / SemiPermanent / Temporary`、`position` | TS3 频道同时承载语音与文字（kind 双记）；OOPZ 语音房 = Voice；Discord=Text/Voice + 分类（Category 映射为父频道），公告/舞台/论坛频道归扩展层 |
| `Channel`（扩展字段） | `codec + codec_quality`（TS3：Speex×3 / CeltMono / OpusVoice / OpusMusic）、`needed_talk_power`、`delete_delay`、`phonetic_name`、`banner_gfx_url/mode`、`forced_silence`、`max_family_clients` | `Ext` |
| `Member` | `id`、`server_id`、`nickname`、`avatar`、`is_bot`、`online: Online / Idle / Offline / Invisible`、`role_ids`、`channel_id`（所在语音/文字频道）、`joined_at` | Discord=服务器成员（guild member） |
| `MemberState` | `input_muted`、`output_muted`、`deafened`、`away(+message)`、`speaking`、`talk_power`、`channel_commander` ★、`priority_speaker` ★、`recording` ★ | TS3 `clientlist`/`clientinfo` 变量；★ 为 `clientupdate` 运行时可设字段；Discord 由 `VOICE_STATE_UPDATE`/presence 驱动 |
| `Member`（扩展字段） | `uid`、`db_id`、`country`、`version/platform`、`total_connections`、`created/last_connected`、`idle_time`、`badges`、`my_team_speak_id`、`ip`（需权限） | TS3 `Ext` |
| `Message` | `id`、`target: Channel / Direct / Server`、`author`、`content`、`mentions`、`reference`（引用）、`attachments`、`created_at`、`edited_at` | |
| `Role` | `id`、`name`、`color`、`position`、`permissions` | TS3=服务器组/频道组；OOPZ 单角色模型（一人一角色）标注；Discord=角色（颜色/位置/权限位图，层级按 position） |
| `VoiceState` | `member_id`、`channel_id`、`self_mute`、`self_deaf`、`speaking` | Discord 由 `VOICE_STATE_UPDATE` 驱动 |
| `SelfMember` | 当前会话自身在服务器内的视图（可自我更新，见 §9） | |

---

## 2. 连接与会话生命周期

### 2.1 连接配置 `ConnectOptions`（builder 模式）

| 配置项 | 说明 | 平台 |
|--------|------|------|
| `address` | 服务器地址（host:port、邀请码/连接串） | TS3 支持地址别名解析（§11）；KOOK/OOPZ/Discord 由凭据决定 |
| `credential` | 见 §3，按平台注入 | 全部 |
| `nickname` | 初始昵称 | TS3（clientinit）/ OOPZ（账号昵称）/ KOOK、Discord（bot 自带） |
| `initial_channel` | 连接后直接进入的频道（`ChannelId` 或路径 + 频道密码） | TS3 ★（`clientinit` 初始频道） |
| `initial_state` | 初始 `input_muted / output_muted / away` | TS3 |
| `version_spoof` | 客户端版本伪装（TS3 `Version` 枚举 / OOPZ `clientVersion` + 伪装头） | TS3 / OOPZ |
| `identity_security_level` | 连接前要求的安全等级（不足则自动提升，见 §3） | TS3 |
| `server_uid_pin` | 校验预期服务器 UID，防 DNS 劫持 | TS3 |
| `network` | 本地绑定地址、代理（SOCKS/HTTP）、超时、DNS 解析器注入 | 全部 |
| `reconnect` | 重连策略（次数/退避曲线/抖动） | 全部 |
| `bookkeeping` | 状态簿记开关与范围 | 全部 |
| `intents` ★ | Gateway 事件订阅范围（Intents 位集）；特权 intents 需在开发者后台预先开启，未开启时相关能力降级并明确报错 | Discord |

### 2.2 生命周期状态机

```
Created → Connecting → Authenticating → Handshaking → Connected
                ↑                                   │  │
                └────── Reconnecting ←──────────────┘  │（永久性失败）
                                                       ▼
                                                  Disconnected
```

- 会话状态事件：`Connecting`、`Authenticating`、`Connected`、`TemporarilyDisconnected(reason)`、`Reconnecting`、`Reconnected`、`Closed(reason)`。
- **临时断开原因**（TS3）：`Timeout`（网络超时，自动重连并恢复频道/静音状态/频道订阅）、`ServerStop`（服务器重启，等待恢复）；被踢/被封**不**自动重连。
- 永久断开原因：鉴权失败、被踢、被封、服务器删除、主动断开。

### 2.3 心跳与保活

| 平台 | 机制 |
|------|------|
| TS3 | 语音/控制连接低优先级 keepalive 包；ServerQuery 空闲时以 `whoami`/`version` 作保活 |
| KOOK | WS 网关 30s PING（±5s 抖动，携带最大已处理 `sn`），6s 无 PONG 判死；HELLO 6s 超时 |
| OOPZ | WS 首包即鉴权（`event 253`），心跳 `event 254`，SDK 侧自动重连 + JWT 续期 |
| Discord | Gateway `HELLO` 下发 `heartbeat_interval`（≈41.25s）→ 定时心跳（携带最后序号 `seq`），未收到 Heartbeat ACK 判死；语音网关独立心跳（≈5s + 递增 nonce）；载荷 zlib-stream / zstd-stream 压缩可选 |
| KOOK 语音 | 推流期间每 45s `POST /voice/keep-alive`，防止静音期资源回收 |

### 2.4 断线恢复（Resume）

| 平台 | 恢复机制 |
|------|----------|
| KOOK | `?resume=1&sn=<已处理序号>&session_id=<旧会话>` → 服务端重放缺失事件；`sn` 可持久化跨进程重启续传；恢复失败收到 `RECONNECT(40106/40107/40108)` 则全量重建 |
| OOPZ | 自动重连 + JWT 自动续期（刷新阈值 300s、指数退避重登），重连后按域重新订阅 |
| Discord | 断线后 `RESUME`（`session_id` + 最后处理 `seq`）→ 服务端重放缺失事件；收到不可恢复的 `INVALID_SESSION` → 退避后重新 `IDENTIFY` 全量重建；`seq` 可持久化跨进程续传 |
| TS3 | 超时自动重连，恢复频道位置、静音状态、频道订阅，并重新提升 identity 安全等级 |
| 通用 | 恢复失败 → 自动降级为全量重建（重新拉取服务器状态簿记） |

### 2.5 断开与会话管理

- `disconnect(DisconnectOptions { reason, message })` —— 带原因离开（TS3 `clientdisconnect` reasonid）。
- `SessionManager`：创建/枚举/按标签查找会话；每会话独立事件流、状态簿记、凭据、限速桶；支持运行时增减会话。
- 连接统计 `ConnectionStats`：RTT、丢包率、带宽（TS3 `clientrequestconnectioninfo` / `serverrequestconnectioninfo`）、KOOK `sn` 处理延迟、重连次数。
- ★ 反洪水：TS3 `client is flooding` 错误识别 → 自动退避并上报（见 §12）。

---

## 3. 身份与凭据

统一 `CredentialProvider` 抽象：向驱动提供并按需刷新凭据；配 `CredentialStore` 做本地加密持久化。

| 凭据类型 | 内容 | 平台 |
|----------|------|------|
| `Ts3Identity` ★增强 | ECC P-256 keypair + hash-cash 计数器；`create()` / `level()` / `upgrade_level(target)`；TS3 客户端格式字符串互转（导入现有 identity）；连接中自动提升安全等级并发出 `IdentityLevelIncreasing/Increased` 事件 | TS3 |
| `Ts3QueryLogin` ★ | ServerQuery 账号密码（`login`）；可经客户端协议申请（`clientsetserverquerylogin`） | TS3 |
| `KookBotToken` | `Authorization: Bot <token>`（REST + WS URL 参数） | KOOK |
| `KookOauth2` | authorization_code 换取/刷新 token（`Bearer`） | KOOK |
| `KookWebhookSecret` | verify_token + AES-256-CBC encrypt key（webhook 模式） | KOOK |
| `OopzAccount` ★ | 手机号/密码/短信登录 → `signature` / JWT；RSA PKCS1v15 + SHA256 请求签名；`deviceId` / `clientMessageId` 生成；JWT 自动续期（阈值 300s、退避重登） | OOPZ |
| `DiscordBotToken` | REST `Authorization: Bot <token>` + Gateway `IDENTIFY` token；配套 intents 声明（§2.1） | Discord |

`CredentialStore` trait：`get / put / remove`，建议实现加密存储（identity、token、sn 序号、signature 的落盘）。

---

## 4. 服务器状态簿记（Bookkeeping）

- **内存镜像**：`Book { self_member, server, channels(树), members, roles, voice_states }`。
- **属性级变更驱动**：`PropertyAdded / PropertyChanged / PropertyRemoved`（参照 `ts-bookkeeping` 的 `PropertyId` 模型），附带 `invoker`（操作者）与 `extra`（原因，如"加入"与"因订阅可见"的区分）。
- **数据来源**：TS3 全量推送 + 订阅范围；KOOK/OOPZ REST 全量拉取 + WS 增量修正；Discord `GUILD_CREATE` 全量推送 + REST 补拉（成员列表分页 / `REQUEST_GUILD_MEMBERS` chunk，需 `GUILD_MEMBERS` intent）+ WS 增量修正（`CHANNEL_*` / `GUILD_MEMBER_*` / `PRESENCE_*` / `VOICE_STATE_*`）。
- **查询 API**：`server()`、`channel(id)`、`channels()`、`member(id)`、`members()`、`channel_tree()`（渲染层级树）、`find_channel(name)`、`find_member(name)`。
- **一致性控制**：TS3 频道树订阅范围（§11）；OOPZ 按域订阅（`event 249`）；Discord 成员/在线状态可见性由 intents 决定；重连/恢复失败后全量重建。
- **可关闭**：低内存机器人可关簿记，仅走事件流与按需 REST 查询。

---

## 5. 事件系统

### 5.1 事件总线

- 双消费模式：`async Stream` 订阅（参照 `tsclientlib` 必须轮询的事件流）+ 回调注册。
- 每会话独立总线；订阅时可用**事件类型 / 服务器 / 频道**过滤器本地过滤。
- 平台订阅差异的适配：KOOK 无 intents（全量推送，本地过滤）；OOPZ 需显式按域订阅（自动随簿记范围发送 `event 249`）；TS3 用 `servernotifyregister`（ServerQuery 侧）或频道订阅（客户端侧）；Discord 用 Gateway Intents 位集声明（`GUILDS`、`GUILD_MESSAGES`、`GUILD_VOICE_STATES` 等，特权 intents `MESSAGE_CONTENT` / `GUILD_MEMBERS` / `GUILD_PRESENCES` 需开发者后台开启），服务端按位过滤，本地仍可再过滤。

### 5.2 统一事件集

| 类别 | 事件 |
|------|------|
| 连接 | `Connected`、`TemporarilyDisconnected`、`Reconnected`、`Closed`、`IdentityLevelIncreased`（TS3） |
| 服务器 | `ServerUpdated`、`ServerEdited`、`HostMessageChanged`、`SelfRemoved`（被移出/服务器解散：KOOK `deleted_guild`、OOPZ `area.update`、Discord `guild_delete`/`guild_update`） |
| 频道 | `ChannelCreated`（KOOK `added_channel`、OOPZ `event 25`、TS3 属性新增、Discord `channel_create`）、`ChannelUpdated`（Discord `channel_update`）、`ChannelDeleted`（Discord `channel_delete`）、`ChannelMoved`（Discord 随 `channel_update` 位置/父级变化） |
| 成员 | `MemberJoined`（加入服务器：KOOK `joined_guild`、Discord `guild_member_add`）、`MemberLeft`（Discord `guild_member_remove`）、`MemberOnline/MemberOffline`（KOOK `guild_member_online/offline`、OOPZ `event 27`、Discord `presence_update`）、`MemberUpdated`（Discord `guild_member_update`）、`RoleAssigned/RoleRevoked`（OOPZ `event 52`、Discord `guild_member_update` 角色位变化） |
| 语音 | `SelfVoiceJoined/SelfVoiceLeft`（含被移动区分）、`MemberVoiceJoined/MemberVoiceLeft`（KOOK `joined_channel/exited_channel`、OOPZ `event 19/20`、Discord `voice_state_update`）、`SpeakingStarted/SpeakingStopped`（Discord 语音网关 `SPEAKING` / 接流侧 ssrc 活跃）、`TalkPowerRequested`（TS3）、`AudioCanSendChanged/AudioCanReceiveChanged` |
| 消息 | `MessageCreated`、`MessageEdited`（KOOK `updated_message`、OOPZ `event 56/57`、Discord `message_update`）、`MessageDeleted`（撤回；Discord `message_delete`/`message_delete_bulk`）、`ReactionAdded/ReactionRemoved`（Discord `message_reaction_add/remove`）、`PinnedMessageChanged`（Discord `channel_pins_update`）、`ButtonClicked`（KOOK `message_btn_click`、Discord `interaction_create`，扩展层） |
| 管理 | `MemberKicked`、`MemberBanned`（Discord `guild_ban_add/guild_ban_remove`，踢出经审计日志）、`TextMuteChanged`（OOPZ `event 12`、Discord timeout 经 `guild_member_update`）、`VoiceMuteChanged`（OOPZ `event 11`、Discord server mute 经 `voice_state_update`）、`ClientMoved`（被管理员移动；Discord `voice_state_update` 由他人变更） |
| 其他 | `FriendRequest`（OOPZ `event 2`，扩展层）、`RawEvent`（未映射透传） |

### 5.3 原始事件映射表

文档化每个平台的原生事件 → 统一事件的映射：TS3 属性变更流（`PropertyId` 全集）、KOOK 约 35 个系统事件（`type=255` + `extra.type`）、OOPZ 约 24 个整数事件 ID、Discord Gateway dispatch 事件全集（`READY`、`GUILD_*`、`CHANNEL_*`、`MESSAGE_*`、`VOICE_*`、`INTERACTION_*` 等 40+ 类型）。未映射事件一律经 `RawEvent { platform, payload }` 透传。

---

## 6. 语音模块

### 6.1 控制面

- `join_voice(channel, password?)` / `leave_voice()`；被移动/被踢出语音频道事件。
- 语音状态簿记：谁在哪个频道、静音/闭麦状态。
- **能力分级**（驱动声明）：`FullDuplex`（TS3 / Discord，收发双向）/ `PushOnly`（KOOK 公开 API 仅推流）/ `RtcBridge`（OOPZ）。

### 6.2 发送端

| 环节 | 内容 |
|------|------|
| `AudioSource` trait | 应用提供 **f32 PCM 48kHz** 拉取式采样流（单声道/立体声） |
| 编码 | Opus（TS3：`OpusVoice` / `OpusMusic` 可选；KOOK：libopus 48k；Discord：libopus 48k） |
| TS3 传输 | 语音 UDP 包（`AudioData::C2S`），每帧 ≤960 样本驱动 |
| KOOK 传输 | `POST /voice/join` 换取 `ip/port/ssrc/payload_type/rtcp_mux/bitrate` → **RTP/UDP 推流**；码率超约 120% 被掐断；地址与源 IP 绑定 |
| OOPZ 传输 | REST 换 `rtc_token + rtc_channel_name` → Agora RTC 进房 + UID 绑定心跳（桥式，扩展层封装细节） |
| Discord 传输 | 语音网关 WSS 信令：`VOICE_STATE_UPDATE`（自身进房/静音标志）+ `VOICE_SERVER_UPDATE`（token/endpoint/ssrc）→ 建立语音 WSS → UDP 发现（IP discovery）→ `SELECT_PROTOCOL`（dvp）；媒体面 RTP/UDP，**AEAD 加密**（`aead_xchacha20_poly1305_rtpsize` / `aead_aes256_gcm_rtpsize`，旧 `xsalsa20_poly1305` 已废弃），`SPEAKING` 指示说话状态；收发双向 |
| 流控制 | 播放结束/静音期的保活（KOOK 45s keep-alive）、资源释放（KOOK `/voice/leave`、Discord 语音网关断开 + `leave_voice`） |

### 6.3 接收端

- 分用户队列：每发言成员独立 `AudioQueue`。
- 抖动缓冲（TS3）：包序号排序、乱序/重复拒绝、**FEC 丢包恢复**（≤3 包）、自适应缓冲（≤0.5s / 50 包）。
- 混音器：多路 → 立体声 f32；**每用户音量**；返回"停止说话"成员列表（`fill_buffer` 模式，参照 tsclientlib `AudioHandler`）。
- `AudioSink` trait：应用消费解码后 PCM（自行播放或采集）。
- 平台差异：KOOK 公开 API **不支持收流**（能力 ✗）；OOPZ 经 RTC 桥可收；Discord 支持收流（每用户独立 ssrc，逐成员队列 + 抖动缓冲/混音器直接复用 univox-voice；语音必须实现 DAVE，见 §6.8）。
- 说话检测：`SpeakingStarted / SpeakingStopped` 事件（TS3 基于音频流、OOPZ 基于 RTC 指示器、Discord 基于语音网关 `SPEAKING` 事件）。

### 6.4 Whisper（定向耳语）— TS3 ✓

- 发送 whisper 至**目标列表**（成员/频道混合，≤65 目标；受 `i_client_whisper_power` / 目标 `i_client_needed_whisper_power` 门控——权限不足时服务器静默丢弃，`Ok` 只代表已发送）。
- 接收 whisper 单独标记（`S2CWhisper`），可区分普通语音（`SpeakingStarted/Stopped.whispering`）。
- **Whisper 列表管理**：客户端本地目标列表的增删查与激活（原版客户端功能，tsclientlib 未实现）。

### 6.5 ★ 3D 定位音频 — TS3

- Listener 位置/朝向属性 + **每成员 voice position**；纯客户端本地渲染（不经过服务器）。
- 混音器支持按位置衰减（tsclientlib 未实现）。

### 6.6 ★ 本地音频操作 — TS3

- 对单个成员的本地音量调节 / 本地静音与取消（`clientmute` / `clientunmute`，含"拒绝所有 whisper"本地开关）。
- 本地录音标记与录音事件（`client_is_recording`）。

### 6.7 便利层

- `play_file(path) / play_url(url)`：封装常见"放歌"流程（转码建议由应用侧 ffmpeg 管道完成，库只收 PCM）。
- `can_send_audio() / can_receive_audio()` 查询 + 变更事件（TS3：静音/away/talk power 不足/临时断开任一即变 false；Discord：语音网关断开/DAVE 会话失效即变 false）。

### 6.8 ★ DAVE 端到端加密 — Discord

- **强制**：2026-03 起 Discord 所有（非舞台）语音通话强制 DAVE-capable 客户端——未实现 DAVE 的语音会话被语音网关拒绝（close 4016/4017），收发皆然。
- **协议**：由语音网关 opcodes 驱动的 MLS 派生协议——`ML-KEM` 密钥封装握手、按成员加入/离开的密钥过渡（upgrade/downgrade）、逐 RTP 帧认证加密（AES-256-GCM / AES-128-GCM）。
- **实现**：跟随官方 [discord/dave-protocol](https://github.com/discord/dave-protocol) 规范，全部隔离在 `univox-discord` 驱动内；`univox-voice` 管线不变（进入混音/编码前已完成解密，出编码后完成加密）。
- **风险**：见 §14.2（实现复杂度最高的一项，参考 discord.py / serenity / `@snazzah/davey` 的实现）。

---

## 7. 消息模块

### 7.1 发送目标

| 目标 | 说明 | 平台 |
|------|------|------|
| `Channel` | 频道消息 | 全部（TS3 频道消息 targetmode=2） |
| `Direct` | 私信 | 全部（TS3 targetmode=1；KOOK 需先建私信会话 `/direct-message/create`；OOPZ `sendImMessage`；Discord DM/群组 DM 频道） |
| `Server` | 服务器广播 | TS3（targetmode=3）；KOOK/OOPZ/Discord ✗ |
| `Poke` | 戳一戳（通知型消息） | TS3（`clientpoke`） |
| `Global` | 全实例广播 | TS3 `gm`（`Ext`，query 级） |

### 7.2 内容抽象 `MessageContent`

- `Plain(text)`。
- `Rich`：公共富文本子集（粗体/斜体/下划线/链接/行内代码/代码块/引用/颜色）→ 映射到各平台语法（KOOK KMarkdown、OOPZ markdown、TS3 BBCode 风格、Discord markdown）；**超集内容自动降级为纯文本**并在返回值标注。
- `Mentions`：`@all` / 成员列表（OOPZ `(met)<uid>` 语法、KOOK `(met)<id>` 语法、Discord `<@id>` / `@everyone` 自动转换）。
- `Reference`：引用回复（KOOK `referenceMessageId`、OOPZ 同名、Discord `message_reference`；TS3 ✗）。
- `Attachments`：图片/视频/文件/语音，经资产上传通道（KOOK `/asset/create`、OOPZ COS 签名直传、TS3 文件传输、Discord multipart 直传 CDN）。
- `Card`：KOOK 卡片消息（`Ext`）；Discord Embeds / Components（`Ext`）→ 按钮/选择菜单点击经 interaction 回流为 `ButtonClicked`。

### 7.3 消息操作与事件

| 功能 | KOOK | OOPZ | Discord | TS3 |
|------|------|------|---------|-----|
| 编辑 | ✓ `/message/update` | ✓ `event 57/56` | ✓ | ✗ |
| 撤回/删除 | ✓ `/message/delete` | ✓ `recall`（`event 6/8`） | ✓ | ✗ |
| 表情回应 reactions | ✓ add/delete/list（私信同） | ✓ `gimReaction/imReaction` | ✓（含自定义表情） | ✗ |
| 置顶 | ✓ pin/unpin | ✓ `messageTop` | ✓ | ✗ |
| 历史 | ✓ `/message/list` 等 | ✓ `/client/v1/list/v1/message` | ✓ | ✗ |
| 离线消息 | — | — | ✗ | ★ ✓ `messageadd/list/get/updateflag/del` |
| 输入中指示 | ✗ | ✗ | ✓ `TYPING_START` | ✓ `clientchatcomposing/closed` |
| 按钮点击事件 | ✓ `message_btn_click` | ✗ | ✓ interaction（`Ext`） | ✗ |

---

## 8. 频道与服务器管理

### 8.1 频道管理

- `create_channel(ChannelOptions)`：名称、类型（语音/文字）、父频道 + 排序、主题/描述、密码、人数上限（含家族上限/无限/继承）、默认频道标志、持久性（永久/半永久/临时 + `delete_delay`）、语音编解码器与音质、音标名 ★（`channel_name_phonetic`）、横幅 ★（TS3）。
- `edit_channel / delete_channel(force?) / move_channel(重排序)`。
- 频道权限覆写：KOOK `/channel-role/*`（create/index/update/delete/sync）、TS3 `channeladdperm/channeldelperm/channelpermlist` + 每(频道,成员)权限 ★（`channelclientaddperm/...`）、Discord permission overwrites（按角色/成员的 allow/deny 位图）、OOPZ 有限（标注）。
- Discord 频道类型全集：文字/语音/分类/公告/舞台/论坛频道 CRUD ✓，排序与父级调整 ✓（子区 Threads 见 §11.4）。
- 频道信息查询：`channellist`（选项 `-topic -flags -voice -limits -icon -banners`）、`channelinfo`、`channelfind` ★。

### 8.2 服务器管理

- `get_server() / edit_server()`：名称、公告（hostmessage + 模式）、hostbanner、密码、人数上限、默认频道/组、antiflood 参数 ★、所需 identity 安全等级、编解码加密模式、临时频道删除延迟。
- `leave_server()`（KOOK `/guild/leave`、OOPZ quit、TS3 断开、Discord 退群）。
- **邀请链接**：KOOK `/invite/create|list|delete`（TS3 无对应概念——用服务器地址 + 密码 / privilege key 承担；OOPZ 未公开；Discord `/invites` 全功能 ✓——创建/列表/撤销，含有效期与使用次数）。
- ★ TS3 快照：`serversnapshotcreate / serversnapshotdeploy`（频道 + 权限整体备份/恢复，支持 v2/v3）。
- ★ TS3 临时密码：`servertemppasswordadd / list / del`（限时频道/服务器密码）。

### 8.3 成员移动

- `move_member(member, channel)`：TS3 `clientmove`、KOOK `/channel/move-user`（批量）、OOPZ `dragInto`、Discord `PATCH /guilds/{id}/voice-states`（移动/断开语音，需权限）。
- `kick_from_channel(member)`（TS3 reasonid=4；Discord 断开其语音连接）。

---

## 9. 成员与角色权限

### 9.1 成员查询

| 功能 | 说明 |
|------|------|
| `member(id) / members()` | 列表可带选项：UID/away/语音/组/时间/国家/IP/徽章（映射 TS3 `clientlist` 选项；KOOK `/guild/user-list`、OOPZ `/area/v2/members`、Discord 成员列表分页/`REQUEST_GUILD_MEMBERS` chunk） |
| `member_detail(id)` | 详情：注册时间/最后上线/总连接数/版本/平台/国家/头像（TS3 `clientinfo`；KOOK `/user/view`；OOPZ `personDetail`） |
| ★ `member_find(name)` | 按名字查用户（TS3 `clientfind`） |
| 在线状态 | KOOK `/user/get-online-status`、OOPZ `event 27` |
| ★ 频道内成员 | TS3 `channelclientlist / channelclientvariable`（频道内各成员的频道级状态） |

### 9.2 ★ clientdb 与 ID 映射 — TS3

- **clientdb 管理**：`clientdblist / clientdbfind / clientdbinfo / clientdbedit / clientdbdelete`（服务器账号数据库）。
- **ID 映射**：`clientgetids`（uid→clids）、`clientgetuidfromclid`、`clientgetdbidfromuid`、`clientgetnamefromuid`、`clientgetnamefromdbid`。
- **自定义字段**：`customsearch / custominfo`（用户资料扩展键值）。

### 9.3 角色与组

| 功能 | TS3 | KOOK | OOPZ | Discord |
|------|-----|------|------|---------|
| 列表/详情 | ✓ `servergrouplist / channelgrouplist` | ✓ `/guild-role/list` | ✓（单角色模型） | ✓ `/guilds/{id}/roles` |
| 增/删/改 | ✓ add/del/edit/rename/copy | ✓ create/update/delete | ◐ 重命名 | ✓ create/modify/delete |
| 权限编辑 | ✓ `servergroupaddperm/delperm/permlist`、auto 系列 | ✓ `/guild-role/*` | ✗ | ✓ permissions 位图 |
| 成员授予/撤销 | ✓ `servergroupaddclient/delclient` | ✓ `grant/revoke` | ✓ `role/editUserRole` | ✓ `PUT/DELETE /guilds/{id}/members/{uid}/roles/{rid}` |
| 组成员列表 | ✓ `servergroupclientlist`、`servergroupsbyclientid` | — | — | ✓ `GET /guilds/{id}/roles/{rid}/members` |
| 频道组指派 | ★ ✓ `setclientchannelgroup`、`channelgroupclientlist` | — | — | — |

### 9.4 权限系统

- **权限字典**：全量权限表（TS3 `permissionlist`，约 400 项，含分组/描述）、`permidgetbyname`。
- **查询**：`perm_find`（谁有该权限）、`perm_get`、`perm_overview`（某成员的完整授权视图，含继承/取反/skip）、`permreset` ★（重置到默认）。
- **按成员授权（TS3）**：`clientaddperm / clientdelperm / clientpermlist`（绕过组直接对单个成员授权）。
- **推送**：服务器主动下发自身所需权限（`clientneededpermissions`）与**权限提示 hints**（能否执行某动作的快速判断）。
- **检查 API**：`can(action, target) -> bool` —— 优先用 hints/本地簿记判定，必要时发查询；Discord 权限完全本地可算（成员角色位图 + 频道 overwrites + @everyone 继承），`can()` 无需网络请求（管理者视角的操作权限仍以 REST 403 兜底上报）。

### 9.5 发言权（Talk Power）

- ★ 请求发言：`request_talk_power(message)`（`clientupdate client_talk_request`）→ 事件 `TalkPowerRequested`（管理员侧）。
- ★ 授予发言：向请求者授予临时 talk-power 服务器组（`servergroupaddclient`），到期自动收回。
- 频道门槛：`channel_needed_talk_power` 读取与编辑。

---

## 10. 管理与安全（Moderation）

| 功能 | TS3 | KOOK | OOPZ | Discord |
|------|-----|------|------|---------|
| 踢出（频道/服务器，带原因） | ✓ `clientkick`（reasonid 4/5） | ✓ `/guild/kickout`、`/channel/kickout`（踢出语音房） | ✓ `/area/v3/remove` | ✓（服务器级 `DELETE /guilds/{id}/members/{uid}`；语音房用移动/断开） |
| 封禁 | ✓ `banadd`（IP/名字/UID/myTSID/硬件 ID + 时长）/`banlist`/`bandel`/`bandelall`/`banclient`（现抓现封） | ✓ `/blacklist/*` | ✓ `block/unblock/blocks` | ✓ `/guilds/{id}/bans`（create/list/remove，含时限与原因） |
| 文字禁言 | ✗（无原生，可经权限组模拟） | ◐ 全员禁言 `/guild-mute/*` | ✓ `disableText/recoverText`（`event 12`） | ✓ timeout（`communication_disabled_until`，文字+语音一并生效） |
| 语音禁麦 | ◐ 经服务器组/权限实现 | ◐ `/guild-mute`（voice 类） | ✓ `disableVoice/recoverVoice`（`event 11`） | ✓ server mute/deaf（`PATCH` voice-state） |
| 移动 | ✓（§8.3） | ✓ | ✓ | ✓ |
| 投诉 | ✓ `complainadd/list/del/delall`（`Ext`） | ✗ | ✗ | ✗ |
| ★ 反洪水退避 | ✓ 识别 `client is flooding` → 自动退避 | —（RFC6585 限速，见 §12） | — | —（REST 限速严格，见 §12） |

---

## 11. 平台扩展层

非通用能力不进统一 API，通过 `Ts3Ext / KookExt / OopzExt / DiscordExt` 扩展 trait 暴露；能力矩阵标 `Ext`。

### 11.1 TS3 扩展（`Ts3Ext`）

**★ ServerQuery 管理驱动**（独立于客户端协议的第二条接入路径；兼作未来 TS6 管理面）：
- 连接：`login/logout/use/whoami/quit`；传输 telnet（10011）与 SSH（10022，RSA hostkey）。
- 事件：`servernotifyregister / unregister`（事件类型 `server / channel / textserver / textchannel / textprivate`）→ 汇入统一事件总线。
- ★ 查询账号管理：`queryloginadd / querylogindel / queryloginlist`。
- query 级独占能力：`serverlist / servercreate / serverdelete / serverstart / serverstop / serverprocessstop`、`instanceinfo/instanceedit`、`hostinfo`、`bindinglist`、`serveridgetbyport` ★、`gm` 全局广播、`logview` 过滤查询。

**★ 插件命令转发**：`send_plugin_command(payload, target: Single / CurrentTab / Clients / All)` + `PluginCommandReceived` 事件（`plugincmd`，客户端间经服务器中继）。

**★ 头像 / 图标 / 横幅**：`avatar_set / avatar_get / avatar_remove`（文件传输 `/avatar_<dbid>`）、`icon_upload / icon_download`（`icon_<iconid>`）、频道横幅设置。

**★ 运行时自身状态**（`clientupdate` 全字段）：频道指挥官、优先说话者、录音中、徽章（含签名徽章）、客户端描述、`meta_data`、默认 token、运行时改名/away/静音。

**★ 密码本地校验**：`verify_server_password / verify_channel_password`（`hashpassword` 本地哈希比对，不发服务器）。

**其余**：
- privilege key（特权密钥）：`privilegekeyadd/use/delete/list`（`token*` 兼容别名）。
- 文件传输完整面：`upload / download`（断点续传 seek、覆盖开关）、`list_files`（`ftgetfilelist`）、`file_info`、`delete_file`、`create_dir`、`rename_file`、传输列表（`ftlist`）、`stop_transfer`；上传/下载配额属性。
- 频道树订阅：`subscribe / unsubscribe / subscribe_all / unsubscribe_all` + 订阅变更事件。
- 日志：`add_log`（`logadd` 写自定义日志行）。
- myTeamSpeak 集成变量：`client_myteamspeak_id`、签名徽章。
- ★ 客户端本地信息查询：服务器/频道连接信息（`serverconnectinfo / channelconnectinfo`）、当前声音处理器 ID（`currentschandlerid`）、成员空闲时间（`client_idle_time`）。
- 解析器：SRV DNS（`_ts3._udp.<host>`）+ TSDNS（TCP 41144）地址解析、昵称解析。
- 协议级：identity 安全等级提升（§3）、License 类型读取、连接加密。

### 11.2 KOOK 扩展（`KookExt`）

- **webhook 接入模式**（与 WS 互斥）：challenge 验证回显、AES-256-CBC 解密。
- **OAuth2** 授权码流程（scope：`get_user_info`、`get_user_guilds`）。
- **CardMessage 构建器**（10 类模块 + 按钮 → `ButtonClicked` 事件）。
- KMarkdown 完整语法。
- 表情管理：`/guild-emoji/list|create|update|delete`。
- 好友：`/friend`、`/friend/request|handle-request|delete|block|unblock`。
- 游戏状态：`/game/create|update|delete|activity`。
- 帖子/线程：`/thread/*`、`/post`、`/category/list`。
- 其他：亲密度 `/intimacy/*`、服务器徽章 `/badge/guild`、模板 `/template/*`、用户资料 `/user/view|me`、在线用户 `/user/online|offline`、语音列表 `/voice/list`、成员当前语音房 `/channel-user/get-joined-channel`、服务器头衔 `/guild/nickname`、助力记录 `/guild-boost/history`。

### 11.3 OOPZ 扩展（`OopzExt`）

- **Agora 桥配置**：app_id、桥后端选择（浏览器自动化）、UID 绑定与心跳桥接、`play_url / play_file / play_bytes` 播放辅助。
- COS 媒体上传（签名直传 URL）。
- 好友：`requests / response / list`（`event 2/4`）。
- 发现页：banner / recommend。
- 用户扩展：备注名（remarkName）、等级/积分（`/user_points/v1/level_info`）、隐身（stealth）状态。
- 互动消息：`/client/v1/interaction/v1/send`、会话建立 `/client/v1/chat/v1/to`。
- 订阅管理：按域订阅/退订（`event 249`）。

### 11.4 Discord 扩展（`DiscordExt`）

- **Application Commands**：斜杠/用户/消息命令注册与更新、响应与 followup、`autocomplete`、Modal 表单（交互事件归统一事件总线）。
- **Components**：按钮/选择菜单/文本框布局 → `INTERACTION_CREATE` → `ButtonClicked` 等统一事件（§7.2）。
- **Embed 构建器**：富卡片（标题/描述/字段/图片/页脚，≤10 embed/消息）。
- **子区 Threads**：创建/归档/成员管理/列表（`thread_*` Gateway 事件并入频道事件）。
- **舞台 Stage**：stage instance 开播/结束管理（舞台频道语音面与普通语音一致，无 DAVE）。
- **表情/Sticker 管理**：guild emoji/sticker CRUD。
- **审计日志**：`GET /guilds/{id}/audit-logs`——补全管理操作的 `invoker`（谁踢的/谁封的/谁改的）。
- **Webhook**：interaction webhook 响应（免网关异步回复）、outgoing webhook 管理。
- **自身状态**：presence（在线状态/自定义活动）、昵称/头像设置。
- **语音便利层**：`play_url / play_file`（同 §6.7，转码由应用侧完成）、每用户本地音量（混音器级）、`self_mute/self_deaf` 运行时切换。

---

## 12. 横切基础设施

| 模块 | 内容 |
|------|------|
| **日志** | `tracing` 分级；协议级开关（TS3 `log_commands / log_packets / log_udp_packets` 的等价物）；各平台 REST/WS 收发日志 |
| **限速** | `RateLimiter` 抽象 + 平台默认预算：KOOK 按 RFC6585 处理 429 与重试头；OOPZ 无文档 → 保守默认 + 可调；Discord 全局 + 每路由桶（429 + `X-RateLimit-*` 头，含 `global` 标志与 bucket hash 复用）；TS3 反洪水点数模型 ★ → 指数退避 |
| **错误分类** | `Error { Network, Auth, Permission { missing }, RateLimited { retry_after }, Flood, NotFound, InvalidArgument, Audio, FileTransfer, Platform(raw) }`——统一分类 + 原始平台错误/错误码保留 |
| **配置** | 文件（TOML/YAML）与 builder 双入口；按会话覆盖（重连策略、代理、日志级别、簿记开关、音频参数） |
| **持久化** | `CredentialStore`（§3）；KOOK `sn + session_id` 可跨进程重启续传；Discord `session_id + seq` 同理；OOPZ signature/JWT 缓存；会话快照 |
| **网络** | 代理（SOCKS5/HTTP）、本地地址绑定、IPv4/IPv6、DNS 解析器注入 |
| **重试** | 幂等操作自动重试 + 退避 + 抖动；区分可重试/不可重试错误 |
| **时间** | 统一 UTC 时间戳；换算 OOPZ 微秒时标、TS3 相对时间 |

---

## 13. 平台能力矩阵

图例：✓ 完整 · ◐ 部分/受限 · ✗ 无 · `Ext` 仅扩展层 · ★ tsclientlib 未实现、本库补充规划。

| 功能 | TS3 | KOOK | OOPZ | Discord |
|------|-----|------|------|---------|
| 机器人凭据 | 客户端协议 + identity | Bot token / OAuth2 `Ext` | 账号登录（逆向） | Bot token（OAuth2 `Ext`） |
| ServerQuery/管理接口 | `Ext` ★ | — | — | —（审计日志 `Ext`） |
| 消息：频道 | ✓ | ✓ | ✓ | ✓ |
| 消息：私信 | ✓ | ✓ | ✓ | ✓ |
| 消息：服务器广播 | ✓ | ✗ | ✗ | ✗ |
| Poke/戳一戳 | ✓ | ✗ | ✗ | ✗ |
| 富文本 | ◐（BBCode 风格） | ✓ KMarkdown | ✓ markdown | ✓ markdown |
| 卡片消息 | ✗ | `Ext` ✓ | ✗ | `Ext` ✓（Embeds/Components） |
| 提及 | ✗ | ✓ | ✓ | ✓ |
| 引用回复 | ✗ | ✓ | ✓ | ✓ |
| Reactions | ✗ | ✓ | ✓ | ✓ |
| 编辑/撤回 | ✗ | ✓ | ✓ | ✓ |
| 置顶 | ✗ | ✓ | ✓ | ✓ |
| 消息历史 | ✗ | ✓ | ✓ | ✓ |
| 离线消息 | ✓（`Ext` ★） | ✗ | ✗ | ✗ |
| 输入中指示 | ✓ | ✗ | ✗ | ✓ |
| 语音：进出频道 | ✓ | ✓ | ✓ | ✓ |
| 语音：发送 | ✓ | ✓（RTP 推流） | ◐（Agora 桥） | ✓（RTP + AEAD） |
| 语音：接收 | ✓（混音/抖动缓冲） | ✗（公开 API） | ◐（桥） | ✓（依赖 DAVE，§6.8） |
| 每用户音量（本地） | ✓ ★ | ✗ | ◐ | ✓（本地混音） |
| Whisper | ✓ ★（含列表管理） | ✗ | ✗ | ✗ |
| 3D 定位音频 | `Ext` ★ | ✗ | ✗ | ✗ |
| Speaking 事件 | ✓ | ◐（仅服务器侧状态） | ◐（RTC 指示） | ✓（语音网关） |
| 频道 CRUD | ✓ | ✓ | ◐ | ✓ |
| 频道权限覆写 | ✓ | ✓ | ◐ | ✓ |
| 服务器编辑 | ✓ | ◐ | ◐ | ◐ |
| 邀请链接 | ◐（地址+密码/privilege key） | ✓ | ✗ | ✓ |
| 用户移动 | ✓ | ✓ | ✓ | ✓ |
| 角色/组管理 | ✓ | ✓ | ◐（单角色） | ✓ |
| 权限查询/检查 | ✓（含 hints） | ✓ | ◐ | ✓（本地计算） |
| 发言权请求/授予 | ✓ ★ | — | — | — |
| 踢出 | ✓ | ✓ | ✓ | ✓ |
| 封禁/黑名单 | ✓（多维度） | ✓ | ✓ | ✓（含时限/原因） |
| 文字禁言 | ✗ | ◐（全员） | ✓ | ✓（timeout） |
| 语音禁麦 | ◐（经权限组） | ◐ | ✓ | ✓（server mute/deaf） |
| 投诉 | `Ext` ✓ | ✗ | ✗ | ✗ |
| 文件传输 | ✓ | ◐（资产上传） | ◐（COS 上传） | ◐（附件 CDN 直传） |
| 头像/图标管理 | `Ext` ★ | — | — | ◐（自身头像/表情） |
| 插件命令转发 | `Ext` ★ | ✗ | ✗ | ✗ |
| 事件断线恢复 | ✓（重连重建） | ✓（sn resume） | ✓（重登+续订） | ✓（RESUME） |
| 心跳保活 | ✓ | ✓ | ✓ | ✓ |
| Webhook 接入 | — | `Ext` ✓ | — | `Ext` ✓（interaction） |

Discord 列备注：语音收发依赖 DAVE 端到端加密实现（§6.8）；消息/成员/在线状态的可见性受特权 intents 约束（§5.1，未开启时相关能力降级）。

---

## 14. 未来项与风险

### 14.1 TeamSpeak 6（预留，不实现）

- 现状（2026-09）：服务端 `v6.0.0-beta9`、客户端 `6.0.0-beta4.1`，**无公开 SDK / 协议文档**。
- 已知兼容路径：语音线路与 TS3 兼容（UDP 9987 / Opus，TS3 客户端可连 TS6 服务器）；ServerQuery 沿用 TS3 命令集（SSH 10022 / HTTP 10080 / HTTPS 10443，明文 10011 已移除）——本库的 ServerQuery 管理驱动（§11.1）设计为可直接复用。
- 其他已知：强制 myTeamSpeak 账号体系、Communities 托管服务器、聊天基于 Matrix（端到端加密）、屏幕共享协议未文档化。
- 触发条件：公开 SDK 或协议文档后，评估 `Platform::Ts6` 独立驱动（聊天面走 Matrix 客户端库的可行性）。

### 14.2 风险

| 风险 | 缓解 |
|------|------|
| OOPZ 为逆向协议：无官方保障，客户端更新即失效（版本头/签名） | 协议细节全部隔离在驱动内；版本伪装参数可配置；`RawEvent` 保证未映射信息不丢；失效时快速定位 |
| KOOK 语音仅推流且码率管控（超 ~120% 掐断/处罚） | 能力标注清晰；发送端限速与 bitrate 遵从 |
| TS3 反洪水（连接/命令过快被封 IP） | 统一限速器 + 洪水错误退避（§10/§12） |
| TS6 协议变动 | TS6 仅作未来项，不阻塞 v1 |
| KOOK webhook 与 WS 互斥 | 凭据/接入模式二选一，配置校验 |
| Discord 语音强制 DAVE E2EE（2026-03 起，未实现即被 4016/4017 拒绝）：MLS 派生 + ML-KEM 握手 + 逐帧加密，实现复杂度为本库最高单项 | 跟随官方 dave-protocol 规范（§6.8）；参考 discord.py / serenity / `@snazzah/davey`；未完成前语音能力按降级标注 |
| Discord 特权 intents（`MESSAGE_CONTENT` / `GUILD_MEMBERS` / `GUILD_PRESENCES`）需开发者后台开启；≥100 服务器需验证审核 | intents 可配置；缺失时相关能力降级并明确报错，而非静默空数据 |
| Discord REST 限速严格（全局 + 每路由桶，违规 429/封禁） | 统一限速器默认预算（§12），遵循 429 重试头 |

### 14.3 明确不做（非目标）

- 音频**设备**采集/播放（麦克风/扬声器）——应用层职责（库只收发 PCM 流）。
- GUI / 客户端 UI。
- 屏幕共享 / 视频通话（TS6 screen share 协议未公开；KOOK/OOPZ 视频不在范围）。
- 端到端加密聊天（TS6 Matrix E2E）。
- myTeamSpeak / 平台账号的注册与托管（只做已有凭据的接入）。
- **Discord 用户账号自动化（self-bot / 用户 token 接入）**——违反 Discord ToS；Discord 仅支持官方 Bot API。

---

## 附：参考来源

- TeamSpeak 3：[ReSpeak/tsclientlib](https://github.com/ReSpeak/tsclientlib)（含 `ts-bookkeeping`、`tsproto-packets`）、TS3 ServerQuery 文档（`serverquerydocs`）、ClientQuery 插件接口、[TS3 Client Plugin SDK](https://github.com/teamspeak/ts3client-pluginsdk)
- TeamSpeak 6：[teamspeak/teamspeak6-server](https://github.com/teamspeak/teamspeak6-server)（releases/docs）、[TS6 ServerQuery 文档](https://mintlify.wiki/teamspeak/teamspeak6-server/server-query/overview.md)
- KOOK：[developer.kookapp.cn](https://developer.kookapp.cn) 与 [kaiheila/api-docs](https://github.com/kaiheila/api-docs)、[TWT233/khl.py](https://github.com/TWT233/khl.py)、[gehongyan/Kook.Net](https://github.com/gehongyan/Kook.Net)、[shuyangzhang/kookvoice](https://github.com/shuyangzhang/kookvoice)
- OOPZ：[DeeChael/oopz-api-docs](https://github.com/DeeChael/oopz-api-docs)、[tangqingfeng7/Oopzbot-SDK](https://github.com/tangqingfeng7/Oopzbot-SDK)（逆向协议，无官方文档）
- Discord：[官方开发者文档](https://discord.com/developers/docs)（Gateway / Intents / Voice / Interactions / Permissions）、[discord/dave-protocol](https://github.com/discord/dave-protocol)（语音 E2EE 规范）、Rust 参考 [serenity](https://github.com/serenity-rs/serenity) / [twilight](https://github.com/twilight-rs/twilight)、[discord.py](https://github.com/Rapptz/discord.py)（语音/DAVE 实现）
