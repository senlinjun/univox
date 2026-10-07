# TS3 协议实现笔记（univox-ts3）

实现过程对照真实 ts3server 3.13.8 验证得出的协议事实与坑位，供后续维护与
TS6 适配参考。

## 1. 连接层（UDP 9987）

### 1.1 握手（新协议，server ≥ 3.1）

1. **Init1**（C→S，明文）：`MAC=TS3INIT1`，PId 固定 `0x65`，内容
   `[version(4)][0][timestamp(4)][random0(4)][8 零]`。服务器可能回
   `Init1`（继续）或 `Init127`（要求重发，需循环重试）。
2. **Init2/3**：回显 random0，交换 random1。
3. **initivexpand2**（S→C 命令包，伪造加密）：携带服务器 RSA 证书链、
   `beta`（54 字节）、`ek`（服务器临时 Curve25519 公钥）。
   - 证书链验证：根证书固定公钥（ReSpeak tsdeclarations），每级证书的
     `next_key = pub × clamp(sha512(block[1..]))`，末级为服务器公钥。
   - `SharedIV = sha512(ECDH(our_ephemeral, ek))[0..32]`，其中
     `our_ephemeral = client_private_key × ek_random`（注意：客户端的
     `ek` 素数标量来自身份私钥与随机数推导），再与 alpha（10 字节）/
     beta（54 字节）异或。
   - `SharedMac = sha1(SharedIV)[0..8]`。
4. **clientek**（C→S，Command PId=1，伪造加密）：`ek`（客户端临时公钥）
   + `proof`（身份私钥对 `ek||beta` 的 ECDSA-P256 签名）。
5. **clientinit**（Command PId=2）：注意**必须**包含
   `client_input_hardware=1` 与 `client_output_hardware=1` ——
   服务器只在这类客户端之间转发语音（未声明的客户端收不到任何语音包，
   也不会被标记为说话）。
6. **initserver**（S→C）：`aclid` 为本端 clid；它在语义上作为对
   clientinit 的应答。服务器**还会**为 clientinit 发独立的 Ack
   （ack 流 PId=1，payload=2）——不加密（见 §1.3），按 payload 幂等
   处理即可，无需特判丢弃。

### 1.2 包头

- C2S：`MAC(8) | PId(2) | CId(2) | Type(1)`，共 13 字节。
- S2C：`MAC(8) | PId(2) | Type(1)`，共 11 字节。
- Type 字节低 4 位为类型（0=Voice 1=VoiceWhisper 2=Command 3=CommandLow
  4=Ping 5=Pong 6=Ack 7=AckLow 8=Init），高 4 位为 flags
  （0x10=Fragmented 0x20=NewProtocol 0x40=Compressed 0x80=Unencrypted）。
- 客户端发出的 Command 恒带 NewProtocol。

### 1.3 加密（EAX，AES-128，8 字节 tag）

- 密钥材料：`sha256(0x31|type|gen(4)|SharedIV)`（有 CId，即 0x31）/
  `sha256(0x30|…)`（无 CId），key=前 16 字节，nonce=后 16 字节，
  再 `key[0..2] ^= PId`。每个 (type, gen) 缓存一份。
- EAX 的 AAD = **包头去 MAC 后的 3~5 字节（PId|CId|Type）**，不含 MAC。
- 伪造加密（FakeKey `c:\windows\syste` / FakeNonce `m\firewall32.cpl`）：
  initivexpand2（server pid 0）、clientek（client pid 1）、双方各自的
  首个 Ack（pid 0）。
- **服务器的 Ack 从第 2 个起不加密**（实测 3.13.8）：UNENCRYPTED 标志
  置位，MAC = SharedMac，payload 明文 = 被 ack 的 PId（BE16）。客户端
  必须先看 UNENCRYPTED 标志再尝试解密，否则所有 Ack 解密失败——ack
  丢失不会被服务器补发（对重复包静默丢弃、不 re-ack），滞留的命令重发
  12 次（约 2.5 分钟）后连接被判死。解密失败时抓到的原始包
  （`MAC(8)|PId(2)|Type(1)=0x86|payload(2)`）是定位此问题的关键证据。
- Ping/Pong 恒不加密，MAC 必须填 `SharedMac` —— 初版漏掉导致服务器
  静默丢包、30 秒后判死。

### 1.4 命令关联

- 客户端命令追加 `return_code=N`；服务器以 `error id=… return_code=N`
  包结束该请求。
- **响应行可能在 error 包之后以通知形式到达**（见 §3）。

## 2. 身份

- P-256，tomcrypt DER（BitString unused-bits 必须为 7，与 OpenSSL 不同）。
- 官方串格式：`<counter>V<base64(DER)>`，`V` 分隔符可能出现在 base64 内，
  解析需回退。
- 安全等级 = `sha1(base64(pubDER)‖counter)` 前导零位数；clientinit 的
  `client_key_offset` 必须等于生成身份时的 counter，否则 519。
