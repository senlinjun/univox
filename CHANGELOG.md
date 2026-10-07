# Changelog

## 0.1.0 — 2026-10-07

首个公开版本：**TeamSpeak 3 驱动完整交付**，经真实服务器（3.13.8）验收
（148 个测试全绿：单元 + 协议向量 + 本地真实服务器集成；另有 3 个长时/手动
回归默认 `--ignored`）。

- **univox** —— 门面 crate：单依赖获得统一抽象与全部已交付驱动；根平铺
  re-export（`univox::{ConnectOptions, Event, Ts3Driver, …}`）+ 模块别名
  `core / ts3 / proto / voice`；`voice` feature（默认开启）统一门控
  audiopus——`default-features = false` 时静态 libopus 完全退出依赖树。
- **univox-core** —— 统一数据模型（Server/Channel/Member/Message/Role）、
  Session/Driver trait、统一事件总线、状态簿记（Book）、能力声明、多会话
  管理器、凭据抽象、限速器、音频 trait。
- **univox-ts3-proto** —— TS3 线协议：包头编解码、Init1+RSA 谜题握手、
  initivexpand2 Curve25519/ECDSA 证书链、EAX 加密、分片/QuickLZ、
  P-256 identity（tsclientlib JSON 兼容）、命令序列化。
- **univox-ts3** —— 原生客户端协议驱动（UDP 9987；断线监督、自动重连、
  状态恢复）+ ServerQuery 管理驱动（raw/SSH）；消息与管理面、whisper、
  文件传输、头像/图标/横幅、临时密码、频道组、Talk Power、连接信息，
  经 `Ts3Ext` 平台扩展 trait 暴露。
- **univox-voice** —— Opus 编解码、逐成员抖动缓冲、混音器、3D 定位音频
  （距离衰减 + 立体声声像）。
- KOOK / OOPZ / Discord 驱动仍为规划（功能面与平台能力矩阵见
  docs/FEATURES.md）。

以下为发布前最后一批客户端 API 补全的明细；多项是既有抽象的接线补全
（`with_extension` 槽、`ChannelOptions.extra`、`hash_password` 此前为空置）。

### 连接与身份

- **`Ts3ConnectOptions`**（`univox_ts3::Ts3ConnectOptions`，经
  `ConnectOptions::with_extension` 传入）：服务器密码、连接时 privilege key
  （`clientinit client_default_token`，重连不重放）、可选 `server_uid_pin`
  （防 DNS 劫持）。密码一律收明文、库内哈希（base64(sha1)）。
- **`ConnectOptions::initial_channel` 从死字段变活**：`Path` /
  `PathWithPassword` 走 clientinit，`Id(cid)` 连接后 clientmove；invite
  链接携带的频道在未显式指定时也会使用。
- **`upgrade_identity_to: Option<u8>`**：connect 前自动做 hash-cash 升级
  （例如 24，无需自己算），升级后发 `Event::IdentityLevelIncreased`，
  重连沿用升级后的身份。
- **tsclientlib 身份 JSON 兼容**：`Identity::from_tsclientlib_json` /
  `to_tsclientlib_json`，格式 `{"key": base64(32B 标量), "counter",
  "max_counter"}`（tsproto 0.2 字节序，读取兼容 tomcrypt DER）。

### 事件与 roster

- **notifyclientpoke → `MessageCreated`**：`target = Poke(invoker)`、
  `author = invoker`，消费方可原样回戳。
- **roster extra 保真**：`Server.extra` 现在填充 initserver 全部剩余字段
  （ask_for_privilegekey / hostmessage_mode / flag_password 等）；
  `hostmessage_mode` 同时解析为枚举（原为硬编码 Log）；
  `notifychanneledited` 改为增量合并 extra、空 name 不覆盖（原为整行替换）；
  `notifychannelcreated/edited` 的父字段 `cpid` 现已识别（book 镜像的
  parent 不再丢失）。Member/Channel extra 本就透传整行，新增真实行
  fixture 测试锁定 client_unique_id / client_servergroups /
  channel_needed_talk_power / channel_icon_id 等字段。
- **破坏性变更：`Event::MemberLeft { reason }` 由 `String` 改为
  `MemberLeftReason` 枚举**（Left/Moved{by}/Unsubscribed/Timeout/
  ChannelKicked{by,message}/ServerKicked{by,message}/Banned{by,message}/
  ServerStop/Quit/Other(raw)），reasonid 遵循 tsdeclarations `Reason`
  枚举（6=封禁、8=主动退出——旧资料"8=踢、10=封禁"有误）。

### 传输与命令

