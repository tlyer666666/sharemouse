# DeskBridge

[![CI](https://github.com/tlyer666666/sharemouse/actions/workflows/ci.yml/badge.svg)](https://github.com/tlyer666666/sharemouse/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/tlyer666666/sharemouse?display_name=tag)](https://github.com/tlyer666666/sharemouse/releases/latest)
[![License](https://img.shields.io/github/license/tlyer666666/sharemouse)](LICENSE)

[下载最新版](https://github.com/tlyer666666/sharemouse/releases/latest) · [查看发布记录](https://github.com/tlyer666666/sharemouse/releases) · [协议与安全设计](docs/PROTOCOL.md)

DeskBridge 是一个无账号、无云服务的 Windows / macOS 局域网键盘鼠标共享工具。它让连接在一台电脑上的鼠标和键盘，通过屏幕边缘自然切换到另一台电脑；控制连接只接受本机、私有网段和链路本地 IPv4 地址。

这是一个独立实现的实验性 MVP，与 ShareMouse 及其开发商没有关联，也不使用其名称、图标、协议或代码。

> Windows 便携版和 macOS 测试版请从 [GitHub Releases](https://github.com/tlyer666666/sharemouse/releases/latest) 下载。Windows 已完成本机构建与启动验证；macOS 由 GitHub Actions 在真实 macOS runner 编译，但 CoreGraphics 权限、不同键盘布局和长时间使用仍需真机验证。

## 目前可用

- Windows 与 macOS 使用同一份 Rust 核心代码
- UDP 局域网自动发现，自动发现失败时可填写 IP 或主机名
- 鼠标移动、左右/中键、扩展键、滚轮和常用键盘按键
- 左右屏幕布局与 80–600 ms 边缘推动时间
- 使用 USB HID usage 进行跨平台键位传输
- 256 位随机配对密钥
- HMAC-SHA256 双向认证和 HKDF 会话密钥派生
- ChaCha20-Poly1305 加密并验证每一帧，严格递增序号防重放
- `Ctrl + Alt + Shift + Esc`（macOS 为 `Control + Option + Shift + Esc`）紧急返回
- 心跳、断线自动返回、远端按键和鼠标按钮自动释放
- Windows Per-Monitor V2 DPI 清单
- macOS 输入监控和辅助功能权限检测

暂未实现：剪贴板、文件拖放、上下布局、多于两台设备、登录时启动、系统托盘、互联网中继和锁屏控制。

## 快速开始

假设 Mac 放在 Windows 的右边：

1. 从 [Releases](https://github.com/tlyer666666/sharemouse/releases/latest) 下载 Windows 便携版和对应的 macOS 测试版；也可以按下文从源码构建。
2. 首次启动时，Windows 防火墙只勾选“专用网络”；不要开放公用网络。
3. 复制任意一台电脑自动生成的现有配对密钥，粘贴到另一台；不要在两端分别“重置配对”。
4. 等待局域网设备出现；两端都选择对方。若 5 秒后仍未出现，填写对方局域网 IP。
5. Windows 选择“对方在右侧”，Mac 选择“对方在左侧”，然后在两端点击“保存并开启”。
6. 持续把 Windows 鼠标推向右侧边缘约 180 ms；鼠标会出现在 Mac 左侧。持续推向 Mac 左侧边缘即可返回。

按住鼠标按钮时不会触发跨屏，屏幕上下角各有 24 px 防误触区域。

### Windows 构建

普通使用无需构建，下载 `DeskBridge-Windows-x64.exe` 后直接运行。首次启动若出现 Windows 防火墙提示，只允许“专用网络”。当前可执行文件未做商业代码签名，Windows 可能显示信誉提示。

以下内容仅供开发：

要求：Windows 10/11、Rust stable、MSVC Build Tools、WebView2 Runtime。

在 PowerShell 中运行：

```powershell
Set-ExecutionPolicy -Scope Process Bypass
.\scripts\build-windows.ps1
```

输出文件：

```text
dist/windows/DeskBridge.exe
```

也可以直接开发运行：

```powershell
cargo run --manifest-path src-tauri\Cargo.toml
```

Windows 限制：`SendInput` 受 UIPI 保护，普通权限的 DeskBridge 不能控制管理员窗口、UAC 安全桌面、锁屏或登录界面。MVP 不会要求管理员权限，也不会绕过这些保护。

### macOS 构建

要求：macOS 13 或更新版本、Xcode Command Line Tools、Rust stable。Apple Silicon 与 Intel Mac 都使用当前机器的原生 Rust target。

```bash
xcode-select --install
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
bash scripts/build-macos.sh
open src-tauri/target/release/bundle/macos/DeskBridge.app
```

第一次运行后，在以下位置允许 DeskBridge：

```text
系统设置 → 隐私与安全性 → 输入监控
系统设置 → 隐私与安全性 → 辅助功能
```

还需要在系统提示时允许“本地网络”。授权后完全退出再重新打开应用。开发版如果移动了 `.app`、改变 Bundle ID 或重新签名，macOS 可能会再次要求授权。

`build-macos.sh` 生成本地 ad-hoc 签名的 `.app`，只适合你自己的 Mac。给其他人分发前需要 Apple Developer ID 签名和 notarization。

> Windows 适配器已完成本地编译和启动验证；macOS CoreGraphics 适配器由 CI 编译验证，但必须在实际 Mac 上完成 TCC 权限、键盘布局和跨机输入实测后才能视为稳定版本。

## 配置方法

应用首次启动时自动生成设备 ID 和随机配对密钥。

配置位置：

- Windows：`%APPDATA%\DeskBridge\settings.json`
- macOS：`~/Library/Application Support/DeskBridge/settings.json`

macOS 配置文件会设置为 `0600`。Windows 配置位于当前用户的 profile 下。MVP 尚未把配对密钥迁移到 Windows Credential Manager / DPAPI 或 macOS Keychain，因此不要把配置文件发给别人；正式发布前应完成系统密钥库迁移。

## 网络与隐私

```text
控制端 ── TCP 24817（认证 + 加密输入）── 接收端
   └──── UDP 24816（仅设备发现）─────────┘
```

- 不需要账号，不访问 DeskBridge 云服务，也不包含遥测。
- 自动发现广播只包含协议版本、随机设备 ID、设备显示名、操作系统和监听端口。
- 配对密钥不会通过网络发送。
- 键盘内容不会写入日志。
- TCP 帧最大 64 字节，单帧读取总时限为 3 秒、写入总时限为 750 ms，序号必须严格递增；错误密钥、篡改帧、重复帧和乱序帧都会中止连接。
- 手动地址允许填写 `192.168.1.20`、`192.168.1.20:24817` 或解析到私有 IPv4 的局域网主机名；公网地址会被拒绝。

如果设备无法发现：

1. 确认两台电脑连接同一个 Wi‑Fi / 有线局域网，且访客网络没有“客户端隔离”。
2. 暂时断开会接管路由的 VPN，或在 VPN 中允许本地网络。
3. 在 Windows Defender 防火墙中允许 DeskBridge 的专用网络访问。
4. 确认 UDP 24816 和 TCP 24817 没被其他程序占用。
5. 使用 `ipconfig`（Windows）或 `ipconfig getifaddr en0`（macOS）查看 IP，然后手动填写。

## 安全返回

紧急返回快捷键由控制端原生钩子在本机处理，不会发送到网络：

- Windows：`Ctrl + Alt + Shift + Esc`
- macOS：`Control + Option + Shift + Esc`

以下情况都会释放远端所有已按下的按键和鼠标按钮，并把输入返回本机：

- 紧急快捷键
- 点击“立即返回本机”
- 关闭共享总开关
- 对端返回边缘
- 认证失败、连接断开或 3 秒心跳超时
- 正常退出进程

## 已知限制

- 当前只支持一台对端和左/右布局。
- macOS 当前以主显示器边界切换；复杂的多显示器负坐标布局尚未完成。
- 默认保持物理键位。Windows Ctrl 与 macOS Command 不会做“语义快捷键”互换。
- 不保证 Fn、媒体键、死键、不同键盘布局或输入法组合键完全一致。
- Windows 高权限窗口和 macOS Secure Input / 登录窗口不会被绕过。
- 传输已经加密，但设备信任目前通过手动共享的高熵密钥完成；还没有双端六位 SAS 确认界面。
- 这是设置窗口应用，尚未常驻系统托盘或菜单栏。
- 强制结束进程、系统崩溃或断电无法保证完成远端按键释放；遇到卡键时在对端按一下对应按键即可复位。

## 开发与验证

```powershell
cargo fmt --manifest-path src-tauri\Cargo.toml -- --check
cargo clippy --manifest-path src-tauri\Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path src-tauri\Cargo.toml
```

核心测试覆盖：

- 配对密钥规范化和拒绝无效密钥
- 所有输入消息的二进制编解码
- 客户端/服务端认证握手
- ChaCha20-Poly1305 双向加密消息传输

协议和状态机细节见 [docs/PROTOCOL.md](docs/PROTOCOL.md)。

## 项目结构

```text
DeskBridge/
├── ui/                         Tauri 内嵌设置界面
├── src-tauri/src/
│   ├── app.rs                  IPC 与设置编排
│   ├── config.rs               配置和密钥生成
│   ├── discovery.rs            UDP 局域网发现
│   ├── network.rs              TCP 会话、心跳和控制状态机
│   ├── protocol.rs             握手、AEAD 帧和输入消息
│   └── platform/
│       ├── windows.rs          Win32 hooks / SendInput
│       └── macos.rs            CGEventTap / CGEventPost
├── scripts/                    两端本地构建脚本
└── docs/PROTOCOL.md            安全模型与协议说明
```