- 重试连接必须换新身份（521 = clone 检测）。

## 3. 命令响应的“通知化”命名（重要）

客户端协议下，不少命令的**响应行不叫命令名**，而是通知形式
（对照 tsdeclarations Messages.toml 的 `notify=` 字段）：

| 请求                  | 响应名                    |
|-----------------------|---------------------------|
| banlist               | notifybanlist             |
| clientdblist          | notifyclientdblist        |
| clientgetuidfromclid  | notifyclientuidfromclid   |
| clientgetdbidfromuid  | notifyclientdbidfromuid   |
| clientgetnamefromuid  | notifyclientnamefromuid   |
| clientgetnamefromdbid | notifyclientnamefromdbid  |
| messagelist           | notifymessagelist         |
| messageget            | notifymessage             |
| complainlist          | notifycomplainlist        |
| ftgetfilelist         | notifyfilelist(+finished) |
| ftinitupload          | notifystartupload         |
| ftinitdownload        | notifystartdownload / notifystatusfiletransfer（失败） |
| whoami 等             | 无名（首字段即数据）      |
| clientdbinfo          | 无名，但首字段 `client_flag_avatar` 为空值裸键，易被当成命令名 |
| clientlist -away      | 无名，但**无人在离开状态时**首字段 `client_away_message` 为空值裸键，同样易被当成命令名 |

连接层据此做精确路由：响应行发给等待中的 exec（同时仍广播给簿记泵）。

## 4. 服务器行为怪癖（3.13.8 实测）

- 登录后服务器**不会**主动推 clientlist；channellist 会推（带
  channellistfinished），guest 无法主动请求（permid 27）。
- `clientupdate` 不接受 `client_description`（1538）；可接受 away、
  badges、meta_data、is_channel_commander 等。
- `plugincmd` 的 targetmode 即官方 PluginTargetMode 枚举值
  （Single=0, CurrentTab=1, Clients=2, All=3）；本版本 2 需要额外
  target 参数，4 直接拒绝。响应为 `notifyplugincmd`（含 invokerid）。
- `complandel` 的参数是 `tcldbid` + `fcldbid`（不是文档写的 banid）。
- `channelclientlist` 在 3.13.8 不存在（256）。
- 文件传输（见 §5）：`ftgetfilelist` 要求 `cpw`（可空）必须存在；
  `ftinitupload/download` 须带 `proto=1`，否则 payload 帧协议不同。
- `clientgetavatar` 不存在；头像按 `/avatar_<client_base64HashClientUID>`
  存取，上传时服务器会重命名。

## 5. 文件传输（TCP，独立端口）

1. `ftinitupload`/`ftinitdownload`（带 `proto=1`）→ error 包之后收到
   `notifystartupload/download`（含 ftkey、port、size）。失败则为
   `notifystatusfiletransfer`（status=2051 文件不存在 / 2054 路径非法等）。
   `ip` 字段不可靠：实测 ts3server 3.13.8 在 `filetransfer_ip=0.0.0.0`
   （默认配置）时根本不带 `ip` 字段，也有服务器报告 `0.0.0.0`/空值——
   官方客户端此时改用语音连接的对端 IP 连该端口；客户端实现必须同样
   处理，否则会连到 127.0.0.1 直接 ECONNREFUSED（移动端必现）。
2. TCP 连到该端口，发送 ASCII ftkey（无 ack），随后：
   - 上传：写完全部字节 → shutdown 写端 → 等服务器关闭（落盘完成）；
   - 下载：读到 EOF（size 只是参考）。
3. `ftstop serverftfid delete=0` 收尾（无害）。
4. `ftdeletefile`/`ftrenamefile` 也要带 `cpw`。

## 6. 语音

- C2S：`[inner_id(2)][codec(1)][opus]`，inner_id 与外层包 id 同计数器；
  codec 4 = OpusVoice，20ms/960 样本 @48kHz。
- S2C：`[inner_id(2)][from(2)][codec(1)][opus]`；whisper 的 S2C 形状相同。
- 收发双方都必须在 clientinit 声明过硬件（§1.1 第 5 步）。
- 语音包同样走 EAX（voice_encryption=true 时），不重传、无确认。

### 6.1 Whisper（耳语，2026-10 实测 3.13.8）

- **C2S 两种格式**（PId 同 voice 计数器，外层 Type=1 VoiceWhisper）：
  - 旧格式（Newprotocol 不置位）：`[codec][N 频道数][M 客户端数]
    [cid:8 ×N][clid:16 ×M][opus]`，成员/频道可混合（≤65 目标）。
    实测 `send_whisper`（成员定向）经服务器转发可被目标收到。
  - newprotocol 格式（Newprotocol 置位）：`[codec][whisper_type][target]
    [target_id:8][opus]`；whisper_type=1、target=0、target_id=cid 定向
    整个频道（`send_whisper_to_channel` 即此格式，已验证）。
