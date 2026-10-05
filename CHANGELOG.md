# Changelog

## Unreleased — 客户端 API 补全（连接/身份/事件/传输/构建）

面向下游嵌入方的一批 API 补全与保真度修复；多项是既有抽象的接线补全
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

### 测试

- 单元：book 映射（poke/left-reason/extra fixture）、identity JSON 往返、
  handshake 参数、传输句柄（本地 TCP mock）。
- 集成（本地真实 ts3server，`tests/features_integration.rs`）：
  流式上传/下载往返、abort 删除半成品、move_channel 排序、
  server_groups/channel_groups/own_permissions/subscribe_all、
  privilege key 连接时生效 + uid pin 拒绝伪造服务器、服务器密码正反例、
  双客户端 poke 事件。