- **流式文件传输**（`Ts3Ext`）：
  `download_file_stream` → `FileDownload { size, received, next_chunk, finish }`，
  `upload_file_stream` → `FileUpload { size, written, write_chunk, finish, abort }`。
  频道密码收明文、内部哈希。取消 = drop（上传侧自动 ftstop delete=1 删除
  半成品；已验证 socket 先关会被服务器当作完成并提交）。整缓冲
  `upload_file` / `download_file` 变为薄包装。
- **avatar**：`upload_avatar` 上传后自动 `clientupdate client_flag_avatar=<md5>`；
  新增 `download_avatar_by_uid(uid)` 与 `avatar_path(hash)`。
- **`Ts3Ext::move_channel(channel, parent, order, password)`**：同父重排
  自动走 `channeledit channel_order`（channelmove 同父报 770），跨父走
  `channelmove`。
- **类型化查询**（`Ts3Ext`）：`server_groups() -> Vec<ServerGroup>`、
  `channel_groups() -> Vec<ChannelGroup>`、`own_permissions() ->
  Vec<(String, i64)>`（1281 按空列表处理）、`subscribe_all()`。
- **`ChannelOptions.extra` 透传**：create_channel / edit_channel 把 extra
  键值作为原生命令参数（此前是死字段）——`channel_order` /
  `channel_needed_talk_power` 等编辑专用字段由此可用。

### 构建

- **`univox-ts3` 的 `voice` feature（默认开启）**：`univox-voice`（audiopus）
  改为可选依赖；`CODEC_OPUS_VOICE` 移至 `univox-ts3-proto`（univox-voice
  保留 re-export）。关闭时 `start_sending`/`start_receiving` 返回
  `Error::Unsupported`，whisper 收发不受影响。嵌入方可
  `default-features = false` 以避免与自身 opus-rs 的静态 libopus 符号冲突
  （Android cmake 风险）。
  验收：`cargo check -p univox-ts3 --no-default-features` 通过、
  `cargo tree` 无 audiopus、默认构建与 `cargo test --workspace` 不变。

### 身份回写 / counter 语义 / create_dir

- **`Ts3Session::identity() -> &Identity`**：connect（含
  `upgrade_identity_to` 升级）后的最终身份可读回；调用方应在连接后持久化
  `counter()`/`max_counter()`（hash-cash 搜索从 max_counter 续起，等级可能
  被服务器上调）。
- **counter 复用语义确认**：`client_key_offset` 是工作量证明标记而非
  服务端消耗的 nonce——tsclientlib 0.2 从不按次递增（直接发送
  `identity.counter()`），真机验证同一身份/counter 连续两次连接均成功；
  重连 supervisor 无需递增。等级被上调的场景由 connect 侧
  `upgrade_identity_to` 覆盖（真机验证：提升到 14 后无升级被拒、有升级通过）。
- **`Ts3Ext::create_dir(channel, path, password)`**（`ftcreatedir`，参数名
  为 `dirname`）。

### 重连自状态 / ClientMoved reason / 传输密码 / join_voice

- **重连恢复自身状态**：`update_self` 应用过的最后一份运行时状态（away、
  指挥官、徽章等）在重连成功后自动重放（`restore_state` 开启时）；
  input/output muted 本就随 clientinit 重发。`ReconnectPolicy.restore_state`
  文档同步对齐（明确不恢复频道订阅及原因）。
- **破坏性变更：`Event::ClientMoved` 增加 `reason: ClientMoveReason`**。
  真机抓包（3.13.8）：频道踢对被踢者以 `notifyclientmoved reasonid=4`
  + `reasonmsg` + invoker 到达（移动为 reasonid=1），此前被整行丢弃、
  无法区分「被踢」与「被移动」。`ClientMoveReason`：Moved /
  ChannelKicked{by,message} / Other(raw)。
- **破坏性变更（trait 签名）**：`list_files` / `delete_file` 增加
  `password: Option<&str>`（明文、内部哈希），密码频道不再需要 exec 兜底。
- **`join_voice` 不再忽略 password**：`clientmove` 支持可选 `cpw`
  （base64(sha1)），真机验证正反例。

### Whisper（FEATURES.md §6.4）

- **`Ts3Ext::send_whisper(targets, frame)`**：按目标列表（成员/频道混合，
  ≤65）发送耳语帧，旧协议目标列表格式（`[codec][N][M][cid:8×N][clid:16×M]
  [opus]`），真机验证成员定向可收。注意权限门控（whisper power）不足时
  服务器静默丢弃——`Ok(())` 只代表"已发送"。
