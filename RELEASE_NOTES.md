## DeskBridge v0.1.0

首个可用版本：在同一局域网内，让 Windows 与 macOS 共用一套键盘和鼠标。

### 下载

- `DeskBridge-Windows-x64.exe`：Windows 10/11 便携版，直接运行。
- `DeskBridge-macOS-arm64.zip`：由 GitHub Actions 在 Apple Silicon runner 构建的本地签名应用。
- `SHA256SUMS.txt`：下载文件的 SHA-256 校验值。

### 使用前须知

- 两台设备必须配置相同的配对密钥，并连接同一个可信局域网。
- Windows 首次运行只允许“专用网络”防火墙访问；当前二进制未做商业代码签名。
- macOS 需要允许“本地网络”“输入监控”和“辅助功能”；当前 macOS 版本未做 Developer ID 公证。
- 本版本不支持文件拖放、剪贴板、多于两台设备、UAC/登录安全桌面和互联网中继。

功能与限制见仓库 `README.md`，协议细节见 `docs/PROTOCOL.md`。
