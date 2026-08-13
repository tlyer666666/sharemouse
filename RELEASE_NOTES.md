## DeskBridge v0.1.0

首个可用版本：在同一局域网内，让 Windows 与 macOS 共用一套键盘和鼠标。

### 下载

- `DeskBridge-Windows-x64.exe`：Windows 10/11 便携版，直接运行。
- `DeskBridge-macOS-arm64.zip`：由 GitHub Actions 在 Apple Silicon runner 构建的本地签名应用。
- `SHA256SUMS.txt`：下载文件的 SHA-256 校验值。

### 主要功能

- 局域网自动发现，找不到时可手动填写 IP。
- 左右屏幕边缘切换，可调整触发时间。
- HMAC-SHA256 双向认证、HKDF 会话密钥和 ChaCha20-Poly1305 加密。
- 断线、心跳超时和紧急快捷键自动归还本机输入并释放按键。
- 不需要账号、不连接云端、不记录键盘内容。

### 使用前须知

- 两台设备必须配置相同的配对密钥，并连接同一个可信局域网。
- Windows 首次运行只允许“专用网络”防火墙访问；当前二进制未做商业代码签名。
- macOS 需要允许“本地网络”“输入监控”和“辅助功能”；当前 macOS 版本未做 Developer ID 公证。
- 本版本不支持文件拖放、剪贴板、多于两台设备、UAC/登录安全桌面和互联网中继。

完整设置与安全说明请阅读仓库的 `README.md` 和 `docs/PROTOCOL.md`。
