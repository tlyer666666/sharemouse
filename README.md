# DeskBridge

![CI](https://github.com/tlyer666666/sharemouse/actions/workflows/ci.yml/badge.svg)

DeskBridge is a lightweight local mouse/keyboard sharing app for Windows and macOS on the same LAN.

- UDP discovery on the LAN, with a manual IP fallback; no account, no cloud service, no telemetry
- Edge-triggered switching between two computers, left or right, plus an emergency return hotkey
- Pairing with a 256-bit key: HMAC-SHA256 handshake and ChaCha20-Poly1305 frames; input is released on disconnect or timeout

## Download

- Go to [GitHub Releases](https://github.com/tlyer666666/sharemouse/releases/latest)
- Download the Windows executable: `DeskBridge-Windows-x64.exe`
- Download the macOS package: `DeskBridge-macOS-arm64.zip`

## Build

### Windows

```powershell
Set-ExecutionPolicy -Scope Process Bypass
./scripts/build-windows.ps1
```

Output:

```text
dist/windows/DeskBridge.exe
```

### macOS

```bash
xcode-select --install
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
./scripts/build-macos.sh
```

Output:

```text
src-tauri/target/release/bundle/macos/DeskBridge.app
```

### Dev checks

```powershell
cargo fmt --manifest-path src-tauri/Cargo.toml -- --check
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path src-tauri/Cargo.toml --release
```

## Limitations

- Windows and macOS only, two computers, left/right layout.
- No clipboard, file transfer, system tray icon or login item yet.
- Windows: elevated windows, the UAC secure desktop and the lock screen are out of reach.
- macOS: Secure Input and the login window are out of reach.
- The pairing key is kept in a plaintext settings file, not in the OS keychain yet.
- Release binaries are unsigned: the macOS build is ad-hoc signed and the Windows build is not code-signed.

## Protocol

See [docs/PROTOCOL.md](docs/PROTOCOL.md)

## Ports

- UDP: `24816`
- TCP: `24817`

## Source layout

```text
DeskBridge/
  ui/                         Tauri front-end
  src-tauri/src/              Rust backend and protocol/network layer
  scripts/                    Local build scripts
```

## Licensing

Licensed under the MIT license. See [LICENSE](LICENSE).

DeskBridge is an independent implementation and is not affiliated with ShareMouse, Synergy, Deskflow or Input Leap.
