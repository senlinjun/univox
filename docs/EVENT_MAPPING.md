# TS3 事件映射表（univox-ts3 → univox-core）

`apply_to_book`（crates/univox-ts3/src/book.rs）把客户端协议的命令/通知流
映射为统一 Book 更新与 Event。本表是 FEATURES.md §5.3 映射承诺的落地版。

## 1. 初始同步（不发事件，只填镜像）

| 线上形态 | 识别方式 | 镜像效果 |
|---|---|---|
| 登录推 channellist | 无名，首字段 `cid` | 全量 channels |
| clientlist 请求响应 | 无名，首字段 `clid` | 全量 members + states |
| initserver | 无名，首字段 `virtualserver_*` | server 信息 + self_member |
| servergrouplist / channelgrouplist | 首字段 `sgid` / `cgid` | （通知流透传） |
| channellistfinished | 同名 | 无操作（同步完成标记） |

> 服务器登录只推 channellist，不推 clientlist —— session 启动时软请求
> 一次 `clientlist -uid -away -voice -groups`（guest 无权限则忽略）。

## 2. 命令/通知 → 事件

| TS3 线上命令 | 统一事件 | 说明 |
|---|---|---|
| notifycliententerview | MemberJoined / MemberUpdated | `ctid` 为目标频道 |
| notifyclientleftview / notifyclientdisconnect | MemberLeft（结构化 MemberLeftReason） | 移除 member/state；reasonid 1/4/5/6/8/… 映射见 model.rs，未识别的保留 `Other("reasonid=N")` |
| notifyclientmoved | ClientMoved | `ctid` 新频道；invoker + 结构化 reason（1=Moved，4=ChannelKicked{by,message}——频道踢对被踢者走此通知） |
| notifyclientupdated | MemberUpdated | **增量行**：空 nickname 不覆盖已有值，extra 合并 |
| notifytextmessage | MessageCreated | targetmode 1/2/3 → Direct/Channel/Server |
| notifyclientpoke | MessageCreated | target=Poke(invoker)、author=invoker，便于原样回戳 |
| notifychannelcreated | ChannelCreated | 更新镜像 |
| notifychanneledited | ChannelUpdated | **增量行**：extra 合并、空 name 不覆盖（行里只带改动字段，父字段为 `cpid`） |
| notifychanneldeleted | ChannelDeleted | 删除镜像 |
| notifychannelmoved | ChannelMoved | cpid/order |
| notifyplugincmd | PluginCommandReceived | invokerid→member，data→payload |
| SpeakingStarted/Stopped | （由语音管线发出） | 混音器每帧统计贡献者 |

## 3. 语音管线事件（session.rs）

| 内部时机 | 统一事件 |
|---|---|
| 进入接收后，某成员首次贡献混音帧 | SpeakingStarted { member } |
| 连续 20ms 帧无该成员贡献 | SpeakingStopped { member } |

## 4. 生命周期事件（session.rs 监督者）

| 时机 | 事件序列 |
|---|---|
| 连接建立 | Connected |
| 连接死亡（非用户发起） | TemporarilyDisconnected { reason } → Reconnecting |
| 重连成功、状态恢复后 | Connected + Reconnected |
| 重连策略耗尽 | Closed { Network("reconnect attempts exhausted") }，状态 Disconnected |
| 用户 disconnect() | Closed { Requested }（监督者不重连） |

## 5. TS3 特有事件（不映射，走扩展层/Raw）

- `notifyclientneededpermissions`、`notifyservergroupclientadded` 等：
  进入 Raw(RawEvent { name, payload })，供 Ts3Ext 用户按需消费。
- 文件传输进度、whisper 目标列表等管理面：Ts3Ext trait 方法直接返回
  （FEATURES.md §11），不走事件。

## 6. 簿记可关闭

`ConnectOptions::bookkeeping(BookkeepingConfig { enabled: false, .. })`
时镜像不写入（`Book::with/with_mut` 返回 None），事件照常发布。