- **破坏性变更：`Event::SpeakingStarted/Stopped` 增加 `whispering` 字段**。
  根因修复：S2C 耳语 relay 与普通语音形状相同，此前按 NEWPROTOCOL 标志
  区分导致所有耳语被当作普通语音（标志判断本身有误，区分靠包类型
  Voice/VoiceWhisper——见 `docs/TS3_PROTOCOL_NOTES.md` §6.1）。
  `parse_voice` 签名相应增加包类型参数（proto 内部 API）。
- **Whisper 列表管理（纯客户端本地）**：`whisper_lists / add_whisper_list /
  remove_whisper_list / set_active_whisper_list / active_whisper_list /
  send_whisper_to_active_list`；列表随会话保存在内存（重连保留，进程退出
  不持久化）。
- `send_whisper_to_channel` 保留原签名与 newprotocol 格式（频道定向，
  已验证），作为 `send_whisper` 的补充。

### 命令面补全（临时密码 / 频道组 / 图标横幅 / 密码校验 / TalkPower / 连接信息 / 3D 音频）

- **临时密码**（§8.2）：`add_temp_password / temp_passwords /
  remove_temp_password`（`servertemppassword*`；列表响应双份需去重、
  空表 1281 按空处理——均已处理）。
- **频道组指派**（§9.3）：`set_member_channel_group`（数据库 id 自动解析）、
  `channel_group_members`。**修正：server/channelgrouplist 的 kind 列线上
  名为 `type`（0=模板 1=常规 2=ServerQuery），此前读 `sgtype`/`cgtype`
  恒为 0**。
- **密码本地校验**（§11）：`verify_channel_password` + 导出
  `hash_password`。实测频道密码在服务器端加盐存储（channelinfo 的哈希
  每次创建都不同），无法远端比对——按原版客户端语义改为对照本地缓存
  的成功哈希（join/create 成功后自动记录）；无密码频道恒 true。
- **Talk Power**（§9.5）：`request_talk_power / cancel_talk_power_request /
  grant_talk_power`（授予可定时自动收回）。事件
  `TalkPowerRequested` 由 book 泵在 `client_talk_request` 0→1 跳变时发出。
  线上备注：3.13.8 拒绝文档写法 `client_talk_request=1`（1538），
  实际生效键为 `client_talk_request_time`，`_msg` 参数不被接受。
- **连接/本地信息**（§11）：`member_idle_time`（`clientlist -times`）、
  `member_connection_info / server_connection_info`（`getconnectioninfo`；
  `clientconnectioninfo/serverconnectioninfo` 是 Query 专属，客户端协议
  256）。协议笔记 §9 有完整怪癖清单（`clientinfo`/`channelinfo` 响应以
  `client_default_channel`/`channel_topic` 通知名到达等）。
- **图标/横幅**（§8.1/§11）：`upload_icon / download_icon /
  set_member_icon / set_channel_icon / set_host_banner`。图标以
  `i_icon_id` 权限存储（`channeledit channel_icon_id` 被 1538 拒绝），
  icon id = CRC64-ECMA 低 32 位（无新依赖，自带实现）。
- **3D 定位音频**（§6.5，纯本地渲染）：univox-voice `Mixer` 新增
  `set_listener / set_member_position / clear_member_position /
  clear_listener` 与 `mix_frame_stereo`（等功率立体声声像 + 距离衰减，
  参考距离 2 m）；`mix_frame` 对定位成员仅做距离衰减（单声道契约不变，
  未定位成员零回归）。`Ts3Session` 暴露
  `set_listener_position / set_member_position / clear_member_position /
  clear_listener_position`（需接收管线运行中）。

### 测试

- 单元：book 映射（poke/left-reason/extra fixture）、identity JSON 往返、
  handshake 参数、传输句柄（本地 TCP mock）、whisper 包组装。
- 集成补：成员定向 whisper 可收 + SpeakingStarted whispering 标记、
  whisper 列表 CRUD/激活边界、临时密码往返、频道组指派往返、
  verify_channel_password 正反例、talk power 授予自动收回、
  连接信息查询、图标上传/下载/指派、hostbanner 回读。
- 单元补：TalkPowerRequested 0→1 跳变（重复不重发）、CRC64 校验向量、
  Mixer 定位衰减/声像（远距衰减、右侧成员右耳响、正前居中）、
  whisper 包组装。
- 集成（本地真实 ts3server，`tests/features_integration.rs`）：
  流式上传/下载往返、abort 删除半成品、move_channel 排序、
  server_groups/channel_groups/own_permissions/subscribe_all、
  privilege key 连接时生效 + uid pin 拒绝伪造服务器、服务器密码正反例、
  双客户端 poke 事件。
