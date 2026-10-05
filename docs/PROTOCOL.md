# DeskBridge v1 protocol

本文档描述 DeskBridge v1 协议的传输格式和失败处理。实现以 `src-tauri/src/protocol.rs` 为准。

## 发现

每台设备每秒向 IPv4 limited broadcast `255.255.255.255:24816` 发送一条不超过 1400 字节的 JSON 广播：

```json
{
  "magic": "deskbridge-discovery-v1",
  "version": 1,
  "node_id": "128-bit-random-id-as-hex",
  "name": "Office Windows",
  "os": "Windows",
  "port": 24817
}
```

广播不包含配对密钥、键盘事件或任何持久身份凭据。6 秒未再次出现的设备会从在线列表移除。

## 认证握手

两端预先拥有相同的 32 字节随机配对密钥。密钥由操作系统 CSPRNG 生成并以 64 位十六进制展示。

客户端发送固定长度 hello：

```text
magic[8] | version[2] | client_id[32 ASCII] | client_nonce[32] | HMAC-SHA256[32]
```

服务端验证后返回：

```text
magic[8] | version[2] | server_id[32 ASCII] | server_nonce[32] | HMAC-SHA256[32]
```

服务端 HMAC 还会覆盖客户端 nonce，因此旧的响应无法被重放。双方以配对密钥和两个随机 nonce 通过 HKDF-SHA256 分别派生 `client → server` 与 `server → client` 两把 256 位密钥。

配对密钥是高熵预共享密钥，不是可离线穷举的六位 PIN。

## 加密帧

握手后的每条消息使用 ChaCha20-Poly1305：

```text
frame_length u32 BE | sequence u64 BE | ciphertext | Poly1305 tag[16]
```

- `frame_length` 包含序号、密文和 tag，不包含自身的 4 字节。
- 最大帧长为 64 字节；握手和每个完整帧都必须在 3 秒总时限内读完，并在 750 ms 总时限内写完。
- 每个方向的序号从 1 开始且必须严格递增。
- 96 位 nonce 为 4 个零字节与 64 位序号的拼接；两个方向使用不同密钥。
- 序号同时作为 AEAD additional authenticated data。
- 解密失败、重复、跳号、乱序、过长帧或未知消息都会断开连接。

## 消息

载荷第一个字节为消息类型：

| 类型 | 名称 | 载荷 |
|---:|---|---|
| 1 | Activate | entry edge `u8` |
| 2 | ActivateAck | 无 |
| 3 | MouseMove | `dx i32`, `dy i32` |
| 4 | MouseButton | button `u8`, down `u8` |
| 5 | Wheel | horizontal `i32`, vertical `i32` |
| 6 | Key | USB HID usage `u16`, down `u8` |
| 7 | ReturnControl | 无 |
| 8 | ReleaseAll | 无 |
| 9 | Ping | monotonic milliseconds `u64` |
| 10 | Pong | 原样返回的 monotonic milliseconds `u64` |

所有整数均为大端序。

## 控制状态机

```text
Idle → edge dwell → Connecting → RemoteActive → Returning → Idle
                           │             │
                           └─ failure ───┴─ disconnect → ReleaseAll → Idle
```

接收端只有在完成认证、本机共享开关已打开、对端身份与已选设备一致且输入注入权限可用时才接受 `Activate`。一次只允许一个接收会话。

发送端收到 `ActivateAck` 后才开始抑制和转发本机输入。接收端在入口边缘检测到向外移动时发送 `ReturnControl`。任何失败路径都执行 `ReleaseAll`。

## 威胁边界

协议防止同一局域网上不知道配对密钥的设备读取、伪造或重放输入。它不解决：

- 已取得任一端本地用户权限的恶意程序
- 已泄露的配对密钥
- 被修改的 DeskBridge 二进制
- Windows UAC / secure desktop 或 macOS Secure Input
- 首次人工复制密钥时所使用通道的泄露

后续正式版本还应把配对密钥存入 DPAPI / Credential Manager 与 macOS 钥匙串，并加入双端 SAS 确认和固定设备身份。