- **S2C relay 一律为 VoiceWhisper 类型**，形状与普通 S2C 语音完全相同
  （`[inner_id][from][codec][opus]`）——**区分靠外层包类型，不靠任何
  标志位**。此前按 NEWPROTOCOL 标志判断导致所有耳语被当作普通语音
  解析（音频正常但丢失 whisper 标记）。
- 权限门控（`i_client_whisper_power` / 目标
  `i_client_needed_whisper_power`）不足时服务器**静默丢弃**：语音包无
  ack，发送方无从得知对方是否听到。
- whisper 列表是纯客户端本地概念（原版客户端存于配置），与服务器无
  任何命令交互。

## 7. 稳定性要点

- 出站命令重发：350ms 起、指数退避、12 次放弃；30 秒无入包判定断线。
- 入站乱序命令包进 receive queue；分片（Fragmented）首包带压缩标记，
  QuickLZ 解压在重组完成后进行。
- 新 socket 必须先 `writable().await`（tokio 就绪注册竞态会导致首包
  静默丢弃 —— 重连握手全部超时的根因）。

## 8. 客户端协议经验补充（2026-10，3.13 实测）

- **clientleftview 的 reasonid** 用 tsdeclarations `Reason` 枚举：
  0=离开视野（切频道）、1=Moved（有 invoker）、2=订阅、3=超时、
  4=频道踢、5=服务器踢、6=封禁（带 bantime）、7/11=服务器停止/关闭、
  8=主动退出。旧资料里"8=踢、10=封禁"是误传。
- **channelmove 同父频道内重排报 770**（"already member of channel"）；
  同父内排序要走 `channeledit cid=… channel_order=…`，跨父用 channelmove。
  另外 `notifychannelcreated/edited` 的父字段是 `cpid`（channellist 是 `pid`）。
- **文件传输的取消语义**：客户端直接关闭 payload socket 会被服务器当作
  "传输完成"并提交已写入的部分文件。正确取消顺序是先 `ftstop
  serverftfid delete=1`（趁 payload 连接还开着），再关 socket。
  另外 UDP actor 的命令是串行的，并发 exec 会被拒（"concurrent client
  command"）——收尾型命令要做有界重试。
- **clientdbinfo 的响应行没有可靠的名字**：行首的空字段会被解析器当成
  命令名（无头像时是 `client_flag_avatar`；设置头像后变成下一个空字段，
  如 `client_description`）。识别标准改为"行内含 client_database_id"。
- **clientlist -away 同型陷阱**：列表里无人处于离开状态时，响应行首是
  空值裸键 `client_away_message`，会被当成命令名 → exec 返回 `Ok(空)`
  而非报错，花名册静默变空。识别标准改为"行内含 clid"（连接层，
  `is_response_name` 的 clientlist 分支）。
- **clientupdate client_flag_avatar=<md5hex>**：文件必须已上传，否则
  报 2051（file not found）；值是头像文件的小写 hex MD5。
- **serveredit 设置服务器密码**：只发 `virtualserver_password=…` 即可
  （flag 自动置位）；显式带 `virtualserver_flag_password` 反而报 1538。
- **clientpermlist** 对没有任何权限记录的客户端返回错误 1281（database
  empty result set），应按空列表处理。
- **身份 JSON（tsclientlib/tsproto 0.2）**：`{"key": base64(32 字节裸私钥
  标量), "counter": u64, "max_counter": u64}`，key 序列化等价于
  `EccKeyPrivP256::to_short()` 的 base64；读取时 tomcrypt DER 形式同样接受。
- **clientinit 支持 `client_default_token`**（privilege key，连接时即消费，
  明文）；`client_server_password` / `client_default_channel_password` 均为
  `base64(sha1(明文))`。
- **`client_key_offset`（hash-cash counter）可跨连接复用**：它只是
  工作量证明（服务器验证 `sha1(base64(pubkey)||counter)` 前导零位数），
  不是服务端消耗的一次性值。tsclientlib 0.2 每次连接都直接发送
  `identity.counter()`，从不递增；连接后发现服务器要求更高等级时
  就地 `upgrade_level` 后重连。因此身份必须在连接后回写持久化
  （counter/max_counter），保证升级搜索不回退、等级不退步。
- **ftcreatedir 的路径参数名是 `dirname`**（不是 `path`）。
- **频道踢的到达形态**：被踢客户端收到 `notifyclientmoved reasonid=4`
  （带 `reasonmsg`、`invokerid/invokername/invokeruid`，落到默认频道）；
  旁观者收到的是 `clientleftview reasonid=4` + enterview。
  `notifyclientmoved` 的 reasonid 同样遵循 `Reason` 枚举（1=Moved）。
- **clientmove 支持可选 `cpw`**（base64(sha1)）：切入密码频道用。
