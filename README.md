# DeskBridge

![CI](https://github.com/tlyer666666/sharemouse/actions/workflows/ci.yml/badge.svg)

DeskBridge is a lightweight local mouse/keyboard sharing app for Windows and macOS on the same LAN.

- Local discovery on UDP
- Edge-triggered switching by default
- Encrypted pairing with HMAC-SHA256 + ChaCha20-Poly1305
- Auto emergency key release + heartbeat timeouts
- No cloud login, no account, no keyboard data collection

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
